use super::ir::{self, Texture};
use super::{BlendedCab, CabLayout, Cabinet};
use crate::dsp::biquad::Biquad;

/// Supro 1×10 small combo with a small ceramic speaker, multi-mic'd.
///
/// Three mic captures are synthesised and blended (see [`BlendedCab`]): the close
/// SM57 dynamic, a close R121 ribbon, and an ambient room mic. This is a
/// **single-speaker open-back combo** (see [`CabLayout::Single`]) — there is no
/// neighbour cone, so the close mic hears only its own cone, and the boxy,
/// top-limited small-speaker voice is carried entirely by the voicing and texture.
///
/// **Model, not a capture.** The existing cabs' voicings are checked against
/// measured captures; this one has no reference capture, so it is a
/// plausible small-combo approximation, not a measured SM57/R121 setup.
///
/// Small-cone close-mic (SM57) signature:
///   • HP at 105 Hz — a 10" cone can't make the deep low end a 12" can, so the
///     sub is trimmed rather than extended.
///   • +1.5 dB low shelf at 130 Hz and a +3.5 dB boxy hump at 250 Hz — the
///     boxed, mid-forward small-combo body.
///   • −2.5 dB dip at 750 Hz (the combo's mid scoop)
///   • +3.5 dB at 2.5 kHz and +2 dB at 3.5 kHz (the small cone's presence)
///   • −11 dB high shelf at 5 kHz, 4th-order LP at 5.2 kHz — the early top
///     rolloff of a small cone, darker than any 12".
pub struct SuproCab {
    inner: BlendedCab,
}

// Close-mic texture, shared by both channels (only the scatter seeds differ per
// channel; the room pair carries the stereo width). A small open-back combo: a
// short, diffuse tail.
const TEX_REFL: &[(f32, f32)] = &[
    (0.22, -0.15),
    (0.47, 0.11),
    (0.95, -0.085),
    (2.60, 0.075),
    (5.80, -0.065),
    (8.20, 0.058),
    (10.60, -0.052),
    (12.00, 0.046),
    (13.40, -0.041),
    (15.20, 0.034),
    (16.80, -0.029),
    (18.40, 0.025),
];
// A small box resonates higher than a big one; the cone breakup sits lower than a
// 12"'s, matching the 10" driver's earlier top rolloff.
const TEX_MODES: &[(f32, f32, f32)] = &[
    (92.0, 50.0, 0.004),
    (116.0, 45.0, 0.004),
    (2800.0, 5.0, 0.040),
];
const SCATTER_L: &[ir::Scatter] = &[
    ir::Scatter {
        seed: 51,
        count: 20,
        band: (2400.0, 6000.0),
        t60_ms: (4.0, 9.5),
        gain: 0.022,
    },
    ir::Scatter {
        seed: 53,
        count: 22,
        band: (600.0, 2400.0),
        t60_ms: (5.5, 14.0),
        gain: 0.021,
    },
];
const SCATTER_R: &[ir::Scatter] = &[
    ir::Scatter {
        seed: 52,
        count: 20,
        band: (2400.0, 6000.0),
        t60_ms: (4.0, 9.5),
        gain: 0.022,
    },
    ir::Scatter {
        seed: 54,
        count: 22,
        band: (600.0, 2400.0),
        t60_ms: (5.5, 14.0),
        gain: 0.021,
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

const ROOM_TEX_L: Texture = Texture {
    predelay: 96,
    reflections: &[
        (2.40, 0.20),
        (5.20, -0.16),
        (8.60, 0.13),
        (11.40, -0.10),
        (13.80, 0.075),
        (16.00, -0.055),
    ],
    modes: &[(72.0, 60.0, 0.005), (170.0, 50.0, 0.004)],
    scatter: &[],
};
const ROOM_TEX_R: Texture = Texture {
    predelay: 128,
    reflections: &[
        (2.80, 0.18),
        (5.90, -0.15),
        (9.60, 0.12),
        (11.90, -0.095),
        (13.80, 0.07),
        (15.60, -0.05),
    ],
    modes: &[(76.0, 60.0, 0.005), (178.0, 50.0, 0.004)],
    scatter: &[],
};

impl SuproCab {
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
        let mut inner = BlendedCab::new(sr, irs, CabLayout::Single);
        // Per-cab level trim (see `BlendedCab::set_level`).
        inner.set_level(1.216);
        Self { inner }
    }

    /// SM57 close-mic: the boxy, mid-forward small-cone voicing.
    fn voicing_sm57(sr: f32) -> impl FnMut(f32) -> f32 {
        let mut bands = [
            Biquad::highpass(sr, 105.0, 1.1),
            Biquad::low_shelf(sr, 130.0, 1.5),
            Biquad::peak_eq(sr, 250.0, 0.9, 3.5),
            Biquad::peak_eq(sr, 750.0, 1.2, -2.5),
            Biquad::peak_eq(sr, 1300.0, 1.5, 5.0),
            Biquad::peak_eq(sr, 2500.0, 1.0, 3.5),
            Biquad::peak_eq(sr, 3500.0, 1.1, 2.0),
            Biquad::high_shelf(sr, 5000.0, -11.0),
            Biquad::lowpass(sr, 5200.0, 0.707),
            Biquad::lowpass(sr, 5200.0, 0.707),
        ];
        move |x| bands.iter_mut().fold(x, |acc, b| b.process(acc))
    }

    /// R121 ribbon close-mic: warmer, softer presence, silky top.
    fn voicing_ribbon(sr: f32) -> impl FnMut(f32) -> f32 {
        let mut bands = [
            Biquad::highpass(sr, 100.0, 1.1),
            Biquad::low_shelf(sr, 150.0, 1.2),
            Biquad::peak_eq(sr, 240.0, 0.9, 3.0),
            Biquad::peak_eq(sr, 1200.0, 1.3, 1.5),
            Biquad::high_shelf(sr, 4200.0, -13.0),
            Biquad::lowpass(sr, 4600.0, 0.707),
        ];
        move |x| bands.iter_mut().fold(x, |acc, b| b.process(acc))
    }

    /// Room mic: darker, distance-coloured small combo.
    fn voicing_room(sr: f32) -> impl FnMut(f32) -> f32 {
        let mut bands = [
            Biquad::highpass(sr, 95.0, 0.8),
            Biquad::low_shelf(sr, 150.0, 1.5),
            Biquad::peak_eq(sr, 350.0, 1.2, -1.5),
            Biquad::peak_eq(sr, 1000.0, 1.0, 1.5),
            Biquad::high_shelf(sr, 3600.0, -8.0),
            Biquad::lowpass(sr, 4200.0, 0.707),
        ];
        move |x| bands.iter_mut().fold(x, |acc, b| b.process(acc))
    }
}

impl Cabinet for SuproCab {
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
        analysis::assert_plausible("supro close L", sr, &TEX_L);
        analysis::assert_plausible("supro close R", sr, &TEX_R);
        analysis::assert_plausible("supro room L", sr, &ROOM_TEX_L);
        analysis::assert_plausible("supro room R", sr, &ROOM_TEX_R);
        analysis::assert_lr_symmetry("supro close", &TEX_L, &TEX_R);
        analysis::assert_lr_symmetry("supro room", &ROOM_TEX_L, &ROOM_TEX_R);
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
        check!("supro close L", SuproCab::voicing_sm57, &TEX_L);
        check!("supro close R", SuproCab::voicing_sm57, &TEX_R);
        check!("supro room L", SuproCab::voicing_room, &ROOM_TEX_L);
        check!("supro room R", SuproCab::voicing_room, &ROOM_TEX_R);
    }
}
