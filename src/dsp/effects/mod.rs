//! Pedals and rack effects — the stompbox/processor chain that sits around the
//! amp and cabinet core. Each effect is a self-contained `struct` with a
//! `process` method; the [`DspChain`](super::DspChain) wires them together.
//!
//! Logic shared between several effects lives here so the individual files stay
//! focused on their *voicing* (which frequencies, how much gain) rather than
//! re-implementing the same plumbing:
//!   • [`OnePoleLp`] — the variable low-pass used by passive tone controls.
//!   • [`ThreeBandEq`] — low-shelf / mid-peak / high-shelf trio shared by the
//!     pre-amp and post-cab equalizers.
//!   • [`db_to_lin`] / [`lin_to_db`] — decibel conversions for dynamics stages.
//!   • [`param_changed`] — the "did this knob move enough to rebuild?" test.

use crate::dsp::biquad::Biquad;
use std::f32::consts::PI;

pub mod chorus;
pub mod clean_boost;
pub mod compressor;
pub mod delay;
pub mod distortion;
pub mod flanger;
pub mod fuzz;
pub mod graphic_eq;
pub mod metal_core;
pub mod noise_gate;
pub mod parametric_eq;
pub mod phaser;
pub mod pitch;
pub mod preamp_eq;
pub mod reverb;
pub mod spring;
pub mod tremolo;
pub mod tube_screamer;
pub mod uni_vibe;
pub mod wah;

pub use chorus::Chorus;
pub use clean_boost::CleanBoost;
pub use compressor::Compressor;
pub use delay::Delay;
pub use distortion::Distortion;
pub use flanger::Flanger;
pub use fuzz::Fuzz;
pub use graphic_eq::GraphicEq;
pub use metal_core::MetalCore;
pub use noise_gate::NoiseGate;
pub use parametric_eq::ParametricEq;
pub use phaser::Phaser;
pub use pitch::Pitch;
pub use preamp_eq::PreampEq;
pub use reverb::Reverb;
pub use spring::SpringReverb;
pub use tremolo::Tremolo;
pub use tube_screamer::TubeScreamer;
pub use uni_vibe::UniVibe;
pub use wah::Wah;

/// A knob is "dirty" — worth rebuilding filter coefficients for — only once it has
/// moved by more than this. Filter rebuilds aren't free, so we skip them while a
/// control sits still (the common case on the audio thread).
const PARAM_EPSILON: f32 = 0.001;

/// Has a control moved far enough since we last acted on it to justify a rebuild?
#[inline]
pub fn param_changed(new: f32, last: f32) -> bool {
    (new - last).abs() > PARAM_EPSILON
}

/// Constant-power wet/dry gains for a **decorrelated** wet path (reverb, delay):
/// returns `(dry, wet)` with `dry² + wet² = 1`, so a `0.5` mix is neither the −3 dB
/// dip of a linear crossfade nor a boost. The `sqrt` law rather than `cos/sin` —
/// same constant-power property, one hardware sqrt each instead of two transcendentals.
///
/// Not for the flanger/chorus/phaser, whose wet is a (modulated) copy of the dry:
/// there the two paths are correlated and the linear fader is the right curve.
#[inline]
pub(super) fn equal_power(mix: f32) -> (f32, f32) {
    let m = mix.clamp(0.0, 1.0);
    ((1.0 - m).sqrt(), m.sqrt())
}

/// Fractional read `delay` samples behind `write`, with 4-point Catmull-Rom
/// interpolation.
///
/// Smoother than linear for a **time-varying** delay: linear interpolation is a
/// piecewise-linear approximation whose slope steps at every integer boundary, and a
/// swept delay turns those steps into a zipper (worst where the wet is loud — the
/// flanger reaches ~12× the dry at high feedback). The two extra taps cost a handful
/// of multiplies. `delay` is clamped so all four taps stay in the ring.
#[inline]
pub(super) fn read_cubic(buf: &[f32], write: usize, delay: f32) -> f32 {
    let len = buf.len();
    if len < 5 {
        return buf.get(write % len.max(1)).copied().unwrap_or(0.0);
    }
    // Taps at delays i-1, i, i+1, i+2 must all be in range.
    let d = delay.clamp(1.0, (len - 3) as f32);
    let i = d.floor() as usize;
    let t = d - i as f32;
    // `idx(k)` is `k` samples behind the write head; larger k = older.
    let idx = |k: usize| buf[(write + len - k) % len];
    let (p0, p1, p2, p3) = (idx(i - 1), idx(i), idx(i + 1), idx(i + 2));
    // Catmull-Rom between p1 (t = 0) and p2 (t = 1).
    let a = -0.5 * p0 + 1.5 * p1 - 1.5 * p2 + 0.5 * p3;
    let b = p0 - 2.5 * p1 + 2.0 * p2 - 0.5 * p3;
    let c = -0.5 * p0 + 0.5 * p2;
    ((a * t + b) * t + c) * t + p1
}

/// Default smoothing time for an output-scaling coefficient.
///
/// Long enough that no single-sample step is audible, short enough that a knob
/// move still feels immediate (a fader that lags visibly reads as broken).
const DEFAULT_SMOOTH_MS: f32 = 8.0;

/// A one-pole smoother for a gain or level coefficient.
///
/// Every output-scaling control used to be applied as an instantaneous per-sample
/// multiply. That is a step in the waveform — a zipper, or a click when the step
/// is large. Two cases make it audible in practice:
///
/// - **A knob move.** The TS-808's drive reaches a 41 dB step (its shelf gain
///   goes to 117×), the clean boost's gain a 24 dB step. One keypress, one
///   discontinuity.
/// - **A MIDI CC sweep.** `MidiTarget` binds a CC to `delay_mix`, `reverb_mix`,
///   `chorus_mix`, `boost_gain`, `ts_drive`, `ds_drive` and others
///   (`src/midi.rs`). An expression pedal moving those writes a *new* value
///   every few samples, so the unsmoothed gain is effectively a staircase at
///   audio rate. This is the case that made the problem real rather than
///   theoretical.
///
/// The smoother is a one-pole (`y += (target - y) * k`), which is linear in
/// amplitude — a constant time constant, the usual choice for a fader. It costs
/// one multiply-add per sample per control.
pub struct SmoothedGain {
    current: f32,
    target: f32,
    coeff: f32,
    /// Has a value been set since construction? Until then the `current` seed is
    /// only a placeholder, because the effect has not actually been given a
    /// control value yet.
    primed: bool,
}

impl SmoothedGain {
    /// Create a smoother already settled on `initial`.
    ///
    /// `initial` is a *placeholder* for the value a control has not been given
    /// yet. The first [`set`](Self::set) adopts its argument verbatim, so an
    /// effect that is constructed and immediately driven at `mix = 0` passes its
    /// input through bit-exactly rather than ramping in from the default.
    pub fn new(initial: f32, sr: f32) -> Self {
        Self::with_ms(initial, DEFAULT_SMOOTH_MS, sr)
    }

    /// As [`new`](Self::new), with an explicit smoothing time in milliseconds.
    pub fn with_ms(initial: f32, ms: f32, sr: f32) -> Self {
        let coeff = if ms <= 0.0 {
            1.0
        } else {
            1.0 - (-1.0 / (ms * 0.001 * sr)).exp()
        };
        Self {
            current: initial,
            target: initial,
            coeff,
            primed: false,
        }
    }

    /// Set the value to move toward. Cheap enough to call every sample; the
    /// smoothing is what makes that safe.
    ///
    /// The **first** call after construction snaps: there is no prior value to
    /// ramp from, and a phantom ramp here would make a freshly-constructed
    /// effect audibly wrong until it settled (and would break bit-exact dry
    /// transparency). Later calls ramp as normal.
    #[inline]
    pub fn set(&mut self, value: f32) {
        if !self.primed {
            self.primed = true;
            self.current = value;
            self.target = value;
            return;
        }
        self.target = value;
    }

    /// Advance one sample and return the smoothed value. Named `step`
    /// rather than `next` so it does not read as an `Iterator` method.
    #[inline]
    pub fn step(&mut self) -> f32 {
        self.current += (self.target - self.current) * self.coeff;
        self.current
    }

    /// The not-yet-smoothed target, for the (rare) caller that needs the
    /// instantaneous value.
    #[inline]
    pub fn target(&self) -> f32 {
        self.target
    }

    /// Jump straight to `value`, skipping the ramp. For state that is not an
    /// audible gain (a preset load, a sample-rate change) or for a bypass
    /// toggle, where the caller handles the transition itself.
    #[inline]
    pub fn reset(&mut self, value: f32) {
        self.current = value;
        self.target = value;
    }

    /// The current smoothed value, without advancing.
    #[inline]
    pub fn value(&self) -> f32 {
        self.current
    }
}

/// Convert decibels to a linear amplitude ratio.
#[inline]
pub fn db_to_lin(db: f32) -> f32 {
    10.0_f32.powf(db / 20.0)
}

/// Convert a linear amplitude (floored to avoid `-inf`) to decibels.
#[inline]
pub fn lin_to_db(x: f32) -> f32 {
    20.0 * x.max(1e-6).log10()
}

/// One-pole low-pass filter — the workhorse behind passive "tone" controls.
///
/// A guitar pedal's tone knob is, electrically, a variable RC low-pass: turn it
/// down and the corner frequency drops, rolling off the highs. Both the TS-808 and
/// the Big Muff model their tone stage exactly this way, so they share this filter
/// and only differ in the frequency range they sweep it across.
pub struct OnePoleLp {
    z: f32,
    coeff: f32,
}

impl OnePoleLp {
    pub fn new() -> Self {
        Self { z: 0.0, coeff: 0.0 }
    }

    /// Set the −3 dB corner to `freq` Hz at sample rate `sr`.
    #[inline]
    pub fn set_cutoff(&mut self, freq: f32, sr: f32) {
        self.coeff = 1.0 - (-2.0 * PI * freq / sr).exp();
    }

    #[inline]
    pub fn process(&mut self, x: f32) -> f32 {
        self.z += self.coeff * (x - self.z);
        self.z
    }
}

impl Default for OnePoleLp {
    fn default() -> Self {
        Self::new()
    }
}

/// Mono three-band equalizer: low shelf → mid peak → high shelf in series.
///
/// Both the pre-amp EQ and the post-cab parametric EQ are this exact topology;
/// they differ only in their centre frequencies, mid Q and gain range. Factoring
/// the filter trio out here means the [`ParametricEq`] (stereo = two of these) and
/// [`PreampEq`] (mono = one) wrappers only own their voicing and the dirty-check,
/// not three biquads' worth of duplicated rebuild code.
pub struct ThreeBandEq {
    sr: f32,
    low_freq: f32,
    mid_freq: f32,
    mid_q: f32,
    high_freq: f32,
    low: Biquad,
    mid: Biquad,
    high: Biquad,
}

impl ThreeBandEq {
    /// Build a flat (0 dB) EQ with the given band layout.
    pub fn new(sr: f32, low_freq: f32, mid_freq: f32, mid_q: f32, high_freq: f32) -> Self {
        Self {
            sr,
            low_freq,
            mid_freq,
            mid_q,
            high_freq,
            low: Biquad::low_shelf(sr, low_freq, 0.0),
            mid: Biquad::peak_eq(sr, mid_freq, mid_q, 0.0),
            high: Biquad::high_shelf(sr, high_freq, 0.0),
        }
    }

    /// Recompute the three biquads for the given per-band gains (in dB).
    ///
    /// Retunes in place so a live knob move does not discard the filter state
    /// and click (three state resets, six across the stereo pair).
    pub fn set_gains_db(&mut self, low_db: f32, mid_db: f32, high_db: f32) {
        self.low.set_low_shelf(self.sr, self.low_freq, low_db);
        self.mid
            .set_peak_eq(self.sr, self.mid_freq, self.mid_q, mid_db);
        self.high.set_high_shelf(self.sr, self.high_freq, high_db);
    }

    #[inline]
    pub fn process(&mut self, x: f32) -> f32 {
        self.high.process(self.mid.process(self.low.process(x)))
    }
}

#[cfg(test)]
mod tests {
    use super::{SmoothedGain, equal_power, read_cubic};

    const SR: f32 = 48_000.0;

    /// The reverb/delay crossfade must hold constant power across the knob and be a
    /// pure passthrough at mix 0.
    #[test]
    fn equal_power_holds_constant_power() {
        let (d0, w0) = equal_power(0.0);
        assert!(
            (d0 - 1.0).abs() < 1e-6 && w0.abs() < 1e-6,
            "mix 0 passthrough"
        );
        for k in 0..=100 {
            let m = k as f32 / 100.0;
            let (d, w) = equal_power(m);
            assert!(
                (d * d + w * w - 1.0).abs() < 1e-5,
                "mix {m}: power {}",
                d * d + w * w
            );
        }
        assert!(equal_power(1.0).0.abs() < 1e-6, "mix 1 is all wet");
    }

    /// `read_cubic` is exact at integer delays, holds a constant at any fractional
    /// delay, and reproduces a linear ramp (Catmull-Rom is exact for degree ≤ 1) —
    /// the correctness floor under the smoother modulation reads.
    #[test]
    fn read_cubic_is_exact_at_integers_and_linear() {
        let buf: Vec<f32> = (0..64).map(|i| i as f32).collect();
        let write = 0usize;
        let at = |k: usize| buf[(write + buf.len() - k) % buf.len()];
        for d in 1..20usize {
            assert!((read_cubic(&buf, write, d as f32) - at(d)).abs() < 1e-4);
        }
        let flat = vec![0.25f32; 64];
        for k in 0..40 {
            let d = 1.0 + k as f32 * 0.1;
            assert!((read_cubic(&flat, write, d) - 0.25).abs() < 1e-5);
        }
        for i in 2..20usize {
            for f in [0.0f32, 0.25, 0.5, 0.75, 1.0] {
                let want = at(i) * (1.0 - f) + at(i + 1) * f;
                let got = read_cubic(&buf, write, i as f32 + f);
                assert!((got - want).abs() < 1e-3, "i={i} f={f}: {got} vs {want}");
            }
        }
    }

    /// The smoother must not step: a hard target change should be spread over
    /// the smoothing time, not applied in one sample.
    #[test]
    fn a_target_change_is_ramped_not_stepped() {
        let mut g = SmoothedGain::new(1.0, SR);
        // Prime with the starting value, then let it settle.
        for _ in 0..1000 {
            g.set(1.0);
            g.step();
        }
        assert!((g.value() - 1.0).abs() < 1e-3, "did not settle at 1.0");
        g.set(2.0);
        // The very next sample must still be near 1.0.
        let first = g.step();
        assert!(
            first < 1.1,
            "smoother stepped immediately to {first} instead of ramping"
        );
        // And it should get there: the 8 ms time constant is ~385 samples, so
        // ~4.4 time constants (4000 samples) leaves under 1e-3 of the step.
        for _ in 0..4000 {
            g.step();
        }
        assert!(
            (g.value() - 2.0).abs() < 1e-3,
            "never reached the target: {}",
            g.value()
        );
    }

    /// Below-knob-jitter movement must not accumulate into a visible ramp: a
    /// control sitting still (the common case) should cost nothing and move
    /// nothing.
    #[test]
    fn a_settled_smoother_is_transparent() {
        let mut g = SmoothedGain::new(0.7, SR);
        for _ in 0..5000 {
            g.set(0.7);
            let y = g.step();
            assert!((y - 0.7).abs() < 1e-6, "a static control drifted to {y}");
        }
    }

    /// The 8 ms default should settle within a few tens of milliseconds — a
    /// fader that lags visibly reads as broken.
    #[test]
    fn the_default_time_constant_is_fast_enough_to_feel_immediate() {
        let mut g = SmoothedGain::new(0.0, SR);
        g.set(0.0); // prime
        g.step();
        g.set(1.0);
        let mut n = 0usize;
        while g.value() < 0.99 && n < SR as usize {
            g.step();
            n += 1;
        }
        let ms = n as f32 / (SR / 1000.0);
        assert!(
            ms < 40.0,
            "reaching 99% took {ms:.1} ms; the default smoothing is too slow"
        );
        assert!(ms > 5.0, "settled in {ms:.1} ms — too fast to smooth");
    }

    /// The first value after construction must be adopted **verbatim**.
    ///
    /// The constructed value is only a placeholder — the effect has not been
    /// given a control value yet. Without this snap a freshly-constructed effect
    /// driven at `mix = 0` would ramp in from the default mix and fail
    /// bit-exact dry transparency for its first few milliseconds. This is what
    /// keeps `fully_dry_is_passthrough` honest across every effect.
    #[test]
    fn the_first_value_is_adopted_verbatim() {
        let mut g = SmoothedGain::new(0.5, SR);
        // Constructed at 0.5; driven at 0.0 straight away.
        g.set(0.0);
        assert_eq!(
            g.step(),
            0.0,
            "the first value after construction must be exact, not ramped"
        );

        // A second change *does* ramp, so the snap is not just "always exact".
        g.set(1.0);
        let y = g.step();
        assert!(y > 0.0 && y < 0.5, "second change should ramp, got {y}");
    }

    /// A MIDI CC sweep rewrites the target every few samples, which is the case
    /// that made the unsmoothed gains audible. The output must stay a smooth
    /// ramp rather than a staircase.
    #[test]
    fn a_ramping_target_produces_a_staircase_free_output() {
        // A CC sweep over ~1 s from 0 to 1, rewritten at 100 Hz.
        let mut g = SmoothedGain::new(0.0, SR);
        let mut worst = 0.0f32;
        let mut prev = 0.0f32;
        let sweep_samples = SR as usize;
        let period = SR as usize / 100; // 100 Hz control update
        for n in 0..sweep_samples {
            if n == 0 {
                g.set(0.0); // prime at the start of the sweep
                g.step();
            }
            if n % period == 0 {
                g.set(n as f32 / sweep_samples as f32);
            }
            let y = g.step();
            worst = worst.max((y - prev).abs());
            prev = y;
        }
        // A 100 Hz staircase on a 0..1 gain would step by 1/100 = 0.01 per
        // update. Smoothing keeps the per-sample change far below that.
        assert!(
            worst < 1e-3,
            "CC sweep produced a {worst:.5} step; expected a smooth ramp"
        );
    }
}
