//! Session persistence: the versioned `session.toml` manifest and the atomic
//! project-folder save/load operations.
//!
//! A **session** is a portable project folder (see
//! [`timeline-sessions-plan.md`](../../timeline-sessions-plan.md) §7):
//!
//! ```text
//! ~/.config/rusty-riff/sessions/<name>/
//!   session.toml        # versioned manifest: timing, transport, rig, tracks
//!   audio/track-<id>.<ext>   # imported originals and dry raw takes
//!   irs/cabinet.<ext>        # the selected external IR, when applicable
//! ```
//!
//! This module is pure IO plus the serializable view of a [`Session`]. It never
//! touches the audio thread: the UI builds a [`Manifest`] from its control-thread
//! state, [`write_session`] copies assets and commits the manifest, and
//! [`read_manifest`]/[`Manifest::into_session`] reconstruct a runtime session for
//! the UI to install.

use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};

use crate::preset::Preset;
use crate::session::{AssetRef, Session, Track, TrackKind, TrackLifecycle};

/// File name of the manifest inside a session folder.
pub const MANIFEST_FILE: &str = "session.toml";
/// Manifest schema version written by this build.
pub const MANIFEST_VERSION: u32 = 1;

/// The serializable project description.
#[derive(Debug, Deserialize, Serialize)]
pub struct Manifest {
    pub version: u32,
    pub name: String,
    pub project_sample_rate: u32,
    #[serde(default)]
    pub transport: TransportSection,
    #[serde(default)]
    pub metronome: MetronomeSection,
    /// Complete built-in rig snapshot, reused from the preset schema.
    pub rig: Preset,
    /// Session-relative path to the selected external IR, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_ir: Option<String>,
    /// Whether the external IR was the active cab at save time.
    #[serde(default)]
    pub external_ir_active: bool,
    /// Loaded CLAP insert identity + opaque state sidecar.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clap_insert: Option<ClapInsertSection>,
    /// Loaded AU amp identity + parameter sidecar.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub au_amp: Option<AuAmpSection>,
    #[serde(default)]
    pub tracks: Vec<TrackSection>,
}

/// Persisted identity/state of a loaded CLAP insert.
#[derive(Debug, Deserialize, Serialize)]
pub struct ClapInsertSection {
    /// Bundle path (not portable, restored best-effort on the same machine).
    pub path: String,
    pub id: String,
    pub name: String,
    /// Session-relative opaque state sidecar, when the plugin exposes one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
}

/// Persisted identity/state of a loaded AU amp.
#[derive(Debug, Deserialize, Serialize)]
pub struct AuAmpSection {
    pub name: String,
    pub type_code: u32,
    pub subtype: u32,
    pub manufacturer: u32,
    #[serde(default)]
    pub amp_only: bool,
    /// Session-relative parameter-snapshot sidecar.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<String>,
}

/// One AU parameter's persisted value.
#[derive(Debug, Deserialize, Serialize)]
pub struct AuParamState {
    pub id: u32,
    pub value: f64,
}

/// Wrapper so the AU parameter list can be a TOML table (`[[params]]`).
#[derive(Debug, Deserialize, Serialize)]
pub struct AuParamsFile {
    pub params: Vec<AuParamState>,
}

/// Runtime CLAP spec gathered from a live plugin for saving.
pub struct ClapSpec {
    pub path: PathBuf,
    pub id: String,
    pub name: String,
    pub state: Vec<u8>,
}

/// Runtime AU spec gathered from a live plugin for saving.
pub struct AuSpec {
    pub name: String,
    pub type_code: u32,
    pub subtype: u32,
    pub manufacturer: u32,
    pub amp_only: bool,
    pub params: Vec<(u32, f64)>,
}

/// External plugins to restore after loading a session.
#[derive(Default)]
pub struct SessionExternal {
    pub clap: Option<ClapSpec>,
    pub au: Option<AuSpec>,
}

/// A file to write from memory into the project folder.
pub struct AssetBytes {
    pub rel: String,
    pub bytes: Vec<u8>,
}

/// Transport state, in project ticks / frames.
#[derive(Debug, Default, Deserialize, Serialize)]
pub struct TransportSection {
    pub playhead: u64,
    /// Active seek/move step in seconds. Deserialized leniently so manifests
    /// written before fractional steps (whole-second integers) still load.
    #[serde(deserialize_with = "de_seek_seconds")]
    pub seek_seconds: f32,
    pub loop_enabled: bool,
    pub loop_start: u64,
    pub loop_end: u64,
}

/// Accept a seek step written either as an integer (`5`) or a float (`0.5`).
fn de_seek_seconds<'de, D>(deserializer: D) -> Result<f32, D::Error>
where
    D: serde::Deserializer<'de>,
{
    struct Secs;
    impl serde::de::Visitor<'_> for Secs {
        type Value = f32;
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("a number of seconds (integer or float)")
        }
        fn visit_i64<E: serde::de::Error>(self, v: i64) -> Result<f32, E> {
            Ok(v as f32)
        }
        fn visit_u64<E: serde::de::Error>(self, v: u64) -> Result<f32, E> {
            Ok(v as f32)
        }
        fn visit_f64<E: serde::de::Error>(self, v: f64) -> Result<f32, E> {
            Ok(v as f32)
        }
    }
    deserializer.deserialize_any(Secs)
}

#[derive(Debug, Default, Deserialize, Serialize)]
pub struct MetronomeSection {
    pub enabled: bool,
    pub bpm: u32,
}

/// One timeline row. `asset` is relative to the session folder.
#[derive(Debug, Deserialize, Serialize)]
pub struct TrackSection {
    pub id: u64,
    /// `"import"` or `"raw_take"`.
    pub kind: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asset: Option<String>,
    pub start_ticks: u64,
    pub length_ticks: u64,
    pub gain: f32,
    pub muted: bool,
    /// Input trim (dB) applied when this raw take was captured, if calibrated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_trim_db: Option<f32>,
    /// Engine reference version the trim was calibrated against.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub calibration_ref: Option<u32>,
}

/// A source file to copy into the project folder. `rel` is the destination path
/// relative to the session directory (e.g. `audio/track-3.wav`).
pub struct AssetCopy {
    pub source: PathBuf,
    pub rel: String,
}

/// The default root for saved sessions.
pub fn sessions_root() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".config/rusty-riff/sessions"))
}

/// Root for unsaved recovery captures.
pub fn recovery_root() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".config/rusty-riff/recovery"))
}

/// The folder a session named `name` would be saved to under the default root.
pub fn default_session_dir(name: &str) -> Option<PathBuf> {
    sessions_root().map(|root| root.join(sanitize_name(name)))
}

/// Turn a user-supplied name into a safe single-segment folder name.
pub fn sanitize_name(name: &str) -> String {
    let mut out: String = name
        .trim()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '_'
            }
        })
        .collect();
    while out.starts_with('.') || out.starts_with('_') {
        out.remove(0);
    }
    if out.is_empty() {
        out.push_str("session");
    }
    out
}

/// Resolve a manifest asset path against the session folder, rejecting absolute
/// paths and `..` escapes so a supposedly portable session cannot reference files
/// outside itself.
pub fn resolve_asset(dir: &Path, rel: &str) -> Result<PathBuf> {
    let p = Path::new(rel);
    if p.is_absolute() {
        bail!("asset path must be relative to the session: {rel}");
    }
    if p.components().any(|c| matches!(c, Component::ParentDir)) {
        bail!("asset path escapes the session folder: {rel}");
    }
    Ok(dir.join(p))
}

/// Read and validate a session manifest from `dir`.
pub fn read_manifest(dir: &Path) -> Result<Manifest> {
    let path = dir.join(MANIFEST_FILE);
    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("reading session manifest {}", path.display()))?;
    let manifest: Manifest =
        toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
    if manifest.version > MANIFEST_VERSION {
        bail!(
            "session version {} is newer than this build supports ({MANIFEST_VERSION})",
            manifest.version
        );
    }
    Ok(manifest)
}

/// Copy assets into the project folder and commit the manifest atomically.
///
/// The manifest is written last (via a temp file + rename), so a failure while
/// copying leaves the previously saved manifest intact. Overwrites an existing
/// session in place.
pub fn write_session(
    dir: &Path,
    manifest: &Manifest,
    assets: &[AssetCopy],
    blobs: &[AssetBytes],
) -> Result<()> {
    std::fs::create_dir_all(dir.join("audio"))
        .with_context(|| format!("creating session folder {}", dir.display()))?;
    for blob in blobs {
        let dst = dir.join(&blob.rel);
        if let Some(parent) = dst.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        std::fs::write(&dst, &blob.bytes).with_context(|| format!("writing {}", dst.display()))?;
    }
    for asset in assets {
        let dst = dir.join(&asset.rel);
        // Re-saving a loaded project would otherwise copy a file onto itself,
        // truncating the source before it is read. Identical files are a no-op.
        if dst.exists()
            && std::fs::canonicalize(&asset.source)
                .ok()
                .zip(std::fs::canonicalize(&dst).ok())
                .is_some_and(|(src, dst)| src == dst)
        {
            continue;
        }
        if let Some(parent) = dst.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        std::fs::copy(&asset.source, &dst).with_context(|| {
            format!(
                "copying asset {} -> {}",
                asset.source.display(),
                dst.display()
            )
        })?;
    }
    let text = toml::to_string_pretty(manifest).context("serializing session manifest")?;
    let tmp = dir.join(format!("{MANIFEST_FILE}.tmp"));
    std::fs::write(&tmp, text).with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, dir.join(MANIFEST_FILE))
        .with_context(|| format!("committing manifest in {}", dir.display()))?;
    Ok(())
}

/// Metadata written next to a finalized dry capture so an abandoned take can be
/// discovered and restored on a later launch.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RecoveryMeta {
    pub version: u32,
    pub id: u64,
    pub name: String,
    /// Timeline start in project ticks.
    pub start_ticks: u64,
    pub project_sample_rate: u32,
    pub source_sample_rate: u32,
    pub frames: u64,
    pub overflowed: bool,
}

/// Version of the recovery sidecar schema.
pub const RECOVERY_META_VERSION: u32 = 1;

/// A recoverable dry take found under the recovery root.
#[derive(Clone, Debug)]
pub struct RecoveryTake {
    pub wav: PathBuf,
    /// Folder containing the take.
    pub dir: PathBuf,
    pub meta: RecoveryMeta,
}

impl RecoveryTake {
    pub fn label(&self) -> &str {
        &self.meta.name
    }
}

/// Write the metadata sidecar for `wav` (same stem, `.toml` extension).
pub fn write_recovery_meta(wav: &Path, meta: &RecoveryMeta) -> Result<()> {
    let path = wav.with_extension("toml");
    let text = toml::to_string_pretty(meta).context("serializing recovery metadata")?;
    std::fs::write(&path, text).with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

/// Scan the recovery root for dry takes. A WAV without a sidecar is still
/// surfaced (with a default record) so a crash mid-finalize is recoverable.
pub fn list_recovery() -> Vec<RecoveryTake> {
    let Some(root) = recovery_root() else {
        return Vec::new();
    };
    let Ok(dirs) = std::fs::read_dir(&root) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for dir in dirs.flatten() {
        let dir_path = dir.path();
        if !dir_path.is_dir() {
            continue;
        }
        let Ok(files) = std::fs::read_dir(&dir_path) else {
            continue;
        };
        for file in files.flatten() {
            let wav = file.path();
            if !wav.is_file() || wav.extension().and_then(|e| e.to_str()) != Some("wav") {
                continue;
            }
            let meta = std::fs::read_to_string(wav.with_extension("toml"))
                .ok()
                .and_then(|text| toml::from_str::<RecoveryMeta>(&text).ok())
                .unwrap_or_else(|| RecoveryMeta {
                    version: RECOVERY_META_VERSION,
                    id: 0,
                    name: wav
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or("Recovered take")
                        .to_owned(),
                    start_ticks: 0,
                    project_sample_rate: 0,
                    source_sample_rate: 0,
                    frames: 0,
                    overflowed: true,
                });
            out.push(RecoveryTake {
                wav,
                dir: dir_path.clone(),
                meta,
            });
        }
    }
    out.sort_by(|a, b| a.wav.cmp(&b.wav));
    out
}

/// Delete a recovery WAV and its sidecar, then remove the folder if it is empty.
pub fn discard_recovery_file(wav: &Path) {
    let _ = std::fs::remove_file(wav);
    let _ = std::fs::remove_file(wav.with_extension("toml"));
    if let Some(dir) = wav.parent()
        && dir
            .read_dir()
            .map(|mut it| it.next().is_none())
            .unwrap_or(false)
    {
        let _ = std::fs::remove_dir(dir);
    }
}

/// True when `path` lives under the recovery root (used to GC incorporated takes).
pub fn is_recovery_asset(path: &Path) -> bool {
    recovery_root()
        .map(|root| path.starts_with(root))
        .unwrap_or(false)
}

/// A saved session discovered under the default root.
pub struct SessionEntry {
    pub dir: PathBuf,
    pub name: String,
    pub tracks: usize,
}

/// List valid sessions under the default root (unreadable/corrupt folders are
/// skipped rather than surfaced as errors).
pub fn list_sessions() -> Vec<SessionEntry> {
    let Some(root) = sessions_root() else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(&root) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        if let Ok(manifest) = read_manifest(&dir) {
            out.push(SessionEntry {
                name: manifest.name.clone(),
                tracks: manifest.tracks.len(),
                dir,
            });
        }
    }
    out.sort_by_key(|e| e.name.to_lowercase());
    out
}

impl Manifest {
    /// Reconstruct a runtime [`Session`] from this manifest, resolving every asset
    /// path against `dir`. Tracks are marked [`TrackLifecycle::Loading`]; the UI
    /// decodes and installs them.
    pub fn into_session(&self, dir: &Path) -> Result<Session> {
        let mut session = Session::new(self.project_sample_rate);
        session.set_name(self.name.clone());
        session.set_project_sample_rate(self.project_sample_rate);
        session.set_seek_seconds(self.transport.seek_seconds);
        session.set_saved_dir(Some(dir.to_path_buf()));

        let mut tracks = Vec::with_capacity(self.tracks.len());
        for t in &self.tracks {
            let kind = match t.kind.as_str() {
                "import" => TrackKind::Import,
                "raw_take" => TrackKind::RawTake,
                other => bail!("unknown track kind '{other}'"),
            };
            let asset = match &t.asset {
                Some(rel) => Some(AssetRef {
                    path: resolve_asset(dir, rel)?,
                    // Filled in once the asset is decoded.
                    source_sample_rate: 0,
                    source_channels: 0,
                }),
                None => None,
            };
            tracks.push(Track {
                id: t.id,
                name: t.name.clone(),
                kind,
                lifecycle: TrackLifecycle::Loading,
                asset,
                start_ticks: t.start_ticks,
                length_ticks: t.length_ticks,
                gain: t.gain,
                muted: t.muted,
                peaks: Vec::new(),
                generation: 0,
                input_trim_db: t.input_trim_db,
                calibration_ref: t.calibration_ref,
            });
        }
        session.restore_tracks(tracks);
        Ok(session)
    }
}

/// Build a manifest from runtime parts. Kept here so the UI only has to gather
/// values, not know the on-disk shape.
#[allow(clippy::too_many_arguments)]
pub fn build_manifest(
    name: String,
    project_sample_rate: u32,
    transport: TransportSection,
    metronome: MetronomeSection,
    rig: Preset,
    external_ir: Option<String>,
    external_ir_active: bool,
    clap_insert: Option<ClapInsertSection>,
    au_amp: Option<AuAmpSection>,
    tracks: Vec<TrackSection>,
) -> Result<Manifest> {
    if project_sample_rate == 0 {
        return Err(anyhow!("project sample rate must be non-zero"));
    }
    Ok(Manifest {
        version: MANIFEST_VERSION,
        name,
        project_sample_rate,
        transport,
        metronome,
        rig,
        external_ir,
        external_ir_active,
        clap_insert,
        au_amp,
        tracks,
    })
}

impl Manifest {
    /// Read the external-plugin identity/state sidecars referenced by this
    /// manifest. Missing or unreadable sidecars degrade to `None` rather than
    /// failing the whole session load.
    pub fn load_external(&self, dir: &Path) -> SessionExternal {
        let clap = self.clap_insert.as_ref().map(|s| {
            let state = s
                .state
                .as_ref()
                .and_then(|rel| std::fs::read(resolve_asset(dir, rel).ok()?).ok())
                .unwrap_or_default();
            ClapSpec {
                path: PathBuf::from(&s.path),
                id: s.id.clone(),
                name: s.name.clone(),
                state,
            }
        });
        let au = self.au_amp.as_ref().map(|a| {
            let params = a
                .params
                .as_ref()
                .and_then(|rel| std::fs::read_to_string(resolve_asset(dir, rel).ok()?).ok())
                .and_then(|text| toml::from_str::<AuParamsFile>(&text).ok())
                .map(|file| file.params.into_iter().map(|p| (p.id, p.value)).collect())
                .unwrap_or_default();
            AuSpec {
                name: a.name.clone(),
                type_code: a.type_code,
                subtype: a.subtype,
                manufacturer: a.manufacturer,
                amp_only: a.amp_only,
                params,
            }
        });
        SessionExternal { clap, au }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "rusty-riff-project-test-{tag}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn sanitize_name_makes_a_single_safe_segment() {
        assert_eq!(sanitize_name("My Set 1"), "My_Set_1");
        assert_eq!(sanitize_name("../../etc/passwd"), "etc_passwd");
        assert_eq!(sanitize_name(""), "session");
        assert_eq!(sanitize_name("..."), "session");
    }

    #[test]
    fn resolve_asset_rejects_escapes() {
        let dir = Path::new("/tmp/session");
        assert!(resolve_asset(dir, "audio/a.wav").is_ok());
        assert!(resolve_asset(dir, "/etc/passwd").is_err());
        assert!(resolve_asset(dir, "../secret").is_err());
    }

    #[test]
    fn manifest_round_trips_through_a_project_folder() {
        let dir = tmp_dir("roundtrip");
        let params = crate::dsp::Params::new();
        let rig = Preset::from_params("rig".into(), None, &params);
        let manifest = build_manifest(
            "My Session".into(),
            48_000,
            TransportSection {
                playhead: 1234,
                seek_seconds: 10.0,
                loop_enabled: true,
                loop_start: 100,
                loop_end: 200,
            },
            MetronomeSection {
                enabled: true,
                bpm: 140,
            },
            rig,
            None,
            false,
            None,
            None,
            vec![TrackSection {
                id: 7,
                kind: "import".into(),
                name: "Backing".into(),
                asset: Some("audio/track-7.wav".into()),
                start_ticks: 480,
                length_ticks: 9600,
                gain: 0.5,
                muted: true,
                input_trim_db: None,
                calibration_ref: None,
            }],
        )
        .expect("build manifest");

        // A dummy asset so the copy succeeds.
        std::fs::create_dir_all(dir.join("audio")).expect("mkdir");
        let src = dir.join("source.wav");
        std::fs::write(&src, b"not really a wav").expect("write src");
        let assets = [AssetCopy {
            source: src,
            rel: "audio/track-7.wav".into(),
        }];
        write_session(&dir, &manifest, &assets, &[]).expect("write");

        let read = read_manifest(&dir).expect("read");
        assert_eq!(read.name, "My Session");
        assert_eq!(read.transport.seek_seconds, 10.0);
        assert_eq!(read.tracks.len(), 1);
        assert_eq!(read.tracks[0].id, 7);

        let session = read.into_session(&dir).expect("into_session");
        assert_eq!(session.name(), "My Session");
        assert_eq!(session.track(7).map(|t| t.muted), Some(true));
        assert_eq!(session.track(7).and_then(|t| t.input_trim_db), None);
        assert_eq!(session.seek_seconds(), 10.0);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Old manifests spelled `seek_seconds` as a whole number; fractional steps
    /// are new, so both integer and float forms must parse.
    #[test]
    fn seek_seconds_accepts_integer_and_fractional_forms() {
        let old =
            "playhead = 0\nseek_seconds = 5\nloop_enabled = false\nloop_start = 0\nloop_end = 0\n";
        let t: TransportSection = toml::from_str(old).expect("old integer step must load");
        assert_eq!(t.seek_seconds, 5.0);

        let new = old.replace("seek_seconds = 5", "seek_seconds = 0.5");
        let t: TransportSection = toml::from_str(&new).expect("fractional step must load");
        assert_eq!(t.seek_seconds, 0.5);
    }

    /// Trim metadata round-trips, and an older manifest without the fields still
    /// parses (both fields serde-default to `None`).
    #[test]
    fn track_trim_metadata_round_trips_and_old_manifests_load() {
        let section = TrackSection {
            id: 3,
            kind: "raw_take".into(),
            name: "Take 1".into(),
            asset: Some("audio/track-3.wav".into()),
            start_ticks: 0,
            length_ticks: 96_000,
            gain: 1.0,
            muted: false,
            input_trim_db: Some(7.5),
            calibration_ref: Some(1),
        };
        let text = toml::to_string(&section).expect("serialize");
        let back: TrackSection = toml::from_str(&text).expect("parse");
        assert_eq!(back.input_trim_db, Some(7.5));
        assert_eq!(back.calibration_ref, Some(1));

        let old = "id = 3\nkind = \"raw_take\"\nname = \"Take 1\"\n\
                   start_ticks = 0\nlength_ticks = 96000\ngain = 1.0\nmuted = false\n";
        let old: TrackSection = toml::from_str(old).expect("old manifest parses");
        assert_eq!(old.input_trim_db, None);
        assert_eq!(old.calibration_ref, None);
    }

    #[test]
    fn resaving_in_place_does_not_truncate_assets() {
        // Simulates saving a session that was loaded from its own folder: the
        // asset source and destination are the same file.
        let dir = tmp_dir("resave");
        let params = crate::dsp::Params::new();
        let rig = Preset::from_params("rig".into(), None, &params);
        let manifest = build_manifest(
            "S".into(),
            48_000,
            TransportSection::default(),
            MetronomeSection::default(),
            rig,
            None,
            false,
            None,
            None,
            Vec::new(),
        )
        .expect("manifest");

        std::fs::create_dir_all(dir.join("audio")).expect("mkdir");
        let payload = vec![7u8; 4096];
        let asset = dir.join("audio/track-1.wav");
        std::fs::write(&asset, &payload).expect("seed asset");

        let assets = [AssetCopy {
            source: asset.clone(),
            rel: "audio/track-1.wav".into(),
        }];
        write_session(&dir, &manifest, &assets, &[]).expect("resave");

        let after = std::fs::read(&asset).expect("read asset");
        assert_eq!(
            after, payload,
            "in-place resave must not truncate the asset"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn recovery_sidecar_round_trips_and_discards() {
        let dir = tmp_dir("recovery");
        std::fs::create_dir_all(&dir).expect("mkdir");
        let wav = dir.join("take-3.wav");
        std::fs::write(&wav, b"partial").expect("write wav");
        let meta = RecoveryMeta {
            version: RECOVERY_META_VERSION,
            id: 3,
            name: "Take 3".into(),
            start_ticks: 960,
            project_sample_rate: 48_000,
            source_sample_rate: 48_000,
            frames: 5,
            overflowed: false,
        };
        write_recovery_meta(&wav, &meta).expect("write meta");

        let text = std::fs::read_to_string(wav.with_extension("toml")).expect("read meta");
        let parsed: RecoveryMeta = toml::from_str(&text).expect("parse meta");
        assert_eq!(parsed.start_ticks, 960);
        assert_eq!(parsed.name, "Take 3");

        discard_recovery_file(&wav);
        assert!(!wav.exists());
        assert!(!wav.with_extension("toml").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn external_plugin_state_round_trips() {
        let dir = tmp_dir("plugins");
        let params = crate::dsp::Params::new();
        let rig = Preset::from_params("rig".into(), None, &params);
        let manifest = build_manifest(
            "S".into(),
            48_000,
            TransportSection::default(),
            MetronomeSection::default(),
            rig,
            None,
            false,
            Some(ClapInsertSection {
                path: "/tmp/x.clap".into(),
                id: "com.example.x".into(),
                name: "X".into(),
                state: Some("plugins/insert.state".into()),
            }),
            Some(AuAmpSection {
                name: "AU: Amp".into(),
                type_code: 1,
                subtype: 2,
                manufacturer: 3,
                amp_only: true,
                params: Some("plugins/amp.params".into()),
            }),
            Vec::new(),
        )
        .expect("manifest");

        let param_state = vec![
            AuParamState {
                id: 11,
                value: 0.25,
            },
            AuParamState {
                id: 42,
                value: 0.75,
            },
        ];
        let blobs = [
            AssetBytes {
                rel: "plugins/insert.state".into(),
                bytes: vec![1, 2, 3, 4],
            },
            AssetBytes {
                rel: "plugins/amp.params".into(),
                bytes: toml::to_string(&AuParamsFile {
                    params: param_state,
                })
                .expect("params toml")
                .into_bytes(),
            },
        ];
        // `plugins/` must exist before writing blobs.
        std::fs::create_dir_all(dir.join("plugins")).expect("mkdir");
        write_session(&dir, &manifest, &[], &blobs).expect("write");

        let read = read_manifest(&dir).expect("read");
        let external = read.load_external(&dir);
        let clap = external.clap.expect("clap spec");
        assert_eq!(clap.state, vec![1, 2, 3, 4]);
        let au = external.au.expect("au spec");
        assert!(au.amp_only);
        assert_eq!(au.params, vec![(11, 0.25), (42, 0.75)]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_newer_manifest_version_is_rejected() {
        let dir = tmp_dir("version");
        std::fs::create_dir_all(&dir).expect("mkdir");
        std::fs::write(
            dir.join(MANIFEST_FILE),
            "version = 999\nname = \"x\"\nproject_sample_rate = 48000\n[transport]\n[metronome]\n[rig]\n",
        )
        .expect("write");
        assert!(read_manifest(&dir).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
