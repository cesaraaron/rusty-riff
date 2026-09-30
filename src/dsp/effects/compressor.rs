use super::{SmoothedGain, db_to_lin, lin_to_db, param_changed};

/// Front-of-chain feed-forward compressor — evens out picking dynamics and adds
/// sustain, the single biggest "studio" upgrade for clean and edge-of-breakup
/// tones. A peak-follower detector drives a hard-knee gain computer in the dB
/// domain; the gain itself is then smoothed with the same attack/release feel so
/// it never zippers or clicks. Auto makeup compensates for the threshold pull-down
/// so turning up Sustain doesn't drop the level.
pub struct Compressor {
    sr: f32,
    env: f32,  // peak-follower envelope (linear amplitude)
    gain: f32, // smoothed gain-reduction (linear)

    // Gain-computer constants, recomputed only when a knob moves. These used to
    // be derived per sample — a `lin_to_db`, a `db_to_lin` and two `powf` on
    // every single sample, for values that only change when a knob does.
    thresh_db: f32,
    ratio: f32,
    atk: f32,
    rel: f32,
    auto_makeup: f32,
    last_sustain: f32,
    last_attack: f32,

    /// Output makeup, smoothed. `level` spans 0–2×.
    level: SmoothedGain,
}

impl Compressor {
    pub fn new(sr: f32) -> Self {
        let mut c = Self {
            sr,
            env: 0.0,
            gain: 1.0,
            thresh_db: 0.0,
            ratio: 1.0,
            atk: 0.0,
            rel: 0.0,
            auto_makeup: 1.0,
            last_sustain: -1.0,
            last_attack: -1.0,
            // `level` 0.5 = unity, matching the default knob position.
            level: SmoothedGain::new(1.0, sr),
        };
        c.set_sustain(0.0);
        c.set_attack(0.0);
        c
    }

    /// Recompute the gain computer for a new SUSTAIN.
    ///
    /// The auto-makeup is **faded out toward the transparent end** of the knob.
    /// It used to be `db_to_lin(-thresh_db * (1-1/ratio) * 0.5)` with no sustain
    /// term, which at `sustain = 0` (threshold −6 dB, ratio 2:1) is
    /// `db_to_lin(1.5)` = **+1.5 dB**: engaging the compressor at its most
    /// transparent setting was itself a 1.5 dB level step, before any
    /// compression happened, and bypassing the pedal dropped that 1.5 dB.
    ///
    /// The fade is `1 + (full - 1) * sustain`, which is **unity at sustain = 0**
    /// and **exactly the old value at sustain = 1**, so the top of the knob —
    /// where the makeup is doing its actual job — is unchanged. Scaling the
    /// dB directly by `sustain` would also fix the zero but would pull ~4.7 dB
    /// out of the mid-range, which is more than the defect warrants: it rewrites
    /// the sound of every preset that uses the compressor mid-way.
    fn set_sustain(&mut self, sustain: f32) {
        self.thresh_db = -6.0 - sustain * 34.0; // 0 → −6 dB, 1 → −40 dB
        self.ratio = 2.0 + sustain * 8.0; // 2:1 … 10:1
        let full = db_to_lin(-self.thresh_db * (1.0 - 1.0 / self.ratio) * 0.5);
        self.auto_makeup = 1.0 + (full - 1.0) * sustain;
        self.last_sustain = sustain;
    }

    /// Detector attack/release coefficients for a new ATTACK.
    fn set_attack(&mut self, attack: f32) {
        let atk_ms = 0.5 + attack * 49.5;
        let rel_ms = 150.0; // musical fixed auto-release
        self.atk = (-1.0 / (atk_ms * 0.001 * self.sr)).exp();
        self.rel = (-1.0 / (rel_ms * 0.001 * self.sr)).exp();
        self.last_attack = attack;
    }

    /// `sustain` 0–1 = compression amount (lower threshold + higher ratio),
    /// `attack` 0–1 = 0.5–50 ms attack, `level` 0–1 = output makeup (≈0–2×).
    #[inline]
    pub fn process(&mut self, x: f32, sustain: f32, attack: f32, level: f32) -> f32 {
        if param_changed(sustain, self.last_sustain) {
            self.set_sustain(sustain);
        }
        if param_changed(attack, self.last_attack) {
            self.set_attack(attack);
        }

        // Peak-follower detector: fast attack chases transients, slower release.
        let a = x.abs();
        let det_coeff = if a > self.env { self.atk } else { self.rel };
        self.env = det_coeff * self.env + (1.0 - det_coeff) * a;

        // Gain computer (dB domain, hard knee).
        let over = lin_to_db(self.env) - self.thresh_db;
        let target = if over > 0.0 {
            db_to_lin(-over * (1.0 - 1.0 / self.ratio))
        } else {
            1.0
        };

        // Smooth the applied gain: pull down fast (attack), recover slow (release).
        let g_coeff = if target < self.gain {
            self.atk
        } else {
            self.rel
        };
        self.gain = g_coeff * self.gain + (1.0 - g_coeff) * target;

        self.level.set(level.clamp(0.0, 1.0) * 2.0);
        x * self.gain * self.auto_makeup * self.level.step()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::PI;

    /// The compressor must stay finite and actually reduce dynamic range: a loud
    /// passage should come out closer in level to a quiet passage than it went in.
    #[test]
    fn reduces_dynamic_range_and_is_finite() {
        let sr = 48_000.0;
        let f = 220.0;
        // Heavy compression settings.
        let (sustain, attack, level) = (0.9, 0.2, 0.5);

        let rms_at = |amp: f32| {
            let mut c = Compressor::new(sr);
            let mut sum = 0.0f64;
            let mut count = 0u32;
            let warmup = sr as usize / 4;
            for n in 0..(sr as usize) {
                let x = (2.0 * PI * f * n as f32 / sr).sin() * amp;
                let y = c.process(x, sustain, attack, level);
                assert!(y.is_finite(), "non-finite output");
                if n >= warmup {
                    sum += (y * y) as f64;
                    count += 1;
                }
            }
            (sum / count as f64).sqrt()
        };

        let loud_in = 0.8f64;
        let quiet_in = 0.1f64;
        let loud_out = rms_at(0.8);
        let quiet_out = rms_at(0.1);

        let in_ratio = loud_in / quiet_in; // 8×
        let out_ratio = loud_out / quiet_out;
        assert!(
            out_ratio < in_ratio,
            "compressor did not reduce dynamic range: in {in_ratio:.2}× out {out_ratio:.2}×"
        );
    }

    /// At the transparent end of the SUSTAIN knob the compressor must not change
    /// the level. It used to apply +1.5 dB of auto-makeup at `sustain = 0`
    /// (`db_to_lin(1.5)`), so engaging the pedal was itself an audible step
    /// before any compression occurred — and bypassing it dropped that 1.5 dB.
    #[test]
    fn zero_sustain_is_level_transparent() {
        let sr = 48_000.0;
        let f = 220.0;
        // Well below the −6 dB threshold, so there is nothing to compress: what
        // comes out must be exactly what went in, at unity gain.
        let mut c = Compressor::new(sr);
        let mut worst = 0.0f32;
        let mut sum_in = 0.0f64;
        let mut sum_out = 0.0f64;
        let mut count = 0u32;
        for n in 0..(sr as usize) {
            let x = (2.0 * PI * f * n as f32 / sr).sin() * 0.05; // ≈ −26 dBFS
            let y = c.process(x, 0.0, 0.5, 0.5); // sustain 0, level 0.5 = unity
            assert!(y.is_finite());
            if n > sr as usize / 2 {
                sum_in += (x * x) as f64;
                sum_out += (y * y) as f64;
                count += 1;
                // 0.05 amplitude: 1% of that is 0.0005, far under a 1.5 dB (19%)
                // error, so this cannot pass by being loose.
                worst = worst.max((y - x).abs());
            }
        }
        let ratio = (sum_out / count as f64).sqrt() / (sum_in / count as f64).sqrt();
        assert!(
            (ratio - 1.0).abs() < 0.02,
            "zero-sustain compressor is not unity: in/out rms ratio {ratio:.4}"
        );
        assert!(
            worst < 0.05 * 0.01,
            "zero-sustain compressor alters the waveform: worst deviation {worst:.6}"
        );
    }

    /// The auto-makeup must still rise with SUSTAIN, which is its whole purpose
    /// (turning sustain up should not drop the level).
    #[test]
    fn auto_makeup_still_compensates_at_high_sustain() {
        let sr = 48_000.0;
        let f = 220.0;
        let rms_at = |sustain: f32, amp: f32| {
            let mut c = Compressor::new(sr);
            let mut sum = 0.0f64;
            let mut count = 0u32;
            for n in 0..(sr as usize) {
                let x = (2.0 * PI * f * n as f32 / sr).sin() * amp;
                let y = c.process(x, sustain, 0.2, 0.5);
                if n > sr as usize / 2 {
                    sum += (y * y) as f64;
                    count += 1;
                }
            }
            (sum / count as f64).sqrt()
        };
        // Loud passages must not collapse as sustain comes up.
        let quiet = rms_at(1.0, 0.3);
        assert!(
            quiet > 0.05,
            "heavily compressed quiet passage collapsed to {quiet}"
        );
    }

    /// Turning the LEVEL knob must not step the output. The knob spans 0–2×, so an
    /// unsmoothed move is up to a 6 dB discontinuity; it is also a common pedal
    /// tweak, unlike the sustain knob which is usually set once.
    #[test]
    fn level_knob_moves_smoothly() {
        let sr = 48_000.0;
        let mut c = Compressor::new(sr);
        // A DC probe: the output is then exactly `gain * level`, so the
        // sample-to-sample step is entirely the level smoother's doing.
        let mut prev = c.process(0.1, 0.3, 0.3, 0.5);
        let mut worst = 0.0f32;
        for n in 0..2400 {
            // A hard 0.5 -> 1.0 jump at sample 1200: 0.5*2=1.0 up to 1.0*2=2.0.
            let level = if n == 1200 { 1.0 } else { 0.5 };
            let y = c.process(0.1, 0.3, 0.3, level);
            worst = worst.max((y - prev).abs());
            prev = y;
        }
        // An unsmoothed 0.1 -> 0.2 step would be 0.1. Smoothing spreads it.
        assert!(
            worst < 0.01,
            "level knob move steps the output by {worst:.5}"
        );
    }
}
