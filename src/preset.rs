use anyhow::{Context, Result};
use rust_embed::Embed;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering::Relaxed;

use crate::dsp::{AmpModel, CabModel, ChainStage, Params, sanitize_chain_order};

#[derive(Embed)]
#[folder = "presets/"]
#[include = "*.toml"]
struct BundledPresets;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PresetSource {
    System,
    #[default]
    User,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Preset {
    pub name: String,
    pub description: Option<String>,
    /// Recording year the tone is inspired by, for the anachronism check. `None`
    /// for user presets and any file without a date.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub year: Option<u32>,
    #[serde(skip)]
    pub source: PresetSource,
    #[serde(skip)]
    pub path: Option<PathBuf>,
    pub noise_gate: Option<NgSection>,
    pub compressor: Option<CmpSection>,
    pub pitch: Option<PitchSection>,
    pub wah: Option<WahSection>,
    pub fuzz: Option<FuzzSection>,
    pub tube_screamer: TsSection,
    pub distortion: Option<DsSection>,
    pub metal_core: Option<MlSection>,
    pub preamp_eq: Option<PeqSection>,
    pub uni_vibe: Option<UniVibeSection>,
    pub boost: Option<BoostSection>,
    pub amp: AmpSection,
    pub cabinet: Option<CabSection>,
    pub graphic_eq: Option<GraphicEqSection>,
    pub eq: Option<EqSection>,
    pub flanger: Option<FlangerSection>,
    pub chorus: Option<ChorusSection>,
    pub phaser: Option<PhaserSection>,
    pub tremolo: Option<TremoloSection>,
    pub delay: Option<DelaySection>,
    pub reverb: ReverbSection,
    /// Studio-master output width. Absent in older presets → default (`1.3`).
    pub master: Option<MasterSection>,
    /// Signal-chain order as stage names (`"gate"`, `"comp"`, `"amp"`, `"cab"`,
    /// `"delay"`…). Absent in older presets → the shipped default order. Legacy
    /// `"ampcab"` entries migrate to `"amp"`, `"cab"`.
    pub chain: Option<ChainSection>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct NgSection {
    pub enabled: Option<bool>,
    pub threshold: f32,
    pub release: f32,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct CmpSection {
    pub enabled: Option<bool>,
    pub sustain: f32,
    pub attack: f32,
    pub level: f32,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct PitchSection {
    pub enabled: Option<bool>,
    pub pitch: f32,
    pub mix: f32,
    pub tone: f32,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct WahSection {
    pub enabled: Option<bool>,
    pub freq: f32,
    pub sens: f32,
    pub q: f32,
    pub mix: f32,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct PeqSection {
    pub enabled: Option<bool>,
    pub low: f32,
    pub mid: f32,
    pub high: f32,
}

/// Uni-Vibe — a front-of-amp four-stage all-pass "vibe". `mode` blends chorus
/// (0, dry + phase) to vibrato (1, phase only).
#[derive(Debug, Deserialize, Serialize)]
pub struct UniVibeSection {
    pub enabled: Option<bool>,
    pub rate: f32,
    pub depth: f32,
    pub mix: f32,
    pub mode: f32,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct FuzzSection {
    pub enabled: Option<bool>,
    pub fuzz: f32,
    pub tone: f32,
    pub level: f32,
    /// 0 = Big Muff, 1 = Fuzz Face. Defaults to 0 so existing presets keep the Muff.
    #[serde(default = "fuzz_type_default")]
    pub r#type: f32,
    /// Guitar volume as the fuzz sees it (1 = full). Defaults to 1 so existing
    /// presets are unchanged.
    #[serde(default = "fuzz_guitar_default")]
    pub guitar: f32,
}

fn fuzz_type_default() -> f32 {
    0.0
}

fn fuzz_guitar_default() -> f32 {
    1.0
}

#[derive(Debug, Deserialize, Serialize)]
pub struct TsSection {
    pub enabled: Option<bool>,
    pub drive: f32,
    pub tone: f32,
    pub level: f32,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct DsSection {
    pub enabled: Option<bool>,
    pub drive: f32,
    pub tone: f32,
    pub level: f32,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct MlSection {
    pub enabled: Option<bool>,
    pub dist: f32,
    pub low: f32,
    pub high: f32,
    pub level: f32,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct BoostSection {
    pub enabled: Option<bool>,
    pub gain: f32,
    pub treble: f32,
    pub bass: f32,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct AmpSection {
    /// "marshall" | "mesa" | "randall" | "vox" | "hiwatt" | "plexi" | "fender" | "supro" | "tweed"
    pub model: Option<String>,
    /// Model-specific front-panel knob positions, keyed by the control's stable
    /// slug (e.g. `gain`, `presence`, `cut`, `normal`). This is the canonical
    /// representation; the fixed fields below exist only to load presets written
    /// before per-model controls and are omitted when a preset is saved.
    #[serde(default)]
    pub knobs: std::collections::BTreeMap<String, f32>,
    // ── Legacy fixed fields (pre per-model controls) ──────────────────────────
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gain: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bass: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mid: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub treble: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub presence: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub master: Option<f32>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct CabSection {
    /// "mesa" (default) | "marshall" | "orange" | "wem" | "vox" | "fender"
    pub model: Option<String>,
    /// 0.0 = edge (off-axis, dark) … 1.0 = center (on-axis, bright). Default 0.5.
    #[serde(default = "mic_pos_default")]
    pub mic_pos: f32,
    /// 0.0 = close SM57 dynamic … 1.0 = R121 ribbon. Default 0.15.
    #[serde(default = "mic_blend_default")]
    pub mic_blend: f32,
    /// 0.0 = dry close mic only … 1.0 = full ambient room mic. Default 0.15.
    #[serde(default = "mic_room_default")]
    pub mic_room: f32,
}

fn mic_pos_default() -> f32 {
    0.5
}

fn mic_blend_default() -> f32 {
    0.15
}

fn mic_room_default() -> f32 {
    0.15
}

/// Boss GE-7 graphic EQ: seven band faders (low → high) plus an output level,
/// all 0–1 with 0.5 = flat/unity.
#[derive(Debug, Deserialize, Serialize)]
pub struct GraphicEqSection {
    pub enabled: Option<bool>,
    pub band1: f32,
    pub band2: f32,
    pub band3: f32,
    pub band4: f32,
    pub band5: f32,
    pub band6: f32,
    pub band7: f32,
    pub level: f32,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct EqSection {
    pub enabled: Option<bool>,
    pub low: f32,
    pub mid: f32,
    pub high: f32,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct DelaySection {
    pub enabled: Option<bool>,
    pub time: f32,
    pub feedback: f32,
    pub mix: f32,
    /// 0 = digital ping-pong, 1 = tape (Echoplex-style). Defaults to 0 so older
    /// presets keep the digital delay.
    #[serde(default = "delay_type_default")]
    pub r#type: f32,
}

fn delay_type_default() -> f32 {
    0.0
}

#[derive(Debug, Deserialize, Serialize)]
pub struct FlangerSection {
    pub enabled: Option<bool>,
    pub rate: f32,
    pub depth: f32,
    pub feedback: f32,
    pub mix: f32,
    /// 0 = generic stereo flanger, 1 = Electric Mistress.
    #[serde(default = "flanger_type_default")]
    pub r#type: f32,
}

fn flanger_type_default() -> f32 {
    0.0
}

#[derive(Debug, Deserialize, Serialize)]
pub struct ChorusSection {
    pub enabled: Option<bool>,
    pub rate: f32,
    pub depth: f32,
    pub mix: f32,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct PhaserSection {
    pub enabled: Option<bool>,
    pub rate: f32,
    pub depth: f32,
    pub feedback: f32,
    pub mix: f32,
    /// 0 = generic stereo phaser, 1 = Phase 90.
    #[serde(default = "phaser_type_default")]
    pub r#type: f32,
}

fn phaser_type_default() -> f32 {
    0.0
}

/// Tremolo / Vibrato: one LFO, blended between amplitude (tremolo) and pitch
/// (vibrato) modulation. All fields 0.0–1.0.
#[derive(Debug, Deserialize, Serialize)]
pub struct TremoloSection {
    pub enabled: Option<bool>,
    pub rate: f32,
    pub depth: f32,
    pub shape: f32,
    pub mode: f32,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct ReverbSection {
    pub enabled: Option<bool>,
    pub room: f32,
    pub damp: f32,
    pub mix: f32,
}

/// Studio-master output stage. `width` is the stereo widener: `1.0` = neutral
/// reference (untouched sides), `1.3` = the historic shipped sound. Optional —
/// omitting `[master]` resets the width to its default.
#[derive(Debug, Deserialize, Serialize)]
pub struct MasterSection {
    pub width: f32,
}

/// Signal-chain order: stage names from input to output, e.g.
/// `["gate", "comp", "fuzz", "amp", "cab", "delay", "reverb"]`. Unknown names
/// are ignored and missing stages are appended in default order on apply, so a
/// hand-edited or older file can never build a half chain. Legacy `"ampcab"`
/// expands to `"amp"`, `"cab"`.
#[derive(Debug, Deserialize, Serialize)]
pub struct ChainSection {
    pub order: Vec<String>,
}

impl Preset {
    pub fn load(path: &Path, source: PresetSource) -> Result<Self> {
        let src =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        let mut preset: Self =
            toml::from_str(&src).with_context(|| format!("parsing {}", path.display()))?;
        preset.source = source;
        preset.path = Some(path.to_path_buf());
        Ok(preset)
    }

    pub fn delete(&self) -> Result<()> {
        let path = self
            .path
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("preset has no file path"))?;
        std::fs::remove_file(path).with_context(|| format!("deleting {}", path.display()))
    }

    /// Copy this preset to an arbitrary `dest` path (typed in the UI).
    /// A `~` prefix resolves against the home directory. Overwrites an existing
    /// file: the user picks the destination each time, so re-exporting must work.
    /// System presets (which have no on-disk `path` when running from an
    /// installed binary) are serialized instead of copied.
    pub fn export_to(&self, dest: &Path) -> Result<PathBuf> {
        let dest = expand_tilde(dest);
        if let Some(parent) = dest.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        if let Some(src) = self.path.as_ref()
            && src.is_file()
        {
            std::fs::copy(src, &dest)
                .with_context(|| format!("exporting {} to {}", src.display(), dest.display()))?;
            return Ok(dest);
        }
        let toml_str = toml::to_string_pretty(self).with_context(|| "serializing preset")?;
        std::fs::write(&dest, toml_str)
            .with_context(|| format!("exporting to {}", dest.display()))?;
        Ok(dest)
    }

    /// Validate `src` as a preset and copy it into the user presets directory.
    /// Fails when a preset file with the resulting name already exists — the
    /// caller surfaces the error so nothing is silently overwritten.
    pub fn import_from(src: &Path) -> Result<PathBuf> {
        let src = expand_tilde(src);
        let preset = Self::load(&src, PresetSource::User)?;
        let dir = user_preset_dir()?;
        let filename = sanitize_filename(&preset.name);
        let dest = dir.join(format!("{filename}.toml"));
        if dest.exists() {
            return Err(anyhow::anyhow!(
                "already have a preset at {} — rename or delete it first",
                dest.display()
            ));
        }
        let toml_str = toml::to_string_pretty(&preset).with_context(|| "serializing preset")?;
        std::fs::write(&dest, toml_str)
            .with_context(|| format!("importing to {}", dest.display()))?;
        Ok(dest)
    }

    pub fn from_params(name: String, description: Option<String>, params: &Params) -> Self {
        let amp_model = AmpModel::from_u8(params.amp_model.load(Relaxed));
        let amp_model_str = match amp_model {
            AmpModel::Marshall => "marshall",
            AmpModel::Mesa => "mesa",
            AmpModel::Randall => "randall",
            AmpModel::Vox => "vox",
            AmpModel::Hiwatt => "hiwatt",
            AmpModel::Plexi => "plexi",
            AmpModel::Fender => "fender",
            AmpModel::Supro => "supro",
            AmpModel::Tweed => "tweed",
        };
        let cab_model = CabModel::from_u8(params.cab_model.load(Relaxed));
        let cab_model_str = match cab_model {
            CabModel::Mesa => "mesa",
            CabModel::Marshall => "marshall",
            CabModel::Orange => "orange",
            CabModel::Wem => "wem",
            CabModel::Vox => "vox",
            CabModel::Fender => "fender",
            CabModel::Supro => "supro",
            CabModel::Tweed => "tweed",
        };
        Self {
            name,
            description,
            year: None,
            source: PresetSource::User,
            path: None,
            noise_gate: Some(NgSection {
                enabled: Some(params.ng_enabled.load(Relaxed)),
                threshold: params.ng_threshold.load(Relaxed),
                release: params.ng_release.load(Relaxed),
            }),
            compressor: Some(CmpSection {
                enabled: Some(params.cmp_enabled.load(Relaxed)),
                sustain: params.cmp_sustain.load(Relaxed),
                attack: params.cmp_attack.load(Relaxed),
                level: params.cmp_level.load(Relaxed),
            }),
            pitch: Some(PitchSection {
                enabled: Some(params.pitch_enabled.load(Relaxed)),
                pitch: params.pitch_pitch.load(Relaxed),
                mix: params.pitch_mix.load(Relaxed),
                tone: params.pitch_tone.load(Relaxed),
            }),
            wah: Some(WahSection {
                enabled: Some(params.wah_enabled.load(Relaxed)),
                freq: params.wah_freq.load(Relaxed),
                sens: params.wah_sens.load(Relaxed),
                q: params.wah_q.load(Relaxed),
                mix: params.wah_mix.load(Relaxed),
            }),
            fuzz: Some(FuzzSection {
                enabled: Some(params.fz_enabled.load(Relaxed)),
                fuzz: params.fz_fuzz.load(Relaxed),
                tone: params.fz_tone.load(Relaxed),
                level: params.fz_level.load(Relaxed),
                r#type: params.fz_type.load(Relaxed),
                guitar: params.fz_guitar.load(Relaxed),
            }),
            tube_screamer: TsSection {
                enabled: Some(params.ts_enabled.load(Relaxed)),
                drive: params.ts_drive.load(Relaxed),
                tone: params.ts_tone.load(Relaxed),
                level: params.ts_level.load(Relaxed),
            },
            distortion: Some(DsSection {
                enabled: Some(params.ds_enabled.load(Relaxed)),
                drive: params.ds_drive.load(Relaxed),
                tone: params.ds_tone.load(Relaxed),
                level: params.ds_level.load(Relaxed),
            }),
            metal_core: Some(MlSection {
                enabled: Some(params.ml_enabled.load(Relaxed)),
                dist: params.ml_dist.load(Relaxed),
                low: params.ml_low.load(Relaxed),
                high: params.ml_high.load(Relaxed),
                level: params.ml_level.load(Relaxed),
            }),
            preamp_eq: Some(PeqSection {
                enabled: Some(params.peq_enabled.load(Relaxed)),
                low: params.peq_low.load(Relaxed),
                mid: params.peq_mid.load(Relaxed),
                high: params.peq_high.load(Relaxed),
            }),
            uni_vibe: Some(UniVibeSection {
                enabled: Some(params.uv_enabled.load(Relaxed)),
                rate: params.uv_rate.load(Relaxed),
                depth: params.uv_depth.load(Relaxed),
                mix: params.uv_mix.load(Relaxed),
                mode: params.uv_mode.load(Relaxed),
            }),
            boost: Some(BoostSection {
                enabled: Some(params.boost_enabled.load(Relaxed)),
                gain: params.boost_gain.load(Relaxed),
                treble: params.boost_treble.load(Relaxed),
                bass: params.boost_bass.load(Relaxed),
            }),
            amp: AmpSection {
                model: Some(amp_model_str.to_string()),
                knobs: amp_model
                    .controls()
                    .iter()
                    .enumerate()
                    .map(|(i, k)| (k.slug.to_string(), params.amp_knob_value(amp_model, i)))
                    .collect(),
                gain: None,
                bass: None,
                mid: None,
                treble: None,
                presence: None,
                master: None,
            },
            cabinet: Some(CabSection {
                model: Some(cab_model_str.to_string()),
                mic_pos: params.mic_pos.load(Relaxed),
                mic_blend: params.mic_blend.load(Relaxed),
                mic_room: params.mic_room.load(Relaxed),
            }),
            graphic_eq: Some(GraphicEqSection {
                enabled: Some(params.geq_enabled.load(Relaxed)),
                band1: params.geq_b1.load(Relaxed),
                band2: params.geq_b2.load(Relaxed),
                band3: params.geq_b3.load(Relaxed),
                band4: params.geq_b4.load(Relaxed),
                band5: params.geq_b5.load(Relaxed),
                band6: params.geq_b6.load(Relaxed),
                band7: params.geq_b7.load(Relaxed),
                level: params.geq_level.load(Relaxed),
            }),
            eq: Some(EqSection {
                enabled: Some(params.eq_enabled.load(Relaxed)),
                low: params.eq_low.load(Relaxed),
                mid: params.eq_mid.load(Relaxed),
                high: params.eq_high.load(Relaxed),
            }),
            delay: Some(DelaySection {
                enabled: Some(params.delay_enabled.load(Relaxed)),
                time: params.delay_time.load(Relaxed),
                feedback: params.delay_feedback.load(Relaxed),
                mix: params.delay_mix.load(Relaxed),
                r#type: params.delay_type.load(Relaxed),
            }),
            flanger: Some(FlangerSection {
                enabled: Some(params.fl_enabled.load(Relaxed)),
                rate: params.fl_rate.load(Relaxed),
                depth: params.fl_depth.load(Relaxed),
                feedback: params.fl_feedback.load(Relaxed),
                mix: params.fl_mix.load(Relaxed),
                r#type: params.fl_type.load(Relaxed),
            }),
            chorus: Some(ChorusSection {
                enabled: Some(params.ch_enabled.load(Relaxed)),
                rate: params.ch_rate.load(Relaxed),
                depth: params.ch_depth.load(Relaxed),
                mix: params.ch_mix.load(Relaxed),
            }),
            phaser: Some(PhaserSection {
                enabled: Some(params.ph_enabled.load(Relaxed)),
                rate: params.ph_rate.load(Relaxed),
                depth: params.ph_depth.load(Relaxed),
                feedback: params.ph_feedback.load(Relaxed),
                mix: params.ph_mix.load(Relaxed),
                r#type: params.ph_type.load(Relaxed),
            }),
            tremolo: Some(TremoloSection {
                enabled: Some(params.trem_enabled.load(Relaxed)),
                rate: params.trem_rate.load(Relaxed),
                depth: params.trem_depth.load(Relaxed),
                shape: params.trem_shape.load(Relaxed),
                mode: params.trem_mode.load(Relaxed),
            }),
            reverb: ReverbSection {
                enabled: Some(params.rev_enabled.load(Relaxed)),
                room: params.rev_room.load(Relaxed),
                damp: params.rev_damp.load(Relaxed),
                mix: params.rev_mix.load(Relaxed),
            },
            master: Some(MasterSection {
                width: params.master_width.load(Relaxed),
            }),
            chain: Some(ChainSection {
                order: params
                    .chain_slots()
                    .iter()
                    .filter_map(|&v| ChainStage::from_u8(v))
                    .map(|s| s.name().to_owned())
                    .collect(),
            }),
        }
    }

    pub fn save_to_user_dir(&self) -> Result<PathBuf> {
        let dir = user_preset_dir()?;

        let filename = sanitize_filename(&self.name);
        let path = dir.join(format!("{filename}.toml"));

        let toml_str = toml::to_string_pretty(self).with_context(|| "serializing preset")?;
        std::fs::write(&path, toml_str)?;
        Ok(path)
    }

    /// Write all preset values into the shared atomic params.
    pub fn apply(&self, params: &Params) {
        if let Some(ng) = &self.noise_gate {
            params.ng_enabled.store(ng.enabled.unwrap_or(true), Relaxed);
            params
                .ng_threshold
                .store(ng.threshold.clamp(0.0, 1.0), Relaxed);
            params.ng_release.store(ng.release.clamp(0.0, 1.0), Relaxed);
        }

        if let Some(cmp) = &self.compressor {
            params
                .cmp_enabled
                .store(cmp.enabled.unwrap_or(true), Relaxed);
            params
                .cmp_sustain
                .store(cmp.sustain.clamp(0.0, 1.0), Relaxed);
            params.cmp_attack.store(cmp.attack.clamp(0.0, 1.0), Relaxed);
            params.cmp_level.store(cmp.level.clamp(0.0, 1.0), Relaxed);
        } else {
            params.cmp_enabled.store(false, Relaxed);
        }

        if let Some(pitch) = &self.pitch {
            params
                .pitch_enabled
                .store(pitch.enabled.unwrap_or(true), Relaxed);
            params
                .pitch_pitch
                .store(pitch.pitch.clamp(0.0, 1.0), Relaxed);
            params.pitch_mix.store(pitch.mix.clamp(0.0, 1.0), Relaxed);
            params.pitch_tone.store(pitch.tone.clamp(0.0, 1.0), Relaxed);
        } else {
            params.pitch_enabled.store(false, Relaxed);
        }

        if let Some(wah) = &self.wah {
            params
                .wah_enabled
                .store(wah.enabled.unwrap_or(true), Relaxed);
            params.wah_freq.store(wah.freq.clamp(0.0, 1.0), Relaxed);
            params.wah_sens.store(wah.sens.clamp(0.0, 1.0), Relaxed);
            params.wah_q.store(wah.q.clamp(0.0, 1.0), Relaxed);
            params.wah_mix.store(wah.mix.clamp(0.0, 1.0), Relaxed);
        } else {
            params.wah_enabled.store(false, Relaxed);
        }

        if let Some(fz) = &self.fuzz {
            params.fz_enabled.store(fz.enabled.unwrap_or(true), Relaxed);
            params.fz_fuzz.store(fz.fuzz.clamp(0.0, 1.0), Relaxed);
            params.fz_tone.store(fz.tone.clamp(0.0, 1.0), Relaxed);
            params.fz_level.store(fz.level.clamp(0.0, 1.0), Relaxed);
            params.fz_type.store(fz.r#type.clamp(0.0, 1.0), Relaxed);
            params.fz_guitar.store(fz.guitar.clamp(0.0, 1.0), Relaxed);
        } else {
            params.fz_enabled.store(false, Relaxed);
        }

        let ts = &self.tube_screamer;
        params.ts_enabled.store(ts.enabled.unwrap_or(true), Relaxed);
        params.ts_drive.store(ts.drive.clamp(0.0, 1.0), Relaxed);
        params.ts_tone.store(ts.tone.clamp(0.0, 1.0), Relaxed);
        params.ts_level.store(ts.level.clamp(0.0, 1.0), Relaxed);

        if let Some(ds) = &self.distortion {
            params.ds_enabled.store(ds.enabled.unwrap_or(true), Relaxed);
            params.ds_drive.store(ds.drive.clamp(0.0, 1.0), Relaxed);
            params.ds_tone.store(ds.tone.clamp(0.0, 1.0), Relaxed);
            params.ds_level.store(ds.level.clamp(0.0, 1.0), Relaxed);
        } else {
            params.ds_enabled.store(false, Relaxed);
        }

        if let Some(ml) = &self.metal_core {
            params.ml_enabled.store(ml.enabled.unwrap_or(true), Relaxed);
            params.ml_dist.store(ml.dist.clamp(0.0, 1.0), Relaxed);
            params.ml_low.store(ml.low.clamp(0.0, 1.0), Relaxed);
            params.ml_high.store(ml.high.clamp(0.0, 1.0), Relaxed);
            params.ml_level.store(ml.level.clamp(0.0, 1.0), Relaxed);
        } else {
            params.ml_enabled.store(false, Relaxed);
        }

        if let Some(peq) = &self.preamp_eq {
            params
                .peq_enabled
                .store(peq.enabled.unwrap_or(true), Relaxed);
            params.peq_low.store(peq.low.clamp(0.0, 1.0), Relaxed);
            params.peq_mid.store(peq.mid.clamp(0.0, 1.0), Relaxed);
            params.peq_high.store(peq.high.clamp(0.0, 1.0), Relaxed);
        } else {
            params.peq_enabled.store(false, Relaxed);
        }

        if let Some(uv) = &self.uni_vibe {
            params.uv_enabled.store(uv.enabled.unwrap_or(true), Relaxed);
            params.uv_rate.store(uv.rate.clamp(0.0, 1.0), Relaxed);
            params.uv_depth.store(uv.depth.clamp(0.0, 1.0), Relaxed);
            params.uv_mix.store(uv.mix.clamp(0.0, 1.0), Relaxed);
            params.uv_mode.store(uv.mode.clamp(0.0, 1.0), Relaxed);
        } else {
            params.uv_enabled.store(false, Relaxed);
        }

        if let Some(boost) = &self.boost {
            params
                .boost_enabled
                .store(boost.enabled.unwrap_or(true), Relaxed);
            params.boost_gain.store(boost.gain.clamp(0.0, 1.0), Relaxed);
            params
                .boost_treble
                .store(boost.treble.clamp(0.0, 1.0), Relaxed);
            params.boost_bass.store(boost.bass.clamp(0.0, 1.0), Relaxed);
        } else {
            params.boost_enabled.store(false, Relaxed);
        }

        let amp = &self.amp;
        let model = match amp.model.as_deref() {
            Some("mesa") => AmpModel::Mesa,
            Some("randall") => AmpModel::Randall,
            Some("vox") => AmpModel::Vox,
            Some("hiwatt") => AmpModel::Hiwatt,
            Some("plexi") => AmpModel::Plexi,
            Some("fender") => AmpModel::Fender,
            Some("supro") => AmpModel::Supro,
            Some("tweed") => AmpModel::Tweed,
            _ => AmpModel::Marshall,
        };
        params.amp_model.store(model as u8, Relaxed);
        // Start from the model's declared defaults, overlay any legacy fixed fields
        // (pre per-model presets) by role, then the canonical `knobs` map.
        for (i, knob) in model.controls().iter().enumerate() {
            params.set_amp_knob(model, i, knob.default);
        }
        for (field, value) in [
            ("gain", amp.gain),
            ("bass", amp.bass),
            ("mid", amp.mid),
            ("treble", amp.treble),
            ("presence", amp.presence),
            ("master", amp.master),
        ] {
            if let (Some(slot), Some(v)) = (model.knob_slot(field), value) {
                params.set_amp_knob(model, slot, v);
            }
        }
        for (slug, &v) in &amp.knobs {
            if let Some(slot) = model.knob_slot(slug) {
                params.set_amp_knob(model, slot, v);
            }
        }

        if let Some(cab) = &self.cabinet {
            let cab_model = match cab.model.as_deref() {
                Some("marshall") => CabModel::Marshall,
                Some("orange") => CabModel::Orange,
                Some("wem") => CabModel::Wem,
                Some("vox") => CabModel::Vox,
                Some("fender") => CabModel::Fender,
                Some("supro") => CabModel::Supro,
                Some("tweed") => CabModel::Tweed,
                _ => CabModel::Mesa,
            };
            params.cab_model.store(cab_model as u8, Relaxed);
            params.mic_pos.store(cab.mic_pos.clamp(0.0, 1.0), Relaxed);
            params
                .mic_blend
                .store(cab.mic_blend.clamp(0.0, 1.0), Relaxed);
            params.mic_room.store(cab.mic_room.clamp(0.0, 1.0), Relaxed);
        }

        if let Some(geq) = &self.graphic_eq {
            params
                .geq_enabled
                .store(geq.enabled.unwrap_or(true), Relaxed);
            params.geq_b1.store(geq.band1.clamp(0.0, 1.0), Relaxed);
            params.geq_b2.store(geq.band2.clamp(0.0, 1.0), Relaxed);
            params.geq_b3.store(geq.band3.clamp(0.0, 1.0), Relaxed);
            params.geq_b4.store(geq.band4.clamp(0.0, 1.0), Relaxed);
            params.geq_b5.store(geq.band5.clamp(0.0, 1.0), Relaxed);
            params.geq_b6.store(geq.band6.clamp(0.0, 1.0), Relaxed);
            params.geq_b7.store(geq.band7.clamp(0.0, 1.0), Relaxed);
            params.geq_level.store(geq.level.clamp(0.0, 1.0), Relaxed);
        } else {
            params.geq_enabled.store(false, Relaxed);
        }

        if let Some(eq) = &self.eq {
            params.eq_enabled.store(eq.enabled.unwrap_or(true), Relaxed);
            params.eq_low.store(eq.low.clamp(0.0, 1.0), Relaxed);
            params.eq_mid.store(eq.mid.clamp(0.0, 1.0), Relaxed);
            params.eq_high.store(eq.high.clamp(0.0, 1.0), Relaxed);
        } else {
            params.eq_enabled.store(false, Relaxed);
        }

        if let Some(dly) = &self.delay {
            params
                .delay_enabled
                .store(dly.enabled.unwrap_or(true), Relaxed);
            params.delay_time.store(dly.time.clamp(0.0, 1.0), Relaxed);
            params
                .delay_feedback
                .store(dly.feedback.clamp(0.0, 1.0), Relaxed);
            params.delay_mix.store(dly.mix.clamp(0.0, 1.0), Relaxed);
            params.delay_type.store(dly.r#type.clamp(0.0, 1.0), Relaxed);
        } else {
            params.delay_enabled.store(false, Relaxed);
        }

        if let Some(fl) = &self.flanger {
            params.fl_enabled.store(fl.enabled.unwrap_or(true), Relaxed);
            params.fl_rate.store(fl.rate.clamp(0.0, 1.0), Relaxed);
            params.fl_depth.store(fl.depth.clamp(0.0, 1.0), Relaxed);
            params
                .fl_feedback
                .store(fl.feedback.clamp(0.0, 1.0), Relaxed);
            params.fl_mix.store(fl.mix.clamp(0.0, 1.0), Relaxed);
            params.fl_type.store(fl.r#type.clamp(0.0, 1.0), Relaxed);
        } else {
            params.fl_enabled.store(false, Relaxed);
        }

        if let Some(ch) = &self.chorus {
            params.ch_enabled.store(ch.enabled.unwrap_or(true), Relaxed);
            params.ch_rate.store(ch.rate.clamp(0.0, 1.0), Relaxed);
            params.ch_depth.store(ch.depth.clamp(0.0, 1.0), Relaxed);
            params.ch_mix.store(ch.mix.clamp(0.0, 1.0), Relaxed);
        } else {
            params.ch_enabled.store(false, Relaxed);
        }

        if let Some(ph) = &self.phaser {
            params.ph_enabled.store(ph.enabled.unwrap_or(true), Relaxed);
            params.ph_rate.store(ph.rate.clamp(0.0, 1.0), Relaxed);
            params.ph_depth.store(ph.depth.clamp(0.0, 1.0), Relaxed);
            params
                .ph_feedback
                .store(ph.feedback.clamp(0.0, 1.0), Relaxed);
            params.ph_mix.store(ph.mix.clamp(0.0, 1.0), Relaxed);
            params.ph_type.store(ph.r#type.clamp(0.0, 1.0), Relaxed);
        } else {
            params.ph_enabled.store(false, Relaxed);
        }

        if let Some(tr) = &self.tremolo {
            params
                .trem_enabled
                .store(tr.enabled.unwrap_or(true), Relaxed);
            params.trem_rate.store(tr.rate.clamp(0.0, 1.0), Relaxed);
            params.trem_depth.store(tr.depth.clamp(0.0, 1.0), Relaxed);
            params.trem_shape.store(tr.shape.clamp(0.0, 1.0), Relaxed);
            params.trem_mode.store(tr.mode.clamp(0.0, 1.0), Relaxed);
        } else {
            params.trem_enabled.store(false, Relaxed);
        }

        let rev = &self.reverb;
        params
            .rev_enabled
            .store(rev.enabled.unwrap_or(true), Relaxed);
        params.rev_room.store(rev.room.clamp(0.0, 1.0), Relaxed);
        params.rev_damp.store(rev.damp.clamp(0.0, 1.0), Relaxed);
        params.rev_mix.store(rev.mix.clamp(0.0, 1.0), Relaxed);

        // Studio master: omitted → the default width, so loading is deterministic.
        match &self.master {
            Some(m) => params.master_width.store(m.width.clamp(0.0, 2.0), Relaxed),
            None => params
                .master_width
                .store(crate::dsp::DEFAULT_MASTER_WIDTH, Relaxed),
        }

        if let Some(chain) = &self.chain {
            // Names → stage ids. `"ampcab"` is the legacy pre-split combined
            // block: expand it to consecutive `"amp"`, `"cab"` at the same spot so
            // old presets keep their exact topology. `sanitize_chain_order` then
            // drops unknowns/dupes, appends missing stages, and repairs a cab
            // placed before its amp.
            let ids: Vec<u8> = chain
                .order
                .iter()
                .flat_map(|n| {
                    let n = n.trim().to_lowercase();
                    if n == "ampcab" {
                        vec![ChainStage::Amp as u8, ChainStage::Cab as u8]
                    } else {
                        ChainStage::from_name(&n)
                            .map(|s| s as u8)
                            .into_iter()
                            .collect()
                    }
                })
                .collect();
            params.set_chain_order(&sanitize_chain_order(&ids));
        } else {
            // Older presets predate chain order: fall back to the shipped order.
            params.set_chain_order(&ChainStage::default_order());
        }
    }
}

// ── Discovery ─────────────────────────────────────────────────────────────────

/// The user presets directory, created on demand.
fn user_preset_dir() -> Result<PathBuf> {
    let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("cannot find home dir"))?;
    let dir = home.join(".config").join("rusty-riff").join("presets");
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Map a preset display name to a filesystem-safe `snake_case` file stem.
fn sanitize_filename(name: &str) -> String {
    name.to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '_' })
        .collect()
}

/// Resolve a leading `~` against the home directory so typed paths behave like
/// a shell. Non-tilde paths pass through unchanged.
fn expand_tilde(path: &Path) -> PathBuf {
    let s = path.to_string_lossy();
    if let Some(home) = dirs::home_dir() {
        if s == "~" {
            return home;
        }
        if let Some(rest) = s.strip_prefix("~/") {
            return home.join(rest);
        }
    }
    path.to_path_buf()
}

pub fn find_preset_files() -> Vec<(PathBuf, PresetSource)> {
    let system_dir = PathBuf::from("presets");
    let mut result: Vec<(PathBuf, PresetSource)> = Vec::new();

    let scan = |dir: &PathBuf, source: PresetSource| -> Vec<(PathBuf, PresetSource)> {
        if let Ok(entries) = std::fs::read_dir(dir) {
            let mut files: Vec<PathBuf> = entries
                .filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|ext| ext == "toml"))
                .collect();
            files.sort();
            files.into_iter().map(|p| (p, source)).collect()
        } else {
            vec![]
        }
    };

    result.extend(scan(&system_dir, PresetSource::System));

    if let Some(home) = dirs::home_dir() {
        let user_dir = home.join(".config").join("rusty-riff").join("presets");
        result.extend(scan(&user_dir, PresetSource::User));
    }

    result
}

fn load_embedded() -> Vec<Preset> {
    let mut names: Vec<String> = BundledPresets::iter().map(|n| n.into_owned()).collect();
    names.sort();
    names
        .into_iter()
        .filter_map(|name| {
            let file = BundledPresets::get(&name)?;
            let src = std::str::from_utf8(file.data.as_ref()).ok()?;
            let mut preset: Preset = toml::from_str(src)
                .map_err(|e| eprintln!("Warning: skipping embedded preset {name}: {e}"))
                .ok()?;
            preset.source = PresetSource::System;
            preset.path = None;
            Some(preset)
        })
        .collect()
}

pub fn load_all() -> Vec<Preset> {
    let from_disk: Vec<Preset> = find_preset_files()
        .into_iter()
        .filter_map(|(path, source)| {
            Preset::load(&path, source)
                .map_err(|e| eprintln!("Warning: skipping preset {}: {e}", path.display()))
                .ok()
        })
        .collect();

    // If the ./presets/ directory is absent (installed binary), fall back to embedded.
    let system_on_disk = from_disk.iter().any(|p| p.source == PresetSource::System);
    if system_on_disk {
        from_disk
    } else {
        let mut all = load_embedded();
        all.extend(from_disk);
        all
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every bundled preset must deserialize against the current schema — guards
    /// against a typo or a renamed field silently breaking a shipped preset.
    #[test]
    fn all_bundled_presets_parse() {
        let mut count = 0;
        for entry in std::fs::read_dir("presets").expect("presets/ dir") {
            let p = entry.unwrap().path();
            if p.extension().is_some_and(|ext| ext == "toml") {
                Preset::load(&p, PresetSource::System)
                    .unwrap_or_else(|e| panic!("failed to parse {}: {e}", p.display()));
                count += 1;
            }
        }
        assert!(count > 0, "no bundled presets found to validate");
    }

    /// Intro year of a few named devices, for the anachronism check.
    const DEVICE_YEARS: &[(&str, u32)] = &[
        ("tube_screamer", 1979), // Ibanez TS-808
        ("distortion", 1978),    // Boss DS-1
        ("metal_core", 2004),    // Boss ML-2
        ("boost", 1972),         // Colorsound Power Boost (approx.)
    ];

    /// Known anachronisms accepted for now. Empty — Phase 5 Batch 1 removed the
    /// last cases (the TS-808 in Shine On and Hotel California, replaced by the
    /// Power Boost and the tweed Deluxe respectively).
    const KNOWN_ANACHRONISMS: &[(&str, &str)] = &[];

    /// A bundled preset must not enable a device that did not exist when the tone
    /// was recorded, except for the documented known cases above.
    #[test]
    fn no_new_device_anachronisms() {
        for entry in std::fs::read_dir("presets").expect("presets/ dir") {
            let path = entry.unwrap().path();
            if path.extension().is_none_or(|e| e != "toml") {
                continue;
            }
            let stem = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_owned();
            let preset = Preset::load(&path, PresetSource::System)
                .unwrap_or_else(|e| panic!("failed to parse {}: {e}", path.display()));
            let Some(year) = preset.year else {
                continue;
            };
            let mut enabled: Vec<&str> = Vec::new();
            if preset.tube_screamer.enabled.unwrap_or(false) {
                enabled.push("tube_screamer");
            }
            if preset
                .distortion
                .as_ref()
                .and_then(|s| s.enabled)
                .unwrap_or(false)
            {
                enabled.push("distortion");
            }
            if preset
                .metal_core
                .as_ref()
                .and_then(|s| s.enabled)
                .unwrap_or(false)
            {
                enabled.push("metal_core");
            }
            if preset
                .boost
                .as_ref()
                .and_then(|s| s.enabled)
                .unwrap_or(false)
            {
                enabled.push("boost");
            }
            for dev in enabled {
                let Some((_, debut)) = DEVICE_YEARS.iter().find(|(n, _)| *n == dev) else {
                    continue;
                };
                if *debut > year {
                    assert!(
                        KNOWN_ANACHRONISMS.contains(&(stem.as_str(), dev)),
                        "{stem} (year {year}) enables {dev} (debut {debut}) — \
                         not in KNOWN_ANACHRONISMS"
                    );
                }
            }
        }
    }

    /// A sound-determining fingerprint: chain order, amp/cab/mic/master
    /// selectors, every stage's on/off flag, the active amp's full knob bank, and
    /// every knob (including `fz_type`/`delay_type`) of each **enabled** stage.
    /// Knob values of *disabled* effects are intentionally retained by `apply`,
    /// so they are not part of the audible rig and are excluded.
    fn rig_fingerprint(p: &Params) -> Vec<f32> {
        let mut v: Vec<f32> = p.chain_slots().iter().map(|&b| f32::from(b)).collect();
        v.push((p.amp_model() as u8) as f32);
        v.push((p.cab_model() as u8) as f32);
        v.push(p.mic_pos.load(Relaxed));
        v.push(p.mic_blend.load(Relaxed));
        v.push(p.mic_room.load(Relaxed));
        v.push(p.master_width.load(Relaxed));
        for flag in [
            &p.ng_enabled,
            &p.cmp_enabled,
            &p.pitch_enabled,
            &p.wah_enabled,
            &p.fz_enabled,
            &p.ts_enabled,
            &p.ds_enabled,
            &p.ml_enabled,
            &p.peq_enabled,
            &p.uv_enabled,
            &p.boost_enabled,
            &p.geq_enabled,
            &p.eq_enabled,
            &p.fl_enabled,
            &p.ch_enabled,
            &p.ph_enabled,
            &p.trem_enabled,
            &p.delay_enabled,
            &p.rev_enabled,
        ] {
            v.push(if flag.load(Relaxed) { 1.0 } else { 0.0 });
        }

        // The active amp's *used* knob bank (the amp is always live). Slots beyond
        // the model's control count are padding and are not audible, so they are
        // excluded (a preset legitimately leaves them at the prior value).
        let model = p.amp_model();
        for i in 0..model.controls().len() {
            v.push(p.amp_params[model as usize][i].load(Relaxed));
        }

        // Every knob of each enabled stage, read through the same fields the
        // `mono_stage!` / `stereo_stage!` macros use.
        macro_rules! knobs {
            ($enabled:ident, $($param:ident),+) => {
                if p.$enabled.load(Relaxed) {
                    $( v.push(p.$param.load(Relaxed)); )+
                }
            };
        }
        knobs!(ng_enabled, ng_threshold, ng_release);
        knobs!(pitch_enabled, pitch_pitch, pitch_mix, pitch_tone);
        knobs!(wah_enabled, wah_freq, wah_sens, wah_q, wah_mix);
        knobs!(cmp_enabled, cmp_sustain, cmp_attack, cmp_level);
        knobs!(fz_enabled, fz_fuzz, fz_tone, fz_level, fz_type, fz_guitar);
        knobs!(ts_enabled, ts_drive, ts_tone, ts_level);
        knobs!(ds_enabled, ds_drive, ds_tone, ds_level);
        knobs!(ml_enabled, ml_dist, ml_low, ml_high, ml_level);
        knobs!(peq_enabled, peq_low, peq_mid, peq_high);
        knobs!(uv_enabled, uv_rate, uv_depth, uv_mix, uv_mode);
        knobs!(boost_enabled, boost_gain, boost_treble, boost_bass);
        knobs!(
            geq_enabled,
            geq_b1,
            geq_b2,
            geq_b3,
            geq_b4,
            geq_b5,
            geq_b6,
            geq_b7,
            geq_level
        );
        knobs!(eq_enabled, eq_low, eq_mid, eq_high);
        knobs!(fl_enabled, fl_rate, fl_depth, fl_feedback, fl_mix, fl_type);
        knobs!(ch_enabled, ch_rate, ch_depth, ch_mix);
        knobs!(ph_enabled, ph_rate, ph_depth, ph_feedback, ph_mix, ph_type);
        knobs!(trem_enabled, trem_rate, trem_depth, trem_shape, trem_mode);
        knobs!(
            delay_enabled,
            delay_time,
            delay_feedback,
            delay_mix,
            delay_type
        );
        knobs!(rev_enabled, rev_room, rev_damp, rev_mix);
        v
    }

    /// A prior rig deliberately different from the shipped defaults in every way
    /// a preset could fail to overwrite.
    fn hostile_params() -> Params {
        let p = Params::new();
        p.amp_model.store(AmpModel::Randall as u8, Relaxed);
        p.cab_model.store(CabModel::Vox as u8, Relaxed);
        p.mic_pos.store(0.95, Relaxed);
        p.mic_blend.store(1.0, Relaxed);
        p.mic_room.store(1.0, Relaxed);
        p.master_width.store(0.2, Relaxed);
        // Gate off (default is on) so an omitted `[noise_gate]` would leak state.
        p.ng_enabled.store(false, Relaxed);
        // Effects most presets leave off, flipped on with stray state.
        p.pitch_enabled.store(true, Relaxed);
        p.wah_enabled.store(true, Relaxed);
        p.uv_enabled.store(true, Relaxed);
        p.geq_enabled.store(true, Relaxed);
        p.fl_enabled.store(true, Relaxed);
        p.ch_enabled.store(true, Relaxed);
        p.ph_enabled.store(true, Relaxed);
        p.trem_enabled.store(true, Relaxed);
        // A reversed chain, so a preset without `[chain]` must reset it.
        let mut reversed = ChainStage::default_order();
        reversed.reverse();
        p.set_chain_order(&reversed);
        // Every knob scrambled, so any value a preset fails to overwrite is
        // detectable either in the fingerprint or in the rendered audio.
        macro_rules! scramble {
            ($($param:ident),+ $(,)?) => {
                $( p.$param.store(0.93, Relaxed); )+
            };
        }
        scramble!(
            ng_threshold,
            ng_release,
            pitch_pitch,
            pitch_mix,
            pitch_tone,
            wah_freq,
            wah_sens,
            wah_q,
            wah_mix,
            cmp_sustain,
            cmp_attack,
            cmp_level,
            fz_fuzz,
            fz_tone,
            fz_level,
            ts_drive,
            ts_tone,
            ts_level,
            ds_drive,
            ds_tone,
            ds_level,
            ml_dist,
            ml_low,
            ml_high,
            ml_level,
            peq_low,
            peq_mid,
            peq_high,
            uv_rate,
            uv_depth,
            uv_mix,
            uv_mode,
            geq_b1,
            geq_b2,
            geq_b3,
            geq_b4,
            geq_b5,
            geq_b6,
            geq_b7,
            geq_level,
            eq_low,
            eq_mid,
            eq_high,
            fl_rate,
            fl_depth,
            fl_feedback,
            fl_mix,
            ch_rate,
            ch_depth,
            ch_mix,
            ph_rate,
            ph_depth,
            ph_feedback,
            ph_mix,
            trem_rate,
            trem_depth,
            trem_shape,
            trem_mode,
            delay_time,
            delay_feedback,
            delay_mix,
            rev_room,
            rev_damp,
            rev_mix,
        );
        p.fz_type.store(1.0, Relaxed);
        p.delay_type.store(1.0, Relaxed);
        for bank in &p.amp_params {
            for knob in bank.iter() {
                knob.store(0.93, Relaxed);
            }
        }
        p
    }

    /// The Phase 5 gate: loading any bundled preset gives the same audible rig
    /// no matter what was loaded before it. All bundled presets set
    /// `[noise_gate]`/`[cabinet]` and omit `[chain]`, so a hostile prior rig must
    /// be fully overwritten. Guards against a future preset that forgets one.
    #[test]
    fn bundled_presets_load_deterministically() {
        for entry in std::fs::read_dir("presets").expect("presets/ dir") {
            let path = entry.unwrap().path();
            if path.extension().is_none_or(|ext| ext != "toml") {
                continue;
            }
            let preset = Preset::load(&path, PresetSource::System)
                .unwrap_or_else(|e| panic!("failed to parse {}: {e}", path.display()));

            let fresh = Params::new();
            preset.apply(&fresh);
            let hostile = hostile_params();
            preset.apply(&hostile);

            assert_eq!(
                rig_fingerprint(&fresh),
                rig_fingerprint(&hostile),
                "{} is not deterministic across prior states",
                path.display()
            );
        }
    }

    /// The render-level determinism gate: a bundled preset must produce
    /// bit-identical audio whether it was applied to a fresh or a hostile prior
    /// rig. Stronger than the fingerprint because it exercises the full DSP path.
    #[test]
    fn bundled_presets_render_identically_after_hostile_state() {
        use crate::dsp::DspChain;

        // A `DspChain::new` is ~1.7 s in debug at 48 kHz (cab/amp construction,
        // not sample count), so the render gate covers a representative subset of
        // bundled presets chosen to span the effect types the bundles actually
        // enable; the fingerprint test above covers the full set cheaply.
        const RENDER_SUBSET: &[&str] = &[
            // comp, fuzz, TS, pre-EQ, vibe, EQ, delay, reverb
            "pink_floyd_shine_on_crazy_diamond.toml",
            // comp, fuzz, pre-EQ, EQ, phaser, delay, reverb
            "pink_floyd_another_brick_pt2.toml",
            // comp, EQ, chorus, delay, reverb
            "eagles_hotel_california_clean.toml",
        ];

        let sr = 48_000.0;
        // A deterministic, decaying two-tone DI at 0.5 peak.
        let di: Vec<f32> = (0..4800)
            .map(|n| {
                let t = n as f32 / sr;
                let env = (-8.0 * t).exp();
                0.5 * env
                    * ((2.0 * std::f32::consts::PI * 110.0 * t).sin()
                        + 0.5 * (2.0 * std::f32::consts::PI * 440.0 * t).sin())
            })
            .collect();

        let mut rendered = 0;
        for entry in std::fs::read_dir("presets").expect("presets/ dir") {
            let path = entry.unwrap().path();
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if !RENDER_SUBSET.contains(&name) {
                continue;
            }
            rendered += 1;
            let preset = Preset::load(&path, PresetSource::System)
                .unwrap_or_else(|e| panic!("failed to parse {}: {e}", path.display()));

            let render = |prior: Params| -> (Vec<f32>, Vec<f32>) {
                preset.apply(&prior);
                let params = std::sync::Arc::new(prior);
                let mut chain = DspChain::new(sr, std::sync::Arc::clone(&params));
                let mut l = vec![0.0f32; di.len()];
                let mut r = vec![0.0f32; di.len()];
                chain.process_block(&di, &mut l, &mut r);
                (l, r)
            };

            let (fresh_l, fresh_r) = render(Params::new());
            let (hostile_l, hostile_r) = render(hostile_params());
            for (channel, fresh, hostile) in
                [("L", &fresh_l, &hostile_l), ("R", &fresh_r, &hostile_r)]
            {
                if let Some(i) = fresh.iter().zip(hostile).position(|(a, b)| a != b) {
                    panic!(
                        "{} {channel} differs by prior state at sample {i}",
                        path.display()
                    );
                }
            }
        }
        assert_eq!(
            rendered,
            RENDER_SUBSET.len(),
            "a subset preset was not found; update RENDER_SUBSET"
        );
    }

    /// Scratch dir for export tests: unique per process so parallel tests never
    /// collide. Callers remove what they create.
    fn scratch_dir(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "rusty-riff-preset-test-{}-{}",
            tag,
            std::process::id()
        ))
    }

    #[test]
    fn export_copies_preset_bytes_to_dest() {
        let bundled: Vec<PathBuf> = std::fs::read_dir("presets")
            .expect("presets/ dir")
            .map(|e| e.unwrap().path())
            .filter(|p| p.extension().is_some_and(|ext| ext == "toml"))
            .collect();
        assert!(!bundled.is_empty());
        let preset = Preset::load(&bundled[0], PresetSource::System).unwrap();

        let dir = scratch_dir("export");
        let dest = dir.join("nested").join("shared.toml");
        let out = preset.export_to(&dest).unwrap();
        assert_eq!(out, dest);
        let want = std::fs::read(&bundled[0]).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), want);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn export_overwrites_an_existing_dest() {
        let bundled: Vec<PathBuf> = std::fs::read_dir("presets")
            .expect("presets/ dir")
            .map(|e| e.unwrap().path())
            .filter(|p| p.extension().is_some_and(|ext| ext == "toml"))
            .collect();
        let preset = Preset::load(&bundled[0], PresetSource::System).unwrap();

        let dir = scratch_dir("export-overwrite");
        std::fs::create_dir_all(&dir).unwrap();
        let dest = dir.join("tone.toml");
        std::fs::write(&dest, "stale").unwrap();
        preset.export_to(&dest).unwrap();
        assert_ne!(std::fs::read_to_string(&dest).unwrap(), "stale");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn export_serializes_a_pathless_preset() {
        // System presets from an installed binary have no on-disk path; export
        // must still produce a parseable file.
        let mut preset = Preset::load(
            &std::fs::read_dir("presets")
                .expect("presets/ dir")
                .map(|e| e.unwrap().path())
                .find(|p| p.extension().is_some_and(|ext| ext == "toml"))
                .unwrap(),
            PresetSource::System,
        )
        .unwrap();
        preset.path = None;

        let dir = scratch_dir("export-pathless");
        let dest = dir.join("embedded.toml");
        preset.export_to(&dest).unwrap();
        let round_tripped = Preset::load(&dest, PresetSource::User).unwrap();
        assert_eq!(round_tripped.name, preset.name);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn import_rejects_invalid_toml() {
        let dir = scratch_dir("import-invalid");
        std::fs::create_dir_all(&dir).unwrap();
        let bad = dir.join("bad.toml");
        std::fs::write(&bad, "this is [not valid").unwrap();
        assert!(Preset::import_from(&bad).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn sanitize_and_tilde_helpers() {
        assert_eq!(sanitize_filename("My Lead Tone!"), "my_lead_tone_");
        let home = dirs::home_dir().unwrap();
        assert_eq!(expand_tilde(Path::new("~/x.toml")), home.join("x.toml"));
        assert_eq!(
            expand_tilde(Path::new("/abs/x.toml")),
            PathBuf::from("/abs/x.toml")
        );
    }

    /// A preset carrying a custom chain order applies it; the order round-trips
    /// through save/parse.
    #[test]
    fn preset_chain_order_applies_and_round_trips() {
        let params = Params::new();
        let mut moved: Vec<u8> = ChainStage::default_order().into_iter().collect();
        moved.retain(|&v| v != ChainStage::Comp as u8);
        moved.insert(12, ChainStage::Comp as u8); // comp after the cab stage
        let moved: [u8; crate::dsp::CHAIN_LEN] = moved.try_into().unwrap();
        params.set_chain_order(&moved);

        let preset = Preset::from_params("Moved".to_string(), None, &params);
        let names = &preset.chain.as_ref().expect("chain saved").order;
        assert_eq!(names[8], "vibe");
        assert_eq!(names[9], "boost");
        assert_eq!(names[10], "amp");
        assert_eq!(names[11], "cab");
        assert_eq!(names[12], "comp");

        // Apply onto fresh params and confirm the slots land.
        let fresh = Params::new();
        preset.apply(&fresh);
        assert_eq!(fresh.chain_slots(), moved);

        // And through TOML serialization.
        let toml_str = toml::to_string_pretty(&preset).unwrap();
        let back: Preset = toml::from_str(&toml_str).unwrap();
        let fresher = Params::new();
        back.apply(&fresher);
        assert_eq!(fresher.chain_slots(), moved);
    }

    /// A legacy `"ampcab"` entry expands to consecutive `"amp"`, `"cab"` at the
    /// same spot, so pre-split presets keep their exact topology.
    #[test]
    fn preset_legacy_ampcab_expands_to_amp_cab() {
        let params = Params::new();
        // A minimal preset file is not needed: build the preset and apply it.
        let mut preset = Preset::from_params("Legacy".to_string(), None, &params);
        preset.chain = Some(ChainSection {
            order: vec![
                "gate".to_string(),
                "ampcab".to_string(),
                "delay".to_string(),
            ],
        });
        preset.apply(&params);

        let slots = params.chain_slots();
        let amp = slots
            .iter()
            .position(|&v| v == ChainStage::Amp as u8)
            .expect("amp present");
        let cab = slots
            .iter()
            .position(|&v| v == ChainStage::Cab as u8)
            .expect("cab present");
        assert_eq!(cab, amp + 1, "amp and cab must expand consecutively");
        // Every other stage is present exactly once (sanitize appended them).
        let mut sorted = slots;
        sorted.sort_unstable();
        let mut want = ChainStage::default_order();
        want.sort_unstable();
        assert_eq!(sorted, want);
    }

    /// Presets without a chain (all existing files) fall back to the default
    /// order; unknown names are dropped and missing stages appended.
    #[test]
    fn preset_chain_missing_or_invalid_falls_back() {
        let params = Params::new();
        // A valid but non-default prior order: a preset without `[chain]` resets it.
        let mut shuffled = ChainStage::default_order();
        shuffled.swap(0, 1); // Gate <-> Whammy
        params.set_chain_order(&shuffled);

        // No chain section → default order.
        let mut preset = Preset::from_params("X".to_string(), None, &params);
        preset.chain = None;
        preset.apply(&params);
        assert_eq!(params.chain_slots(), ChainStage::default_order());

        // Junk names dropped, dupes collapsed, missing stages appended.
        preset.chain = Some(ChainSection {
            order: vec![
                "bogus".to_string(),
                "delay".to_string(),
                "delay".to_string(),
            ],
        });
        preset.apply(&params);
        let slots = params.chain_slots();
        assert_eq!(slots[0], ChainStage::Delay as u8);
        assert_eq!(slots.len(), crate::dsp::CHAIN_LEN);
        let mut sorted = slots;
        sorted.sort_unstable();
        let mut want = ChainStage::default_order();
        want.sort_unstable();
        assert_eq!(sorted, want);
    }

    /// The studio-master width round-trips through save/apply, and an omitted
    /// `[master]` section resets to the default for deterministic loading.
    #[test]
    fn preset_master_width_round_trips_and_defaults() {
        let params = Params::new();
        params.master_width.store(1.0, Relaxed);
        let preset = Preset::from_params("Neutral".to_string(), None, &params);
        assert_eq!(
            preset.master.as_ref().expect("master saved").width,
            1.0,
            "saved width must match"
        );

        let fresh = Params::new();
        fresh.master_width.store(1.3, Relaxed);
        preset.apply(&fresh);
        assert_eq!(fresh.master_width.load(Relaxed), 1.0);

        // Omitted section → the default width.
        let mut no_master = Preset::from_params("X".to_string(), None, &params);
        no_master.master = None;
        fresh.master_width.store(0.5, Relaxed);
        no_master.apply(&fresh);
        assert_eq!(
            fresh.master_width.load(Relaxed),
            crate::dsp::DEFAULT_MASTER_WIDTH
        );
    }
}
