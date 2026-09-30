use super::{OnePoleLp, SmoothedGain, param_changed};
use crate::dsp::biquad::Biquad;
use crate::dsp::oversample::Oversampler4;

/// Ibanez TS-808 Tube Screamer simulation.
///
/// Signal path (per the circuit, R.G. Keen, *The Technology of the Tube
/// Screamer*, geofex.com 1998):
///   input coupling → clipping-stage gain shape → symmetric diode clip →
///   output coupling → variable tone LP → level
///
/// **Input coupling.** The input buffer is an emitter follower; the signal
/// reaches the clipping stage through a **1 µF** coupling cap into the ~10 kΩ
/// bias network, i.e. a corner of a few Hz that does not touch the guitar band.
/// There is **no ~340 Hz input high-pass** — an earlier revision of this model
/// used `0.047 µF × 10 kΩ ≈ 340 Hz`, which is a misreading of the circuit and
/// over-cut the bass.
///
/// **The 720 Hz shape is a *gain* shelf, not a filter.** The clipping stage is a
/// non-inverting amp whose inverting leg is `4.7 kΩ + 0.047 µF` to AC ground:
/// `gain = 1 + Zf/Zi`, with `Zi = 4.7 kΩ + 1/(s·0.047 µF)`. At DC `Zi → ∞` so the
/// stage passes at **unity**; the gain rises on a first-order (6 dB/oct) curve
/// from the `1/(2π·4.7 kΩ·0.047 µF) ≈ 720 Hz` corner toward
/// `1 + (51 kΩ + Drive)/4.7 kΩ`. So the fundamental and low harmonics get a
/// partial lift and the mids/highs the full drive gain — the TS's mid-forward
/// "tightness" is that the bass is boosted *least*, not cut. We model exactly
/// that: a first-order 720 Hz high-shelf from unity up to the drive gain, before
/// a soft clipper.
///
/// **Clipping.** The stock TS-808 uses two anti-parallel silicon diodes: the
/// clip is **symmetric** and produces predominantly odd harmonics. (An earlier
/// revision modelled an asymmetric pair for "warmth"; that is the SD-1-style
/// mod, not the stock pedal, and is not used here.)
///
/// The absolute gain staging (pickup level → diode threshold) is an
/// approximation; the topology, the 720 Hz bass/treble split and the
/// drive-dependent `51 pF` feedback rolloff follow the source above.
pub struct TubeScreamer {
    sr: f32,
    dc_block: Biquad,    // input coupling (1 µF × 10 kΩ ≈ 16 Hz)
    shelf_hp: OnePoleLp, // HP leg of the first-order RC gain shelf (corner 720 Hz)
    /// Zf / Zi: the extra high-frequency gain, *smoothed*. The raw value runs to
    /// 117x (41 dB) at full drive, so an unsmoothed knob step is a 41 dB
    /// discontinuity in the waveform.
    shelf_gain: SmoothedGain,
    os: Oversampler4,     // 4× oversample the soft-clip stage to suppress aliasing
    fb_lp: OnePoleLp,     // 51 pF feedback pole: clipping-stage treble loss, drive-dependent
    out_dc_block: Biquad, // output coupling cap (10 µF × 100 Ω ≈ 16 Hz)
    tone: OnePoleLp,      // variable 1-pole LP tone control (1 kΩ / 0.22 µF network)
    last_tone: f32,
    last_drive: f32,
}

/// Clipping-stage series resistance and drive pot (circuit values, ohms).
const ZI_OHMS: f32 = 4_700.0;
/// Fixed feedback resistance (`51 kΩ`) plus the `Drive` pot (`500 kΩ`).
const ZF_FIXED_OHMS: f32 = 51_000.0;
const DRIVE_POT_OHMS: f32 = 500_000.0;
/// `Zi` network corner: `1/(2π · 4.7 kΩ · 0.047 µF)`.
const CORNER_720_HZ: f32 = 720.0;
/// Feedback capacitor (`51 pF`) across `Zf`; its pole rolls the clipping stage
/// off earlier as the drive pot raises `Zf`.
const FB_CAP_FARADS: f32 = 51e-12;

/// Feedback-network pole `1/(2π · Zf · 51 pF)` with `Zf = 51 kΩ + Drive·500 kΩ`:
/// ~61 kHz at minimum drive (inaudible, so the pedal is bright at low gain) down
/// to ~5.7 kHz at maximum — the TS's characteristic treble loss under gain.
fn feedback_corner_hz(drive: f32) -> f32 {
    let rf = ZF_FIXED_OHMS + drive.clamp(0.0, 1.0) * DRIVE_POT_OHMS;
    1.0 / (2.0 * std::f32::consts::PI * rf * FB_CAP_FARADS)
}

impl TubeScreamer {
    pub fn new(sr: f32) -> Self {
        let mut ts = Self {
            sr,
            dc_block: Biquad::highpass(sr, 16.0, 0.707),
            shelf_hp: OnePoleLp::new(),
            // Settled on the value `set_drive(0.0)` computes, so the first
            // samples are not ramped in from an unrelated gain.
            shelf_gain: SmoothedGain::new((ZF_FIXED_OHMS + 0.0 * DRIVE_POT_OHMS) / ZI_OHMS, sr),
            os: Oversampler4::new(sr),
            fb_lp: OnePoleLp::new(),
            out_dc_block: Biquad::highpass(sr, 16.0, 0.707),
            tone: OnePoleLp::new(),
            last_tone: -1.0, // force first update
            last_drive: -1.0,
        };
        ts.set_tone(0.6);
        ts.set_drive(0.0);
        ts
    }

    fn set_tone(&mut self, tone: f32) {
        // tone 0 → ~500 Hz (dark), tone 1 → ~7 kHz (bright)
        let freq = 500.0 * (7000.0_f32 / 500.0).powf(tone);
        self.tone.set_cutoff(freq, self.sr);
        self.last_tone = tone;
    }

    /// Recompute the clipping-stage gain shelf at the current drive. The stage is
    /// `1 + Zf/(R + 1/sC)`, i.e. a first-order 720 Hz high-shelf from unity (DC) up
    /// to `1 + Zf/R`; we apply `x + (Zf/R)·HP₇₂₀(x)` exactly.
    fn set_drive(&mut self, drive: f32) {
        self.shelf_hp.set_cutoff(CORNER_720_HZ, self.sr);
        self.shelf_gain
            .set((ZF_FIXED_OHMS + drive * DRIVE_POT_OHMS) / ZI_OHMS);
        self.fb_lp.set_cutoff(feedback_corner_hz(drive), self.sr);
        self.last_drive = drive;
    }

    /// `drive` 0–1, `tone` 0–1, `level` 0–1
    #[inline]
    pub fn process(&mut self, x: f32, drive: f32, tone: f32, level: f32) -> f32 {
        if param_changed(tone, self.last_tone) {
            self.set_tone(tone);
        }
        if param_changed(drive, self.last_drive) {
            self.set_drive(drive);
        }

        // Input coupling (≈ unity in the guitar band) then the 720 Hz first-order
        // RC gain shelf: unity at DC, rising toward the drive gain above 720 Hz.
        let x = self.dc_block.process(x);
        let hp = x - self.shelf_hp.process(x);
        let x = x + self.shelf_gain.step() * hp;

        // 4× oversampled symmetric diode soft-clip.
        let x = self.os.process(x, soft_clip);

        // Feedback-network pole: the clipping stage's own treble loss, moving down
        // from ~61 kHz to ~5.7 kHz as drive raises Zf.
        let x = self.fb_lp.process(x);

        // The real pedal's output coupling cap; strips any residual offset.
        let x = self.out_dc_block.process(x);

        self.tone.process(x) * level * 0.5
    }
}

/// Symmetric silicon-diode soft clip (two anti-parallel diodes in the feedback
/// path). Bounded to ±1 with a soft knee; odd-symmetric, so it adds odd
/// harmonics and no static DC.
#[inline]
fn soft_clip(x: f32) -> f32 {
    x.tanh()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::PI;

    const SR: f32 = 48_000.0;

    /// Single-bin amplitude estimate via the Goertzel algorithm (a unit sine reads
    /// ~1.0 over integer cycles). Computed in `f64`: the recurrence's poles sit on
    /// the unit circle, so over the long windows these tests use, an `f32`
    /// accumulator loses the small bins (the fundamental can read as noise).
    fn goertzel(samples: &[f32], f: f32, sr: f32) -> f32 {
        let w = 2.0 * std::f64::consts::PI * f as f64 / sr as f64;
        let coeff = 2.0 * w.cos();
        let (mut s1, mut s2) = (0.0f64, 0.0f64);
        for &x in samples {
            let s0 = x as f64 + coeff * s1 - s2;
            s2 = s1;
            s1 = s0;
        }
        let real = s1 - s2 * w.cos();
        let imag = s2 * w.sin();
        ((real * real + imag * imag).sqrt() / (samples.len() as f64 / 2.0)) as f32
    }

    fn rms(v: &[f32]) -> f64 {
        (v.iter().map(|s| (*s as f64) * (*s as f64)).sum::<f64>() / v.len() as f64).sqrt()
    }

    /// Render a steady sine through the TS at the given controls and return the
    /// settled tail. `f0` should divide the window into whole cycles so the Goertzel
    /// bins land exactly on the harmonics (no spectral leakage).
    fn render(f0: f32, amp: f32, drive: f32, tone: f32, level: f32) -> Vec<f32> {
        let mut ts = TubeScreamer::new(SR);
        let n = SR as usize; // 1.0 s
        let warmup = n / 4; // 12 000 — a whole number of periods for the test tones
        let mut out = Vec::with_capacity(n - warmup);
        for i in 0..n {
            let x = (2.0 * PI * f0 * i as f32 / SR).sin() * amp;
            let y = ts.process(x, drive, tone, level);
            assert!(y.is_finite(), "TS non-finite at {i}");
            if i >= warmup {
                out.push(y);
            }
        }
        out
    }

    /// The TS must stay finite and bounded across the whole control range, hot or
    /// cold input.
    #[test]
    fn finite_and_bounded_across_controls() {
        let mut ts = TubeScreamer::new(SR);
        for drive in [0.0f32, 0.3, 0.6, 1.0] {
            for tone in [0.0f32, 0.5, 1.0] {
                for n in 0..(SR as usize / 4) {
                    let x = (2.0 * PI * 110.0 * n as f32 / SR).sin() * 0.8;
                    let y = ts.process(x, drive, tone, 0.7);
                    assert!(y.is_finite(), "TS produced non-finite output");
                    assert!(y.abs() < 2.0, "TS output unbounded: {y}");
                }
            }
        }
    }

    /// Turning the tone knob up must brighten the output: more high-frequency
    /// energy passes through the variable low-pass.
    #[test]
    fn tone_controls_brightness() {
        let sr = 48_000.0;
        let high_energy = |tone: f32| {
            let mut ts = TubeScreamer::new(sr);
            let mut sum = 0.0f64;
            let warmup = sr as usize / 4;
            for n in 0..(sr as usize) {
                let x = (2.0 * PI * 4000.0 * n as f32 / sr).sin() * 0.1;
                let y = ts.process(x, 0.5, tone, 0.7);
                if n >= warmup {
                    sum += (y * y) as f64;
                }
            }
            sum
        };
        assert!(
            high_energy(0.9) > high_energy(0.1),
            "tone knob did not control brightness"
        );
    }

    // ── Clip transfer (unit tests on the diode model itself) ──────────────────

    /// The clipper must be a real bipolar saturator: sign-preserving, bounded, and
    /// monotonic. The sign check is a direct guard on the rectifier bug — negating
    /// the negative half once turned the stage into a full-wave rectifier.
    #[test]
    fn clip_preserves_sign_is_bounded_and_monotonic() {
        let mut prev = f32::NEG_INFINITY;
        let mut x = -8.0f32;
        while x <= 8.0 {
            let y = soft_clip(x);
            assert!(y.is_finite(), "clip non-finite at {x}");
            assert!(y > -1.0001 && y < 1.0001, "clip out of bounds at {x}: {y}");
            if x > 0.01 {
                assert!(y > 0.0, "positive input {x} produced non-positive {y}");
            }
            if x < -0.01 {
                assert!(y < 0.0, "negative input {x} produced non-negative {y}");
            }
            assert!(y >= prev - 1e-6, "clip not monotonic at {x}: {y} < {prev}");
            prev = y;
            x += 0.01;
        }
    }

    /// The stock TS-808 clips symmetrically (two anti-parallel silicon diodes), so
    /// the transfer is odd: `f(-x) == -f(x)`.
    #[test]
    fn clip_is_symmetric() {
        for a in [0.2f32, 0.8, 1.2, 4.0] {
            assert!(
                (soft_clip(a) + soft_clip(-a)).abs() < 1e-6,
                "clip not symmetric at ±{a}"
            );
        }
    }

    // ── Full-stage spectral behaviour ─────────────────────────────────────────

    /// Regression for the rectifier bug: the played fundamental must dominate its
    /// octave by a wide margin.
    #[test]
    fn negative_half_is_not_rectified() {
        for f0 in [440.0f32, 587.33, 659.25] {
            let out = render(f0, 0.1, 0.5, 0.6, 0.7);
            let fund = goertzel(&out, f0, SR);
            let octave = goertzel(&out, 2.0 * f0, SR);
            assert!(
                fund > 4.0 * octave,
                "{f0} Hz: octave ghost (fund {fund:.4}, 2f {octave:.4}) — rectifying?"
            );
        }
    }

    /// The output coupling cap must leave no static DC on the output.
    #[test]
    fn output_has_no_dc_offset() {
        for f0 in [110.0f32, 440.0, 880.0] {
            let out = render(f0, 0.1, 0.8, 0.6, 0.7);
            let mean = out.iter().map(|&x| x as f64).sum::<f64>() / out.len() as f64;
            assert!(mean.abs() < 1e-3, "{f0} Hz: DC offset {mean:.6}");
        }
    }

    /// A properly oversampled clipper puts essentially all of its energy on the
    /// exact harmonics of the input. Aliasing — the harsh digital "fizz" — would
    /// scatter energy onto inharmonic bins and pull this fraction down.
    #[test]
    fn output_is_harmonic_not_aliased() {
        for (f0, drive, tone) in [
            (440.0f32, 0.6f32, 0.6f32),
            (660.0, 0.9, 0.8),
            (2000.0, 0.8, 1.0),
        ] {
            let out = render(f0, 0.15, drive, tone, 0.7);
            let harm_pow: f64 = (1..=10)
                .map(|k| (goertzel(&out, f0 * k as f32, SR) as f64).powi(2) / 2.0)
                .sum();
            let frac = harm_pow / rms(&out).powi(2);
            assert!(
                frac > 0.95,
                "{f0} Hz: only {:.1}% of energy is harmonic — aliasing present",
                100.0 * frac
            );
        }
    }

    /// A mild TS overdrive adds harmonics but must not bury the played note: the
    /// fundamental stays the loudest partial across the usable range.
    #[test]
    fn fundamental_leads_the_spectrum() {
        for f0 in [220.0f32, 440.0, 523.25, 659.25] {
            let out = render(f0, 0.1, 0.45, 0.6, 0.7);
            let h: Vec<f32> = (1..=8).map(|k| goertzel(&out, f0 * k as f32, SR)).collect();
            let peak = h.iter().cloned().fold(0.0f32, f32::max);
            assert!(
                h[0] >= peak * 0.99,
                "{f0} Hz: fundamental ({:.4}) is not the loudest partial ({peak:.4})",
                h[0]
            );
        }
    }

    /// Turning the drive up must add saturation harmonics: total harmonic content
    /// relative to the fundamental rises monotonically with the drive knob.
    #[test]
    fn drive_increases_harmonic_distortion() {
        let thd = |drive: f32| {
            let out = render(523.25, 0.05, drive, 0.8, 0.7);
            let fund = goertzel(&out, 523.25, SR);
            let upper: f32 = (2..=8).map(|k| goertzel(&out, 523.25 * k as f32, SR)).sum();
            upper / fund.max(1e-9)
        };
        let (lo, mid, hi) = (thd(0.05), thd(0.4), thd(0.9));
        assert!(
            hi > mid && mid > lo,
            "drive did not add harmonics monotonically: {lo:.3} -> {mid:.3} -> {hi:.3}"
        );
    }

    /// The clipping-stage gain is the circuit's first-order RC shelf: unity at DC,
    /// rising monotonically with frequency toward `1 + Zf/Zi`. The 120 Hz low end is
    /// lifted (so the pedal is not thin) but less than the mid/top — and it is
    /// **not** cut, which was the old 340 Hz input-HP bug.
    #[test]
    fn shelf_rises_from_unity_toward_the_drive_gain() {
        // Tiny amplitude: stay on the clipper's linear region so this measures the
        // shelf shape rather than the clipping harmonics.
        let amp = 0.0005;
        let low = goertzel(&render(120.0, amp, 0.8, 1.0, 1.0), 120.0, SR);
        let mid = goertzel(&render(720.0, amp, 0.8, 1.0, 1.0), 720.0, SR);
        let high = goertzel(&render(3000.0, amp, 0.8, 1.0, 1.0), 3000.0, SR);
        // Passed/boosted, not cut below the dry reference (×level×0.5).
        assert!(
            low > amp * 0.5,
            "low end was cut: low {low:.6} vs dry {amp:.6}"
        );
        // A first-order shelf's magnitude rises monotonically with frequency.
        assert!(
            low < mid && mid < high,
            "shelf not rising: low {low:.6} mid {mid:.6} high {high:.6}"
        );
        // Lifted at the top, but only modestly over the bass (6 dB/oct RC shape,
        // not the old steep 2nd-order biquad that left the low end at unity).
        assert!(
            high > low * 1.3,
            "top not lifted over the low end: low {low:.6} high {high:.6}"
        );
    }

    /// The 51 pF feedback cap puts the clipping stage's treble pole at
    /// `1/(2π · Zf · 51 pF)`, with `Zf = 51 kΩ + Drive·500 kΩ` — ~61 kHz at min
    /// drive (bright) falling to ~5.7 kHz at max (the TS's treble loss under
    /// gain). Pins the sourced feedback pole against a drifting constant.
    #[test]
    fn feedback_pole_falls_with_drive() {
        let min = feedback_corner_hz(0.0);
        let max = feedback_corner_hz(1.0);
        assert!(
            (min - 61_200.0).abs() / 61_200.0 < 0.05,
            "min-drive feedback corner off: {min:.0} Hz"
        );
        assert!(
            (max - 5_660.0).abs() / 5_660.0 < 0.05,
            "max-drive feedback corner off: {max:.0} Hz"
        );
        assert!(max < min, "feedback pole must fall as drive rises");
    }

    /// The feedback pole must actually engage the stage: at max drive the 8 kHz
    /// gain ratio (high ÷ low drive) is cut below the shelf's own ratio. The 720 Hz
    /// shelf alone would lift 8 kHz by ~9.8×; the 5.7 kHz pole brings it to ~5.8×.
    /// Probed at a tiny amplitude so the clipper stays linear and only the filter
    /// (not clipping) is measured.
    #[test]
    fn feedback_pole_reduces_the_high_drive_treble_ratio() {
        let amp = 0.001;
        let gain = |drive: f32| goertzel(&render(8000.0, amp, drive, 1.0, 1.0), 8000.0, SR) / amp;
        let ratio = gain(1.0) / gain(0.0).max(1e-9);
        assert!(
            (3.0..7.5).contains(&ratio),
            "feedback pole missing or wrong: 8 kHz drive ratio {ratio:.2} (shelf-alone ≈9.8, with-pole ≈5.8)"
        );
    }
}
