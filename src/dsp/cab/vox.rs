use super::ir::{self, Texture};
use super::{BlendedCab, CabLayout, Cabinet};
use crate::dsp::biquad::Biquad;

/// Vox 2×12 with Celestion Alnico Blue speakers, multi-mic'd.
///
/// Two mic captures are synthesised and blended (see [`BlendedCab`]): the close
/// SM57 dynamic, a close R121 ribbon, and an ambient room mic. Unlike the
/// closed 4×12s this is an **open-back 2×12** (see [`CabLayout::TwoByTwelve`]):
/// the close mic hears only its stacked partner, the low resonance is smaller,
/// and the top stays open — the chime the AC30 is famous for.
///
/// Alnico Blue close-mic (SM57) signature:
///   • Resonant sub HP at 80 Hz (keeps the low-E fundamental, cuts the rumble)
///   • +2.5 dB low shelf at 120 Hz + a +5 dB resonant hump at 105 Hz (cab depth)
///   • +2 dB mound at 250 Hz (a leaner body than a 4×12 — open back)
///   • −3.5 dB dip at 900 Hz (the mid scoop)
///   • +3 dB at 2 kHz and +3.5 dB at 3.2 kHz (the Alnico's vocal upper-mid chime)
///   • −10 dB high shelf at 6 kHz, 4th-order LP at 7.2 kHz (soft cone rolloff)
pub struct VoxCab {
    inner: BlendedCab,
}

// Close-mic texture, shared by both channels (references are mono — only the
// scatter seeds differ per channel; the room pair carries the stereo width).
// Open-back 2×12: a slightly shorter, more diffuse tail than the closed 4×12s.
const TEX_REFL: &[(f32, f32)] = &[
    (0.28, -0.15),
    (0.58, 0.11),
    (1.18, -0.085),
    (3.20, 0.075),
    (6.80, -0.065),
    (9.20, 0.060),
    (12.00, -0.055),
    (13.40, 0.048),
    (14.80, -0.043),
    (16.60, 0.036),
    (18.20, -0.031),
    (19.80, 0.027),
];
const TEX_MODES: &[(f32, f32, f32)] = &[
    (96.0, 50.0, 0.004),
    (118.0, 45.0, 0.004),
    (2800.0, 5.0, 0.040),
];
const SCATTER_L: &[ir::Scatter] = &[
    ir::Scatter {
        seed: 31,
        count: 22,
        band: (2000.0, 6800.0),
        t60_ms: (4.5, 11.0),
        gain: 0.024,
    },
    ir::Scatter {
        seed: 33,
        count: 24,
        band: (550.0, 2300.0),
        t60_ms: (6.0, 15.0),
        gain: 0.022,
    },
];
const SCATTER_R: &[ir::Scatter] = &[
    ir::Scatter {
        seed: 32,
        count: 22,
        band: (2000.0, 6800.0),
        t60_ms: (4.5, 11.0),
        gain: 0.024,
    },
    ir::Scatter {
        seed: 34,
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
    predelay: 110,
    reflections: &[
        (2.60, 0.20),
        (5.60, -0.16),
        (9.40, 0.13),
        (12.60, -0.10),
        (15.00, 0.075),
        (17.40, -0.055),
    ],
    modes: &[(78.0, 60.0, 0.005), (180.0, 50.0, 0.004)],
    scatter: &[],
};
const ROOM_TEX_R: Texture = Texture {
    predelay: 145,
    reflections: &[
        (3.00, 0.18),
        (6.40, -0.15),
        (10.60, 0.12),
        (13.10, -0.095),
        (15.00, 0.07),
        (17.00, -0.05),
    ],
    modes: &[(82.0, 60.0, 0.005), (190.0, 50.0, 0.004)],
    scatter: &[],
};

impl VoxCab {
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
        let mut inner = BlendedCab::new(sr, irs, CabLayout::TwoByTwelve);
        // Per-cab level trim (see `BlendedCab::set_level`).
        inner.set_level(1.679);
        Self { inner }
    }

    /// SM57 close-mic: the bright, chimy Alnico Blue voicing.
    fn voicing_sm57(sr: f32) -> impl FnMut(f32) -> f32 {
        let mut bands = [
            Biquad::highpass(sr, 80.0, 1.1),
            Biquad::low_shelf(sr, 120.0, 2.5),
            Biquad::peak_eq(sr, 105.0, 1.1, 5.0),
            Biquad::peak_eq(sr, 250.0, 0.8, 2.0),
            // Open-back 2×12: a shallower pocket than a closed 4×12.
            Biquad::peak_eq(sr, 900.0, 1.2, -3.5),
            Biquad::peak_eq(sr, 2000.0, 1.0, 3.0),
            Biquad::peak_eq(sr, 3200.0, 1.1, 3.5),
            Biquad::high_shelf(sr, 6000.0, -10.0),
            Biquad::lowpass(sr, 7200.0, 0.707),
            Biquad::lowpass(sr, 7200.0, 0.707),
        ];
        move |x| bands.iter_mut().fold(x, |acc, b| b.process(acc))
    }

    /// R121 ribbon close-mic: warmer, softer presence, silky top.
    fn voicing_ribbon(sr: f32) -> impl FnMut(f32) -> f32 {
        let mut bands = [
            Biquad::highpass(sr, 78.0, 1.1),
            Biquad::low_shelf(sr, 150.0, 1.5),
            Biquad::peak_eq(sr, 100.0, 1.1, 4.5),
            Biquad::peak_eq(sr, 300.0, 0.8, 2.0),
            Biquad::peak_eq(sr, 1500.0, 1.3, 1.5),
            Biquad::high_shelf(sr, 4200.0, -13.0),
            Biquad::lowpass(sr, 6200.0, 0.707),
        ];
        move |x| bands.iter_mut().fold(x, |acc, b| b.process(acc))
    }

    /// Room mic: darker, distance-coloured Alnico.
    fn voicing_room(sr: f32) -> impl FnMut(f32) -> f32 {
        let mut bands = [
            Biquad::highpass(sr, 75.0, 0.8),
            Biquad::low_shelf(sr, 150.0, 2.0),
            Biquad::peak_eq(sr, 350.0, 1.2, -1.5),
            Biquad::peak_eq(sr, 1000.0, 1.0, 1.5),
            Biquad::high_shelf(sr, 3800.0, -8.0),
            Biquad::lowpass(sr, 5200.0, 0.707),
        ];
        move |x| bands.iter_mut().fold(x, |acc, b| b.process(acc))
    }
}

impl Cabinet for VoxCab {
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
        analysis::assert_plausible("vox close L", sr, &TEX_L);
        analysis::assert_plausible("vox close R", sr, &TEX_R);
        analysis::assert_plausible("vox room L", sr, &ROOM_TEX_L);
        analysis::assert_plausible("vox room R", sr, &ROOM_TEX_R);
        analysis::assert_lr_symmetry("vox close", &TEX_L, &TEX_R);
        analysis::assert_lr_symmetry("vox room", &ROOM_TEX_L, &ROOM_TEX_R);
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
        check!("vox close L", VoxCab::voicing_sm57, &TEX_L);
        check!("vox close R", VoxCab::voicing_sm57, &TEX_R);
        check!("vox room L", VoxCab::voicing_room, &ROOM_TEX_L);
        check!("vox room R", VoxCab::voicing_room, &ROOM_TEX_R);
    }
}
