//! Integration test over the bundled preset bank: every `presets/*.toml` must
//! parse and survive the real `render_preset` path deterministically, finite,
//! within the output ceiling, and non-silent. Mirrors `tests/fidelity_harness.rs`
//! but sweeps the whole bank. (Does not spawn the example binary.)
//!
//! Runtime is kept down by rendering a short slice of the `Chugs` phrase with
//! `max_tail_s: 0.0` and a brief settling preroll — the tail and the full phrase
//! only shape decay, not parsing, finiteness, bounds, or determinism — and by
//! checking presets on parallel threads (each render rebuilds the DSP chain, so
//! a per-preset cost dominates the sweep).

use rusty_riff::analysis::render::{RenderOpts, render_preset};
use rusty_riff::analysis::synth::{Phrase, phrase};
use rusty_riff::preset::{Preset, PresetSource};

fn bundled_paths() -> Vec<std::path::PathBuf> {
    let mut paths: Vec<_> = std::fs::read_dir("presets")
        .expect("presets/ dir")
        .map(|entry| entry.expect("read presets/ entry").path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "toml"))
        .collect();
    paths.sort();
    paths
}

/// Render one preset twice and return the first check that fails.
fn check_preset(path: &std::path::Path, di: &[f32], opts: &RenderOpts) -> Result<(), String> {
    let preset = Preset::load(path, PresetSource::System)
        .map_err(|e| format!("parse {}: {e}", path.display()))?;

    let a = render_preset(&preset, di, opts);
    let b = render_preset(&preset, di, opts);

    if a.l != b.l {
        return Err(format!(
            "{} render is not deterministic (L)",
            path.display()
        ));
    }
    if a.r != b.r {
        return Err(format!(
            "{} render is not deterministic (R)",
            path.display()
        ));
    }

    let peak =
        a.l.iter()
            .chain(a.r.iter())
            .fold(0.0f32, |m, &x| m.max(x.abs()));
    if !peak.is_finite() {
        return Err(format!("{} produced a non-finite sample", path.display()));
    }
    if peak > 1.0 {
        return Err(format!(
            "{} peak {peak} exceeded the output ceiling",
            path.display()
        ));
    }
    if peak <= 0.0 {
        return Err(format!("{} rendered silence over the DI", path.display()));
    }
    Ok(())
}

#[test]
fn every_bundled_preset_parses_renders_and_is_deterministic() {
    // A short slice of the phrase (plucks still land at 0.0 s and 0.35 s).
    let full = phrase(Phrase::Chugs, 48_000.0);
    let di = &full[..(48_000.0 * 0.6) as usize];
    let opts = RenderOpts {
        preroll_s: 0.1,
        max_tail_s: 0.0,
        ..RenderOpts::default()
    };

    let paths = bundled_paths();
    assert_eq!(
        paths.len(),
        20,
        "bundled preset count changed; update the expectation if intentional"
    );

    let failures: Vec<String> = std::thread::scope(|scope| {
        let handles: Vec<_> = paths
            .iter()
            .map(|path| scope.spawn(|| check_preset(path, di, &opts)))
            .collect();
        handles
            .into_iter()
            .filter_map(|handle| handle.join().expect("preset worker panicked").err())
            .collect()
    });

    assert!(
        failures.is_empty(),
        "{} of {} bundled presets failed:\n{}",
        failures.len(),
        paths.len(),
        failures.join("\n")
    );
}
