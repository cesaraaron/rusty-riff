//! Stereo-linked lookahead limiter for the master bus.
//!
//! The old master "limiter" was a memoryless rational clip: unity below a
//! `0.95` knee and a hard asymptote at 1.0. Sitting under a rig whose amps are
//! trimmed to a low RMS but whose transients are tall, it did the one thing a
//! limiter should never do — it *distorted* peaks instead of turning them down.
//! That is the harsh "saturation" you hear when a couple of pedals stack gain
//! into the ceiling.
//!
//! This is a real feed-forward limiter: it looks ahead, computes the gain that
//! keeps the output under a fixed ceiling, and applies it smoothly. Peaks are
//! turned down cleanly (gain reduction) rather than clipped, and a fixed makeup
//! gain lifts the whole program so the rig lands at a consumer-loud level.
//!
//! The detector is **stereo-linked** (one gain for both channels from the larger
//! of the two), so a hard-panned peak cannot shift the stereo image — which the
//! old per-channel clip did.

/// Lookahead window, milliseconds. Long enough to open the gain before a
/// transient reaches the output, short enough to be inaudible as latency.
const LOOKAHEAD_MS: f32 = 1.5;

/// Gain-*recovery* time constant, milliseconds. Slow, so the gain does not pump
/// between notes.
const RELEASE_MS: f32 = 150.0;

/// Gain-*application* time constant, milliseconds. Must be well under the
/// lookahead so the gain is fully down by the time the peak arrives, otherwise
/// the limiter overshoots and has to clip.
const ATTACK_MS: f32 = 0.15;

/// Output ceiling as a linear sample peak — roughly −1 dBFS of headroom.
pub const CEILING: f32 = 0.891;

/// Fixed makeup gain applied inside the limiter so the rig lands at a
/// consumer-loud level instead of the low RMS the amp trims target. Applied
/// *before* the ceiling, so it lifts quiet material and is absorbed by the
/// limiter on loud material rather than clipping at the converter.
pub const MAKEUP: f32 = 1.413; // +3.0 dB

/// A stereo-linked lookahead limiter. Real-time safe: all buffers are
/// preallocated by [`Limiter::new`], and `process` neither allocates nor blocks.
pub struct Limiter {
    delay_l: Vec<f32>,
    delay_r: Vec<f32>,
    pos: usize,
    /// Current gain, 1.0 when open. Applied to the delayed signal.
    gain: f32,
    attack: f32,
    release: f32,
}

impl Limiter {
    pub fn new(sr: f32) -> Self {
        let look = ((sr * LOOKAHEAD_MS / 1000.0).round() as usize).max(1);
        let sr = sr.max(1.0);
        Self {
            delay_l: vec![0.0; look],
            delay_r: vec![0.0; look],
            pos: 0,
            gain: 1.0,
            attack: 1.0 - (-1.0 / (ATTACK_MS / 1000.0 * sr)).exp(),
            release: 1.0 - (-1.0 / (RELEASE_MS / 1000.0 * sr)).exp(),
        }
    }

    /// Latency the lookahead adds, in samples.
    pub fn latency(&self) -> usize {
        self.delay_l.len()
    }

    /// Clear the lookahead and open the gain. Used when the limiter is
    /// re-seated (not on a normal parameter change).
    pub fn reset(&mut self) {
        self.delay_l.fill(0.0);
        self.delay_r.fill(0.0);
        self.pos = 0;
        self.gain = 1.0;
    }

    #[inline]
    pub fn process(&mut self, l: f32, r: f32) -> (f32, f32) {
        let li = l * MAKEUP;
        let ri = r * MAKEUP;
        let peak = li.abs().max(ri.abs());
        // Gain that keeps the peak at the ceiling; 1.0 when already under it.
        let target = if peak > CEILING { CEILING / peak } else { 1.0 };
        let coeff = if target < self.gain {
            self.attack
        } else {
            self.release
        };
        self.gain += (target - self.gain) * coeff;

        // The smoothed gain can lag a fast transient by a fraction of a dB on the
        // very first peak; the clamp makes the ceiling an exact guarantee. It only
        // ever touches that sub-percent overshoot, never steady state.
        let dl = (self.delay_l[self.pos] * self.gain).clamp(-CEILING, CEILING);
        let dr = (self.delay_r[self.pos] * self.gain).clamp(-CEILING, CEILING);
        self.delay_l[self.pos] = li;
        self.delay_r[self.pos] = ri;
        self.pos += 1;
        if self.pos == self.delay_l.len() {
            self.pos = 0;
        }
        (dl, dr)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quiet_signal_passes_at_makeup() {
        let mut lim = Limiter::new(48_000.0);
        // Prime past the lookahead with a quiet signal; the output mirrors the
        // input delayed by `latency`, scaled only by the makeup gain.
        let n = lim.latency();
        for _ in 0..n {
            lim.process(0.1, -0.1);
        }
        for _ in 0..100 {
            let (l, r) = lim.process(0.1, -0.1);
            assert!(
                (l - 0.1 * MAKEUP).abs() < 1e-5,
                "quiet signal must pass at makeup, got {l}"
            );
            assert!((r + 0.1 * MAKEUP).abs() < 1e-5);
        }
    }

    #[test]
    fn holds_the_ceiling_on_a_hot_signal() {
        let mut lim = Limiter::new(48_000.0);
        let mut peak = 0.0f32;
        // A full-scale square drives the detector hard; after the lookahead it
        // must be pinned at/under the ceiling.
        for i in 0..4000 {
            let x = if i % 2 == 0 { 1.0 } else { -1.0 };
            let (l, r) = lim.process(x, x);
            if i > lim.latency() + 100 {
                peak = peak.max(l.abs()).max(r.abs());
            }
        }
        assert!(
            peak <= CEILING + 1e-4,
            "limiter let a sample through at {peak} (ceiling {CEILING})"
        );
    }

    #[test]
    fn stereo_linked_a_panned_peak_keeps_the_image() {
        let mut lim = Limiter::new(48_000.0);
        // Left is hot and right is silent: both channels must receive the same
        // gain, so the silent side stays silent rather than one side clipping.
        let mut l_peak = 0.0f32;
        let mut r_peak = 0.0f32;
        for i in 0..4000 {
            let x = if i % 2 == 0 { 1.0 } else { -1.0 };
            let (l, r) = lim.process(x, 0.0);
            if i > lim.latency() + 100 {
                l_peak = l_peak.max(l.abs());
                r_peak = r_peak.max(r.abs());
            }
        }
        assert!(l_peak <= CEILING + 1e-4);
        assert!(r_peak < 1e-6, "the silent channel must stay silent");
    }

    #[test]
    fn latency_is_the_lookahead_window() {
        let lim = Limiter::new(48_000.0);
        // 1.5 ms at 48 kHz.
        assert_eq!(lim.latency(), 72);
    }

    #[test]
    fn recovers_after_the_peak() {
        let mut lim = Limiter::new(48_000.0);
        // Slam it, then feed silence long enough for the release to recover.
        for _ in 0..500 {
            lim.process(1.0, 1.0);
        }
        for _ in 0..48_000 {
            lim.process(0.0, 0.0);
        }
        // A quiet signal now passes at unity again.
        for _ in 0..200 {
            lim.process(0.1, 0.1);
        }
        for _ in 0..10 {
            let (l, _) = lim.process(0.1, 0.1);
            assert!((l - 0.1 * MAKEUP).abs() < 2e-3, "gain did not recover: {l}");
        }
    }
}
