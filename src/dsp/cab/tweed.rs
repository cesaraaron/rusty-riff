use super::ir::{self, Texture};
use super::{BlendedCab, CabLayout, Cabinet};
use crate::dsp::biquad::Biquad;

/// Tweed 1×12 open-back combo with a Jensen-style ceramic speaker, multi-mic'd.
///
/// Three mic captures are synthesised and blended (see [`BlendedCab`]): the close
/// SM57 dynamic, a close R121 ribbon, and an ambient room mic. A **single-speaker
/// open-back combo** (see [`CabLayout::Single`]) — no neighbour cone.
///
/// **Model, not a capture.** The existing cabs' voicings are checked against
/// measured captures; this one has no reference capture, so it is a
/// plausible small-combo approximation.
///
/// 12" cone close-mic (SM57) signature:
///   • HP at 85 Hz — a 12" reaches lower than the Supro 1×10, so the sub is
///     trimmed less (the tweed's loose low end lives here).
///   • +2 dB low shelf at 120 Hz and a +3 dB boxy hump at 200 Hz.
///   • −2.5 dB dip at 800 Hz (the tweed mid scoop)
///   • +3 dB at 2.5 kHz and +2.5 dB at 3.5 kHz (the small cone's presence)
///   • −11 dB high shelf at 6 kHz, 4th-order LP at 6.2 kHz.
pub struct TweedCab {
    inner: BlendedCab,
}

const TEX_REFL: &[(f32, f32)] = &[
    (0.25, -0.15),
    (0.53, 0.11),
    (1.05, -0.085),
    (2.90, 0.075),
    (6.40, -0.065),
    (8.80, 0.058),
    (11.50, -0.052),
    (12.90, 0.046),
    (14.30, -0.041),
    (16.10, 0.034),
    (17.70, -0.029),
    (19.30, 0.025),
];
const TEX_MODES: &[(f32, f32, f32)] = &[
    (108.0, 50.0, 0.004),
    (196.0, 44.0, 0.004),
    (3200.0, 5.0, 0.040),
];
const SCATTER_L: &[ir::Scatter] = &[
    ir::Scatter {
        seed: 61,
        count: 21,
        band: (2500.0, 6500.0),
        t60_ms: (4.5, 10.0),
        gain: 0.023,
    },
    ir::Scatter {
        seed: 63,
        count: 22,
        band: (600.0, 2400.0),
        t60_ms: (6.0, 14.0),
        gain: 0.021,
    },
];
const SCATTER_R: &[ir::Scatter] = &[
    ir::Scatter {
        seed: 62,
        count: 21,
        band: (2500.0, 6500.0),
        t60_ms: (4.5, 10.0),
        gain: 0.023,
    },
    ir::Scatter {
        seed: 64,
        count: 22,
        band: (600.0, 2400.0),
        t60_ms: (6.0, 14.0),
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
    predelay: 104,
    reflections: &[
        (2.50, 0.20),
        (5.40, -0.16),
        (8.90, 0.13),
        (11.80, -0.10),
        (14.20, 0.075),
        (16.40, -0.055),
    ],
    modes: &[(74.0, 60.0, 0.005), (172.0, 50.0, 0.004)],
    scatter: &[],
};
const ROOM_TEX_R: Texture = Texture {
    predelay: 136,
    reflections: &[
        (2.90, 0.18),
        (6.10, -0.15),
        (9.90, 0.12),
        (12.30, -0.095),
        (14.20, 0.07),
        (16.00, -0.05),
    ],
    modes: &[(78.0, 60.0, 0.005), (180.0, 50.0, 0.004)],
    scatter: &[],
};

impl TweedCab {
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
        inner.set_level(1.82);
        Self { inner }
    }

    /// SM57 close-mic: the warm, mid-forward 12" tweed voicing.
    fn voicing_sm57(sr: f32) -> impl FnMut(f32) -> f32 {
        let mut bands = [
            Biquad::highpass(sr, 85.0, 1.1),
            Biquad::low_shelf(sr, 120.0, 2.0),
            Biquad::peak_eq(sr, 200.0, 0.9, 3.0),
            Biquad::peak_eq(sr, 800.0, 1.2, -2.5),
            Biquad::peak_eq(sr, 2500.0, 1.0, 3.0),
            Biquad::peak_eq(sr, 3500.0, 1.1, 2.5),
            Biquad::high_shelf(sr, 6000.0, -11.0),
            Biquad::lowpass(sr, 6200.0, 0.707),
            Biquad::lowpass(sr, 6200.0, 0.707),
        ];
        move |x| bands.iter_mut().fold(x, |acc, b| b.process(acc))
    }

    /// R121 ribbon close-mic: warmer, softer presence.
    fn voicing_ribbon(sr: f32) -> impl FnMut(f32) -> f32 {
        let mut bands = [
            Biquad::highpass(sr, 82.0, 1.1),
            Biquad::low_shelf(sr, 140.0, 1.5),
            Biquad::peak_eq(sr, 195.0, 0.9, 2.8),
            Biquad::peak_eq(sr, 1200.0, 1.3, 1.5),
            Biquad::high_shelf(sr, 4800.0, -13.0),
            Biquad::lowpass(sr, 5200.0, 0.707),
        ];
        move |x| bands.iter_mut().fold(x, |acc, b| b.process(acc))
    }

    /// Room mic: darker, distance-coloured 12".
    fn voicing_room(sr: f32) -> impl FnMut(f32) -> f32 {
        let mut bands = [
            Biquad::highpass(sr, 78.0, 0.8),
            Biquad::low_shelf(sr, 150.0, 1.6),
            Biquad::peak_eq(sr, 350.0, 1.2, -1.5),
            Biquad::peak_eq(sr, 1000.0, 1.0, 1.5),
            Biquad::high_shelf(sr, 4000.0, -8.0),
            Biquad::lowpass(sr, 4800.0, 0.707),
        ];
        move |x| bands.iter_mut().fold(x, |acc, b| b.process(acc))
    }
}

impl Cabinet for TweedCab {
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
        analysis::assert_plausible("tweed close L", sr, &TEX_L);
        analysis::assert_plausible("tweed close R", sr, &TEX_R);
        analysis::assert_plausible("tweed room L", sr, &ROOM_TEX_L);
        analysis::assert_plausible("tweed room R", sr, &ROOM_TEX_R);
        analysis::assert_lr_symmetry("tweed close", &TEX_L, &TEX_R);
        analysis::assert_lr_symmetry("tweed room", &ROOM_TEX_L, &ROOM_TEX_R);
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
        check!("tweed close L", TweedCab::voicing_sm57, &TEX_L);
        check!("tweed close R", TweedCab::voicing_sm57, &TEX_R);
        check!("tweed room L", TweedCab::voicing_room, &ROOM_TEX_L);
        check!("tweed room R", TweedCab::voicing_room, &ROOM_TEX_R);
    }
}
