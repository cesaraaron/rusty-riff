use std::f32::consts::{PI, TAU};

use super::SmoothedGain;
/// Uni-Vibe: a four-stage, LFO-swept all-pass cascade with a photocell-style throb,
/// summed with the dry signal — the "vibe" behind the DSOTM lead tones.
///
/// Where the phaser sweeps a clean four-stage all-pass for a hollow whoosh, the
/// Uni-Vibe is its warmer, wobblier ancestor: the same all-pass phase network, but
/// driven by a *lamp/photocell* whose response is non-linear, so the sweep lingers
/// low and breathes rather than gliding evenly, and the stage break frequencies are
/// staggered (unequal cap values) so the notches do not sit in a tidy harmonic
/// stack. In **chorus** mode the dry signal is summed back in (the classic
/// swirling "vibe"); in **vibrato** mode the wet phase signal runs alone, and
/// because an all-pass's group delay moves with the sweep, that alone produces the
/// pitch shimmer.
///
/// Placed in the **mono pre-amp chain, last before the amp**, mirroring how
/// Gilmour ran it — guitar → fuzz/boost → Uni-Vibe → Hiwatt — so it sees the raw
/// pedal signal and interacts with the fuzz the way the real pedal does.
///
/// Knob ranges (all normalised 0–1):
///   RATE  → LFO speed, 0.1–8 Hz (exponential).
///   DEPTH → sweep width; how far the all-pass break frequencies glide.
///   MIX   → chorus dry/wet blend (0 = dry, 1 = fully wet).
///   MODE  → 0 = chorus (dry + phase), 1 = vibrato (phase only).
pub struct UniVibe {
    /// One delay element of state per all-pass stage.
    z: [f32; N_STAGES],
    phase: f32,
    /// Wet/dry, smoothed so a mix move is not a step.
    mix: SmoothedGain,
    sr: f32,
}

/// Four cascaded all-pass stages — the Uni-Vibe's phase network.
const N_STAGES: usize = 4;
/// Lowest all-pass break frequency the sweep reaches (bottom of the throb).
const F_MIN: f32 = 180.0;
/// Highest break frequency at full depth (top of the sweep).
const F_MAX: f32 = 2200.0;
/// Per-stage stagger: a real Uni-Vibe's stages use unequal caps, so their break
/// frequencies do not move in lockstep. These multipliers widen the notch spacing
/// away from a tidy harmonic stack for the pedal's characteristic wobble.
const STAGE_SPREAD: [f32; N_STAGES] = [1.0, 0.78, 1.30, 0.62];
/// Photocell LFO shaping: `lfo^SHAPE` makes the sweep dwell at the bottom of its
/// travel and snap up, the lamp lag that gives the vibe its breathing pulse.
const LFO_SHAPE: f32 = 1.35;

impl UniVibe {
    pub fn new(sr: f32) -> Self {
        Self {
            z: [0.0; N_STAGES],
            phase: 0.0,
            // `DEFAULT_UV_MIX`.
            mix: SmoothedGain::new(0.50, sr),
            sr,
        }
    }

    /// `rate`, `depth`, `mix`, `mode` are all 0–1.
    #[inline]
    pub fn process(&mut self, x: f32, rate: f32, depth: f32, mix: f32, mode: f32) -> f32 {
        // Exponential rate map: 0.1–8 Hz, spreading the slow, musical speeds across
        // most of the knob (mirrors the phaser/tremolo).
        let rate_hz = 0.1 * 80.0_f32.powf(rate.clamp(0.0, 1.0));
        self.phase = (self.phase + rate_hz / self.sr).fract();

        // Sine LFO, then the photocell's dwell-at-the-bottom shaping.
        let lfo = (0.5 - 0.5 * (self.phase * TAU).cos()).powf(LFO_SHAPE);
        let depth = depth.clamp(0.0, 1.0);

        // Four swept first-order all-passes, staggered per stage. `fc` glides
        // exponentially across the sweep band; each stage's all-pass coefficient
        // a = (tan(π·fc/sr) − 1) / (tan(π·fc/sr) + 1) puts its −90° point at `fc`.
        let mut s = x;
        for (zi, &spread) in self.z.iter_mut().zip(STAGE_SPREAD.iter()) {
            let fc =
                (F_MIN * (F_MAX / F_MIN).powf(depth * lfo) * spread).clamp(20.0, self.sr * 0.45);
            let t = (PI * fc / self.sr).tan();
            let a = (t - 1.0) / (t + 1.0);
            let y = a * s + *zi;
            *zi = s - a * y;
            s = y;
        }
        let wet = s;

        // Chorus blends dry + phase; vibrato runs fully wet (the moving group delay
        // alone gives the pitch shimmer). `mode` slides between the two.
        self.mix.set(mix.clamp(0.0, 1.0));
        let mix = self.mix.step();
        let mode = mode.clamp(0.0, 1.0);
        let wet_amt = mix + (1.0 - mix) * mode;
        x * (1.0 - wet_amt) + wet * wet_amt
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::PI;

    const SR: f32 = 48_000.0;

    /// A windowed peak-amplitude envelope of the output, sampled past a warm-up.
    fn window_peaks<F: FnMut(usize) -> f32>(
        n: usize,
        warmup: usize,
        win: usize,
        mut f: F,
    ) -> Vec<f32> {
        let mut peaks = Vec::new();
        let mut peak = 0.0f32;
        for i in 0..n {
            let y = f(i);
            assert!(y.is_finite(), "uni-vibe non-finite at {i}");
            if i >= warmup {
                peak = peak.max(y.abs());
                if (i - warmup) % win == win - 1 {
                    peaks.push(peak);
                    peak = 0.0;
                }
            }
        }
        peaks
    }

    /// With `mix = 0` and `mode = 0` (chorus, fully dry) the pedal must pass the
    /// input through untouched.
    #[test]
    fn fully_dry_is_passthrough() {
        let mut uv = UniVibe::new(SR);
        for n in 0..2000 {
            let x = (n as f32 * 0.03).sin();
            let y = uv.process(x, 0.4, 0.8, 0.0, 0.0);
            assert!((y - x).abs() < 1e-6, "dry path colored: {y} vs {x}");
        }
    }

    /// Extreme settings (fast, deep) must stay finite and bounded — the all-pass
    /// cascade has unit magnitude, so it cannot run away.
    #[test]
    fn finite_and_bounded_under_extreme_settings() {
        let mut uv = UniVibe::new(SR);
        let mut max_abs = 0.0f32;
        for n in 0..(SR as usize) {
            let x = (2.0 * PI * 300.0 * n as f32 / SR).sin() * 0.9;
            let y = uv.process(x, 1.0, 1.0, 1.0, 1.0);
            assert!(y.is_finite(), "non-finite at {n}");
            max_abs = max_abs.max(y.abs());
        }
        assert!(max_abs < 2.0, "all-pass cascade ran away: {max_abs}");
    }

    /// With a wet mix the sweeping notches must modulate a steady carrier — the
    /// envelope of a sine in the sweep band should vary over time.
    #[test]
    fn wet_output_throbs_over_time() {
        let mut uv = UniVibe::new(SR);
        let peaks = window_peaks(SR as usize * 3, SR as usize, 256, |n| {
            let x = (2.0 * PI * 600.0 * n as f32 / SR).sin();
            uv.process(x, 0.6, 1.0, 0.5, 0.0)
        });
        let hi = peaks.iter().cloned().fold(0.0f32, f32::max);
        let lo = peaks.iter().cloned().fold(f32::INFINITY, f32::min);
        assert!(
            hi - lo > 0.1,
            "wet output not modulated (env {lo:.3}..{hi:.3})"
        );
    }

    /// Deeper DEPTH must glide the break frequencies across a wider band, so the
    /// notch travels further and modulates the carrier's envelope more.
    #[test]
    fn deeper_depth_sweeps_further() {
        let envelope_range = |depth: f32| {
            let mut uv = UniVibe::new(SR);
            let peaks = window_peaks(SR as usize * 2, SR as usize, 256, |n| {
                let x = (2.0 * PI * 600.0 * n as f32 / SR).sin();
                uv.process(x, 0.6, depth, 0.5, 0.0)
            });
            let hi = peaks.iter().cloned().fold(0.0f32, f32::max);
            let lo = peaks.iter().cloned().fold(f32::INFINITY, f32::min);
            hi - lo
        };
        assert!(
            envelope_range(1.0) > envelope_range(0.02) * 1.5,
            "depth knob does not widen the sweep"
        );
    }

    /// Vibrato mode runs fully wet, so even at `mix = 0` it must color the signal
    /// (unlike chorus mode, which is dry at `mix = 0`).
    #[test]
    fn vibrato_mode_is_fully_wet() {
        let mut uv = UniVibe::new(SR);
        let mut differs = false;
        for n in 0..4000 {
            let x = (2.0 * PI * 500.0 * n as f32 / SR).sin();
            let y = uv.process(x, 0.5, 1.0, 0.0, 1.0);
            if (y - x).abs() > 1e-4 {
                differs = true;
            }
        }
        assert!(differs, "vibrato mode did not run fully wet at mix = 0");
    }
}
