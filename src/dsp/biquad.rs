use std::f32::consts::PI;

/// Second-order IIR filter, Direct Form II Transposed.
/// Coefficients follow the Audio EQ Cookbook by Robert Bristow-Johnson.
///
/// Every design has a constructor (`highpass`, `low_shelf`, …) **and** a
/// matching `set_*` method that recomputes coefficients *in place* while
/// preserving `z1`/`z2`. The `set_*` form is what a live control change must
/// use: rebuilding a `Biquad` re-zeros the two state words, and the discarded
/// `z1` is a large fraction of signal amplitude for a shelving or EQ filter, so
/// a fresh `Biquad` is an audible zipper on every knob step. The cookbook maths
/// lives once per design in a `*_coeffs` helper, shared by the constructor and
/// the setter, so the two can never drift apart.
pub struct Biquad {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    z1: f32,
    z2: f32,
}

impl Biquad {
    /// A pass-through filter: unity gain, no state.
    fn unity() -> Self {
        Self {
            b0: 1.0,
            b1: 0.0,
            b2: 0.0,
            a1: 0.0,
            a2: 0.0,
            z1: 0.0,
            z2: 0.0,
        }
    }

    /// Store normalised coefficients, **preserving** `z1`/`z2`.
    ///
    /// This is the primitive every `set_*` goes through. Preserving the state
    /// is what makes a live coefficient change click-free: the recursion
    /// continues from where it was, so the only discontinuity is the (much
    /// smaller) change in the transfer function itself.
    #[inline]
    fn set_coeffs(&mut self, [b0, b1, b2, a0, a1, a2]: [f32; 6]) {
        self.b0 = b0 / a0;
        self.b1 = b1 / a0;
        self.b2 = b2 / a0;
        self.a1 = a1 / a0;
        self.a2 = a2 / a0;
    }

    /// Store normalised coefficients, **zeroing** `z1`/`z2`. Only for
    /// construction — see [`set_coeffs`](Self::set_coeffs) for live changes.
    fn init_coeffs(&mut self, c: [f32; 6]) {
        self.set_coeffs(c);
        self.z1 = 0.0;
        self.z2 = 0.0;
    }

    // ── Cookbook designs ─────────────────────────────────────────────────────
    //
    // Each returns un-normalised `(b0, b1, b2, a0, a1, a2)`. Keeping these
    // separate from the storage is what lets the constructor and the in-place
    // setter share one implementation of the maths.

    fn hp_coeffs(sr: f32, freq: f32, q: f32) -> [f32; 6] {
        let w0 = 2.0 * PI * freq / sr;
        let (s, c) = (w0.sin(), w0.cos());
        let alpha = s / (2.0 * q);
        [
            (1.0 + c) / 2.0,
            -(1.0 + c),
            (1.0 + c) / 2.0,
            1.0 + alpha,
            -2.0 * c,
            1.0 - alpha,
        ]
    }

    fn lp_coeffs(sr: f32, freq: f32, q: f32) -> [f32; 6] {
        let w0 = 2.0 * PI * freq / sr;
        let (s, c) = (w0.sin(), w0.cos());
        let alpha = s / (2.0 * q);
        [
            (1.0 - c) / 2.0,
            1.0 - c,
            (1.0 - c) / 2.0,
            1.0 + alpha,
            -2.0 * c,
            1.0 - alpha,
        ]
    }

    fn low_shelf_coeffs(sr: f32, freq: f32, gain_db: f32) -> [f32; 6] {
        let a = 10.0_f32.powf(gain_db / 40.0);
        let w0 = 2.0 * PI * freq / sr;
        let (s, c) = (w0.sin(), w0.cos());
        let alpha = s / 2.0 * 2.0_f32.sqrt();
        let sq = 2.0 * a.sqrt() * alpha;
        [
            a * ((a + 1.0) - (a - 1.0) * c + sq),
            2.0 * a * ((a - 1.0) - (a + 1.0) * c),
            a * ((a + 1.0) - (a - 1.0) * c - sq),
            (a + 1.0) + (a - 1.0) * c + sq,
            -2.0 * ((a - 1.0) + (a + 1.0) * c),
            (a + 1.0) + (a - 1.0) * c - sq,
        ]
    }

    fn high_shelf_coeffs(sr: f32, freq: f32, gain_db: f32) -> [f32; 6] {
        let a = 10.0_f32.powf(gain_db / 40.0);
        let w0 = 2.0 * PI * freq / sr;
        let (s, c) = (w0.sin(), w0.cos());
        let alpha = s / 2.0 * 2.0_f32.sqrt();
        let sq = 2.0 * a.sqrt() * alpha;
        [
            a * ((a + 1.0) + (a - 1.0) * c + sq),
            -2.0 * a * ((a - 1.0) + (a + 1.0) * c),
            a * ((a + 1.0) + (a - 1.0) * c - sq),
            (a + 1.0) - (a - 1.0) * c + sq,
            2.0 * ((a - 1.0) - (a + 1.0) * c),
            (a + 1.0) - (a - 1.0) * c - sq,
        ]
    }

    fn peak_eq_coeffs(sr: f32, freq: f32, q: f32, gain_db: f32) -> [f32; 6] {
        let a = 10.0_f32.powf(gain_db / 40.0);
        let w0 = 2.0 * PI * freq / sr;
        let (s, c) = (w0.sin(), w0.cos());
        let alpha = s / (2.0 * q);
        [
            1.0 + alpha * a,
            -2.0 * c,
            1.0 - alpha * a,
            1.0 + alpha / a,
            -2.0 * c,
            1.0 - alpha / a,
        ]
    }

    // ── High-pass ────────────────────────────────────────────────────────────

    pub fn highpass(sr: f32, freq: f32, q: f32) -> Self {
        let mut b = Self::unity();
        b.init_coeffs(Self::hp_coeffs(sr, freq, q));
        b
    }

    /// Retune a [`highpass`](Self::highpass) in place, preserving state.
    pub fn set_highpass(&mut self, sr: f32, freq: f32, q: f32) {
        self.set_coeffs(Self::hp_coeffs(sr, freq, q));
    }

    // ── Low-pass ─────────────────────────────────────────────────────────────

    pub fn lowpass(sr: f32, freq: f32, q: f32) -> Self {
        let mut b = Self::unity();
        b.init_coeffs(Self::lp_coeffs(sr, freq, q));
        b
    }

    /// Retune a [`lowpass`](Self::lowpass) in place, preserving state.
    pub fn set_lowpass(&mut self, sr: f32, freq: f32, q: f32) {
        self.set_coeffs(Self::lp_coeffs(sr, freq, q));
    }

    // ── Low shelf ────────────────────────────────────────────────────────────

    pub fn low_shelf(sr: f32, freq: f32, gain_db: f32) -> Self {
        let mut b = Self::unity();
        b.init_coeffs(Self::low_shelf_coeffs(sr, freq, gain_db));
        b
    }

    /// Retune a [`low_shelf`](Self::low_shelf) in place, preserving state.
    pub fn set_low_shelf(&mut self, sr: f32, freq: f32, gain_db: f32) {
        self.set_coeffs(Self::low_shelf_coeffs(sr, freq, gain_db));
    }

    // ── High shelf ───────────────────────────────────────────────────────────

    pub fn high_shelf(sr: f32, freq: f32, gain_db: f32) -> Self {
        let mut b = Self::unity();
        b.init_coeffs(Self::high_shelf_coeffs(sr, freq, gain_db));
        b
    }

    /// Retune a [`high_shelf`](Self::high_shelf) in place, preserving state.
    pub fn set_high_shelf(&mut self, sr: f32, freq: f32, gain_db: f32) {
        self.set_coeffs(Self::high_shelf_coeffs(sr, freq, gain_db));
    }

    // ── Peak EQ ──────────────────────────────────────────────────────────────

    pub fn peak_eq(sr: f32, freq: f32, q: f32, gain_db: f32) -> Self {
        let mut b = Self::unity();
        b.init_coeffs(Self::peak_eq_coeffs(sr, freq, q, gain_db));
        b
    }

    /// Retune a [`peak_eq`](Self::peak_eq) in place, preserving state.
    pub fn set_peak_eq(&mut self, sr: f32, freq: f32, q: f32, gain_db: f32) {
        self.set_coeffs(Self::peak_eq_coeffs(sr, freq, q, gain_db));
    }

    // ── Band-pass ────────────────────────────────────────────────────────────

    /// Constant 0 dB peak-gain band-pass (cookbook BPF). Unity at the centre
    /// frequency, falling away on both sides — used to tap a resonant band for
    /// the speaker-load resonance model.
    pub fn bandpass(sr: f32, freq: f32, q: f32) -> Self {
        let w0 = 2.0 * PI * freq / sr;
        let (s, c) = (w0.sin(), w0.cos());
        let alpha = s / (2.0 * q);
        let mut b = Self::unity();
        b.init_coeffs([alpha, 0.0, -alpha, 1.0 + alpha, -2.0 * c, 1.0 - alpha]);
        b
    }

    #[inline]
    pub fn process(&mut self, x: f32) -> f32 {
        let y = self.b0 * x + self.z1;
        self.z1 = self.b1 * x - self.a1 * y + self.z2;
        self.z2 = self.b2 * x - self.a2 * y;
        y
    }

    /// Steady-state magnitude response at angular frequency `w` (radians/sample).
    ///
    /// `H(z) = (b0 + b1 z^-1 + b2 z^-2) / (1 + a1 z^-1 + a2 z^-2)` evaluated on
    /// the unit circle, so `z = e^{jw} = cos(w) - j sin(w)`.
    #[cfg(test)]
    pub fn gain_at(&self, w: f32) -> f32 {
        let (c, s) = (w.cos(), w.sin());
        // Real and imaginary parts of e^{-jw} and e^{-2jw}.
        let e1 = (c, -s);
        let e2 = (2.0 * c * c - 1.0, -2.0 * s * c);
        let num = (
            self.b0 + self.b1 * e1.0 + self.b2 * e2.0,
            self.b1 * e1.1 + self.b2 * e2.1,
        );
        let den = (
            1.0 + self.a1 * e1.0 + self.a2 * e2.0,
            self.a1 * e1.1 + self.a2 * e2.1,
        );
        let num_mag = (num.0 * num.0 + num.1 * num.1).sqrt();
        let den_mag = (den.0 * den.0 + den.1 * den.1).sqrt();
        num_mag / den_mag
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::PI;

    /// Every design's `set_*` must produce **exactly** the same coefficients as
    /// its constructor. The state-preserving setters are only trustworthy if
    /// they are the same filter.
    #[test]
    fn setters_match_their_constructors_bit_for_bit() {
        let sr = 48_000.0;
        let cases: &[(f32, f32, f32)] = &[(0.0, 0.25, 1.0), (0.5, 0.5, 0.0), (1.0, 1.0, 1.0)];

        for &(f, q, g) in cases {
            let a = Biquad::highpass(sr, f, q);
            let mut b = Biquad::unity();
            b.set_highpass(sr, f, q);
            assert_eq!(a.b0, b.b0, "hp b0 mismatch at f={f} q={q}");
            assert_eq!(a.b1, b.b1, "hp b1 mismatch at f={f} q={q}");
            assert_eq!(a.b2, b.b2, "hp b2 mismatch at f={f} q={q}");
            assert_eq!(a.a1, b.a1, "hp a1 mismatch at f={f} q={q}");
            assert_eq!(a.a2, b.a2, "hp a2 mismatch at f={f} q={q}");

            let a = Biquad::lowpass(sr, f, q);
            let mut b = Biquad::unity();
            b.set_lowpass(sr, f, q);
            assert_eq!(a.b0, b.b0, "lp b0 mismatch at f={f} q={q}");
            assert_eq!(a.a1, b.a1, "lp a1 mismatch at f={f} q={q}");

            let a = Biquad::low_shelf(sr, f, g);
            let mut b = Biquad::unity();
            b.set_low_shelf(sr, f, g);
            assert_eq!(a.b0, b.b0, "lowshelf b0 mismatch at f={f} g={g}");
            assert_eq!(a.b2, b.b2, "lowshelf b2 mismatch at f={f} g={g}");
            assert_eq!(a.a1, b.a1, "lowshelf a1 mismatch at f={f} g={g}");

            let a = Biquad::high_shelf(sr, f, g);
            let mut b = Biquad::unity();
            b.set_high_shelf(sr, f, g);
            assert_eq!(a.b0, b.b0, "highshelf b0 mismatch at f={f} g={g}");
            assert_eq!(a.b1, b.b1, "highshelf b1 mismatch at f={f} g={g}");
            assert_eq!(a.b2, b.b2, "highshelf b2 mismatch at f={f} g={g}");
            assert_eq!(a.a1, b.a1, "highshelf a1 mismatch at f={f} g={g}");
            assert_eq!(a.a2, b.a2, "highshelf a2 mismatch at f={f} g={g}");

            let a = Biquad::peak_eq(sr, f, q, g);
            let mut b = Biquad::unity();
            b.set_peak_eq(sr, f, q, g);
            assert_eq!(a.b0, b.b0, "peak b0 mismatch at f={f} q={q} g={g}");
            assert_eq!(a.b2, b.b2, "peak b2 mismatch at f={f} q={q} g={g}");
            assert_eq!(a.a1, b.a1, "peak a1 mismatch at f={f} q={q} g={g}");
        }
    }

    /// The whole point of the `set_*` form: a live retune must not disturb the
    /// recursion. This is the regression the rebuild-on-knob-move bug caused.
    #[test]
    fn set_high_shelf_preserves_state_across_a_live_change() {
        let sr = 48_000.0;
        let mut b = Biquad::high_shelf(sr, 1800.0, 6.0);
        // Run it up so z1/z2 hold a real signal.
        let mut warm = [0.0f32; 64];
        for (i, w) in warm.iter_mut().enumerate() {
            *w = b.process((i as f32 * 0.1).sin() * 0.5);
        }
        let (z1, z2) = (b.z1, b.z2);
        assert!(
            z1.abs() > 1e-6,
            "test needs non-trivial state to be meaningful"
        );

        b.set_high_shelf(sr, 1800.0, -12.0);
        assert_eq!((b.z1, b.z2), (z1, z2), "set_* must not touch the state");

        // A fresh Biquad is exactly the defect being fixed: identical
        // coefficients, but zeroed state.
        let fresh = Biquad::high_shelf(sr, 1800.0, -12.0);
        assert_eq!(fresh.z1, 0.0);
        assert_eq!(fresh.z2, 0.0);
        assert_eq!(fresh.b0, b.b0, "same filter, so same coefficients");
    }

    /// Each `set_*` must be a no-op for state; only the coefficients may move.
    #[test]
    fn every_setter_preserves_state() {
        let sr = 48_000.0;
        let mut filters: Vec<(&str, Biquad)> = vec![
            ("hp", Biquad::highpass(sr, 300.0, 0.707)),
            ("lp", Biquad::lowpass(sr, 3000.0, 0.707)),
            ("lowshelf", Biquad::low_shelf(sr, 500.0, 9.0)),
            ("highshelf", Biquad::high_shelf(sr, 1800.0, -9.0)),
            ("peak", Biquad::peak_eq(sr, 1000.0, 2.0, 6.0)),
        ];
        for (_, f) in &mut filters {
            for i in 0..32 {
                f.process((i as f32).sin() * 0.3);
            }
        }
        let states: Vec<(f32, f32)> = filters.iter().map(|(_, f)| (f.z1, f.z2)).collect();

        filters[0].1.set_highpass(sr, 700.0, 0.707);
        filters[1].1.set_lowpass(sr, 9000.0, 0.707);
        filters[2].1.set_low_shelf(sr, 500.0, -9.0);
        filters[3].1.set_high_shelf(sr, 1800.0, 9.0);
        filters[4].1.set_peak_eq(sr, 1000.0, 2.0, -6.0);

        for ((name, f), &(z1, z2)) in filters.iter().zip(&states) {
            assert_eq!((f.z1, f.z2), (z1, z2), "{name} setter zeroed its state");
        }
    }

    /// A live gain change with preserved state must be *smoother* than a
    /// rebuild. This is the regression the rebuild-on-knob-move bug caused.
    ///
    /// The scenario is a knob drag: a keypress moves a shelf 0.05 of knob
    /// travel, which is 1.2 dB on a ±12 dB band and 1.5 dB on the ML-2's
    /// ±15 dB shelves. The tone sits *above* the shelf corner so the gain
    /// change is actually audible, and the drag is monotonic — it never jumps
    /// back down, because a large instantaneous step leaves stale state that
    /// is wrong for the new transfer function too, and that is not what a
    /// knob does.
    ///
    /// Rebuilding is the worse case for a large reason: it discards `z1`,
    /// which for a settled shelf is the filter's own steady state, so the
    /// output drops to `b0 * x` — losing the full accumulated gain.
    #[test]
    fn live_retune_is_smoother_than_rebuilding() {
        const SR: f32 = 48_000.0;
        /// Warm-up so both filters reach steady state before the first retune.
        const WARMUP: usize = 512;
        const STEP_SAMPLES: usize = 64;
        /// One keypress of shelf travel, in dB.
        const STEP_DB: f32 = 1.2;
        const STEPS: usize = 10; // 0 -> +12 dB

        // 4 kHz through a 1.8 kHz shelf: the shelf's gain applies to this tone.
        let tone = |i: usize| (2.0 * PI * 4000.0 * i as f32 / SR).sin() * 0.2;

        let peak_step = |preserved: bool| {
            let mut b = Biquad::high_shelf(SR, 1800.0, 0.0);
            let mut out: Vec<f32> = Vec::with_capacity(STEP_SAMPLES * STEPS + 1);
            for i in 0..WARMUP + STEP_SAMPLES * STEPS {
                let x = tone(i);
                let step = (i - WARMUP) / STEP_SAMPLES;
                if i >= WARMUP && (i - WARMUP).is_multiple_of(STEP_SAMPLES) {
                    let g = (step as f32) * STEP_DB;
                    if preserved {
                        b.set_high_shelf(SR, 1800.0, g);
                    } else {
                        b = Biquad::high_shelf(SR, 1800.0, g);
                    }
                }
                out.push(b.process(x));
            }
            out[WARMUP..]
                .windows(2)
                .map(|w| (w[1] - w[0]).abs())
                .fold(0.0f32, f32::max)
        };

        let smooth = peak_step(true);
        let rebuild = peak_step(false);
        assert!(
            smooth < rebuild,
            "preserved-state retune ({smooth:.5}) should have a smaller peak step \
             than rebuild-on-change ({rebuild:.5})"
        );
    }

    /// Sanity: the designs still produce the response the cookbook specifies.
    #[test]
    fn designs_hit_their_cookbook_targets() {
        let sr = 48_000.0;
        let db = |g: f32| 20.0 * g.abs().max(1e-9).log10();

        // High shelf at 1 kHz: +12 dB well above, 0 dB well below.
        let hs = Biquad::high_shelf(sr, 1000.0, 12.0);
        let at = |b: &Biquad, f: f32| db(b.gain_at(2.0 * PI * f / sr));
        assert!((at(&hs, 10_000.0) - 12.0).abs() < 0.5, "+12 shelf high end");
        assert!(at(&hs, 50.0).abs() < 0.5, "+12 shelf low end");

        // 0 dB shelf is exactly unity (this is what made the Vox presence
        // filter a dead filter - a mathematical no-op, not just a silent one).
        let flat = Biquad::high_shelf(sr, 4500.0, 0.0);
        for f in [50.0, 500.0, 4500.0, 12_000.0] {
            assert!(
                at(&flat, f) < 0.01,
                "0 dB shelf should be unity at {f} Hz, got {} dB",
                at(&flat, f)
            );
        }

        // Low shelf, mirrored.
        let ls = Biquad::low_shelf(sr, 200.0, -9.0);
        assert!((at(&ls, 30.0) + 9.0).abs() < 0.5, "-9 shelf low end");
        assert!(at(&ls, 8000.0).abs() < 0.5, "-9 shelf high end");

        // High-pass / low-pass corners.
        let hp = Biquad::highpass(sr, 1000.0, 0.707);
        assert!(at(&hp, 10_000.0).abs() < 0.2, "hp passband");
        assert!(at(&hp, 100.0) < -18.0, "hp stopband");
        let lp = Biquad::lowpass(sr, 3000.0, 0.707);
        assert!(at(&lp, 100.0).abs() < 0.2, "lp passband");
        assert!(at(&lp, 20_000.0) < -18.0, "lp stopband");

        // Peak EQ hits its gain at centre and unity far outside.
        let pk = Biquad::peak_eq(sr, 1000.0, 2.0, 6.0);
        assert!((at(&pk, 1000.0) - 6.0).abs() < 0.5, "peak centre");
        assert!(at(&pk, 20.0).abs() < 0.5, "peak low wing");
    }
}
