//! Plain timing harness for the rusty-riff DSP chain — no criterion, no new
//! dependencies: just a `main` that times and prints.
//!
//! Build the release binary before trusting the numbers (debug construction is
//! dominated by unoptimized filter-coefficient setup):
//!
//!     cargo run --release --example bench
//!
//! It reports:
//!   1. `DspChain::new` construction time (ms),
//!   2. per-480-frame-block `process_block` cost (µs) and the fraction of the
//!      10 ms realtime budget it consumes, for a couple of representative
//!      bundled presets,
//!   3. `CabBank::new` construction time (ms).

use anyhow::Result;
use rusty_riff::dsp::cab::CabBank;
use rusty_riff::dsp::{DspChain, Params};
use rusty_riff::preset::{self, Preset, PresetSource};
use std::f64::consts::TAU;
use std::hint::black_box;
use std::sync::Arc;
use std::time::{Duration, Instant};

const SR: f32 = 48_000.0;
/// 480 frames @ 48 kHz = the 10 ms audio-callback budget.
const BLOCK: usize = 480;
/// Construction is timed as the best of several cold builds.
const CONSTRUCT_RUNS: usize = 5;
const WARMUP_BLOCKS: usize = 25;
/// ~20 s of audio at 480 frames/block.
const MEASURE_BLOCKS: usize = 2_000;

fn main() -> Result<()> {
    let presets = pick_presets()?;

    println!("rusty-riff DSP bench — {SR:.0} Hz, {BLOCK}-frame blocks (10.0 ms budget)\n");

    let params = params_for(&presets[0].1);
    bench_chain_construction(&params);
    bench_cab_construction();

    println!();
    for (label, preset) in &presets {
        bench_process_block(label, preset);
    }
    Ok(())
}

/// A representative pair from the surviving bundled set — a lead and a
/// rhythm/chorus part — falling back to the first two bundled presets if the
/// named ones are absent.
///
/// These were "November Rain" and "Back in Black" until 2026-09-29, when twelve
/// bundled presets were retired (see `docs/retired-presets.md`). The comment
/// above used to say "a high-gain lead and a crunch rhythm"; no surviving preset
/// is a high-gain lead any more, so the pair is a Hiwatt lead and a Tweed
/// rhythm instead.
fn pick_presets() -> Result<Vec<(String, Preset)>> {
    const WANTED: [&str; 2] = ["Time (Solo)", "Stairway"];

    let mut bundled: Vec<Preset> = preset::load_all()
        .into_iter()
        .filter(|p| p.source == PresetSource::System)
        .collect();
    anyhow::ensure!(
        !bundled.is_empty(),
        "no bundled presets found — run this from the repo root (presets/ present)"
    );

    let mut chosen = Vec::new();
    for want in WANTED {
        if let Some(i) = bundled.iter().position(|p| p.name.contains(want)) {
            let p = bundled.remove(i);
            chosen.push((p.name.clone(), p));
        }
    }
    for p in bundled {
        if chosen.len() >= WANTED.len() {
            break;
        }
        chosen.push((p.name.clone(), p));
    }
    Ok(chosen)
}

/// A default `Params` with `preset` applied, shared by `Arc` like the engine does.
fn params_for(preset: &Preset) -> Arc<Params> {
    let params = Arc::new(Params::new());
    preset.apply(&params);
    params
}

fn bench_chain_construction(params: &Arc<Params>) {
    let mut best = Duration::MAX;
    for _ in 0..CONSTRUCT_RUNS {
        let start = Instant::now();
        let chain = DspChain::new(SR, Arc::clone(params));
        best = best.min(start.elapsed());
        black_box(&chain);
    }
    println!(
        "chain construction  DspChain::new   min of {CONSTRUCT_RUNS}: {:>8.2} ms",
        ms(best)
    );
}

fn bench_cab_construction() {
    let mut best = Duration::MAX;
    for _ in 0..CONSTRUCT_RUNS {
        let start = Instant::now();
        let cab = CabBank::new(SR);
        best = best.min(start.elapsed());
        black_box(&cab);
    }
    println!(
        "cab construction    CabBank::new    min of {CONSTRUCT_RUNS}: {:>8.2} ms",
        ms(best)
    );
}

fn bench_process_block(label: &str, preset: &Preset) {
    let params = params_for(preset);
    let mut chain = DspChain::new(SR, Arc::clone(&params));

    let input: Vec<f32> = (0..BLOCK)
        .map(|i| 0.2 * (TAU * 220.0 * i as f64 / SR as f64).sin() as f32)
        .collect();
    let mut out_l = vec![0.0f32; BLOCK];
    let mut out_r = vec![0.0f32; BLOCK];

    for _ in 0..WARMUP_BLOCKS {
        chain.process_block(&input, &mut out_l, &mut out_r);
    }

    let start = Instant::now();
    for _ in 0..MEASURE_BLOCKS {
        chain.process_block(&input, &mut out_l, &mut out_r);
    }
    let elapsed = start.elapsed();
    black_box(out_l[0]);

    let budget_us = BLOCK as f64 / SR as f64 * 1e6;
    let us_per_block = elapsed.as_secs_f64() * 1e6 / MEASURE_BLOCKS as f64;
    let realtime_pct = us_per_block / budget_us * 100.0;
    println!(
        "process_block       {label}\n                    {:>8.2} µs/block   ({realtime_pct:>6.2}% of the {:.1} ms realtime budget)",
        us_per_block,
        budget_us / 1e3,
    );
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1e3
}
