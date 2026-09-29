//! Session / project model: the canonical, control-thread-only description of a
//! multitrack timeline.
//!
//! A [`Session`] owns the editable list of [`Track`]s, the project time base and
//! the seek-step choice. It is deliberately independent of any particular
//! [`crate::audio::AudioEngine`]: decoded playback buffers live in bounded
//! audio-thread slots ([`crate::dsp::player`]) and are *caches*, not the source of
//! truth. When a device change rebuilds the engine, the app reconstructs those
//! caches from each track's asset (see [`AssetRef`]).
//!
//! ## Project time base
//! Timeline positions are stored as **project ticks**: integer frames at the
//! session's [`project_sample_rate`](Session::project_sample_rate). Saving a clip
//! start in ticks means a later device or sample-rate change cannot drift the
//! placement. [`ticks_to_frames`] converts to output frames with a single rounding
//! rule (half away from zero) that live playback and the future offline exporter
//! must both use.
//!
//! This module is pure data: it performs no IO and owns no audio state, so it is
//! cheap to unit-test.
//!
//! ## Invariants — do not break
//!
//! - **Realtime safety.** No allocation, blocking, filesystem IO, or
//!   `Vec`/plugin drop in the audio callback. Displaced tracks and plugins are
//!   returned via rings and dropped on the control thread. The capture ring is
//!   fixed-size; overflow is flagged, never silently truncated.
//! - **Coherence per callback.** All `TrackCommand`s are drained at the top of
//!   `on_input`, so a block sees a whole track set. Install success/failure is
//!   reported back via `TrackAck`.
//! - **Generation guard.** A decode/capture for id `X` at generation `G` must not
//!   populate a row that has been replaced; `finish_decode` uses the pending
//!   generation, `poll_capture` rejects mismatched generations.
//! - **Take alignment uses `CaptureState::start_frame`** — the exact project
//!   frame of the first sample — **not** the arm-time playhead. (This was buggy:
//!   the value was read from an always-zero `CaptureResult.start_frame`. That
//!   field was removed and `poll_capture` reads the shared atomic.)
//! - **Project ticks, not engine frames, are persisted.** [`ticks_to_frames`]
//!   (round half away from zero) is the single conversion shared by playback and
//!   the offline exporter. The project rate is adopted from the first engine and
//!   kept across device changes.
//! - **Portable paths only.** `resolve_asset` rejects absolute paths and `..`, so
//!   a session cannot reference files outside its folder.
//! - **Re-saving in place is safe.** See [`crate::project`].

use std::path::PathBuf;

/// A stable track identifier. Survives reordering and outlives any single
/// audio-engine instance.
pub type TrackId = u64;

/// Generation counter for a track's installed playback buffer. A delayed decode
/// carries the generation it was requested for, so a stale worker result can be
/// rejected instead of populating a row the user has since replaced.
pub type Generation = u64;

/// What a track's samples are used for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TrackKind {
    /// Imported backing/reference audio. Stereo, monitor-only, never captured
    /// and never processed by the rig.
    Import,
    /// A dry raw take, captured from the selected input channel. Mono, summed
    /// into the take bus and processed by the (separate) take-bus rig.
    RawTake,
}

/// Where a track is in its lifecycle.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TrackLifecycle {
    /// A background decode is in flight.
    Loading,
    /// Capture is in progress (raw takes only).
    Recording,
    /// Playable.
    Ready,
    /// Failed; carries a human-readable reason.
    Error(String),
}

/// Reference to a track's on-disk source.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AssetRef {
    /// Path to the source audio file.
    pub path: PathBuf,
    /// Sample rate of the file as decoded, before rate matching.
    pub source_sample_rate: u32,
    /// Channel count of the file as decoded.
    pub source_channels: u16,
}

/// One timeline row.
#[derive(Clone, Debug)]
pub struct Track {
    pub id: TrackId,
    pub name: String,
    pub kind: TrackKind,
    pub lifecycle: TrackLifecycle,
    /// `None` until a decode/capture has produced a file.
    pub asset: Option<AssetRef>,
    /// Timeline start in project ticks.
    pub start_ticks: u64,
    /// Length in project ticks.
    pub length_ticks: u64,
    /// For [`TrackKind::Import`] this is the monitor output volume; for
    /// [`TrackKind::RawTake`] it is the pre-rig level (so it changes how the rig
    /// reacts, not just how loud the result is).
    pub gain: f32,
    pub muted: bool,
    /// Cached min/max envelope for the waveform display (computed off-thread).
    pub peaks: Vec<(f32, f32)>,
    /// Bumped on every (re)install so stale decodes can be discarded.
    pub generation: Generation,
    /// Input trim (dB) applied when this raw take was captured, if calibrated.
    pub input_trim_db: Option<f32>,
    /// Engine reference version the trim was calibrated against.
    pub calibration_ref: Option<u32>,
}

impl Track {
    /// First tick after this clip ends.
    pub fn end_ticks(&self) -> u64 {
        self.start_ticks.saturating_add(self.length_ticks)
    }

    pub fn is_ready(&self) -> bool {
        matches!(self.lifecycle, TrackLifecycle::Ready)
    }
}

/// Selectable seek increments, in seconds, ascending. The sub-second steps are
/// for precise placement; the rest are coarse jumps.
pub const SEEK_STEPS: [f32; 8] = [0.01, 0.05, 0.1, 0.5, 1.0, 5.0, 10.0, 30.0];
/// The increment a new session starts on.
pub const DEFAULT_SEEK_STEP: f32 = 1.0;

/// The editable project. Owned by the UI thread; never touched by the audio
/// callback.
pub struct Session {
    project_sample_rate: u32,
    tracks: Vec<Track>,
    selected: Option<TrackId>,
    seek_seconds: f32,
    next_id: TrackId,
    temp_id: String,
    name: String,
    /// `true` once the session has been saved to or loaded from a project folder.
    saved_dir: Option<PathBuf>,
}

impl Session {
    /// A new, empty session whose time base is `project_sample_rate`.
    pub fn new(project_sample_rate: u32) -> Self {
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        Self {
            project_sample_rate: project_sample_rate.max(1),
            tracks: Vec::new(),
            selected: None,
            seek_seconds: DEFAULT_SEEK_STEP,
            next_id: 1,
            temp_id: format!("{secs}"),
            name: "Untitled".to_owned(),
            saved_dir: None,
        }
    }

    pub fn project_sample_rate(&self) -> u32 {
        self.project_sample_rate
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn set_name(&mut self, name: String) {
        self.name = name;
    }

    /// The project folder this session was last saved to / loaded from.
    pub fn saved_dir(&self) -> Option<&PathBuf> {
        self.saved_dir.as_ref()
    }

    pub fn set_saved_dir(&mut self, dir: Option<PathBuf>) {
        self.saved_dir = dir;
    }

    /// Replace the whole track list (used when loading a project) and move the id
    /// counter past every restored id.
    pub fn restore_tracks(&mut self, tracks: Vec<Track>) {
        self.next_id = tracks
            .iter()
            .map(|t| t.id)
            .max()
            .map_or(1, |m| m.saturating_add(1));
        self.selected = tracks.first().map(|t| t.id);
        self.tracks = tracks;
    }

    /// Adopt a new engine rate as the project rate. Only meaningful before any
    /// clips carry ticks; callers should preserve the stored rate once set.
    pub fn set_project_sample_rate(&mut self, rate: u32) {
        self.project_sample_rate = rate.max(1);
    }

    pub fn tracks(&self) -> &[Track] {
        &self.tracks
    }

    pub fn is_empty(&self) -> bool {
        self.tracks.is_empty()
    }

    pub fn len(&self) -> usize {
        self.tracks.len()
    }

    pub fn track(&self, id: TrackId) -> Option<&Track> {
        self.tracks.iter().find(|t| t.id == id)
    }

    pub fn track_mut(&mut self, id: TrackId) -> Option<&mut Track> {
        self.tracks.iter_mut().find(|t| t.id == id)
    }

    pub fn index_of(&self, id: TrackId) -> Option<usize> {
        self.tracks.iter().position(|t| t.id == id)
    }

    /// Allocate a fresh id without adding a row (used to arm a recording before
    /// its audio-thread start is acknowledged).
    pub fn alloc_id(&mut self) -> TrackId {
        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        id
    }

    /// Append a track and return its id.
    #[allow(clippy::too_many_arguments)]
    pub fn push(
        &mut self,
        id: TrackId,
        name: String,
        kind: TrackKind,
        asset: Option<AssetRef>,
        start_ticks: u64,
        length_ticks: u64,
        lifecycle: TrackLifecycle,
    ) -> TrackId {
        self.tracks.push(Track {
            id,
            name,
            kind,
            lifecycle,
            asset,
            start_ticks,
            length_ticks,
            gain: 1.0,
            muted: false,
            peaks: Vec::new(),
            generation: 0,
            input_trim_db: None,
            calibration_ref: None,
        });
        if self.selected.is_none() {
            self.selected = Some(id);
        }
        id
    }

    /// Remove a track. The caller is responsible for telling the audio engine to
    /// drop any installed playback buffer first.
    pub fn remove(&mut self, id: TrackId) -> Option<Track> {
        let idx = self.index_of(id)?;
        let removed = self.tracks.remove(idx);
        if self.selected == Some(id) {
            self.selected = self
                .tracks
                .get(idx.min(self.tracks.len().saturating_sub(1)))
                .map(|t| t.id);
        }
        Some(removed)
    }

    pub fn selected(&self) -> Option<TrackId> {
        self.selected
    }

    pub fn select(&mut self, id: TrackId) {
        if self.index_of(id).is_some() {
            self.selected = Some(id);
        }
    }

    pub fn select_next(&mut self, forward: bool) {
        let Some(id) = self.selected else {
            self.selected = self.tracks.first().map(|t| t.id);
            return;
        };
        let Some(idx) = self.index_of(id) else {
            self.selected = self.tracks.first().map(|t| t.id);
            return;
        };
        let len = self.tracks.len();
        if len == 0 {
            self.selected = None;
            return;
        }
        let next = if forward {
            (idx + 1).min(len - 1)
        } else {
            idx.saturating_sub(1)
        };
        self.selected = Some(self.tracks[next].id);
    }

    /// End of the last clip, in ticks (0 when empty).
    pub fn extent_ticks(&self) -> u64 {
        self.tracks.iter().map(Track::end_ticks).max().unwrap_or(0)
    }

    pub fn seek_seconds(&self) -> f32 {
        self.seek_seconds
    }

    pub fn set_seek_seconds(&mut self, secs: f32) {
        // Nearest step within a small tolerance: manifests round-trip the
        // sub-second steps through decimal text, which is not exact in `f32`.
        if let Some(&s) = SEEK_STEPS
            .iter()
            .find(|&&s| (s - secs).abs() <= 1e-4 * s.max(1.0))
        {
            self.seek_seconds = s;
        }
    }

    /// Advance/reverse through [`SEEK_STEPS`] by `dir` (+1 / -1). Clamps at the
    /// ends rather than wrapping.
    pub fn cycle_seek_step(&mut self, dir: i32) -> f32 {
        let idx = SEEK_STEPS
            .iter()
            .position(|&s| s == self.seek_seconds)
            .or_else(|| SEEK_STEPS.iter().position(|&s| s == DEFAULT_SEEK_STEP))
            .unwrap_or(0) as i32;
        let last = SEEK_STEPS.len() as i32 - 1;
        let next = (idx + dir).clamp(0, last) as usize;
        self.seek_seconds = SEEK_STEPS[next];
        self.seek_seconds
    }

    /// Directory for this session's recoverable capture assets.
    pub fn recovery_dir(&self) -> Option<PathBuf> {
        dirs::home_dir().map(|h| h.join(".config/rusty-riff/recovery").join(&self.temp_id))
    }

    pub fn temp_id(&self) -> &str {
        &self.temp_id
    }

    /// Convert project ticks to output frames at `out_rate`, rounding half away
    /// from zero. The single rounding rule shared by playback and export.
    pub fn ticks_to_frames(&self, ticks: u64, out_rate: f32) -> usize {
        ticks_to_frames(ticks, self.project_sample_rate, out_rate)
    }

    /// Convert output frames at `out_rate` back to project ticks.
    pub fn frames_to_ticks(&self, frames: usize, out_rate: f32) -> u64 {
        frames_to_ticks(frames, self.project_sample_rate, out_rate)
    }
}

/// Convert project ticks to frames at `out_rate`, rounding half away from zero.
pub fn ticks_to_frames(ticks: u64, project_rate: u32, out_rate: f32) -> usize {
    if project_rate == 0 || out_rate <= 0.0 {
        return ticks as usize;
    }
    let ratio = f64::from(out_rate) / f64::from(project_rate);
    (ticks as f64 * ratio).round() as usize
}

/// Convert frames at `out_rate` to project ticks, rounding half away from zero.
pub fn frames_to_ticks(frames: usize, project_rate: u32, out_rate: f32) -> u64 {
    if project_rate == 0 || out_rate <= 0.0 {
        return frames as u64;
    }
    let ratio = f64::from(project_rate) / f64::from(out_rate);
    (frames as f64 * ratio).round() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(id: TrackId, start: u64, len: u64) -> Track {
        Track {
            id,
            name: format!("t{id}"),
            kind: TrackKind::Import,
            lifecycle: TrackLifecycle::Ready,
            asset: None,
            start_ticks: start,
            length_ticks: len,
            gain: 1.0,
            muted: false,
            peaks: Vec::new(),
            generation: 0,
            input_trim_db: None,
            calibration_ref: None,
        }
    }

    #[test]
    fn extent_is_the_latest_clip_end() {
        let mut s = Session::new(48_000);
        assert_eq!(s.extent_ticks(), 0);
        s.push(
            1,
            "a".into(),
            TrackKind::Import,
            None,
            0,
            1000,
            TrackLifecycle::Ready,
        );
        s.push(
            2,
            "b".into(),
            TrackKind::Import,
            None,
            5000,
            1000,
            TrackLifecycle::Ready,
        );
        assert_eq!(s.extent_ticks(), 6000);
    }

    #[test]
    fn remove_reselects_a_neighbour() {
        let mut s = Session::new(48_000);
        s.push(
            1,
            "a".into(),
            TrackKind::Import,
            None,
            0,
            1,
            TrackLifecycle::Ready,
        );
        s.push(
            2,
            "b".into(),
            TrackKind::Import,
            None,
            0,
            1,
            TrackLifecycle::Ready,
        );
        s.select(1);
        assert_eq!(s.remove(1).map(|t| t.id), Some(1));
        assert_eq!(s.selected(), Some(2));
        assert_eq!(s.remove(2).map(|t| t.id), Some(2));
        assert_eq!(s.selected(), None);
        assert!(s.is_empty());
    }

    #[test]
    fn seek_steps_cycle_clamps_and_includes_fine_steps() {
        let mut s = Session::new(48_000);
        assert_eq!(s.seek_seconds(), 1.0, "new sessions start on the fine step");
        // Coarsen, and clamp at the top (no wrap).
        assert_eq!(s.cycle_seek_step(1), 5.0);
        assert_eq!(s.cycle_seek_step(1), 10.0);
        assert_eq!(s.cycle_seek_step(1), 30.0);
        assert_eq!(s.cycle_seek_step(1), 30.0);
        // Refine all the way down, and clamp at the bottom.
        assert_eq!(s.cycle_seek_step(-1), 10.0);
        assert_eq!(s.cycle_seek_step(-1), 5.0);
        assert_eq!(s.cycle_seek_step(-1), 1.0);
        assert_eq!(s.cycle_seek_step(-1), 0.5);
        assert_eq!(s.cycle_seek_step(-1), 0.1);
        assert_eq!(s.cycle_seek_step(-1), 0.05);
        assert_eq!(s.cycle_seek_step(-1), 0.01);
        assert_eq!(s.cycle_seek_step(-1), 0.01);
        s.set_seek_seconds(7.0);
        assert_eq!(s.seek_seconds(), 0.01, "unsupported steps are ignored");
    }

    #[test]
    fn set_seek_seconds_snaps_near_matches() {
        let mut s = Session::new(48_000);
        // Decimal round-trip of a sub-second step still lands on the step.
        s.set_seek_seconds(0.1);
        assert_eq!(s.seek_seconds(), 0.1);
        s.set_seek_seconds(0.010_000_1);
        assert_eq!(s.seek_seconds(), 0.01);
        s.set_seek_seconds(0.123);
        assert_eq!(s.seek_seconds(), 0.01, "far values are ignored");
    }

    #[test]
    fn tick_conversion_rounds_half_away_from_zero() {
        // 1 tick at 48 k = 1 frame at 48 k.
        assert_eq!(ticks_to_frames(1, 48_000, 48_000.0), 1);
        // 1 tick at 24 k = 2 frames at 48 k.
        assert_eq!(ticks_to_frames(1, 24_000, 48_000.0), 2);
        // Rounding: 3 ticks at 44.1 k -> 48 k is 3.265 -> 3.
        assert_eq!(ticks_to_frames(3, 44_100, 48_000.0), 3);
        assert_eq!(ticks_to_frames(100, 48_000, 48_000.0), 100);
        assert_eq!(frames_to_ticks(2, 24_000, 48_000.0), 1);
    }

    #[test]
    fn alloc_ids_are_unique_and_monotonic() {
        let mut s = Session::new(48_000);
        let a = s.alloc_id();
        let b = s.alloc_id();
        assert_ne!(a, b);
        assert!(b > a);
    }

    #[test]
    fn select_next_walks_tracks() {
        let mut s = Session::new(48_000);
        s.push(
            1,
            "a".into(),
            TrackKind::Import,
            None,
            0,
            1,
            TrackLifecycle::Ready,
        );
        s.push(
            2,
            "b".into(),
            TrackKind::Import,
            None,
            0,
            1,
            TrackLifecycle::Ready,
        );
        s.select(1);
        s.select_next(true);
        assert_eq!(s.selected(), Some(2));
        s.select_next(true);
        assert_eq!(s.selected(), Some(2), "clamps at the end");
        s.select_next(false);
        assert_eq!(s.selected(), Some(1));
    }

    #[test]
    fn engine_timebase_conversion_via_session() {
        let s = Session::new(24_000);
        assert_eq!(s.ticks_to_frames(1, 48_000.0), 2);
        assert_eq!(s.frames_to_ticks(2, 48_000.0), 1);
        let _ = track(9, 0, 0);
    }
}
