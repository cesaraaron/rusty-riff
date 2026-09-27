use super::{OnePoleLp, param_changed};
use crate::dsp::biquad::Biquad;
use crate::dsp::oversample::Oversampler4;

/// Multi-voice fuzz pedal (Big Muff / Fuzz Face / Tone Bender MkII).
///
/// Signal path:
///   DC block → input HP (~70 Hz) → [4× OS: two cascaded asymmetric soft-clip
///   stages] → DC block → (Muff mid scoop) → variable tone LP → level
///
/// Fuzz character & authenticity:
///   • A fuzz is far more saturated than an overdrive/distortion: it slams the
///     signal into near-square clipping for long, singing sustain. We model the
///     classic two–transistor-stage gain structure as **two cascaded soft-clip
///     stages** inside the oversampler — one stage alone stays too "polite".
///   • The clipping is mildly **asymmetric**, which is what gives a fuzz its
///     spitty, gated edge and a touch of octave texture on the top.
///   • **Big Muff** (TYPE low): mid-scooped — a fixed dip around 700 Hz gives the
///     scooped, wall-of-sound timbre.
///   • **Fuzz Face** (TYPE mid): lower gain into a softer, rounder germanium-style
///     clip that keeps the midrange, so it cleans up and stays vocal.
///   • **Tone Bender MkII** (TYPE high): Jimmy Page's Led Zeppelin fuzz — three
///     germanium transistors running hotter and harder than a Fuzz Face, with a
///     thicker, more compressed midrange bite (no scoop).
///   • The tone control is a simple dark→bright low-pass sweep, like the passive
///     tone stage feeding the output buffer.
///   • 4× oversampling is essential here: square-ish clipping is extremely rich
///     in harmonics, so the alias products must be pushed well above the band.
pub struct Fuzz {
    sr: f32,
    dc_block: Biquad,
    input_hp: Biquad,
    os: Oversampler4,
    // Removes the DC the asymmetric clipper injects before it reaches the amp.
    post_dc: Biquad,
    // Fixed mid scoop — the Big Muff "smiley" voicing.
    scoop: Biquad,
    // Volume-dependent input loading for the Fuzz Face voice: the pedal's low input
    // impedance works against the guitar's volume pot + cable capacitance, so rolling
    // the guitar volume back both thins and cleans the fuzz. Modelled as an input HP
    // whose corner rises as `guitar` falls.
    guitar_hp: Biquad,
    last_guitar: f32,
    // Variable 1-pole low-pass for the tone control.
    tone: OnePoleLp,
    last_tone: f32,
}

impl Fuzz {
    pub fn new(sr: f32) -> Self {
        let mut fz = Self {
            sr,
            dc_block: Biquad::highpass(sr, 10.0, 0.707),
            // Tighten the very low end before the huge gain so the fuzz doesn't
            // turn to mud, but keep the guitar fundamental intact.
            input_hp: Biquad::highpass(sr, 70.0, 0.707),
            os: Oversampler4::new(sr),
            post_dc: Biquad::highpass(sr, 45.0, 0.707),
            // −9 dB dip at 700 Hz: the scooped Muff midrange.
            scoop: Biquad::peak_eq(sr, 700.0, 0.7, -9.0),
            guitar_hp: Biquad::highpass(sr, 70.0, 0.707),
            last_guitar: 1.0,
            tone: OnePoleLp::new(),
            last_tone: -1.0, // force first update
        };
        fz.set_tone(0.5);
        fz
    }

    fn set_tone(&mut self, tone: f32) {
        // tone 0 → ~400 Hz (dark/woolly), tone 1 → ~6 kHz (bright/buzzy)
        let freq = 400.0 * (6000.0_f32 / 400.0).powf(tone);
        self.tone.set_cutoff(freq, self.sr);
        self.last_tone = tone;
    }

    /// Rebuild the Fuzz Face input-loading HP for a new guitar-volume setting: the
    /// pedal's low input impedance eats the lows first as the guitar is rolled back.
    fn set_guitar(&mut self, guitar: f32) {
        let gv = guitar.clamp(0.03, 1.0);
        let corner = (70.0 * (1.0 / gv).powf(1.3)).clamp(70.0, 1200.0);
        self.guitar_hp = Biquad::highpass(self.sr, corner, 0.707);
        self.last_guitar = gv;
    }

    /// `fuzz` 0–1 (sustain/gain), `tone` 0–1, `level` 0–1, `kind` 0–1 selecting the
    /// voicing (low = Big Muff, mid = Fuzz Face, high = Tone Bender MkII; thresholds
    /// at 0.25 / 0.75), and `guitar` 0–1 = the guitar's volume knob as the pedal sees
    /// it (1 = full). Rolling `guitar` back cleans the fuzz up — strongest on the
    /// Fuzz Face, whose low input impedance thins and cleans as the pot closes.
    #[allow(clippy::too_many_arguments)]
    #[inline]
    pub fn process(
        &mut self,
        x: f32,
        fuzz: f32,
        tone: f32,
        level: f32,
        kind: f32,
        guitar: f32,
    ) -> f32 {
        if param_changed(tone, self.last_tone) {
            self.set_tone(tone);
        }
        if param_changed(guitar, self.last_guitar) {
            self.set_guitar(guitar);
        }
        let gv = guitar.clamp(0.03, 1.0);

        let x = self.dc_block.process(x);
        // The guitar's own volume attenuates what reaches the pedal.
        let x = x * gv;
        let x = self.input_hp.process(x);

        // Three fuzz voicings share the pedal. The Big Muff slams the signal into
        // cascaded near-square clippers and scoops the mids for its wall-of-sound;
        // the Fuzz Face runs a lower gain into a softer, rounder germanium-style
        // clip and keeps the midrange; the Tone Bender MkII drives a three-stage
        // germanium chain harder still, for its thicker, more compressed bite.
        let voice = if kind < 0.25 {
            Voice::Muff
        } else if kind < 0.75 {
            Voice::FuzzFace
        } else {
            Voice::ToneBender
        };
        // The Fuzz Face's low input impedance loads the guitar, so its cleanup is
        // stronger and it thins out as the guitar volume closes. Bypassed at full
        // volume so the pedal's default voice is unchanged.
        let x = if voice == Voice::FuzzFace && gv < 0.999 {
            self.guitar_hp.process(x)
        } else {
            x
        };
        let (x, level_scalar) = match voice {
            Voice::Muff => {
                // Enormous gain into the cascaded clippers — this is what makes it a
                // fuzz rather than an overdrive.
                let gain = 1.0 + fuzz * 120.0;
                let x = self.os.process(x, |u| {
                    let s1 = fuzz_clip(u * gain);
                    fuzz_clip(s1 * 2.5)
                });
                (x, 0.5)
            }
            Voice::FuzzFace => {
                // The Fuzz Face's input transistor is biased through the guitar's
                // volume pot, so rolling the guitar back shaves the stage gain too —
                // not just the signal level. Combined with the input scale above this
                // is what lets a Fuzz Face clean up dramatically as you turn down.
                let gain = (1.0 + fuzz * 55.0) * gv * gv;
                let x = self.os.process(x, |u| {
                    let s = ff_clip(u * gain);
                    ff_clip(s * 1.7)
                });
                (x, 0.62)
            }
            Voice::ToneBender => {
                // Hotter than the Fuzz Face and a touch harder-kneed: the MkII's
                // third germanium stage pushes it into a thicker, more sustained
                // clip while keeping the mids (no scoop).
                let gain = 1.0 + fuzz * 90.0;
                let x = self.os.process(x, |u| {
                    let s1 = tb_clip(u * gain);
                    let s2 = tb_clip(s1 * 2.1);
                    tb_clip(s2 * 1.3)
                });
                (x, 0.55)
            }
        };

        let x = self.post_dc.process(x);
        // Only the Big Muff scoops; the Fuzz Face and Tone Bender keep their mids.
        let x = match voice {
            Voice::Muff => self.scoop.process(x),
            _ => x,
        };

        self.tone.process(x) * level * level_scalar
    }
}

/// The three fuzz voicings the `TYPE` control switches between.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Voice {
    Muff,
    FuzzFace,
    ToneBender,
}

/// Asymmetric soft clipper for the fuzz gain stages.
///
/// The positive half saturates a touch harder than the negative half. Cascading
/// two of these drives the waveform toward a gated square wave (long sustain),
/// and the asymmetry seeds the even-harmonic "octave" shimmer fuzz is known for.
#[inline]
fn fuzz_clip(x: f32) -> f32 {
    if x >= 0.0 {
        x.tanh()
    } else {
        0.85 * (x / 0.85).tanh()
    }
}

/// Softer, rounder asymmetric clipper for the Fuzz Face voicing. A germanium
/// Fuzz Face clips more gradually than the Muff's near-square stages, and its two
/// transistors are less symmetric: this shaper has a gentler knee and a stronger
/// positive/negative asymmetry, giving the vocal, slightly gated Fuzz Face
/// character (and the even harmonics that asymmetry produces).
#[inline]
fn ff_clip(x: f32) -> f32 {
    if x >= 0.0 {
        (0.7 * x).tanh() / 0.7
    } else {
        0.8 * (0.9 * x / 0.8).tanh()
    }
}

/// Tone Bender MkII clipper. The MkII runs a third germanium transistor, so it
/// clips harder and with a sharper knee than a Fuzz Face, but keeps a little
/// positive/negative asymmetry for the even-harmonic grit that makes it sing.
#[inline]
fn tb_clip(x: f32) -> f32 {
    if x >= 0.0 {
        (1.15 * x).tanh() / 1.15
    } else {
        0.82 * ((x * 1.1) / 0.82).tanh()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::PI;

    /// The fuzz must stay finite and bounded even at maximum sustain on a hot, low
    /// note — its two cascaded clippers run at enormous gain, so any instability or
    /// runaway DC would show up here.
    #[test]
    fn finite_bounded_and_saturates() {
        let sr = 48_000.0;
        let mut fz = Fuzz::new(sr);
        let mut max_abs = 0.0f32;
        let mut sum = 0.0f64;
        let warmup = sr as usize / 4;
        let total = sr as usize;
        let mut count = 0u32;
        for n in 0..total {
            let x = (2.0 * PI * 82.41 * n as f32 / sr).sin() * 0.8;
            let y = fz.process(x, 1.0, 0.5, 0.7, 0.0, 1.0);
            assert!(y.is_finite(), "fuzz produced non-finite output at {n}");
            if n >= warmup {
                max_abs = max_abs.max(y.abs());
                sum += y as f64;
                count += 1;
            }
        }
        assert!(max_abs <= 1.0, "fuzz output exceeded bounds: {max_abs}");
        // Heavy clipping should still produce a healthy signal, not silence.
        assert!(max_abs > 0.05, "fuzz output too quiet: {max_abs}");
        // Asymmetric clipping is fine, but the post-DC block must keep the mean
        // near zero so the fuzz doesn't push DC into the amp.
        let dc = (sum / count as f64).abs();
        assert!(dc < 0.02, "fuzz has DC offset: {dc}");
    }

    /// The Fuzz Face and Tone Bender voicings must stay finite and bounded, and
    /// each must sound genuinely different from the Big Muff and from each other
    /// (different gain structure and mid handling) — a preset switching `type` has
    /// to hear a change.
    #[test]
    fn fuzz_voicings_are_distinct_and_bounded() {
        let sr = 48_000.0;
        let mut muff = Fuzz::new(sr);
        let mut face = Fuzz::new(sr);
        let mut bender = Fuzz::new(sr);
        let mut max_face = 0.0f32;
        let mut max_bender = 0.0f32;
        let mut diff_face = 0.0f32;
        let mut diff_bender = 0.0f32;
        for n in 0..(sr as usize) {
            let x = (2.0 * PI * 110.0 * n as f32 / sr).sin() * 0.7;
            let a = muff.process(x, 0.8, 0.5, 0.7, 0.0, 1.0);
            let b = face.process(x, 0.8, 0.5, 0.7, 0.5, 1.0);
            let c = bender.process(x, 0.8, 0.5, 0.7, 1.0, 1.0);
            assert!(b.is_finite() && c.is_finite(), "fuzz non-finite at {n}");
            max_face = max_face.max(b.abs());
            max_bender = max_bender.max(c.abs());
            diff_face += (a - b).abs();
            diff_bender += (a - c).abs();
        }
        assert!(max_face <= 2.0, "fuzz face output unbounded: {max_face}");
        assert!(
            max_bender <= 2.0,
            "tone bender output unbounded: {max_bender}"
        );
        assert!(max_face > 0.05, "fuzz face output too quiet: {max_face}");
        assert!(
            max_bender > 0.05,
            "tone bender output too quiet: {max_bender}"
        );
        assert!(
            diff_face / sr > 0.05,
            "fuzz face voicing barely differs from the Muff"
        );
        assert!(
            diff_bender / sr > 0.05,
            "tone bender voicing barely differs from the Muff"
        );
    }

    /// Single-bin DFT magnitude (f64 accumulator: the long windows and the small
    /// bins need the extra precision).
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

    /// The guitar-volume model: rolling the guitar back must *clean the Fuzz Face
    /// up* — harmonic distortion (upper harmonics over the fundamental) falls as the
    /// clipping eases. Full volume is the dirty fuzz.
    #[test]
    fn fuzz_face_cleans_up_as_guitar_volume_falls() {
        let sr = 48_000.0;
        let thd = |guitar: f32| {
            let mut fz = Fuzz::new(sr);
            let n = sr as usize;
            let warmup = n / 4;
            let mut out = Vec::with_capacity(n - warmup);
            for i in 0..n {
                let x = (2.0 * PI * 100.0 * i as f32 / sr).sin() * 0.8;
                let y = fz.process(x, 0.9, 0.5, 0.7, 0.5, guitar);
                assert!(y.is_finite(), "fuzz non-finite at {i}");
                if i >= warmup {
                    out.push(y);
                }
            }
            let fund = goertzel(&out, 100.0, sr);
            let upper: f32 = (2..=8).map(|k| goertzel(&out, 100.0 * k as f32, sr)).sum();
            upper / fund.max(1e-9)
        };
        let full = thd(1.0);
        let rolled = thd(0.3);
        assert!(
            rolled < full * 0.7,
            "fuzz face did not clean up with guitar volume: full THD {full:.3} vs rolled-back {rolled:.3}"
        );
    }
}
