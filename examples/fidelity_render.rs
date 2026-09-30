//! Fidelity reference harness.
//!
//! Renders bundled presets over a synthetic Karplus-Strong corpus (or a user DI
//! directory), reports loudness/tone/level metrics, and supports reference
//! comparison, ABX file generation, benchmarking, and baseline drift checks.
//!
//! ```text
//! cargo run --release --example fidelity_render -- [OPTIONS]
//!
//!   --presets all | <stem>[,<stem>…]   bundled presets from ./presets (default: all)
//!   --synth                            use analysis::synth::corpus (default if no --di)
//!   --di <dir>                         user DI corpus with manifest.toml
//!   --sr <hz>                          render rate (default 48000)
//!   --out <dir>                        default: target/fidelity/<unix-seconds>/
//!   --width preset | <float>           keep preset master width or override
//!   --ref <dir>                        reference excerpts: <dir>/<preset-stem>.wav
//!   --abx <stemA>:<stemB>              level-matched blind A/B/X files
//!   --bench                            realtime factor per preset @ 44.1/48/96 kHz
//!   --write-baseline <file>            write synthetic-corpus metrics
//!   --check <file>                     compare against a baseline; error on drift
//! ```

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use rusty_riff::analysis::metrics::{
    crest_db, db, envelope, ltas_third_octave, lufs_integrated, lufs_short_term_max,
    noise_floor_db, peak, percentile, rms, side_to_mid_db, spectral_centroid, stereo_correlation,
    treble_mod_depth,
};
use rusty_riff::analysis::render::{
    Render, RenderOpts, read_wav_mono, render_preset, write_wav_f32_stereo,
};
use rusty_riff::analysis::synth::{Phrase, corpus, phrase};
use rusty_riff::preset::{Preset, PresetSource};

/// Placeholder width recorded when the preset's own master width is kept.
/// (TOML cannot represent NaN, so a sentinel is used.)
const PRESET_WIDTH: f32 = -1.0;

// ── CLI ────────────────────────────────────────────────────────────────────────

struct Args {
    presets: Vec<String>,
    di_dir: Option<PathBuf>,
    sr: f32,
    out: PathBuf,
    width_override: Option<f32>,
    ref_dir: Option<PathBuf>,
    abx: Option<(String, String)>,
    bench: bool,
    write_baseline: Option<PathBuf>,
    check: Option<PathBuf>,
}

fn parse_args() -> Result<Args> {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let mut args = Args {
        presets: Vec::new(),
        di_dir: None,
        sr: 48_000.0,
        out: PathBuf::new(),
        width_override: None,
        ref_dir: None,
        abx: None,
        bench: false,
        write_baseline: None,
        check: None,
    };
    let mut i = 0;
    while i < argv.len() {
        let a = &argv[i];
        let next = |i: &mut usize| -> Result<String> {
            *i += 1;
            argv.get(*i)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("missing value for {a}"))
        };
        match a.as_str() {
            "--presets" => {
                let v = next(&mut i)?;
                args.presets = if v == "all" {
                    Vec::new()
                } else {
                    v.split(',').map(str::to_owned).collect()
                };
            }
            "--synth" => {}
            "--di" => args.di_dir = Some(PathBuf::from(next(&mut i)?)),
            "--sr" => args.sr = next(&mut i)?.parse().context("--sr")?,
            "--out" => args.out = PathBuf::from(next(&mut i)?),
            "--width" => {
                let v = next(&mut i)?;
                args.width_override = if v == "preset" {
                    None
                } else {
                    Some(v.parse().with_context(|| format!("--width {v}"))?)
                };
            }
            "--ref" => args.ref_dir = Some(PathBuf::from(next(&mut i)?)),
            "--abx" => {
                let v = next(&mut i)?;
                let (a, b) = v
                    .split_once(':')
                    .ok_or_else(|| anyhow::anyhow!("--abx wants <stemA>:<stemB>"))?;
                args.abx = Some((a.to_owned(), b.to_owned()));
            }
            "--bench" => args.bench = true,
            "--write-baseline" => args.write_baseline = Some(PathBuf::from(next(&mut i)?)),
            "--check" => args.check = Some(PathBuf::from(next(&mut i)?)),
            other => bail!("unknown argument {other}"),
        }
        i += 1;
    }
    if args.out.as_os_str().is_empty() {
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        args.out = PathBuf::from(format!("target/fidelity/{secs}"));
    }
    Ok(args)
}

// ── Presets and DI sources ─────────────────────────────────────────────────────

/// `(stem, preset)` for the selected bundled presets.
fn load_presets(args: &Args) -> Result<Vec<(String, Preset)>> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir("presets").context("reading presets/")? {
        let path = entry?.path();
        if path.extension().is_none_or(|e| e != "toml") {
            continue;
        }
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("preset")
            .to_owned();
        if !args.presets.is_empty() && !args.presets.contains(&stem) {
            continue;
        }
        let preset = Preset::load(&path, PresetSource::System)
            .with_context(|| format!("parsing {}", path.display()))?;
        out.push((stem, preset));
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    if out.is_empty() {
        bail!("no presets selected");
    }
    Ok(out)
}

#[derive(Deserialize)]
struct DiManifest {
    #[serde(default)]
    di: Vec<DiEntry>,
}

#[derive(Deserialize)]
struct DiEntry {
    file: String,
    #[serde(default)]
    guitar: String,
    #[serde(default)]
    calibrated: bool,
    #[serde(default)]
    trim_db: f32,
    #[serde(default = "default_reference_version")]
    reference_version: u32,
}

fn default_reference_version() -> u32 {
    rusty_riff::audio::calibration::REFERENCE_VERSION
}

/// `(name, mono samples)` for every DI in the run.
fn load_di(args: &Args) -> Result<Vec<(String, Vec<f32>)>> {
    match &args.di_dir {
        Some(dir) => {
            let text = std::fs::read_to_string(dir.join("manifest.toml"))
                .with_context(|| format!("reading {}", dir.join("manifest.toml").display()))?;
            let manifest: DiManifest = toml::from_str(&text).context("parsing DI manifest")?;
            let mut out = Vec::new();
            for entry in &manifest.di {
                if entry.reference_version != default_reference_version() {
                    println!(
                        "  warning: {} uses reference_version {} (engine is {})",
                        entry.file,
                        entry.reference_version,
                        default_reference_version()
                    );
                }
                let mut samples = read_wav_mono(&dir.join(&entry.file), args.sr)
                    .with_context(|| format!("decoding {}", entry.file))?;
                if !entry.calibrated && entry.trim_db.abs() > 1e-6 {
                    let g = 10f32.powf(entry.trim_db / 20.0);
                    for x in &mut samples {
                        *x *= g;
                    }
                }
                let name = Path::new(&entry.file)
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("di")
                    .to_owned();
                let _ = (&entry.guitar, entry.reference_version);
                out.push((name, samples));
            }
            if out.is_empty() {
                bail!("DI manifest has no entries");
            }
            Ok(out)
        }
        None => Ok(corpus(args.sr)
            .into_iter()
            .map(|(name, samples)| (name.to_owned(), samples))
            .collect()),
    }
}

// ── Reports ────────────────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize, Clone, Copy)]
struct Band {
    hz: f32,
    db: f32,
}

#[derive(Serialize, Deserialize)]
struct RenderReport {
    preset: String,
    di: String,
    sr: f32,
    width: f32,
    lufs_i: f32,
    lufs_st_max: f32,
    sample_peak: f32,
    rms_db: f32,
    crest_db: f32,
    centroid_hz: f32,
    correlation: f32,
    side_to_mid_db: f32,
    punch_p95_med: f32,
    treble_mod: f32,
    noise_floor_db: f32,
    /// Mean of the render, as a fraction of its RMS. A static DC offset shows up
    /// here and nowhere else: `ltas_third_octave` is mean-normalized and the
    /// 70 Hz band starts above most of a real amp's DC blocker, so an offset
    /// large enough to waste headroom is invisible to every other metric.
    dc_ratio: f32,
    ltas: Vec<Band>,
}

#[derive(Serialize, Deserialize)]
struct Report {
    renders: Vec<RenderReport>,
}

fn measure(preset: &str, di: &str, r: &Render, width: f32) -> RenderReport {
    let mid: Vec<f32> = r.l.iter().zip(&r.r).map(|(a, b)| 0.5 * (a + b)).collect();
    let mut env = envelope(&mid, r.sr, 5.0);
    env.sort_by(f32::total_cmp);
    let punch = percentile(&env, 0.95) / percentile(&env, 0.5).max(1e-9);
    let ltas = ltas_third_octave(&mid, r.sr)
        .into_iter()
        .map(|(hz, db)| Band { hz, db })
        .collect();
    RenderReport {
        preset: preset.to_owned(),
        di: di.to_owned(),
        sr: r.sr,
        width,
        lufs_i: lufs_integrated(&r.l, &r.r, r.sr),
        lufs_st_max: lufs_short_term_max(&r.l, &r.r, r.sr),
        sample_peak: peak(&r.l).max(peak(&r.r)),
        rms_db: db(rms(&mid)),
        crest_db: crest_db(&mid),
        centroid_hz: spectral_centroid(&mid, r.sr),
        correlation: stereo_correlation(&r.l, &r.r),
        side_to_mid_db: side_to_mid_db(&r.l, &r.r),
        punch_p95_med: punch,
        treble_mod: treble_mod_depth(&mid, r.sr),
        noise_floor_db: noise_floor_db(&mid, r.sr),
        dc_ratio: {
            let mean = mid.iter().sum::<f32>() / mid.len().max(1) as f32;
            let r_rms = rms(&mid).max(1e-12);
            (mean / r_rms).abs()
        },
        ltas,
    }
}

fn write_reports(out: &Path, reports: &[RenderReport]) -> Result<()> {
    let report = Report {
        renders: reports
            .iter()
            .map(|r| RenderReport {
                preset: r.preset.clone(),
                di: r.di.clone(),
                sr: r.sr,
                width: r.width,
                lufs_i: r.lufs_i,
                lufs_st_max: r.lufs_st_max,
                sample_peak: r.sample_peak,
                rms_db: r.rms_db,
                crest_db: r.crest_db,
                centroid_hz: r.centroid_hz,
                correlation: r.correlation,
                side_to_mid_db: r.side_to_mid_db,
                punch_p95_med: r.punch_p95_med,
                treble_mod: r.treble_mod,
                noise_floor_db: r.noise_floor_db,
                dc_ratio: r.dc_ratio,
                ltas: r.ltas.clone(),
            })
            .collect(),
    };
    std::fs::write(out.join("report.toml"), toml::to_string_pretty(&report)?)?;

    let mut csv = String::from(
        "preset,di,sr,width,lufs_i,lufs_st_max,sample_peak,rms_db,crest_db,\
         centroid_hz,correlation,side_to_mid_db,punch_p95_med,treble_mod,noise_floor_db,dc_ratio\n",
    );
    for r in reports {
        csv.push_str(&format!(
            "{},{},{},{},{:.3},{:.3},{:.6},{:.3},{:.3},{:.1},{:.4},{:.2},{:.3},{:.3},{:.2},{:.6}\n",
            r.preset,
            r.di,
            r.sr,
            r.width,
            r.lufs_i,
            r.lufs_st_max,
            r.sample_peak,
            r.rms_db,
            r.crest_db,
            r.centroid_hz,
            r.correlation,
            r.side_to_mid_db,
            r.punch_p95_med,
            r.treble_mod,
            r.noise_floor_db,
            r.dc_ratio,
        ));
    }
    std::fs::write(out.join("summary.csv"), csv)?;
    Ok(())
}

// ── Baseline ───────────────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize)]
struct Baseline {
    sr: f32,
    phrase: String,
    #[serde(rename = "render")]
    renders: Vec<BaselineEntry>,
}

#[derive(Serialize, Deserialize)]
struct BaselineEntry {
    preset: String,
    lufs_i: f32,
    crest_db: f32,
    correlation: f32,
    centroid_hz: f32,
    ltas: Vec<Band>,
}

/// Turn every render of phrase `phrase` into a baseline entry keyed by preset.
fn baseline_from(reports: &[RenderReport], sr: f32, phrase: &str) -> Baseline {
    let renders = reports
        .iter()
        .filter(|r| r.di == phrase)
        .map(|r| BaselineEntry {
            preset: r.preset.clone(),
            lufs_i: r.lufs_i,
            crest_db: r.crest_db,
            correlation: r.correlation,
            centroid_hz: r.centroid_hz,
            ltas: r.ltas.clone(),
        })
        .collect();
    Baseline {
        sr,
        phrase: phrase.to_owned(),
        renders,
    }
}

fn check_baseline(path: &Path, reports: &[RenderReport], sr: f32) -> Result<()> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading baseline {}", path.display()))?;
    let baseline: Baseline = toml::from_str(&text).context("parsing baseline")?;
    if (baseline.sr - sr).abs() > 1.0 {
        bail!("baseline is for {} Hz, run is {sr} Hz", baseline.sr);
    }
    let mut violations = 0usize;
    for want in &baseline.renders {
        let Some(got) = reports
            .iter()
            .find(|r| r.preset == want.preset && r.di == baseline.phrase)
        else {
            println!("  MISSING {} / {}", want.preset, baseline.phrase);
            violations += 1;
            continue;
        };
        if (got.lufs_i - want.lufs_i).abs() > 0.1 {
            println!(
                "  {}: lufs_i {:.3} vs {:.3} (±0.1)",
                want.preset, got.lufs_i, want.lufs_i
            );
            violations += 1;
        }
        if (got.crest_db - want.crest_db).abs() > 0.2 {
            println!(
                "  {}: crest_db {:.3} vs {:.3} (±0.2)",
                want.preset, got.crest_db, want.crest_db
            );
            violations += 1;
        }
        if (got.correlation - want.correlation).abs() > 0.02 {
            println!(
                "  {}: correlation {:.4} vs {:.4} (±0.02)",
                want.preset, got.correlation, want.correlation
            );
            violations += 1;
        }
        if want.centroid_hz > 0.0
            && (got.centroid_hz - want.centroid_hz).abs() / want.centroid_hz > 0.01
        {
            println!(
                "  {}: centroid {:.1} vs {:.1} (±1%)",
                want.preset, got.centroid_hz, want.centroid_hz
            );
            violations += 1;
        }
        // The output ceiling is a **guarantee, not a measurement**, so this is a
        // hard bound rather than a drift tolerance: `soft_limit` asymptotes to
        // exactly 1.0 and the audio engine's final output stage re-applies it
        // after the take-bus sum, so nothing above that can reach the converter.
        //
        // This gate is the reason finding A1 survived so long: `sample_peak` was
        // *recorded* and never checked, so a limiter that raised its own ceiling
        // to +1.21 dBFS looked fine for months.
        if got.sample_peak > 1.0 {
            println!(
                "  {}: sample_peak {:.5} exceeds the 1.0 ceiling",
                want.preset, got.sample_peak
            );
            violations += 1;
        }
        // A static DC offset is invisible to every other metric here: the LTAS is
        // mean-normalized and starts at 70 Hz, above most of the amps' DC
        // blockers. A few percent of RMS is already audibly pumping and wastes
        // headroom; nothing legitimate sits there.
        if got.dc_ratio > 0.02 {
            println!(
                "  {}: dc_offset {:.4} of rms (±0.02)",
                want.preset, got.dc_ratio
            );
            violations += 1;
        }
        for (g, w) in got.ltas.iter().zip(&want.ltas) {
            if (g.db - w.db).abs() > 0.25 {
                println!(
                    "  {}: LTAS {:.0} Hz {:.2} vs {:.2} (±0.25 dB)",
                    want.preset, w.hz, g.db, w.db
                );
                violations += 1;
            }
        }
    }
    if violations > 0 {
        bail!("{violations} baseline tolerance violation(s)");
    }
    println!("baseline check: OK ({} presets)", baseline.renders.len());
    Ok(())
}

// ── Reference comparison ───────────────────────────────────────────────────────

/// Integrated-LUFS level match of `l`/`r` to `target` LUFS.
fn match_loudness(l: &[f32], r: &[f32], sr: f32, target_lufs: f32) -> (Vec<f32>, Vec<f32>) {
    let lufs = lufs_integrated(l, r, sr);
    if !lufs.is_finite() || !target_lufs.is_finite() {
        return (l.to_vec(), r.to_vec());
    }
    let gain = 10f32.powf((target_lufs - lufs) / 20.0);
    (
        l.iter().map(|x| x * gain).collect(),
        r.iter().map(|x| x * gain).collect(),
    )
}

fn ltas_rms_diff_db(a: &[Band], b: &[Band]) -> f32 {
    let mut sum = 0.0f32;
    let mut n = 0usize;
    for (x, y) in a.iter().zip(b) {
        if (80.0..=8000.0).contains(&x.hz) {
            let d = x.db - y.db;
            sum += d * d;
            n += 1;
        }
    }
    if n == 0 { 0.0 } else { (sum / n as f32).sqrt() }
}

fn run_ref(args: &Args, presets: &[(String, Preset)], out: &Path) -> Result<()> {
    let Some(ref_dir) = &args.ref_dir else {
        return Ok(());
    };
    println!("\nreference comparison (level-matched by integrated LUFS):");
    for (stem, preset) in presets {
        let ref_path = ref_dir.join(format!("{stem}.wav"));
        if !ref_path.exists() {
            continue;
        }
        let mono = read_wav_mono(&ref_path, args.sr)?;
        let (rl, rr) = (mono.clone(), mono);
        let target = lufs_integrated(&rl, &rr, args.sr);
        let di = phrase(Phrase::Chugs, args.sr);
        let opts = RenderOpts {
            sr: args.sr,
            width_override: args.width_override,
            ..RenderOpts::default()
        };
        let render = render_preset(preset, &di, &opts);
        let (ml, mr) = match_loudness(&render.l, &render.r, args.sr, target);
        let dir = out.join(stem);
        std::fs::create_dir_all(&dir)?;
        write_wav_f32_stereo(
            &dir.join("render_matched.wav"),
            &Render {
                l: ml.clone(),
                r: mr.clone(),
                sr: args.sr,
            },
        )?;
        write_wav_f32_stereo(
            &dir.join("ref_matched.wav"),
            &Render {
                l: rl.clone(),
                r: rr.clone(),
                sr: args.sr,
            },
        )?;

        let render_mid: Vec<f32> = ml.iter().zip(&mr).map(|(a, b)| 0.5 * (a + b)).collect();
        let ref_mid: Vec<f32> = rl.iter().zip(&rr).map(|(a, b)| 0.5 * (a + b)).collect();
        let render_ltas: Vec<Band> = ltas_third_octave(&render_mid, args.sr)
            .into_iter()
            .map(|(hz, db)| Band { hz, db })
            .collect();
        let ref_ltas: Vec<Band> = ltas_third_octave(&ref_mid, args.sr)
            .into_iter()
            .map(|(hz, db)| Band { hz, db })
            .collect();
        println!(
            "  {stem:<38} ltas_rms_diff {:>5.2} dB   centroid_delta {:>+6.0} Hz   crest_delta {:>+5.2} dB",
            ltas_rms_diff_db(&render_ltas, &ref_ltas),
            spectral_centroid(&render_mid, args.sr) - spectral_centroid(&ref_mid, args.sr),
            crest_db(&render_mid) - crest_db(&ref_mid),
        );
    }
    Ok(())
}

// ── ABX ────────────────────────────────────────────────────────────────────────

fn run_abx(args: &Args, a_stem: &str, b_stem: &str) -> Result<()> {
    let presets = load_presets(args)?;
    let di = phrase(Phrase::Chugs, args.sr);
    let opts = RenderOpts {
        sr: args.sr,
        width_override: args.width_override,
        ..RenderOpts::default()
    };
    let render_of = |stem: &str| -> Result<Render> {
        let preset = presets
            .iter()
            .find(|(s, _)| s == stem)
            .map(|(_, p)| p)
            .ok_or_else(|| anyhow::anyhow!("no preset stem '{stem}'"))?;
        Ok(render_preset(preset, &di, &opts))
    };
    let a = render_of(a_stem)?;
    let b = render_of(b_stem)?;
    // Level-match both to the quieter of the two.
    let la = lufs_integrated(&a.l, &a.r, args.sr);
    let lb = lufs_integrated(&b.l, &b.r, args.sr);
    let target = la.min(lb);
    let (al, ar) = match_loudness(&a.l, &a.r, args.sr, target);
    let (bl, br) = match_loudness(&b.l, &b.r, args.sr, target);

    // X is A or B chosen by a seeded LCG; the seed is printed and stored.
    let mut seed = 0x1234_5678u32;
    seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
    let x_is_a = (seed >> 8) & 1 == 0;
    let (xl, xr) = if x_is_a {
        (al.clone(), ar.clone())
    } else {
        (bl.clone(), br.clone())
    };

    let dir = args.out.join("abx");
    std::fs::create_dir_all(&dir)?;
    write_wav_f32_stereo(
        &dir.join("A.wav"),
        &Render {
            l: al,
            r: ar,
            sr: args.sr,
        },
    )?;
    write_wav_f32_stereo(
        &dir.join("B.wav"),
        &Render {
            l: bl,
            r: br,
            sr: args.sr,
        },
    )?;
    write_wav_f32_stereo(
        &dir.join("X.wav"),
        &Render {
            l: xl,
            r: xr,
            sr: args.sr,
        },
    )?;

    #[derive(Serialize)]
    struct Key<'a> {
        a: &'a str,
        b: &'a str,
        x: &'a str,
        seed: u32,
        lufs: f32,
    }
    std::fs::write(
        dir.join("key.toml"),
        toml::to_string_pretty(&Key {
            a: a_stem,
            b: b_stem,
            x: if x_is_a { a_stem } else { b_stem },
            seed,
            lufs: target,
        })?,
    )?;
    std::fs::write(
        dir.join("listening-notes.md"),
        format!(
            "# ABX listening notes\n\n- date:\n- listener:\n- playback system:\n\
             - preset A: {a_stem}\n- preset B: {b_stem}\n- trials:\n- answers:\n- confidence:\n- free text:\n"
        ),
    )?;
    println!(
        "ABX files written to {} (X = {}; seed {seed})",
        dir.display(),
        if x_is_a { a_stem } else { b_stem }
    );
    Ok(())
}

// ── Bench ──────────────────────────────────────────────────────────────────────

fn run_bench(args: &Args) -> Result<()> {
    let presets = load_presets(args)?;
    println!("realtime factor (rendering 10 s of chugs; warn if rtf > 0.5):");
    for rate in [44_100.0f32, 48_000.0, 96_000.0] {
        let di = phrase(Phrase::Chugs, rate);
        let di = di.repeat(6); // ~10 s
        for (stem, preset) in &presets {
            let opts = RenderOpts {
                sr: rate,
                width_override: args.width_override,
                ..RenderOpts::default()
            };
            let start = std::time::Instant::now();
            let _ = render_preset(preset, &di, &opts);
            let wall = start.elapsed().as_secs_f32();
            let audio = di.len() as f32 / rate;
            let rtf = wall / audio;
            let flag = if rtf > 0.5 { "  <-- over budget" } else { "" };
            println!("  {rate:>7.0} Hz  {stem:<38} rtf {rtf:>5.3}{flag}");
        }
    }
    Ok(())
}

// ── Main ───────────────────────────────────────────────────────────────────────

fn main() -> Result<()> {
    let args = parse_args()?;
    std::fs::create_dir_all(&args.out)
        .with_context(|| format!("creating {}", args.out.display()))?;

    if args.bench {
        return run_bench(&args);
    }
    if let Some((a, b)) = args.abx.clone() {
        return run_abx(&args, &a, &b);
    }

    let presets = load_presets(&args)?;
    let di = load_di(&args)?;
    let width = args.width_override.unwrap_or(PRESET_WIDTH);

    let mut reports = Vec::new();
    for (stem, preset) in &presets {
        let dir = args.out.join(stem);
        for (di_name, samples) in &di {
            let opts = RenderOpts {
                sr: args.sr,
                width_override: args.width_override,
                ..RenderOpts::default()
            };
            let render = render_preset(preset, samples, &opts);
            write_wav_f32_stereo(&dir.join(format!("{di_name}.wav")), &render)?;
            reports.push(measure(stem, di_name, &render, width));
        }
    }
    write_reports(&args.out, &reports)?;
    println!(
        "rendered {} preset(s) x {} DI(s) -> {}",
        presets.len(),
        di.len(),
        args.out.display()
    );

    run_ref(&args, &presets, &args.out)?;

    if let Some(path) = &args.write_baseline {
        let baseline = baseline_from(&reports, args.sr, Phrase::Chugs.name());
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, toml::to_string_pretty(&baseline)?)
            .with_context(|| format!("writing {}", path.display()))?;
        println!("baseline written: {}", path.display());
    }
    if let Some(path) = &args.check {
        check_baseline(path, &reports, args.sr)?;
    }
    Ok(())
}
