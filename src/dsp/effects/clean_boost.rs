//! Clean boost — a linear front-end booster modelled on the Colorsound Power
//! Boost (the "power boost" Gilmour ran into the Hiwatt).
//!
//! Unlike the Tube Screamer and DS-1, this pedal does **not** clip: it is a
//! broad-band gain stage with a two-band shelving tone stack (Bass / Treble).
//! Its job is to hit the amp harder, so it sits in the pre-amp chain right
//! before the amp and lets the amp's own gain stage provide the colour.
//!
//! Model, not a circuit-exact emulation: a single linear gain plus a low and a
//! high shelf. The real pedal's interactive passive/active tone network is only
//! approximated.
use super::{db_to_lin, param_changed};
use crate::dsp::biquad::Biquad;

const TREBLE_FREQ: f32 = 3000.0;
const BASS_FREQ: f32 = 120.0;
/// Gain range at the top of the knob (the panel knob maps 0–1 linearly to 0…this).
const MAX_GAIN_DB: f32 = 24.0;
/// Tone knobs map 0–1 to ± this many dB around the 0.5 centre (flat).
const TONE_RANGE_DB: f32 = 24.0;

pub struct CleanBoost {
    sr: f32,
    bass: Biquad,
    treble: Biquad,
    last_treble: f32,
    last_bass: f32,
}

impl CleanBoost {
    pub fn new(sr: f32) -> Self {
        let mut b = Self {
            sr,
            bass: Biquad::low_shelf(sr, BASS_FREQ, 0.0),
            treble: Biquad::high_shelf(sr, TREBLE_FREQ, 0.0),
            last_treble: -1.0,
            last_bass: -1.0,
        };
        b.set_tone(0.5, 0.5);
        b
    }

    fn set_tone(&mut self, treble: f32, bass: f32) {
        let db = |v: f32| (v - 0.5) * TONE_RANGE_DB; // 0 → −12, 0.5 → 0, 1 → +12
        self.treble.set_high_shelf(self.sr, TREBLE_FREQ, db(treble));
        self.bass.set_low_shelf(self.sr, BASS_FREQ, db(bass));
        self.last_treble = treble;
        self.last_bass = bass;
    }

    /// `gain`, `treble`, `bass` are all 0–1. The gain knob is a linear
    /// `0…MAX_GAIN_DB` boost; the tone knobs are shelves around a flat 0.5.
    #[inline]
    pub fn process(&mut self, x: f32, gain: f32, treble: f32, bass: f32) -> f32 {
        if param_changed(treble, self.last_treble) || param_changed(bass, self.last_bass) {
            self.set_tone(treble, bass);
        }
        let g = db_to_lin(gain.clamp(0.0, 1.0) * MAX_GAIN_DB);
        self.treble.process(self.bass.process(x)) * g
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::PI;

    /// Each knob must move what it should and the output must stay finite.
    #[test]
    fn knobs_track_level_and_bands() {
        let sr = 48_000.0;
        let rms = |knobs: (f32, f32, f32), freq: f32| {
            let mut b = CleanBoost::new(sr);
            let mut sum = 0.0f64;
            for n in 0..(sr as usize) {
                let x = (2.0 * PI * freq * n as f32 / sr).sin();
                let y = b.process(x, knobs.0, knobs.1, knobs.2);
                assert!(y.is_finite(), "clean boost produced a non-finite sample");
                if n >= sr as usize / 2 {
                    sum += (y * y) as f64;
                }
            }
            sum.sqrt()
        };

        assert!(
            rms((1.0, 0.5, 0.5), 440.0) > rms((0.0, 0.5, 0.5), 440.0),
            "gain knob dead"
        );
        assert!(
            rms((0.5, 0.5, 1.0), 60.0) > rms((0.5, 0.5, 0.0), 60.0),
            "bass knob dead"
        );
        assert!(
            rms((0.5, 1.0, 0.5), 8000.0) > rms((0.5, 0.0, 0.5), 8000.0),
            "treble knob dead"
        );
    }

    /// At minimum gain and flat tone the pedal is magnitude-unity — "clean" is
    /// the whole point of the model. (A 0 dB shelf is an allpass, so compare RMS,
    /// not sample-for-sample.)
    #[test]
    fn minimum_gain_is_unity_and_clean() {
        let sr = 48_000.0;
        let mut b = CleanBoost::new(sr);
        let (mut in_sq, mut out_sq) = (0.0f64, 0.0f64);
        for n in 0..(sr as usize) {
            let x = (2.0 * PI * 220.0 * n as f32 / sr).sin();
            let y = b.process(x, 0.0, 0.5, 0.5);
            if n >= sr as usize / 2 {
                in_sq += (x * x) as f64;
                out_sq += (y * y) as f64;
            }
        }
        let ratio = (out_sq / in_sq).sqrt();
        assert!(
            (ratio - 1.0).abs() < 0.02,
            "not unity at minimum gain: {ratio}"
        );
    }
}
