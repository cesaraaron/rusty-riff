use std::f32::consts::TAU;

use super::SmoothedGain;
/// Stereo flanger: a short LFO-swept delay mixed back with the dry signal, the
/// moving comb-filter notches producing the classic "jet plane" sweep. Feedback
/// (regeneration) sharpens the notches into a resonant, metallic voice.
///
/// Sits in the stereo rack after the cab and parametric EQ, before the delay —
/// modulation belongs on the finished tone, ahead of the ambience. The two
/// channels share one LFO but read it a quarter-cycle apart, so the sweep drifts
/// across the stereo field instead of moving in lockstep.
///
/// Knob ranges (all normalised 0–1):
///   RATE     → LFO speed, 0.05–5 Hz (exponential).
///   DEPTH    → sweep width; the delay swings between `MIN_MS` and up to ~5 ms.
///   FEEDBACK → regeneration, 0–90%; higher = sharper, ringing notches.
///   MIX      → dry/wet blend; 0 = dry, 0.5 = deepest flange, 1 = fully wet.
pub struct Flanger {
    buf_l: Vec<f32>,
    buf_r: Vec<f32>,
    write: usize,
    phase: f32,
    /// Wet/dry, smoothed. `flanger_mix` is a MIDI CC target.
    mix: SmoothedGain,
    sr: f32,
}

/// Shortest delay in the sweep — a small floor keeps the interpolation reading
/// valid samples and avoids a through-zero click at the top of the sweep.
const MIN_MS: f32 = 0.5;
/// Longest additional delay the DEPTH knob can add on top of `MIN_MS`.
const SWEEP_MS: f32 = 4.5;
/// Buffer headroom above the deepest possible delay (`MIN_MS + SWEEP_MS`).
const MAX_MS: f32 = 6.0;
/// Electric Mistress sweep range — a shorter throw than the generic flanger.
const MISTRESS_MIN_MS: f32 = 0.4;
const MISTRESS_SWEEP_MS: f32 = 3.0;

impl Flanger {
    pub fn new(sr: f32) -> Self {
        let len = (sr * MAX_MS / 1000.0) as usize + 2;
        Self {
            buf_l: vec![0.0; len],
            buf_r: vec![0.0; len],
            write: 0,
            phase: 0.0,
            // `DEFAULT_FL_MIX`.
            mix: SmoothedGain::new(0.50, sr),
            sr,
        }
    }

    /// Interpolated read `delay` samples behind the write head (Catmull-Rom).
    #[inline]
    fn read(buf: &[f32], write: usize, delay: f32) -> f32 {
        super::read_cubic(buf, write, delay)
    }

    /// `kind` < 0.5 = the generic stereo flanger; ≥ 0.5 = **Electric Mistress**
    /// (EHX): a *mono* pedal with a shorter sweep throw, and its signature
    /// **Filter Matrix** — with DEPTH at zero the sweep freezes into a static comb.
    #[allow(clippy::too_many_arguments)]
    #[inline]
    pub fn process(
        &mut self,
        l: f32,
        r: f32,
        rate: f32,
        depth: f32,
        feedback: f32,
        mix: f32,
        kind: f32,
    ) -> (f32, f32) {
        let mistress = kind >= 0.5;

        // LFO advances once per sample; exponential map spreads the slow, musical
        // rates across most of the knob's travel.
        let rate_hz = 0.05 * 100.0_f32.powf(rate.clamp(0.0, 1.0));
        self.phase = (self.phase + rate_hz / self.sr).fract();

        let depth = depth.clamp(0.0, 1.0);
        let (min_ms, sweep_ms) = if mistress {
            (MISTRESS_MIN_MS, MISTRESS_SWEEP_MS)
        } else {
            (MIN_MS, SWEEP_MS)
        };
        // Filter Matrix: DEPTH at zero freezes the sweep into a static comb.
        let frozen = mistress && depth < 0.05;
        let lfo = |ph: f32| {
            if frozen {
                0.5
            } else {
                0.5 - 0.5 * (ph.fract() * TAU).cos()
            }
        };
        let span = min_ms + sweep_ms * depth;

        // The Electric Mistress is a mono pedal, so the *wet* path (and the
        // regeneration it feeds) collapses to one channel and drops the
        // quarter-cycle offset. The **dry** path stays stereo: folding the dry
        // signal would hard-mono everything downstream even at mix = 0, which
        // makes a fully-dry Mistress a different, wider-fading signal than it
        // was at mix = 1. Folding only the wet keeps it transparent.
        let m = 0.5 * (l + r);
        let (wet_in_l, wet_in_r) = if mistress { (m, m) } else { (l, r) };
        let del_l = (min_ms + span * lfo(self.phase)) * self.sr / 1000.0;
        let del_r = if mistress {
            del_l
        } else {
            (min_ms + span * lfo(self.phase + 0.25)) * self.sr / 1000.0
        };

        let wet_l = Self::read(&self.buf_l, self.write, del_l);
        let wet_r = Self::read(&self.buf_r, self.write, del_r);

        // Regeneration feeds the swept tap back in; capped below unity so the comb
        // never runs away.
        let fb = feedback.clamp(0.0, 1.0) * 0.9;
        self.buf_l[self.write] = wet_in_l + wet_l * fb;
        self.buf_r[self.write] = wet_in_r + wet_r * fb;
        let len = self.buf_l.len();
        self.write = (self.write + 1) % len;

        // Dry is the caller's own L/R; only the wet is the pedal's mono signal.
        self.mix.set(mix.clamp(0.0, 1.0));
        let mix = self.mix.step();
        (l * (1.0 - mix) + wet_l * mix, r * (1.0 - mix) + wet_r * mix)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::PI;

    const SR: f32 = 48_000.0;

    /// `mix = 0` must pass the dry signal through untouched on both channels,
    /// for **both** kinds. The Electric Mistress (`kind = 1.0`) is a mono pedal,
    /// so the regression this pins is that it used to fold L/R to mono *before*
    /// the mix - which made a fully-dry Mistress a hard mono fold of the whole
    /// rack. The differing L/R inputs are what catch a collapse.
    #[test]
    fn fully_dry_is_passthrough() {
        for kind in [0.0f32, 1.0] {
            let mut f = Flanger::new(SR);
            for n in 0..2000 {
                let x = (n as f32 * 0.03).sin();
                let (l, r) = f.process(x, x * 0.7, 0.4, 0.8, 0.6, 0.0, kind);
                assert!(
                    (l - x).abs() < 1e-6 && (r - x * 0.7).abs() < 1e-6,
                    "kind {kind} is not dry-transparent: got ({l}, {r})"
                );
            }
        }
    }

    /// A mono pedal must fold its *wet* to mono, not its dry. With a hard-panned
    /// input and the wet at full, the Mistress's output should be the same
    /// signal in both channels; at mix = 0 the original pair must survive.
    #[test]
    fn mistress_folds_the_wet_but_not_the_dry() {
        let mut f = Flanger::new(SR);
        for n in 0..4000 {
            let l_in = (n as f32 * 0.021).sin();
            let r_in = (n as f32 * 0.037).cos();
            let (l, r) = f.process(l_in, r_in, 0.4, 0.8, 0.5, 0.0, 1.0);
            assert!((l - l_in).abs() < 1e-6, "dry L was folded at {n}");
            assert!((r - r_in).abs() < 1e-6, "dry R was folded at {n}");
        }

        // Wet-only probe: mix = 1 must be a single mono signal in both channels.
        let mut f = Flanger::new(SR);
        let mut max_diff: f32 = 0.0;
        for n in 0..4000 {
            let l_in = (n as f32 * 0.021).sin();
            let r_in = (n as f32 * 0.037).cos();
            let (l, r) = f.process(l_in, r_in, 0.4, 0.8, 0.5, 1.0, 1.0);
            max_diff = max_diff.max((l - r).abs());
        }
        assert!(
            max_diff < 1e-6,
            "Mistress wet should be mono, L/R diverged by {max_diff}"
        );
    }

    /// Extreme settings (fast, deep, near-max feedback) must stay finite and
    /// bounded — the feedback comb must not blow up.
    #[test]
    fn finite_and_bounded_under_extreme_settings() {
        let mut f = Flanger::new(SR);
        let mut max_abs = 0.0f32;
        for n in 0..(SR as usize) {
            let x = (2.0 * PI * 220.0 * n as f32 / SR).sin() * 0.9;
            let (l, r) = f.process(x, x, 1.0, 1.0, 1.0, 0.5, 0.0);
            assert!(l.is_finite() && r.is_finite(), "non-finite at {n}");
            max_abs = max_abs.max(l.abs()).max(r.abs());
        }
        assert!(max_abs < 12.0, "feedback comb ran away: {max_abs}");
    }

    /// With a wet mix the moving notches must actually modulate the tone — a
    /// steady sine should come out with a time-varying envelope, not a constant.
    #[test]
    fn wet_output_sweeps_over_time() {
        let mut f = Flanger::new(SR);
        let mut min_e = f32::INFINITY;
        let mut max_e = 0.0f32;
        // Skip the first sweep so the buffer has filled.
        for n in 0..(SR as usize * 3) {
            let x = (2.0 * PI * 1500.0 * n as f32 / SR).sin();
            let (l, _r) = f.process(x, x, 0.6, 1.0, 0.5, 0.5, 0.0);
            if n > SR as usize {
                min_e = min_e.min(l.abs());
                max_e = max_e.max(l.abs());
            }
        }
        assert!(
            max_e - min_e > 0.1,
            "wet output not modulated (env {min_e:.3}..{max_e:.3})"
        );
    }

    /// Deeper DEPTH must sweep the delay across a wider range, so the swept comb
    /// notch travels further and the wet tone's envelope is modulated more
    /// deeply. Measured at a low frequency, where the notch spacing in delay-time
    /// is wide enough that a shallow sweep only grazes it while a deep one crosses
    /// it fully. (Peak amplitude, not RMS: RMS averages the modulation away.)
    #[test]
    fn deeper_depth_sweeps_further() {
        let envelope_range = |depth: f32| {
            let mut f = Flanger::new(SR);
            // Peak amplitude within each short window, so the metric follows the
            // notch sweep rather than the 300 Hz carrier.
            let mut window_peaks = Vec::new();
            let mut peak = 0.0f32;
            for n in 0..(SR as usize * 2) {
                let x = (2.0 * PI * 300.0 * n as f32 / SR).sin();
                let (l, _r) = f.process(x, x, 0.6, depth, 0.4, 0.5, 0.0);
                if n > SR as usize {
                    peak = peak.max(l.abs());
                    if n % 200 == 0 {
                        window_peaks.push(peak);
                        peak = 0.0;
                    }
                }
            }
            let hi = window_peaks.iter().cloned().fold(0.0f32, f32::max);
            let lo = window_peaks.iter().cloned().fold(f32::INFINITY, f32::min);
            hi - lo
        };
        assert!(
            envelope_range(1.0) > envelope_range(0.05) * 1.5,
            "depth knob does not widen the sweep"
        );
    }

    /// FEEDBACK is regeneration: an impulse must leave a longer-lived tail with
    /// feedback up than with it at zero.
    #[test]
    fn feedback_extends_the_tail() {
        let tail_energy = |fb: f32| {
            let mut f = Flanger::new(SR);
            f.process(1.0, 1.0, 0.2, 0.5, fb, 1.0, 0.0);
            let mut e = 0.0f64;
            for n in 1..4000 {
                let (l, _r) = f.process(0.0, 0.0, 0.2, 0.5, fb, 1.0, 0.0);
                if n > 500 {
                    e += (l * l) as f64;
                }
            }
            e
        };
        assert!(
            tail_energy(0.85) > tail_energy(0.0) * 2.0,
            "feedback does not lengthen the tail"
        );
    }

    /// Electric Mistress mode is mono: at **full wet** any stereo input collapses
    /// to one signal, so the two output channels are identical. At `mix = 0` the
    /// stereo image must survive untouched — the mono fold belongs to the wet,
    /// not to the pedal's input. (Folding the dry too meant a Mistress turned
    /// down silently mono-folded the whole downstream rack.)
    #[test]
    fn mistress_mode_is_mono() {
        let mut f = Flanger::new(SR);
        let mut max_diff = 0.0f32;
        for n in 0..(SR as usize) {
            let x = (2.0 * PI * 700.0 * n as f32 / SR).sin();
            let (l, r) = f.process(x, x * 0.3, 0.5, 0.6, 0.4, 1.0, 1.0);
            max_diff = max_diff.max((l - r).abs());
        }
        assert!(
            max_diff < 1e-6,
            "mistress not mono at full wet (L/R diff {max_diff})"
        );

        // Dry: the input is (x, 0.3x) with x peaking at 1.0, so the two
        // channels must still differ by 0.7.
        let mut f = Flanger::new(SR);
        let mut max_diff = 0.0f32;
        for n in 0..(SR as usize) {
            let x = (2.0 * PI * 700.0 * n as f32 / SR).sin();
            let (l, r) = f.process(x, x * 0.3, 0.5, 0.6, 0.4, 0.0, 1.0);
            max_diff = max_diff.max((l - r).abs());
        }
        assert!(
            (max_diff - 0.7).abs() < 1e-5,
            "mistress folded the stereo image when dry: L/R diff {max_diff}"
        );
    }

    /// The Electric Mistress **Filter Matrix**: with DEPTH at zero the sweep freezes,
    /// so a steady tone's envelope stops moving (a static comb, not a sweep).
    #[test]
    fn mistress_filter_matrix_freezes_the_sweep() {
        let envelope_range = |depth: f32| {
            let mut f = Flanger::new(SR);
            let mut window_peaks = Vec::new();
            let mut peak = 0.0f32;
            for n in 0..(SR as usize * 3) {
                let x = (2.0 * PI * 300.0 * n as f32 / SR).sin();
                let (l, _r) = f.process(x, x, 0.6, depth, 0.3, 0.5, 1.0);
                if n > SR as usize {
                    peak = peak.max(l.abs());
                    if n % 200 == 0 {
                        window_peaks.push(peak);
                        peak = 0.0;
                    }
                }
            }
            let hi = window_peaks.iter().cloned().fold(0.0f32, f32::max);
            let lo = window_peaks.iter().cloned().fold(f32::INFINITY, f32::min);
            hi - lo
        };
        assert!(
            envelope_range(0.0) < 0.02,
            "filter matrix still sweeps: {}",
            envelope_range(0.0)
        );
        assert!(
            envelope_range(1.0) > 0.1,
            "mistress sweep dead at full depth: {}",
            envelope_range(1.0)
        );
    }
}
