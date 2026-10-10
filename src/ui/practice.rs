//! Session timeline pane and track browser.
//!
//! The pane is a scrollable list: a transport line followed by one row per
//! timeline track (name, kind, mute, gain, start/end and a mini waveform scaled
//! to the shared session extent). Everything editable is owned by the
//! control-thread [`Session`]; decoded playback buffers are caches installed
//! into the audio engine.
//!
//! The browser imports a file as a **new** track at the current playhead. Raw
//! takes are captured dry through the take-bus pipeline (see
//! [`crate::recording`]) and re-amped live by the current rig.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crossterm::event::KeyCode;
use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Paragraph},
};
use std::sync::atomic::Ordering::Relaxed;

use super::styles::{ACCENT, AMBER, CHROME, DIM, GRID, HOT, SAFE, WARN, panel_style};
use crate::audio::AudioEngine;
use crate::audio::calibration::{InputCalibration, REFERENCE_VERSION};
use crate::dsp::Params;
use crate::dsp::cab::{ExternalIrCab, LIVE_MAX_IR_LEN, load_ir};
use crate::dsp::metronome::Metronome;
use crate::dsp::player::{MAX_TRACKS, TrackKind as PlayerKind};
use crate::practice::{DecodedTrack, Practice, decode_track, peak_buckets, peaks};
use crate::preset::Preset;
use crate::project::{self, AssetCopy, MetronomeSection, TrackSection, TransportSection};
use crate::recording::{
    CaptureCommand, CaptureResult, CaptureState, LivePeaks, spawn_capture_worker,
};
use crate::session::{AssetRef, Session, TrackId, TrackKind, TrackLifecycle};

/// Default track-row height, in terminal lines. Tall enough to read the
/// waveform envelope; `Tab` zooms a row beyond this.
const ROW_HEIGHT: usize = 3;

/// Fixed left gutter width shared by the ruler and every track row, so the
/// waveform columns line up. See `row_header`.
const GUTTER: usize = 27;

/// Tick spacings (seconds) the ruler may choose from, coarsest that avoids
/// label collisions. Sub-second entries matter once the view is zoomed in.
const RULER_TICKS: [f64; 17] = [
    0.01, 0.02, 0.05, 0.1, 0.2, 0.25, 0.5, 1.0, 2.0, 5.0, 10.0, 15.0, 30.0, 60.0, 120.0, 300.0,
    600.0,
];

/// Horizontal time-zoom windows in seconds, largest first. `None` (fit) sits
/// above these; zooming out past the largest returns to fit.
const ZOOM_WINDOWS: [f64; 8] = [60.0, 30.0, 10.0, 5.0, 2.0, 1.0, 0.5, 0.2];

/// Ruler span (seconds) shown when the timeline has no content yet, so the
/// empty pane still reads as a usable one-minute placement range.
const EMPTY_SPAN_SECS: f64 = 60.0;

/// One discovered audio file in the browser.
struct TrackFile {
    path: PathBuf,
    label: String,
    detail: String,
}

/// A background decode in flight, targeting a specific row id.
struct PendingDecode {
    id: TrackId,
    generation: u64,
    kind: TrackKind,
    path: PathBuf,
    rx: Receiver<Result<DecodedTrack, String>>,
}

/// Which row owns the pane cursor.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Selection {
    Transport,
    Track(TrackId),
}

/// Track-list row heights, cycled with `Tab` on a focused row.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum RowZoom {
    /// Every visible row at the default height.
    Normal,
    /// One row fills the whole track-list area.
    Expanded(TrackId),
    /// Two rows share the area 50/50.
    Split(TrackId, TrackId),
}

/// Waveform glyph family, cycled with `Shift+Tab`. Each cell is subdivided into
/// `cols × rows` block sub-cells.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum WaveGlyphs {
    /// 2 × 4 dots (U+2800) — finest, dotted.
    Braille,
    /// 2 × 3 solid blocks (legacy computing U+1FB00).
    Sextant,
    /// 2 × 2 solid quadrants.
    Quadrant,
    /// 1 × 2 half blocks — the coarsest, fully connected.
    Half,
}

/// How the waveform maps sample amplitude to row height.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum WaveGain {
    /// Auto-scale each track so its loudest peak fills most of the row.
    Normalized,
    /// True scale: sample values are shown as-is (peaks at ±1 fill the row).
    Absolute,
}

impl WaveGain {
    fn next(self) -> Self {
        match self {
            WaveGain::Normalized => WaveGain::Absolute,
            WaveGain::Absolute => WaveGain::Normalized,
        }
    }

    fn label(self) -> &'static str {
        match self {
            WaveGain::Normalized => "normalized",
            WaveGain::Absolute => "absolute",
        }
    }
}

/// Fraction of the row the loudest peak fills under [`WaveGain::Normalized`].
const PEAK_FILL: f32 = 0.9;

/// Display gain that scales a track so its loudest peak fills [`PEAK_FILL`] of
/// the row. Silence (or an empty envelope) stays flat.
fn envelope_gain(peaks: &[(f32, f32)]) -> f32 {
    let peak = peaks
        .iter()
        .fold(0.0f32, |m, &(lo, hi)| m.max(hi.abs()).max(lo.abs()));
    if peak > 1e-6 { PEAK_FILL / peak } else { 0.0 }
}

impl WaveGlyphs {
    fn cols(self) -> usize {
        match self {
            WaveGlyphs::Half => 1,
            _ => 2,
        }
    }

    fn rows(self) -> usize {
        match self {
            WaveGlyphs::Braille => 4,
            WaveGlyphs::Sextant => 3,
            WaveGlyphs::Quadrant | WaveGlyphs::Half => 2,
        }
    }

    fn label(self) -> &'static str {
        match self {
            WaveGlyphs::Braille => "braille",
            WaveGlyphs::Sextant => "sextant",
            WaveGlyphs::Quadrant => "quadrant",
            WaveGlyphs::Half => "half-block",
        }
    }

    fn next(self) -> Self {
        match self {
            WaveGlyphs::Braille => WaveGlyphs::Sextant,
            WaveGlyphs::Sextant => WaveGlyphs::Quadrant,
            WaveGlyphs::Quadrant => WaveGlyphs::Half,
            WaveGlyphs::Half => WaveGlyphs::Braille,
        }
    }

    /// Render an occupied sub-cell pattern. `cells` is row-major: bit
    /// `r * cols + d`.
    fn glyph(self, cells: u32) -> char {
        match self {
            WaveGlyphs::Braille => {
                let mut mask = 0u8;
                for r in 0..4 {
                    for d in 0..2 {
                        if cells & (1 << (r * 2 + d)) != 0 {
                            mask |= braille_bit(d, r);
                        }
                    }
                }
                braille_glyph(mask)
            }
            WaveGlyphs::Sextant => SEXTANT_GLYPHS[(cells & 0x3F) as usize],
            WaveGlyphs::Quadrant => quadrant_glyph(cells as u8),
            WaveGlyphs::Half => match cells & 0b11 {
                0b01 => '▀',
                0b10 => '▄',
                0b11 => '█',
                _ => ' ',
            },
        }
    }
}

/// How long a transient status message stays on screen.
const NOTICE_TTL: Duration = Duration::from_secs(5);

/// A transient status message with the time it was raised, so the header can
/// retire it after [`NOTICE_TTL`].
struct Notice {
    text: String,
    at: Instant,
}

impl Notice {
    fn expired(&self, now: Instant) -> bool {
        now.duration_since(self.at) >= NOTICE_TTL
    }
}

/// All practice UI state, owned by the UI thread. Persists across a device
/// change: the [`Session`] and its recovery paths are the source of truth, and
/// decoded caches are rebuilt.
pub(super) struct PracticeUi {
    pub(super) browser_open: bool,
    browser_cursor: usize,
    files: Vec<TrackFile>,
    /// Typed path alternative to browsing.
    path_input: String,
    /// 0 = file list, 1 = path field.
    field: usize,
    /// Live filter over the file list while `/` search is active.
    browser_filter: String,
    searching: bool,
    /// The Library sub-view (edit + persist the import path and its flags).
    library_open: bool,
    library_field: usize,
    library_path: String,
    library_only: bool,
    library_subpaths: bool,
    message: Option<Notice>,
    sample_rate: f32,
    decodes: Vec<PendingDecode>,

    /// Canonical project state.
    pub(super) session: Session,
    selection: Selection,
    /// Track-list row-height zoom (cycled with `Tab`).
    row_zoom: RowZoom,
    /// Horizontal time-zoom window in seconds; `None` fits the whole timeline.
    view_secs: Option<f64>,
    /// Waveform glyph family (cycled with `Shift+Tab`).
    wave_glyphs: WaveGlyphs,
    /// Waveform amplitude mapping (cycled with `Shift+A`).
    wave_gain: WaveGain,
    /// Row zoom to restore when the active take stops (recording auto-expands
    /// its row to the full pane).
    record_zoom: Option<RowZoom>,

    /// Capture plumbing (attached per engine).
    capture_cmd: Option<Sender<CaptureCommand>>,
    capture_result: Option<Receiver<CaptureResult>>,
    recording_id: Option<TrackId>,
    /// Growing peaks published by the capture worker while a take records.
    live_peaks: Option<Arc<Mutex<LivePeaks>>>,
    /// Small modal editing a track's gain.
    gain_edit: Option<TrackId>,
    /// Small modal nudging a track's timeline start.
    move_edit: Option<TrackId>,
    next_generation: u64,
    /// The project time base is adopted from the first engine and kept across
    /// later device changes, so clip offsets never drift.
    rate_adopted: bool,
}

impl PracticeUi {
    pub(super) fn new() -> Self {
        Self {
            browser_open: false,
            browser_cursor: 0,
            files: Vec::new(),
            path_input: String::new(),
            field: 0,
            browser_filter: String::new(),
            searching: false,
            library_open: false,
            library_field: 0,
            library_path: String::new(),
            library_only: false,
            library_subpaths: false,
            message: None,
            sample_rate: 48_000.0,
            decodes: Vec::new(),
            session: Session::new(48_000),
            selection: Selection::Transport,
            row_zoom: RowZoom::Normal,
            view_secs: None,
            wave_glyphs: WaveGlyphs::Braille,
            wave_gain: WaveGain::Normalized,
            record_zoom: None,
            capture_cmd: None,
            capture_result: None,
            recording_id: None,
            live_peaks: None,
            gain_edit: None,
            move_edit: None,
            next_generation: 1,
            rate_adopted: false,
        }
    }

    fn generation(&mut self) -> u64 {
        let g = self.next_generation;
        self.next_generation = self.next_generation.saturating_add(1);
        g
    }

    /// Adopt the running engine's rate and (re)attach the capture writer. Called
    /// once per engine start; keeps the session across device changes.
    pub(super) fn attach(&mut self, engine: &mut AudioEngine) {
        self.sample_rate = engine.sample_rate();
        // Adopt the project time base once; later device changes keep it.
        if !self.rate_adopted {
            self.session
                .set_project_sample_rate(self.sample_rate as u32);
            self.rate_adopted = true;
        }
        if let Some(consumer) = engine.take_capture_consumer() {
            self.capture_cmd = Some(spawn_capture_worker(consumer));
        }
        // Any decode that was in flight was resampled for the *old* rate; reissue
        // it at the new rate rather than installing a mismatched buffer.
        let inflight = std::mem::take(&mut self.decodes);
        self.reinstall_ready_tracks();
        for pending in inflight {
            self.start_decode(pending.id, pending.kind, pending.path);
        }
    }

    /// Re-decode every ready track from its asset into the fresh engine (device
    /// change). Missing files become error rows rather than silently vanishing.
    fn reinstall_ready_tracks(&mut self) {
        let ids: Vec<TrackId> = self
            .session
            .tracks()
            .iter()
            .filter(|t| t.is_ready() && t.asset.is_some())
            .map(|t| t.id)
            .collect();
        for id in ids {
            let Some(track) = self.session.track(id) else {
                continue;
            };
            let Some(asset) = track.asset.clone() else {
                continue;
            };
            self.start_decode(id, track.kind, asset.path);
        }
    }

    /// Open the browser, rescanning the configured practice locations.
    pub(super) fn open_browser(&mut self) {
        self.files = scan();
        self.browser_cursor = 0;
        self.field = 0;
        self.browser_filter.clear();
        self.searching = false;
        self.library_open = false;
        self.message = None;
        self.browser_open = true;
    }

    /// Rescan the library after a settings change, keeping the cursor in range.
    fn rescan_files(&mut self) {
        self.files = scan();
        self.browser_cursor = self.browser_cursor.min(self.files.len().saturating_sub(1));
    }

    /// Open the Library settings sub-view, seeded from the saved config.
    fn open_library(&mut self) {
        let cfg = load_import_config();
        self.library_path = cfg
            .path
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();
        self.library_only = cfg.only;
        self.library_subpaths = cfg.subpaths;
        self.library_field = 0;
        self.library_open = true;
        self.searching = false;
    }

    /// Persist the Library settings and rescan.
    fn save_library(&mut self) {
        let path = self.library_path.trim();
        let cfg = ImportConfig {
            path: (!path.is_empty()).then(|| PathBuf::from(path)),
            only: self.library_only,
            subpaths: self.library_subpaths,
        };
        match save_import_config(&cfg) {
            Ok(()) => {
                self.library_open = false;
                self.rescan_files();
                self.note("Library saved");
            }
            Err(e) => self.note(format!("Library save failed: {e}")),
        }
    }

    /// Start a background decode of `path`, targeting `id`.
    fn start_decode(&mut self, id: TrackId, kind: TrackKind, path: PathBuf) {
        let sr = self.sample_rate;
        let (tx, rx) = std::sync::mpsc::channel();
        let decode_path = path.clone();
        std::thread::spawn(move || {
            let result = decode_track(&decode_path, sr).map_err(|e| e.to_string());
            let _ = tx.send(result);
        });
        let generation = self.generation();
        self.decodes.push(PendingDecode {
            id,
            generation,
            kind,
            path,
            rx,
        });
    }

    /// Create a new import row at the current playhead and start decoding.
    fn import_at_playhead(&mut self, path: PathBuf, practice: &Practice) {
        let id = self.session.alloc_id();
        let start_ticks = self
            .session
            .frames_to_ticks(practice.position(), self.sample_rate);
        let name = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("import")
            .to_owned();
        self.session.push(
            id,
            name,
            TrackKind::Import,
            None,
            start_ticks,
            0,
            TrackLifecycle::Loading,
        );
        self.selection = Selection::Track(id);
        self.start_decode(id, TrackKind::Import, path);
        self.note("Loading…".to_owned());
    }

    /// Poll background decodes and capture results once per UI tick. Returns
    /// `true` if the engine was touched.
    pub(super) fn poll(
        &mut self,
        engine: &mut AudioEngine,
        practice: &Practice,
        capture: &CaptureState,
        calibration: &InputCalibration,
    ) -> bool {
        // Retire a stale status message.
        if self
            .message
            .as_ref()
            .is_some_and(|n| n.expired(Instant::now()))
        {
            self.message = None;
        }

        let mut touched = false;
        touched |= self.poll_decodes(engine);
        touched |= self.poll_capture(engine, capture, calibration);
        touched |= self.poll_acks(engine);
        // Auto-stopped at a loop out-point: finalize on the UI side.
        if self.recording_id.is_some()
            && capture.auto_stop.load(Relaxed)
            && capture.active.load(Relaxed)
        {
            self.finalize_capture(capture, practice);
        }
        touched
    }

    fn poll_decodes(&mut self, engine: &mut AudioEngine) -> bool {
        let mut touched = false;
        // Move the queue out so `self` methods (which touch `self.decodes`) can
        // be called while iterating.
        let mut queue = std::mem::take(&mut self.decodes);
        let mut keep = Vec::with_capacity(queue.len());
        for pending in queue.drain(..) {
            match pending.rx.try_recv() {
                Ok(Ok(decoded)) => {
                    self.finish_decode(engine, &pending, decoded);
                    touched = true;
                }
                Ok(Err(e)) => {
                    self.set_error(pending.id, e);
                    touched = true;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => keep.push(pending),
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.set_error(pending.id, "decode worker disconnected".to_owned());
                    touched = true;
                }
            }
        }
        self.decodes = keep;
        touched
    }

    fn finish_decode(
        &mut self,
        engine: &mut AudioEngine,
        pending: &PendingDecode,
        decoded: DecodedTrack,
    ) {
        let frames = decoded.track.frames();
        let Some((start_ticks, gain, muted, name)) = self
            .session
            .track(pending.id)
            .map(|t| (t.start_ticks, t.gain, t.muted, t.name.clone()))
        else {
            return;
        };
        let start_frames = self.session.ticks_to_frames(start_ticks, self.sample_rate);
        let length_ticks = self.session.frames_to_ticks(frames, self.sample_rate);
        let mut player_track = decoded.track;
        player_track.start = start_frames;
        let pk = peaks(&player_track, peak_buckets(frames, self.sample_rate));

        let asset = AssetRef {
            path: pending.path.clone(),
            source_sample_rate: decoded.source_sample_rate,
            source_channels: decoded.source_channels,
        };
        if let Some(track) = self.session.track_mut(pending.id) {
            track.asset = Some(asset);
            track.length_ticks = length_ticks;
            track.peaks = pk;
            track.generation = pending.generation;
            track.lifecycle = TrackLifecycle::Ready;
        }

        let kind = to_player_kind(pending.kind);
        if let Err(e) = engine.install_track(
            pending.id,
            pending.generation,
            kind,
            player_track,
            gain,
            muted,
        ) {
            self.set_error(pending.id, e.to_string());
            return;
        }
        self.note(format!("Loaded {name}"));
    }

    fn poll_acks(&mut self, engine: &mut AudioEngine) -> bool {
        let acks = engine.poll_track_acks();
        let mut touched = false;
        for ack in acks {
            if !ack.installed
                && let Some(err) = ack.error
            {
                self.set_error(ack.id, err);
                touched = true;
            }
        }
        touched
    }

    fn poll_capture(
        &mut self,
        engine: &mut AudioEngine,
        capture: &CaptureState,
        calibration: &InputCalibration,
    ) -> bool {
        let Some(rx) = &self.capture_result else {
            return false;
        };
        let Ok(result) = rx.try_recv() else {
            return false;
        };
        self.capture_result = None;
        let current = self.recording_id;
        if current != Some(result.generation) {
            return false;
        }
        self.recording_id = None;
        self.live_peaks = None;
        let mut touched = false;
        if let Some(err) = result.error.clone() {
            self.set_error(result.generation, err);
            return touched;
        }
        let Some(player_track) = result.track else {
            self.session.remove(result.generation);
            self.reselect_after_removal();
            self.sanitize_zoom();
            self.note("Empty take discarded".to_owned());
            return touched;
        };
        let frames = player_track.frames();
        // The audio thread records the exact project frame of the first captured
        // sample; fall back to the arm-time playhead only if that latch is unset.
        let start_frame = capture.start_frame.load(Relaxed) as usize;
        let start_ticks = self.session.frames_to_ticks(start_frame, self.sample_rate);
        let length_ticks = self.session.frames_to_ticks(frames, self.sample_rate);
        let generation = self.generation();
        let (gain, muted, name) = self
            .session
            .track(result.generation)
            .map_or((1.0, false, "Take".to_owned()), |t| {
                (t.gain, t.muted, t.name.clone())
            });
        let take_path = result.path.clone();
        if let Some(track) = self.session.track_mut(result.generation) {
            track.asset = Some(AssetRef {
                path: result.path,
                source_sample_rate: self.sample_rate as u32,
                source_channels: 1,
            });
            track.start_ticks = start_ticks;
            track.length_ticks = length_ticks;
            track.peaks = result.peaks.clone();
            track.generation = generation;
            track.lifecycle = TrackLifecycle::Ready;
            // Record the calibration this take was captured under, so it can be
            // re-amped/exported identically. Trim 0 (uncalibrated) records None.
            let trim = calibration.trim_db.load(Relaxed);
            let (input_trim_db, calibration_ref) = if trim.abs() < 1e-3 {
                (None, None)
            } else {
                (Some(trim), Some(REFERENCE_VERSION))
            };
            track.input_trim_db = input_trim_db;
            track.calibration_ref = calibration_ref;
        }
        if let Err(e) = engine.install_track(
            result.generation,
            generation,
            PlayerKind::RawTake,
            player_track,
            gain,
            muted,
        ) {
            self.set_error(result.generation, e.to_string());
            return touched;
        }
        touched = true;
        // Record enough metadata that an unsaved take is recoverable next launch.
        let meta = project::RecoveryMeta {
            version: project::RECOVERY_META_VERSION,
            id: result.generation,
            name,
            start_ticks,
            project_sample_rate: self.session.project_sample_rate(),
            source_sample_rate: self.sample_rate as u32,
            frames: frames as u64,
            overflowed: result.overflowed,
        };
        let meta_note = match project::write_recovery_meta(&take_path, &meta) {
            Ok(()) => "",
            Err(_) => " (recovery metadata failed)",
        };
        let note = if result.overflowed {
            " (incomplete: capture overflowed)"
        } else {
            ""
        };
        self.note(format!("Take ready{note}{meta_note}"));
        touched
    }

    fn set_error(&mut self, id: TrackId, msg: String) {
        if let Some(track) = self.session.track_mut(id) {
            track.lifecycle = TrackLifecycle::Error(msg.clone());
        }
        self.note(msg);
    }

    // ── Focused-control edits (called from the key handler) ─────────────────────

    pub(super) fn toggle_play(&self, practice: &Practice) {
        let now = !practice.playing.load(Relaxed);
        practice.playing.store(now, Relaxed);
    }

    pub(super) fn toggle_loop(&self, practice: &Practice) {
        let now = !practice.loop_enabled.load(Relaxed);
        practice.loop_enabled.store(now, Relaxed);
    }

    /// Move the cursor between track rows. The transport/header row is not a
    /// landing spot: with tracks present the cursor always sits on one of them
    /// (the first/last when nothing is selected yet).
    pub(super) fn move_selection(&mut self, forward: bool) {
        if self.selected_track().is_none() {
            let id = if forward {
                self.session.tracks().first().map(|t| t.id)
            } else {
                self.session.tracks().last().map(|t| t.id)
            };
            if let Some(id) = id {
                self.selection = Selection::Track(id);
                self.session.select(id);
            }
            return;
        }
        self.session.select_next(forward);
        if let Some(next) = self.session.selected() {
            self.selection = Selection::Track(next);
        }
    }

    /// After a row disappears, keep the cursor on a track (or the empty
    /// transport state when the timeline is now empty).
    fn reselect_after_removal(&mut self) {
        self.selection = match self.session.selected() {
            Some(id) => Selection::Track(id),
            None => Selection::Transport,
        };
    }

    /// `Tab` on the timeline: cycle the focused row's zoom state.
    /// `Normal → Expanded(focused)`; from `Expanded(A)`, `Tab` on `B` gives
    /// `Split(A, B)`; `Tab` on an already zoomed row collapses to `Normal`.
    pub(super) fn tab_zoom(&mut self) {
        let Selection::Track(id) = self.selection else {
            return;
        };
        if self.session.track(id).is_none() {
            return;
        }
        self.row_zoom = match self.row_zoom {
            RowZoom::Normal => RowZoom::Expanded(id),
            RowZoom::Expanded(a) if a == id => RowZoom::Normal,
            RowZoom::Expanded(a) => RowZoom::Split(a, id),
            RowZoom::Split(_, _) => RowZoom::Normal,
        };
    }

    /// Drop a zoom state that references a removed row.
    fn sanitize_zoom(&mut self) {
        let dead = |id: &TrackId| self.session.track(*id).is_none();
        match self.row_zoom {
            RowZoom::Expanded(a) if dead(&a) => self.row_zoom = RowZoom::Normal,
            RowZoom::Split(a, b) if dead(&a) || dead(&b) => self.row_zoom = RowZoom::Normal,
            _ => {}
        }
    }

    pub(super) fn seek_by(&self, practice: &Practice, direction: i32) {
        let step = f64::from(self.session.seek_seconds());
        let delta = (step * f64::from(self.sample_rate)) as i64;
        let cur = practice.position() as i64;
        // No upward clamp to the clip extent: an empty (or short) timeline must
        // still let the playhead move so the first import lands where you put it.
        let next = (cur + delta * i64::from(direction)).max(0);
        practice.request_seek(next as usize);
    }

    pub(super) fn cycle_seek_step(&mut self, direction: i32) {
        let step = self.session.cycle_seek_step(direction);
        self.note(format!("Step: {}s", format_step(step)));
    }

    /// Shared time-axis length for the transport, ruler, and waveform rows: the
    /// last clip's end, but at least the current playhead, so a seek into empty
    /// space (or a fresh recording) stays visible.
    fn span_frames(&self, practice: &Practice) -> usize {
        let extent = self
            .session
            .ticks_to_frames(self.session.extent_ticks(), self.sample_rate);
        let extent = if extent == 0 {
            (EMPTY_SPAN_SECS * f64::from(self.sample_rate)) as usize
        } else {
            extent
        };
        extent.max(practice.position()).max(1)
    }

    /// The visible time window `(start_frame, len_frames)`. Fit (`None`) covers
    /// the whole span; a zoomed window is centred on the playhead and clamped so
    /// it never runs past the span.
    fn view(&self, practice: &Practice) -> (usize, usize) {
        let span = self.span_frames(practice);
        let Some(secs) = self.view_secs else {
            return (0, span);
        };
        let len = ((secs * f64::from(self.sample_rate)).round() as usize).clamp(1, span.max(1));
        let pos = practice.position().min(span);
        let start = pos.saturating_sub(len / 2).min(span - len);
        (start, len)
    }

    /// Zoom the time axis in (smaller window) around the playhead.
    pub(super) fn zoom_in(&mut self) {
        self.view_secs = match self.view_secs {
            None => Some(ZOOM_WINDOWS[0]),
            Some(cur) => ZOOM_WINDOWS
                .iter()
                .rev()
                .find(|&&w| w < cur - 1e-9)
                .copied()
                .or(Some(cur)),
        };
        self.note(self.view_label());
    }

    /// Zoom the time axis out; past the largest window returns to fit.
    pub(super) fn zoom_out(&mut self) {
        self.view_secs = match self.view_secs {
            None => None,
            Some(cur) => ZOOM_WINDOWS.iter().find(|&&w| w > cur + 1e-9).copied(),
        };
        self.note(self.view_label());
    }

    fn view_label(&self) -> String {
        match self.view_secs {
            None => "View: fit".to_owned(),
            Some(s) => format!("View: {}s", format_step(s as f32)),
        }
    }

    /// `Shift+Tab`: cycle the waveform glyph family.
    pub(super) fn cycle_glyphs(&mut self) {
        self.wave_glyphs = self.wave_glyphs.next();
        self.note(format!("Waveform: {}", self.wave_glyphs.label()));
    }

    /// `Shift+A`: toggle the waveform amplitude mapping.
    pub(super) fn cycle_gain(&mut self) {
        self.wave_gain = self.wave_gain.next();
        self.note(format!("Waveform gain: {}", self.wave_gain.label()));
    }

    /// Set the loop in-point at the playhead, opening a one-second region if the
    /// current out-point is not ahead of it.
    pub(super) fn set_loop_start(&self, practice: &Practice) {
        let pos = practice.position() as u64;
        practice.loop_start.store(pos, Relaxed);
        if practice.loop_end.load(Relaxed) <= pos {
            practice
                .loop_end
                .store(pos + self.sample_rate as u64, Relaxed);
        }
        practice.loop_enabled.store(true, Relaxed);
    }

    /// Set the loop out-point at the playhead, pulling the in-point back if needed.
    pub(super) fn set_loop_end(&self, practice: &Practice) {
        let pos = practice.position() as u64;
        practice.loop_end.store(pos, Relaxed);
        if practice.loop_start.load(Relaxed) >= pos {
            practice.loop_start.store(0, Relaxed);
        }
        practice.loop_enabled.store(true, Relaxed);
    }

    pub(super) fn toggle_selected_mute(&mut self, engine: &mut AudioEngine) {
        let Some(id) = self.selected_track() else {
            return;
        };
        let Some(track) = self.session.track_mut(id) else {
            return;
        };
        track.muted = !track.muted;
        let muted = track.muted;
        let _ = engine.set_track_mute(id, muted);
    }

    pub(super) fn delete_selected(
        &mut self,
        engine: &mut AudioEngine,
        practice: &Practice,
        capture: &CaptureState,
    ) {
        let _ = practice;
        let Some(id) = self.selected_track() else {
            return;
        };
        // Removing the row that is currently recording also stops the writer.
        if self.recording_id == Some(id) {
            self.abort_capture(capture);
        }
        // A deleted take's recovery file must go too, or it is offered back as a
        // "recoverable take" on the next launch even though it was discarded.
        let recovery = self.track_recovery_file(id);
        let _ = engine.remove_track(id);
        if let Some(path) = recovery {
            project::discard_recovery_file(&path);
        }
        self.session.remove(id);
        self.reselect_after_removal();
        self.sanitize_zoom();
    }

    /// The recovery WAV a track points at, if it is one, so removing the track can
    /// discard it. Imported assets and takes already saved into a project live
    /// elsewhere, so those return `None` and their files are never touched.
    fn track_recovery_file(&self, id: TrackId) -> Option<PathBuf> {
        self.session
            .track(id)
            .and_then(|t| t.asset.as_ref())
            .map(|a| a.path.clone())
            .filter(|p| project::is_recovery_asset(p))
    }

    fn selected_track(&self) -> Option<TrackId> {
        match self.selection {
            Selection::Transport => None,
            Selection::Track(id) => Some(id),
        }
    }

    /// Jump to the loop in-point when a valid loop is enabled, otherwise to the
    /// root of the timeline (frame 0).
    pub(super) fn go_to_start(&self, practice: &Practice) {
        let a = practice.loop_start.load(Relaxed) as usize;
        let b = practice.loop_end.load(Relaxed) as usize;
        let looping = practice.loop_enabled.load(Relaxed) && b > a;
        practice.request_seek(if looping { a } else { 0 });
    }

    // ── Capture lifecycle ───────────────────────────────────────────────────────

    /// Handle `R`: arm a fresh raw-take row, or stop the active take.
    /// Arm a new take, or stop the one in flight. Returns `true` when a take was
    /// just started, so the caller can bring the timeline panel forward.
    pub(super) fn arm_or_stop(
        &mut self,
        engine: &mut AudioEngine,
        practice: &Practice,
        capture: &CaptureState,
    ) -> bool {
        let _ = engine;
        if self.recording_id.is_some() {
            // Stop: silence the callback, ask the writer to finalize, and park the
            // transport where the take ended.
            self.finalize_capture(capture, practice);
            return false;
        }
        self.arm(capture, practice)
    }

    fn arm(&mut self, capture: &CaptureState, practice: &Practice) -> bool {
        let ready = self
            .session
            .tracks()
            .iter()
            .filter(|t| t.is_ready())
            .count();
        if ready >= MAX_TRACKS {
            self.note(format!("Timeline is full ({MAX_TRACKS} tracks)"));
            return false;
        }
        let Some(cmd) = &self.capture_cmd else {
            self.note("Capture writer unavailable".to_owned());
            return false;
        };
        let id = self.session.alloc_id();
        let start_ticks = self
            .session
            .frames_to_ticks(practice.position(), self.sample_rate);
        let count = self
            .session
            .tracks()
            .iter()
            .filter(|t| t.kind == TrackKind::RawTake)
            .count()
            + 1;
        self.session.push(
            id,
            format!("Take {count}"),
            TrackKind::RawTake,
            None,
            start_ticks,
            0,
            TrackLifecycle::Recording,
        );
        self.selection = Selection::Track(id);
        // A recording gets the whole pane so its waveform is easy to watch;
        // restore whatever was shown when it stops.
        self.record_zoom = Some(self.row_zoom);
        self.row_zoom = RowZoom::Expanded(id);

        let path = self
            .session
            .recovery_dir()
            .map(|d| d.join(format!("take-{id}.wav")))
            .unwrap_or_else(|| PathBuf::from(format!("take-{id}.wav")));
        let (tx, rx) = std::sync::mpsc::channel();
        let live = Arc::new(Mutex::new(LivePeaks::new(self.sample_rate as u32)));
        if cmd
            .send(CaptureCommand::Begin {
                generation: id,
                path,
                sample_rate: self.sample_rate as u32,
                result: tx,
                live: live.clone(),
            })
            .is_err()
        {
            self.note("Capture writer unavailable".to_owned());
            self.session.remove(id);
            self.reselect_after_removal();
            return false;
        }
        self.capture_result = Some(rx);
        self.recording_id = Some(id);
        self.live_peaks = Some(live);
        capture.arm(id);
        // Start transport if paused so the take follows the playhead.
        practice.playing.store(true, Relaxed);
        self.note("Recording…".to_owned());
        true
    }

    fn finalize_capture(&mut self, capture: &CaptureState, practice: &Practice) {
        capture.disarm();
        if let Some(tx) = &self.capture_cmd {
            let _ = tx.send(CaptureCommand::End);
        }
        // Stopping a take pauses the timeline so the playhead stays on the take.
        practice.playing.store(false, Relaxed);
        if let Some(z) = self.record_zoom.take() {
            self.row_zoom = z;
        }
        self.note("Finalizing take…".to_owned());
    }

    /// Abort an in-flight take (device change / quit).
    pub(super) fn abort_capture(&mut self, capture: &CaptureState) {
        capture.disarm();
        if let Some(tx) = &self.capture_cmd {
            let _ = tx.send(CaptureCommand::Abort);
        }
        if let Some(id) = self.recording_id.take() {
            self.session.remove(id);
        }
        if let Some(z) = self.record_zoom.take() {
            self.row_zoom = z;
        }
        self.capture_result = None;
        self.live_peaks = None;
        self.reselect_after_removal();
    }

    pub(super) fn is_recording(&self) -> bool {
        self.recording_id.is_some()
    }

    pub(super) fn set_message(&mut self, msg: String) {
        self.note(msg);
    }

    /// Raise a transient status message in the timeline header.
    fn note(&mut self, text: impl Into<String>) {
        self.message.replace(Notice {
            text: text.into(),
            at: Instant::now(),
        });
    }

    // ── Session persistence ─────────────────────────────────────────────────────

    /// Start a fresh, empty project at the current engine rate, dropping every
    /// installed track and any in-flight capture.
    pub(super) fn new_session(
        &mut self,
        engine: &mut AudioEngine,
        practice: &Practice,
        metronome: &Metronome,
        capture: &CaptureState,
    ) {
        if self.recording_id.is_some() {
            self.abort_capture(capture);
        }
        let ids: Vec<TrackId> = self.session.tracks().iter().map(|t| t.id).collect();
        for id in ids {
            let _ = engine.remove_track(id);
        }
        self.decodes.clear();
        self.capture_result = None;
        self.live_peaks = None;
        let rate = self.sample_rate as u32;
        self.session = Session::new(rate);
        self.session.set_project_sample_rate(rate);
        self.rate_adopted = true;
        self.selection = Selection::Transport;
        self.row_zoom = RowZoom::Normal;
        self.view_secs = None;
        self.record_zoom = None;
        practice.reset();
        metronome.active.store(false, Relaxed);
        self.note("New session".to_owned());
    }

    /// Save the current project to `ctx.dir`, copying every ready track's source
    /// asset and the selected IR into the folder. Returns how many tracks had no
    /// recoverable source (and were therefore not written).
    pub(super) fn save_session(&mut self, ctx: SaveContext<'_>) -> anyhow::Result<usize> {
        let name = self.session.name().to_owned();
        let mut sections = Vec::with_capacity(self.session.len());
        let mut assets = Vec::new();
        let mut written: Vec<(TrackId, String)> = Vec::new();
        let mut gc: Vec<PathBuf> = Vec::new();
        let mut skipped = 0usize;
        for track in self.session.tracks() {
            let rel = match (&track.asset, track.is_ready()) {
                (Some(asset), true) => {
                    let ext = asset
                        .path
                        .extension()
                        .and_then(|e| e.to_str())
                        .unwrap_or("wav");
                    let rel = format!("audio/track-{}.{}", track.id, ext);
                    let dst = ctx.dir.join(&rel);
                    if asset.path.exists() {
                        assets.push(AssetCopy {
                            source: asset.path.clone(),
                            rel: rel.clone(),
                        });
                        if project::is_recovery_asset(&asset.path) {
                            gc.push(asset.path.clone());
                        }
                        written.push((track.id, rel.clone()));
                        Some(rel)
                    } else if dst.exists() {
                        // The source vanished but the project already holds the copy
                        // -- e.g. a recovery take discarded after being restored into
                        // the session. Keep the copy and retarget the track at it.
                        written.push((track.id, rel.clone()));
                        Some(rel)
                    } else {
                        // No source and no copy: the audio is unrecoverable. Save the
                        // track without it rather than failing the whole save, which
                        // is what used to happen.
                        skipped += 1;
                        None
                    }
                }
                _ => {
                    skipped += 1;
                    None
                }
            };
            sections.push(TrackSection {
                id: track.id,
                kind: kind_slug(track.kind).to_owned(),
                name: track.name.clone(),
                asset: rel,
                start_ticks: track.start_ticks,
                length_ticks: track.length_ticks,
                gain: track.gain,
                muted: track.muted,
                input_trim_db: track.input_trim_db,
                calibration_ref: track.calibration_ref,
            });
        }

        let external_ir = match ctx.external_ir {
            Some(path) => {
                let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("wav");
                let rel = format!("irs/cabinet.{ext}");
                assets.push(AssetCopy {
                    source: path.to_path_buf(),
                    rel: rel.clone(),
                });
                Some(rel)
            }
            None => None,
        };

        // External plugins: write identity + state sidecars.
        let mut blobs: Vec<project::AssetBytes> = Vec::new();
        let clap_insert = ctx.clap.as_ref().map(|spec| {
            let rel = "plugins/insert.state".to_owned();
            blobs.push(project::AssetBytes {
                rel: rel.clone(),
                bytes: spec.state.clone(),
            });
            project::ClapInsertSection {
                path: spec.path.to_string_lossy().into_owned(),
                id: spec.id.clone(),
                name: spec.name.clone(),
                state: Some(rel),
            }
        });
        let au_amp = ctx.au.as_ref().map(|spec| {
            let rel = "plugins/amp.params".to_owned();
            let list: Vec<project::AuParamState> = spec
                .params
                .iter()
                .map(|&(id, value)| project::AuParamState { id, value })
                .collect();
            if let Ok(text) = toml::to_string(&project::AuParamsFile { params: list }) {
                blobs.push(project::AssetBytes {
                    rel: rel.clone(),
                    bytes: text.into_bytes(),
                });
            }
            project::AuAmpSection {
                name: spec.name.clone(),
                type_code: spec.type_code,
                subtype: spec.subtype,
                manufacturer: spec.manufacturer,
                amp_only: spec.amp_only,
                params: Some(rel),
            }
        });

        let transport = TransportSection {
            playhead: self
                .session
                .frames_to_ticks(ctx.practice.position(), self.sample_rate),
            seek_seconds: self.session.seek_seconds(),
            loop_enabled: ctx.practice.loop_enabled.load(Relaxed),
            loop_start: self.session.frames_to_ticks(
                ctx.practice.loop_start.load(Relaxed) as usize,
                self.sample_rate,
            ),
            loop_end: self.session.frames_to_ticks(
                ctx.practice.loop_end.load(Relaxed) as usize,
                self.sample_rate,
            ),
        };
        let metronome = MetronomeSection {
            enabled: ctx.metronome.active.load(Relaxed),
            bpm: ctx.metronome.get_bpm(),
        };
        let rig = Preset::from_params(name.clone(), None, ctx.params);
        let manifest = project::build_manifest(
            name,
            self.session.project_sample_rate(),
            transport,
            metronome,
            rig,
            external_ir,
            ctx.external_ir_active,
            clap_insert,
            au_amp,
            sections,
        )?;
        project::write_session(ctx.dir, &manifest, &assets, &blobs)?;
        // Retarget each track at its copy inside the project folder, so the
        // running session and later saves reference the portable asset.
        for (id, rel) in written {
            if let Some(track) = self.session.track_mut(id)
                && let Some(asset) = track.asset.as_mut()
            {
                asset.path = ctx.dir.join(rel);
            }
        }
        // Verified incorporation: the recovery copies are now redundant.
        for wav in gc {
            project::discard_recovery_file(&wav);
        }
        self.session.set_saved_dir(Some(ctx.dir.to_path_buf()));
        Ok(skipped)
    }

    /// Load the project folder `dir`: replace the session, apply the rig and
    /// external IR, restore the transport/metronome, and (re)decode every track.
    pub(super) fn load_session(
        &mut self,
        dir: &Path,
        engine: &mut AudioEngine,
        params: &Params,
        practice: &Practice,
        metronome: &Metronome,
        capture: &CaptureState,
    ) -> anyhow::Result<Option<project::SessionExternal>> {
        let manifest = project::read_manifest(dir)?;
        let new_session = manifest.into_session(dir)?;
        let external = manifest.load_external(dir);

        if self.recording_id.is_some() {
            self.abort_capture(capture);
        }
        let old_ids: Vec<TrackId> = self.session.tracks().iter().map(|t| t.id).collect();
        for id in old_ids {
            let _ = engine.remove_track(id);
        }
        self.decodes.clear();
        self.capture_result = None;
        self.live_peaks = None;
        self.row_zoom = RowZoom::Normal;
        self.view_secs = None;
        self.record_zoom = None;

        manifest.rig.apply(params);

        // External IR: restore (or clear) on both chains.
        params.cab_external_loaded.store(false, Relaxed);
        params.cab_external_active.store(false, Relaxed);
        let mut ir_error = None;
        if let Some(rel) = &manifest.external_ir {
            match project::resolve_asset(dir, rel)
                .and_then(|p| load_ir(&p, self.sample_rate, LIVE_MAX_IR_LEN))
            {
                Ok(loaded) => {
                    let live = Box::new(ExternalIrCab::new(self.sample_rate, loaded.duplicate()));
                    let take = Box::new(ExternalIrCab::new(self.sample_rate, loaded));
                    let _ = engine.set_external_cab(Some(live));
                    let _ = engine.set_external_cab_take(Some(take));
                    params.cab_external_loaded.store(true, Relaxed);
                    params
                        .cab_external_active
                        .store(manifest.external_ir_active, Relaxed);
                }
                Err(e) => ir_error = Some(format!("IR not restored: {e}")),
            }
        }

        self.session = new_session;
        self.rate_adopted = true;
        self.reselect_after_removal();

        let pending: Vec<(TrackId, TrackKind, PathBuf)> = self
            .session
            .tracks()
            .iter()
            .filter_map(|t| t.asset.as_ref().map(|a| (t.id, t.kind, a.path.clone())))
            .collect();
        for (id, kind, path) in pending {
            self.start_decode(id, kind, path);
        }

        practice.reset();
        let playhead = self
            .session
            .ticks_to_frames(manifest.transport.playhead, self.sample_rate);
        practice.position.store(playhead as u64, Relaxed);
        practice.request_seek(playhead);
        practice.loop_start.store(
            self.session
                .ticks_to_frames(manifest.transport.loop_start, self.sample_rate)
                as u64,
            Relaxed,
        );
        practice.loop_end.store(
            self.session
                .ticks_to_frames(manifest.transport.loop_end, self.sample_rate) as u64,
            Relaxed,
        );
        practice
            .loop_enabled
            .store(manifest.transport.loop_enabled, Relaxed);
        metronome.set_bpm(manifest.metronome.bpm);
        metronome.active.store(manifest.metronome.enabled, Relaxed);

        self.note(match ir_error {
            Some(note) => format!("Loaded {} · {note}", self.session.name()),
            None => format!("Loaded {}", self.session.name()),
        });
        Ok(Some(external))
    }

    /// True when a session track already plays this recovery WAV, so it must not
    /// be restored twice or discarded out from under the track that uses it.
    pub(super) fn references_recovery(&self, wav: &Path) -> bool {
        self.session
            .tracks()
            .iter()
            .any(|t| t.asset.as_ref().is_some_and(|a| a.path == wav))
    }

    /// Recovery WAVs the running session already owns -- live takes plus takes
    /// restored from a crash. The session browser hides these so they are not
    /// offered back as if they were still crash leftovers.
    pub(super) fn recovery_in_use(&self) -> std::collections::HashSet<PathBuf> {
        self.session
            .tracks()
            .iter()
            .filter_map(|t| t.asset.as_ref())
            .map(|a| a.path.clone())
            .filter(|p| project::is_recovery_asset(p))
            .collect()
    }

    /// Bring an abandoned recovery take into the current session as a new raw-take
    /// row, decoding it off-thread. It is not deleted until a session save
    /// incorporates it (verified incorporation).
    pub(super) fn restore_recovery(&mut self, take: &project::RecoveryTake) {
        if self.references_recovery(&take.wav) {
            self.note("That take is already in the session".to_owned());
            return;
        }
        let ready = self
            .session
            .tracks()
            .iter()
            .filter(|t| t.is_ready())
            .count();
        if ready >= MAX_TRACKS {
            self.note(format!("Timeline is full ({MAX_TRACKS} tracks)"));
            return;
        }
        let id = self.session.alloc_id();
        self.session.push(
            id,
            take.meta.name.clone(),
            TrackKind::RawTake,
            Some(AssetRef {
                path: take.wav.clone(),
                source_sample_rate: take.meta.source_sample_rate,
                source_channels: 1,
            }),
            take.meta.start_ticks,
            0,
            TrackLifecycle::Loading,
        );
        self.selection = Selection::Track(id);
        self.start_decode(id, TrackKind::RawTake, take.wav.clone());
        self.note(format!("Restoring {}", take.label()));
    }

    /// Build a frozen export job for the unmuted raw takes. Returns a
    /// human-readable error when there is nothing to render. `range_frames`, when
    /// set, is a loop-region `[start, end)` in engine frames, converted to project
    /// ticks here.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn build_export_job(
        &self,
        dest: PathBuf,
        params: &Params,
        ir_path: Option<PathBuf>,
        ir_active: bool,
        insert: Option<crate::export::BuildExternal>,
        amp: Option<crate::export::BuildExternal>,
        range_frames: Option<(usize, usize)>,
    ) -> Result<crate::export::ExportJob, String> {
        let clips: Vec<crate::export::ExportClip> = self
            .session
            .tracks()
            .iter()
            .filter(|t| t.kind == TrackKind::RawTake && !t.muted && t.is_ready())
            .filter_map(|t| {
                t.asset.as_ref().map(|a| crate::export::ExportClip {
                    path: a.path.clone(),
                    start_ticks: t.start_ticks,
                    gain: t.gain,
                })
            })
            .collect();
        if clips.is_empty() {
            return Err("No unmuted raw takes to export".to_owned());
        }
        let range_ticks = range_frames.map(|(start, end)| {
            (
                self.session.frames_to_ticks(start, self.sample_rate),
                self.session.frames_to_ticks(end, self.sample_rate),
            )
        });
        Ok(crate::export::ExportJob {
            dest,
            sample_rate: self.session.project_sample_rate(),
            project_sample_rate: self.session.project_sample_rate(),
            clips,
            rig: crate::export::snapshot_rig(params),
            range_ticks,
            ir_path,
            ir_active,
            insert,
            amp,
        })
    }

    /// The live loop region as an `(start, end)` frame pair, or `None` when it is
    /// empty (out-point not past the in-point).
    pub(super) fn loop_region_frames(&self, practice: &Practice) -> Option<(usize, usize)> {
        let a = practice.loop_start.load(Relaxed) as usize;
        let b = practice.loop_end.load(Relaxed) as usize;
        (b > a).then_some((a, b))
    }

    /// One-line description of the `E` dialog's range control. `loop_region` is
    /// the selected mode; the times come from the live loop markers.
    pub(super) fn export_range_label(&self, practice: &Practice, loop_region: bool) -> String {
        if !loop_region {
            return "Range: full session   [Tab] loop region".to_owned();
        }
        match self.loop_region_frames(practice) {
            Some((a, b)) => format!(
                "Range: loop region {} – {}   [Tab] full session",
                mmss_precise(a, self.sample_rate),
                mmss_precise(b, self.sample_rate),
            ),
            None => {
                "Range: loop region (none set — use [ and ] first)   [Tab] full session".to_owned()
            }
        }
    }

    // ── Browser / gain modal input ──────────────────────────────────────────────

    /// Handle a key while the track browser is open. Returns `true` when the key
    /// added a backing track to the timeline, so the caller can bring the timeline
    /// panel forward.
    pub(super) fn handle_browser_key(&mut self, code: KeyCode, practice: &Practice) -> bool {
        if self.library_open {
            self.handle_library_key(code);
            return false;
        }
        if self.searching {
            match code {
                KeyCode::Esc => {
                    self.browser_filter.clear();
                    self.searching = false;
                }
                KeyCode::Backspace => {
                    self.browser_filter.pop();
                    self.browser_cursor = 0;
                }
                KeyCode::Char(c) => {
                    self.browser_filter.push(c);
                    self.browser_cursor = 0;
                }
                KeyCode::Up => self.browser_cursor = self.browser_cursor.saturating_sub(1),
                KeyCode::Down => {
                    let n = self.visible_file_indices().len();
                    self.browser_cursor = (self.browser_cursor + 1).min(n.saturating_sub(1));
                }
                KeyCode::Enter => return self.import_selected(practice),
                _ => {}
            }
            return false;
        }
        match code {
            KeyCode::Up if self.field == 0 => {
                self.browser_cursor = self.browser_cursor.saturating_sub(1);
            }
            KeyCode::Down if self.field == 0 => {
                let n = self.visible_file_indices().len();
                self.browser_cursor = (self.browser_cursor + 1).min(n.saturating_sub(1));
            }
            KeyCode::Tab => self.field = 1 - self.field,
            // `/` starts a fresh filter over the file list.
            KeyCode::Char('/') if self.field == 0 => {
                self.browser_filter.clear();
                self.browser_cursor = 0;
                self.searching = true;
            }
            KeyCode::Char('l') | KeyCode::Char('L') if self.field == 0 => self.open_library(),
            KeyCode::Enter => {
                if self.field == 1 {
                    let p = self.path_input.trim().to_owned();
                    if p.is_empty() {
                        self.note("Type a file path first".to_owned());
                        return false;
                    }
                    self.import_at_playhead(PathBuf::from(p), practice);
                    self.browser_open = false;
                    return true;
                }
                return self.import_selected(practice);
            }
            KeyCode::Backspace if self.field == 1 => {
                self.path_input.pop();
            }
            KeyCode::Char(c) if self.field == 1 => {
                self.path_input.push(c);
            }
            KeyCode::Esc | KeyCode::Char('b') | KeyCode::Char('B') => {
                self.browser_open = false;
            }
            _ => {}
        }
        false
    }

    /// Indices into `files` visible under the active filter, in list order.
    fn visible_file_indices(&self) -> Vec<usize> {
        if self.browser_filter.is_empty() {
            return (0..self.files.len()).collect();
        }
        let needle = self.browser_filter.to_lowercase();
        self.files
            .iter()
            .enumerate()
            .filter(|(_, f)| {
                f.label.to_lowercase().contains(&needle)
                    || f.detail.to_lowercase().contains(&needle)
                    || f.path.to_string_lossy().to_lowercase().contains(&needle)
            })
            .map(|(i, _)| i)
            .collect()
    }

    /// Import the highlighted file, honoring the active filter. Returns `true`
    /// when a track was added.
    fn import_selected(&mut self, practice: &Practice) -> bool {
        let path = self
            .visible_file_indices()
            .get(self.browser_cursor)
            .map(|&i| self.files[i].path.clone());
        if let Some(path) = path {
            self.import_at_playhead(path, practice);
            self.browser_open = false;
            self.searching = false;
            return true;
        }
        false
    }

    /// Library settings sub-view: edit the import path and its flags.
    fn handle_library_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Esc => self.library_open = false,
            KeyCode::Tab | KeyCode::Down => self.library_field = (self.library_field + 1) % 3,
            KeyCode::BackTab | KeyCode::Up => self.library_field = (self.library_field + 2) % 3,
            KeyCode::Char(' ') if self.library_field == 0 => self.library_path.push(' '),
            KeyCode::Char(' ') => match self.library_field {
                1 => self.library_only = !self.library_only,
                2 => self.library_subpaths = !self.library_subpaths,
                _ => {}
            },
            KeyCode::Backspace if self.library_field == 0 => {
                self.library_path.pop();
            }
            KeyCode::Char(c) if self.library_field == 0 => self.library_path.push(c),
            KeyCode::Enter => self.save_library(),
            _ => {}
        }
    }

    pub(super) fn open_gain_edit(&mut self) {
        if let Some(id) = self.selected_track() {
            self.gain_edit = Some(id);
        }
    }

    pub(super) fn handle_gain_key(&mut self, code: KeyCode, engine: &mut AudioEngine) {
        let Some(id) = self.gain_edit else {
            return;
        };
        match code {
            KeyCode::Left | KeyCode::Down | KeyCode::Char('-') => {
                self.nudge_gain(engine, id, -0.05)
            }
            KeyCode::Right | KeyCode::Up | KeyCode::Char('+') | KeyCode::Char('=') => {
                self.nudge_gain(engine, id, 0.05);
            }
            KeyCode::Char('r') | KeyCode::Char('R') => self.set_gain(engine, id, 1.0),
            KeyCode::Enter | KeyCode::Esc => self.gain_edit = None,
            _ => {}
        }
    }

    fn nudge_gain(&mut self, engine: &mut AudioEngine, id: TrackId, delta: f32) {
        let current = self.session.track(id).map_or(1.0, |t| t.gain);
        self.set_gain(engine, id, (current + delta).clamp(0.0, 2.0));
    }

    fn set_gain(&mut self, engine: &mut AudioEngine, id: TrackId, value: f32) {
        if let Some(track) = self.session.track_mut(id) {
            track.gain = value;
        }
        let _ = engine.set_track_gain(id, value);
    }

    pub(super) fn gain_open(&self) -> bool {
        self.gain_edit.is_some()
    }

    // ── Clip move ───────────────────────────────────────────────────────────────

    pub(super) fn open_move_edit(&mut self) {
        if let Some(id) = self.selected_track() {
            self.move_edit = Some(id);
        }
    }

    pub(super) fn move_open(&self) -> bool {
        self.move_edit.is_some()
    }

    /// Arrows nudge the clip by the current step; `+`/`-` cycle that step from
    /// inside the modal; `R` returns the clip to the top.
    pub(super) fn handle_move_key(&mut self, code: KeyCode, engine: &mut AudioEngine) {
        let Some(id) = self.move_edit else {
            return;
        };
        let step = self.session.seek_seconds();
        match code {
            KeyCode::Left | KeyCode::Down => self.nudge_start(engine, id, -step),
            KeyCode::Right | KeyCode::Up => self.nudge_start(engine, id, step),
            KeyCode::Char('+') | KeyCode::Char('=') => self.cycle_seek_step(1),
            KeyCode::Char('-') => self.cycle_seek_step(-1),
            KeyCode::Char('r') | KeyCode::Char('R') => self.set_start_ticks(engine, id, 0),
            KeyCode::Enter | KeyCode::Esc => self.move_edit = None,
            _ => {}
        }
    }

    fn nudge_start(&mut self, engine: &mut AudioEngine, id: TrackId, seconds: f32) {
        let cur = self.session.track(id).map_or(0, |t| t.start_ticks);
        let delta = (f64::from(seconds) * f64::from(self.session.project_sample_rate())).round();
        let next = (cur as f64 + delta).max(0.0) as u64;
        self.set_start_ticks(engine, id, next);
    }

    fn set_start_ticks(&mut self, engine: &mut AudioEngine, id: TrackId, ticks: u64) {
        if let Some(track) = self.session.track_mut(id) {
            track.start_ticks = ticks;
        }
        let frames = self.session.ticks_to_frames(ticks, self.sample_rate);
        let _ = engine.set_track_start(id, frames);
    }

    // ── Rendering ───────────────────────────────────────────────────────────────

    /// Render the timeline pane. `focused` is true while the pane owns focus;
    /// `recording` drives the transport REC lamp.
    pub(super) fn render(
        &self,
        f: &mut Frame,
        area: Rect,
        practice: &Practice,
        focused: bool,
        blink: bool,
        recording: bool,
    ) {
        if area.height < 2 || area.width < 4 {
            return;
        }
        let focused_glyph = if focused { ACCENT } else { shade(ACCENT, 0.55) };
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Plain)
            .border_style(border_style(focused))
            .title(Line::from(Span::styled(
                " T I M E L I N E ",
                Style::default()
                    .fg(focused_glyph)
                    .add_modifier(Modifier::BOLD),
            )))
            .style(panel_style());
        let inner = block.inner(area);
        f.render_widget(block, area);

        // The ruler needs a spare line; on a very short pane fall back to the
        // old transport + rows + hint layout.
        let show_ruler = inner.height >= 4;
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints(if show_ruler {
                vec![
                    Constraint::Length(1), // transport
                    Constraint::Length(1), // time ruler
                    Constraint::Min(1),    // track list
                    Constraint::Length(1), // hint/message
                ]
            } else {
                vec![
                    Constraint::Length(1), // transport
                    Constraint::Min(1),    // track list
                    Constraint::Length(1), // hint/message
                ]
            })
            .split(inner);
        let (tracks_row, hint_row) = if show_ruler {
            (rows[2], rows[3])
        } else {
            (rows[1], rows[2])
        };

        self.render_transport(f, rows[0], practice, blink, recording);
        if show_ruler {
            self.render_ruler(f, rows[1], practice);
        }
        self.render_tracks(f, tracks_row, practice, focused);
        self.render_hint(f, hint_row, focused);
    }

    /// The time axis under the transport: `m:ss` tick labels at a spacing that
    /// never collides, plus the playhead marker, aligned to the waveform gutter.
    fn render_ruler(&self, f: &mut Frame, area: Rect, practice: &Practice) {
        let width = area.width as usize;
        let wave_w = width.saturating_sub(GUTTER);
        if wave_w == 0 {
            return;
        }
        let (view_start, view_len) = self.view(practice);
        let secs = view_len as f64 / f64::from(self.sample_rate.max(1.0));
        // Coarsest tick whose labels do not collide at this window width.
        let tick = RULER_TICKS
            .iter()
            .copied()
            .find(|&t| {
                secs > 0.0
                    && (t / secs) * wave_w as f64
                        >= (ruler_label(view_start, self.sample_rate, t).len() as f64 + 1.0)
                            .max(8.0)
            })
            .unwrap_or(*RULER_TICKS.last().unwrap_or(&1.0));

        let mut cells: Vec<(char, bool)> = vec![(' ', false); wave_w];
        let mut last_end = 0usize;
        let mut t = 0.0f64;
        loop {
            let col = if secs > 0.0 {
                ((t / secs) * wave_w.saturating_sub(1) as f64).round() as usize
            } else {
                0
            };
            if col < wave_w {
                let clock = view_start + (t * f64::from(self.sample_rate)) as usize;
                let label = ruler_label(clock, self.sample_rate, tick);
                let w = label.chars().count();
                // Right-align a label that would spill past the edge, but never
                // overlap the previous one.
                let start = col.min(wave_w.saturating_sub(w));
                if start >= last_end {
                    for (k, ch) in label.chars().enumerate() {
                        if start + k < wave_w {
                            cells[start + k] = (ch, false);
                        }
                    }
                    last_end = (start + w).min(wave_w);
                }
            }
            let next = t + tick;
            if next <= t || next > secs {
                break;
            }
            t = next;
        }

        // Minor ticks between the labelled ranges, as dim low ticks (U+02CC).
        // Never overwrite a label, and skip when they would crowd.
        let minor = tick / 5.0;
        if secs > 0.0 && minor > 0.0 && (minor / secs) * wave_w as f64 >= 2.0 {
            let mut t = minor;
            while t < secs {
                let col = ((t / secs) * wave_w.saturating_sub(1) as f64).round() as usize;
                if col < wave_w && cells[col].0 == ' ' {
                    cells[col] = ('ˌ', false);
                }
                t += minor;
            }
        }

        let pos = practice.position();
        if pos >= view_start && pos < view_start + view_len {
            let pos_col = (pos - view_start) * wave_w.saturating_sub(1) / view_len.max(1);
            if pos_col < wave_w {
                cells[pos_col] = ('▼', true);
            }
        }

        let mut spans = vec![
            Span::raw(" ".repeat(GUTTER - 1)),
            Span::styled("│", divider_style()),
        ];
        for (ch, playhead) in cells {
            let style = if playhead {
                Style::default().fg(HOT).add_modifier(Modifier::BOLD)
            } else if ch == ' ' {
                Style::default()
            } else if ch == 'ˌ' {
                Style::default().fg(DIM)
            } else {
                Style::default().fg(GRID)
            };
            spans.push(Span::styled(ch.to_string(), style));
        }
        f.render_widget(Paragraph::new(Line::from(spans)), area);
    }

    fn render_transport(
        &self,
        f: &mut Frame,
        area: Rect,
        practice: &Practice,
        blink: bool,
        recording: bool,
    ) {
        let playing = practice.playing.load(Relaxed);
        let total = self.span_frames(practice);
        let position = practice.position().min(total);
        let state = if playing {
            Span::styled("▶ ", Style::default().fg(SAFE).add_modifier(Modifier::BOLD))
        } else {
            Span::styled("⏸ ", Style::default().fg(DIM))
        };
        let loop_on = practice.loop_enabled.load(Relaxed);
        let mut transport = vec![
            // Align the readout with the waveform/ruler gutter.
            Span::styled(format!("{:>width$}", "", width = GUTTER), Style::default()),
            state,
            if recording && blink {
                Span::styled(
                    "●REC ",
                    Style::default().fg(HOT).add_modifier(Modifier::BOLD),
                )
            } else if recording {
                Span::styled("○REC ", Style::default().fg(HOT))
            } else {
                Span::raw("     ")
            },
            Span::styled(
                format!(
                    "{} / {}  ",
                    mmss_precise(position, self.sample_rate),
                    mmss(total, self.sample_rate)
                ),
                Style::default().fg(CHROME),
            ),
            Span::styled(
                format!("{}s ", format_step(self.session.seek_seconds())),
                Style::default().fg(AMBER),
            ),
            if let Some(secs) = self.view_secs {
                Span::styled(
                    format!("view {}s ", format_step(secs as f32)),
                    Style::default().fg(ACCENT),
                )
            } else {
                Span::raw("")
            },
            Span::styled(
                if loop_on { "LOOP " } else { "loop " },
                Style::default()
                    .fg(if loop_on { HOT } else { DIM })
                    .add_modifier(if loop_on {
                        Modifier::BOLD
                    } else {
                        Modifier::empty()
                    }),
            ),
        ];
        if loop_on {
            let a = practice.loop_start.load(Relaxed) as usize;
            let b = practice.loop_end.load(Relaxed) as usize;
            transport.push(Span::styled(
                format!(
                    "[{} ▸ {}]  ",
                    mmss(a, self.sample_rate),
                    mmss(b, self.sample_rate)
                ),
                Style::default().fg(HOT),
            ));
        }
        if let Some(msg) = &self.message {
            transport.push(Span::styled(msg.text.clone(), Style::default().fg(WARN)));
        }
        f.render_widget(Paragraph::new(Line::from(transport)), area);
    }

    fn render_tracks(&self, f: &mut Frame, area: Rect, practice: &Practice, focused: bool) {
        let tracks = self.session.tracks();
        if tracks.is_empty() {
            // Even with nothing to draw, keep the gutter divider and the
            // playhead line spanning the pane.
            let width = area.width as usize;
            let (view_start, view_len) = self.view(practice);
            let position = practice.position();
            let wave_w = width.saturating_sub(GUTTER);
            if width > GUTTER {
                let pos_col =
                    (wave_w > 0 && position >= view_start && position < view_start + view_len)
                        .then(|| {
                            ((position - view_start) * wave_w.saturating_sub(1) / view_len.max(1))
                                .min(wave_w - 1)
                        });
                let line_style = Style::default().fg(HOT).add_modifier(Modifier::BOLD);
                let lines: Vec<Line> = (0..area.height)
                    .map(|_| {
                        let mut spans = vec![
                            Span::raw(" ".repeat(GUTTER - 1)),
                            Span::styled("│", divider_style()),
                        ];
                        match pos_col {
                            Some(pc) => {
                                spans.push(Span::raw(" ".repeat(pc)));
                                spans.push(Span::styled("│", line_style));
                                spans.push(Span::raw(
                                    " ".repeat(width.saturating_sub(GUTTER + pc + 1)),
                                ));
                            }
                            None => spans.push(Span::raw(" ".repeat(wave_w))),
                        }
                        Line::from(spans)
                    })
                    .collect();
                f.render_widget(Paragraph::new(lines), area);
            }
            // Center the empty-state hint in the pane.
            if area.height > 0 {
                let hint = Line::from(vec![
                    Span::styled("no tracks — press ", Style::default().fg(DIM)),
                    Span::styled("B", Style::default().fg(AMBER)),
                    Span::styled(" to import, ", Style::default().fg(DIM)),
                    Span::styled("R", Style::default().fg(AMBER)),
                    Span::styled(" to record a raw take", Style::default().fg(DIM)),
                ]);
                let mid = Rect {
                    x: area.x,
                    y: area.y + area.height / 2,
                    width: area.width,
                    height: 1,
                };
                f.render_widget(Paragraph::new(hint).alignment(Alignment::Center), mid);
            }
            return;
        }
        let selected_idx = match self.selection {
            Selection::Transport => None,
            Selection::Track(id) => self.session.index_of(id),
        };
        let (view_start, view_len) = self.view(practice);
        let position = practice.position();
        let width = area.width as usize;
        let h = area.height as usize;

        // Resolve the zoom state into a top-to-bottom list of (track index,
        // row height). Zoomed rows hide the others so they can use the pane.
        let rows: Vec<(usize, usize)> = match self.row_zoom {
            RowZoom::Expanded(id) if self.session.index_of(id).is_some() => {
                vec![(self.session.index_of(id).unwrap_or(0), h)]
            }
            RowZoom::Split(a, b)
                if self.session.index_of(a).is_some() && self.session.index_of(b).is_some() =>
            {
                vec![
                    (self.session.index_of(a).unwrap_or(0), h / 2),
                    (self.session.index_of(b).unwrap_or(0), h - h / 2),
                ]
            }
            _ => {
                let visible = (h / ROW_HEIGHT).max(1);
                let offset = selected_idx.map_or(0, |i| i.saturating_sub(visible - 1));
                tracks
                    .iter()
                    .enumerate()
                    .skip(offset)
                    .take(visible)
                    .map(|(i, _)| (i, ROW_HEIGHT))
                    .collect()
            }
        };

        let live_guard = self.live_peaks.as_ref().and_then(|m| m.try_lock().ok());
        let mut y = area.y;
        for (i, row_h) in rows {
            let row_h = row_h.min(area.bottom().saturating_sub(y) as usize);
            if row_h == 0 {
                break;
            }
            let track = &tracks[i];
            let live = match (self.recording_id, live_guard.as_deref()) {
                (Some(rid), Some(lp)) if rid == track.id => Some(lp),
                _ => None,
            };
            let focused_row = focused && selected_idx == Some(i);
            let rect = Rect {
                x: area.x,
                y,
                width: area.width,
                height: row_h as u16,
            };
            let lines = self.track_lines(
                track,
                focused_row,
                practice,
                position,
                view_start,
                view_len,
                width,
                live,
                row_h,
            );
            f.render_widget(Paragraph::new(lines), rect);
            y += row_h as u16;
        }
    }

    /// Build the `height` lines for one track row: a fixed-width header in the
    /// gutter on the first line and a symmetric min/max envelope across every
    /// line, so taller rows reveal more of the waveform.
    #[allow(clippy::too_many_arguments)]
    fn track_lines<'a>(
        &self,
        track: &crate::session::Track,
        focused: bool,
        practice: &Practice,
        position: usize,
        view_start: usize,
        view_len: usize,
        width: usize,
        live: Option<&LivePeaks>,
        height: usize,
    ) -> Vec<Line<'a>> {
        let start = self
            .session
            .ticks_to_frames(track.start_ticks, self.sample_rate);
        let length = self
            .session
            .ticks_to_frames(track.length_ticks, self.sample_rate);
        let (peaks, frames, loaded): (&[(f32, f32)], usize, bool) = match live {
            Some(lp) => {
                let n = lp.frames();
                (lp.peaks(), n, n > 0)
            }
            None => (
                track.peaks.as_slice(),
                length,
                track.is_ready() && length > 0,
            ),
        };
        let recording = matches!(track.lifecycle, TrackLifecycle::Recording);
        let header = self.row_header(track, focused, loaded || recording);
        let blank = Line::from(vec![
            Span::raw(" ".repeat(GUTTER - 1)),
            Span::styled("│", divider_style()),
        ]);
        let wave_w = width.saturating_sub(GUTTER);
        let height = height.max(1);

        let empty = !loaded || wave_w == 0 || view_len == 0 || peaks.is_empty();
        if empty {
            let mut first = header;
            if let TrackLifecycle::Error(e) = &track.lifecycle {
                first.push(Span::styled(
                    format!(" {}", truncate(e, wave_w.max(4))),
                    Style::default().fg(WARN),
                ));
            } else if recording {
                first.push(Span::styled(" recording…", Style::default().fg(HOT)));
            }
            let mut out = vec![Line::from(first)];
            for _ in 1..height {
                out.push(blank.clone());
            }
            return out;
        }

        let loop_on = practice.loop_enabled.load(Relaxed);
        let (la, lb) = (
            practice.loop_start.load(Relaxed) as usize,
            practice.loop_end.load(Relaxed) as usize,
        );
        let pos_col = (position >= view_start && position < view_start + view_len)
            .then(|| (position - view_start) * wave_w.saturating_sub(1) / view_len.max(1));
        let mid = height / 2;
        let glyphs = self.wave_glyphs;
        let cols = glyphs.cols();
        let rows = glyphs.rows();
        // Vertical centre, in sub-cell rows.
        let center = (height * rows) as f32 / 2.0;
        let sub_cols = cols * wave_w;
        // Amplitude mapping: auto-fit each track, or show the true scale.
        let gain = match self.wave_gain {
            WaveGain::Normalized => envelope_gain(peaks),
            WaveGain::Absolute => 1.0,
        };

        // Min/max envelope for one sub-column, in sub-cell rows around the
        // centre. `None` when the sub-column falls outside the clip.
        let sub_env = |sc: usize| -> Option<(f32, f32)> {
            let f0 = view_start + sc * view_len / sub_cols.max(1);
            let f1 = (view_start + ((sc + 1) * view_len) / sub_cols.max(1)).max(f0 + 1);
            let a = f0.max(start);
            let b = f1.min(start.saturating_add(frames));
            if a >= b {
                return None;
            }
            let b0 = (a - start) * peaks.len() / frames.max(1);
            let b1 = (((b - start) * peaks.len()) / frames.max(1))
                .max(b0 + 1)
                .min(peaks.len());
            let (mut lo, mut hi) = (0.0f32, 0.0f32);
            for &(l, h) in &peaks[b0..b1] {
                lo = lo.min(l);
                hi = hi.max(h);
            }
            let top = center - (hi * gain).clamp(-1.0, 1.0) * center;
            let bottom = center - (lo * gain).clamp(-1.0, 1.0) * center;
            Some((top.max(0.0), bottom.min((height * rows) as f32)))
        };

        let mut out: Vec<Line<'a>> = Vec::with_capacity(height);
        for line in 0..height {
            let mut spans: Vec<Span<'a>> = if line == 0 {
                header.clone()
            } else {
                vec![
                    Span::raw(" ".repeat(GUTTER - 1)),
                    Span::styled("│", divider_style()),
                ]
            };
            let mut buf = String::new();
            let mut run_style: Option<Style> = None;
            for col in 0..wave_w {
                let f0 = view_start + col * view_len / wave_w.max(1);
                let in_loop = loop_on && lb > la && f0 >= la && f0 < lb;
                let mut env = [None; 2];
                for (d, slot) in env.iter_mut().enumerate().take(cols) {
                    *slot = sub_env(col * cols + d);
                }
                let inside = env[..cols].iter().any(|e| e.is_some());
                let (ch, style) = if Some(col) == pos_col {
                    ('│', Style::default().fg(HOT).add_modifier(Modifier::BOLD))
                } else if !inside {
                    if in_loop {
                        ('·', Style::default().fg(HOT))
                    } else {
                        (' ', Style::default().fg(CHROME))
                    }
                } else {
                    let mut cells = 0u32;
                    for (d, band) in env[..cols].iter().enumerate() {
                        let Some((top, bottom)) = *band else {
                            continue;
                        };
                        for r in 0..rows {
                            let k = (line * rows + r) as f32;
                            if k >= top && k < bottom {
                                cells |= 1 << (r * cols + d);
                            }
                        }
                    }
                    if cells != 0 {
                        (
                            glyphs.glyph(cells),
                            Style::default().fg(if in_loop { HOT } else { CHROME }),
                        )
                    } else if line == mid {
                        ('·', Style::default().fg(DIM))
                    } else {
                        (' ', Style::default().fg(CHROME))
                    }
                };
                if run_style != Some(style) {
                    if !buf.is_empty() {
                        spans.push(Span::styled(
                            std::mem::take(&mut buf),
                            run_style.unwrap_or_default(),
                        ));
                    }
                    run_style = Some(style);
                }
                buf.push(ch);
            }
            if !buf.is_empty() {
                spans.push(Span::styled(buf, run_style.unwrap_or_default()));
            }
            out.push(Line::from(spans));
        }
        out
    }

    /// Fixed-width left gutter (see [`GUTTER`]): cursor, LED, tag, `uncal`
    /// flag, name, gain. Kept a constant width so every row's waveform — and the
    /// ruler — share one column grid.
    fn row_header<'a>(
        &self,
        track: &crate::session::Track,
        focused: bool,
        active: bool,
    ) -> Vec<Span<'a>> {
        let (tag, tag_color) = match track.kind {
            TrackKind::Import => ("IMP ", CHROME),
            TrackKind::RawTake => ("TAKE", CHROME),
        };
        // A raw take captured before calibration carries no input trim.
        let uncal = matches!(track.kind, TrackKind::RawTake) && track.input_trim_db.is_none();
        let recording = matches!(track.lifecycle, TrackLifecycle::Recording);
        let led = if track.muted {
            Span::styled("○ ", Style::default().fg(DIM))
        } else if recording {
            Span::styled("◉ ", Style::default().fg(HOT))
        } else if active {
            Span::styled("● ", Style::default().fg(SAFE))
        } else {
            Span::styled("· ", Style::default().fg(DIM))
        };
        let name_w = 13usize;
        let gain = format!("{:>3}%", (track.gain * 100.0).round() as i32);
        vec![
            Span::styled(
                if focused { "▌" } else { " " }.to_owned(),
                Style::default().fg(ACCENT),
            ),
            led,
            Span::styled(tag.to_owned(), Style::default().fg(tag_color)),
            Span::styled(
                if uncal { "!" } else { " " }.to_owned(),
                Style::default().fg(AMBER),
            ),
            Span::raw(" "),
            Span::styled(
                format!("{:<name_w$}", truncate(&track.name, name_w)),
                Style::default().fg(if active { CHROME } else { DIM }),
            ),
            Span::styled(gain, Style::default().fg(AMBER)),
            Span::styled("│", divider_style()),
        ]
    }

    fn render_hint(&self, f: &mut Frame, area: Rect, focused: bool) {
        let hint = if focused {
            Line::from(vec![
                Span::styled("Space", Style::default().fg(AMBER)),
                Span::styled(" play/pause  ", Style::default().fg(DIM)),
                Span::styled("M", Style::default().fg(AMBER)),
                Span::styled(" mute  ", Style::default().fg(DIM)),
                Span::styled("Enter", Style::default().fg(AMBER)),
                Span::styled(" start  ", Style::default().fg(DIM)),
                Span::styled("←/→", Style::default().fg(AMBER)),
                Span::styled(" seek  ", Style::default().fg(DIM)),
                Span::styled("+/-", Style::default().fg(AMBER)),
                Span::styled(" step  ", Style::default().fg(DIM)),
                Span::styled("G", Style::default().fg(AMBER)),
                Span::styled(" gain  ", Style::default().fg(DIM)),
                Span::styled("H", Style::default().fg(AMBER)),
                Span::styled(" move  ", Style::default().fg(DIM)),
                Span::styled("z", Style::default().fg(AMBER)),
                Span::styled(" zoom  ", Style::default().fg(DIM)),
                Span::styled("[ ] L", Style::default().fg(AMBER)),
                Span::styled(" loop  ", Style::default().fg(DIM)),
                Span::styled("Del", Style::default().fg(AMBER)),
                Span::styled(" remove  ", Style::default().fg(DIM)),
                Span::styled("R", Style::default().fg(AMBER)),
                Span::styled(" rec", Style::default().fg(DIM)),
            ])
        } else {
            Line::from(vec![
                Span::styled("3", Style::default().fg(AMBER)),
                Span::styled(" focus the timeline  ·  ", Style::default().fg(DIM)),
                Span::styled("B", Style::default().fg(AMBER)),
                Span::styled(" import  ·  ", Style::default().fg(DIM)),
                Span::styled("R", Style::default().fg(AMBER)),
                Span::styled(" record", Style::default().fg(DIM)),
            ])
        };
        f.render_widget(Paragraph::new(hint).alignment(Alignment::Left), area);
    }

    /// Render the import browser modal.
    pub(super) fn render_browser(&self, f: &mut Frame) {
        let area = centered_rect(62, f.area());
        f.render_widget(Clear, area);
        let title = if self.library_open {
            " I M P O R T   L I B R A R Y ".to_owned()
        } else if self.browser_filter.is_empty() {
            " I M P O R T   T R A C K ".to_owned()
        } else {
            format!(
                " I M P O R T   T R A C K   filter: {} ",
                self.browser_filter
            )
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Double)
            .border_style(Style::default().fg(ACCENT))
            .title(Span::styled(
                title,
                Style::default().fg(AMBER).add_modifier(Modifier::BOLD),
            ))
            .style(panel_style());
        let inner = block.inner(area);
        f.render_widget(block, area);

        if self.library_open {
            self.render_library(f, inner);
            return;
        }

        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1), // intro / filter
                Constraint::Length(1), // path field
                Constraint::Min(1),    // file list
                Constraint::Length(1), // hint
                Constraint::Length(1), // message
            ])
            .split(inner);

        if self.searching {
            f.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled("filter ", Style::default().fg(DIM)),
                    Span::styled(self.browser_filter.clone(), Style::default().fg(CHROME)),
                    Span::styled("▌", Style::default().fg(ACCENT)),
                ])),
                rows[0],
            );
        } else {
            f.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled("Enter", Style::default().fg(AMBER)),
                    Span::styled(
                        " adds the file as a new track at the playhead",
                        Style::default().fg(DIM),
                    ),
                ])),
                rows[0],
            );
        }

        let path_focus = if self.field == 1 { "▌" } else { " " };
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("path ", Style::default().fg(DIM)),
                Span::styled(
                    if self.path_input.is_empty() {
                        "(type a file path…)".to_owned()
                    } else {
                        self.path_input.clone()
                    },
                    Style::default().fg(if self.path_input.is_empty() {
                        DIM
                    } else {
                        CHROME
                    }),
                ),
                Span::styled(path_focus.to_owned(), Style::default().fg(ACCENT)),
            ])),
            rows[1],
        );

        let visible_idx = self.visible_file_indices();
        let mut lines: Vec<Line> = Vec::with_capacity(visible_idx.len());
        for (pos, &i) in visible_idx.iter().enumerate() {
            let selected = pos == self.browser_cursor && self.field == 0 && !self.searching;
            lines.push(file_entry(&self.files[i], selected));
        }
        if visible_idx.is_empty() {
            let text = if self.browser_filter.is_empty() {
                "  (no audio files found — L sets the library, or type a path above)"
            } else {
                "  (no matching files)"
            };
            lines.push(Line::from(Span::styled(text, Style::default().fg(DIM))));
        }
        let visible = rows[2].height as usize;
        let offset = self
            .browser_cursor
            .saturating_sub(visible.saturating_sub(1));
        f.render_widget(
            Paragraph::new(lines.into_iter().skip(offset).collect::<Vec<_>>()),
            rows[2],
        );

        let footer = if self.searching {
            vec![
                Span::styled("type", Style::default().fg(AMBER)),
                Span::styled(" filter  ", Style::default().fg(DIM)),
                Span::styled("Backspace", Style::default().fg(AMBER)),
                Span::styled(" edit  ", Style::default().fg(DIM)),
                Span::styled("Esc", Style::default().fg(AMBER)),
                Span::styled(" clear", Style::default().fg(DIM)),
            ]
        } else {
            vec![
                Span::styled("↑/↓", Style::default().fg(AMBER)),
                Span::styled(" files  ", Style::default().fg(DIM)),
                Span::styled("/", Style::default().fg(AMBER)),
                Span::styled(" search  ", Style::default().fg(DIM)),
                Span::styled("Tab", Style::default().fg(AMBER)),
                Span::styled(" path  ", Style::default().fg(DIM)),
                Span::styled("L", Style::default().fg(AMBER)),
                Span::styled(" library  ", Style::default().fg(DIM)),
                Span::styled("Esc / B", Style::default().fg(AMBER)),
                Span::styled(" close", Style::default().fg(DIM)),
            ]
        };
        f.render_widget(
            Paragraph::new(Line::from(footer)).alignment(Alignment::Center),
            rows[3],
        );

        let msg = self
            .message
            .as_ref()
            .map(|n| n.text.clone())
            .unwrap_or_default();
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(msg, Style::default().fg(WARN))))
                .alignment(Alignment::Center),
            rows[4],
        );
    }

    /// The Library settings sub-view: the import path and its two flags.
    fn render_library(&self, f: &mut Frame, area: Rect) {
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1), // path label
                Constraint::Length(2), // path input
                Constraint::Length(1), // only
                Constraint::Length(1), // subpaths
                Constraint::Min(1),    // spacer
                Constraint::Length(1), // hint
            ])
            .split(area);

        let field_style = |active: bool| {
            if active {
                Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(DIM)
            }
        };

        f.render_widget(
            Paragraph::new(Span::styled(
                "Library path (empty = the usual folders):",
                field_style(self.library_field == 0),
            )),
            rows[0],
        );
        f.render_widget(
            Paragraph::new(Span::styled(
                if self.library_path.is_empty() {
                    "(type a directory…)".to_owned()
                } else {
                    self.library_path.clone()
                },
                Style::default().fg(if self.library_path.is_empty() {
                    DIM
                } else {
                    CHROME
                }),
            ))
            .block(Block::default().borders(Borders::BOTTOM).border_style(
                if self.library_field == 0 {
                    Style::default().fg(ACCENT)
                } else {
                    Style::default().fg(DIM)
                },
            )),
            rows[1],
        );

        let mark = |on: bool| if on { "[x]" } else { "[ ]" };
        f.render_widget(
            Paragraph::new(Span::styled(
                format!("{} Only this path", mark(self.library_only)),
                field_style(self.library_field == 1),
            )),
            rows[2],
        );
        f.render_widget(
            Paragraph::new(Span::styled(
                format!("{} Include subpaths", mark(self.library_subpaths)),
                field_style(self.library_field == 2),
            )),
            rows[3],
        );

        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("↑/↓", Style::default().fg(AMBER)),
                Span::styled(" field  ", Style::default().fg(DIM)),
                Span::styled("Space", Style::default().fg(AMBER)),
                Span::styled(" toggle  ", Style::default().fg(DIM)),
                Span::styled("Enter", Style::default().fg(AMBER)),
                Span::styled(" save  ", Style::default().fg(DIM)),
                Span::styled("Esc", Style::default().fg(AMBER)),
                Span::styled(" cancel", Style::default().fg(DIM)),
            ]))
            .alignment(Alignment::Center),
            rows[5],
        );
    }

    /// Render the per-track gain modal.
    pub(super) fn render_gain_modal(&self, f: &mut Frame) {
        let Some(id) = self.gain_edit else {
            return;
        };
        let Some(track) = self.session.track(id) else {
            return;
        };
        let area = centered_box(46, 7, f.area());
        f.render_widget(Clear, area);
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Double)
            .border_style(Style::default().fg(ACCENT))
            .title(Span::styled(
                " T R A C K   G A I N ",
                Style::default().fg(AMBER).add_modifier(Modifier::BOLD),
            ))
            .style(panel_style());
        let inner = block.inner(area);
        f.render_widget(block, area);

        let kind = match track.kind {
            TrackKind::Import => "monitor volume",
            TrackKind::RawTake => "pre-rig level (drives the amp)",
        };
        let pct = (track.gain * 100.0).round() as i32;
        let bar_w = inner.width.saturating_sub(2) as usize;
        let filled = ((track.gain / 2.0).clamp(0.0, 1.0) * bar_w as f32).round() as usize;
        let bar = format!(
            "{}{}",
            "█".repeat(filled),
            "·".repeat(bar_w.saturating_sub(filled))
        );
        let text = vec![
            Line::from(Span::styled(
                truncate(&track.name, inner.width as usize),
                Style::default().fg(CHROME),
            )),
            Line::from(Span::styled(kind.to_owned(), Style::default().fg(DIM))),
            Line::from(Span::styled(bar, Style::default().fg(ACCENT))),
            Line::from(vec![
                Span::styled(
                    format!("{pct}%  "),
                    Style::default().fg(AMBER).add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    "←/→ adjust · R reset · Enter/Esc close",
                    Style::default().fg(DIM),
                ),
            ]),
        ];
        f.render_widget(Paragraph::new(text), inner);
    }

    /// Render the clip-position modal.
    pub(super) fn render_move_modal(&self, f: &mut Frame) {
        let Some(id) = self.move_edit else {
            return;
        };
        let Some(track) = self.session.track(id) else {
            return;
        };
        let area = centered_box(48, 7, f.area());
        f.render_widget(Clear, area);
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Double)
            .border_style(Style::default().fg(ACCENT))
            .title(Span::styled(
                " M O V E   C L I P ",
                Style::default().fg(AMBER).add_modifier(Modifier::BOLD),
            ))
            .style(panel_style());
        let inner = block.inner(area);
        f.render_widget(block, area);

        let start_frames = self
            .session
            .ticks_to_frames(track.start_ticks, self.sample_rate);
        let length_frames = self
            .session
            .ticks_to_frames(track.length_ticks, self.sample_rate);
        let text = vec![
            Line::from(Span::styled(
                truncate(&track.name, inner.width as usize),
                Style::default().fg(CHROME),
            )),
            Line::from(vec![
                Span::styled("start ", Style::default().fg(DIM)),
                Span::styled(
                    mmss(start_frames, self.sample_rate),
                    Style::default().fg(AMBER).add_modifier(Modifier::BOLD),
                ),
                Span::styled("   end ", Style::default().fg(DIM)),
                Span::styled(
                    mmss(start_frames + length_frames, self.sample_rate),
                    Style::default().fg(CHROME),
                ),
            ]),
            Line::from(Span::styled(
                "←/→ move by step · +/− change step · R to top",
                Style::default().fg(DIM),
            )),
            Line::from(Span::styled("Enter / Esc close", Style::default().fg(DIM))),
        ];
        f.render_widget(Paragraph::new(text), inner);
    }
}

fn to_player_kind(kind: TrackKind) -> PlayerKind {
    match kind {
        TrackKind::Import => PlayerKind::Import,
        TrackKind::RawTake => PlayerKind::RawTake,
    }
}

fn kind_slug(kind: TrackKind) -> &'static str {
    match kind {
        TrackKind::Import => "import",
        TrackKind::RawTake => "raw_take",
    }
}

/// Everything the UI must gather from outside the session when saving it.
pub(super) struct SaveContext<'a> {
    pub dir: &'a Path,
    pub params: &'a Params,
    pub practice: &'a Practice,
    pub metronome: &'a Metronome,
    pub external_ir: Option<&'a Path>,
    pub external_ir_active: bool,
    pub clap: Option<project::ClapSpec>,
    pub au: Option<project::AuSpec>,
}

fn file_entry(file: &TrackFile, selected: bool) -> Line<'static> {
    let (prefix, style) = if selected {
        (
            "▶ ",
            Style::default()
                .fg(ACCENT)
                .add_modifier(Modifier::BOLD | Modifier::REVERSED),
        )
    } else {
        ("  ", Style::default().fg(CHROME))
    };
    Line::from(vec![
        Span::styled(
            prefix.to_owned(),
            Style::default().fg(if selected { ACCENT } else { DIM }),
        ),
        Span::styled(file.label.clone(), style),
        Span::styled(format!("  {}", file.detail), Style::default().fg(DIM)),
    ])
}

/// Braille dot bit for dot-column `d` (0/1, left→right) and dot-row `r`
/// (0..4, top→bottom).
fn braille_bit(d: usize, r: usize) -> u8 {
    match (d, r) {
        (0, 0) => 0x01,
        (0, 1) => 0x02,
        (0, 2) => 0x04,
        (0, 3) => 0x40,
        (1, 0) => 0x08,
        (1, 1) => 0x10,
        (1, 2) => 0x20,
        (1, 3) => 0x80,
        _ => 0,
    }
}

/// The Unicode braille glyph for an 8-bit dot pattern (U+2800 base).
fn braille_glyph(bits: u8) -> char {
    char::from_u32(0x2800 + bits as u32).unwrap_or(' ')
}

/// Solid 2 × 2 quadrant glyph for a 4-bit pattern: bit 0 top-left, 1 top-right,
/// 2 bottom-left, 3 bottom-right.
fn quadrant_glyph(bits: u8) -> char {
    match bits & 0b1111 {
        0b0000 => ' ',
        0b0001 => '▘',
        0b0010 => '▝',
        0b0011 => '▀',
        0b0100 => '▖',
        0b0101 => '▌',
        0b0110 => '▞',
        0b0111 => '▛',
        0b1000 => '▗',
        0b1001 => '▚',
        0b1010 => '▐',
        0b1011 => '▜',
        0b1100 => '▄',
        0b1101 => '▙',
        0b1110 => '▟',
        _ => '█',
    }
}

/// The 64 sextant glyphs indexed by a 6-bit pattern. Positions are numbered
/// `1 2 / 3 4 / 5 6`, so bit `k` is position `k + 1`. The block omits three
/// patterns that already have characters: `{1,3,5}` (`▌`), `{2,4,6}` (`▐`) and
/// all six (`█`).
const SEXTANT_GLYPHS: [char; 64] = build_sextants();

const fn build_sextants() -> [char; 64] {
    let mut table = [' '; 64];
    let mut cp: u32 = 0x1FB00;
    let mut n: u32 = 1;
    while n <= 6 {
        let mut s: u32 = 0;
        while s < (1u32 << (n - 1)) {
            let mut mask: u32 = 1u32 << (n - 1);
            let mut i: u32 = 1;
            while i < n {
                if s & (1u32 << (i - 1)) != 0 {
                    mask |= 1u32 << (i - 1);
                }
                i += 1;
            }
            table[mask as usize] = match mask {
                0b010101 => '▌',
                0b101010 => '▐',
                0b111111 => '█',
                _ => {
                    let c = match char::from_u32(cp) {
                        Some(c) => c,
                        None => '?',
                    };
                    cp += 1;
                    c
                }
            };
            s += 1;
        }
        n += 1;
    }
    table
}

/// Ruler tick label. Kept as short as the context allows — seconds only under a
/// minute, and the precision follows the tick so zoomed windows read cleanly.
fn ruler_label(frames: usize, sr: f32, tick: f64) -> String {
    let secs = if sr > 0.0 {
        frames as f64 / f64::from(sr)
    } else {
        0.0
    };
    let m = (secs / 60.0).floor() as u64;
    let s = secs - m as f64 * 60.0;
    if tick >= 1.0 {
        if m > 0 {
            format!("{m}:{:02}", s.floor() as u64)
        } else {
            format!("{}", s.floor() as u64)
        }
    } else if tick >= 0.1 {
        if m > 0 {
            format!("{m}:{s:04.1}")
        } else {
            format!("{s:.1}")
        }
    } else if m > 0 {
        format!("{m}:{s:05.2}")
    } else {
        format!("{s:.2}")
    }
}

/// Format a step in seconds without a trailing `.0` (e.g. `5`, `0.5`).
fn format_step(secs: f32) -> String {
    if secs.fract().abs() < 1e-6 {
        format!("{}", secs as i64)
    } else {
        format!("{secs}")
    }
}

/// `mm:ss.dd` (hundredths) for the exact cursor time at `sr`.
fn mmss_precise(frames: usize, sr: f32) -> String {
    let centis = if sr > 0.0 {
        ((frames as f64 / f64::from(sr)) * 100.0).round() as u64
    } else {
        0
    };
    let m = centis / 6000;
    let s = (centis / 100) % 60;
    let cs = centis % 100;
    format!("{m:02}:{s:02}.{cs:02}")
}

/// `mm:ss` for a frame count at `sr`.
pub(super) fn mmss(frames: usize, sr: f32) -> String {
    let secs = if sr > 0.0 {
        (frames as f32 / sr) as u64
    } else {
        0
    };
    format!("{:02}:{:02}", secs / 60, secs % 60)
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_owned()
    } else {
        s.chars().take(n.saturating_sub(1)).collect::<String>() + "…"
    }
}

/// Standard practice locations, scanned for audio files.
/// User import-library settings, persisted in `~/.config/rusty-riff/practice.conf`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct ImportConfig {
    /// Base directory to fetch tracks from.
    path: Option<PathBuf>,
    /// Scan only `path` (skip the Music/Desktop/`.` defaults).
    only: bool,
    /// Recurse into `path`'s subdirectories (off by default).
    subpaths: bool,
}

fn import_config_path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".config").join("rusty-riff").join("practice.conf"))
}

/// Parse `practice.conf`. Unknown keys and `#` comments are ignored.
fn parse_import_config(text: &str) -> ImportConfig {
    let mut cfg = ImportConfig::default();
    for line in text.lines() {
        let line = line.split('#').next().unwrap_or("").trim();
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        match key.trim() {
            "path" | "dir" => {
                let value = value.trim();
                if !value.is_empty() {
                    cfg.path = Some(PathBuf::from(value));
                }
            }
            "only" => cfg.only = truthy(value.trim()),
            "subpaths" => cfg.subpaths = truthy(value.trim()),
            _ => {}
        }
    }
    cfg
}

fn load_import_config() -> ImportConfig {
    import_config_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map(|t| parse_import_config(&t))
        .unwrap_or_default()
}

fn save_import_config(cfg: &ImportConfig) -> std::io::Result<()> {
    let path = import_config_path()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "no config dir"))?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut text = String::new();
    if let Some(p) = &cfg.path {
        text.push_str(&format!("path = {}\n", p.display()));
    }
    text.push_str(&format!("only = {}\n", cfg.only));
    text.push_str(&format!("subpaths = {}\n", cfg.subpaths));
    std::fs::write(path, text)
}

fn truthy(value: &str) -> bool {
    matches!(
        value.to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

/// Expand a leading `~` against the home directory.
fn expand_tilde(path: &Path) -> PathBuf {
    if let Ok(rest) = path.strip_prefix("~")
        && let Some(home) = dirs::home_dir()
    {
        return home.join(rest);
    }
    path.to_path_buf()
}

fn scan() -> Vec<TrackFile> {
    let cfg = load_import_config();
    // `$RUSTY_AMP_PRACTICE_DIR` overrides the configured base path.
    let base = std::env::var("RUSTY_AMP_PRACTICE_DIR")
        .ok()
        .map(PathBuf::from)
        .or_else(|| cfg.path.clone());
    let depth = if cfg.subpaths { 3 } else { 0 };

    let mut roots: Vec<(PathBuf, usize)> = Vec::new();
    if let Some(base) = base {
        roots.push((expand_tilde(&base), depth));
    }
    // The usual locations stay unless the user restricted the library to one
    // path (or there is no configured path at all).
    if !cfg.only || roots.is_empty() {
        if let Some(home) = dirs::home_dir() {
            roots.push((home.join("Music"), 3));
            roots.push((home.join("Desktop"), 3));
        }
        roots.push((PathBuf::from("."), 3));
    }

    let mut out: Vec<TrackFile> = Vec::new();
    for (root, depth) in roots {
        collect_audio(&root, depth, &mut out);
    }
    out.sort_by(|a, b| a.label.cmp(&b.label).then(a.path.cmp(&b.path)));
    out.dedup_by(|a, b| a.path == b.path);
    out
}

fn collect_audio(dir: &Path, depth: usize, out: &mut Vec<TrackFile>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if depth > 0 {
                collect_audio(&path, depth - 1, out);
            }
        } else if is_audio(&path) {
            let label = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("track")
                .to_owned();
            let detail = path
                .parent()
                .and_then(|p| p.to_str())
                .unwrap_or_default()
                .to_owned();
            out.push(TrackFile {
                path,
                label,
                detail,
            });
        }
    }
}

fn is_audio(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| {
        e.eq_ignore_ascii_case("mp3")
            || e.eq_ignore_ascii_case("wav")
            || e.eq_ignore_ascii_case("flac")
    })
}

fn centered_rect(percent_x: u16, area: Rect) -> Rect {
    let width = (area.width * percent_x / 100).max(30).min(area.width);
    let height = (area.height * 70 / 100).max(10);
    let x = (area.width.saturating_sub(width)) / 2;
    let y = (area.height.saturating_sub(height)) / 2;
    Rect {
        x: area.x + x,
        y: area.y + y,
        width,
        height,
    }
}

fn centered_box(width: u16, height: u16, area: Rect) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    let x = (area.width.saturating_sub(width)) / 2;
    let y = (area.height.saturating_sub(height)) / 2;
    Rect {
        x: area.x + x,
        y: area.y + y,
        width,
        height,
    }
}

fn shade(c: Color, factor: f32) -> Color {
    match c {
        Color::Rgb(r, g, b) => Color::Rgb(
            (r as f32 * factor) as u8,
            (g as f32 * factor) as u8,
            (b as f32 * factor) as u8,
        ),
        other => other,
    }
}

fn border_style(active: bool) -> Style {
    Style::default().fg(if active { ACCENT } else { shade(ACCENT, 0.5) })
}

/// Dimmed accent used for the gutter/timeline divider (matches an unfocused
/// pane border).
fn divider_style() -> Style {
    Style::default().fg(shade(ACCENT, 0.5))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ready(name: &str) -> (Option<AssetRef>, TrackKind) {
        (
            Some(AssetRef {
                path: PathBuf::from(name),
                source_sample_rate: 48_000,
                source_channels: 1,
            }),
            TrackKind::RawTake,
        )
    }

    fn scratch_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "rusty-riff-practice-{}-{}-{}",
            tag,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    /// A recovery WAV referenced by a session track must be recognised, so the
    /// browser's discard can refuse to delete it out from under the track.
    #[test]
    fn references_recovery_matches_session_tracks() {
        let mut ui = PracticeUi::new();
        let wav = PathBuf::from("/some/recovery/take-1.wav");
        assert!(!ui.references_recovery(&wav));
        ui.session.push(
            1,
            "take".into(),
            TrackKind::RawTake,
            Some(AssetRef {
                path: wav.clone(),
                source_sample_rate: 48_000,
                source_channels: 1,
            }),
            0,
            48_000,
            TrackLifecycle::Ready,
        );
        assert!(ui.references_recovery(&wav));
        assert!(!ui.references_recovery(Path::new("/other/take-2.wav")));
    }

    /// Importing a backing track from the browser reports `true` so the caller can
    /// focus the timeline; keys that add nothing report `false`.
    #[test]
    fn browser_enter_reports_an_added_track() {
        let practice = Practice::new();
        let mut ui = PracticeUi::new();
        ui.files.push(TrackFile {
            path: PathBuf::from("/tmp/backing.wav"),
            label: "backing".to_owned(),
            detail: String::new(),
        });
        ui.browser_open = true;
        let before = ui.session.tracks().len();
        assert!(ui.handle_browser_key(KeyCode::Enter, &practice));
        assert_eq!(ui.session.tracks().len(), before + 1, "the track was added");
        assert!(!ui.browser_open, "the browser closes on import");

        // A key that adds nothing must not request a focus change.
        ui.browser_open = true;
        assert!(!ui.handle_browser_key(KeyCode::Esc, &practice));
    }

    /// Starting a take reports `true` so the caller can focus the timeline; a
    /// failed arm (no capture writer) reports `false` and adds no row.
    #[test]
    fn arming_reports_a_started_take() {
        let practice = Practice::new();
        let capture = CaptureState::new();

        let mut ui = PracticeUi::new();
        assert!(!ui.arm(&capture, &practice), "no writer → did not start");
        assert!(ui.session.tracks().is_empty(), "nothing was added");

        let (tx, _rx) = std::sync::mpsc::channel();
        ui.capture_cmd = Some(tx);
        assert!(ui.arm(&capture, &practice), "with a writer → started");
        assert_eq!(ui.session.tracks().len(), 1, "the take row was added");
    }

    /// A track whose recovery source vanished (e.g. the recovery row was discarded
    /// after the take was restored) must not fail the whole save: the existing
    /// project copy is kept and the track retargeted at it.
    #[test]
    fn save_heals_a_track_whose_source_vanished() {
        let dir = scratch_dir("heal");
        let mut ui = PracticeUi::new();
        let missing = ui
            .session
            .recovery_dir()
            .expect("recovery dir")
            .join("take-9.wav");
        assert!(!missing.exists(), "the probe asset must not exist");
        ui.session.push(
            9,
            "take".into(),
            TrackKind::RawTake,
            Some(AssetRef {
                path: missing,
                source_sample_rate: 48_000,
                source_channels: 1,
            }),
            0,
            48_000,
            TrackLifecycle::Ready,
        );
        // A copy from an earlier save is already in the project folder.
        std::fs::create_dir_all(dir.join("audio")).expect("mkdir");
        std::fs::write(dir.join("audio/track-9.wav"), b"audio").expect("seed copy");

        let params = Params::new();
        let practice = Practice::new();
        let metronome = Metronome::new();
        let ctx = SaveContext {
            dir: &dir,
            params: &params,
            practice: &practice,
            metronome: &metronome,
            external_ir: None,
            external_ir_active: false,
            clap: None,
            au: None,
        };
        ui.save_session(ctx)
            .expect("a vanished source must not fail the save");
        assert_eq!(
            ui.session
                .track(9)
                .expect("track 9")
                .asset
                .as_ref()
                .expect("asset")
                .path,
            dir.join("audio/track-9.wav"),
            "the track must be retargeted at the existing project copy"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn export_job_includes_only_unmuted_ready_takes() {
        let mut ui = PracticeUi::new();
        let params = Params::new();

        let (asset, kind) = ready("a.wav");
        ui.session.push(
            1,
            "take a".into(),
            kind,
            asset,
            0,
            48_000,
            TrackLifecycle::Ready,
        );
        let (asset, kind) = ready("b.wav");
        ui.session.push(
            2,
            "take b".into(),
            kind,
            asset,
            0,
            48_000,
            TrackLifecycle::Ready,
        );
        ui.session.track_mut(2).expect("track b").muted = true;
        // An import must never be part of a guitar-only export.
        ui.session.push(
            3,
            "backing".into(),
            TrackKind::Import,
            Some(AssetRef {
                path: PathBuf::from("c.wav"),
                source_sample_rate: 48_000,
                source_channels: 2,
            }),
            0,
            48_000,
            TrackLifecycle::Ready,
        );

        let job = ui
            .build_export_job(
                PathBuf::from("/tmp/out.wav"),
                &params,
                None,
                false,
                None,
                None,
                None,
            )
            .expect("job");
        assert_eq!(job.clips.len(), 1, "only the unmuted take should export");
        assert_eq!(job.clips[0].path, PathBuf::from("a.wav"));
        assert_eq!(job.sample_rate, ui.session.project_sample_rate());
    }

    #[test]
    fn export_job_errors_when_nothing_to_render() {
        let ui = PracticeUi::new();
        let params = Params::new();
        let err = ui
            .build_export_job(
                PathBuf::from("/tmp/out.wav"),
                &params,
                None,
                false,
                None,
                None,
                None,
            )
            .err()
            .expect("empty session must not produce a job");
        assert!(err.contains("No unmuted"), "{err}");
    }

    #[test]
    fn export_job_carries_the_loop_region() {
        let mut ui = PracticeUi::new();
        let params = Params::new();
        let (asset, kind) = ready("a.wav");
        ui.session.push(
            1,
            "take a".into(),
            kind,
            asset,
            0,
            48_000,
            TrackLifecycle::Ready,
        );

        let job = ui
            .build_export_job(
                PathBuf::from("/tmp/out.wav"),
                &params,
                None,
                false,
                None,
                None,
                Some((4_800, 9_600)),
            )
            .expect("job");
        assert_eq!(
            job.range_ticks,
            Some((
                ui.session.frames_to_ticks(4_800, 48_000.0),
                ui.session.frames_to_ticks(9_600, 48_000.0),
            ))
        );

        // No range → render the whole session.
        let job = ui
            .build_export_job(
                PathBuf::from("/tmp/out.wav"),
                &params,
                None,
                false,
                None,
                None,
                None,
            )
            .expect("job");
        assert_eq!(job.range_ticks, None);
    }

    #[test]
    fn go_to_start_uses_loop_in_point_when_enabled() {
        let ui = PracticeUi::new();
        let practice = Practice::new();
        practice.loop_enabled.store(true, Relaxed);
        practice.loop_start.store(4_800, Relaxed);
        practice.loop_end.store(9_600, Relaxed);
        ui.go_to_start(&practice);
        assert_eq!(practice.snapshot().seek, Some(4_800));
    }

    #[test]
    fn go_to_start_falls_back_to_zero() {
        let ui = PracticeUi::new();
        let practice = Practice::new();
        // No loop region.
        ui.go_to_start(&practice);
        assert_eq!(practice.snapshot().seek, Some(0));
        // A degenerate (empty) loop region also goes to the root.
        practice.loop_enabled.store(true, Relaxed);
        practice.loop_start.store(100, Relaxed);
        practice.loop_end.store(100, Relaxed);
        ui.go_to_start(&practice);
        assert_eq!(practice.snapshot().seek, Some(0));
    }

    #[test]
    fn finalize_pauses_the_transport() {
        let mut ui = PracticeUi::new();
        let practice = Practice::new();
        let capture = CaptureState::new();
        practice.playing.store(true, Relaxed);
        ui.finalize_capture(&capture, &practice);
        assert!(!practice.playing.load(Relaxed), "stop must pause");
    }

    #[test]
    fn seek_moves_on_an_empty_timeline() {
        let ui = PracticeUi::new();
        let practice = Practice::new();
        // No tracks: extent is zero, but stepping must still move the playhead
        // so the first import can be placed.
        ui.seek_by(&practice, 1);
        let step = (ui.session.seek_seconds() * 48_000.0) as usize;
        assert_eq!(practice.snapshot().seek, Some(step));
        ui.seek_by(&practice, -1);
        assert_eq!(practice.snapshot().seek, Some(0));
    }

    #[test]
    fn tab_zoom_cycles_through_row_heights() {
        let mut ui = PracticeUi::new();
        let (asset, kind) = ready("a.wav");
        ui.session
            .push(1, "a".into(), kind, asset, 0, 48_000, TrackLifecycle::Ready);
        let (asset, kind) = ready("b.wav");
        ui.session
            .push(2, "b".into(), kind, asset, 0, 48_000, TrackLifecycle::Ready);

        // Transport focus is inert.
        ui.selection = Selection::Transport;
        ui.tab_zoom();
        assert_eq!(ui.row_zoom, RowZoom::Normal);

        ui.selection = Selection::Track(1);
        ui.tab_zoom();
        assert_eq!(ui.row_zoom, RowZoom::Expanded(1));
        // Tab on a different row splits the pane 50/50.
        ui.selection = Selection::Track(2);
        ui.tab_zoom();
        assert_eq!(ui.row_zoom, RowZoom::Split(1, 2));
        // Tab again collapses back to normal.
        ui.tab_zoom();
        assert_eq!(ui.row_zoom, RowZoom::Normal);

        // Tab on an already expanded row collapses it.
        ui.selection = Selection::Track(1);
        ui.tab_zoom();
        assert_eq!(ui.row_zoom, RowZoom::Expanded(1));
        ui.tab_zoom();
        assert_eq!(ui.row_zoom, RowZoom::Normal);
    }

    fn screen_text(term: &ratatui::Terminal<ratatui::backend::TestBackend>) -> String {
        let buf = term.backend().buffer();
        let mut out = String::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                out.push_str(buf[(x, y)].symbol());
            }
            out.push('\n');
        }
        out
    }

    #[test]
    fn renders_ruler_and_every_zoom_state() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let mut ui = PracticeUi::new();
        let practice = Practice::new();
        for id in 1..=2u64 {
            ui.session.push(
                id,
                format!("t{id}"),
                TrackKind::Import,
                Some(AssetRef {
                    path: PathBuf::from(format!("{id}.wav")),
                    source_sample_rate: 48_000,
                    source_channels: 2,
                }),
                0,
                48_000,
                TrackLifecycle::Ready,
            );
            ui.session.track_mut(id).expect("track").peaks = vec![(-0.5, 0.5); 64];
        }
        ui.move_selection(true);

        let mut term = Terminal::new(TestBackend::new(90, 20)).expect("test backend");
        for zoom in [RowZoom::Normal, RowZoom::Expanded(1), RowZoom::Split(1, 2)] {
            ui.row_zoom = zoom;
            term.draw(|f| {
                let area = f.area();
                ui.render(f, area, &practice, true, false, false);
            })
            .expect("draw");
            let text = screen_text(&term);
            assert!(text.contains("0.2"), "ruler tick missing under {zoom:?}");
            let braille = text.chars().any(|c| ('\u{2800}'..='\u{28FF}').contains(&c));
            assert!(braille, "braille waveform missing under {zoom:?}");
        }
    }

    #[test]
    fn waveform_aggregates_a_transient_into_its_column() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let mut ui = PracticeUi::new();
        let practice = Practice::new();
        ui.session.push(
            1,
            "t".into(),
            TrackKind::Import,
            Some(AssetRef {
                path: PathBuf::from("t.wav"),
                source_sample_rate: 48_000,
                source_channels: 2,
            }),
            0,
            48_000,
            TrackLifecycle::Ready,
        );
        // One full-scale bucket among silence: the envelope must show it as a
        // single spike, not smear it across the whole row.
        let mut peaks = vec![(0.0f32, 0.0f32); 64];
        peaks[32] = (-1.0, 1.0);
        ui.session.track_mut(1).expect("track").peaks = peaks;
        ui.move_selection(true);

        let mut term = Terminal::new(TestBackend::new(90, 12)).expect("test backend");
        term.draw(|f| {
            let area = f.area();
            ui.render(f, area, &practice, true, false, false);
        })
        .expect("draw");
        let text = screen_text(&term);
        // The transient paints braille glyphs in one (at most two) cells per
        // line; the silent span stays blank/dim.
        let braille = text
            .chars()
            .filter(|c| ('\u{2800}'..='\u{28FF}').contains(c))
            .count();
        assert!(
            braille >= ROW_HEIGHT,
            "transient should paint at least one row, got {braille}"
        );
        assert!(
            braille <= ROW_HEIGHT * 2,
            "a single transient must not fill the pane, got {braille}"
        );
        assert!(text.contains('·'), "silent span should show the baseline");
    }

    #[test]
    fn zoom_in_and_out_cycle_windows_and_reset_to_fit() {
        let mut ui = PracticeUi::new();
        assert_eq!(ui.view_secs, None);
        ui.zoom_in();
        assert_eq!(ui.view_secs, Some(60.0), "first zoom-in picks the widest");
        for _ in 0..20 {
            ui.zoom_in();
        }
        assert_eq!(ui.view_secs, Some(0.2), "zoom-in clamps at the closest");
        for _ in 0..20 {
            ui.zoom_out();
        }
        assert_eq!(
            ui.view_secs, None,
            "zoom-out past the widest returns to fit"
        );
    }

    #[test]
    fn view_is_playhead_centred_and_clamped() {
        let mut ui = PracticeUi::new();
        let practice = Practice::new();
        let (asset, kind) = ready("a.wav");
        ui.session.push(
            1,
            "a".into(),
            kind,
            asset,
            0,
            48_000 * 10,
            TrackLifecycle::Ready,
        );
        assert_eq!(ui.view(&practice), (0, 480_000), "fit covers the span");
        ui.view_secs = Some(2.0);
        assert_eq!(ui.view(&practice), (0, 96_000), "clamped at the left edge");
        practice.store_position(240_000);
        assert_eq!(
            ui.view(&practice),
            (192_000, 96_000),
            "centred on the playhead"
        );
    }

    #[test]
    fn ruler_label_precision_follows_tick() {
        // Under a minute: seconds only, as short as the tick allows.
        assert_eq!(ruler_label(0, 48_000.0, 1.0), "0");
        assert_eq!(ruler_label(24_000, 48_000.0, 0.1), "0.5");
        assert_eq!(ruler_label(480, 48_000.0, 0.01), "0.01");
        // Past a minute: minutes and seconds.
        assert_eq!(ruler_label(48_000 * 65, 48_000.0, 1.0), "1:05");
    }

    #[test]
    fn cycle_glyphs_walks_every_style_and_wraps() {
        let mut ui = PracticeUi::new();
        assert_eq!(ui.wave_glyphs, WaveGlyphs::Braille);
        ui.cycle_glyphs();
        assert_eq!(ui.wave_glyphs, WaveGlyphs::Sextant);
        ui.cycle_glyphs();
        assert_eq!(ui.wave_glyphs, WaveGlyphs::Quadrant);
        ui.cycle_glyphs();
        assert_eq!(ui.wave_glyphs, WaveGlyphs::Half);
        ui.cycle_glyphs();
        assert_eq!(ui.wave_glyphs, WaveGlyphs::Braille);
    }

    #[test]
    fn wave_glyphs_map_their_patterns() {
        // Half: bit 0 top, bit 1 bottom.
        assert_eq!(WaveGlyphs::Half.glyph(0b01), '▀');
        assert_eq!(WaveGlyphs::Half.glyph(0b10), '▄');
        assert_eq!(WaveGlyphs::Half.glyph(0b11), '█');

        // Quadrant: bits TL, TR, BL, BR (row-major).
        assert_eq!(WaveGlyphs::Quadrant.glyph(0b0011), '▀'); // top row
        assert_eq!(WaveGlyphs::Quadrant.glyph(0b1001), '▚'); // TL + BR
        assert_eq!(WaveGlyphs::Quadrant.glyph(0b0110), '▞'); // TR + BL
        assert_eq!(WaveGlyphs::Quadrant.glyph(0b1111), '█');

        // Braille: bit r*2 + d, dot-column 0 top dot is U+2801.
        assert_eq!(WaveGlyphs::Braille.glyph(0b1), '\u{2801}');

        // Sextant: positions 1..6 row-major; the three patterns with existing
        // characters are remapped.
        assert_eq!(WaveGlyphs::Sextant.glyph(0b000001), '\u{1FB00}'); // pos 1
        assert_eq!(WaveGlyphs::Sextant.glyph(0b000011), '\u{1FB02}'); // pos 1+2
        assert_eq!(WaveGlyphs::Sextant.glyph(0b100000), '\u{1FB1E}'); // pos 6
        assert_eq!(WaveGlyphs::Sextant.glyph(0b010101), '▌'); // left column
        assert_eq!(WaveGlyphs::Sextant.glyph(0b101010), '▐'); // right column
        assert_eq!(WaveGlyphs::Sextant.glyph(0b111111), '█'); // full
    }

    #[test]
    fn glyph_styles_render_in_their_own_fonts() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let mut ui = PracticeUi::new();
        let practice = Practice::new();
        ui.session.push(
            1,
            "t".into(),
            TrackKind::Import,
            Some(AssetRef {
                path: PathBuf::from("t.wav"),
                source_sample_rate: 48_000,
                source_channels: 2,
            }),
            0,
            48_000,
            TrackLifecycle::Ready,
        );
        ui.session.track_mut(1).expect("track").peaks = vec![(-0.5, 0.5); 64];
        ui.move_selection(true);

        let mut term = Terminal::new(TestBackend::new(90, 12)).expect("test backend");
        let render = |ui: &PracticeUi, term: &mut Terminal<TestBackend>| {
            term.draw(|f| {
                let area = f.area();
                ui.render(f, area, &practice, true, false, false);
            })
            .expect("draw");
            screen_text(term)
        };

        ui.wave_glyphs = WaveGlyphs::Quadrant;
        let q = render(&ui, &mut term);
        assert!(
            q.chars().any(|c| "▘▝▖▗▀▄▌▐▚▞▛▜▙▟█".contains(c)),
            "quadrant style drew no solid blocks"
        );
        assert!(
            !q.chars().any(|c| ('\u{2800}'..='\u{28FF}').contains(&c)),
            "quadrant style still drew braille"
        );

        ui.wave_glyphs = WaveGlyphs::Sextant;
        let s = render(&ui, &mut term);
        assert!(
            s.chars().any(|c| ('\u{1FB00}'..='\u{1FB3B}').contains(&c)),
            "sextant style drew no legacy-computing blocks"
        );
    }

    #[test]
    fn move_selection_never_lands_on_the_transport() {
        let mut ui = PracticeUi::new();
        let (asset, kind) = ready("a.wav");
        ui.session
            .push(1, "a".into(), kind, asset, 0, 48_000, TrackLifecycle::Ready);
        let (asset, kind) = ready("b.wav");
        ui.session
            .push(2, "b".into(), kind, asset, 0, 48_000, TrackLifecycle::Ready);

        // From no selection: backward picks the last, forward the first.
        ui.selection = Selection::Transport;
        ui.move_selection(false);
        assert_eq!(ui.selection, Selection::Track(2));
        // Clamps at the first row and never returns to the header.
        ui.move_selection(false);
        assert_eq!(ui.selection, Selection::Track(1));
        ui.move_selection(false);
        assert_eq!(ui.selection, Selection::Track(1));
        // Clamps at the last row.
        ui.move_selection(true);
        assert_eq!(ui.selection, Selection::Track(2));
        ui.move_selection(true);
        assert_eq!(ui.selection, Selection::Track(2));
    }

    #[test]
    fn stopping_a_take_restores_the_previous_row_zoom() {
        let mut ui = PracticeUi::new();
        let capture = CaptureState::new();
        let practice = Practice::new();
        ui.record_zoom = Some(RowZoom::Split(1, 2));
        ui.row_zoom = RowZoom::Expanded(9);
        ui.finalize_capture(&capture, &practice);
        assert_eq!(ui.row_zoom, RowZoom::Split(1, 2));
        assert_eq!(ui.record_zoom, None);
    }

    #[test]
    fn envelope_gain_fits_the_peak_with_headroom() {
        assert_eq!(envelope_gain(&[]), 0.0);
        assert_eq!(envelope_gain(&[(0.0, 0.0)]), 0.0);
        assert!((envelope_gain(&[(-0.2, 0.2)]) - 4.5).abs() < 1e-5);
        assert!((envelope_gain(&[(-1.0, 1.0)]) - 0.9).abs() < 1e-5);
    }

    #[test]
    fn cycle_gain_toggles_normalized_and_absolute() {
        let mut ui = PracticeUi::new();
        assert_eq!(ui.wave_gain, WaveGain::Normalized);
        ui.cycle_gain();
        assert_eq!(ui.wave_gain, WaveGain::Absolute);
        ui.cycle_gain();
        assert_eq!(ui.wave_gain, WaveGain::Normalized);
    }

    #[test]
    fn normalized_fills_more_than_absolute() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let mut ui = PracticeUi::new();
        let practice = Practice::new();
        ui.session.push(
            1,
            "t".into(),
            TrackKind::Import,
            Some(AssetRef {
                path: PathBuf::from("t.wav"),
                source_sample_rate: 48_000,
                source_channels: 2,
            }),
            0,
            48_000,
            TrackLifecycle::Ready,
        );
        // A quiet track: ±0.2 raw amplitude.
        ui.session.track_mut(1).expect("track").peaks = vec![(-0.2, 0.2); 64];
        ui.move_selection(true);

        let dots = |text: &str| -> u32 {
            text.chars()
                .filter(|c| ('\u{2800}'..='\u{28FF}').contains(c))
                .map(|c| (c as u32 - 0x2800).count_ones())
                .sum()
        };
        let mut term = Terminal::new(TestBackend::new(90, 12)).expect("test backend");
        let render = |ui: &PracticeUi, term: &mut Terminal<TestBackend>| {
            term.draw(|f| {
                let area = f.area();
                ui.render(f, area, &practice, true, false, false);
            })
            .expect("draw");
            screen_text(term)
        };

        ui.wave_gain = WaveGain::Normalized;
        let normalized = dots(&render(&ui, &mut term));
        ui.wave_gain = WaveGain::Absolute;
        let absolute = dots(&render(&ui, &mut term));
        assert!(
            normalized > absolute,
            "normalized ({normalized}) should fill more than absolute ({absolute})"
        );
    }

    /// Deleting a take must identify its recovery WAV so it can be discarded, but
    /// never an import's original file or a take already saved into a project.
    #[test]
    fn deleted_takes_identify_their_recovery_file() {
        let mut ui = PracticeUi::new();
        let recovery = ui
            .session
            .recovery_dir()
            .expect("recovery dir")
            .join("take-1.wav");
        ui.session.push(
            1,
            "take".into(),
            TrackKind::RawTake,
            Some(AssetRef {
                path: recovery.clone(),
                source_sample_rate: 48_000,
                source_channels: 1,
            }),
            0,
            48_000,
            TrackLifecycle::Ready,
        );
        ui.session.push(
            2,
            "backing".into(),
            TrackKind::Import,
            Some(AssetRef {
                path: PathBuf::from("/music/song.wav"),
                source_sample_rate: 48_000,
                source_channels: 2,
            }),
            0,
            48_000,
            TrackLifecycle::Ready,
        );
        ui.session.push(
            3,
            "saved".into(),
            TrackKind::RawTake,
            Some(AssetRef {
                path: PathBuf::from("/projects/My_Set/audio/track-3.wav"),
                source_sample_rate: 48_000,
                source_channels: 1,
            }),
            0,
            48_000,
            TrackLifecycle::Ready,
        );
        assert_eq!(ui.track_recovery_file(1), Some(recovery));
        assert_eq!(
            ui.track_recovery_file(2),
            None,
            "an import's file is not a recovery file"
        );
        assert_eq!(
            ui.track_recovery_file(3),
            None,
            "a saved take lives in its project, not recovery"
        );
    }

    #[test]
    fn deleting_a_zoomed_row_clears_the_zoom() {
        let mut ui = PracticeUi::new();
        let (asset, kind) = ready("a.wav");
        ui.session
            .push(1, "a".into(), kind, asset, 0, 48_000, TrackLifecycle::Ready);
        ui.selection = Selection::Track(1);
        ui.tab_zoom();
        assert_eq!(ui.row_zoom, RowZoom::Expanded(1));
        ui.session.remove(1);
        ui.sanitize_zoom();
        assert_eq!(ui.row_zoom, RowZoom::Normal);
    }

    #[test]
    fn empty_timeline_shows_the_divider_and_minor_ticks() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let ui = PracticeUi::new();
        let practice = Practice::new();
        let mut term = Terminal::new(TestBackend::new(90, 16)).expect("test backend");
        term.draw(|f| {
            let area = f.area();
            ui.render(f, area, &practice, true, false, false);
        })
        .expect("draw");
        let text = screen_text(&term);
        // Divider at the gutter plus the playhead at frame 0 sit side by side.
        assert!(text.contains("││"), "gutter divider missing");
        // Minor ruler ticks between the labels.
        assert!(text.contains('ˌ'), "ruler minor ticks missing");
    }

    #[test]
    fn notices_expire_after_the_ttl() {
        let n = Notice {
            text: "hi".into(),
            at: Instant::now(),
        };
        assert!(!n.expired(Instant::now()));
        assert!(n.expired(Instant::now() + NOTICE_TTL));
    }

    #[test]
    fn import_config_parses_flags_and_comments() {
        let cfg =
            parse_import_config("path = ~/Music/guitar\nonly = yes\nsubpaths = true  # recurse\n");
        assert_eq!(cfg.path.as_deref(), Some(Path::new("~/Music/guitar")));
        assert!(cfg.only);
        assert!(cfg.subpaths);

        let cfg = parse_import_config("only = false\nsubpaths = 0\n");
        assert_eq!(cfg.path, None);
        assert!(!cfg.only && !cfg.subpaths);
    }

    #[test]
    fn expand_tilde_uses_the_home_dir() {
        let expanded = expand_tilde(Path::new("~/guitar"));
        assert!(expanded.ends_with("guitar"));
        assert!(!expanded.to_string_lossy().starts_with('~'));
        // Relative paths are untouched.
        assert_eq!(expand_tilde(Path::new("tracks")), PathBuf::from("tracks"));
    }

    #[test]
    fn browser_filter_matches_label_and_path() {
        let mut ui = PracticeUi::new();
        ui.files = vec![
            TrackFile {
                path: PathBuf::from("/a/backing_track.wav"),
                label: "backing_track".into(),
                detail: "/a".into(),
            },
            TrackFile {
                path: PathBuf::from("/b/solo.flac"),
                label: "solo".into(),
                detail: "/b".into(),
            },
        ];
        assert_eq!(ui.visible_file_indices(), vec![0, 1]);
        ui.browser_filter = "solo".into();
        assert_eq!(ui.visible_file_indices(), vec![1]);
        ui.browser_filter = "/A/".into();
        assert_eq!(ui.visible_file_indices(), vec![0]);
    }
}
