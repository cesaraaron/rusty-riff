//! Input-level calibration: shared lock-free state plus the pure per-sample
//! trim/window loop.
//!
//! The engine reference level fixes what a defined performance (hard open-E
//! strums, volume on 10) should read in the engine, so amp breakup does not
//! depend on the user's interface gain. A trim of 0 dB is the uncalibrated
//! default and is bit-identical to no trim at all.

use atomic_float::AtomicF32;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use crate::analysis::metrics::{db, percentile};
use crate::dsp::effects::db_to_lin;

/// Length of one measurement window.
pub const CAL_WINDOW_MS: f32 = 10.0;
/// Capacity of the audio → UI window-stats ring.
pub const CAL_RING_CAPACITY: usize = 4096;
/// |x| at or above this counts as raw clipping.
pub const CLIP_THRESHOLD: f32 = 0.999;
/// Time constant of the trim-gain smoother.
pub const TRIM_SMOOTH_MS: f32 = 20.0;

/// Shared, lock-free input-calibration state (control thread ⇄ audio thread).
#[derive(Default)]
pub struct InputCalibration {
    /// Trim applied to the guitar input, in dB. `0.0` = uncalibrated.
    pub trim_db: AtomicF32,
    /// UI sets while the wizard is capturing; the audio thread then pushes stats.
    pub measuring: AtomicBool,
    /// Sticky: a raw (pre-trim) sample reached [`CLIP_THRESHOLD`]. UI clears it.
    pub raw_clip: AtomicBool,
    /// Sticky: the stats ring was full and a window was dropped. UI clears it.
    pub stats_overflow: AtomicBool,
}

impl InputCalibration {
    pub fn new() -> Self {
        Self::default()
    }
}

/// One accumulated measurement window on the **raw** (pre-trim) signal.
#[derive(Clone, Copy, Debug, Default)]
pub struct WindowStat {
    pub peak_raw: f32,
    pub sum_sq_raw: f32,
    pub frames: u32,
}

/// The audio thread's trim smoother state.
#[derive(Clone, Copy, Debug)]
pub struct TrimState {
    /// Current (smoothed) linear gain.
    pub gain: f32,
    /// One-pole coefficient for [`TRIM_SMOOTH_MS`] at the engine rate.
    pub coeff: f32,
}

impl TrimState {
    /// Build from the loaded trim so there is no startup ramp.
    pub fn new(sr: f32, trim_db: f32) -> Self {
        Self {
            gain: db_to_lin(trim_db),
            coeff: 1.0 - (-1.0 / (sr * TRIM_SMOOTH_MS / 1000.0)).exp(),
        }
    }
}

/// Sticky flags raised by one processed block.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Flags {
    pub clipped: bool,
    pub overflow: bool,
}

/// Apply the smoothed trim to `buf` in place, reporting raw clipping and, while
/// `measuring`, accumulating windows of `win_len` frames into `sink`.
///
/// `sink` returns `true` when the window was accepted, `false` when the ring was
/// full (the window is still reset). This is the whole input-conditioning hot
/// path; it never allocates. When not measuring, `win` is kept clear.
pub fn condition_block(
    buf: &mut [f32],
    state: &mut TrimState,
    win: &mut WindowStat,
    win_len: u32,
    target: f32,
    measuring: bool,
    mut sink: impl FnMut(WindowStat) -> bool,
) -> Flags {
    let mut flags = Flags::default();
    if !measuring {
        *win = WindowStat::default();
    }
    for x in buf.iter_mut() {
        let raw = *x;
        if raw.abs() >= CLIP_THRESHOLD {
            flags.clipped = true;
        }
        if measuring {
            win.peak_raw = win.peak_raw.max(raw.abs());
            win.sum_sq_raw += raw * raw;
            win.frames += 1;
            if win.frames >= win_len {
                if !sink(*win) {
                    flags.overflow = true;
                }
                *win = WindowStat::default();
            }
        }
        // One-pole toward target; exact 1.0 when target and gain are both 1.0.
        state.gain += state.coeff * (target - state.gain);
        *x = raw * state.gain;
    }
    flags
}

// ── Reference targets and persistence ─────────────────────────────────────────

/// Bumped when the reference targets change; recorded with each saved entry so
/// the UI can warn about a stale calibration. Version 2 = targets measured on the
/// reference rig (see `docs/fidelity-implement.md` → B8).
pub const REFERENCE_VERSION: u32 = 2;

/// The pickup class a calibration was measured with.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PickupClass {
    SingleCoil,
    P90,
    #[default]
    Humbucker,
}

/// Engine-reference targets: the 99th-percentile 10 ms window peak a defined
/// performance should read, per pickup class.
///
/// Verified for humbucker and single-coil against the reference rig (Scarlett
/// Solo Gen 4, INST on, gain 9 o'clock; Donner DST-152 bridge HB / neck SC) in
/// B8; P90 remains provisional (no P90 guitar was available to measure).
pub fn target_peak_dbfs(p: PickupClass) -> f32 {
    match p {
        PickupClass::SingleCoil => -24.6,
        PickupClass::P90 => -4.5,
        PickupClass::Humbucker => -19.7,
    }
}

impl PickupClass {
    pub fn next(self) -> Self {
        match self {
            Self::SingleCoil => Self::P90,
            Self::P90 => Self::Humbucker,
            Self::Humbucker => Self::SingleCoil,
        }
    }

    pub fn prev(self) -> Self {
        match self {
            Self::SingleCoil => Self::Humbucker,
            Self::P90 => Self::SingleCoil,
            Self::Humbucker => Self::P90,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::SingleCoil => "single-coil",
            Self::P90 => "P90",
            Self::Humbucker => "humbucker",
        }
    }
}

/// Identity of an input: device name, channel count and guitar channel.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InputIdentity {
    pub device: String,
    pub channels: u16,
    pub channel: u16,
}

/// One saved calibration (a row in `input-calibration.toml`).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CalibrationEntry {
    pub device: String,
    pub channels: u16,
    pub channel: u16,
    #[serde(default)]
    pub trim_db: f32,
    #[serde(default)]
    pub pickup: PickupClass,
    #[serde(default)]
    pub measured_peak_dbfs: f32,
    #[serde(default)]
    pub target_peak_dbfs: f32,
    #[serde(default = "current_reference_version")]
    pub reference_version: u32,
    #[serde(default)]
    pub calibrated_unix: u64,
    #[serde(default)]
    pub note: String,
}

impl CalibrationEntry {
    pub fn identity(&self) -> InputIdentity {
        InputIdentity {
            device: self.device.clone(),
            channels: self.channels,
            channel: self.channel,
        }
    }

    pub fn matches(&self, identity: &InputIdentity) -> bool {
        self.device == identity.device
            && self.channels == identity.channels
            && self.channel == identity.channel
    }
}

/// The on-disk file: a version plus one row per calibrated input.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CalibrationFile {
    #[serde(default = "current_reference_version")]
    pub version: u32,
    #[serde(default)]
    pub inputs: Vec<CalibrationEntry>,
}

fn current_reference_version() -> u32 {
    REFERENCE_VERSION
}

/// `~/.config/rusty-riff/input-calibration.toml`.
pub fn calibration_path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".config/rusty-riff/input-calibration.toml"))
}

fn read_file(path: &Path) -> CalibrationFile {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| toml::from_str::<CalibrationFile>(&text).ok())
        .unwrap_or_else(|| CalibrationFile {
            version: REFERENCE_VERSION,
            inputs: Vec::new(),
        })
}

fn write_file(path: &Path, file: &CalibrationFile) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let text = toml::to_string_pretty(file)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    // Write a sibling temp file and rename, so a failed write never corrupts the
    // existing calibration.
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)
}

/// Load the saved calibration for `identity`, if any. Malformed files are treated
/// as absent (best-effort, never blocks startup).
pub fn load_calibration(identity: &InputIdentity) -> Option<CalibrationEntry> {
    calibration_path().and_then(|path| load_calibration_at(&path, identity))
}

/// Load at an explicit path (test seam).
pub fn load_calibration_at(path: &Path, identity: &InputIdentity) -> Option<CalibrationEntry> {
    read_file(path)
        .inputs
        .into_iter()
        .find(|e| e.matches(identity))
}

/// Save/replace the entry for the same identity, keeping every other entry.
pub fn save_calibration(entry: &CalibrationEntry) -> std::io::Result<()> {
    match calibration_path() {
        Some(path) => save_calibration_at(&path, entry),
        None => Ok(()),
    }
}

/// Save at an explicit path (test seam).
pub fn save_calibration_at(path: &Path, entry: &CalibrationEntry) -> std::io::Result<()> {
    let mut file = read_file(path);
    file.version = REFERENCE_VERSION;
    let identity = entry.identity();
    file.inputs.retain(|e| !e.matches(&identity));
    file.inputs.push(entry.clone());
    write_file(path, &file)
}

/// Remove the entry for `identity`, keeping every other entry.
pub fn remove_calibration(identity: &InputIdentity) -> std::io::Result<()> {
    match calibration_path() {
        Some(path) => remove_calibration_at(&path, identity),
        None => Ok(()),
    }
}

/// Remove at an explicit path (test seam).
pub fn remove_calibration_at(path: &Path, identity: &InputIdentity) -> std::io::Result<()> {
    let mut file = read_file(path);
    file.inputs.retain(|e| !e.matches(identity));
    write_file(path, &file)
}

// ── Calibration computation (pure) ─────────────────────────────────────────────

/// The outcome of a calibration capture.
#[derive(Clone, Debug, PartialEq)]
pub struct CalResult {
    pub measured_peak_dbfs: f32,
    pub noise_floor_dbfs: f32,
    pub target_peak_dbfs: f32,
    pub trim_db: f32,
    pub warnings: Vec<CalWarning>,
}

/// Why a capture could not produce a calibration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CalError {
    /// A raw sample hit full scale — lower the interface gain and retry.
    Clipped,
    /// Fewer than 2 s of playing was captured.
    TooShort,
    /// The playing was effectively silent.
    NoSignal,
    /// Window statistics were dropped (ring full) — retry.
    Overflow,
}

/// Non-fatal notes about a calibration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CalWarning {
    /// The trim is large (above +18 dB).
    HighTrim,
    /// The signal-to-noise ratio is under 40 dB.
    LowSnr,
    /// The raw trim was outside the ±24 dB clamp.
    Clamped,
}

/// Number of 10 ms windows in the 2 s minimum capture.
const MIN_PLAY_WINDOWS: usize = 200;

/// Compute a calibration from raw play/noise windows. Pure; unit-tested.
pub fn compute_calibration(
    play: &[WindowStat],
    noise: &[WindowStat],
    pickup: PickupClass,
    clipped: bool,
    overflow: bool,
) -> Result<CalResult, CalError> {
    if clipped {
        return Err(CalError::Clipped);
    }
    if overflow {
        return Err(CalError::Overflow);
    }
    if play.len() < MIN_PLAY_WINDOWS {
        return Err(CalError::TooShort);
    }

    let mut peaks: Vec<f32> = play.iter().map(|w| w.peak_raw).collect();
    peaks.sort_by(f32::total_cmp);
    let measured_peak_dbfs = db(percentile(&peaks, 0.99));
    if measured_peak_dbfs < -50.0 {
        return Err(CalError::NoSignal);
    }

    let noise_frames: f32 = noise.iter().map(|w| w.frames as f32).sum();
    let noise_rms = if noise_frames <= 0.0 {
        0.0
    } else {
        (noise.iter().map(|w| w.sum_sq_raw).sum::<f32>() / noise_frames).sqrt()
    };
    let noise_floor_dbfs = db(noise_rms);

    let target = target_peak_dbfs(pickup);
    let raw_trim = target - measured_peak_dbfs;
    let trim_db = raw_trim.clamp(-24.0, 24.0);

    let mut warnings = Vec::new();
    if (raw_trim - trim_db).abs() > f32::EPSILON {
        warnings.push(CalWarning::Clamped);
    }
    if trim_db > 18.0 {
        warnings.push(CalWarning::HighTrim);
    }
    if measured_peak_dbfs - noise_floor_dbfs < 40.0 {
        warnings.push(CalWarning::LowSnr);
    }

    Ok(CalResult {
        measured_peak_dbfs,
        noise_floor_dbfs,
        target_peak_dbfs: target,
        trim_db,
        warnings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::PI;

    fn state(sr: f32, db: f32) -> TrimState {
        TrimState::new(sr, db)
    }

    /// Uncalibrated (0 dB) is bit-identical: gain settles at exactly 1.0 and
    /// `x * 1.0 == x`.
    #[test]
    fn zero_db_is_bit_identical() {
        let sr = 48_000.0;
        let mut st = state(sr, 0.0);
        let mut win = WindowStat::default();
        let original: Vec<f32> = (0..1000)
            .map(|i| (2.0 * PI * 120.0 * i as f32 / sr).sin() * 0.5)
            .collect();
        let mut buf = original.clone();
        let flags = condition_block(&mut buf, &mut st, &mut win, 480, 1.0, false, |_| true);
        assert_eq!(buf, original, "0 dB trim altered the signal");
        assert!(!flags.clipped && !flags.overflow);
        assert_eq!(st.gain, 1.0);
    }

    /// A step to +6 dB converges to 2.0× without overshoot.
    #[test]
    fn plus_six_db_converges_without_overshoot() {
        let sr = 48_000.0;
        let target = db_to_lin(6.0);
        let mut st = state(sr, 0.0);
        let mut win = WindowStat::default();
        let n = (sr * (5.0 * TRIM_SMOOTH_MS / 1000.0)) as usize;
        let mut buf = vec![1.0f32; n];
        let mut over = 0.0f32;
        // Process in small blocks, watching the gain.
        for chunk in buf.chunks_mut(64) {
            condition_block(chunk, &mut st, &mut win, 480, target, false, |_| true);
            over = over.max(st.gain);
        }
        assert!(
            (st.gain - target).abs() / target < 0.01,
            "gain {} did not converge to {target}",
            st.gain
        );
        assert!(over <= target * 1.001, "overshoot to {over}");
    }

    /// Window stats sum to the expected energy of a known sine.
    #[test]
    fn window_stats_match_a_known_sine() {
        let sr = 48_000.0;
        let amp = 0.25f32;
        let freq = 1000.0;
        let n = 4800usize; // 100 ms
        let mut st = state(sr, 0.0);
        let mut win = WindowStat::default();
        let mut buf: Vec<f32> = (0..n)
            .map(|i| amp * (2.0 * PI * freq * i as f32 / sr).sin())
            .collect();
        let mut windows = Vec::new();
        condition_block(&mut buf, &mut st, &mut win, 480, 1.0, true, |w| {
            windows.push(w);
            true
        });
        assert_eq!(windows.len(), 10, "expected ten 10 ms windows");
        for w in windows {
            assert_eq!(w.frames, 480);
            assert!((w.peak_raw - amp).abs() < 1e-3, "peak {}", w.peak_raw);
            let expected = w.sum_sq_raw;
            let ideal = 0.5 * amp * amp * 480.0;
            assert!(
                (expected - ideal).abs() / ideal < 0.02,
                "energy {expected} vs {ideal}"
            );
        }
    }

    /// Raw clipping is flagged even when the trim is negative.
    #[test]
    fn clipping_flagged_from_raw_with_negative_trim() {
        let mut st = state(48_000.0, -12.0);
        let mut win = WindowStat::default();
        let mut buf = vec![0.0f32, 1.0, 0.0];
        let flags = condition_block(
            &mut buf,
            &mut st,
            &mut win,
            480,
            db_to_lin(-12.0),
            false,
            |_| true,
        );
        assert!(flags.clipped, "a raw full-scale sample must flag clipping");
        // The trimmed output is below full scale.
        assert!(buf[1].abs() < 0.5);
    }

    /// A full ring raises the overflow flag rather than blocking.
    #[test]
    fn full_ring_reports_overflow() {
        let mut st = state(48_000.0, 0.0);
        let mut win = WindowStat::default();
        let mut buf = vec![0.1f32; 960];
        let flags = condition_block(&mut buf, &mut st, &mut win, 480, 1.0, true, |_| false);
        assert!(flags.overflow && !flags.clipped);
    }

    fn scratch(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("rusty-riff-cal-test-{tag}-{}", std::process::id()))
    }

    fn identity(device: &str, channels: u16, channel: u16) -> InputIdentity {
        InputIdentity {
            device: device.to_string(),
            channels,
            channel,
        }
    }

    fn entry(device: &str, channels: u16, channel: u16, trim: f32) -> CalibrationEntry {
        CalibrationEntry {
            device: device.to_string(),
            channels,
            channel,
            trim_db: trim,
            pickup: PickupClass::Humbucker,
            measured_peak_dbfs: -10.5,
            target_peak_dbfs: target_peak_dbfs(PickupClass::Humbucker),
            reference_version: REFERENCE_VERSION,
            calibrated_unix: 1_790_000_000,
            note: String::new(),
        }
    }

    #[test]
    fn calibration_round_trips_two_identities() {
        let path = scratch("two").join("input-calibration.toml");
        save_calibration_at(&path, &entry("Scarlett 2i2", 2, 0, 7.5)).unwrap();
        save_calibration_at(&path, &entry("Scarlett 2i2", 2, 1, -2.0)).unwrap();
        let a = load_calibration_at(&path, &identity("Scarlett 2i2", 2, 0)).unwrap();
        let b = load_calibration_at(&path, &identity("Scarlett 2i2", 2, 1)).unwrap();
        assert_eq!(a.trim_db, 7.5);
        assert_eq!(b.trim_db, -2.0);
        assert_eq!(a.pickup, PickupClass::Humbucker);
        assert_eq!(a.reference_version, REFERENCE_VERSION);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn replacing_one_entry_keeps_the_other() {
        let path = scratch("replace").join("input-calibration.toml");
        save_calibration_at(&path, &entry("Scarlett 2i2", 2, 0, 7.5)).unwrap();
        save_calibration_at(&path, &entry("Scarlett 2i2", 2, 1, -2.0)).unwrap();
        save_calibration_at(&path, &entry("Scarlett 2i2", 2, 0, 3.0)).unwrap();
        assert_eq!(
            load_calibration_at(&path, &identity("Scarlett 2i2", 2, 0))
                .unwrap()
                .trim_db,
            3.0
        );
        assert_eq!(
            load_calibration_at(&path, &identity("Scarlett 2i2", 2, 1))
                .unwrap()
                .trim_db,
            -2.0
        );
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn remove_drops_only_that_entry() {
        let path = scratch("remove").join("input-calibration.toml");
        save_calibration_at(&path, &entry("Dev", 1, 0, 5.0)).unwrap();
        save_calibration_at(&path, &entry("Dev", 2, 0, 6.0)).unwrap();
        remove_calibration_at(&path, &identity("Dev", 1, 0)).unwrap();
        assert!(load_calibration_at(&path, &identity("Dev", 1, 0)).is_none());
        assert!(load_calibration_at(&path, &identity("Dev", 2, 0)).is_some());
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn malformed_file_is_treated_as_absent() {
        let path = scratch("bad").join("input-calibration.toml");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "this is [ not toml").unwrap();
        assert!(load_calibration_at(&path, &identity("Dev", 1, 0)).is_none());
        // A later save overwrites the malformed file.
        save_calibration_at(&path, &entry("Dev", 1, 0, 1.0)).unwrap();
        assert!(load_calibration_at(&path, &identity("Dev", 1, 0)).is_some());
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    fn windows(peak: f32, count: usize) -> Vec<WindowStat> {
        (0..count)
            .map(|_| WindowStat {
                peak_raw: peak,
                sum_sq_raw: peak * peak * 480.0,
                frames: 480,
            })
            .collect()
    }

    #[test]
    fn nominal_humbucker_calibration() {
        // A measured P99 exactly 12 dB below the humbucker target → +12 dB trim.
        let measured_db = target_peak_dbfs(PickupClass::Humbucker) - 12.0;
        let play = windows(db_to_lin(measured_db), 250);
        let noise = windows(db_to_lin(measured_db - 60.0), 200);
        let r = compute_calibration(&play, &noise, PickupClass::Humbucker, false, false).unwrap();
        assert!(
            (r.measured_peak_dbfs - measured_db).abs() < 0.1,
            "{}",
            r.measured_peak_dbfs
        );
        assert!((r.trim_db - 12.0).abs() < 0.1, "{}", r.trim_db);
        assert!(r.warnings.is_empty(), "{:?}", r.warnings);
    }

    #[test]
    fn pickup_targets_shift_the_trim() {
        let measured_db = -15.0f32;
        let play = windows(db_to_lin(measured_db), 250);
        let noise = windows(db_to_lin(-80.0), 200);
        for class in [
            PickupClass::SingleCoil,
            PickupClass::P90,
            PickupClass::Humbucker,
        ] {
            let r = compute_calibration(&play, &noise, class, false, false).unwrap();
            let want = target_peak_dbfs(class) - measured_db;
            assert!(
                (r.trim_db - want).abs() < 0.1,
                "{class:?}: {} vs {want}",
                r.trim_db
            );
        }
    }

    #[test]
    fn calibration_errors_cover_each_branch() {
        let play = windows(0.5, 250);
        let noise = windows(1e-4, 200);
        assert_eq!(
            compute_calibration(&play, &noise, PickupClass::Humbucker, true, false),
            Err(CalError::Clipped)
        );
        assert_eq!(
            compute_calibration(&play, &noise, PickupClass::Humbucker, false, true),
            Err(CalError::Overflow)
        );
        let short = windows(0.5, 10);
        assert_eq!(
            compute_calibration(&short, &noise, PickupClass::Humbucker, false, false),
            Err(CalError::TooShort)
        );
        let quiet = windows(db_to_lin(-60.0), 250);
        assert_eq!(
            compute_calibration(&quiet, &noise, PickupClass::Humbucker, false, false),
            Err(CalError::NoSignal)
        );
    }

    #[test]
    fn clamping_and_low_snr_warn() {
        // A measured P99 25 dB below the target → raw +25 dB, clamped to +24.
        let measured_db = target_peak_dbfs(PickupClass::Humbucker) - 25.0;
        let play = windows(db_to_lin(measured_db), 250);
        let clean_noise = windows(db_to_lin(measured_db - 60.0), 200);
        let r =
            compute_calibration(&play, &clean_noise, PickupClass::Humbucker, false, false).unwrap();
        assert_eq!(r.trim_db, 24.0);
        assert!(r.warnings.contains(&CalWarning::Clamped));
        assert!(r.warnings.contains(&CalWarning::HighTrim));

        // Noise only ~24 dB below the peak → LowSnr.
        let noisy = windows(db_to_lin(-30.0), 200);
        let r2 = compute_calibration(
            &windows(0.5, 250),
            &noisy,
            PickupClass::Humbucker,
            false,
            false,
        )
        .unwrap();
        assert!(
            r2.warnings.contains(&CalWarning::LowSnr),
            "{:?}",
            r2.warnings
        );
    }
}
