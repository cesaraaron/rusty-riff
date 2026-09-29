//! Offline preset rendering for the fidelity harness and tests.
//!
//! A fresh [`Params`] + [`DspChain`] is built per render; the signal is
//! processed in blocks through the full live chain, and a decaying tail is
//! appended so reverbs/delays ring out. Offline only — may allocate freely.

use crate::dsp::{DspChain, Params};
use crate::preset::Preset;
use anyhow::{Context, Result};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::Ordering::Relaxed;

/// How to render: sample rate, settling preroll, tail cap, optional master-width
/// override, and the processing block size.
#[derive(Clone, Copy, Debug)]
pub struct RenderOpts {
    pub sr: f32,
    /// Seconds of silence rendered first so filters/sag/bloom settle.
    pub preroll_s: f32,
    /// Maximum seconds of tail rendered after the DI.
    pub max_tail_s: f32,
    /// Override the preset's master width (e.g. `Some(1.0)` for a mono reference).
    pub width_override: Option<f32>,
    pub block: usize,
}

impl Default for RenderOpts {
    fn default() -> Self {
        Self {
            sr: 48_000.0,
            preroll_s: 0.5,
            max_tail_s: 4.0,
            width_override: None,
            block: 512,
        }
    }
}

/// A stereo float render.
#[derive(Clone, Debug)]
pub struct Render {
    pub l: Vec<f32>,
    pub r: Vec<f32>,
    pub sr: f32,
}

/// Render `preset` over `di` (mono) and return the stereo result.
pub fn render_preset(preset: &Preset, di: &[f32], opts: &RenderOpts) -> Render {
    let params = Arc::new(Params::new());
    preset.apply(&params);
    if let Some(width) = opts.width_override {
        params.master_width.store(width, Relaxed);
    }
    let mut chain = DspChain::new(opts.sr, Arc::clone(&params));

    let block = opts.block.max(1);
    let mut l: Vec<f32> = Vec::new();
    let mut r: Vec<f32> = Vec::new();

    // Settle the chain, then render the DI, then its tail.
    let preroll = (opts.sr * opts.preroll_s).max(0.0) as usize;
    render_into(&mut chain, &vec![0.0f32; preroll], &mut l, &mut r, block);
    render_into(&mut chain, di, &mut l, &mut r, block);

    let max_tail = (opts.sr * opts.max_tail_s).max(0.0) as usize;
    let hold = (opts.sr * 0.25) as usize;
    let silence = vec![0.0f32; max_tail.min(block)];
    let mut rendered = 0usize;
    let mut quiet_for = 0usize;
    while rendered < max_tail {
        let n = block.min(max_tail - rendered);
        let mut bl = vec![0.0f32; n];
        let mut br = vec![0.0f32; n];
        chain.process_block(&silence[..n], &mut bl, &mut br);
        let peak = bl
            .iter()
            .chain(br.iter())
            .fold(0.0f32, |m, &x| m.max(x.abs()));
        l.extend_from_slice(&bl);
        r.extend_from_slice(&br);
        rendered += n;
        if peak < 1e-4 {
            quiet_for += n;
            if quiet_for >= hold {
                break;
            }
        } else {
            quiet_for = 0;
        }
    }

    Render { l, r, sr: opts.sr }
}

fn render_into(
    chain: &mut DspChain,
    input: &[f32],
    l: &mut Vec<f32>,
    r: &mut Vec<f32>,
    block: usize,
) {
    let mut bl = vec![0.0f32; block];
    let mut br = vec![0.0f32; block];
    for chunk in input.chunks(block) {
        let n = chunk.len();
        chain.process_block(chunk, &mut bl[..n], &mut br[..n]);
        l.extend_from_slice(&bl[..n]);
        r.extend_from_slice(&br[..n]);
    }
}

/// Write a render as a 32-bit float stereo WAV.
pub fn write_wav_f32_stereo(path: &Path, render: &Render) -> Result<()> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    let spec = hound::WavSpec {
        channels: 2,
        sample_rate: render.sr as u32,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    let mut writer = hound::WavWriter::create(path, spec)
        .with_context(|| format!("creating {}", path.display()))?;
    for (l, r) in render.l.iter().zip(&render.r) {
        writer.write_sample(*l)?;
        writer.write_sample(*r)?;
    }
    writer.finalize()?;
    Ok(())
}

/// Decode any supported audio file and fold it to mono at `sr`, exactly as the
/// exporter sums a raw take.
pub fn read_wav_mono(path: &Path, sr: f32) -> Result<Vec<f32>> {
    let decoded = crate::practice::decode_track(path, sr)?;
    let l = decoded.track.l;
    let r = decoded.track.r;
    let n = l.len().min(r.len());
    Ok((0..n).map(|i| 0.5 * (l[i] + r[i])).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::synth::{Phrase, phrase};
    use crate::preset::PresetSource;

    fn bundled(name: &str) -> Preset {
        let path = std::path::PathBuf::from("presets").join(name);
        Preset::load(&path, PresetSource::System)
            .unwrap_or_else(|e| panic!("load {}: {e}", path.display()))
    }

    #[test]
    fn render_preset_is_bit_deterministic() {
        let preset = bundled("pink_floyd_time_solo.toml");
        let di = phrase(Phrase::Chugs, 48_000.0);
        let opts = RenderOpts {
            max_tail_s: 0.0,
            ..RenderOpts::default()
        };
        let a = render_preset(&preset, &di, &opts);
        let b = render_preset(&preset, &di, &opts);
        assert_eq!(a.l, b.l, "left channel diverged between runs");
        assert_eq!(a.r, b.r, "right channel diverged between runs");
    }

    #[test]
    fn every_bundled_preset_renders_finite_and_bounded() {
        let di = phrase(Phrase::Chugs, 48_000.0);
        // The tail is irrelevant to finiteness/bounds and would only add runtime.
        let opts = RenderOpts {
            max_tail_s: 0.0,
            ..RenderOpts::default()
        };
        let mut count = 0;
        for entry in std::fs::read_dir("presets").expect("presets/ dir") {
            let path = entry.unwrap().path();
            if path.extension().is_none_or(|ext| ext != "toml") {
                continue;
            }
            count += 1;
            let preset = Preset::load(&path, PresetSource::System)
                .unwrap_or_else(|e| panic!("parse {}: {e}", path.display()));
            let render = render_preset(&preset, &di, &opts);
            assert!(
                render
                    .l
                    .iter()
                    .chain(render.r.iter())
                    .all(|x| x.is_finite()),
                "{} produced a non-finite sample",
                path.display()
            );
            let peak = render
                .l
                .iter()
                .chain(render.r.iter())
                .fold(0.0f32, |m, &x| m.max(x.abs()));
            assert!(
                peak <= 1.0,
                "{} peak {peak} exceeded the output ceiling",
                path.display()
            );
        }
        assert!(count > 0, "no bundled presets found");
    }

    #[test]
    fn width_override_changes_the_render() {
        let preset = bundled("pink_floyd_time_solo.toml");
        let di = phrase(Phrase::Chugs, 48_000.0);
        let mono = render_preset(
            &preset,
            &di,
            &RenderOpts {
                width_override: Some(1.0),
                max_tail_s: 0.5,
                ..RenderOpts::default()
            },
        );
        let wide = render_preset(
            &preset,
            &di,
            &RenderOpts {
                width_override: Some(1.3),
                max_tail_s: 0.5,
                ..RenderOpts::default()
            },
        );
        assert!(
            mono.l.iter().zip(&wide.l).any(|(a, b)| a != b),
            "width override had no effect"
        );
    }
}
