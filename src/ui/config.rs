use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use atomic_float::AtomicF32;
use ratatui::style::Color;

use super::styles::{
    PEDAL_BLUE, PEDAL_CYAN, PEDAL_GOLD, PEDAL_GREEN, PEDAL_INDIGO, PEDAL_LIME, PEDAL_MINT,
    PEDAL_ORANGE, PEDAL_ORCHID, PEDAL_PINK, PEDAL_PURPLE, PEDAL_RED, PEDAL_ROSE, PEDAL_SAND,
    PEDAL_SILVER, PEDAL_STEEL, PEDAL_TEAL, PEDAL_VIBE, PEDAL_YELLOW,
};
use crate::dsp::Params;
pub(super) struct Knob {
    pub(super) label: &'static str,
    pub(super) param: fn(&Params) -> &Arc<AtomicF32>,
}

/// How a pedal's controls are drawn in the detail editor. Most pedals use rotary
/// knobs; a graphic EQ reads far more naturally as a bank of vertical faders, so
/// the table picks the widget per pedal rather than hardwiring it in `draw.rs`.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum PedalUi {
    Knobs,
    Sliders,
}

/// A rig pedal: its livery, the slice of `KNOBS` it owns, its on/off flag, and the
/// widget its controls render as. `render_rig` walks this table to draw both the
/// compact tiles and the detail editor, so adding a pedal is a single entry here
/// (plus its knobs above).
pub(super) struct Pedal {
    pub(super) name: &'static str,
    pub(super) color: Color,
    pub(super) start: usize,
    pub(super) end: usize,
    pub(super) enabled: fn(&Params) -> &Arc<AtomicBool>,
    pub(super) ui: PedalUi,
}

// Knob-index ranges: each section owns a contiguous `[START, END)` slice of the
// KNOBS array below. The amp/mic panels and the PEDALS table reference these
// bounds.
//
// IMPORTANT: ←/→ navigation walks KNOBS linearly, so this order must match the
// KNOBS array one-to-one. The layout follows the DSP signal flow: the amp tone
// stack and cabinet mics first (their own fixed panel), then the pedals in the
// order the sound actually travels — the pre-amp drive chain, then the post-cab
// rack — so the board and ←/→ navigation mirror the header ribbon.
pub(super) const AMP_START: usize = 0;
pub(super) const AMP_END: usize = crate::dsp::amp::AMP_MAX;
pub(super) const MIC_START: usize = AMP_END;
pub(super) const MIC_END: usize = MIC_START + 3;
// Pre-amp pedals (before the amp), in signal order.
pub(super) const NG_START: usize = MIC_END;
pub(super) const NG_END: usize = NG_START + 2;
pub(super) const PITCH_START: usize = NG_END;
pub(super) const PITCH_END: usize = PITCH_START + 3;
pub(super) const WAH_START: usize = PITCH_END;
pub(super) const WAH_END: usize = WAH_START + 4;
pub(super) const CMP_START: usize = WAH_END;
pub(super) const CMP_END: usize = CMP_START + 3;
pub(super) const FUZZ_START: usize = CMP_END;
pub(super) const FUZZ_END: usize = FUZZ_START + 4;
pub(super) const TS_START: usize = FUZZ_END;
pub(super) const TS_END: usize = TS_START + 3;
pub(super) const DS_START: usize = TS_END;
pub(super) const DS_END: usize = DS_START + 3;
pub(super) const ML_START: usize = DS_END;
pub(super) const ML_END: usize = ML_START + 4;
pub(super) const PEQ_START: usize = ML_END;
pub(super) const PEQ_END: usize = PEQ_START + 3;
// Uni-Vibe — the last pedal before the amp (guitar → fuzz → vibe → amp).
pub(super) const UV_START: usize = PEQ_END;
pub(super) const UV_END: usize = UV_START + 4;
// Clean boost — linear front-end gain, the last pedal before the amp.
pub(super) const BOOST_START: usize = UV_END;
pub(super) const BOOST_END: usize = BOOST_START + 3;
// Post-cab rack pedals (after the cab), in signal order.
pub(super) const GEQ_START: usize = BOOST_END;
pub(super) const GEQ_END: usize = GEQ_START + 8;
pub(super) const EQ_START: usize = GEQ_END;
pub(super) const EQ_END: usize = EQ_START + 3;
pub(super) const FL_START: usize = EQ_END;
pub(super) const FL_END: usize = FL_START + 4;
pub(super) const CH_START: usize = FL_END;
pub(super) const CH_END: usize = CH_START + 3;
pub(super) const PH_START: usize = CH_END;
pub(super) const PH_END: usize = PH_START + 4;
pub(super) const TREM_START: usize = PH_END;
pub(super) const TREM_END: usize = TREM_START + 4;
pub(super) const DELAY_START: usize = TREM_END;
pub(super) const DELAY_END: usize = DELAY_START + 4;
pub(super) const REV_START: usize = DELAY_END;
pub(super) const REV_END: usize = REV_START + 3;

pub(super) const KNOBS: &[Knob] = &[
    // 0..AMP_MAX: Amp front-panel controls. Labels and count are model-dependent
    // (see `AmpModel::controls`); `render_amp_box` draws the active model's own
    // labels and only its first `knob_count` slots. The placeholder labels here are
    // never shown — the accessors resolve the active model's bank.
    Knob {
        label: "AMP",
        param: crate::dsp::amp_param::<0>,
    },
    Knob {
        label: "AMP",
        param: crate::dsp::amp_param::<1>,
    },
    Knob {
        label: "AMP",
        param: crate::dsp::amp_param::<2>,
    },
    Knob {
        label: "AMP",
        param: crate::dsp::amp_param::<3>,
    },
    Knob {
        label: "AMP",
        param: crate::dsp::amp_param::<4>,
    },
    Knob {
        label: "AMP",
        param: crate::dsp::amp_param::<5>,
    },
    Knob {
        label: "AMP",
        param: crate::dsp::amp_param::<6>,
    },
    // MIC_START..MIC_END: Cabinet mics (position, dynamic↔ribbon blend, room amount)
    Knob {
        label: "MIC",
        param: |p| &p.mic_pos,
    },
    Knob {
        label: "BLEND",
        param: |p| &p.mic_blend,
    },
    Knob {
        label: "ROOM",
        param: |p| &p.mic_room,
    },
    // ── Pre-amp pedals (before the amp), in signal order ──
    // 9–10: Noise Gate
    Knob {
        label: "THRESH",
        param: |p| &p.ng_threshold,
    },
    Knob {
        label: "RELEASE",
        param: |p| &p.ng_release,
    },
    // 11–13: Pitch shifter / Whammy
    Knob {
        label: "PITCH",
        param: |p| &p.pitch_pitch,
    },
    Knob {
        label: "MIX",
        param: |p| &p.pitch_mix,
    },
    Knob {
        label: "TONE",
        param: |p| &p.pitch_tone,
    },
    // 14–17: Auto-wah
    Knob {
        label: "FREQ",
        param: |p| &p.wah_freq,
    },
    Knob {
        label: "SENS",
        param: |p| &p.wah_sens,
    },
    Knob {
        label: "Q",
        param: |p| &p.wah_q,
    },
    Knob {
        label: "MIX",
        param: |p| &p.wah_mix,
    },
    // 18–20: Compressor
    Knob {
        label: "SUSTAIN",
        param: |p| &p.cmp_sustain,
    },
    Knob {
        label: "ATTACK",
        param: |p| &p.cmp_attack,
    },
    Knob {
        label: "LEVEL",
        param: |p| &p.cmp_level,
    },
    // 21–24: Fuzz (TYPE: 0 = Big Muff, 0.5 = Fuzz Face, 1 = Tone Bender)
    Knob {
        label: "TYPE",
        param: |p| &p.fz_type,
    },
    Knob {
        label: "FUZZ",
        param: |p| &p.fz_fuzz,
    },
    Knob {
        label: "TONE",
        param: |p| &p.fz_tone,
    },
    Knob {
        label: "LEVEL",
        param: |p| &p.fz_level,
    },
    // 25–27: TS-808
    Knob {
        label: "DRIVE",
        param: |p| &p.ts_drive,
    },
    Knob {
        label: "TONE",
        param: |p| &p.ts_tone,
    },
    Knob {
        label: "LEVEL",
        param: |p| &p.ts_level,
    },
    // 28–30: DS-1
    Knob {
        label: "DRIVE",
        param: |p| &p.ds_drive,
    },
    Knob {
        label: "TONE",
        param: |p| &p.ds_tone,
    },
    Knob {
        label: "LEVEL",
        param: |p| &p.ds_level,
    },
    // 31–34: Boss ML-2 Metal Core
    Knob {
        label: "DIST",
        param: |p| &p.ml_dist,
    },
    Knob {
        label: "LOW",
        param: |p| &p.ml_low,
    },
    Knob {
        label: "HIGH",
        param: |p| &p.ml_high,
    },
    Knob {
        label: "LEVEL",
        param: |p| &p.ml_level,
    },
    // 35–37: Pre-amp EQ
    Knob {
        label: "LOW",
        param: |p| &p.peq_low,
    },
    Knob {
        label: "MID",
        param: |p| &p.peq_mid,
    },
    Knob {
        label: "HIGH",
        param: |p| &p.peq_high,
    },
    // 38–41: Uni-Vibe
    Knob {
        label: "RATE",
        param: |p| &p.uv_rate,
    },
    Knob {
        label: "DEPTH",
        param: |p| &p.uv_depth,
    },
    Knob {
        label: "MIX",
        param: |p| &p.uv_mix,
    },
    Knob {
        label: "MODE",
        param: |p| &p.uv_mode,
    },
    // 42–44: Clean boost
    Knob {
        label: "GAIN",
        param: |p| &p.boost_gain,
    },
    Knob {
        label: "TREBLE",
        param: |p| &p.boost_treble,
    },
    Knob {
        label: "BASS",
        param: |p| &p.boost_bass,
    },
    // ── Post-cab rack pedals (after the cab), in signal order ──
    // 45–52: Graphic EQ (Boss GE-7 — seven band faders + output level)
    Knob {
        label: "100",
        param: |p| &p.geq_b1,
    },
    Knob {
        label: "220",
        param: |p| &p.geq_b2,
    },
    Knob {
        label: "470",
        param: |p| &p.geq_b3,
    },
    Knob {
        label: "1K",
        param: |p| &p.geq_b4,
    },
    Knob {
        label: "2.2K",
        param: |p| &p.geq_b5,
    },
    Knob {
        label: "4.7K",
        param: |p| &p.geq_b6,
    },
    Knob {
        label: "10K",
        param: |p| &p.geq_b7,
    },
    Knob {
        label: "LEVEL",
        param: |p| &p.geq_level,
    },
    // 50–52: Parametric EQ
    Knob {
        label: "LOW",
        param: |p| &p.eq_low,
    },
    Knob {
        label: "MID",
        param: |p| &p.eq_mid,
    },
    Knob {
        label: "HIGH",
        param: |p| &p.eq_high,
    },
    // 53–56: Flanger
    Knob {
        label: "RATE",
        param: |p| &p.fl_rate,
    },
    Knob {
        label: "DEPTH",
        param: |p| &p.fl_depth,
    },
    Knob {
        label: "FEEDBACK",
        param: |p| &p.fl_feedback,
    },
    Knob {
        label: "MIX",
        param: |p| &p.fl_mix,
    },
    // 57–59: Chorus
    Knob {
        label: "RATE",
        param: |p| &p.ch_rate,
    },
    Knob {
        label: "DEPTH",
        param: |p| &p.ch_depth,
    },
    Knob {
        label: "MIX",
        param: |p| &p.ch_mix,
    },
    // 60–63: Phaser
    Knob {
        label: "RATE",
        param: |p| &p.ph_rate,
    },
    Knob {
        label: "DEPTH",
        param: |p| &p.ph_depth,
    },
    Knob {
        label: "FEEDBACK",
        param: |p| &p.ph_feedback,
    },
    Knob {
        label: "MIX",
        param: |p| &p.ph_mix,
    },
    // 64–67: Tremolo / Vibrato
    Knob {
        label: "RATE",
        param: |p| &p.trem_rate,
    },
    Knob {
        label: "DEPTH",
        param: |p| &p.trem_depth,
    },
    Knob {
        label: "SHAPE",
        param: |p| &p.trem_shape,
    },
    Knob {
        label: "MODE",
        param: |p| &p.trem_mode,
    },
    // 68–71: Delay (TYPE: 0 = digital ping-pong, 0.5 = Echorec, 1 = tape)
    Knob {
        label: "TIME",
        param: |p| &p.delay_time,
    },
    Knob {
        label: "FEEDBACK",
        param: |p| &p.delay_feedback,
    },
    Knob {
        label: "MIX",
        param: |p| &p.delay_mix,
    },
    Knob {
        label: "TYPE",
        param: |p| &p.delay_type,
    },
    // 72–74: Reverb
    Knob {
        label: "ROOM",
        param: |p| &p.rev_room,
    },
    Knob {
        label: "DAMP",
        param: |p| &p.rev_damp,
    },
    Knob {
        label: "MIX",
        param: |p| &p.rev_mix,
    },
];

// Rig pedals in navigation order (mirrors the KNOBS slices above). The tile
// grid and detail editor both iterate this table.
pub(super) const PEDALS: &[Pedal] = &[
    // Pre-amp drive chain (before the amp), in signal order.
    Pedal {
        name: "NOISE GATE",
        color: PEDAL_SILVER,
        start: NG_START,
        end: NG_END,
        enabled: |p| &p.ng_enabled,
        ui: PedalUi::Knobs,
    },
    Pedal {
        name: "WHAMMY",
        color: PEDAL_CYAN,
        start: PITCH_START,
        end: PITCH_END,
        enabled: |p| &p.pitch_enabled,
        ui: PedalUi::Knobs,
    },
    Pedal {
        name: "WAH",
        color: PEDAL_ORCHID,
        start: WAH_START,
        end: WAH_END,
        enabled: |p| &p.wah_enabled,
        ui: PedalUi::Knobs,
    },
    Pedal {
        name: "COMP",
        color: PEDAL_GOLD,
        start: CMP_START,
        end: CMP_END,
        enabled: |p| &p.cmp_enabled,
        ui: PedalUi::Knobs,
    },
    Pedal {
        name: "FUZZ",
        color: PEDAL_RED,
        start: FUZZ_START,
        end: FUZZ_END,
        enabled: |p| &p.fz_enabled,
        ui: PedalUi::Knobs,
    },
    Pedal {
        name: "TS-808",
        color: PEDAL_GREEN,
        start: TS_START,
        end: TS_END,
        enabled: |p| &p.ts_enabled,
        ui: PedalUi::Knobs,
    },
    Pedal {
        name: "DS-1",
        color: PEDAL_ORANGE,
        start: DS_START,
        end: DS_END,
        enabled: |p| &p.ds_enabled,
        ui: PedalUi::Knobs,
    },
    Pedal {
        name: "ML-2 METAL CORE",
        color: PEDAL_STEEL,
        start: ML_START,
        end: ML_END,
        enabled: |p| &p.ml_enabled,
        ui: PedalUi::Knobs,
    },
    Pedal {
        name: "PRE-AMP EQ",
        color: PEDAL_LIME,
        start: PEQ_START,
        end: PEQ_END,
        enabled: |p| &p.peq_enabled,
        ui: PedalUi::Knobs,
    },
    Pedal {
        name: "UNI-VIBE",
        color: PEDAL_VIBE,
        start: UV_START,
        end: UV_END,
        enabled: |p| &p.uv_enabled,
        ui: PedalUi::Knobs,
    },
    Pedal {
        name: "CLEAN BOOST",
        color: PEDAL_MINT,
        start: BOOST_START,
        end: BOOST_END,
        enabled: |p| &p.boost_enabled,
        ui: PedalUi::Knobs,
    },
    // Post-cab rack (after the cab), in signal order.
    Pedal {
        name: "GRAPHIC EQ",
        color: PEDAL_SAND,
        start: GEQ_START,
        end: GEQ_END,
        enabled: |p| &p.geq_enabled,
        ui: PedalUi::Sliders,
    },
    Pedal {
        name: "PARAMETRIC EQ",
        color: PEDAL_TEAL,
        start: EQ_START,
        end: EQ_END,
        enabled: |p| &p.eq_enabled,
        ui: PedalUi::Knobs,
    },
    Pedal {
        name: "FLANGER",
        color: PEDAL_INDIGO,
        start: FL_START,
        end: FL_END,
        enabled: |p| &p.fl_enabled,
        ui: PedalUi::Knobs,
    },
    Pedal {
        name: "CHORUS",
        color: PEDAL_PINK,
        start: CH_START,
        end: CH_END,
        enabled: |p| &p.ch_enabled,
        ui: PedalUi::Knobs,
    },
    Pedal {
        name: "PHASER",
        color: PEDAL_YELLOW,
        start: PH_START,
        end: PH_END,
        enabled: |p| &p.ph_enabled,
        ui: PedalUi::Knobs,
    },
    Pedal {
        name: "TREMOLO / VIBRATO",
        color: PEDAL_ROSE,
        start: TREM_START,
        end: TREM_END,
        enabled: |p| &p.trem_enabled,
        ui: PedalUi::Knobs,
    },
    Pedal {
        name: "DELAY",
        color: PEDAL_PURPLE,
        start: DELAY_START,
        end: DELAY_END,
        enabled: |p| &p.delay_enabled,
        ui: PedalUi::Knobs,
    },
    Pedal {
        name: "SPRING REVERB",
        color: PEDAL_BLUE,
        start: REV_START,
        end: REV_END,
        enabled: |p| &p.rev_enabled,
        ui: PedalUi::Knobs,
    },
];

// Sentinel focus value for the "+ ADD" tile at the end of the board. It is not
// a real knob index, so any code that indexes `KNOBS` must guard against it.
pub(super) const ADD_TILE: usize = KNOBS.len();

// Sentinel focus value for the practice timeline pane. Like `ADD_TILE`, it is not
// a real knob index, so any code that indexes `KNOBS` must guard against it.
pub(super) const PRACTICE_TILE: usize = KNOBS.len() + 1;

// Sentinel focus value for the live-order ribbon (panel 1). The ribbon owns
// focus as a whole; the selected stage within it lives in session state
// (`chain_cursor`), not in `focus`.
pub(super) const CHAIN_TILE: usize = KNOBS.len() + 2;

/// Which top-level panels the user has chosen to show. Panel 1 (the live-order
/// ribbon) is always visible; panels 2–4 are toggled live with the `2` / `3` /
/// `4` keys (`1` only focuses the ribbon). Hidden panels are skipped by focus
/// navigation (like off-board pedals) and left out of the layout so their space
/// is reclaimed.
#[derive(Clone, Copy)]
pub(super) struct Panels {
    /// Amp & cabinet panel (the selector row plus the amp/mic knob panel).
    pub amp: bool,
    /// Guitar pedalboard.
    pub rig: bool,
    /// Practice timeline.
    pub timeline: bool,
}

impl Panels {
    /// Every panel visible — the startup state (session-only; not persisted).
    pub(super) const fn all_visible() -> Self {
        Self {
            amp: true,
            rig: true,
            timeline: true,
        }
    }
}

impl Default for Panels {
    fn default() -> Self {
        Self::all_visible()
    }
}

/// Index into `PEDALS` owning the given knob, or `None` for amp/mic knobs.
pub(super) fn pedal_of(knob: usize) -> Option<usize> {
    PEDALS.iter().position(|p| (p.start..p.end).contains(&knob))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsp::ChainStage;

    // These tests pin the hand-maintained contract between the KNOBS array and
    // the PEDALS table (see the module comment): the ←/→ navigation walks KNOBS
    // linearly, so pedal ranges must tile the pedal region contiguously with no
    // gaps or overlaps. The `add-pedal` skill edits these tables, so a slip here
    // would silently break navigation — fail loudly instead.

    #[test]
    fn pedal_ranges_are_contiguous_and_end_at_knobs_len() {
        // The first pedal starts right after the amp+mic block.
        assert_eq!(
            PEDALS[0].start, MIC_END,
            "first pedal must follow the mic knobs"
        );
        // Each pedal has a non-empty range that abuts the next with no gap/overlap.
        for w in PEDALS.windows(2) {
            assert!(
                w[0].start < w[0].end,
                "{} has an empty knob range",
                w[0].name
            );
            assert_eq!(
                w[0].end, w[1].start,
                "gap or overlap between {} and {}",
                w[0].name, w[1].name
            );
        }
        let last = PEDALS.last().expect("PEDALS is non-empty");
        assert!(
            last.start < last.end,
            "{} has an empty knob range",
            last.name
        );
        assert_eq!(
            last.end,
            KNOBS.len(),
            "KNOBS has trailing knobs that no pedal owns"
        );
    }

    #[test]
    fn amp_and_mic_ranges_cover_the_head_of_knobs() {
        assert_eq!(AMP_START, 0);
        assert_eq!(AMP_END, MIC_START, "amp and mic sections must abut");
        assert!(MIC_END <= KNOBS.len());
    }

    #[test]
    fn pedal_of_maps_every_pedal_knob_back_to_its_pedal() {
        for (pi, p) in PEDALS.iter().enumerate() {
            for k in p.start..p.end {
                assert_eq!(pedal_of(k), Some(pi), "{} knob {k} misrouted", p.name);
            }
        }
    }

    #[test]
    fn amp_and_mic_knobs_belong_to_no_pedal() {
        for k in AMP_START..MIC_END {
            assert_eq!(
                pedal_of(k),
                None,
                "amp/mic knob {k} wrongly claimed by a pedal"
            );
        }
    }

    #[test]
    fn add_tile_is_out_of_the_knob_range() {
        // The +ADD sentinel must never index KNOBS.
        assert_eq!(ADD_TILE, KNOBS.len());
        assert_eq!(pedal_of(ADD_TILE), None);
    }

    #[test]
    fn pedal_names_are_unique() {
        for (i, a) in PEDALS.iter().enumerate() {
            for b in &PEDALS[i + 1..] {
                assert_ne!(a.name, b.name, "duplicate pedal name {}", a.name);
            }
        }
    }

    #[test]
    fn table_sizes_are_stable() {
        // Deliberate tripwire: bump these when you add or remove a pedal/knob so
        // the change is a conscious, reviewed edit rather than an accident.
        assert_eq!(PEDALS.len(), 19, "pedal count changed");
        assert_eq!(KNOBS.len(), 79, "knob count changed");
    }

    #[test]
    fn chain_stage_pedal_index_matches_pedals_table() {
        // The DSP chain order addresses pedals by PEDALS index: every pedal must
        // own exactly one stage and round-trip through it, and the amp and cab
        // stages must own none. The `add-pedal` flow extends both sides together.
        for (pi, p) in PEDALS.iter().enumerate() {
            let stage =
                ChainStage::from_pedal_index(pi).unwrap_or_else(|| panic!("{pi} has no stage"));
            assert_eq!(
                stage.pedal_index(),
                Some(pi),
                "{} maps to the wrong stage",
                p.name
            );
        }
        assert_eq!(ChainStage::Amp.pedal_index(), None);
        assert_eq!(ChainStage::Cab.pedal_index(), None);
        assert_eq!(ChainStage::default_order().len(), PEDALS.len() + 2);
    }
}
