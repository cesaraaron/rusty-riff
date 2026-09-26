pub mod amp;
pub mod biquad;
pub mod cab;
pub mod conv;
pub mod effects;
pub mod metronome;
pub mod oversample;
pub mod player;
pub mod resample;
pub mod tonestack;
pub mod tuner;

pub use metronome::{Metronome, MetronomeVoice};
pub use player::{PlayerTrack, PlayerVoice, Transport};
pub use tuner::Tuner;

use atomic_float::AtomicF32;
use std::sync::Arc;
use std::sync::atomic::{
    AtomicBool, AtomicU8, AtomicU64, AtomicUsize,
    Ordering::{Relaxed, SeqCst},
};

use amp::{AMP_MAX, AmpBank, AmpKnob};
use cab::{CabBank, ExternalIrCab};
use effects::{
    Chorus, CleanBoost, Compressor, Delay, Distortion, Flanger, Fuzz, GraphicEq, MetalCore,
    NoiseGate, ParametricEq, Phaser, Pitch, PreampEq, Reverb, Tremolo, TubeScreamer, UniVibe, Wah,
};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum AmpModel {
    Marshall = 0,
    Mesa = 1,
    Randall = 2,
    Vox = 3,
    Hiwatt = 4,
    Plexi = 5,
    Fender = 6,
    Supro = 7,
    Tweed = 8,
}

impl AmpModel {
    pub fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::Mesa,
            2 => Self::Randall,
            3 => Self::Vox,
            4 => Self::Hiwatt,
            5 => Self::Plexi,
            6 => Self::Fender,
            7 => Self::Supro,
            8 => Self::Tweed,
            _ => Self::Marshall,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Marshall => "Marshall JCM800",
            Self::Mesa => "Mesa Dual Rectifier",
            Self::Randall => "Randall Warhead",
            Self::Vox => "Vox AC30",
            Self::Hiwatt => "Hiwatt DR103",
            Self::Plexi => "Marshall Plexi",
            Self::Fender => "Fender Twin Reverb",
            Self::Supro => "Supro Combo",
            Self::Tweed => "Tweed Deluxe",
        }
    }

    pub fn short_name(self) -> &'static str {
        match self {
            Self::Marshall => "JCM800",
            Self::Mesa => "DUAL RECT",
            Self::Randall => "RANDALL",
            Self::Vox => "AC30",
            Self::Hiwatt => "DR103",
            Self::Plexi => "PLEXI",
            Self::Fender => "TWIN",
            Self::Supro => "SUPRO",
            Self::Tweed => "TWEED",
        }
    }

    pub fn next(self) -> Self {
        match self {
            Self::Marshall => Self::Mesa,
            Self::Mesa => Self::Randall,
            Self::Randall => Self::Vox,
            Self::Vox => Self::Hiwatt,
            Self::Hiwatt => Self::Plexi,
            Self::Plexi => Self::Fender,
            Self::Fender => Self::Supro,
            Self::Supro => Self::Tweed,
            Self::Tweed => Self::Marshall,
        }
    }

    pub fn prev(self) -> Self {
        match self {
            Self::Marshall => Self::Supro,
            Self::Mesa => Self::Marshall,
            Self::Randall => Self::Mesa,
            Self::Vox => Self::Randall,
            Self::Hiwatt => Self::Vox,
            Self::Plexi => Self::Hiwatt,
            Self::Fender => Self::Plexi,
            Self::Supro => Self::Fender,
            Self::Tweed => Self::Supro,
        }
    }

    /// All models in picker order — the single source for the amp modal,
    /// cursor init, and tests.
    pub const ALL: [Self; 9] = [
        Self::Marshall,
        Self::Mesa,
        Self::Randall,
        Self::Vox,
        Self::Hiwatt,
        Self::Plexi,
        Self::Fender,
        Self::Supro,
        Self::Tweed,
    ];

    /// The model's front-panel controls, in the order its DSP decodes them.
    pub fn controls(self) -> &'static [AmpKnob] {
        match self {
            Self::Marshall => amp::marshall::KNOBS,
            Self::Mesa => amp::mesa::KNOBS,
            Self::Randall => amp::randall::KNOBS,
            Self::Vox => amp::vox::KNOBS,
            Self::Hiwatt => amp::hiwatt::KNOBS,
            Self::Plexi => amp::plexi::KNOBS,
            Self::Fender => amp::fender::KNOBS,
            Self::Supro => amp::supro::KNOBS,
            Self::Tweed => amp::tweed::KNOBS,
        }
    }

    /// Number of front-panel knobs this model exposes (`<= AMP_MAX`).
    pub fn knob_count(self) -> usize {
        self.controls().len()
    }

    /// Index of the control with the given stable slug, if the model has it.
    pub fn knob_slot(self, slug: &str) -> Option<usize> {
        self.controls().iter().position(|k| k.slug == slug)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum CabModel {
    Mesa = 0,
    Marshall = 1,
    Orange = 2,
    Wem = 3,
    Vox = 4,
    Fender = 5,
    Supro = 6,
    Tweed = 7,
}

impl CabModel {
    pub fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::Marshall,
            2 => Self::Orange,
            3 => Self::Wem,
            4 => Self::Vox,
            5 => Self::Fender,
            6 => Self::Supro,
            7 => Self::Tweed,
            _ => Self::Mesa,
        }
    }

    #[allow(dead_code)]
    pub fn name(self) -> &'static str {
        match self {
            Self::Mesa => "Mesa 4×12 (V30)",
            Self::Marshall => "Marshall 4×12 (GB)",
            Self::Orange => "Orange PPC412 (V30)",
            Self::Wem => "WEM 4×12 (Fane)",
            Self::Vox => "Vox 2×12 (Alnico Blue)",
            Self::Fender => "Fender 2×12 (Jensen)",
            Self::Supro => "Supro 1×10 (small)",
            Self::Tweed => "Tweed 1×12 (small)",
        }
    }

    pub fn short_name(self) -> &'static str {
        match self {
            Self::Mesa => "MESA V30",
            Self::Marshall => "MARSH GB",
            Self::Orange => "ORANGE",
            Self::Wem => "WEM FANE",
            Self::Vox => "VOX BLUE",
            Self::Fender => "FENDER 12",
            Self::Supro => "SUPRO 10",
            Self::Tweed => "TWEED 12",
        }
    }

    pub fn toggle(self) -> Self {
        match self {
            Self::Mesa => Self::Marshall,
            Self::Marshall => Self::Orange,
            Self::Orange => Self::Wem,
            Self::Wem => Self::Vox,
            Self::Vox => Self::Fender,
            Self::Fender => Self::Supro,
            Self::Supro => Self::Tweed,
            Self::Tweed => Self::Mesa,
        }
    }

    /// All models in picker order — the single source for the cab modal,
    /// cursor init, and tests.
    pub const ALL: [Self; 8] = [
        Self::Mesa,
        Self::Marshall,
        Self::Orange,
        Self::Wem,
        Self::Vox,
        Self::Fender,
        Self::Supro,
        Self::Tweed,
    ];
}

/// One slot in the reorderable signal chain: the 11 pre pedals (mono DSP), the
/// amp head, the cabinet/mic, then the 8 stereo rack pedals. The amp and cab are
/// separate stages so they can be moved independently, but the amp must always
/// precede its cab (see [`sanitize_chain_order`]). Effects placed *between* them
/// model line-level processing in a virtual load box / effects loop, not a pedal
/// wired into the speaker cable. The UI moves these slots with `[` / `]`; presets
/// persist the order by [`ChainStage::name`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum ChainStage {
    Gate = 0,
    Whammy = 1,
    Wah = 2,
    Comp = 3,
    Fuzz = 4,
    Ts = 5,
    Ds = 6,
    Metal = 7,
    PreEq = 8,
    Vibe = 9,
    Boost = 10,
    Amp = 11,
    Cab = 12,
    Geq = 13,
    Eq = 14,
    Flanger = 15,
    Chorus = 16,
    Phaser = 17,
    Trem = 18,
    Delay = 19,
    Reverb = 20,
}

/// Number of slots in [`ChainStage`]: 11 pre + amp + cab + 8 rack.
pub const CHAIN_LEN: usize = 21;

/// Bounded number of attempts the audio-thread reader makes before giving up and
/// reusing its last-good order (see [`Params::try_chain_slots`]).
pub const CHAIN_READ_ATTEMPTS: usize = 4;

impl ChainStage {
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::Gate),
            1 => Some(Self::Whammy),
            2 => Some(Self::Wah),
            3 => Some(Self::Comp),
            4 => Some(Self::Fuzz),
            5 => Some(Self::Ts),
            6 => Some(Self::Ds),
            7 => Some(Self::Metal),
            8 => Some(Self::PreEq),
            9 => Some(Self::Vibe),
            10 => Some(Self::Boost),
            11 => Some(Self::Amp),
            12 => Some(Self::Cab),
            13 => Some(Self::Geq),
            14 => Some(Self::Eq),
            15 => Some(Self::Flanger),
            16 => Some(Self::Chorus),
            17 => Some(Self::Phaser),
            18 => Some(Self::Trem),
            19 => Some(Self::Delay),
            20 => Some(Self::Reverb),
            _ => None,
        }
    }

    /// Stable preset/UI name for this stage. Legacy `"ampcab"` (the pre-split
    /// combined block) is migrated to consecutive `"amp"`, `"cab"` by the preset
    /// parser, not returned here.
    pub fn name(self) -> &'static str {
        match self {
            Self::Gate => "gate",
            Self::Whammy => "whammy",
            Self::Wah => "wah",
            Self::Comp => "comp",
            Self::Fuzz => "fuzz",
            Self::Ts => "ts",
            Self::Ds => "ds",
            Self::Metal => "metal",
            Self::PreEq => "preeq",
            Self::Vibe => "vibe",
            Self::Boost => "boost",
            Self::Amp => "amp",
            Self::Cab => "cab",
            Self::Geq => "geq",
            Self::Eq => "eq",
            Self::Flanger => "flanger",
            Self::Chorus => "chorus",
            Self::Phaser => "phaser",
            Self::Trem => "trem",
            Self::Delay => "delay",
            Self::Reverb => "reverb",
        }
    }

    pub fn from_name(s: &str) -> Option<Self> {
        match s {
            "gate" => Some(Self::Gate),
            "whammy" => Some(Self::Whammy),
            "wah" => Some(Self::Wah),
            "comp" => Some(Self::Comp),
            "fuzz" => Some(Self::Fuzz),
            "ts" => Some(Self::Ts),
            "ds" => Some(Self::Ds),
            "metal" => Some(Self::Metal),
            "preeq" => Some(Self::PreEq),
            "vibe" => Some(Self::Vibe),
            "boost" => Some(Self::Boost),
            "amp" => Some(Self::Amp),
            "cab" => Some(Self::Cab),
            "geq" => Some(Self::Geq),
            "eq" => Some(Self::Eq),
            "flanger" => Some(Self::Flanger),
            "chorus" => Some(Self::Chorus),
            "phaser" => Some(Self::Phaser),
            "trem" => Some(Self::Trem),
            "delay" => Some(Self::Delay),
            "reverb" => Some(Self::Reverb),
            _ => None,
        }
    }

    /// The shipped order: pre pedals → amp → cab → rack (the historical sequence).
    pub fn default_order() -> [u8; CHAIN_LEN] {
        [
            Self::Gate as u8,
            Self::Whammy as u8,
            Self::Wah as u8,
            Self::Comp as u8,
            Self::Fuzz as u8,
            Self::Ts as u8,
            Self::Ds as u8,
            Self::Metal as u8,
            Self::PreEq as u8,
            Self::Vibe as u8,
            Self::Boost as u8,
            Self::Amp as u8,
            Self::Cab as u8,
            Self::Geq as u8,
            Self::Eq as u8,
            Self::Flanger as u8,
            Self::Chorus as u8,
            Self::Phaser as u8,
            Self::Trem as u8,
            Self::Delay as u8,
            Self::Reverb as u8,
        ]
    }

    /// Index into the UI `PEDALS` table, or `None` for the amp and cab stages.
    /// Pre pedals map 1:1; rack pedals sit right after the 11 pre entries.
    pub fn pedal_index(self) -> Option<usize> {
        match self {
            Self::Gate => Some(0),
            Self::Whammy => Some(1),
            Self::Wah => Some(2),
            Self::Comp => Some(3),
            Self::Fuzz => Some(4),
            Self::Ts => Some(5),
            Self::Ds => Some(6),
            Self::Metal => Some(7),
            Self::PreEq => Some(8),
            Self::Vibe => Some(9),
            Self::Boost => Some(10),
            Self::Amp | Self::Cab => None,
            Self::Geq => Some(11),
            Self::Eq => Some(12),
            Self::Flanger => Some(13),
            Self::Chorus => Some(14),
            Self::Phaser => Some(15),
            Self::Trem => Some(16),
            Self::Delay => Some(17),
            Self::Reverb => Some(18),
        }
    }

    pub fn from_pedal_index(pi: usize) -> Option<Self> {
        match pi {
            0 => Some(Self::Gate),
            1 => Some(Self::Whammy),
            2 => Some(Self::Wah),
            3 => Some(Self::Comp),
            4 => Some(Self::Fuzz),
            5 => Some(Self::Ts),
            6 => Some(Self::Ds),
            7 => Some(Self::Metal),
            8 => Some(Self::PreEq),
            9 => Some(Self::Vibe),
            10 => Some(Self::Boost),
            11 => Some(Self::Geq),
            12 => Some(Self::Eq),
            13 => Some(Self::Flanger),
            14 => Some(Self::Chorus),
            15 => Some(Self::Phaser),
            16 => Some(Self::Trem),
            17 => Some(Self::Delay),
            18 => Some(Self::Reverb),
            _ => None,
        }
    }

    /// True for the mono pre pedals. The rack pedals (and the amp/cab stages,
    /// handled explicitly by the dispatch) are not mono pre pedals.
    pub fn is_mono_pedal(self) -> bool {
        matches!(
            self,
            Self::Gate
                | Self::Whammy
                | Self::Wah
                | Self::Comp
                | Self::Fuzz
                | Self::Ts
                | Self::Ds
                | Self::Metal
                | Self::PreEq
                | Self::Vibe
                | Self::Boost
        )
    }
}

/// True when the amp stage precedes the cab stage in `order` (or when one of
/// them is absent, which sanitizing repairs). Used to reject a UI move that
/// would place the cab before its amp.
pub fn amp_precedes_cab(order: &[u8; CHAIN_LEN]) -> bool {
    let amp = order.iter().position(|&v| v == ChainStage::Amp as u8);
    let cab = order.iter().position(|&v| v == ChainStage::Cab as u8);
    match (amp, cab) {
        (Some(a), Some(c)) => a < c,
        _ => true,
    }
}

/// Sanitize a candidate order into a valid one: drop unknown ids and dupes,
/// append missing stages in default order, and repair a cab placed before its
/// amp. Always returns every stage exactly once, so the audio thread never sees
/// a half-built chain.
pub fn sanitize_chain_order(ids: &[u8]) -> [u8; CHAIN_LEN] {
    let mut seen = [false; CHAIN_LEN];
    let mut out = Vec::with_capacity(CHAIN_LEN);
    for &v in ids {
        if let Some(stage) = ChainStage::from_u8(v) {
            let i = stage as usize;
            if !seen[i] {
                seen[i] = true;
                out.push(v);
            }
        }
    }
    for &v in &ChainStage::default_order() {
        let i = ChainStage::from_u8(v).map(|s| s as usize).unwrap_or(0);
        if !seen[i] {
            seen[i] = true;
            out.push(v);
        }
    }
    // The cab may never precede its amp: deterministically swap the pair back.
    let amp = out.iter().position(|&v| v == ChainStage::Amp as u8);
    let cab = out.iter().position(|&v| v == ChainStage::Cab as u8);
    if let (Some(a), Some(c)) = (amp, cab)
        && a > c
    {
        out.swap(a, c);
    }
    out.try_into()
        .unwrap_or_else(|_| ChainStage::default_order())
}

const DEFAULT_AMP_MODEL: u8 = AmpModel::Mesa as u8;
const DEFAULT_CAB_MODEL: u8 = CabModel::Mesa as u8;
const DEFAULT_MIC_POS: f32 = 0.4;
const DEFAULT_MIC_BLEND: f32 = 0.0;
const DEFAULT_MIC_ROOM: f32 = 0.0;
/// Studio-master stereo width. `1.0` = neutral reference (no side boost),
/// `1.3` = the historic shipped widening. The default keeps existing presets
/// and recordings sounding as before; `W` cycles neutral ↔ studio live.
pub const DEFAULT_MASTER_WIDTH: f32 = 1.3;

// When an external IR is loaded it can be toggled against the built-in cabs live;
// it starts inactive (the engine boots on a built-in cab).
const DEFAULT_CAB_EXTERNAL_ACTIVE: bool = false;

// Same story for an external amp (a hosted AU amp sim): it can be toggled against the
// built-in amp+cab live and starts inactive (the engine boots on the built-in amp).
const DEFAULT_AMP_EXTERNAL_ACTIVE: bool = false;

const DEFAULT_NG_ENABLED: bool = true;
const DEFAULT_NG_THRESHOLD: f32 = 0.20;
const DEFAULT_NG_RELEASE: f32 = 0.30;

const DEFAULT_CMP_ENABLED: bool = false;
const DEFAULT_CMP_SUSTAIN: f32 = 0.40;
const DEFAULT_CMP_ATTACK: f32 = 0.30;
const DEFAULT_CMP_LEVEL: f32 = 0.50;

const DEFAULT_PITCH_ENABLED: bool = false;
const DEFAULT_PITCH_PITCH: f32 = 0.50; // unison
const DEFAULT_PITCH_MIX: f32 = 0.50;
const DEFAULT_PITCH_TONE: f32 = 0.70;

const DEFAULT_WAH_ENABLED: bool = false;
const DEFAULT_WAH_FREQ: f32 = 0.40;
const DEFAULT_WAH_SENS: f32 = 0.55;
const DEFAULT_WAH_Q: f32 = 0.50;
const DEFAULT_WAH_MIX: f32 = 0.90;

const DEFAULT_PEQ_ENABLED: bool = false;
const DEFAULT_PEQ_LOW: f32 = 0.50;
const DEFAULT_PEQ_MID: f32 = 0.50;
const DEFAULT_PEQ_HIGH: f32 = 0.50;

// Uni-Vibe (front-of-amp modulation). Off by default; chorus mode at a medium
// depth and mix when added.
const DEFAULT_UV_ENABLED: bool = false;
const DEFAULT_UV_RATE: f32 = 0.30;
const DEFAULT_UV_DEPTH: f32 = 0.60;
const DEFAULT_UV_MIX: f32 = 0.50;
const DEFAULT_UV_MODE: f32 = 0.00;

// Clean boost (linear front-end gain). Off by default; a moderate boost and a
// flat tone stack when added.
const DEFAULT_BOOST_ENABLED: bool = false;
const DEFAULT_BOOST_GAIN: f32 = 0.40;
const DEFAULT_BOOST_TREBLE: f32 = 0.50;
const DEFAULT_BOOST_BASS: f32 = 0.50;

const DEFAULT_FZ_ENABLED: bool = false;
const DEFAULT_FZ_FUZZ: f32 = 0.70;
const DEFAULT_FZ_TONE: f32 = 0.50;
const DEFAULT_FZ_LEVEL: f32 = 0.60;
// 0 = Big Muff (the shipped default), 1 = Fuzz Face.
const DEFAULT_FZ_TYPE: f32 = 0.0;

const DEFAULT_TS_ENABLED: bool = true;
const DEFAULT_TS_DRIVE: f32 = 0.45;
const DEFAULT_TS_TONE: f32 = 0.60;
const DEFAULT_TS_LEVEL: f32 = 0.70;

const DEFAULT_DS_ENABLED: bool = false;
const DEFAULT_DS_DRIVE: f32 = 0.40;
const DEFAULT_DS_TONE: f32 = 0.50;
const DEFAULT_DS_LEVEL: f32 = 0.65;

const DEFAULT_ML_ENABLED: bool = false;
const DEFAULT_ML_DIST: f32 = 0.65;
const DEFAULT_ML_LOW: f32 = 0.50;
const DEFAULT_ML_HIGH: f32 = 0.50;
const DEFAULT_ML_LEVEL: f32 = 0.60;

const DEFAULT_REV_ENABLED: bool = true;
const DEFAULT_REV_ROOM: f32 = 0.55;
const DEFAULT_REV_DAMP: f32 = 0.40;
const DEFAULT_REV_MIX: f32 = 0.25;

// Graphic EQ (Boss GE-7): seven band faders + output level, all default flat.
const DEFAULT_GEQ_ENABLED: bool = false;
const DEFAULT_GEQ_BAND: f32 = 0.50; // 0.5 = 0 dB (flat) for every band
const DEFAULT_GEQ_LEVEL: f32 = 0.50; // 0.5 = unity output

const DEFAULT_EQ_ENABLED: bool = false;
const DEFAULT_EQ_LOW: f32 = 0.50;
const DEFAULT_EQ_MID: f32 = 0.50;
const DEFAULT_EQ_HIGH: f32 = 0.50;

const DEFAULT_DELAY_ENABLED: bool = false;
const DEFAULT_DELAY_TIME: f32 = 0.30;
const DEFAULT_DELAY_FEEDBACK: f32 = 0.40;
const DEFAULT_DELAY_MIX: f32 = 0.30;
// 0 = digital ping-pong, 1 = tape (Echoplex-style).
const DEFAULT_DELAY_TYPE: f32 = 0.0;

const DEFAULT_FL_ENABLED: bool = false;
const DEFAULT_FL_RATE: f32 = 0.30;
const DEFAULT_FL_DEPTH: f32 = 0.55;
const DEFAULT_FL_FEEDBACK: f32 = 0.35;
const DEFAULT_FL_MIX: f32 = 0.50;

const DEFAULT_CH_ENABLED: bool = false;
const DEFAULT_CH_RATE: f32 = 0.25;
const DEFAULT_CH_DEPTH: f32 = 0.50;
const DEFAULT_CH_MIX: f32 = 0.50;

const DEFAULT_PH_ENABLED: bool = false;
const DEFAULT_PH_RATE: f32 = 0.30;
const DEFAULT_PH_DEPTH: f32 = 0.70;
const DEFAULT_PH_FEEDBACK: f32 = 0.40;
const DEFAULT_PH_MIX: f32 = 0.50;

// Tremolo / Vibrato (post-cab modulation, after the phaser, before the delay)
const DEFAULT_TREM_ENABLED: bool = false;
const DEFAULT_TREM_RATE: f32 = 0.35;
const DEFAULT_TREM_DEPTH: f32 = 0.55;
const DEFAULT_TREM_SHAPE: f32 = 0.00; // sine
const DEFAULT_TREM_MODE: f32 = 0.00; // tremolo (amplitude)

// Per-model amp knob defaults now live on each model's `KNOBS` descriptor
// (src/dsp/amp/*.rs), not in global constants.

pub struct Params {
    // Amp model selector
    pub amp_model: Arc<AtomicU8>,

    // Cabinet model selector
    pub cab_model: Arc<AtomicU8>,

    // Mic position (0 = edge/dark, 1 = center/bright)
    pub mic_pos: Arc<AtomicF32>,
    // Mic blend (0 = close SM57 dynamic, 1 = R121 ribbon)
    pub mic_blend: Arc<AtomicF32>,
    // Room mic amount (0 = dry close mic only, 1 = full ambient room)
    pub mic_room: Arc<AtomicF32>,

    // Studio-master stereo width applied on the output bus (`1.0` neutral,
    // `1.3` the historic widening). The output limiter is independent.
    pub master_width: Arc<AtomicF32>,

    // External-IR cab override. `cab_external_active` selects the loaded IR over the
    // built-in cab (flipped live by the UI, instant, no reload). `cab_external_loaded`
    // is set by the control thread so the UI knows an IR is installed and the toggle
    // is meaningful.
    pub cab_external_active: Arc<AtomicBool>,
    pub cab_external_loaded: Arc<AtomicBool>,

    // External-amp override. `amp_external_active` selects a loaded AU amp sim over the
    // built-in amp+cab (flipped live by the UI, instant, no reload). `amp_external_loaded`
    // is set by the control thread so the UI knows an AU is installed and the toggle is
    // meaningful. When active, the built-in amp *and* cab are bypassed (an amp-sim AU
    // brings its own cabinet).
    pub amp_external_active: Arc<AtomicBool>,
    pub amp_external_loaded: Arc<AtomicBool>,
    // `true` = the loaded AU is amp-only, so the built-in cab (or active external IR)
    // still runs on its output; `false` (default) = the AU brings its own cab and the
    // built-in cab is bypassed. `amp_external_latency` is the AU's reported latency in
    // frames, used to delay the built-in path so built-in↔AU stays time-coherent.
    pub amp_external_amp_only: Arc<AtomicBool>,
    pub amp_external_latency: Arc<AtomicUsize>,

    // Noise gate
    pub ng_enabled: Arc<AtomicBool>,
    pub ng_threshold: Arc<AtomicF32>,
    pub ng_release: Arc<AtomicF32>,

    // Compressor (front of chain)
    pub cmp_enabled: Arc<AtomicBool>,
    pub cmp_sustain: Arc<AtomicF32>,
    pub cmp_attack: Arc<AtomicF32>,
    pub cmp_level: Arc<AtomicF32>,

    // Pitch shifter / Whammy (early mono chain, after the gate)
    pub pitch_enabled: Arc<AtomicBool>,
    pub pitch_pitch: Arc<AtomicF32>,
    pub pitch_mix: Arc<AtomicF32>,
    pub pitch_tone: Arc<AtomicF32>,

    // Auto-wah (after the whammy, before the compressor)
    pub wah_enabled: Arc<AtomicBool>,
    pub wah_freq: Arc<AtomicF32>,
    pub wah_sens: Arc<AtomicF32>,
    pub wah_q: Arc<AtomicF32>,
    pub wah_mix: Arc<AtomicF32>,

    // Pre-amp EQ (before the amp)
    pub peq_enabled: Arc<AtomicBool>,
    pub peq_low: Arc<AtomicF32>,
    pub peq_mid: Arc<AtomicF32>,
    pub peq_high: Arc<AtomicF32>,

    // Uni-Vibe (mono, last pedal before the amp — guitar → fuzz → vibe → amp).
    pub uv_enabled: Arc<AtomicBool>,
    pub uv_rate: Arc<AtomicF32>,
    pub uv_depth: Arc<AtomicF32>,
    pub uv_mix: Arc<AtomicF32>,
    pub uv_mode: Arc<AtomicF32>,

    // Clean boost (linear Power-Boost-style front-end gain, before the amp)
    pub boost_enabled: Arc<AtomicBool>,
    pub boost_gain: Arc<AtomicF32>,
    pub boost_treble: Arc<AtomicF32>,
    pub boost_bass: Arc<AtomicF32>,

    // Fuzz (Big Muff style)
    pub fz_enabled: Arc<AtomicBool>,
    pub fz_fuzz: Arc<AtomicF32>,
    pub fz_tone: Arc<AtomicF32>,
    pub fz_level: Arc<AtomicF32>,
    pub fz_type: Arc<AtomicF32>,

    // TS-808
    pub ts_enabled: Arc<AtomicBool>,
    pub ts_drive: Arc<AtomicF32>,
    pub ts_tone: Arc<AtomicF32>,
    pub ts_level: Arc<AtomicF32>,

    // Boss DS-1 Distortion
    pub ds_enabled: Arc<AtomicBool>,
    pub ds_drive: Arc<AtomicF32>,
    pub ds_tone: Arc<AtomicF32>,
    pub ds_level: Arc<AtomicF32>,

    // Boss ML-2 Metal Core (high-gain distortion, after the DS-1)
    pub ml_enabled: Arc<AtomicBool>,
    pub ml_dist: Arc<AtomicF32>,
    pub ml_low: Arc<AtomicF32>,
    pub ml_high: Arc<AtomicF32>,
    pub ml_level: Arc<AtomicF32>,

    // Reverb
    pub rev_enabled: Arc<AtomicBool>,
    pub rev_room: Arc<AtomicF32>,
    pub rev_damp: Arc<AtomicF32>,
    pub rev_mix: Arc<AtomicF32>,

    // Graphic EQ (Boss GE-7, post-cab rack, before the parametric EQ)
    pub geq_enabled: Arc<AtomicBool>,
    pub geq_b1: Arc<AtomicF32>,
    pub geq_b2: Arc<AtomicF32>,
    pub geq_b3: Arc<AtomicF32>,
    pub geq_b4: Arc<AtomicF32>,
    pub geq_b5: Arc<AtomicF32>,
    pub geq_b6: Arc<AtomicF32>,
    pub geq_b7: Arc<AtomicF32>,
    pub geq_level: Arc<AtomicF32>,

    // Parametric EQ
    pub eq_enabled: Arc<AtomicBool>,
    pub eq_low: Arc<AtomicF32>,
    pub eq_mid: Arc<AtomicF32>,
    pub eq_high: Arc<AtomicF32>,

    // Delay
    pub delay_enabled: Arc<AtomicBool>,
    pub delay_time: Arc<AtomicF32>,
    pub delay_feedback: Arc<AtomicF32>,
    pub delay_mix: Arc<AtomicF32>,
    pub delay_type: Arc<AtomicF32>,

    // Flanger (stereo rack, post-cab modulation)
    pub fl_enabled: Arc<AtomicBool>,
    pub fl_rate: Arc<AtomicF32>,
    pub fl_depth: Arc<AtomicF32>,
    pub fl_feedback: Arc<AtomicF32>,
    pub fl_mix: Arc<AtomicF32>,

    // Chorus (stereo rack, post-cab modulation, after the flanger)
    pub ch_enabled: Arc<AtomicBool>,
    pub ch_rate: Arc<AtomicF32>,
    pub ch_depth: Arc<AtomicF32>,
    pub ch_mix: Arc<AtomicF32>,

    // Phaser (stereo rack, post-cab modulation, after the chorus)
    pub ph_enabled: Arc<AtomicBool>,
    pub ph_rate: Arc<AtomicF32>,
    pub ph_depth: Arc<AtomicF32>,
    pub ph_feedback: Arc<AtomicF32>,
    pub ph_mix: Arc<AtomicF32>,

    // Tremolo / Vibrato (stereo rack, post-cab modulation, after the phaser)
    pub trem_enabled: Arc<AtomicBool>,
    pub trem_rate: Arc<AtomicF32>,
    pub trem_depth: Arc<AtomicF32>,
    pub trem_shape: Arc<AtomicF32>,
    pub trem_mode: Arc<AtomicF32>,

    // Amp front-panel controls, one bank per model so switching amps preserves
    // each model's own knob positions (the active model's descriptor — see
    // `AmpModel::controls` — gives the order, labels and defaults). Models expose
    // different numbers of controls, so trailing slots of a bank are unused.
    pub amp_params: [[Arc<AtomicF32>; AMP_MAX]; AmpModel::ALL.len()],

    // Reorderable signal-chain order, one [`ChainStage`] id per slot, published
    // as a **seqlock** so the audio thread always sees a whole, coherent order.
    // `chain_seq` guards the slot array: writers bump it odd, write the slots,
    // then bump it even; readers snapshot the slots and accept the read only if
    // the sequence is unchanged and even. This closes the old window where a
    // concurrent swap could be observed half-applied (a stage duplicated or
    // missing). The audio thread reads once per block via the **bounded**
    // [`Params::try_chain_slots`] (never spinning on a preempted writer) and keeps
    // its last good order; the blocking [`Params::chain_slots`] is control-thread
    // only.
    pub chain_order: Arc<[AtomicU8; CHAIN_LEN]>,
    pub chain_seq: Arc<AtomicU64>,
    /// Serializes *writers* of the chain order so the seqlock stays single-writer.
    /// Only [`Params::set_chain_order`] takes it; the audio thread never touches it.
    chain_write: std::sync::Mutex<()>,
}

/// True when `order` contains every [`ChainStage`] id exactly once (and nothing
/// else). Allocation-free, so it can guard [`Params::set_chain_order`]'s
/// `debug_assert` on the control thread.
fn is_chain_permutation(order: &[u8; CHAIN_LEN]) -> bool {
    let mut seen = [false; CHAIN_LEN];
    for &v in order {
        match ChainStage::from_u8(v) {
            Some(s) => {
                if std::mem::replace(&mut seen[s as usize], true) {
                    return false;
                }
            }
            None => return false,
        }
    }
    seen.iter().all(|&b| b)
}

impl Default for Params {
    fn default() -> Self {
        Self::new()
    }
}

impl Params {
    pub fn new() -> Self {
        macro_rules! p {
            ($v:expr) => {
                Arc::new(AtomicF32::new($v))
            };
        }
        macro_rules! b {
            ($v:expr) => {
                Arc::new(AtomicBool::new($v))
            };
        }
        Self {
            amp_model: Arc::new(AtomicU8::new(DEFAULT_AMP_MODEL)),
            cab_model: Arc::new(AtomicU8::new(DEFAULT_CAB_MODEL)),
            mic_pos: p!(DEFAULT_MIC_POS),
            mic_blend: p!(DEFAULT_MIC_BLEND),
            mic_room: p!(DEFAULT_MIC_ROOM),
            master_width: p!(DEFAULT_MASTER_WIDTH),
            cab_external_active: b!(DEFAULT_CAB_EXTERNAL_ACTIVE),
            cab_external_loaded: b!(false),

            amp_external_active: b!(DEFAULT_AMP_EXTERNAL_ACTIVE),
            amp_external_loaded: b!(false),
            amp_external_amp_only: b!(false),
            amp_external_latency: Arc::new(AtomicUsize::new(0)),

            ng_enabled: b!(DEFAULT_NG_ENABLED),
            ng_threshold: p!(DEFAULT_NG_THRESHOLD),
            ng_release: p!(DEFAULT_NG_RELEASE),

            cmp_enabled: b!(DEFAULT_CMP_ENABLED),
            cmp_sustain: p!(DEFAULT_CMP_SUSTAIN),
            cmp_attack: p!(DEFAULT_CMP_ATTACK),
            cmp_level: p!(DEFAULT_CMP_LEVEL),

            pitch_enabled: b!(DEFAULT_PITCH_ENABLED),
            pitch_pitch: p!(DEFAULT_PITCH_PITCH),
            pitch_mix: p!(DEFAULT_PITCH_MIX),
            pitch_tone: p!(DEFAULT_PITCH_TONE),

            wah_enabled: b!(DEFAULT_WAH_ENABLED),
            wah_freq: p!(DEFAULT_WAH_FREQ),
            wah_sens: p!(DEFAULT_WAH_SENS),
            wah_q: p!(DEFAULT_WAH_Q),
            wah_mix: p!(DEFAULT_WAH_MIX),

            peq_enabled: b!(DEFAULT_PEQ_ENABLED),
            peq_low: p!(DEFAULT_PEQ_LOW),
            peq_mid: p!(DEFAULT_PEQ_MID),
            peq_high: p!(DEFAULT_PEQ_HIGH),

            uv_enabled: b!(DEFAULT_UV_ENABLED),
            uv_rate: p!(DEFAULT_UV_RATE),
            uv_depth: p!(DEFAULT_UV_DEPTH),
            uv_mix: p!(DEFAULT_UV_MIX),
            uv_mode: p!(DEFAULT_UV_MODE),

            boost_enabled: b!(DEFAULT_BOOST_ENABLED),
            boost_gain: p!(DEFAULT_BOOST_GAIN),
            boost_treble: p!(DEFAULT_BOOST_TREBLE),
            boost_bass: p!(DEFAULT_BOOST_BASS),

            fz_enabled: b!(DEFAULT_FZ_ENABLED),
            fz_fuzz: p!(DEFAULT_FZ_FUZZ),
            fz_tone: p!(DEFAULT_FZ_TONE),
            fz_level: p!(DEFAULT_FZ_LEVEL),
            fz_type: p!(DEFAULT_FZ_TYPE),

            ts_enabled: b!(DEFAULT_TS_ENABLED),
            ts_drive: p!(DEFAULT_TS_DRIVE),
            ts_tone: p!(DEFAULT_TS_TONE),
            ts_level: p!(DEFAULT_TS_LEVEL),

            ds_enabled: b!(DEFAULT_DS_ENABLED),
            ds_drive: p!(DEFAULT_DS_DRIVE),
            ds_tone: p!(DEFAULT_DS_TONE),
            ds_level: p!(DEFAULT_DS_LEVEL),

            ml_enabled: b!(DEFAULT_ML_ENABLED),
            ml_dist: p!(DEFAULT_ML_DIST),
            ml_low: p!(DEFAULT_ML_LOW),
            ml_high: p!(DEFAULT_ML_HIGH),
            ml_level: p!(DEFAULT_ML_LEVEL),

            rev_enabled: b!(DEFAULT_REV_ENABLED),
            rev_room: p!(DEFAULT_REV_ROOM),
            rev_damp: p!(DEFAULT_REV_DAMP),
            rev_mix: p!(DEFAULT_REV_MIX),

            geq_enabled: b!(DEFAULT_GEQ_ENABLED),
            geq_b1: p!(DEFAULT_GEQ_BAND),
            geq_b2: p!(DEFAULT_GEQ_BAND),
            geq_b3: p!(DEFAULT_GEQ_BAND),
            geq_b4: p!(DEFAULT_GEQ_BAND),
            geq_b5: p!(DEFAULT_GEQ_BAND),
            geq_b6: p!(DEFAULT_GEQ_BAND),
            geq_b7: p!(DEFAULT_GEQ_BAND),
            geq_level: p!(DEFAULT_GEQ_LEVEL),

            eq_enabled: b!(DEFAULT_EQ_ENABLED),
            eq_low: p!(DEFAULT_EQ_LOW),
            eq_mid: p!(DEFAULT_EQ_MID),
            eq_high: p!(DEFAULT_EQ_HIGH),

            delay_enabled: b!(DEFAULT_DELAY_ENABLED),
            delay_time: p!(DEFAULT_DELAY_TIME),
            delay_feedback: p!(DEFAULT_DELAY_FEEDBACK),
            delay_mix: p!(DEFAULT_DELAY_MIX),
            delay_type: p!(DEFAULT_DELAY_TYPE),

            fl_enabled: b!(DEFAULT_FL_ENABLED),
            fl_rate: p!(DEFAULT_FL_RATE),
            fl_depth: p!(DEFAULT_FL_DEPTH),
            fl_feedback: p!(DEFAULT_FL_FEEDBACK),
            fl_mix: p!(DEFAULT_FL_MIX),

            ch_enabled: b!(DEFAULT_CH_ENABLED),
            ch_rate: p!(DEFAULT_CH_RATE),
            ch_depth: p!(DEFAULT_CH_DEPTH),
            ch_mix: p!(DEFAULT_CH_MIX),

            ph_enabled: b!(DEFAULT_PH_ENABLED),
            ph_rate: p!(DEFAULT_PH_RATE),
            ph_depth: p!(DEFAULT_PH_DEPTH),
            ph_feedback: p!(DEFAULT_PH_FEEDBACK),
            ph_mix: p!(DEFAULT_PH_MIX),

            trem_enabled: b!(DEFAULT_TREM_ENABLED),
            trem_rate: p!(DEFAULT_TREM_RATE),
            trem_depth: p!(DEFAULT_TREM_DEPTH),
            trem_shape: p!(DEFAULT_TREM_SHAPE),
            trem_mode: p!(DEFAULT_TREM_MODE),

            amp_params: std::array::from_fn(|m| {
                let model = AmpModel::ALL[m];
                std::array::from_fn(|i| {
                    let default = model.controls().get(i).map_or(0.0, |k| k.default);
                    p!(default)
                })
            }),

            chain_order: Arc::new(ChainStage::default_order().map(AtomicU8::new)),
            chain_seq: Arc::new(AtomicU64::new(0)),
            chain_write: std::sync::Mutex::new(()),
        }
    }

    pub fn reset_to_defaults(&self) {
        self.amp_model.store(DEFAULT_AMP_MODEL, Relaxed);
        self.cab_model.store(DEFAULT_CAB_MODEL, Relaxed);
        self.mic_pos.store(DEFAULT_MIC_POS, Relaxed);
        self.mic_blend.store(DEFAULT_MIC_BLEND, Relaxed);
        self.mic_room.store(DEFAULT_MIC_ROOM, Relaxed);
        self.master_width.store(DEFAULT_MASTER_WIDTH, Relaxed);
        // Fall back to the built-in cab. The loaded IR (if any) stays installed in
        // the chain — only its active/inactive selection is a default-able param.
        self.cab_external_active
            .store(DEFAULT_CAB_EXTERNAL_ACTIVE, Relaxed);
        // Fall back to the built-in amp and its cab pairing. The loaded AU (if any)
        // stays installed — only the active selection and amp-only routing are reset;
        // the AU's reported latency is intrinsic to the loaded plugin, so it is left.
        self.amp_external_active
            .store(DEFAULT_AMP_EXTERNAL_ACTIVE, Relaxed);
        self.amp_external_amp_only.store(false, Relaxed);

        self.ng_enabled.store(DEFAULT_NG_ENABLED, Relaxed);
        self.ng_threshold.store(DEFAULT_NG_THRESHOLD, Relaxed);
        self.ng_release.store(DEFAULT_NG_RELEASE, Relaxed);

        self.cmp_enabled.store(DEFAULT_CMP_ENABLED, Relaxed);
        self.cmp_sustain.store(DEFAULT_CMP_SUSTAIN, Relaxed);
        self.cmp_attack.store(DEFAULT_CMP_ATTACK, Relaxed);
        self.cmp_level.store(DEFAULT_CMP_LEVEL, Relaxed);

        self.pitch_enabled.store(DEFAULT_PITCH_ENABLED, Relaxed);
        self.pitch_pitch.store(DEFAULT_PITCH_PITCH, Relaxed);
        self.pitch_mix.store(DEFAULT_PITCH_MIX, Relaxed);
        self.pitch_tone.store(DEFAULT_PITCH_TONE, Relaxed);

        self.wah_enabled.store(DEFAULT_WAH_ENABLED, Relaxed);
        self.wah_freq.store(DEFAULT_WAH_FREQ, Relaxed);
        self.wah_sens.store(DEFAULT_WAH_SENS, Relaxed);
        self.wah_q.store(DEFAULT_WAH_Q, Relaxed);
        self.wah_mix.store(DEFAULT_WAH_MIX, Relaxed);

        self.peq_enabled.store(DEFAULT_PEQ_ENABLED, Relaxed);
        self.peq_low.store(DEFAULT_PEQ_LOW, Relaxed);
        self.peq_mid.store(DEFAULT_PEQ_MID, Relaxed);
        self.peq_high.store(DEFAULT_PEQ_HIGH, Relaxed);

        self.uv_enabled.store(DEFAULT_UV_ENABLED, Relaxed);
        self.uv_rate.store(DEFAULT_UV_RATE, Relaxed);
        self.uv_depth.store(DEFAULT_UV_DEPTH, Relaxed);
        self.uv_mix.store(DEFAULT_UV_MIX, Relaxed);
        self.uv_mode.store(DEFAULT_UV_MODE, Relaxed);

        self.boost_enabled.store(DEFAULT_BOOST_ENABLED, Relaxed);
        self.boost_gain.store(DEFAULT_BOOST_GAIN, Relaxed);
        self.boost_treble.store(DEFAULT_BOOST_TREBLE, Relaxed);
        self.boost_bass.store(DEFAULT_BOOST_BASS, Relaxed);

        self.fz_enabled.store(DEFAULT_FZ_ENABLED, Relaxed);
        self.fz_fuzz.store(DEFAULT_FZ_FUZZ, Relaxed);
        self.fz_tone.store(DEFAULT_FZ_TONE, Relaxed);
        self.fz_level.store(DEFAULT_FZ_LEVEL, Relaxed);
        self.fz_type.store(DEFAULT_FZ_TYPE, Relaxed);

        self.ts_enabled.store(DEFAULT_TS_ENABLED, Relaxed);
        self.ts_drive.store(DEFAULT_TS_DRIVE, Relaxed);
        self.ts_tone.store(DEFAULT_TS_TONE, Relaxed);
        self.ts_level.store(DEFAULT_TS_LEVEL, Relaxed);

        self.ds_enabled.store(DEFAULT_DS_ENABLED, Relaxed);
        self.ds_drive.store(DEFAULT_DS_DRIVE, Relaxed);
        self.ds_tone.store(DEFAULT_DS_TONE, Relaxed);
        self.ds_level.store(DEFAULT_DS_LEVEL, Relaxed);

        self.ml_enabled.store(DEFAULT_ML_ENABLED, Relaxed);
        self.ml_dist.store(DEFAULT_ML_DIST, Relaxed);
        self.ml_low.store(DEFAULT_ML_LOW, Relaxed);
        self.ml_high.store(DEFAULT_ML_HIGH, Relaxed);
        self.ml_level.store(DEFAULT_ML_LEVEL, Relaxed);

        self.rev_enabled.store(DEFAULT_REV_ENABLED, Relaxed);
        self.rev_room.store(DEFAULT_REV_ROOM, Relaxed);
        self.rev_damp.store(DEFAULT_REV_DAMP, Relaxed);
        self.rev_mix.store(DEFAULT_REV_MIX, Relaxed);

        self.geq_enabled.store(DEFAULT_GEQ_ENABLED, Relaxed);
        self.geq_b1.store(DEFAULT_GEQ_BAND, Relaxed);
        self.geq_b2.store(DEFAULT_GEQ_BAND, Relaxed);
        self.geq_b3.store(DEFAULT_GEQ_BAND, Relaxed);
        self.geq_b4.store(DEFAULT_GEQ_BAND, Relaxed);
        self.geq_b5.store(DEFAULT_GEQ_BAND, Relaxed);
        self.geq_b6.store(DEFAULT_GEQ_BAND, Relaxed);
        self.geq_b7.store(DEFAULT_GEQ_BAND, Relaxed);
        self.geq_level.store(DEFAULT_GEQ_LEVEL, Relaxed);

        self.eq_enabled.store(DEFAULT_EQ_ENABLED, Relaxed);
        self.eq_low.store(DEFAULT_EQ_LOW, Relaxed);
        self.eq_mid.store(DEFAULT_EQ_MID, Relaxed);
        self.eq_high.store(DEFAULT_EQ_HIGH, Relaxed);

        self.delay_enabled.store(DEFAULT_DELAY_ENABLED, Relaxed);
        self.delay_time.store(DEFAULT_DELAY_TIME, Relaxed);
        self.delay_feedback.store(DEFAULT_DELAY_FEEDBACK, Relaxed);
        self.delay_mix.store(DEFAULT_DELAY_MIX, Relaxed);
        self.delay_type.store(DEFAULT_DELAY_TYPE, Relaxed);

        self.fl_enabled.store(DEFAULT_FL_ENABLED, Relaxed);
        self.fl_rate.store(DEFAULT_FL_RATE, Relaxed);
        self.fl_depth.store(DEFAULT_FL_DEPTH, Relaxed);
        self.fl_feedback.store(DEFAULT_FL_FEEDBACK, Relaxed);
        self.fl_mix.store(DEFAULT_FL_MIX, Relaxed);

        self.ch_enabled.store(DEFAULT_CH_ENABLED, Relaxed);
        self.ch_rate.store(DEFAULT_CH_RATE, Relaxed);
        self.ch_depth.store(DEFAULT_CH_DEPTH, Relaxed);
        self.ch_mix.store(DEFAULT_CH_MIX, Relaxed);

        self.ph_enabled.store(DEFAULT_PH_ENABLED, Relaxed);
        self.ph_rate.store(DEFAULT_PH_RATE, Relaxed);
        self.ph_depth.store(DEFAULT_PH_DEPTH, Relaxed);
        self.ph_feedback.store(DEFAULT_PH_FEEDBACK, Relaxed);
        self.ph_mix.store(DEFAULT_PH_MIX, Relaxed);

        self.trem_enabled.store(DEFAULT_TREM_ENABLED, Relaxed);
        self.trem_rate.store(DEFAULT_TREM_RATE, Relaxed);
        self.trem_depth.store(DEFAULT_TREM_DEPTH, Relaxed);
        self.trem_shape.store(DEFAULT_TREM_SHAPE, Relaxed);
        self.trem_mode.store(DEFAULT_TREM_MODE, Relaxed);

        for model in AmpModel::ALL {
            for (i, knob) in model.controls().iter().enumerate() {
                self.amp_params[model as usize][i].store(knob.default, Relaxed);
            }
        }

        self.set_chain_order(&ChainStage::default_order());
    }

    pub fn amp_model(&self) -> AmpModel {
        AmpModel::from_u8(self.amp_model.load(Relaxed))
    }

    pub fn cab_model(&self) -> CabModel {
        CabModel::from_u8(self.cab_model.load(Relaxed))
    }

    /// Number of front-panel knobs the active amp model exposes.
    pub fn amp_knob_count(&self) -> usize {
        self.amp_model().knob_count()
    }

    /// The shared atomic backing `model`'s `i`-th front-panel knob.
    pub fn amp_knob(&self, model: AmpModel, i: usize) -> &Arc<AtomicF32> {
        &self.amp_params[model as usize][i]
    }

    /// Load `model`'s `i`-th front-panel knob (0–1).
    pub fn amp_knob_value(&self, model: AmpModel, i: usize) -> f32 {
        self.amp_params[model as usize][i].load(Relaxed)
    }

    /// Store `model`'s `i`-th front-panel knob, clamped to 0–1.
    pub fn set_amp_knob(&self, model: AmpModel, i: usize, v: f32) {
        self.amp_params[model as usize][i].store(v.clamp(0.0, 1.0), Relaxed);
    }

    /// Whether `stage` is currently enabled. The amp and cab stages have no
    /// bypass flag and are always live; every other stage mirrors its
    /// `*_enabled` atomic.
    ///
    /// The ordered dispatch consults this *before* any mono↔stereo bridging so a
    /// bypassed stage is wire-transparent in either domain.
    pub fn stage_enabled(&self, stage: ChainStage) -> bool {
        match stage {
            ChainStage::Gate => self.ng_enabled.load(Relaxed),
            ChainStage::Whammy => self.pitch_enabled.load(Relaxed),
            ChainStage::Wah => self.wah_enabled.load(Relaxed),
            ChainStage::Comp => self.cmp_enabled.load(Relaxed),
            ChainStage::Fuzz => self.fz_enabled.load(Relaxed),
            ChainStage::Ts => self.ts_enabled.load(Relaxed),
            ChainStage::Ds => self.ds_enabled.load(Relaxed),
            ChainStage::Metal => self.ml_enabled.load(Relaxed),
            ChainStage::PreEq => self.peq_enabled.load(Relaxed),
            ChainStage::Vibe => self.uv_enabled.load(Relaxed),
            ChainStage::Boost => self.boost_enabled.load(Relaxed),
            // The amp and cab have no bypass flag: the amp always runs, and the
            // cab is skipped only by the dispatch when a full-rig AU supplies it.
            ChainStage::Amp | ChainStage::Cab => true,
            ChainStage::Geq => self.geq_enabled.load(Relaxed),
            ChainStage::Eq => self.eq_enabled.load(Relaxed),
            ChainStage::Flanger => self.fl_enabled.load(Relaxed),
            ChainStage::Chorus => self.ch_enabled.load(Relaxed),
            ChainStage::Phaser => self.ph_enabled.load(Relaxed),
            ChainStage::Trem => self.trem_enabled.load(Relaxed),
            ChainStage::Delay => self.delay_enabled.load(Relaxed),
            ChainStage::Reverb => self.rev_enabled.load(Relaxed),
        }
    }

    /// Audio-thread reader: at most [`CHAIN_READ_ATTEMPTS`] tries, never waits for
    /// a writer. `None` means "a write is in flight — keep your last order".
    ///
    /// Call this once per audio block (or per `process` call), never per sample.
    pub fn try_chain_slots(&self) -> Option<[u8; CHAIN_LEN]> {
        for _ in 0..CHAIN_READ_ATTEMPTS {
            let before = self.chain_seq.load(SeqCst);
            if before & 1 != 0 {
                std::hint::spin_loop();
                continue;
            }
            let slots = std::array::from_fn(|i| self.chain_order[i].load(SeqCst));
            let after = self.chain_seq.load(SeqCst);
            if before == after {
                return Some(slots);
            }
            std::hint::spin_loop();
        }
        None
    }

    /// Blocking snapshot of the whole chain order as one coherent value.
    ///
    /// **Control-thread only** (UI drawing, preset save, tests): it loops until a
    /// clean read. The audio thread must use [`try_chain_slots`](Self::try_chain_slots)
    /// instead, so a preempted writer can never stall the callback.
    pub fn chain_slots(&self) -> [u8; CHAIN_LEN] {
        loop {
            if let Some(slots) = self.try_chain_slots() {
                return slots;
            }
        }
    }

    /// Install a full chain order (UI move / preset apply), publishing it
    /// atomically through the seqlock. The caller must pass a sanitized
    /// permutation containing every stage exactly once (see
    /// [`sanitize_chain_order`]); a `debug_assert` enforces that in debug builds,
    /// and unknown ids are skipped rather than stored.
    pub fn set_chain_order(&self, order: &[u8; CHAIN_LEN]) {
        debug_assert!(
            is_chain_permutation(order),
            "set_chain_order requires a permutation containing every stage exactly once"
        );
        // Serialize writers so the seqlock stays single-writer. The audio thread
        // never takes this lock.
        let _guard = self
            .chain_write
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // Odd sequence: the slots are now mid-write and must not be read.
        self.chain_seq.fetch_add(1, SeqCst);
        for (slot, &v) in self.chain_order.iter().zip(order.iter()) {
            if ChainStage::from_u8(v).is_some() {
                slot.store(v, SeqCst);
            }
        }
        // Even again: the new order is fully published.
        self.chain_seq.fetch_add(1, SeqCst);
    }
}

/// Accessor for the UI knob table: the active model's `I`-th front-panel knob.
/// Const-generic so each amp slot can be a plain `fn(&Params) -> &Arc<AtomicF32>`
/// in the static `KNOBS` table even though the backing bank is model-dependent.
pub fn amp_param<const I: usize>(p: &Params) -> &Arc<AtomicF32> {
    &p.amp_params[p.amp_model.load(Relaxed) as usize][I]
}

pub struct Levels {
    pub input: Arc<AtomicF32>,
    pub output: Arc<AtomicF32>,
}

impl Default for Levels {
    fn default() -> Self {
        Self::new()
    }
}

impl Levels {
    pub fn new() -> Self {
        Self {
            input: Arc::new(AtomicF32::new(0.0)),
            output: Arc::new(AtomicF32::new(0.0)),
        }
    }
}

/// A stereo insert effect processed one block at a time, in place.
///
/// The built-in [`DspChain`] is hardwired, but this is the single extension point
/// third-party plugins hang off: a stereo slot after the cab/rack, before the
/// master bus. CLAP plugins are bridged to this trait by the `host` module. Must
/// be `Send` so an instance built on the UI thread can be handed to the audio
/// thread for processing.
pub trait StereoInsert: Send {
    /// Process one block in place. `left` and `right` always have equal length.
    fn process_block(&mut self, left: &mut [f32], right: &mut [f32]);
}

/// Run a mono effect when its enable flag is set, otherwise pass `$x` through.
///
/// Every pedal in the chain is bypassable and reads its knobs from the shared,
/// lock-free [`Params`]. Spelling that `if enabled { effect.process(load…) } else
/// { x }` dance out once per pedal was the bulk of the old `process` body; this
/// macro expands to the exact same straight-line code (no allocation, no dynamic
/// dispatch) so the audio thread pays nothing for the deduplication.
macro_rules! mono_stage {
    ($self:ident, $p:ident, $x:ident, $enabled:ident, $field:ident, $($param:ident),+) => {
        if $p.$enabled.load(Relaxed) {
            $self.$field.process($x, $($p.$param.load(Relaxed)),+)
        } else {
            $x
        }
    };
}

/// Stereo counterpart of [`mono_stage!`]: passes `($l, $r)` through when bypassed.
macro_rules! stereo_stage {
    ($self:ident, $p:ident, $l:ident, $r:ident, $enabled:ident, $field:ident, $($param:ident),+) => {
        if $p.$enabled.load(Relaxed) {
            $self.$field.process($l, $r, $($p.$param.load(Relaxed)),+)
        } else {
            ($l, $r)
        }
    };
}

/// Signal domain while walking the ordered chain: everything starts mono and
/// becomes stereo at the cab stage (or at the first stereo rack pedal placed
/// before it). A stereo pedal before the amp is folded back to mono by the amp.
#[derive(Clone, Copy)]
enum Sig {
    Mono(f32),
    Stereo(f32, f32),
}

impl Sig {
    fn into_mono(self) -> f32 {
        match self {
            Sig::Mono(x) => x,
            Sig::Stereo(l, r) => 0.5 * (l + r),
        }
    }

    fn into_stereo(self) -> (f32, f32) {
        match self {
            Sig::Mono(x) => (x, x),
            Sig::Stereo(l, r) => (l, r),
        }
    }
}

/// Everything routing-related, read **once** per block (or per `process` call)
/// so a toggle landing mid-block cannot change the topology partway through.
///
/// In particular the [`ChainStage::Cab`] decision is baked into `skip_cab` here,
/// instead of re-reading the AU flags per sample (the R2 defect).
#[derive(Clone, Copy)]
struct BlockRoute {
    /// The coherent chain order for this block.
    order: [u8; CHAIN_LEN],
    /// The hosted AU replaces the built-in amp for this block.
    use_ext_amp: bool,
    /// The separate Cab stage is skipped (a live full-rig AU supplies it).
    skip_cab: bool,
    /// Master-bus width for this block.
    width: f32,
}

pub struct DspChain {
    ng: NoiseGate,
    pitch: Pitch,
    wah: Wah,
    cmp: Compressor,
    fz: Fuzz,
    ts: TubeScreamer,
    ds: Distortion,
    ml: MetalCore,
    peq: PreampEq,
    uv: UniVibe,
    boost: CleanBoost,
    amp: AmpBank,
    cab: CabBank,
    geq: GraphicEq,
    eq: ParametricEq,
    flanger: Flanger,
    chorus: Chorus,
    phaser: Phaser,
    tremolo: Tremolo,
    delay: Delay,
    reverb: Reverb,
    params: Arc<Params>,
    /// Optional third-party stereo insert (e.g. a hosted CLAP plugin), placed
    /// after the stereo rack and before the master bus. `None` = passthrough.
    insert: Option<Box<dyn StereoInsert>>,
    /// Optional user-loaded external-IR cab. When present *and*
    /// `params.cab_external_active`, it replaces the built-in [`CabBank`] at the cab
    /// stage. Built off the audio thread and swapped in lock-free, like `insert`.
    ext_cab: Option<Box<ExternalIrCab>>,
    /// Optional user-loaded external amp (a hosted AU amp sim). When present *and*
    /// `params.amp_external_active`, it replaces the built-in amp (and, unless the AU is
    /// flagged amp-only, the cab too): the mono pre-amp signal is fed to it (duplicated
    /// to stereo) and its output goes to the rack. A [`StereoInsert`] like `insert`,
    /// swapped in lock-free.
    ext_amp: Option<Box<dyn StereoInsert>>,
    /// Delays the built-in amp path by the loaded AU's reported latency so switching
    /// built-in↔AU stays time-coherent. Only engaged while an AU is loaded.
    comp_delay: CompDelay,
    /// Last chain order this chain read cleanly. The audio thread keeps this and
    /// falls back to it when a writer is mid-publish (see [`Self::snapshot_order`]),
    /// so it never waits on the control/UI thread.
    last_order: [u8; CHAIN_LEN],
}

impl DspChain {
    pub fn new(sr: f32, params: Arc<Params>) -> Self {
        let last_order = params.chain_slots();
        Self {
            ng: NoiseGate::new(sr),
            pitch: Pitch::new(sr),
            wah: Wah::new(sr),
            cmp: Compressor::new(sr),
            fz: Fuzz::new(sr),
            ts: TubeScreamer::new(sr),
            ds: Distortion::new(sr),
            ml: MetalCore::new(sr),
            peq: PreampEq::new(sr),
            uv: UniVibe::new(sr),
            boost: CleanBoost::new(sr),
            amp: AmpBank::new(sr),
            cab: CabBank::new(sr),
            geq: GraphicEq::new(sr),
            eq: ParametricEq::new(sr),
            flanger: Flanger::new(sr),
            chorus: Chorus::new(sr),
            phaser: Phaser::new(sr),
            tremolo: Tremolo::new(sr),
            delay: Delay::new(sr),
            reverb: Reverb::new(sr),
            params,
            insert: None,
            ext_cab: None,
            ext_amp: None,
            // Cap the compensation delay at 1 s — far beyond any real plugin latency.
            comp_delay: CompDelay::new(sr as usize),
            last_order,
        }
    }

    /// Install (or clear, with `None`) the stereo plugin insert.
    pub fn set_insert(&mut self, insert: Option<Box<dyn StereoInsert>>) {
        self.insert = insert;
    }

    /// Install (or clear, with `None`) the external amp override.
    pub fn set_ext_amp(&mut self, amp: Option<Box<dyn StereoInsert>>) {
        self.ext_amp = amp;
    }

    /// Swap the external-IR cab, returning the displaced one (if any) for disposal
    /// off the audio thread. Same lock-free pointer-move discipline as
    /// [`replace_insert`](Self::replace_insert): the freed IR/FFT buffers must not be
    /// dropped in the realtime callback.
    #[must_use = "the displaced external cab must be dropped off the audio thread"]
    pub fn replace_external_cab(
        &mut self,
        cab: Option<Box<ExternalIrCab>>,
    ) -> Option<Box<ExternalIrCab>> {
        std::mem::replace(&mut self.ext_cab, cab)
    }

    /// Swap the external amp, returning the displaced one (if any) for disposal off the
    /// audio thread. Same lock-free pointer-move discipline as
    /// [`replace_insert`](Self::replace_insert): a hosted plugin must not be dropped in
    /// the realtime callback.
    #[must_use = "the displaced external amp must be dropped off the audio thread"]
    pub fn replace_ext_amp(
        &mut self,
        amp: Option<Box<dyn StereoInsert>>,
    ) -> Option<Box<dyn StereoInsert>> {
        std::mem::replace(&mut self.ext_amp, amp)
    }

    /// Swap the stereo plugin insert, returning the displaced one (if any).
    ///
    /// The swap itself is just a pointer move, so it is safe to call on the audio
    /// thread. The returned box must be **dropped elsewhere**: freeing a plugin
    /// (and the allocations it owns) on the audio thread would block it. The engine
    /// hands the old insert back to a non-audio thread for disposal.
    #[must_use = "the displaced insert must be dropped off the audio thread"]
    pub fn replace_insert(
        &mut self,
        insert: Option<Box<dyn StereoInsert>>,
    ) -> Option<Box<dyn StereoInsert>> {
        std::mem::replace(&mut self.insert, insert)
    }

    /// The built-in signal path up to (but not including) the master bus, in the
    /// user-configured [`ChainStage`] order, returning a stereo (L, R) pair.
    ///
    /// The signal starts mono; the amp stage folds whatever reaches it to mono,
    /// and the cab stage turns it stereo (or the first stereo rack pedal placed
    /// before them does); mono pedals on a stereo signal sum and duplicate back,
    /// stereo pedals on mono promote to dual mono.
    ///
    /// The plugin insert and the master-bus widen + soft-limit run *after* this; in
    /// the live block path they run in [`process_block`], while the per-sample
    /// [`process`](Self::process) wrapper applies the master bus directly.
    ///
    /// `order` is snapshotted by the caller so the whole chain is read once per
    /// block, not once per sample.
    #[inline]
    fn process_core(&mut self, sample: f32, route: &BlockRoute) -> (f32, f32) {
        self.run_full(sample, route)
    }

    /// Snapshot everything routing-related for one block/call. `allow_ext_amp` is
    /// `false` for the per-sample [`process`](Self::process) path, which never runs
    /// a hosted AU and therefore always keeps the built-in cab.
    #[inline]
    fn route_for_block(&mut self, allow_ext_amp: bool) -> BlockRoute {
        let order = self.snapshot_order();
        let p = &self.params;
        let active = p.amp_external_active.load(Relaxed);
        let amp_only = p.amp_external_amp_only.load(Relaxed);
        let use_ext_amp = allow_ext_amp && self.ext_amp.is_some() && active;
        BlockRoute {
            order,
            use_ext_amp,
            // A live full-rig AU supplies its own cab/mic, so the separate Cab
            // stage is skipped; an amp-only AU still feeds the built-in cab.
            skip_cab: use_ext_amp && !amp_only,
            width: p.master_width.load(Relaxed),
        }
    }

    /// The built-in amp gain/tone stage (mono → mono). The dispatch swaps in a
    /// hosted AU instead of calling this, but only in `process_block`; the
    /// per-sample [`process`](Self::process) path always runs this.
    #[inline]
    fn amp_stage(&mut self, x: f32) -> f32 {
        let p = &self.params;
        let model = p.amp_model();
        let knobs: [f32; AMP_MAX] =
            std::array::from_fn(|i| p.amp_params[model as usize][i].load(Relaxed));
        self.amp.process(model, x, &knobs)
    }

    /// The cabinet stage (mono → stereo). A loaded external IR overrides the built-in
    /// cab when active; otherwise the multi-mic blend renders (the external IR path
    /// ignores the mic knobs — the capture is already miked). Reused by the amp-only
    /// external-amp path, which feeds it the AU's (summed-to-mono) output.
    #[inline]
    fn cab_stage(&mut self, x: f32) -> (f32, f32) {
        let p = &self.params;
        match self.ext_cab.as_mut() {
            Some(ext) if p.cab_external_active.load(Relaxed) => {
                use cab::Cabinet;
                ext.process(x, 0.0, 0.0, 0.0)
            }
            _ => self.cab.process(
                p.cab_model(),
                x,
                p.mic_pos.load(Relaxed),
                p.mic_blend.load(Relaxed),
                p.mic_room.load(Relaxed),
            ),
        }
    }

    /// Position of `stage` in `order`, falling back to a sane default slot when
    /// absent (sanitized orders always contain every stage exactly once).
    fn stage_index(order: &[u8; CHAIN_LEN], stage: ChainStage, fallback: usize) -> usize {
        order
            .iter()
            .position(|&v| v == stage as u8)
            .unwrap_or(fallback)
    }

    /// One mono pre pedal at `stage` (`Amp`/`Cab` and rack stages must not reach here).
    #[inline]
    fn run_mono_stage(&mut self, stage: ChainStage, x: f32) -> f32 {
        let p = &self.params;
        match stage {
            ChainStage::Gate => mono_stage!(self, p, x, ng_enabled, ng, ng_threshold, ng_release),
            ChainStage::Whammy => mono_stage!(
                self,
                p,
                x,
                pitch_enabled,
                pitch,
                pitch_pitch,
                pitch_mix,
                pitch_tone
            ),
            ChainStage::Wah => mono_stage!(
                self,
                p,
                x,
                wah_enabled,
                wah,
                wah_freq,
                wah_sens,
                wah_q,
                wah_mix
            ),
            ChainStage::Comp => mono_stage!(
                self,
                p,
                x,
                cmp_enabled,
                cmp,
                cmp_sustain,
                cmp_attack,
                cmp_level
            ),
            ChainStage::Fuzz => {
                mono_stage!(
                    self, p, x, fz_enabled, fz, fz_fuzz, fz_tone, fz_level, fz_type
                )
            }
            ChainStage::Ts => {
                mono_stage!(self, p, x, ts_enabled, ts, ts_drive, ts_tone, ts_level)
            }
            ChainStage::Ds => {
                mono_stage!(self, p, x, ds_enabled, ds, ds_drive, ds_tone, ds_level)
            }
            ChainStage::Metal => {
                mono_stage!(
                    self, p, x, ml_enabled, ml, ml_dist, ml_low, ml_high, ml_level
                )
            }
            ChainStage::PreEq => {
                mono_stage!(self, p, x, peq_enabled, peq, peq_low, peq_mid, peq_high)
            }
            ChainStage::Vibe => {
                mono_stage!(
                    self, p, x, uv_enabled, uv, uv_rate, uv_depth, uv_mix, uv_mode
                )
            }
            ChainStage::Boost => {
                mono_stage!(
                    self,
                    p,
                    x,
                    boost_enabled,
                    boost,
                    boost_gain,
                    boost_treble,
                    boost_bass
                )
            }
            _ => x,
        }
    }

    /// One stereo rack pedal at `stage` (the amp/cab stages and pre pedals pass through).
    #[inline]
    fn run_stereo_stage(&mut self, stage: ChainStage, l: f32, r: f32) -> (f32, f32) {
        let p = &self.params;
        match stage {
            ChainStage::Geq => stereo_stage!(
                self,
                p,
                l,
                r,
                geq_enabled,
                geq,
                geq_b1,
                geq_b2,
                geq_b3,
                geq_b4,
                geq_b5,
                geq_b6,
                geq_b7,
                geq_level
            ),
            ChainStage::Eq => {
                stereo_stage!(self, p, l, r, eq_enabled, eq, eq_low, eq_mid, eq_high)
            }
            ChainStage::Flanger => stereo_stage!(
                self,
                p,
                l,
                r,
                fl_enabled,
                flanger,
                fl_rate,
                fl_depth,
                fl_feedback,
                fl_mix
            ),
            ChainStage::Chorus => {
                stereo_stage!(self, p, l, r, ch_enabled, chorus, ch_rate, ch_depth, ch_mix)
            }
            ChainStage::Phaser => stereo_stage!(
                self,
                p,
                l,
                r,
                ph_enabled,
                phaser,
                ph_rate,
                ph_depth,
                ph_feedback,
                ph_mix
            ),
            ChainStage::Trem => stereo_stage!(
                self,
                p,
                l,
                r,
                trem_enabled,
                tremolo,
                trem_rate,
                trem_depth,
                trem_shape,
                trem_mode
            ),
            ChainStage::Delay => stereo_stage!(
                self,
                p,
                l,
                r,
                delay_enabled,
                delay,
                delay_time,
                delay_feedback,
                delay_mix,
                delay_type
            ),
            ChainStage::Reverb => {
                stereo_stage!(
                    self,
                    p,
                    l,
                    r,
                    rev_enabled,
                    reverb,
                    rev_room,
                    rev_damp,
                    rev_mix
                )
            }
            _ => (l, r),
        }
    }

    /// Walk one ordered stage with domain bridging: a mono pedal on a stereo
    /// signal sums to mono, processes, and duplicates back (like a real mono
    /// pedal fed from a stereo send); a stereo pedal on mono promotes to dual
    /// mono.
    ///
    /// [`ChainStage::Amp`] is the mono head: it always runs, forcing whatever
    /// domain reaches it down to mono first. [`ChainStage::Cab`] is the
    /// mono→stereo mic stage, skipped only when a full-rig external amp supplies
    /// its own cab/mic. Both have no bypass flag.
    ///
    /// A disabled pedal is **wire-transparent**: it returns the signal untouched
    /// in whatever domain it arrived, so moving a bypassed pedal never changes
    /// the stereo image (a mono pedal on a stereo feed must not collapse it, a
    /// stereo pedal on mono must not promote it). Only a *live* domain-crossing
    /// effect bridges domains.
    #[inline]
    fn run_ordered_stage(&mut self, sig: Sig, stage: ChainStage, route: &BlockRoute) -> Sig {
        match stage {
            ChainStage::Amp => return Sig::Mono(self.amp_stage(sig.into_mono())),
            ChainStage::Cab => {
                return if route.skip_cab {
                    sig
                } else {
                    let (l, r) = self.cab_stage(sig.into_mono());
                    Sig::Stereo(l, r)
                };
            }
            _ => {}
        }
        if !self.params.stage_enabled(stage) {
            return sig;
        }
        if stage.is_mono_pedal() {
            match sig {
                Sig::Mono(x) => Sig::Mono(self.run_mono_stage(stage, x)),
                Sig::Stereo(l, r) => {
                    let y = self.run_mono_stage(stage, 0.5 * (l + r));
                    Sig::Stereo(y, y)
                }
            }
        } else {
            match sig {
                Sig::Stereo(l, r) => {
                    let (l, r) = self.run_stereo_stage(stage, l, r);
                    Sig::Stereo(l, r)
                }
                Sig::Mono(x) => {
                    let (l, r) = self.run_stereo_stage(stage, x, x);
                    Sig::Stereo(l, r)
                }
            }
        }
    }

    /// Walk a slice of the order, applying each stage's domain rules. `route`
    /// carries the per-block Amp/Cab decisions so they are not re-read per sample.
    #[inline]
    fn run_range(&mut self, mut sig: Sig, order: &[u8], route: &BlockRoute) -> Sig {
        for &raw in order {
            if let Some(stage) = ChainStage::from_u8(raw) {
                sig = self.run_ordered_stage(sig, stage, route);
            }
        }
        sig
    }

    /// Full ordered chain, mono in → stereo out.
    #[inline]
    fn run_full(&mut self, sample: f32, route: &BlockRoute) -> (f32, f32) {
        self.run_range(Sig::Mono(sample), &route.order, route)
            .into_stereo()
    }

    /// The chain order for this call: a bounded, non-blocking read that falls
    /// back to the last good order when a writer is mid-publish. Never waits on
    /// the control/UI thread.
    #[inline]
    fn snapshot_order(&mut self) -> [u8; CHAIN_LEN] {
        if let Some(o) = self.params.try_chain_slots() {
            self.last_order = o;
        }
        self.last_order
    }

    /// Process one mono input sample, returning a stereo (L, R) pair.
    ///
    /// The plugin-free path: the ordered core chain followed by the
    /// master bus. The live engine uses [`process_block`] instead, which also runs
    /// the optional plugin insert between the two. This path **always renders the
    /// built-in rig**, never a hosted AU, so it never skips the cab (unlike a
    /// full-rig AU block).
    #[inline]
    pub fn process(&mut self, sample: f32) -> (f32, f32) {
        let route = self.route_for_block(false);
        let (l, r) = self.process_core(sample, &route);
        master_bus(l, r, route.width)
    }

    /// Process a block of mono input samples into stereo output buffers.
    ///
    /// Normal path, per sample: the ordered core chain. When an **external amp**
    /// is loaded and active, the [`ChainStage::Amp`] position is replaced by a
    /// block-based override: everything before it runs per sample (mono), the
    /// signal is duplicated to stereo and run through the hosted plugin (this is
    /// why the amp position needs a block boundary — a plugin processes whole
    /// buffers, not samples), then everything after it runs per sample. The
    /// [`ChainStage::Cab`] stage is skipped when the AU supplies its own cab/mic
    /// (full-rig); an amp-only AU still feeds the built-in cab (or active IR).
    ///
    /// While an AU is *loaded* but the built-in path runs (AU inactive), that path is
    /// delayed by the AU's reported latency ([`comp_delay`]) so toggling built-in↔AU is
    /// time-coherent. Either way, the optional post-rack plugin insert then runs once
    /// over the block, and the stateless master bus is applied per sample. With no
    /// external amp and no insert loaded, the block is bit-identical to calling
    /// [`process`](Self::process) on each sample.
    ///
    /// `out_l`/`out_r` must each be at least `input.len()` long; processing stops at
    /// the shortest of the three slices.
    pub fn process_block(&mut self, input: &[f32], out_l: &mut [f32], out_r: &mut [f32]) {
        let n = input.len().min(out_l.len()).min(out_r.len());
        let input = &input[..n];
        let out_l = &mut out_l[..n];
        let out_r = &mut out_r[..n];

        let amp_loaded = self.ext_amp.is_some();
        // One bounded, coherent routing snapshot for the entire block.
        let route = self.route_for_block(true);
        if route.use_ext_amp {
            let amp_idx = Self::stage_index(&route.order, ChainStage::Amp, 10);
            // Ordered stages before the amp (mono), duplicated to stereo for the
            // block-based plugin.
            for ((&x, l), r) in input.iter().zip(out_l.iter_mut()).zip(out_r.iter_mut()) {
                let pre = self
                    .run_range(Sig::Mono(x), &route.order[..amp_idx], &route)
                    .into_mono();
                *l = pre;
                *r = pre;
            }
            if let Some(ext_amp) = self.ext_amp.as_mut() {
                ext_amp.process_block(out_l, out_r);
            }
            // Everything after the amp: the cab (skipped when the AU supplies it)
            // and any line-level effects placed between amp and cab or after.
            for (l, r) in out_l.iter_mut().zip(out_r.iter_mut()) {
                let (lv, rv) = self
                    .run_range(Sig::Stereo(*l, *r), &route.order[amp_idx + 1..], &route)
                    .into_stereo();
                *l = lv;
                *r = rv;
            }
        } else {
            // Normal path: the full built-in core chain, per sample.
            for ((&x, l), r) in input.iter().zip(out_l.iter_mut()).zip(out_r.iter_mut()) {
                let (lv, rv) = self.process_core(x, &route);
                *l = lv;
                *r = rv;
            }
            // Keep the built-in path time-aligned with the AU while one is loaded, so
            // the built-in↔AU A/B doesn't jump. No AU loaded → untouched (bit-identical).
            if amp_loaded {
                let delay = self.params.amp_external_latency.load(Relaxed);
                for (l, r) in out_l.iter_mut().zip(out_r.iter_mut()) {
                    let (dl, dr) = self.comp_delay.process(*l, *r, delay);
                    *l = dl;
                    *r = dr;
                }
            }
        }

        // Optional third-party stereo insert, one block at a time.
        if let Some(insert) = self.insert.as_mut() {
            insert.process_block(out_l, out_r);
        }

        // Master bus, per sample (width read once per block, in the route).
        for (l, r) in out_l.iter_mut().zip(out_r.iter_mut()) {
            let (wl, wr) = master_bus(*l, *r, route.width);
            *l = wl;
            *r = wr;
        }
    }
}

/// Master bus: stereo-widen then soft-limit. `width` pushes the cab/reverb
/// decorrelation out for a wider, deeper image without losing mono punch (the
/// mid is untouched); `1.0` is the neutral reference (wire-transparent sides).
/// The output soft limiter is independent of the coloration and always runs, so
/// protection never depends on the width setting. Stateless, so it can run
/// per-sample inside the core loop or as a separate pass over a block with
/// identical results.
#[inline]
fn master_bus(l: f32, r: f32, width: f32) -> (f32, f32) {
    let (l, r) = widen(l, r, width);
    (soft_limit(l), soft_limit(r))
}

/// Mid/side stereo widener. `width` 1.0 = unchanged, > 1.0 spreads the sides.
/// The mono (mid) component is preserved exactly, so the center stays solid and
/// the result folds down to mono cleanly.
#[inline]
fn widen(l: f32, r: f32, width: f32) -> (f32, f32) {
    let mid = (l + r) * 0.5;
    let side = (l - r) * 0.5 * width;
    (mid + side, mid - side)
}

/// Transparent soft limiter: unity for |x| < 0.95, gentle knee above.
/// Replaces the old x.tanh() which colored the signal even at normal levels.
#[inline]
fn soft_limit(x: f32) -> f32 {
    let a = x.abs();
    if a < 0.95 {
        x
    } else {
        let excess = a - 0.95;
        x.signum() * (0.95 + excess / (1.0 + excess * 5.0))
    }
}

/// A fixed-capacity stereo delay line used to time-align the built-in amp path with a
/// hosted AU's reported latency. The delay length is set per block (0 = passthrough);
/// changing it self-heals within `cap` samples. Ring buffers are pre-allocated so the
/// audio thread never allocates.
struct CompDelay {
    buf_l: Vec<f32>,
    buf_r: Vec<f32>,
    pos: usize,
}

impl CompDelay {
    fn new(cap: usize) -> Self {
        let cap = cap.max(1);
        Self {
            buf_l: vec![0.0; cap],
            buf_r: vec![0.0; cap],
            pos: 0,
        }
    }

    /// Push `(l, r)` and return the pair from `delay` samples ago (clamped to capacity).
    /// `delay == 0` returns the input unchanged (write-then-read at the same index).
    #[inline]
    fn process(&mut self, l: f32, r: f32, delay: usize) -> (f32, f32) {
        let cap = self.buf_l.len();
        let d = delay.min(cap - 1);
        self.buf_l[self.pos] = l;
        self.buf_r[self.pos] = r;
        let read = (self.pos + cap - d) % cap;
        let out = (self.buf_l[read], self.buf_r[read]);
        self.pos = (self.pos + 1) % cap;
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::PI;

    /// Drive a loud sine through the full chain and confirm the output stays
    /// finite, within the limiter's bounds, and is genuinely stereo (the cab's
    /// dual-mic convolution should decorrelate L and R).
    #[test]
    fn full_chain_is_finite_bounded_and_stereo() {
        let sr = 48_000.0;
        let params = Arc::new(Params::new());
        let mut chain = DspChain::new(sr, params);

        let mut max_abs = 0.0f32;
        let mut channel_diff = 0.0f32;
        let f = 110.0; // low A — exercises the bass/clip interaction
        for n in 0..(sr as usize) {
            let x = (2.0 * PI * f * n as f32 / sr).sin() * 0.8;
            let (l, r) = chain.process(x);
            assert!(l.is_finite() && r.is_finite(), "non-finite output at {n}");
            max_abs = max_abs.max(l.abs()).max(r.abs());
            channel_diff += (l - r).abs();
        }

        // Soft limiter ceiling is ~1.0; allow a hair of headroom.
        assert!(
            max_abs <= 1.05,
            "output exceeded limiter ceiling: {max_abs}"
        );
        // L and R must differ once the reverb/cab decorrelation has filled in.
        assert!(
            channel_diff > 1.0,
            "output is effectively mono: {channel_diff}"
        );
    }

    /// `process_block` must be bit-identical to running `process` per sample:
    /// the block form is purely a buffering convenience and changes no DSP math.
    /// The whole CLAP-insert plan relies on this equivalence holding.
    #[test]
    fn process_block_matches_per_sample() {
        let sr = 48_000.0;
        let input: Vec<f32> = (0..2000)
            .map(|n| (2.0 * PI * 110.0 * n as f32 / sr).sin() * 0.7)
            .collect();

        let mut per_sample = DspChain::new(sr, Arc::new(Params::new()));
        let mut block = DspChain::new(sr, Arc::new(Params::new()));

        let mut out_l = vec![0.0f32; input.len()];
        let mut out_r = vec![0.0f32; input.len()];
        block.process_block(&input, &mut out_l, &mut out_r);

        for ((&x, &bl), &br) in input.iter().zip(out_l.iter()).zip(out_r.iter()) {
            let (l, r) = per_sample.process(x);
            assert_eq!(l, bl, "L diverged from per-sample path");
            assert_eq!(r, br, "R diverged from per-sample path");
        }
    }

    /// Swapping two drives must actually change the sound: comp-before-fuzz vs
    /// fuzz-before-comp through the same knobs must diverge audibly.
    #[test]
    fn swapped_chain_order_changes_output() {
        fn loud_params() -> Arc<Params> {
            let params = Arc::new(Params::new());
            params.cmp_enabled.store(true, Relaxed);
            params.fz_enabled.store(true, Relaxed);
            params
        }
        let sr = 48_000.0;

        let mut a = ChainStage::default_order();
        a.swap(3, 4); // fuzz before comp
        let b = ChainStage::default_order(); // comp before fuzz

        let mut chain_a = DspChain::new(sr, loud_params());
        let mut chain_b = DspChain::new(sr, loud_params());
        chain_a.params.set_chain_order(&a);
        chain_b.params.set_chain_order(&b);

        let mut diff = 0.0f32;
        for n in 0..2000 {
            let x = (2.0 * PI * 110.0 * n as f32 / sr).sin() * 0.7;
            let (al, ar) = chain_a.process(x);
            let (bl, br) = chain_b.process(x);
            diff += (al - bl).abs() + (ar - br).abs();
        }
        assert!(
            diff > 1e-3,
            "swapping comp/fuzz left the output unchanged: {diff}"
        );
    }

    /// A bypassed stereo pedal moved ahead of the amp must be transparent: it
    /// passes the mono signal through untouched (and the amp folds it, losslessly).
    #[test]
    fn bypassed_stereo_pedal_before_amp_is_transparent() {
        let sr = 48_000.0;
        // Chorus is bypassed by default.
        let mut moved = ChainStage::default_order().to_vec();
        moved.retain(|&v| v != ChainStage::Chorus as u8);
        moved.insert(0, ChainStage::Chorus as u8);
        let moved: [u8; CHAIN_LEN] = moved.try_into().unwrap();

        let mut chain_a = DspChain::new(sr, Arc::new(Params::new()));
        let mut chain_b = DspChain::new(sr, Arc::new(Params::new()));
        chain_a.params.set_chain_order(&moved);

        for n in 0..2000 {
            let x = (2.0 * PI * 110.0 * n as f32 / sr).sin() * 0.7;
            let (al, ar) = chain_a.process(x);
            let (bl, br) = chain_b.process(x);
            assert_eq!(al, bl, "L diverged at sample {n}");
            assert_eq!(ar, br, "R diverged at sample {n}");
        }
    }

    /// A bypassed mono pedal on a stereo feed must not collapse it to mono:
    /// before the enabled-check fix, the dispatch summed 0.5*(l+r) *before*
    /// consulting the bypass flag, so a disabled stage still destroyed the image.
    #[test]
    fn bypassed_mono_stage_preserves_stereo() {
        // Wah is bypassed by default; it is a mono pre pedal.
        let mut chain = DspChain::new(48_000.0, Arc::new(Params::new()));
        let route = chain.route_for_block(false);
        let out = chain.run_ordered_stage(Sig::Stereo(0.7, -0.3), ChainStage::Wah, &route);
        match out {
            Sig::Stereo(l, r) => {
                assert_eq!(l, 0.7, "left channel was altered/collapsed");
                assert_eq!(r, -0.3, "right channel was altered/collapsed");
            }
            Sig::Mono(_) => panic!("bypassed mono stage collapsed stereo to mono"),
        }
    }

    /// A bypassed stereo pedal on a mono feed must not promote it early: the
    /// domain only changes when a *live* stereo effect actually runs.
    #[test]
    fn bypassed_stereo_stage_preserves_mono() {
        // Chorus is bypassed by default; it is a stereo rack pedal.
        let mut chain = DspChain::new(48_000.0, Arc::new(Params::new()));
        let route = chain.route_for_block(false);
        let out = chain.run_ordered_stage(Sig::Mono(0.5), ChainStage::Chorus, &route);
        match out {
            Sig::Mono(x) => assert_eq!(x, 0.5, "mono sample was altered"),
            Sig::Stereo(..) => panic!("bypassed stereo stage promoted mono to stereo"),
        }
    }

    /// Wiring sanity for the helper: the amp and cab stages are always live, and
    /// every other stage tracks its own `*_enabled` flag.
    #[test]
    fn stage_enabled_mirrors_bypass_flags() {
        let params = Params::new();
        assert!(params.stage_enabled(ChainStage::Amp));
        assert!(params.stage_enabled(ChainStage::Cab));
        assert!(!params.stage_enabled(ChainStage::Wah));
        params.wah_enabled.store(true, Relaxed);
        assert!(params.stage_enabled(ChainStage::Wah));
        for v in 0..CHAIN_LEN as u8 {
            let stage = ChainStage::from_u8(v).expect("every slot is a stage");
            let _ = params.stage_enabled(stage);
        }
    }

    /// End-to-end route-domain check: with live stereo effects decorrelating L/R,
    /// moving a *bypassed* mono pedal from before the amp into the stereo region
    /// must leave the rendered output bit-identical. Before the transparency fix
    /// the post-amp placement summed the image to mono and changed every sample.
    #[test]
    fn bypassed_mono_stage_moved_after_live_stereo_is_identical() {
        let sr = 48_000.0;
        fn stereo_params() -> Arc<Params> {
            let p = Arc::new(Params::new());
            p.ch_enabled.store(true, Relaxed); // live stereo chorus decorrelates L/R
            p.rev_enabled.store(true, Relaxed); // decorrelated stereo tail
            p
        }

        // Default placement (Wah bypassed, before the amp).
        let default = ChainStage::default_order();
        // Move the bypassed Wah to just after the cab (into the stereo region).
        let mut moved: Vec<u8> = default.to_vec();
        moved.retain(|&v| v != ChainStage::Wah as u8);
        let cab = moved
            .iter()
            .position(|&v| v == ChainStage::Cab as u8)
            .expect("the cab is in the default order");
        moved.insert(cab + 1, ChainStage::Wah as u8);
        let moved: [u8; CHAIN_LEN] = moved.try_into().expect("CHAIN_LEN slots");

        let mut before = DspChain::new(sr, stereo_params());
        let mut after = DspChain::new(sr, stereo_params());
        before.params.set_chain_order(&default);
        after.params.set_chain_order(&moved);

        let mut max_diff = 0.0f32;
        for n in 0..4000 {
            let x = (2.0 * PI * 110.0 * n as f32 / sr).sin() * 0.6;
            let (al, ar) = before.process(x);
            let (bl, br) = after.process(x);
            max_diff = max_diff.max((al - bl).abs()).max((ar - br).abs());
        }
        assert!(
            max_diff < 1e-6,
            "moving the bypassed Wah changed the output by {max_diff}"
        );
    }

    /// A **live** mono pedal sitting on a stereo feed intentionally downmixes to
    /// mono (and duplicates): that is the documented behavior of a real mono
    /// pedal fed from a stereo send, unlike a bypassed one.
    #[test]
    fn live_mono_stage_after_stereo_downsamples_to_mono() {
        let params = Arc::new(Params::new());
        params.wah_enabled.store(true, Relaxed); // live mono effect
        let mut chain = DspChain::new(48_000.0, params);

        let route = chain.route_for_block(false);
        let out = chain.run_ordered_stage(Sig::Stereo(0.4, -0.2), ChainStage::Wah, &route);
        match out {
            Sig::Stereo(l, r) => assert_eq!(
                l, r,
                "live mono stage must sum the stereo feed to dual mono"
            ),
            Sig::Mono(_) => panic!("live mono stage should keep the stereo domain (dual mono)"),
        }
    }

    /// The studio master: width `1.0` is the neutral reference (below the limiter
    /// knee it is wire-transparent), the widener preserves the mid, and the
    /// output limiter still catches peaks at every width.
    #[test]
    fn master_bus_width_is_neutral_at_one_and_limiter_is_independent() {
        let (nl, nr) = master_bus(0.5, -0.2, 1.0);
        assert!(
            (nl - 0.5).abs() < 1e-6 && (nr + 0.2).abs() < 1e-6,
            "width 1.0 must be neutral, got ({nl}, {nr})"
        );

        let (wl, wr) = widen(0.5, -0.2, 1.3);
        let mid_in = 0.5 * (0.5 + -0.2);
        let mid_out = 0.5 * (wl + wr);
        assert!((mid_in - mid_out).abs() < 1e-6, "widener changed the mid");
        assert!(
            (wl - wr).abs() > (0.5 - -0.2f32).abs(),
            "widener did not spread sides"
        );

        for width in [0.0, 1.0, 1.3, 2.0] {
            let (l, r) = master_bus(4.0, -4.0, width);
            assert!(
                l.abs() < 1.2 && r.abs() < 1.2,
                "limiter failed to bound output at width {width}"
            );
        }
    }

    /// A different master width changes the rendered output (the cab decorrelates
    /// L/R, so the sides carry content the widener acts on).
    #[test]
    fn master_width_changes_the_output() {
        fn params(width: f32) -> Arc<Params> {
            let p = Arc::new(Params::new());
            p.master_width.store(width, Relaxed);
            p
        }
        let sr = 48_000.0;
        let mut neutral = DspChain::new(sr, params(1.0));
        let mut wide = DspChain::new(sr, params(1.3));

        let mut diff = 0.0f32;
        for n in 0..2000 {
            let x = (2.0 * PI * 110.0 * n as f32 / sr).sin() * 0.5;
            let (nl, nr) = neutral.process(x);
            let (wl, wr) = wide.process(x);
            diff += (nl - wl).abs() + (nr - wr).abs();
        }
        assert!(diff > 1e-3, "master width had no audible effect: {diff}");
    }

    /// Rapid adjacent swaps (as the UI's `[` / `]` produce) never leave a
    /// duplicate or missing stage: after every move the snapshot is exactly the
    /// order that was written, and a full permutation of all stages.
    #[test]
    fn rapid_reorder_keeps_a_complete_permutation() {
        let params = Params::new();
        let want: Vec<u8> = (0..CHAIN_LEN as u8).collect();
        for round in 0..1000usize {
            let mut order = params.chain_slots();
            let i = round % (CHAIN_LEN - 1);
            order.swap(i, i + 1);
            params.set_chain_order(&order);
            let snap = params.chain_slots();
            assert_eq!(snap, order, "snapshot diverged from the written order");
            let mut sorted = snap;
            sorted.sort_unstable();
            assert_eq!(
                sorted.as_slice(),
                want.as_slice(),
                "not a full permutation after round {round}"
            );
        }
    }

    /// Chain order storage round-trips, and reset restores the shipped order.
    #[test]
    fn chain_order_round_trips_and_resets() {
        let params = Params::new();
        assert_eq!(params.chain_slots(), ChainStage::default_order());
        let mut swapped = ChainStage::default_order();
        swapped.swap(0, 18);
        params.set_chain_order(&swapped);
        assert_eq!(params.chain_slots(), swapped);
        params.reset_to_defaults();
        assert_eq!(params.chain_slots(), ChainStage::default_order());
    }

    /// A snapshot must always be one whole, valid order — never a torn mix of
    /// two concurrent swaps. Two distinct permutations are published in a tight
    /// loop while the reader snapshots; every read must equal one of them. This
    /// is the coherence guarantee the seqlock exists for.
    #[test]
    fn chain_order_snapshot_is_never_torn_under_concurrent_swaps() {
        let params = Arc::new(Params::new());
        let mut a = ChainStage::default_order();
        a.swap(0, 18);
        let mut b = ChainStage::default_order();
        b.swap(3, 4);
        assert_ne!(a, b, "the two permutations must differ");
        // Seed one permutation so the only coherent values are `a` and `b`
        // (otherwise the initial boot order is a valid third value).
        params.set_chain_order(&a);

        let writer = {
            let params = Arc::clone(&params);
            std::thread::spawn(move || {
                for i in 0..200_000u32 {
                    params.set_chain_order(if i & 1 == 0 { &a } else { &b });
                }
            })
        };

        for _ in 0..200_000 {
            let snap = params.chain_slots();
            assert!(
                snap == a || snap == b,
                "reader observed a torn/partial order: {snap:?}"
            );
        }
        writer.join().expect("writer thread panicked");
    }

    /// The audio-thread reader never waits for a stalled writer: with the
    /// seqlock sequence stuck odd (a writer preempted mid-publish), a block
    /// render must still complete, falling back to its last good order.
    #[test]
    fn audio_read_never_waits_for_a_stalled_writer() {
        use std::sync::mpsc;
        use std::time::Duration;

        let sr = 48_000.0;
        let input: Vec<f32> = (0..256)
            .map(|n| (2.0 * PI * 110.0 * n as f32 / sr).sin() * 0.6)
            .collect();

        let params = Arc::new(Params::new());
        let mut chain = DspChain::new(sr, Arc::clone(&params));
        let mut reference = DspChain::new(sr, Arc::clone(&params));

        // Ground truth: render the same input with a clean, even sequence.
        let mut want_l = vec![0.0f32; input.len()];
        let mut want_r = vec![0.0f32; input.len()];
        reference.process_block(&input, &mut want_l, &mut want_r);

        // Fake a writer stuck between the two sequence bumps.
        params.chain_seq.fetch_add(1, SeqCst);

        let (tx, rx) = mpsc::channel();
        let handle = std::thread::spawn(move || {
            let mut l = vec![0.0f32; input.len()];
            let mut r = vec![0.0f32; input.len()];
            chain.process_block(&input, &mut l, &mut r);
            tx.send((l, r)).ok();
        });
        let (got_l, got_r) = rx
            .recv_timeout(Duration::from_secs(2))
            .expect("process_block must not wait for a stalled writer");
        handle.join().ok();

        // Restore an even sequence so later tests see a clean lock.
        params.chain_seq.fetch_add(1, SeqCst);

        assert_eq!(got_l, want_l, "L must match the last-good order render");
        assert_eq!(got_r, want_r, "R must match the last-good order render");
    }

    /// Even with two writer threads publishing different permutations, the
    /// writer mutex keeps the seqlock single-writer, so every reader snapshot is
    /// exactly one of the two orders — never a torn mix.
    #[test]
    fn concurrent_writers_never_tear_the_order() {
        let params = Arc::new(Params::new());
        let mut a = ChainStage::default_order();
        a.swap(0, 18);
        let mut b = ChainStage::default_order();
        b.swap(3, 4);
        params.set_chain_order(&a);

        let writers: Vec<_> = [a, b]
            .into_iter()
            .map(|order| {
                let params = Arc::clone(&params);
                std::thread::spawn(move || {
                    for _ in 0..50_000 {
                        params.set_chain_order(&order);
                    }
                })
            })
            .collect();

        for _ in 0..100_000 {
            let snap = params.chain_slots();
            assert!(
                snap == a || snap == b,
                "reader observed a torn order: {snap:?}"
            );
        }
        for writer in writers {
            writer.join().expect("writer thread panicked");
        }
    }

    /// Sanitizing drops unknowns/dupes and appends missing stages in default order.
    #[test]
    fn sanitize_chain_order_repairs() {
        // Unknown id 99 dropped, dupe gate collapsed, missing stages appended.
        let fixed = sanitize_chain_order(&[3, 99, 3, 10]);
        assert_eq!(&fixed[..2], &[3, 10]);
        assert_eq!(fixed.len(), CHAIN_LEN);
        let mut sorted = fixed;
        sorted.sort_unstable();
        let mut want: Vec<u8> = (0..CHAIN_LEN as u8).collect();
        want.sort_unstable();
        assert_eq!(sorted, want.as_slice());
        // Empty input yields the default order.
        assert_eq!(sanitize_chain_order(&[]), ChainStage::default_order());
    }

    /// Stage names round-trip (preset persistence relies on this).
    #[test]
    fn chain_stage_names_round_trip() {
        for v in 0..CHAIN_LEN as u8 {
            let stage = ChainStage::from_u8(v).expect("every slot is a stage");
            assert_eq!(ChainStage::from_name(stage.name()), Some(stage));
        }
        assert_eq!(ChainStage::from_name("amp"), Some(ChainStage::Amp));
        assert_eq!(ChainStage::from_name("cab"), Some(ChainStage::Cab));
        // `ampcab` is no longer a stage name; the preset parser migrates it.
        assert_eq!(ChainStage::from_name("ampcab"), None);
        assert_eq!(ChainStage::from_name("bogus"), None);
        assert_eq!(ChainStage::from_u8(99), None);
    }

    /// A loaded stereo insert must actually run on the block, between the core
    /// chain and the master bus. A trivial gain insert lets us verify the slot is
    /// wired (output differs from the no-insert path) without depending on a plugin.
    #[test]
    fn loaded_insert_is_applied_to_the_block() {
        struct HalfGain;
        impl StereoInsert for HalfGain {
            fn process_block(&mut self, left: &mut [f32], right: &mut [f32]) {
                for (l, r) in left.iter_mut().zip(right.iter_mut()) {
                    *l *= 0.5;
                    *r *= 0.5;
                }
            }
        }

        let sr = 48_000.0;
        let input: Vec<f32> = (0..1000)
            .map(|n| (2.0 * PI * 220.0 * n as f32 / sr).sin() * 0.6)
            .collect();

        // Fresh chains so internal state (reverb/delay) doesn't bleed between runs.
        let mut bare = DspChain::new(sr, Arc::new(Params::new()));
        let (mut bare_l, mut bare_r) = (vec![0.0; input.len()], vec![0.0; input.len()]);
        bare.process_block(&input, &mut bare_l, &mut bare_r);

        let mut with_insert = DspChain::new(sr, Arc::new(Params::new()));
        with_insert.set_insert(Some(Box::new(HalfGain)));
        let (mut ins_l, mut ins_r) = (vec![0.0; input.len()], vec![0.0; input.len()]);
        with_insert.process_block(&input, &mut ins_l, &mut ins_r);

        // The insert must have changed the output somewhere in the block.
        assert!(
            bare_l.iter().zip(&ins_l).any(|(a, b)| a != b)
                || bare_r.iter().zip(&ins_r).any(|(a, b)| a != b),
            "insert had no effect on the output"
        );
    }

    /// An active external amp must override the built-in amp+cab (its output feeds the
    /// rack instead), and a *loaded but inactive* external amp must leave the built-in
    /// path bit-identical — the live built-in↔external toggle depends on both.
    #[test]
    fn external_amp_overrides_builtin_only_when_active() {
        // A recognisable stereo signature so we can tell the override actually ran.
        struct ConstAmp;
        impl StereoInsert for ConstAmp {
            fn process_block(&mut self, left: &mut [f32], right: &mut [f32]) {
                for (l, r) in left.iter_mut().zip(right.iter_mut()) {
                    *l = 0.3;
                    *r = -0.3;
                }
            }
        }

        let sr = 48_000.0;
        let input: Vec<f32> = (0..1000)
            .map(|n| (2.0 * PI * 220.0 * n as f32 / sr).sin() * 0.6)
            .collect();
        let bufs = || (vec![0.0f32; input.len()], vec![0.0f32; input.len()]);

        // Built-in reference path (no external amp).
        let mut builtin = DspChain::new(sr, Arc::new(Params::new()));
        let (mut bl, mut br) = bufs();
        builtin.process_block(&input, &mut bl, &mut br);

        // External amp loaded *and active*: must diverge from the built-in path.
        let params = Arc::new(Params::new());
        params.amp_external_active.store(true, Relaxed);
        let mut active = DspChain::new(sr, params);
        active.set_ext_amp(Some(Box::new(ConstAmp)));
        let (mut al, mut ar) = bufs();
        active.process_block(&input, &mut al, &mut ar);
        assert!(
            bl.iter().zip(&al).any(|(a, b)| a != b) || br.iter().zip(&ar).any(|(a, b)| a != b),
            "active external amp had no effect (built-in amp+cab not bypassed)"
        );

        // External amp loaded but *inactive*: must match the built-in path exactly.
        let mut inactive = DspChain::new(sr, Arc::new(Params::new()));
        inactive.set_ext_amp(Some(Box::new(ConstAmp)));
        let (mut il, mut ir) = bufs();
        inactive.process_block(&input, &mut il, &mut ir);
        assert_eq!(
            il, bl,
            "inactive external amp altered the built-in path (L)"
        );
        assert_eq!(
            ir, br,
            "inactive external amp altered the built-in path (R)"
        );
    }

    /// In amp-only mode the AU replaces only the amp, so its output must be run through
    /// the built-in cab before the rack — audibly different from the default amp+cab
    /// mode where the AU's output goes straight to the rack.
    #[test]
    fn amp_only_external_amp_runs_through_builtin_cab() {
        // A fake amp that just passes its (duplicated pre-amp) input at a fixed gain.
        struct GainAmp;
        impl StereoInsert for GainAmp {
            fn process_block(&mut self, left: &mut [f32], right: &mut [f32]) {
                for (l, r) in left.iter_mut().zip(right.iter_mut()) {
                    *l *= 0.8;
                    *r *= 0.8;
                }
            }
        }

        let sr = 48_000.0;
        let input: Vec<f32> = (0..2000)
            .map(|n| (2.0 * PI * 110.0 * n as f32 / sr).sin() * 0.5)
            .collect();
        let bufs = || (vec![0.0f32; input.len()], vec![0.0f32; input.len()]);

        // Default (amp+cab): AU output goes straight to the rack.
        let p1 = Arc::new(Params::new());
        p1.amp_external_active.store(true, Relaxed);
        let mut full = DspChain::new(sr, p1);
        full.set_ext_amp(Some(Box::new(GainAmp)));
        let (mut fl, mut fr) = bufs();
        full.process_block(&input, &mut fl, &mut fr);

        // Amp-only: AU output is summed to mono and run through the built-in cab.
        let p2 = Arc::new(Params::new());
        p2.amp_external_active.store(true, Relaxed);
        p2.amp_external_amp_only.store(true, Relaxed);
        let mut amp_only = DspChain::new(sr, p2);
        amp_only.set_ext_amp(Some(Box::new(GainAmp)));
        let (mut ol, mut or) = bufs();
        amp_only.process_block(&input, &mut ol, &mut or);

        assert!(
            fl.iter().zip(&ol).any(|(a, b)| (a - b).abs() > 1e-6)
                || fr.iter().zip(&or).any(|(a, b)| (a - b).abs() > 1e-6),
            "amp-only mode did not route the AU output through the built-in cab"
        );
    }

    /// `route_for_block` is the single source of truth for routing: one read per
    /// block decides Amp/Cab, regardless of which call path asked.
    #[test]
    fn route_truth_table() {
        struct Noop;
        impl StereoInsert for Noop {
            fn process_block(&mut self, _l: &mut [f32], _r: &mut [f32]) {}
        }

        fn route(loaded: bool, active: bool, amp_only: bool, allow_ext_amp: bool) -> (bool, bool) {
            let params = Arc::new(Params::new());
            params.amp_external_active.store(active, Relaxed);
            params.amp_external_amp_only.store(amp_only, Relaxed);
            let mut chain = DspChain::new(48_000.0, params);
            if loaded {
                chain.set_ext_amp(Some(Box::new(Noop)));
            }
            let r = chain.route_for_block(allow_ext_amp);
            (r.use_ext_amp, r.skip_cab)
        }

        for &loaded in &[false, true] {
            for &active in &[false, true] {
                for &amp_only in &[false, true] {
                    // The per-sample path never runs an AU, so it never skips the cab.
                    assert_eq!(
                        route(loaded, active, amp_only, false),
                        (false, false),
                        "per-sample route must keep the built-in cab"
                    );
                    let want_ext = loaded && active;
                    let want_skip = want_ext && !amp_only;
                    assert_eq!(
                        route(loaded, active, amp_only, true),
                        (want_ext, want_skip),
                        "block route mismatch: loaded={loaded} active={active} amp_only={amp_only}"
                    );
                }
            }
        }
    }

    /// `process()` always renders the built-in rig: a flagged full-rig AU must not
    /// make the per-sample path silently drop the cab (the old R2 bug).
    #[test]
    fn per_sample_process_keeps_the_cab_with_a_full_rig_au_flagged() {
        struct Noop;
        impl StereoInsert for Noop {
            fn process_block(&mut self, _l: &mut [f32], _r: &mut [f32]) {}
        }

        let sr = 48_000.0;
        let params = Arc::new(Params::new());
        params.amp_external_active.store(true, Relaxed); // full-rig flagged
        let mut flagged = DspChain::new(sr, params);
        flagged.set_ext_amp(Some(Box::new(Noop)));

        let mut plain = DspChain::new(sr, Arc::new(Params::new()));

        for n in 0..4000 {
            let x = (2.0 * PI * 110.0 * n as f32 / sr).sin() * 0.6;
            let (fl, fr) = flagged.process(x);
            let (pl, pr) = plain.process(x);
            assert_eq!(fl, pl, "flagged L diverged at sample {n}");
            assert_eq!(fr, pr, "flagged R diverged at sample {n}");
        }
    }

    /// With an AU loaded, the built-in path must be delayed by the AU's reported latency
    /// so a built-in↔AU A/B stays time-coherent: the delayed output is the zero-latency
    /// output shifted right by exactly N samples, with N leading zero samples.
    #[test]
    fn latency_comp_delays_builtin_path_by_reported_frames() {
        struct Silent;
        impl StereoInsert for Silent {
            fn process_block(&mut self, _l: &mut [f32], _r: &mut [f32]) {}
        }
        const N: usize = 32;
        let sr = 48_000.0;
        let input: Vec<f32> = (0..800)
            .map(|n| (2.0 * PI * 196.0 * n as f32 / sr).sin() * 0.5)
            .collect();
        let bufs = || (vec![0.0f32; input.len()], vec![0.0f32; input.len()]);

        // AU loaded but inactive, latency 0 → built-in path runs undelayed.
        let p0 = Arc::new(Params::new());
        let mut c0 = DspChain::new(sr, p0);
        c0.set_ext_amp(Some(Box::new(Silent)));
        let (mut l0, mut r0) = bufs();
        c0.process_block(&input, &mut l0, &mut r0);

        // AU loaded but inactive, latency N → built-in path delayed by N samples.
        let pn = Arc::new(Params::new());
        pn.amp_external_latency.store(N, Relaxed);
        let mut cn = DspChain::new(sr, pn);
        cn.set_ext_amp(Some(Box::new(Silent)));
        let (mut ln, mut rn) = bufs();
        cn.process_block(&input, &mut ln, &mut rn);

        for i in 0..N {
            assert_eq!((ln[i], rn[i]), (0.0, 0.0), "leading sample {i} not zero");
        }
        for i in N..input.len() {
            assert_eq!(ln[i], l0[i - N], "L not delayed by {N} at {i}");
            assert_eq!(rn[i], r0[i - N], "R not delayed by {N} at {i}");
        }
    }

    /// `replace_insert` must hand back exactly the insert it displaced — the engine
    /// relies on this to ship the old plugin off the audio thread for disposal.
    #[test]
    fn replace_insert_returns_the_displaced_insert() {
        #[allow(dead_code)]
        struct Tagged(u32);
        impl StereoInsert for Tagged {
            fn process_block(&mut self, _l: &mut [f32], _r: &mut [f32]) {}
        }

        let mut chain = DspChain::new(48_000.0, Arc::new(Params::new()));

        // First install: nothing displaced.
        assert!(chain.replace_insert(Some(Box::new(Tagged(1)))).is_none());

        // Second install: the first one comes back.
        let old = chain
            .replace_insert(Some(Box::new(Tagged(2))))
            .expect("expected the previously installed insert");
        // (Downcasting through dyn isn't available without Any; the round-trip and
        // the None-on-first-install above are enough to prove the swap semantics.)
        drop(old);

        // Clearing returns the live one.
        assert!(chain.replace_insert(None).is_some());
        assert!(chain.replace_insert(None).is_none());
    }

    fn goertzel(samples: &[f32], f: f32, sr: f32) -> f32 {
        let w = 2.0 * PI * f / sr;
        let coeff = 2.0 * w.cos();
        let (mut s1, mut s2) = (0.0f32, 0.0f32);
        for &x in samples {
            let s0 = x + coeff * s1 - s2;
            s2 = s1;
            s1 = s0;
        }
        let real = s1 - s2 * w.cos();
        let imag = s2 * w.sin();
        (real * real + imag * imag).sqrt() / (samples.len() as f32 / 2.0)
    }

    /// The passive FMV tone stack must be stable, peak-bounded (it only cuts), and
    /// its controls must move the right bands: turning a knob up should raise that
    /// band's output. Guards the hand-transcribed analog→digital coefficients.
    #[test]
    fn tonestack_is_stable_and_controls_work() {
        use super::tonestack::{Components, ToneStack};
        let sr = 48_000.0;

        // Measure a band's steady-state level for given (bass, mid, treble).
        let level = |b: f32, m: f32, t: f32, f: f32| {
            let mut ts = ToneStack::new(sr, Components::MARSHALL);
            ts.update(b, m, t);
            let mut out = Vec::with_capacity(sr as usize / 2);
            for n in 0..(sr as usize / 2) {
                let x = (2.0 * PI * f * n as f32 / sr).sin();
                let y = ts.process(x);
                assert!(y.is_finite(), "tonestack non-finite");
                // ignore the first quarter (settling)
                if n >= sr as usize / 8 {
                    out.push(y);
                }
            }
            goertzel(&out, f, sr)
        };

        // Peak-normalised: no setting should pass more than unity (a hair of slack).
        for &(b, m, t) in &[(0.5, 0.5, 0.5), (1.0, 0.0, 1.0), (0.0, 1.0, 0.0)] {
            for &f in &[100.0, 800.0, 4000.0] {
                assert!(level(b, m, t, f) <= 1.2, "tonestack boosts above unity");
            }
        }

        // Bass up → more lows; treble up → more highs; mid up → more mids.
        assert!(
            level(0.9, 0.5, 0.5, 100.0) > level(0.1, 0.5, 0.5, 100.0),
            "bass control inverted/dead at 100 Hz"
        );
        assert!(
            level(0.5, 0.5, 0.9, 4000.0) > level(0.5, 0.5, 0.1, 4000.0),
            "treble control inverted/dead at 4 kHz"
        );
        assert!(
            level(0.5, 0.9, 0.5, 800.0) > level(0.5, 0.1, 0.5, 800.0),
            "mid control inverted/dead at 800 Hz"
        );
    }

    // ── Base-tone quality (amp + cab, no pedals) ──────────────────────────────
    //
    // These tests pin the "clean, melodic, not artificial" voicing the amp/cab
    // were tuned for. The danger with hand-dialled DSP is that the parameters drift
    // back into the failure modes we measured and fixed: the played note buried
    // under its own overtones (fizz), an ice-pick presence band, or one note
    // jumping out far louder than its neighbours. Each test is an objective bound on
    // one of those, measured on the real amp→cab signal path at the shipped default
    // controls, so a future tweak that re-introduces the problem fails loudly.

    /// Amp model paired with the cab it is voiced against.
    const RIGS: [(AmpModel, CabModel); 5] = [
        (AmpModel::Marshall, CabModel::Marshall),
        (AmpModel::Mesa, CabModel::Mesa),
        (AmpModel::Randall, CabModel::Orange),
        (AmpModel::Supro, CabModel::Supro),
        (AmpModel::Tweed, CabModel::Tweed),
    ];

    /// Notes spanning the full usable range, low-E open up into the top octave —
    /// deliberately including the high register (A5–E6), where an over-bright system
    /// makes single notes blast out and lose their body. Lead lines live up here.
    const NECK: [(&str, f32); 11] = [
        ("E2", 82.41),
        ("A2", 110.0),
        ("D3", 146.83),
        ("G3", 196.0),
        ("B3", 246.94),
        ("E4", 329.63),
        ("A4", 440.0),
        ("E5", 659.25),
        ("A5", 880.0),
        ("C6", 1046.5),
        ("E6", 1318.5),
    ];

    /// Render one sustained note through amp+cab at the shipped default controls and
    /// return the mono (L+R) steady-state tail (filter/envelope transients dropped).
    fn render_note(am: AmpModel, cm: CabModel, freq: f32) -> Vec<f32> {
        let sr = 48_000.0;
        let mut amp = amp::AmpBank::new(sr);
        let mut cab = cab::CabBank::new(sr);
        let n = sr as usize;
        let warmup = n / 3;
        let mut out = Vec::with_capacity(n - warmup);
        let knobs = amp::standard_knobs(am, 0.65, 0.50, 0.45, 0.65, 0.50, 0.55);
        for i in 0..n {
            let x = (2.0 * PI * freq * i as f32 / sr).sin() * 0.5;
            let a = amp.process(am, x, &knobs);
            let (l, r) = cab.process(cm, a, 0.5, 0.15, 0.15);
            if i >= warmup {
                out.push(l + r);
            }
        }
        out
    }

    /// Summed Goertzel energy in [lo, hi) on a semitone grid (coarse but consistent).
    fn band_energy(s: &[f32], lo: f32, hi: f32) -> f32 {
        let sr = 48_000.0;
        let mut f = lo;
        let mut e = 0.0;
        while f < hi {
            let g = goertzel(s, f, sr);
            e += g * g;
            f *= 2.0_f32.powf(1.0 / 12.0);
        }
        e
    }

    /// Spectral centroid (Hz) on a quarter-tone grid 50 Hz–10 kHz — a single number
    /// for "how bright": a guitar-amp tone lives in the low hundreds–low thousands,
    /// and a note whose centroid runs into the multi-kHz range reads as ice-pick.
    fn centroid(s: &[f32]) -> f32 {
        let sr = 48_000.0;
        let mut f = 50.0f32;
        let (mut num, mut den) = (0.0f32, 0.0f32);
        while f < 10_000.0 {
            let g = goertzel(s, f, sr);
            num += f * g;
            den += g;
            f *= 2.0_f32.powf(1.0 / 24.0);
        }
        num / den.max(1e-12)
    }

    /// The played note must lead its own overtones. Driving the amp adds harmonics
    /// (that is the point), but if the 5th–10th harmonic ends up *louder than the
    /// fundamental* the note loses its pitch centre and the tone turns to fizz —
    /// the exact failure (fundamental 30–100× below an upper harmonic) the
    /// inter-stage coupling high-passes were re-tuned to cure. We require the
    /// fundamental to stay within ~5× of the loudest partial across the neck.
    #[test]
    fn fundamental_is_not_buried_under_overtones() {
        let sr = 48_000.0;
        for (am, cm) in RIGS {
            for (name, f) in NECK {
                let out = render_note(am, cm, f);
                let h: Vec<f32> = (1..=10).map(|k| goertzel(&out, f * k as f32, sr)).collect();
                let peak = h.iter().cloned().fold(0.0f32, f32::max).max(1e-9);
                let fund_dom = h[0] / peak;
                assert!(
                    fund_dom >= 0.20,
                    "{} {name}: fundamental buried under overtones (funDom {fund_dom:.3})",
                    am.name()
                );
            }
        }
    }

    /// No single note may sit in the harsh / ice-pick zone. Two complementary
    /// bounds: the 2–5 kHz "presence" energy must stay a fraction of the 200 Hz–2
    /// kHz body (so the cab presence spike + upper harmonics don't dominate), and
    /// the overall brightness (centroid) must stay in the musical guitar band. This
    /// guards the cab presence-peak gains and the amp's harmonic generation.
    #[test]
    fn no_note_is_harsh_or_ice_picky() {
        for (am, cm) in RIGS {
            for (name, f) in NECK {
                let out = render_note(am, cm, f);
                let harsh = band_energy(&out, 2000.0, 5000.0);
                let body = band_energy(&out, 200.0, 2000.0).max(1e-12);
                assert!(
                    harsh / body < 0.6,
                    "{} {name}: harsh/ice-pick (2-5k / body = {:.3})",
                    am.name(),
                    harsh / body
                );
                let c = centroid(&out);
                assert!(
                    c < 2500.0,
                    "{} {name}: too bright (centroid {c:.0} Hz)",
                    am.name()
                );
            }
        }
    }

    /// Almost no energy should survive above the speaker's rolloff. A real cab is a
    /// steep low-pass; audible content above ~6 kHz is aliasing/fizz, the "digital"
    /// artefact the 8× oversampling and cab IR exist to suppress. We require the
    /// 6–12 kHz band to be a tiny fraction of the total — a direct guard on the
    /// oversampling and the cab's top-end voicing.
    #[test]
    fn no_audible_fizz_above_the_cab_rolloff() {
        for (am, cm) in RIGS {
            for (name, f) in NECK {
                let out = render_note(am, cm, f);
                let fizz = band_energy(&out, 6000.0, 12_000.0);
                let total = band_energy(&out, 50.0, 12_000.0).max(1e-12);
                assert!(
                    fizz / total < 0.01,
                    "{} {name}: fizz above cab rolloff ({:.2}% of total)",
                    am.name(),
                    100.0 * fizz / total
                );
            }
        }
    }

    /// High notes must not blast out over the mid neck. The original voicing had a
    /// steep rising frequency tilt — a note at 880 Hz ran +17 dB louder than one at
    /// 220 Hz — so single notes high up the neck leapt out and, with their harmonics
    /// past the cab rolloff, collapsed to thin, piercing fundamentals (the "strange
    /// high notes"). At high gain, clipping compression hides this; at clean settings
    /// it is laid bare.
    ///
    /// We compare the top octave (E5–E6) against the mid neck (G3–A4) rather than the
    /// raw min/max across all notes: the deep low E is *intentionally* a touch
    /// quieter on the tight-voiced metal amps (the low-mid cut that makes palm mutes
    /// chug), and penalising that would conflate two different things. What must stay
    /// bounded is the high register relative to the body of the neck.
    #[test]
    fn high_notes_dont_blast_over_the_mid_neck() {
        let level = |am, cm, f: f32| {
            let out = render_note(am, cm, f);
            (out.iter().map(|&x| x * x).sum::<f32>() / out.len() as f32).sqrt()
        };
        for (am, cm) in RIGS {
            let mid = [196.0, 246.94, 329.63, 440.0] // G3 B3 E4 A4
                .iter()
                .map(|&f| level(am, cm, f))
                .sum::<f32>()
                / 4.0;
            let high = [659.25, 880.0, 1046.5, 1318.5] // E5 A5 C6 E6
                .iter()
                .map(|&f| level(am, cm, f))
                .sum::<f32>()
                / 4.0;
            assert!(
                high / mid.max(1e-9) < 2.5,
                "{}: high register blasts over the mid neck (high/mid {:.2}x)",
                am.name(),
                high / mid.max(1e-9)
            );
        }
    }

    /// Power chords (root + fifth + octave) up the neck must stay even and tight —
    /// the rhythm-playing counterpart to the single-note evenness test. No chord
    /// should jump out far louder than its neighbours, and the inaudible
    /// difference-tone "fart" an octave below the root must stay well under the
    /// chord's musical body at every position.
    #[test]
    fn power_chords_are_even_and_tight_across_the_neck() {
        // (root, fifth, octave) for chords rooted up the low strings.
        let chords: [(f32, f32, f32); 6] = [
            (82.41, 123.47, 164.81),  // E2
            (98.0, 146.83, 196.0),    // G2
            (110.0, 164.81, 220.0),   // A2
            (130.81, 196.0, 261.63),  // C3
            (164.81, 246.94, 329.63), // E3
            (220.0, 329.63, 440.0),   // A3
        ];
        let sr = 48_000.0;
        for (am, cm) in RIGS {
            let mut levels = Vec::new();
            for &(r, fifth, oct) in &chords {
                let mut amp = amp::AmpBank::new(sr);
                let mut cab = cab::CabBank::new(sr);
                let n = sr as usize;
                let warmup = n / 3;
                let mut out = Vec::with_capacity(n - warmup);
                let knobs = amp::standard_knobs(am, 0.65, 0.50, 0.45, 0.65, 0.50, 0.55);
                for i in 0..n {
                    let t = i as f32 / sr;
                    let x = ((2.0 * PI * r * t).sin()
                        + (2.0 * PI * fifth * t).sin()
                        + (2.0 * PI * oct * t).sin())
                        * 0.3;
                    let a = amp.process(am, x, &knobs);
                    let (l, rr) = cab.process(cm, a, 0.5, 0.15, 0.15);
                    if i >= warmup {
                        out.push(l + rr);
                    }
                }
                let sub = goertzel(&out, r * 0.5, sr) + goertzel(&out, r * 0.66, sr);
                let body =
                    goertzel(&out, r, sr) + goertzel(&out, fifth, sr) + goertzel(&out, oct, sr);
                assert!(
                    sub / body.max(1e-9) < 0.5,
                    "{}: power chord at {r:.0} Hz is farty (sub/body {:.2})",
                    am.name(),
                    sub / body.max(1e-9)
                );
                levels.push((out.iter().map(|&x| x * x).sum::<f32>() / out.len() as f32).sqrt());
            }
            let lo = levels.iter().cloned().fold(f32::INFINITY, f32::min);
            let hi = levels.iter().cloned().fold(0.0, f32::max);
            assert!(
                hi / lo < 4.0,
                "{}: power chords uneven across neck (spread {:.1}x)",
                am.name(),
                hi / lo
            );
        }
    }

    /// A note must sound the same whether played on its own or right after other
    /// notes. The dynamic grid-bias "bloom" follower deliberately adds even-harmonic
    /// warmth that grows with how hard you play — touch sensitivity — but if it
    /// releases too slowly it stays loaded from the previous notes and over-warms the
    /// *next* note's attack, so the same note picks up a different timbre depending on
    /// what preceded it (an audible note-to-note inconsistency). We render a note's
    /// attack cold (from silence) and again right after a loud lick, and require the
    /// even-harmonic content of its attack to barely move. This guards the bloom
    /// depth/release: within-note give stays, cross-note bleed does not.
    #[test]
    fn a_note_attacks_the_same_regardless_of_what_preceded_it() {
        let sr = 48_000.0;
        let note = 164.81; // E3 — absent from the preceding lick below
        // Attack-window 2nd-harmonic ratio of `note`, optionally after a loud lick.
        let attack_h2_ratio = |am: AmpModel, cm: CabModel, preceded: bool| -> f32 {
            let mut amp = amp::AmpBank::new(sr);
            let mut cab = cab::CabBank::new(sr);
            let knobs = amp::standard_knobs(am, 0.7, 0.5, 0.45, 0.65, 0.5, 0.6);
            let run =
                |amp: &mut amp::AmpBank, cab: &mut cab::CabBank, f: f32, n: usize, amp_in: f32| {
                    let mut last = 0.0;
                    for i in 0..n {
                        let x = (2.0 * PI * f * i as f32 / sr).sin() * amp_in;
                        let a = amp.process(am, x, &knobs);
                        let (l, r) = cab.process(cm, a, 0.5, 0.15, 0.15);
                        last = l + r;
                    }
                    last
                };
            // Settle filters from rest.
            run(&mut amp, &mut cab, 0.0, sr as usize / 20, 0.0);
            if preceded {
                for &f in &[196.0f32, 261.63, 329.63, 220.0] {
                    run(&mut amp, &mut cab, f, sr as usize * 90 / 1000, 0.6);
                }
            }
            // Capture the note's attack (first 80 ms).
            let n = sr as usize * 80 / 1000;
            let mut out = Vec::with_capacity(n);
            for i in 0..n {
                let x = (2.0 * PI * note * i as f32 / sr).sin() * 0.5;
                let a = amp.process(am, x, &knobs);
                let (l, r) = cab.process(cm, a, 0.5, 0.15, 0.15);
                out.push(l + r);
            }
            goertzel(&out, note * 2.0, sr) / goertzel(&out, note, sr).max(1e-9)
        };
        for (am, cm) in RIGS {
            let fresh = attack_h2_ratio(am, cm, false);
            let after = attack_h2_ratio(am, cm, true);
            assert!(
                (after - fresh).abs() < 0.10,
                "{}: note attack changes after other notes (h2/h1 {fresh:.3} → {after:.3})",
                am.name()
            );
        }
    }

    /// A low-E power chord through a high-gain, bass-heavy, mid-scooped rig (the
    /// "Pantera rhythm" worst case) must not turn into sub-bass mush: the inaudible
    /// difference-tone / rumble energy below the low-E fundamental must stay a small
    /// fraction of the musical body harmonics, and the three amp models must be
    /// roughly level-matched so switching models doesn't jump the volume.
    #[test]
    fn power_chord_low_end_is_tight_and_amps_level_matched() {
        let sr = 48_000.0;
        // E2 power chord: root + fifth + octave, like a palm-muted metal chord.
        let chord = [82.41f32, 123.47, 164.81];
        let run = |model: AmpModel| {
            let params = Arc::new(Params::new());
            params.amp_model.store(model as u8, Relaxed);
            params.ts_enabled.store(false, Relaxed);
            params.ds_enabled.store(true, Relaxed);
            params.ds_drive.store(0.72, Relaxed);
            params.ds_tone.store(0.68, Relaxed);
            params.ds_level.store(0.80, Relaxed);
            params.rev_enabled.store(false, Relaxed);
            params.ng_enabled.store(false, Relaxed);
            let knobs = amp::standard_knobs(model, 0.93, 0.82, 0.12, 0.86, 0.73, 0.65);
            for (i, &v) in knobs.iter().enumerate() {
                params.set_amp_knob(model, i, v);
            }
            let mut chain = DspChain::new(sr, params);
            let n = sr as usize;
            let warmup = sr as usize / 3;
            let mut out = Vec::with_capacity(n - warmup);
            for i in 0..n {
                let t = i as f32 / sr;
                let x: f32 = chord.iter().map(|&f| (2.0 * PI * f * t).sin()).sum::<f32>() * 0.18;
                let (l, _r) = chain.process(x);
                if i >= warmup {
                    out.push(l);
                }
            }
            let rms = (out.iter().map(|s| (s * s) as f64).sum::<f64>() / out.len() as f64).sqrt();
            // Perceived (mid-band) level, not raw RMS — see `amps_are_loudness_matched`.
            let mut hp = crate::dsp::biquad::Biquad::highpass(sr, 300.0, 0.707);
            let mut lp = crate::dsp::biquad::Biquad::lowpass(sr, 5000.0, 0.707);
            let mid: Vec<f32> = out.iter().map(|&x| lp.process(hp.process(x))).collect();
            let mid_rms =
                (mid.iter().map(|s| (s * s) as f64).sum::<f64>() / mid.len() as f64).sqrt();
            let m = |f| goertzel(&out, f, sr) as f64;
            let sub = m(41.0) + m(55.0); // sub / difference-tone fart
            let body = m(164.81) + m(247.0) + m(330.0); // musical body harmonics
            (rms, mid_rms, sub / body.max(1e-9))
        };

        let mut rms = Vec::new();
        let mut mid = Vec::new();
        for model in [AmpModel::Marshall, AmpModel::Mesa, AmpModel::Randall] {
            let (r, mr, sub_body) = run(model);
            assert!(
                sub_body < 0.45,
                "{} low end is farty: sub/body = {sub_body:.2}",
                model.name()
            );
            rms.push(r);
            mid.push(mr);
        }
        // Perceived (mid-band) loudness match so switching models doesn't jump the
        // volume. Raw RMS is retained above only as a headroom sanity check.
        let lo = mid.iter().cloned().fold(f64::INFINITY, f64::min);
        let hi = mid.iter().cloned().fold(0.0, f64::max);
        assert!(
            hi / lo < 1.6,
            "amps not perceptually level-matched: mid-band spread {hi:.4}/{lo:.4} = {:.2}x",
            hi / lo
        );
        // No model may run away in absolute (broadband) level.
        let raw_hi = rms.iter().cloned().fold(0.0, f64::max);
        assert!(
            raw_hi.is_finite() && raw_hi < 10.0,
            "amp runaway: {raw_hi:.4}"
        );
    }

    /// Every amp model should be stable (no NaN/blowup) at full gain.
    #[test]
    fn all_amps_stable_at_max_gain() {
        let sr = 48_000.0;
        for model in [AmpModel::Marshall, AmpModel::Mesa, AmpModel::Randall] {
            let mut bank = amp::AmpBank::new(sr);
            let knobs = amp::standard_knobs(model, 1.0, 0.5, 0.5, 0.7, 0.5, 0.7);
            let mut max_abs = 0.0f32;
            for n in 0..(sr as usize / 2) {
                let x = (2.0 * PI * 82.0 * n as f32 / sr).sin();
                let y = bank.process(model, x, &knobs);
                assert!(y.is_finite(), "{} produced non-finite output", model.name());
                max_abs = max_abs.max(y.abs());
            }
            assert!(max_abs < 4.0, "{} runaway: {max_abs}", model.name());
        }
    }
}
