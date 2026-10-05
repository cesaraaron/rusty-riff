//! Perceived loudness of each amp rig on a matched DI.
//!
//! Raw RMS is dominated by whatever low end a chug DI is full of; the ear
//! weights the mids far more, so we also measure the 300 Hz–5 kHz band. The
//! per-amp output trims are matched on this mid-band number (see
//! `dsp::amp::tests::amps_are_loudness_matched`), not the raw RMS.
use rusty_riff::dsp::amp::{self, AmpBank};
use rusty_riff::dsp::biquad::Biquad;
use rusty_riff::dsp::cab::CabBank;
use rusty_riff::dsp::{AmpModel, CabModel};
const SR: f32 = 48_000.0;

/// Natural cab pairing, for context only (the trims are amp-only).
fn cab_for(am: AmpModel) -> CabModel {
    match am {
        AmpModel::Marshall => CabModel::Marshall,
        AmpModel::Mesa => CabModel::Mesa,
        AmpModel::Randall => CabModel::Orange,
        AmpModel::Vox => CabModel::Vox,
        AmpModel::Hiwatt => CabModel::Wem,
        AmpModel::Plexi => CabModel::Marshall,
        AmpModel::Fender => CabModel::Fender,
        AmpModel::Supro => CabModel::Supro,
        AmpModel::Tweed => CabModel::Fender,
    }
}

fn main() {
    // Simple deterministic chug DI: low-E bursts (enough for level comparison).
    let mut di = vec![0.0f32; (SR * 3.0) as usize];
    for k in 0..8 {
        let start = (SR * 0.35) as usize * k;
        for i in 0..(SR * 0.25) as usize {
            let t = i as f32 / SR;
            let env = (-t * 9.0).exp();
            di[start + i] +=
                0.6 * env * (2.0 * std::f32::consts::PI * 82.41 * t).sin().signum() * 0.5;
        }
    }

    let mid_rms = |out: &[f32]| {
        let mut hp = Biquad::highpass(SR, 300.0, 0.707);
        let mut lp = Biquad::lowpass(SR, 5000.0, 0.707);
        let mid: Vec<f32> = out.iter().map(|&x| lp.process(hp.process(x))).collect();
        (mid.iter().map(|&x| x * x).sum::<f32>() / mid.len() as f32).sqrt()
    };
    let rms = |out: &[f32]| (out.iter().map(|&x| x * x).sum::<f32>() / out.len() as f32).sqrt();

    println!(
        "{:<10} {:>9} {:>9} {:>9}",
        "amp", "rms dB", "mid dB", "peak"
    );
    for am in AmpModel::ALL {
        let mut amp = AmpBank::new(SR);
        let mut cab = CabBank::new(SR);
        let knobs = amp::standard_knobs(am, 0.93, 0.5, 0.5, 0.65, 0.5, 0.65);
        let mono: Vec<f32> = di
            .iter()
            .map(|&x| {
                amp.process(
                    am,
                    x,
                    &knobs,
                    rusty_riff::dsp::CabModel::Marshall.speaker_load(),
                )
            })
            .collect();
        let with_cab: Vec<f32> = mono
            .iter()
            .map(|&a| {
                let (l, r) = cab.process(cab_for(am), a, 0.5, 0.15, 0.15);
                0.5 * (l + r)
            })
            .collect();
        let peak = mono.iter().fold(0.0f32, |m, &x| m.max(x.abs()));
        println!(
            "{:<10} {:>9.1} {:>9.1} {:>9.2}   (cab mid {:.1} dB)",
            format!("{am:?}"),
            20.0 * rms(&mono).log10(),
            20.0 * mid_rms(&mono).log10(),
            peak,
            20.0 * mid_rms(&with_cab).log10(),
        );
    }

    // Isolate each cab's own level: one fixed amp (Marshall) through every cab.
    println!("\n{:<16} {:>9}", "cab (Marshall amp)", "mid dB");
    let mut amp = AmpBank::new(SR);
    let mut cab = CabBank::new(SR);
    let knobs = amp::standard_knobs(AmpModel::Marshall, 0.93, 0.5, 0.5, 0.65, 0.5, 0.65);
    let mono: Vec<f32> = di
        .iter()
        .map(|&x| {
            amp.process(
                AmpModel::Marshall,
                x,
                &knobs,
                rusty_riff::dsp::CabModel::Marshall.speaker_load(),
            )
        })
        .collect();
    for cm in CabModel::ALL {
        let with_cab: Vec<f32> = mono
            .iter()
            .map(|&a| {
                let (l, r) = cab.process(cm, a, 0.5, 0.15, 0.15);
                0.5 * (l + r)
            })
            .collect();
        println!(
            "{:<16} {:>9.1}",
            format!("{cm:?}"),
            20.0 * mid_rms(&with_cab).log10()
        );
    }
}
