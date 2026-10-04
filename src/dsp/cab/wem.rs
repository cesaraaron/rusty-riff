use super::ir::{self, Texture};
use super::{BlendedCab, CabLayout, Cabinet};
use crate::dsp::biquad::Biquad;

/// WEM 4×12 loaded with Fane Crescendo speakers, multi-mic'd.
///
/// Same three-capture blend as the other cabs (close SM57 dynamic, close R121
/// ribbon, room mic — see [`BlendedCab`]). The WEM 4×12 is the tall cabinet
/// Gilmour played through his Hiwatt DR103, and the Fane Crescendo is its voice:
/// **bright, efficient and aggressive in the upper-mids**, with a leaner, tighter
/// low end than a Greenback or V30 and a clear, present top that stays open well
/// past where the Celestions roll off. Compared to the Mesa (scooped) it is far
/// less scooped and much brighter; compared to the Marshall Greenback it is
/// leaner in the body and more forward in the 2.5–4 kHz bark.
///
/// Fane-in-WEM close-mic (SM57) signature (the skeleton):
/// Voiced against measured commercial 4×12 captures (see the note in the Mesa
/// `voicing_sm57`):
///   • Resonant sub HP at 76 Hz (tight, controlled low end)
///   • +1.5 dB low shelf at 120 Hz + a +2.5 dB resonant hump at 112 Hz (cab
///     depth — deliberately leaner than the closed-back Orange's chest thump;
///     Phase 7 cut these from +4/+5 dB, which stacked with the amp body lift
///     into ~+10 dB of mud that no bass knob could remove)
///   • +2 dB wide mound at 210 Hz and +2.5 dB at 500 Hz (low-mid body plateau,
///     leaner than the Greenback's)
///   • −4.5 dB at 1400 Hz (the mid pocket — shallower than the V30's, so the
///     mids stay present)
///   • +5 dB at 3000 Hz (the Fane's signature bright, aggressive upper-mid bark,
///     sitting higher than the Greenback's crunch peak)
///   • +4.5 dB at 5000 Hz (the open, extended top a Fane carries before rolloff)
///   • −15 dB high shelf at 6800 Hz (brighter rolloff than the Celestions')
///   • 4th-order LP at 7500 Hz (fizz cut, but higher — Fanes stay open up top)
pub struct WemCab {
    inner: BlendedCab,
}

// Close-mic texture, shared by both channels (references are mono — see the
// Mesa texture note; only the scatter seeds differ per channel, the room pair
// below carries the true stereo decorrelation). The WEM is a big, tall box, so
// the early reflections are dense and a touch later than the smaller cabs',
// timed so the comb notches land in the mid pocket. The two low modes near
// 100–130 Hz add the cab's thump ring where the direct sound is strong, T60s
// kept short of the note (gated references — see the Mesa note); the ~3 kHz mode
// is the Fane's bright cone breakup.
// Dense 6–21 ms tail (see the Mesa TEX_REFL note): real captures are diffuse
// there, not a few discrete echoes.
// Early-tap gains kept small (~0.1) — see the Mesa TEX_REFL note: hot early
// taps are a voicing move, not texture.
const TEX_REFL: &[(f32, f32)] = &[
    (0.32, -0.14),
    (0.66, 0.10),
    (1.30, -0.09),
    (3.50, 0.082),
    (7.00, -0.070),
    (9.40, 0.064),
    (11.80, -0.058),
    (13.60, 0.052),
    (15.40, -0.047),
    (17.20, 0.040),
    (18.80, -0.035),
    (20.20, 0.030),
];
// Breakup mode at texture scale — the Fane breaks up a touch higher than the
// Celestions (3000 Hz vs 2500/3300), part of its brighter character. See the
// Mesa TEX_MODES note.
const TEX_MODES: &[(f32, f32, f32)] = &[
    (105.0, 55.0, 0.004),
    (128.0, 50.0, 0.004),
    (3000.0, 4.5, 0.045),
];
// Fane breakup scatter (a bright, jagged top) plus the mid reflection-ripple
// cluster (see the Mesa scatter note).
const SCATTER_L: &[ir::Scatter] = &[
    ir::Scatter {
        seed: 41,
        count: 22,
        band: (2200.0, 7000.0),
        t60_ms: (5.0, 12.0),
        gain: 0.022,
    },
    ir::Scatter {
        seed: 43,
        count: 24,
        band: (600.0, 2400.0),
        t60_ms: (6.0, 16.0),
        gain: 0.020,
    },
];
const SCATTER_R: &[ir::Scatter] = &[
    ir::Scatter {
        seed: 42,
        count: 22,
        band: (2200.0, 7000.0),
        t60_ms: (5.0, 12.0),
        gain: 0.022,
    },
    ir::Scatter {
        seed: 44,
        count: 24,
        band: (600.0, 2400.0),
        t60_ms: (6.0, 16.0),
        gain: 0.020,
    },
];
const TEX_L: Texture = Texture {
    predelay: 0,
    reflections: TEX_REFL,
    modes: TEX_MODES,
    scatter: SCATTER_L,
};
const TEX_R: Texture = Texture {
    predelay: 0,
    reflections: TEX_REFL,
    modes: TEX_MODES,
    scatter: SCATTER_R,
};

// Room-mic textures: distance pre-delay + denser late reflections for air.
const ROOM_TEX_L: Texture = Texture {
    predelay: 118,
    reflections: &[
        (2.70, 0.20),
        (5.70, -0.16),
        (9.60, 0.13),
        (13.20, -0.10),
        (15.80, 0.076),
        (18.10, -0.056),
    ],
    modes: &[(78.0, 65.0, 0.005), (172.0, 55.0, 0.004)],
    scatter: &[],
};
const ROOM_TEX_R: Texture = Texture {
    predelay: 145,
    reflections: &[
        (3.10, 0.18),
        (6.40, -0.15),
        (10.80, 0.12),
        (13.70, -0.093),
        (15.90, 0.070),
        (17.80, -0.051),
    ],
    modes: &[(82.0, 65.0, 0.005), (182.0, 55.0, 0.004)],
    scatter: &[],
};

impl WemCab {
    /// Zero all state (see [`BlendedCab::clear`]). Called by `CabBank`
    /// when this cab is selected.
    pub fn clear(&mut self) {
        self.inner.clear();
    }

    pub fn new(sr: f32) -> Self {
        let len = ir::ir_len(sr);
        let synth = |v: &mut dyn FnMut(f32) -> f32, t: &Texture| ir::synth(sr, len, v, t);
        let irs = [
            synth(&mut Self::voicing_sm57(sr), &TEX_L),
            synth(&mut Self::voicing_sm57(sr), &TEX_R),
            synth(&mut Self::voicing_ribbon(sr), &TEX_L),
            synth(&mut Self::voicing_ribbon(sr), &TEX_R),
            synth(&mut Self::voicing_room(sr), &ROOM_TEX_L),
            synth(&mut Self::voicing_room(sr), &ROOM_TEX_R),
        ];
        let mut inner = BlendedCab::new(sr, irs, CabLayout::FourByTwelve);
        // Per-cab level trim (see `BlendedCab::set_level`).
        inner.set_level(0.955);
        Self { inner }
    }

    /// SM57 close-mic: the bright, aggressive Fane voicing (the original skeleton).
    fn voicing_sm57(sr: f32) -> impl FnMut(f32) -> f32 {
        let mut bands = [
            // Resonant rise into a ~112 Hz hump, then a broad 120–600 Hz body
            // plateau over a shallow mid pocket (see the Mesa `voicing_sm57`
            // note). Lean by design (Phase 7): a Fane is tighter and brighter
            // than a Greenback, and the old +4/+5 dB lows stacked with the amp
            // body lift into mud no bass knob could cut (all downstream of it).
            Biquad::highpass(sr, 76.0, 1.2),
            Biquad::low_shelf(sr, 120.0, 1.5),
            Biquad::peak_eq(sr, 112.0, 1.1, 2.5),
            Biquad::peak_eq(sr, 210.0, 0.7, 2.0),
            Biquad::peak_eq(sr, 500.0, 0.65, 2.5),
            // Shallow pocket at 1.4 kHz: less scooped than the V30, so the mids
            // stay present under the Fane's bright top.
            Biquad::peak_eq(sr, 1400.0, 1.2, -4.5),
            // Fane bark: a strong 3 kHz peak — the cab's signature aggressive
            // upper-mid cut, sitting higher than the Greenback's crunch peak.
            Biquad::peak_eq(sr, 3000.0, 1.0, 5.0),
            // Extended top: a Fane stays open through 5 kHz before the rolloff.
            Biquad::peak_eq(sr, 5000.0, 1.2, 4.5),
            // Brighter top than the Celestions — a gentler shelf, higher corner.
            Biquad::high_shelf(sr, 6800.0, -15.0),
            Biquad::lowpass(sr, 7500.0, 0.707),
            Biquad::lowpass(sr, 7500.0, 0.707),
        ];
        move |x| bands.iter_mut().fold(x, |acc, b| b.process(acc))
    }

    /// R121 ribbon close-mic: warmer low-mids, softer presence, silky top.
    fn voicing_ribbon(sr: f32) -> impl FnMut(f32) -> f32 {
        let mut bands = [
            Biquad::highpass(sr, 74.0, 1.2),
            Biquad::low_shelf(sr, 150.0, 1.0),
            Biquad::peak_eq(sr, 105.0, 1.1, 2.5), // low resonant hump (cab depth)
            Biquad::peak_eq(sr, 205.0, 0.7, 2.5), // broad low-mid body mound
            Biquad::peak_eq(sr, 520.0, 0.9, 1.5),
            Biquad::peak_eq(sr, 2400.0, 1.5, 2.5), // softer, lower presence
            Biquad::high_shelf(sr, 4300.0, -13.0), // ribbon HF rolloff
            Biquad::lowpass(sr, 6200.0, 0.707),
        ];
        move |x| bands.iter_mut().fold(x, |acc, b| b.process(acc))
    }

    /// Room mic: a darker, distance-coloured version of the cab voicing.
    fn voicing_room(sr: f32) -> impl FnMut(f32) -> f32 {
        let mut bands = [
            Biquad::highpass(sr, 76.0, 0.8),
            Biquad::low_shelf(sr, 150.0, 1.2),
            Biquad::peak_eq(sr, 360.0, 1.2, -1.8),
            Biquad::peak_eq(sr, 900.0, 1.0, 2.0),
            Biquad::high_shelf(sr, 3900.0, -9.5),
            Biquad::lowpass(sr, 5400.0, 0.707),
        ];
        move |x| bands.iter_mut().fold(x, |acc, b| b.process(acc))
    }
}

impl Cabinet for WemCab {
    #[inline]
    fn process(&mut self, sample: f32, mic_pos: f32, blend: f32, room: f32) -> (f32, f32) {
        self.inner.process(sample, mic_pos, blend, room)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ir::analysis;

    #[test]
    fn textures_are_plausible_and_symmetric() {
        let sr = 48_000.0;
        analysis::assert_plausible("wem close L", sr, &TEX_L);
        analysis::assert_plausible("wem close R", sr, &TEX_R);
        analysis::assert_plausible("wem room L", sr, &ROOM_TEX_L);
        analysis::assert_plausible("wem room R", sr, &ROOM_TEX_R);
        analysis::assert_lr_symmetry("wem close", &TEX_L, &TEX_R);
        analysis::assert_lr_symmetry("wem room", &ROOM_TEX_L, &ROOM_TEX_R);
    }

    #[test]
    fn modes_are_realized_in_the_rendered_ir() {
        let sr = 48_000.0;
        let len = ir::ir_len(sr);
        let strip = |t: &Texture| Texture {
            predelay: t.predelay,
            reflections: t.reflections,
            modes: &[],
            scatter: t.scatter,
        };
        macro_rules! check {
            ($tag:expr, $voicing:expr, $tex:expr) => {{
                let full = ir::synth(sr, len, &mut $voicing(sr), $tex);
                let bare = ir::synth(sr, len, &mut $voicing(sr), &strip($tex));
                analysis::assert_modes_realized($tag, &full, &bare, sr, $tex);
            }};
        }
        check!("wem close L", WemCab::voicing_sm57, &TEX_L);
        check!("wem close R", WemCab::voicing_sm57, &TEX_R);
        check!("wem room L", WemCab::voicing_room, &ROOM_TEX_L);
        check!("wem room R", WemCab::voicing_room, &ROOM_TEX_R);
    }
}
