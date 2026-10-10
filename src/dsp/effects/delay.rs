use std::f32::consts::TAU;

use crate::dsp::biquad::Biquad;
use crate::dsp::oversample::Oversampler4;

use super::SmoothedGain;
/// Stereo delay with two voicings selected by `kind`.
///
/// **Digital** (`kind` low) — tempo-free stereo ping-pong: feedback cross-feeds
/// L↔R so repeats bounce across the stereo field. TIME 0–1 maps to 0–500 ms,
/// FEEDBACK 0–1 maps to 0–85% to prevent runaway.
///
/// **Tape** (`kind` high) — an Echoplex EP-3 style echo: each repeat is
/// high-cut and softly saturated (the tape/head bump and compression), and the
/// transport runs slightly unsteady — a slow wow and a faster flutter modulate
/// the read position, so held notes drift the way tape does. Feedback returns to
/// the *same* channel (a mono tape deck, not a ping-pong), at a lower ceiling so
/// the darker repeats don't build up.
pub struct Delay {
    buf_l: Vec<f32>,
    buf_r: Vec<f32>,
    write: usize,
    /// Wet/dry, smoothed. `delay_mix` is a MIDI CC target.
    mix: SmoothedGain,
    sr: f32,
    // Tape transport: wow/flutter LFO phases and the per-channel feedback damping.
    wow_phase: f32,
    flutter_phase: f32,
    damp_l: Biquad,
    damp_r: Biquad,
    // Echorec drum band-limiting (high-cut + low-cut), per channel.
    ec_lp_l: Biquad,
    ec_lp_r: Biquad,
    ec_hp_l: Biquad,
    ec_hp_r: Biquad,
    // 4× oversamplers around the tape/echorec `tanh`, one per channel. The
    // saturation sits on the *feedback* path, so any aliasing would recirculate and
    // could self-sustain; oversampling keeps the repeats clean.
    sat_os_l: Oversampler4,
    sat_os_r: Oversampler4,
}

/// Tape speed modulation: a slow wow plus a faster flutter, as fractions of the
/// delay time (±0.25% / ±0.12% — about ±4 cents combined, an audible but musical
/// drift).
const WOW_HZ: f32 = 0.7;
const WOW_DEPTH: f32 = 0.0025;
const FLUTTER_HZ: f32 = 6.3;
const FLUTTER_DEPTH: f32 = 0.0012;
/// Tape feedback repeats lose their top end (the record/play head gap loss).
const TAPE_DAMP_HZ: f32 = 3200.0;
/// Tape saturation: gentle, so repeated echoes thicken rather than distort.
const TAPE_SAT: f32 = 0.8;

/// Echorec multi-head taps: `(fractional position within the drum period, weight)`.
/// The Binson's playback heads sit at fixed points on the drum, so one pass yields
/// a *cluster* of unevenly spaced repeats rather than a single echo. The drum
/// period is the base delay (`time`), and the cluster recirculates through the
/// first head.
const ECHOREC_HEADS: [(f32, f32); 3] = [(0.30, 0.55), (0.58, 0.80), (1.00, 1.0)];
/// Echorec band limitation: the drum/head gap loss high-cuts, and the tube
/// record/replay electronics low-cut, so repeats are dark and slightly thin.
const ECHOREC_LP_HZ: f32 = 2400.0;
const ECHOREC_HP_HZ: f32 = 90.0;
/// Echorec feedback ceiling (fraction of the knob), below tape's so the cluster
/// doesn't build up.
const ECHOREC_FB: f32 = 0.72;

impl Delay {
    pub fn new(sr: f32) -> Self {
        let max_samples = (sr * 0.5) as usize + 1; // 500 ms max
        Self {
            buf_l: vec![0.0; max_samples],
            buf_r: vec![0.0; max_samples],
            write: 0,
            sr,
            wow_phase: 0.0,
            flutter_phase: 0.0,
            damp_l: Biquad::lowpass(sr, TAPE_DAMP_HZ, 0.707),
            damp_r: Biquad::lowpass(sr, TAPE_DAMP_HZ, 0.707),
            ec_lp_l: Biquad::lowpass(sr, ECHOREC_LP_HZ, 0.707),
            ec_lp_r: Biquad::lowpass(sr, ECHOREC_LP_HZ, 0.707),
            ec_hp_l: Biquad::highpass(sr, ECHOREC_HP_HZ, 0.707),
            ec_hp_r: Biquad::highpass(sr, ECHOREC_HP_HZ, 0.707),
            sat_os_l: Oversampler4::new(sr),
            sat_os_r: Oversampler4::new(sr),
            // `DEFAULT_DELAY_MIX`.
            mix: SmoothedGain::new(0.30, sr),
        }
    }

    /// Oversampled tape saturation on the feedback path, one instance per channel.
    #[inline]
    fn tape_sat_os(&mut self, l: f32, r: f32) -> (f32, f32) {
        let sl = self.sat_os_l.process(l, tape_sat);
        let sr = self.sat_os_r.process(r, tape_sat);
        (sl, sr)
    }

    /// `time`, `feedback`, `mix` 0–1; `kind` 0 = digital ping-pong, 0.5 = Echorec
    /// drum echo, 1 = tape. The thresholds keep `0.0`/`1.0` exactly as before.
    #[inline]
    pub fn process(
        &mut self,
        l: f32,
        r: f32,
        time: f32,
        feedback: f32,
        mix_param: f32,
        kind: f32,
    ) -> (f32, f32) {
        let echorec = kind > 0.25 && kind < 0.75;
        let tape = kind >= 0.75;
        let len = self.buf_l.len();
        let base = time * self.sr * 0.5;
        // Aim the smoother once; each branch below takes one `next()` step.
        self.mix.set(mix_param.clamp(0.0, 1.0));

        // Wow + flutter modulate the read position for the moving-media modes;
        // digital is rock steady.
        let wobble = if tape || echorec {
            self.wow_phase = (self.wow_phase + WOW_HZ / self.sr).fract();
            self.flutter_phase = (self.flutter_phase + FLUTTER_HZ / self.sr).fract();
            let wow = (TAU * self.wow_phase).sin() * WOW_DEPTH;
            let flutter = (TAU * self.flutter_phase).sin() * FLUTTER_DEPTH;
            1.0 + wow + flutter
        } else {
            1.0
        };

        if echorec {
            // Sum the fixed head taps into one cluster, normalised so the total
            // weight keeps the wet level comparable to the other modes.
            let p = (base * wobble).clamp(1.0, (len - 2) as f32);
            let (mut wl, mut wr) = (0.0f32, 0.0f32);
            for &(frac, w) in &ECHOREC_HEADS {
                wl += w * read_tap(&self.buf_l, self.write, p * frac);
                wr += w * read_tap(&self.buf_r, self.write, p * frac);
            }
            let norm = 1.0 / ECHOREC_HEADS.iter().map(|&(_, w)| w).sum::<f32>();
            let fl = self.ec_hp_l.process(self.ec_lp_l.process(wl * norm));
            let fr = self.ec_hp_r.process(self.ec_lp_r.process(wr * norm));
            let (wl, wr) = self.tape_sat_os(fl, fr);
            self.buf_l[self.write] = l + wl * feedback * ECHOREC_FB;
            self.buf_r[self.write] = r + wr * feedback * ECHOREC_FB;
            self.write = (self.write + 1) % len;
            // One `next()` for the pair: calling it twice would advance the
            // smoother twice and give L and R different mix values.
            let (dg, wg) = super::constant_power_mix(self.mix.step());
            let out_l = l * dg + wl * wg;
            let out_r = r * dg + wr * wg;
            return (out_l, out_r);
        }

        let delay = base * wobble;
        let delayed_l = read_tap(&self.buf_l, self.write, delay);
        let delayed_r = read_tap(&self.buf_r, self.write, delay);

        if tape {
            let fb = feedback * 0.7;
            let dl = self.damp_l.process(delayed_l);
            let dr = self.damp_r.process(delayed_r);
            let (dl, dr) = self.tape_sat_os(dl, dr);
            self.buf_l[self.write] = l + dl * fb;
            self.buf_r[self.write] = r + dr * fb;
        } else {
            let fb = feedback * 0.85;
            // Cross-fed feedback → ping-pong.
            self.buf_l[self.write] = l + delayed_r * fb;
            self.buf_r[self.write] = r + delayed_l * fb;
        }
        self.write = (self.write + 1) % len;

        // Constant-power crossfade: the repeat is decorrelated from the dry, so a
        // linear fader would scoop ~3 dB out of the middle of the knob. This keeps
        // the linear wet/dry ratio, so the tuned balance is unchanged.
        let (dg, wg) = super::constant_power_mix(self.mix.step());
        let out_l = l * dg + delayed_l * wg;
        let out_r = r * dg + delayed_r * wg;
        (out_l, out_r)
    }
}

/// Fractional read of `buf` at `delay` samples behind `write` (linear
/// interpolation), clamped so it never reads the future or wraps past the buffer.
#[inline]
fn read_tap(buf: &[f32], write: usize, delay: f32) -> f32 {
    super::read_cubic(buf, write, delay)
}

/// Soft tape saturation, applied under 4× oversampling on the feedback path
/// (see `Delay::tape_sat_os`) — thickens the repeats and tames the runaway without
/// an audible fuzz, and without alias products recirculating through the loop.
#[inline]
fn tape_sat(x: f32) -> f32 {
    (x * TAPE_SAT).tanh() / TAPE_SAT
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `mix = 0` must pass the dry signal through unchanged, in every voicing.
    #[test]
    fn fully_dry_is_passthrough() {
        for kind in [0.0f32, 0.5, 1.0] {
            let mut d = Delay::new(48_000.0);
            for n in 0..1000 {
                let x = (n as f32 * 0.02).sin();
                let (l, r) = d.process(x, x, 0.3, 0.5, 0.0, kind);
                assert!((l - x).abs() < 1e-6 && (r - x).abs() < 1e-6);
            }
        }
    }

    /// A single impulse fed only to the left must re-emerge on the *left* one delay
    /// later (the direct tap), then bounce to the *right* after a second delay — the
    /// cross-fed feedback that makes the echoes ping-pong across the stereo field.
    #[test]
    fn impulse_pings_across_channels() {
        let sr = 48_000.0;
        let mut d = Delay::new(sr);
        let time = 0.2; // → 0.2 * sr * 0.5 = 4800 samples
        let delay_samples = (time * sr * 0.5) as usize;

        // Impulse on the left only, then silence; fully wet with feedback.
        d.process(1.0, 0.0, time, 0.6, 1.0, 0.0);
        // `peak` finds the loudest sample index in `[lo, hi)` for one channel.
        let mut left = (0.0f32, 0usize);
        let mut right = (0.0f32, 0usize);
        for n in 1..(delay_samples * 3) {
            let (l, r) = d.process(0.0, 0.0, time, 0.6, 1.0, 0.0);
            assert!(l.is_finite() && r.is_finite());
            if l.abs() > left.0 {
                left = (l.abs(), n);
            }
            if r.abs() > right.0 {
                right = (r.abs(), n);
            }
        }
        // First repeat lands on the left at ~one delay; the bounce lands on the
        // right at ~two delays.
        assert!(left.0 > 0.5, "left echo missing");
        assert!(right.0 > 0.1, "ping-pong bounce to right missing");
        assert!(
            (left.1 as i64 - delay_samples as i64).abs() <= 2,
            "left echo at {}, expected ~{delay_samples}",
            left.1
        );
        assert!(
            (right.1 as i64 - 2 * delay_samples as i64).abs() <= 2,
            "right bounce at {}, expected ~{}",
            right.1,
            2 * delay_samples
        );
    }

    /// Tape voicing does not ping-pong: an impulse on the left stays on the left,
    /// its repeats damped and saturated.
    #[test]
    fn tape_stays_on_the_same_channel() {
        let sr = 48_000.0;
        let mut d = Delay::new(sr);
        let time = 0.2;
        let delay_samples = (time * sr * 0.5) as usize;

        d.process(1.0, 0.0, time, 0.6, 1.0, 1.0);
        let mut left_energy = 0.0f32;
        let mut right_energy = 0.0f32;
        for n in 1..(delay_samples * 3) {
            let (l, r) = d.process(0.0, 0.0, time, 0.6, 1.0, 1.0);
            assert!(l.is_finite() && r.is_finite());
            // Ignore the first few samples where the dry-ish lead-in lives.
            if n > 8 {
                left_energy += l.abs();
                right_energy += r.abs();
            }
        }
        assert!(left_energy > 0.1, "tape echo missing on the left");
        assert!(
            right_energy < left_energy * 0.05,
            "tape should not ping-pong: right {right_energy:.4} vs left {left_energy:.4}"
        );
    }

    /// The Echorec sums several fixed playback heads, so one impulse yields a
    /// *cluster* of unevenly spaced repeats — not the single echo of tape/digital.
    #[test]
    fn echorec_produces_a_multi_head_cluster() {
        let sr = 48_000.0;
        let mut d = Delay::new(sr);
        let time = 0.2;
        let base = time * sr * 0.5; // 4800 samples
        // Impulse, fully wet, no feedback — isolates the single-pass head cluster.
        d.process(1.0, 1.0, time, 0.0, 1.0, 0.5);
        let n = (base * 1.2) as usize;
        let mut out = Vec::with_capacity(n);
        for _ in 0..n {
            let (l, _r) = d.process(0.0, 0.0, time, 0.0, 1.0, 0.5);
            out.push(l.abs());
        }
        let peak_near = |center: f32| {
            let c = center as usize;
            let lo = c.saturating_sub(48);
            let hi = (c + 48).min(out.len());
            out[lo..hi].iter().cloned().fold(0.0f32, f32::max)
        };
        let head = |frac: f32| peak_near(frac * base);
        let (p1, p2, p3) = (head(0.30), head(0.58), head(1.00));
        assert!(
            p1 > 0.02 && p2 > 0.02 && p3 > 0.02,
            "missing head echoes: {p1:.3} {p2:.3} {p3:.3}"
        );
        // The gaps between heads must be clearly quieter than the heads themselves:
        // a genuine cluster, not a smear.
        let gap = head(0.44).max(head(0.79));
        assert!(
            gap < p1 * 0.7,
            "heads not distinct: gap {gap:.4} vs first head {p1:.4}"
        );
    }

    /// Like tape, the Echorec is a mono drum deck: a left-only impulse stays left.
    #[test]
    fn echorec_stays_on_the_same_channel() {
        let sr = 48_000.0;
        let mut d = Delay::new(sr);
        let time = 0.2;
        let n = (time * sr * 0.5 * 3.0) as usize;
        d.process(1.0, 0.0, time, 0.6, 1.0, 0.5);
        let (mut le, mut re) = (0.0f32, 0.0f32);
        for i in 1..n {
            let (l, r) = d.process(0.0, 0.0, time, 0.6, 1.0, 0.5);
            assert!(l.is_finite() && r.is_finite());
            if i > 8 {
                le += l.abs();
                re += r.abs();
            }
        }
        assert!(le > 0.1, "echorec echo missing on the left");
        assert!(
            re < le * 0.05,
            "echorec should not ping-pong: right {re:.4} vs left {le:.4}"
        );
    }
}
