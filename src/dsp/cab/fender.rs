use super::ir::{self, Texture};
use super::{BlendedCab, CabLayout, Cabinet};
use crate::dsp::biquad::Biquad;

/// Fender 2×12 with Jensen-style ceramic speakers, multi-mic'd.
///
/// Three mic captures are synthesised and blended (see [`BlendedCab`]): the close
/// SM57 dynamic, a close R121 ribbon, and an ambient room mic. Like the Vox this
/// is an **open-back 2×12** (see [`CabLayout::TwoByTwelve`]) — small low
/// resonance, an open top, and only the stacked partner cone arriving late — the
/// clean, sparkly voice under blackface Fender combos.
///
/// Jensen close-mic (SM57) signature:
///   • Resonant sub HP at 82 Hz (keeps the low-E fundamental, cuts the rumble)
///   • +2 dB low shelf at 120 Hz + a +4 dB resonant hump at 100 Hz (a firm but
///     modest cab depth — open back)
///   • −3 dB dip at 800 Hz (the blackface mid scoop)
///   • +2.5 dB at 2.5 kHz and +3 dB at 4 kHz (the sparkle/presence)
///   • −12 dB high shelf at 7 kHz, 4th-order LP at 8 kHz (extends a touch further
///     than the Alnico's soft rolloff)
pub struct FenderCab {
    inner: BlendedCab,
}

// Close-mic texture, shared by both channels (only the scatter seeds differ per
// channel; the room pair carries the stereo width). Open-back 2×12: short,
// diffuse tail.
const TEX_REFL: &[(f32, f32)] = &[
    (0.26, -0.15),
    (0.56, 0.11),
    (1.12, -0.085),
    (3.10, 0.075),
    (6.60, -0.065),
    (9.00, 0.058),
    (11.80, -0.053),
    (13.20, 0.047),
    (14.60, -0.042),
    (16.40, 0.035),
    (18.00, -0.030),
    (19.60, 0.026),
];
const TEX_MODES: &[(f32, f32, f32)] = &[
    (90.0, 50.0, 0.004),
    (112.0, 45.0, 0.004),
    (3000.0, 5.0, 0.042),
];
const SCATTER_L: &[ir::Scatter] = &[
    ir::Scatter {
        seed: 41,
        count: 22,
        band: (2200.0, 7200.0),
        t60_ms: (4.5, 11.0),
        gain: 0.024,
    },
    ir::Scatter {
        seed: 43,
        count: 24,
        band: (550.0, 2300.0),
        t60_ms: (6.0, 15.0),
        gain: 0.022,
    },
];
const SCATTER_R: &[ir::Scatter] = &[
    ir::Scatter {
        seed: 42,
        count: 22,
        band: (2200.0, 7200.0),
        t60_ms: (4.5, 11.0),
        gain: 0.024,
    },
    ir::Scatter {
        seed: 44,
        count: 24,
        band: (550.0, 2300.0),
        t60_ms: (6.0, 15.0),
        gain: 0.022,
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
    predelay: 115,
    reflections: &[
        (2.70, 0.20),
        (5.80, -0.16),
        (9.60, 0.13),
        (12.80, -0.10),
        (15.20, 0.075),
        (17.60, -0.055),
    ],
    modes: &[(74.0, 60.0, 0.005), (176.0, 50.0, 0.004)],
    scatter: &[],
};
const ROOM_TEX_R: Texture = Texture {
    predelay: 148,
    reflections: &[
        (3.10, 0.18),
        (6.50, -0.15),
        (10.80, 0.12),
        (13.30, -0.095),
        (15.20, 0.07),
        (17.20, -0.05),
    ],
    modes: &[(78.0, 60.0, 0.005), (186.0, 50.0, 0.004)],
    scatter: &[],
};

impl FenderCab {
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
        let mut inner = BlendedCab::new(sr, irs, CabLayout::TwoByTwelve);
        // Per-cab level trim (see `BlendedCab::set_level`).
        inner.set_level(1.549);
        Self { inner }
    }

    /// SM57 close-mic: the clean, sparkly Jensen voicing.
    fn voicing_sm57(sr: f32) -> impl FnMut(f32) -> f32 {
        let mut bands = [
            Biquad::highpass(sr, 82.0, 1.1),
            Biquad::low_shelf(sr, 120.0, 2.0),
            Biquad::peak_eq(sr, 100.0, 1.1, 4.0),
            Biquad::peak_eq(sr, 260.0, 0.8, 1.5),
            Biquad::peak_eq(sr, 800.0, 1.0, -3.0),
            Biquad::peak_eq(sr, 2500.0, 1.0, 2.5),
            Biquad::peak_eq(sr, 4000.0, 1.1, 3.0),
            Biquad::high_shelf(sr, 7000.0, -12.0),
            Biquad::lowpass(sr, 8000.0, 0.707),
            Biquad::lowpass(sr, 8000.0, 0.707),
        ];
        move |x| bands.iter_mut().fold(x, |acc, b| b.process(acc))
    }

    /// R121 ribbon close-mic: warmer, softer presence, silky top.
    fn voicing_ribbon(sr: f32) -> impl FnMut(f32) -> f32 {
        let mut bands = [
            Biquad::highpass(sr, 80.0, 1.1),
            Biquad::low_shelf(sr, 150.0, 1.5),
            Biquad::peak_eq(sr, 98.0, 1.1, 3.5),
            Biquad::peak_eq(sr, 300.0, 0.8, 1.5),
            Biquad::peak_eq(sr, 1600.0, 1.3, 1.5),
            Biquad::high_shelf(sr, 4500.0, -14.0),
            Biquad::lowpass(sr, 6500.0, 0.707),
        ];
        move |x| bands.iter_mut().fold(x, |acc, b| b.process(acc))
    }

    /// Room mic: darker, distance-coloured Jensen.
    fn voicing_room(sr: f32) -> impl FnMut(f32) -> f32 {
        let mut bands = [
            Biquad::highpass(sr, 78.0, 0.8),
            Biquad::low_shelf(sr, 150.0, 1.8),
            Biquad::peak_eq(sr, 350.0, 1.2, -1.5),
            Biquad::peak_eq(sr, 1000.0, 1.0, 1.5),
            Biquad::high_shelf(sr, 4000.0, -8.0),
            Biquad::lowpass(sr, 5600.0, 0.707),
        ];
        move |x| bands.iter_mut().fold(x, |acc, b| b.process(acc))
    }
}

impl Cabinet for FenderCab {
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
        analysis::assert_plausible("fender close L", sr, &TEX_L);
        analysis::assert_plausible("fender close R", sr, &TEX_R);
        analysis::assert_plausible("fender room L", sr, &ROOM_TEX_L);
        analysis::assert_plausible("fender room R", sr, &ROOM_TEX_R);
        analysis::assert_lr_symmetry("fender close", &TEX_L, &TEX_R);
        analysis::assert_lr_symmetry("fender room", &ROOM_TEX_L, &ROOM_TEX_R);
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
        check!("fender close L", FenderCab::voicing_sm57, &TEX_L);
        check!("fender close R", FenderCab::voicing_sm57, &TEX_R);
        check!("fender room L", FenderCab::voicing_room, &ROOM_TEX_L);
        check!("fender room R", FenderCab::voicing_room, &ROOM_TEX_R);
    }
}
