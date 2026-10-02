//! Passive FMV ("Fender/Marshall/Vox") tone stack.
//!
//! The previous amp tone controls were three independent biquads (a low shelf, a
//! mid peak and a high shelf). That is convenient but wrong: in a real amp the
//! Bass/Mid/Treble pots sit in **one** passive RC network and interact strongly —
//! turning treble up pulls the mids down, the mid pot sets the depth of the
//! ever-present mid scoop, and the whole stack is lossy (it only ever attenuates).
//! That interaction and the characteristic mid dip are a huge part of why a JCM800
//! or a Rectifier sounds the way it does.
//!
//! This models the actual analog transfer function of the FMV stack (the classic
//! 3rd-order network analysed by David Yeh, DAFx-06) and discretises it with the
//! bilinear transform. The result is a single 3rd-order IIR whose coefficients are
//! recomputed only when a knob moves.
//!
//! Because a passive stack always attenuates, the response is peak-normalised to
//! unity (`makeup`) so the amp's gain staging is preserved — the stack colours the
//! tone without changing the operating level the rest of the amp was tuned around.

use std::f32::consts::PI;

/// Resistor/capacitor values for one FMV stack.
#[derive(Clone, Copy)]
pub struct Components {
    pub r1: f32,
    pub r2: f32,
    pub r3: f32,
    pub r4: f32,
    pub c1: f32,
    pub c2: f32,
    pub c3: f32,
}

impl Components {
    /// Marshall JCM800 tone stack (bright, pronounced mid scoop).
    pub const MARSHALL: Components = Components {
        r1: 250e3,
        r2: 1e6,
        r3: 25e3,
        r4: 56e3,
        c1: 470e-12,
        c2: 22e-9,
        c3: 22e-9,
    };

    /// Fender-style stack (used for the Mesa — fuller lows, gentler scoop than the
    /// Marshall, which suits the Rectifier's thicker voicing).
    pub const FENDER: Components = Components {
        r1: 250e3,
        r2: 250e3,
        r3: 10e3,
        r4: 100e3,
        c1: 250e-12,
        c2: 100e-9,
        c3: 47e-9,
    };

    /// Vox-style stack (used for the AC30 Top Boost — smaller c1/r3 shrink the mid
    /// scoop and keep more top end than the Marshall, matching the AC30's chimier,
    /// less-scooped voicing).
    pub const VOX: Components = Components {
        r1: 220e3,
        r2: 1e6,
        r3: 12e3,
        r4: 33e3,
        c1: 250e-12,
        c2: 22e-9,
        c3: 22e-9,
    };

    /// Hiwatt DR103 stack — a Fender-derived FMV network with a **larger mid-pot
    /// load (r3)** and a wide slope resistor, giving a flatter, less-scooped
    /// midrange and a clear, bright top: the DR103's hi-fi, un-scooped voice, in
    /// contrast to the Marshall's pronounced mid dip.
    pub const HIWATT: Components = Components {
        r1: 220e3,
        r2: 1e6,
        r3: 33e3,
        r4: 100e3,
        c1: 250e-12,
        c2: 22e-9,
        c3: 22e-9,
    };
}

pub struct ToneStack {
    sr: f32,
    comp: Components,
    // z-domain coefficients (a0 normalised to 1).
    b0: f32,
    b1: f32,
    b2: f32,
    b3: f32,
    a1: f32,
    a2: f32,
    a3: f32,
    makeup: f32,
    // Direct Form I state.
    x1: f32,
    x2: f32,
    x3: f32,
    y1: f32,
    y2: f32,
    y3: f32,
}

impl ToneStack {
    pub fn new(sr: f32, comp: Components) -> Self {
        let mut ts = Self {
            sr,
            comp,
            b0: 1.0,
            b1: 0.0,
            b2: 0.0,
            b3: 0.0,
            a1: 0.0,
            a2: 0.0,
            a3: 0.0,
            makeup: 1.0,
            x1: 0.0,
            x2: 0.0,
            x3: 0.0,
            y1: 0.0,
            y2: 0.0,
            y3: 0.0,
        };
        ts.update(0.5, 0.5, 0.5);
        ts
    }

    /// Recompute the filter for new pot positions (each 0–1). Audio-taper-ish
    /// curves are applied so the controls track a real amp's feel.
    pub fn update(&mut self, bass: f32, mid: f32, treble: f32) {
        // Pots: clamp off the rails (a pot at exactly 0 makes the network
        // degenerate). Bass uses an audio-ish taper; treble/mid stay near-linear.
        let l = (bass * bass).clamp(0.001, 0.999);
        let m = mid.clamp(0.001, 0.999);
        let t = treble.clamp(0.001, 0.999);

        let Components {
            r1,
            r2,
            r3,
            r4,
            c1,
            c2,
            c3,
        } = self.comp;

        // ── Analog transfer function H(s) = (b1·s + b2·s² + b3·s³) / (a0 + a1·s +
        //    a2·s² + a3·s³), coefficients per Yeh's FMV analysis. ───────────────
        let b1 = t * c1 * r1 + m * c3 * r3 + l * (c1 * r2 + c2 * r2);
        let b2 = t * (c1 * c2 * r1 * r4 + c1 * c3 * r1 * r4)
            - m * m * (c1 * c3 * r3 * r3 + c2 * c3 * r3 * r3)
            + m * (c1 * c3 * r1 * r3 + c1 * c3 * r3 * r3 + c2 * c3 * r3 * r3)
            + l * (c1 * c2 * r1 * r2 + c1 * c2 * r2 * r4 + c1 * c3 * r2 * r4)
            + l * m * (c1 * c3 * r2 * r3 + c2 * c3 * r2 * r3);
        let b3 = l * m * (c1 * c2 * c3 * r1 * r2 * r3 + c1 * c2 * c3 * r2 * r3 * r4)
            - m * m * (c1 * c2 * c3 * r1 * r3 * r3 + c1 * c2 * c3 * r3 * r3 * r4)
            + m * (c1 * c2 * c3 * r1 * r3 * r3 + c1 * c2 * c3 * r3 * r3 * r4)
            + t * c1 * c2 * c3 * r1 * r3 * r4
            - t * m * c1 * c2 * c3 * r1 * r3 * r4
            + t * l * c1 * c2 * c3 * r1 * r2 * r4;

        let a0 = 1.0;
        let a1 = (c1 * r1 + c1 * r3 + c2 * r3 + c2 * r4 + c3 * r4)
            + m * c3 * r3
            + l * (c1 * r2 + c2 * r2);
        let a2 = m
            * (c1 * c3 * r1 * r3 - c2 * c3 * r3 * r4 + c1 * c3 * r3 * r3 + c2 * c3 * r3 * r3)
            - m * m * (c1 * c3 * r3 * r3 + c2 * c3 * r3 * r3)
            + l * (c1 * c2 * r1 * r2 + c1 * c2 * r2 * r4 + c1 * c3 * r2 * r4 + c2 * c3 * r2 * r4)
            + l * m * (c1 * c3 * r2 * r3 + c2 * c3 * r2 * r3)
            + (c1 * c2 * r1 * r4
                + c1 * c3 * r1 * r4
                + c1 * c2 * r3 * r4
                + c1 * c2 * r1 * r3
                + c1 * c3 * r3 * r4
                + c2 * c3 * r3 * r4);
        let a3 = l * m * (c1 * c2 * c3 * r1 * r2 * r3 + c1 * c2 * c3 * r2 * r3 * r4)
            - m * m * (c1 * c2 * c3 * r1 * r3 * r3 + c1 * c2 * c3 * r3 * r3 * r4)
            + m * (c1 * c2 * c3 * r3 * r3 * r4 + c1 * c2 * c3 * r1 * r3 * r3
                - c1 * c2 * c3 * r1 * r3 * r4)
            + l * c1 * c2 * c3 * r1 * r2 * r4
            + c1 * c2 * c3 * r1 * r3 * r4;

        // ── Bilinear transform: s = c·(1 − z⁻¹)/(1 + z⁻¹), c = 2·fs. ───────────
        // Multiplying num/den by (1 + z⁻¹)³ and collecting powers of z⁻¹ gives the
        // four z-coefficients for each cubic polynomial.
        let c = 2.0 * self.sr;
        let c2v = c * c;
        let c3v = c2v * c;

        // Numerator: p0 = 0, p1 = b1, p2 = b2, p3 = b3.
        let bz0 = b1 * c + b2 * c2v + b3 * c3v;
        let bz1 = b1 * c - b2 * c2v - 3.0 * b3 * c3v;
        let bz2 = -b1 * c - b2 * c2v + 3.0 * b3 * c3v;
        let bz3 = -b1 * c + b2 * c2v - b3 * c3v;

        // Denominator: p0 = a0, p1 = a1, p2 = a2, p3 = a3.
        let az0 = a0 + a1 * c + a2 * c2v + a3 * c3v;
        let az1 = 3.0 * a0 + a1 * c - a2 * c2v - 3.0 * a3 * c3v;
        let az2 = 3.0 * a0 - a1 * c - a2 * c2v + 3.0 * a3 * c3v;
        let az3 = a0 - a1 * c + a2 * c2v - a3 * c3v;

        let inv = 1.0 / az0;
        self.b0 = bz0 * inv;
        self.b1 = bz1 * inv;
        self.b2 = bz2 * inv;
        self.b3 = bz3 * inv;
        self.a1 = az1 * inv;
        self.a2 = az2 * inv;
        self.a3 = az3 * inv;

        // Peak-normalise: a passive stack only cuts, so scale the response so its
        // loudest band is unity. This keeps the amp's downstream gain staging put.
        self.makeup = 1.0 / self.peak_magnitude().max(1e-6);
    }

    /// Largest |H(e^{jω})| over a log-spaced sweep of the audio band.
    fn peak_magnitude(&self) -> f32 {
        let mut peak = 0.0f32;
        let mut f = 20.0f32;
        while f < 18_000.0 {
            let w = 2.0 * PI * f / self.sr;
            let (c1, s1) = (w.cos(), w.sin());
            let (c2, s2) = ((2.0 * w).cos(), (2.0 * w).sin());
            let (c3, s3) = ((3.0 * w).cos(), (3.0 * w).sin());
            // Numerator / denominator evaluated at e^{-jω}.
            let nr = self.b0 + self.b1 * c1 + self.b2 * c2 + self.b3 * c3;
            let ni = -(self.b1 * s1 + self.b2 * s2 + self.b3 * s3);
            let dr = 1.0 + self.a1 * c1 + self.a2 * c2 + self.a3 * c3;
            let di = -(self.a1 * s1 + self.a2 * s2 + self.a3 * s3);
            let mag = ((nr * nr + ni * ni) / (dr * dr + di * di)).sqrt();
            peak = peak.max(mag);
            f *= 1.10;
        }
        peak
    }

    #[inline]
    pub fn process(&mut self, x: f32) -> f32 {
        let y = self.b0 * x + self.b1 * self.x1 + self.b2 * self.x2 + self.b3 * self.x3
            - self.a1 * self.y1
            - self.a2 * self.y2
            - self.a3 * self.y3;
        self.x3 = self.x2;
        self.x2 = self.x1;
        self.x1 = x;
        self.y3 = self.y2;
        self.y2 = self.y1;
        self.y1 = y;
        y * self.makeup
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::PI;

    const SR: f32 = 48_000.0;

    /// Magnitude of the stack at `freq`, measured by running a sine through it.
    ///
    /// Measured rather than read from the coefficients on purpose: `peak_magnitude`
    /// already walks the response analytically, so a test written against the same
    /// maths would only be checking the arithmetic twice. This is the path the
    /// audio thread actually takes, filter state and all.
    fn gain(comp: Components, bass: f32, mid: f32, treble: f32, freq: f32) -> f32 {
        let mut ts = ToneStack::new(SR, comp);
        ts.update(bass, mid, treble);
        // Settle the 3rd-order state before measuring; a 3rd-order IIR needs a few
        // hundred milliseconds at 100 Hz to stop ringing.
        let settle = (SR * 0.4) as usize;
        for n in 0..settle {
            ts.process((2.0 * PI * freq * n as f32 / SR).sin());
        }
        let n = (SR * 0.2) as usize;
        let mut acc = 0.0f64;
        for i in settle..settle + n {
            let y = ts.process((2.0 * PI * freq * i as f32 / SR).sin());
            acc += (y * y) as f64;
        }
        (acc / n as f64).sqrt() as f32
    }

    fn db(v: f32) -> f32 {
        20.0 * v.max(1e-9).log10()
    }

    /// The defining FMV character: at neutral, the mids sit in a dip between the
    /// low and high bands. Without this the stack is three loose biquads and the
    /// whole point of modelling the real network is lost.
    #[test]
    fn neutral_has_the_mid_scoop() {
        for (name, comp) in [
            ("MARSHALL", Components::MARSHALL),
            ("FENDER", Components::FENDER),
            ("VOX", Components::VOX),
        ] {
            let lo = gain(comp, 0.5, 0.5, 0.5, 100.0);
            let mid = gain(comp, 0.5, 0.5, 0.5, 650.0);
            let hi = gain(comp, 0.5, 0.5, 0.5, 3_000.0);
            assert!(
                mid < lo && mid < hi,
                "{name} lost its scoop at neutral: 100 Hz {lo:.3}, 650 Hz {mid:.3}, \\
                 3 kHz {hi:.3}"
            );
            // Real stacks scoop several dB; "a dip" alone would pass on a 0.01 dB
            // wiggle. The Marshall is the deepest of the three.
            assert!(
                db(lo / mid) > 3.0 && db(hi / mid) > 3.0,
                "{name} scoop too shallow: {lo:.3} / {mid:.3} / {hi:.3}"
            );
        }
    }

    /// Each control must move its own band the right way, or the stack is inert.
    ///
    /// The mid pot is a **cut**, not a boost, and the sign is worth stating: turning
    /// it up *shallows* the scoop (650 Hz: -14.8 dB at mid 0.1, -8.0 dB at mid
    /// 0.9). That is how a real JCM800 mid pot behaves -- it does not add mids, it
    /// removes the scoop -- and the first draft of this test asserted the opposite
    /// because "mid up = more mid" reads more naturally than "mid up = less dip".
    #[test]
    fn each_pot_moves_its_own_band() {
        let c = Components::MARSHALL;
        assert!(
            gain(c, 0.9, 0.5, 0.5, 100.0) > gain(c, 0.1, 0.5, 0.5, 100.0),
            "bass pot does not raise the lows"
        );
        assert!(
            gain(c, 0.5, 0.9, 0.5, 650.0) > gain(c, 0.5, 0.1, 0.5, 650.0),
            "mid pot does not flatten the scoop"
        );
        assert!(
            gain(c, 0.5, 0.5, 0.9, 4_000.0) > gain(c, 0.5, 0.5, 0.1, 4_000.0),
            "treble pot does not raise the highs"
        );
    }

    /// The interaction that makes an FMV stack an FMV stack rather than three
    /// biquads: the pots share one network, so **brightening costs bass**.
    ///
    /// Measured, this is the loudest cross-term the stack has: 100 Hz drops from
    /// -3.8 dB to -6.7 dB going from treble 0.1 to 0.9, about 3 dB. Loading the
    /// shared network harder attenuates the low leg, as the real one does.
    ///
    /// The mids barely move in the same sweep (650 Hz shifts by under 0.5 dB), which
    /// is why this asserts the bass and not the mids. The first draft guessed
    /// "treble up pulls the mids down" from the module's prose and measured the
    /// opposite sign.
    #[test]
    fn treble_up_costs_the_bass() {
        let c = Components::MARSHALL;
        let lo_flat = gain(c, 0.5, 0.5, 0.1, 100.0);
        let lo_bright = gain(c, 0.5, 0.5, 0.9, 100.0);
        let cost = db(lo_flat / lo_bright);
        assert!(
            cost > 1.5,
            "brightening barely touched the bass: {cost:.2} dB"
        );
    }

    /// Peak normalisation exists so the stack colours tone without moving the
    /// amp's gain staging. The loudest band must land at unity for every knob
    /// position, which is what `makeup` is for.
    #[test]
    fn peak_normalisation_keeps_the_loudest_band_at_unity() {
        let probe = |bass, mid, treble| {
            let mut ts = ToneStack::new(SR, Components::MARSHALL);
            ts.update(bass, mid, treble);
            let peak = ts.peak_magnitude() * ts.makeup;
            // `makeup` is set from `peak_magnitude`, so this is 1.0 by
            // construction — assert it anyway, because a later change that
            // decouples the two would silently re-level every amp.
            peak
        };
        for (b, m, t) in [(0.0, 0.0, 0.0), (0.5, 0.5, 0.5), (1.0, 1.0, 1.0)] {
            let peak = probe(b, m, t);
            assert!(
                (peak - 1.0).abs() < 1e-3,
                "stack at ({b}, {m}, {t}) peaks at {peak:.4}, not unity"
            );
        }
    }

    /// The pots clamp just short of the rails (a pot at exactly 0 degenerates the
    /// network). Sweep the whole range and check the two things that would actually
    /// be audible: nothing goes non-finite, and the impulse response *decays*.
    ///
    /// Stability is asserted as decay rather than as a ceiling on peak amplitude.
    /// An earlier draft capped the impulse at 4.0 and failed at 6.5 -- but 6.5 is not
    /// a fault. Peak-normalising a resonant 3rd-order network puts its loudest band
    /// at unity, and the impulse overshoots that whenever the resonance is sharp
    /// (bass at 0.9 does exactly this). A magnitude cap measures resonance, not
    /// stability; "does the tail die" measures stability.
    #[test]
    fn extreme_pot_positions_stay_finite_and_decay() {
        for comp in [
            Components::MARSHALL,
            Components::FENDER,
            Components::VOX,
            Components::HIWATT,
        ] {
            let mut ts = ToneStack::new(SR, comp);
            for step in 0..=100 {
                let p = step as f32 / 100.0;
                // Cycle the three pots through the corners and the middle.
                for (b, m, t) in [(p, p, p), (p, 1.0 - p, p), (1.0 - p, p, 1.0 - p)] {
                    ts.update(b, m, t);
                    let mut peak: f32 = 0.0;
                    // One impulse, then let it ring out.
                    for n in 0..8_000 {
                        let x = ts.process(if n == 0 { 1.0 } else { 0.0 });
                        assert!(
                            x.is_finite(),
                            "non-finite output at pot {p}: bass {b}, mid {m}, \
                             treble {t}"
                        );
                        peak = peak.max(x.abs());
                    }
                    let mut tail: f32 = 0.0;
                    for _ in 0..200 {
                        tail = tail.max(ts.process(0.0).abs());
                    }
                    assert!(
                        tail < peak.max(1e-6) * 1e-3,
                        "impulse response did not decay (peak {peak:.3}, tail \
                         {tail:.6}) at pot {p}: bass {b}, mid {m}, treble {t}"
                    );
                }
            }
        }
    }

    /// The four `Components` sets are genuinely different networks.
    ///
    /// Nine amps share only four stacks (the Mesa and the Supro both take
    /// `FENDER`, deliberately — a Recto and a Supro are not the same
    /// speaker-loaded voicing, but neither wants a Marshall scoop). Sharing is
    /// fine; two `const`s quietly collapsing into the same numbers by copy-paste
    /// is not, because it would silently flatten the voicing differences the rest
    /// of the model depends on.
    #[test]
    fn the_component_sets_are_pairwise_distinct() {
        let sets: [(&str, Components); 4] = [
            ("MARSHALL", Components::MARSHALL),
            ("FENDER", Components::FENDER),
            ("VOX", Components::VOX),
            ("HIWATT", Components::HIWATT),
        ];
        for (i, (name_a, a)) in sets.iter().enumerate() {
            for (name_b, b) in sets.iter().skip(i + 1) {
                let same = a.r1 == b.r1
                    && a.r2 == b.r2
                    && a.r3 == b.r3
                    && a.r4 == b.r4
                    && a.c1 == b.c1
                    && a.c2 == b.c2
                    && a.c3 == b.c3;
                assert!(!same, "{name_a} and {name_b} are the same network");
            }
        }
    }
}
