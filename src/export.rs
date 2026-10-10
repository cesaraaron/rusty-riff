//! Offline guitar-take export.
//!
//! Renders the timeline's **unmuted raw takes** — and only those — through a
//! fresh, deterministic instance of the rig to a stereo 32-bit float WAV at the
//! project sample rate. Imports, the metronome, live input and the click are
//! excluded by construction.
//!
//! By default the whole session is rendered: tick 0 through the last take plus a
//! capped effect tail. Setting [`ExportJob::range_ticks`] instead renders exactly
//! that `[start, end)` span (a **loop-region export**) with no appended tail. The
//! chain is still warmed from frame 0 so delay/reverb state is continuous, but
//! only the window is written.
//!
//! The render runs on a worker thread with its own [`DspChain`], built from a
//! **snapshot** of the rig settings (`Preset`), so knob moves during a render do
//! not affect it. External third-party processors cannot be cloned faithfully yet
//! (no state capture), so the UI refuses to export while an AU amp or CLAP insert
//! is loaded; the external **IR** is supported and re-loaded at the export rate.
//!
//! ## Export contract
//!
//! **Range.** By default tick zero to the end of the latest **unmuted raw
//! take**, plus enough silence to render the current rig's delay/reverb tails,
//! with a capped maximum and a silence threshold so a long feedback tail cannot
//! extend the render forever. Muted takes and imported tracks never extend the
//! range. With no takes, the export explains there is nothing to render. The
//! loop region affects monitoring and recording but does not silently truncate
//! the export — a region export is a separate, explicitly labelled mode
//! ([`ExportJob::range_ticks`]).
//!
//! **Snapshot.** At export start every source is frozen: raw take IDs and
//! assets, starts, gains/mutes, the project rate, and the whole rig
//! (amp/pedal/chain/master values, built-in vs external cab, AU amp mode,
//! loaded plugins and their settings). Live UI changes afterwards apply only to
//! **subsequent** exports. A fresh render graph starts from known DSP state,
//! processes in deterministic blocks, and keeps filter and plugin state across
//! those blocks. Each source is resampled from its stored original rate
//! off-thread. Output is a stereo 32-bit float WAV written to a temporary file
//! beside the destination and renamed on success, so progress and cancellation
//! never leave a partial final WAV.
//!
//! **External-rig parity.** The live `DspChain::process_block` has a post-rack
//! CLAP insert and an AU amp override with amp-only/full-rig and latency
//! handling. Reusing a live plugin instance from a worker thread is unsafe, so
//! the export builds separate instances on the worker. Some plugins cannot be
//! faithfully reconstructed from exposed parameters alone; when that is the case
//! the export **refuses** rather than silently producing a built-in-only file,
//! and the raw sources are left intact.
//!
//! **Comparison.** For a single raw take with no backing/click/live input, the
//! rendered output is compared against an appropriately captured monitored
//! take-bus segment at matched start, rate and initial effect state. Gain
//! changes, staggered takes, mute, loop, the output limiter, multiple
//! callback/block sizes and external-plugin latency are all in scope.
//! Sample-exact equivalence is not required for nondeterministic plugins; those
//! are documented exceptions with a stated audible-parity expectation.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering::Relaxed};
use std::sync::mpsc::{self, Receiver};

use anyhow::{Context, Result, bail};

use crate::dsp::cab::{ExternalIrCab, LIVE_MAX_IR_LEN, load_ir};
use crate::dsp::{DspChain, Params, StereoInsert};
use crate::practice::decode_track;
use crate::preset::Preset;

/// Frames per render block. Must stay ≤ [`crate::audio::MAX_BLOCK`].
const BLOCK: usize = 512;
/// Maximum effect tail rendered after the last take, in seconds.
const TAIL_CAP_SECS: f32 = 12.0;
/// A tail block is considered silent below this peak.
const TAIL_THRESHOLD: f32 = 1.0e-4;
/// How long the tail must stay silent before the render stops, in seconds.
const TAIL_HOLD_SECS: f32 = 0.25;
/// Silence rendered through the chain before the first written frame, in seconds,
/// so the rig reaches steady state instead of playing the first note into a cold
/// graph. Matches the offline harness's `preroll_s`.
const PREROLL_SECS: f32 = 0.5;

/// One raw take to place on the export timeline. `path` is decoded/resampled at
/// the export rate off-thread.
#[derive(Clone)]
pub struct ExportClip {
    pub path: PathBuf,
    pub start_ticks: u64,
    pub gain: f32,
}

/// Where an external processor belongs in the export chain.
pub enum ExternalPlacement {
    /// Post-rack stereo insert.
    Insert,
    /// Amp-position override. `amp_only` keeps the built-in cab/IR in the path.
    Amp {
        amp_only: bool,
        latency_frames: usize,
    },
}

/// A freshly built external processor plus a token that must stay alive for the
/// whole render (a CLAP `LoadedPlugin` unloads the bundle when dropped).
pub struct ExternalInstance {
    pub insert: Box<dyn StereoInsert>,
    pub placement: ExternalPlacement,
    pub keepalive: Box<dyn std::any::Any>,
}

/// Builder run **on the export worker** so a plugin instance is created and owned
/// on that thread. The closure captures only `Send` identity/state data.
pub type BuildExternal = Box<dyn FnOnce() -> anyhow::Result<ExternalInstance> + Send>;

/// A frozen export request.
pub struct ExportJob {
    pub dest: PathBuf,
    pub sample_rate: u32,
    pub project_sample_rate: u32,
    pub clips: Vec<ExportClip>,
    /// Rig snapshot applied to a private [`Params`] instance.
    pub rig: Preset,
    /// Optional `[start, end)` window in project ticks. `None` renders the whole
    /// session (tick 0 → last take + effect tail); `Some` renders exactly that
    /// span (a loop-region export), with no appended tail.
    pub range_ticks: Option<(u64, u64)>,
    /// External IR source, re-loaded at the export rate when set.
    pub ir_path: Option<PathBuf>,
    pub ir_active: bool,
    /// CLAP insert, re-instantiated with captured state on the worker.
    pub insert: Option<BuildExternal>,
    /// AU amp override, re-instantiated with captured state on the worker.
    pub amp: Option<BuildExternal>,
}

/// Handle to a running export on a worker thread.
pub struct ExportHandle {
    pub rx: Receiver<Result<PathBuf>>,
    pub cancel: Arc<AtomicBool>,
    /// Progress in per-mille (0..=1000).
    pub progress: Arc<AtomicU32>,
}

impl ExportHandle {
    /// Current progress as a percentage.
    pub fn percent(&self) -> u32 {
        self.progress.load(Relaxed) / 10
    }

    pub fn cancel(&self) {
        self.cancel.store(true, Relaxed);
    }
}

/// Spawn the render worker and return a handle to poll.
pub fn spawn(job: ExportJob) -> ExportHandle {
    let cancel = Arc::new(AtomicBool::new(false));
    let progress = Arc::new(AtomicU32::new(0));
    let (tx, rx) = mpsc::channel();
    let worker_cancel = Arc::clone(&cancel);
    let worker_progress = Arc::clone(&progress);
    std::thread::spawn(move || {
        let result = run(job, &worker_progress, &worker_cancel);
        let _ = tx.send(result);
    });
    ExportHandle {
        rx,
        cancel,
        progress,
    }
}

fn run(mut job: ExportJob, progress: &AtomicU32, cancel: &AtomicBool) -> Result<PathBuf> {
    if job.clips.is_empty() {
        bail!("no unmuted raw takes to export");
    }
    let sr = job.sample_rate as f32;

    // A private, deterministic rig built from the frozen snapshot.
    let params = Arc::new(Params::new());
    job.rig.apply(&params);
    params.cab_external_loaded.store(false, Relaxed);
    params.cab_external_active.store(false, Relaxed);
    let mut chain = DspChain::new(sr, Arc::clone(&params));

    // External CLAP insert / AU amp, built on this worker thread from captured
    // state. Both keep-alives are held for the whole render.
    let _insert_keepalive = if let Some(build) = job.insert.take() {
        let instance = build()?;
        chain.set_insert(Some(instance.insert));
        Some(instance.keepalive)
    } else {
        None
    };
    let _amp_keepalive = if let Some(build) = job.amp.take() {
        let instance = build()?;
        let (amp_only, latency_frames) = match instance.placement {
            ExternalPlacement::Amp {
                amp_only,
                latency_frames,
            } => (amp_only, latency_frames),
            ExternalPlacement::Insert => (false, 0),
        };
        params.amp_external_loaded.store(true, Relaxed);
        params.amp_external_active.store(true, Relaxed);
        params.amp_external_amp_only.store(amp_only, Relaxed);
        params.amp_external_latency.store(latency_frames, Relaxed);
        chain.set_ext_amp(Some(instance.insert));
        Some(instance.keepalive)
    } else {
        None
    };

    if let Some(path) = &job.ir_path {
        // The *same* cap the live path used. `load_ir` unit-energy-normalises,
        // so loading this IR longer offline than the engine did would render a
        // quieter, duller cab than the one that was just monitored. An export
        // has to reproduce what you heard.
        match load_ir(path, sr, LIVE_MAX_IR_LEN) {
            Ok(loaded) => {
                if job.ir_active {
                    params.cab_external_loaded.store(true, Relaxed);
                    params.cab_external_active.store(true, Relaxed);
                    let _ =
                        chain.replace_external_cab(Some(Box::new(ExternalIrCab::new(sr, loaded))));
                }
            }
            // A selected IR that cannot be re-loaded would silently render through
            // the built-in cab, which is not a faithful export.
            Err(e) if job.ir_active => {
                bail!("loading external IR {}: {e}", path.display());
            }
            Err(_) => {}
        }
    }
    render_with_chain(&job, progress, cancel, chain)
}

/// Decode every clip, then stream the mixed take bus through `chain`.
fn render_with_chain(
    job: &ExportJob,
    progress: &AtomicU32,
    cancel: &AtomicBool,
    mut chain: DspChain,
) -> Result<PathBuf> {
    let sr = job.sample_rate as f32;
    let rate = f64::from(job.project_sample_rate);

    let mut decoded: Vec<DecodedClip> = Vec::with_capacity(job.clips.len());
    for clip in &job.clips {
        let sample = decode_track(&clip.path, sr)
            .with_context(|| format!("decoding {}", clip.path.display()))?;
        let l = sample.track.l;
        let r = sample.track.r;
        let n = l.len().min(r.len());
        // Raw takes are mono (duplicated to stereo on install); sum back to mono.
        let mono: Vec<f32> = (0..n).map(|i| 0.5 * (l[i] + r[i])).collect();
        let start = if rate > 0.0 {
            (clip.start_ticks as f64 * f64::from(sr) / rate).round() as usize
        } else {
            clip.start_ticks as usize
        };
        decoded.push(DecodedClip {
            start,
            mono,
            gain: clip.gain,
        });
    }

    let program = decoded
        .iter()
        .map(|c| c.start + c.mono.len())
        .max()
        .unwrap_or(0);
    let tail_cap = (sr * TAIL_CAP_SECS) as usize;

    // Render window. `None` = whole session through the effect tail. `Some` = a
    // loop-region export: exactly that span, no appended tail. The chain is still
    // processed from frame 0 (so state is warmed) but only `[win_start, win_end)`
    // is written.
    let (win_start, win_end, bounded) = match job.range_ticks {
        Some((start_ticks, end_ticks)) => {
            if end_ticks <= start_ticks {
                bail!("the export range is empty");
            }
            let start = (start_ticks as f64 * f64::from(sr) / rate).round() as usize;
            let end = (end_ticks as f64 * f64::from(sr) / rate).round() as usize;
            if end <= start {
                bail!("the export range is too short to render");
            }
            (start, end, true)
        }
        None => (0, program.saturating_add(tail_cap), false),
    };
    let window_len = win_end.saturating_sub(win_start).max(1);
    let total_estimate = window_len;

    // Write to a temp file beside the destination so a failure never leaves a
    // partial file where the user expects the finished WAV.
    let tmp = job.dest.with_extension("wav.tmp");
    if let Some(parent) = job.dest.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    let spec = hound::WavSpec {
        channels: 2,
        sample_rate: job.sample_rate,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    let mut writer = hound::WavWriter::create(&tmp, spec)
        .with_context(|| format!("creating {}", tmp.display()))?;

    let mut mono = vec![0.0f32; BLOCK];
    let mut left = vec![0.0f32; BLOCK];
    let mut right = vec![0.0f32; BLOCK];
    let mut written = 0usize;
    let hold_frames = (sr * TAIL_HOLD_SECS) as usize;
    let mut silent_for = 0usize;

    // Settle the chain on silence before the first written frame. Without this
    // the render starts with every stateful stage cold: the amps' rectifier-sag
    // and bias envelopes at zero, the cab convolver's delay line empty, the
    // reverb and delay buffers silent, and the oversampler histories unwarmed.
    // The first pluck of a take then plays into a rig that has not yet found its
    // operating point. The offline harness already prerolls (`analysis::render.rs`,
    // `preroll_s = 0.5`) — this is the same idea, and the reason the harness and
    // the exporter previously measured different things.
    if PREROLL_SECS > 0.0 {
        let mut settle = vec![0.0f32; BLOCK];
        let mut sl = vec![0.0f32; BLOCK];
        let mut sr_ = vec![0.0f32; BLOCK];
        let mut left_to_run = (sr * PREROLL_SECS) as usize;
        while left_to_run > 0 {
            let n = BLOCK.min(left_to_run);
            settle[..n].fill(0.0);
            chain.process_block(&settle[..n], &mut sl[..n], &mut sr_[..n]);
            left_to_run -= n;
        }
    }

    while written < win_end {
        if cancel.load(Relaxed) {
            drop(writer);
            let _ = std::fs::remove_file(&tmp);
            bail!("export cancelled");
        }
        let remaining = win_end.saturating_sub(written);
        let n = BLOCK.min(remaining);
        mono[..n].fill(0.0);
        for clip in &decoded {
            // Overlap-add this clip's contribution to the block, clamped to the
            // render window so a region export never writes outside it.
            let clip_end = clip.start + clip.mono.len();
            let lo = (written).max(clip.start).max(win_start);
            let hi = (written + n).min(clip_end).min(win_end);
            if lo < hi {
                for out in lo..hi {
                    mono[out - written] += clip.mono[out - clip.start] * clip.gain;
                }
            }
        }

        chain.process_block(&mono[..n], &mut left[..n], &mut right[..n]);

        // Write only the frames inside the window.
        let lo = written.max(win_start);
        let hi = (written + n).min(win_end);
        for i in lo..hi {
            writer.write_sample(left[i - written])?;
            writer.write_sample(right[i - written])?;
        }

        // The whole-session render stops once the effect tail goes silent; a
        // bounded region always runs to its out-point.
        if !bounded && written >= program {
            let peak = left[..n]
                .iter()
                .chain(right[..n].iter())
                .fold(0.0f32, |m, &x| m.max(x.abs()));
            if peak < TAIL_THRESHOLD {
                silent_for += n;
                if silent_for >= hold_frames {
                    break;
                }
            } else {
                silent_for = 0;
            }
        }

        written += n;
        let done = written.saturating_sub(win_start);
        let permille = (done.saturating_mul(1000) / total_estimate).min(1000) as u32;
        progress.store(permille, Relaxed);
    }

    writer.finalize().context("finalizing export WAV")?;
    std::fs::rename(&tmp, &job.dest)
        .with_context(|| format!("moving export into {}", job.dest.display()))?;
    progress.store(1000, Relaxed);
    Ok(job.dest.clone())
}

struct DecodedClip {
    start: usize,
    mono: Vec<f32>,
    gain: f32,
}

/// Build a snapshot [`Preset`] for a job from live parameters. Kept here so the
/// UI can freeze the rig without knowing the preset internals.
pub fn snapshot_rig(params: &Params) -> Preset {
    Preset::from_params("export".to_owned(), None, params)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a job for a single take and render it, returning the stereo output.
    /// `tag` keeps parallel test temp dirs apart.
    fn render_take(tag: &str, rig: Preset, samples: &[f32]) -> Vec<(f32, f32)> {
        let dir =
            std::env::temp_dir().join(format!("rusty-riff-export-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");
        let take = dir.join("take.wav");
        write_mono_wav(&take, 48_000, samples);

        let job = ExportJob {
            dest: dir.join("out.wav"),
            sample_rate: 48_000,
            project_sample_rate: 48_000,
            clips: vec![ExportClip {
                path: take,
                start_ticks: 0,
                gain: 1.0,
            }],
            rig,
            range_ticks: Some((0, 48_000)), // exactly 1 s, no appended tail
            ir_path: None,
            ir_active: false,
            insert: None,
            amp: None,
        };
        let cancel = AtomicBool::new(false);
        let progress = AtomicU32::new(0);
        let out = run(job, &progress, &cancel).expect("export");

        let mut reader = hound::WavReader::open(&out).expect("open out");
        let raw: Vec<f32> = reader
            .samples::<f32>()
            .map(|s| s.expect("read sample"))
            .collect();
        let _ = std::fs::remove_dir_all(&dir);
        raw.chunks(2).map(|c| (c[0], c[1])).collect()
    }

    /// The export must preroll the chain before writing the first frame.
    ///
    /// The preroll exists because a cold graph has every stateful stage at rest:
    /// the amps' rectifier-sag and dynamic-bias envelopes at zero, the cab
    /// convolver's delay line empty, the reverb and delay buffers silent. The
    /// first pluck would be rendered into a rig that has not found its operating
    /// point. The offline harness already did this (`analysis/render.rs`,
    /// `preroll_s = 0.5`) — this is the same idea, and the reason the harness and
    /// the exporter previously measured different things.
    ///
    /// What this test actually pins is the *dangerous* half: a preroll that
    /// leaks into the written audio would silently shift the whole render later
    /// by `PREROLL_SECS` (24 000 samples here), so every take would land after
    /// the beat it was recorded on. So the onset is measured against the input's
    /// own position plus only the convolver's 128-sample latency — and asserted
    /// to be nowhere near a preroll's worth of extra delay.
    #[test]
    fn preroll_warms_the_chain_without_offsetting_the_render() {
        // The preroll must be non-zero: the exporter used to render from a
        // completely cold chain. (A zero here is caught by the offset assertion
        // below anyway, but silently zeroing it should be an obvious failure.)
        const { assert!(PREROLL_SECS > 0.0) }

        let params = Params::new();
        // A long, obvious tail so the reverb and delay buffers are plainly part
        // of the state being warmed.
        params.rev_enabled.store(true, Relaxed);
        params.rev_mix.store(0.9, Relaxed);
        params.rev_room.store(0.95, Relaxed);
        let rig = snapshot_rig(&params);

        // Silence, then a pluck at a known offset.
        const PLUCK_AT: usize = 1200;
        let mut samples = vec![0.0f32; 4800];
        for (i, s) in samples[PLUCK_AT..PLUCK_AT + 2400].iter_mut().enumerate() {
            let t = i as f32 / 48_000.0;
            *s = (-t * 12.0).exp() * (2.0 * std::f32::consts::PI * 220.0 * t).sin() * 0.6;
        }
        let out = render_take("preroll", rig, &samples);
        assert!(out.len() >= 3600, "render too short: {}", out.len());

        // The take must be audible where it was placed.
        let head_rms: f32 = out[PLUCK_AT..PLUCK_AT + 2000]
            .iter()
            .map(|&(l, r)| ((l * l + r * r) * 0.5).sqrt())
            .sum::<f32>()
            / 2000.0;
        assert!(
            head_rms > 0.001,
            "export is silent over the take: {head_rms}"
        );

        // The onset lands where the input was, plus the cab convolver's 128-sample
        // latency, the master limiter's lookahead, and a few samples of amp
        // front-end phase. A leaked preroll would push it out by
        // (48_000 * PREROLL_SECS) = 24 000 samples. The threshold sits above the
        // amp's hum/noise floor (C5), which is no longer bit-exact zero.
        let onset = out
            .iter()
            .position(|&(l, r)| l.abs() > 0.01 || r.abs() > 0.01)
            .expect("render is entirely silent");
        let limiter_latency = crate::dsp::limiter::Limiter::new(48_000.0).latency();
        let expected = PLUCK_AT + 128 + limiter_latency;
        assert!(
            onset.abs_diff(expected) < 64,
            "onset at {onset}, expected ~{expected} (input {PLUCK_AT} + 128 cab \
             convolution + {limiter_latency} limiter). A large offset means the \
             preroll leaked into the output."
        );
        let preroll_frames = (48_000.0 * PREROLL_SECS) as i64;
        assert!(
            onset.abs_diff(expected) < preroll_frames as usize / 10,
            "onset shifted by a preroll's worth of samples ({preroll_frames})"
        );
    }

    fn write_mono_wav(path: &std::path::Path, sr: u32, samples: &[f32]) {
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: sr,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        };
        let mut w = hound::WavWriter::create(path, spec).expect("wav");
        for &s in samples {
            w.write_sample(s).expect("sample");
        }
        w.finalize().expect("finalize");
    }

    #[test]
    fn exports_a_take_to_stereo_float() {
        let dir =
            std::env::temp_dir().join(format!("rusty-riff-export-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");
        let take = dir.join("take.wav");
        // A short decaying tone so the rig produces something.
        let samples: Vec<f32> = (0..4800).map(|i| (i as f32 * 0.05).sin() * 0.5).collect();
        write_mono_wav(&take, 48_000, &samples);

        let params = Params::new();
        let job = ExportJob {
            dest: dir.join("out.wav"),
            sample_rate: 48_000,
            project_sample_rate: 48_000,
            clips: vec![ExportClip {
                path: take,
                start_ticks: 0,
                gain: 1.0,
            }],
            rig: snapshot_rig(&params),
            range_ticks: None,
            ir_path: None,
            ir_active: false,
            insert: None,
            amp: None,
        };
        let cancel = AtomicBool::new(false);
        let progress = AtomicU32::new(0);
        let out = run(job, &progress, &cancel).expect("export");
        assert!(out.exists());

        let mut reader = hound::WavReader::open(&out).expect("open out");
        let spec = reader.spec();
        assert_eq!(spec.channels, 2);
        assert_eq!(spec.sample_rate, 48_000);
        assert!(spec.bits_per_sample == 32);
        let frames = reader.samples::<f32>().count() / 2;
        assert!(frames >= samples.len(), "export shorter than the take");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn places_clips_at_their_start() {
        let dir =
            std::env::temp_dir().join(format!("rusty-riff-export-start-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");
        let first = dir.join("first.wav");
        let second = dir.join("second.wav");
        let sample: Vec<f32> = (0..1200).map(|i| (i as f32 * 0.05).sin() * 0.4).collect();
        write_mono_wav(&first, 48_000, &sample);
        write_mono_wav(&second, 48_000, &sample);

        let params = Params::new();
        let start_ticks = 24_000; // 0.5 s at the project rate
        let job = ExportJob {
            dest: dir.join("out.wav"),
            sample_rate: 48_000,
            project_sample_rate: 48_000,
            clips: vec![
                ExportClip {
                    path: first,
                    start_ticks: 0,
                    gain: 1.0,
                },
                ExportClip {
                    path: second,
                    start_ticks,
                    gain: 1.0,
                },
            ],
            rig: snapshot_rig(&params),
            range_ticks: None,
            ir_path: None,
            ir_active: false,
            insert: None,
            amp: None,
        };
        let cancel = AtomicBool::new(false);
        let progress = AtomicU32::new(0);
        let out = run(job, &progress, &cancel).expect("export");
        let reader = hound::WavReader::open(&out).expect("open out");
        let frames = reader.len() as usize / 2;
        assert!(
            frames >= start_ticks as usize + sample.len(),
            "second clip's placement was dropped: {frames} frames"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn empty_job_is_rejected() {
        let dir =
            std::env::temp_dir().join(format!("rusty-riff-export-empty-{}", std::process::id()));
        let params = Params::new();
        let job = ExportJob {
            dest: dir.join("out.wav"),
            sample_rate: 48_000,
            project_sample_rate: 48_000,
            clips: Vec::new(),
            rig: snapshot_rig(&params),
            range_ticks: None,
            ir_path: None,
            ir_active: false,
            insert: None,
            amp: None,
        };
        let cancel = AtomicBool::new(false);
        let progress = AtomicU32::new(0);
        let err = run(job, &progress, &cancel).unwrap_err();
        assert!(err.to_string().contains("no unmuted"), "{err}");
    }

    #[test]
    fn range_export_writes_only_the_window() {
        let dir =
            std::env::temp_dir().join(format!("rusty-riff-export-range-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");
        let take = dir.join("take.wav");
        let samples: Vec<f32> = (0..480).map(|i| (i as f32 * 0.05).sin() * 0.5).collect();
        write_mono_wav(&take, 48_000, &samples);

        let params = Params::new();
        let job = ExportJob {
            dest: dir.join("out.wav"),
            sample_rate: 48_000,
            project_sample_rate: 48_000,
            clips: vec![ExportClip {
                path: take,
                start_ticks: 0,
                gain: 1.0,
            }],
            rig: snapshot_rig(&params),
            // Frames [120, 240) at 48 kHz.
            range_ticks: Some((120, 240)),
            ir_path: None,
            ir_active: false,
            insert: None,
            amp: None,
        };
        let cancel = AtomicBool::new(false);
        let progress = AtomicU32::new(0);
        let out = run(job, &progress, &cancel).expect("export");
        let reader = hound::WavReader::open(&out).expect("open out");
        let frames = reader.len() as usize / 2;
        assert_eq!(frames, 120, "region export wrote {frames} frames, want 120");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn range_export_rejects_an_empty_region() {
        let dir = std::env::temp_dir().join(format!(
            "rusty-riff-export-range-empty-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");
        let take = dir.join("take.wav");
        write_mono_wav(&take, 48_000, &[0.1f32; 480]);

        let params = Params::new();
        let job = ExportJob {
            dest: dir.join("out.wav"),
            sample_rate: 48_000,
            project_sample_rate: 48_000,
            clips: vec![ExportClip {
                path: take,
                start_ticks: 0,
                gain: 1.0,
            }],
            rig: snapshot_rig(&params),
            range_ticks: Some((240, 240)),
            ir_path: None,
            ir_active: false,
            insert: None,
            amp: None,
        };
        let cancel = AtomicBool::new(false);
        let progress = AtomicU32::new(0);
        let err = run(job, &progress, &cancel).unwrap_err();
        assert!(err.to_string().contains("empty"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
