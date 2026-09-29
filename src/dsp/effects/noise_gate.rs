use super::db_to_lin;

/// Downward gate that mutes the signal once it drops below the threshold —
/// silences amp hiss and hum between notes without chopping sustain.
///
/// A peak envelope follower drives a smoothed open/closed gain, and the
/// RELEASE knob sets a **hold** time: how long the gate stays open after the
/// signal falls below the threshold before it starts to close. Without the
/// hold, a gate on a picked note closes in the gap before the reverb tail has
/// even started, which is the "chompy" complaint every gate has.
pub struct NoiseGate {
    envelope: f32,
    gain: f32,
    /// Samples of hold left before the gate may begin closing. Reset whenever
    /// the signal is above the threshold.
    hold_left: f32,
    /// Sample rate, to convert the knob's milliseconds into samples.
    sr: f32,
    attack_coeff: f32,
    release_coeff: f32,
    /// Gain-ramp coefficient for the gate *opening*.
    open_coeff: f32,
    /// Gain-ramp coefficient for the gate *closing*, once the hold has expired.
    close_coeff: f32,
}

/// Release knob travel → hold time. The 10 ms floor keeps a low setting from
/// chopping; 500 ms covers a long ring-out.
const HOLD_MIN_MS: f32 = 10.0;
const HOLD_RANGE_MS: f32 = 490.0;

/// Gain ramps, as time constants. Both are long enough that a gate transition
/// is not a hard cut on a non-zero waveform — the old opening coefficient was
/// 0.9, a **0.21 ms** time constant, which gated a pick transient as hard as a
/// switch and clicked on every note. A real gate opens in roughly 0.5–2 ms.
const OPEN_MS: f32 = 1.0;
const CLOSE_MS: f32 = 5.0;

/// One-pole coefficient for a one-pole smoother with time constant `ms`.
#[inline]
fn smoother(ms: f32, sr: f32) -> f32 {
    (-1.0 / (ms * 0.001 * sr)).exp()
}

impl NoiseGate {
    pub fn new(sr: f32) -> Self {
        Self {
            envelope: 0.0,
            gain: 1.0,
            hold_left: 0.0,
            sr,
            // Detector: ~1 ms attack so a pick transient is caught immediately,
            // and a *fast* ~10 ms release so the level estimate tracks the note
            // rather than ringing on. The gate's response time is then set by
            // the HOLD below, which the RELEASE knob drives — with a slow
            // detector release the hold is dead, because the envelope takes
            // hundreds of ms to fall and the hold timer is already expired
            // before the gate is even allowed to start closing.
            attack_coeff: smoother(1.0, sr),
            release_coeff: smoother(10.0, sr),
            open_coeff: smoother(OPEN_MS, sr),
            close_coeff: smoother(CLOSE_MS, sr),
        }
    }

    #[inline]
    pub fn process(&mut self, sample: f32, threshold: f32, release: f32) -> f32 {
        let abs = sample.abs();
        let coeff = if abs > self.envelope {
            self.attack_coeff
        } else {
            self.release_coeff
        };
        self.envelope = coeff * self.envelope + (1.0 - coeff) * abs;

        // threshold 0.0–1.0 maps to -80 dB–0 dB
        let threshold_lin = db_to_lin((threshold - 1.0) * 80.0);

        // Hold: while the signal is above the threshold the timer is reloaded
        // and the gate is open. Below it, the timer counts down; the gate only
        // starts closing once it expires.
        let open = self.envelope >= threshold_lin;
        if open {
            // `hold_left` counts samples, so convert the knob's milliseconds.
            self.hold_left = (HOLD_MIN_MS + release * HOLD_RANGE_MS) * 0.001 * self.sr;
        } else if self.hold_left > 0.0 {
            self.hold_left -= 1.0;
        }
        let target = if self.hold_left > 0.0 { 1.0 } else { 0.0 };

        // Smooth the gain so a transition is a ramp, not a step.
        let gain_coeff = if target > self.gain {
            self.open_coeff
        } else {
            self.close_coeff
        };
        self.gain = gain_coeff * self.gain + (1.0 - gain_coeff) * target;

        sample * self.gain
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::PI;

    const SR: f32 = 48_000.0;

    /// A loud tone above the threshold must pass; a quiet tone below it must be
    /// gated down to near silence. Output stays finite throughout.
    #[test]
    fn passes_loud_and_gates_quiet() {
        let rms_at = |amp: f32| {
            let mut ng = NoiseGate::new(SR);
            let mut sum = 0.0f64;
            let warmup = SR as usize / 4;
            let mut count = 0u32;
            for n in 0..(SR as usize) {
                let x = (2.0 * PI * 220.0 * n as f32 / SR).sin() * amp;
                let y = ng.process(x, 0.4, 0.3);
                assert!(y.is_finite());
                if n >= warmup {
                    sum += (y * y) as f64;
                    count += 1;
                }
            }
            (sum / count as f64).sqrt()
        };

        let loud = rms_at(0.5);
        let quiet = rms_at(0.0005);
        assert!(loud > 0.1, "gate closed on a loud signal: {loud}");
        assert!(
            quiet < loud * 0.1,
            "gate failed to attenuate quiet signal: {quiet}"
        );
    }

    /// Opening the gate must be a ramp, not a hard cut. A gate that opens
    /// instantly multiplies a non-zero pick transient by a step and clicks.
    ///
    /// **The probe is a constant, not a tone**, and that is load-bearing. A
    /// 220 Hz tone's own slew is 0.023 of its amplitude per sample, the same
    /// order as the step under test, so the measurement ends up dominated by
    /// the probe and the two cases look identical — measured 0.0230 for both a
    /// 0.21 ms and a 1 ms opening ramp. A constant has zero slew, so the only
    /// sample-to-sample step is the gate's own.
    #[test]
    fn opening_the_gate_does_not_click() {
        let mut ng = NoiseGate::new(SR);
        // Sit closed on silence so the gain has decayed to zero.
        for _ in 0..(SR as usize / 2) {
            ng.process(0.0, 0.4, 0.3);
        }
        let amp = 0.8f32;
        let mut prev = 0.0f32;
        let mut worst = 0.0f32;
        for _ in 0..2400 {
            let y = ng.process(amp, 0.4, 0.3);
            assert!(y.is_finite());
            worst = worst.max((y - prev).abs());
            prev = y;
        }
        // A hard gate jumps the full `amp` in one sample. Measured on this rig:
        // the old 0.21 ms opening ramp peaks at 0.0800 of the signal amplitude
        // (exactly `amp * (1 - 0.9)`, as a one-pole should), a 1 ms ramp at
        // ~0.0168. The bound sits between the two.
        assert!(
            worst < amp * 0.03,
            "gate opening clicks: worst sample step {worst:.4} on a {amp} signal"
        );
    }

    /// The RELEASE knob is a hold time: the gate must stay open for a while
    /// after the signal drops below the threshold, and longer when the knob is
    /// higher. This is the behaviour the old code computed into `release_ms`
    /// and then discarded with `let _ =`.
    #[test]
    fn release_knob_is_a_hold_time() {
        // The gate's output is `input * gain`, so the hold has to be observed
        // with a small *sub-threshold* probe rather than silence — with a zero
        // input the output is zero whatever the gain is doing. The probe is
        // below the threshold, so it does not itself re-open the gate.
        let threshold = 0.4f32;
        let probe = 0.0005f32;
        assert!(
            db_to_lin((threshold - 1.0) * 80.0) > probe * 4.0,
            "probe too loud"
        );

        let hold_ms = |release: f32| {
            let mut ng = NoiseGate::new(SR);
            // Present a loud note so the hold timer is fully loaded.
            for n in 0..(SR as usize / 4) {
                let x = (2.0 * PI * 220.0 * n as f32 / SR).sin() * 0.5;
                ng.process(x, threshold, release);
            }
            // Go quiet. The probe reveals the gain while staying under the
            // threshold, so the hold is free to run down.
            let mut last_open = 0usize;
            for n in 0..(SR as usize) {
                let y = ng.process(probe, threshold, release);
                if y > probe * 0.05 {
                    last_open = n;
                }
            }
            last_open as f32 / (SR / 1000.0)
        };

        let fast = hold_ms(0.0);
        let slow = hold_ms(1.0);
        assert!(
            slow > fast * 2.0,
            "RELEASE knob does not change the hold time: {fast:.1} ms vs {slow:.1} ms"
        );
        // The knob's documented range is 10–500 ms. The measured value is the
        // hold plus most of the close ramp, so allow headroom on both sides.
        assert!(
            fast > 5.0 && slow < 800.0,
            "hold time out of the intended range: {fast:.1}..{slow:.1} ms"
        );
    }
}
