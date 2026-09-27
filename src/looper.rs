//! Monitor-only phrase looper.
//!
//! The looper records the post-trim dry guitar into a preallocated buffer and
//! plays it back into the monitor path only — the loop bus is mixed in *after*
//! the recording tap in `audio::mod`, exactly like the metronome and the practice
//! player, so a loop never lands in a rendered WAV.
//!
//! The audio thread owns a [`Looper`] (its buffers are allocated once, before the
//! streams start, and never resized in `process`). The UI owns the shared
//! [`LooperControl`], whose atomics carry transport commands in and published
//! state/length out. The audio thread drains the commands once per block and then
//! feeds one post-trim dry sample per frame.

use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, Ordering::Relaxed};

/// Longest loop the buffer can hold, in seconds. At 48 kHz that is 1.44 M frames
/// (~5.8 MB) for the loop plus the same again for the undo layer.
pub const MAX_LOOP_SECS: f32 = 30.0;

/// Looper transport state. Published by the audio thread for the UI.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoopState {
    /// Not recording, not playing; `process` returns silence.
    Idle,
    /// Capturing the dry input into the loop buffer; `process` returns silence.
    Recording,
    /// Playing the loop buffer (optionally layering input over it).
    Playing,
}

impl LoopState {
    /// Encode the state for the shared [`AtomicU8`].
    const fn as_u8(self) -> u8 {
        match self {
            Self::Idle => 0,
            Self::Recording => 1,
            Self::Playing => 2,
        }
    }

    /// Decode a state published to the shared [`AtomicU8`]; unknown bytes decode
    /// to [`LoopState::Idle`] rather than panicking on the audio thread.
    const fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::Recording,
            2 => Self::Playing,
            _ => Self::Idle,
        }
    }
}

/// Live looper transport shared between the UI and the audio thread.
///
/// Commands (`record`/`stop`/`clear`/`undo`) are one-shot flags the audio thread
/// consumes; `overdub` is a level the UI sets and the audio thread reads; `state`
/// and `len` are published back for display.
pub struct LooperControl {
    /// UI → audio: start (or advance) recording.
    record: AtomicBool,
    /// UI → audio: stop recording or pause playback.
    stop: AtomicBool,
    /// UI → audio: discard the loop.
    clear: AtomicBool,
    /// UI → audio: restore the pre-overdub layer.
    undo: AtomicBool,
    /// UI state: layer input while playing.
    overdub: AtomicBool,
    /// Audio → UI: current [`LoopState`].
    state: AtomicU8,
    /// Audio → UI: recorded length in frames.
    len: AtomicU32,
}

impl Default for LooperControl {
    fn default() -> Self {
        Self::new()
    }
}

impl LooperControl {
    pub const fn new() -> Self {
        Self {
            record: AtomicBool::new(false),
            stop: AtomicBool::new(false),
            clear: AtomicBool::new(false),
            undo: AtomicBool::new(false),
            overdub: AtomicBool::new(false),
            state: AtomicU8::new(LoopState::Idle.as_u8()),
            len: AtomicU32::new(0),
        }
    }

    /// Request that the looper start (or advance) recording.
    pub fn request_record(&self) {
        self.record.store(true, Relaxed);
    }

    /// Request that the looper stop recording or pause playback.
    pub fn request_stop(&self) {
        self.stop.store(true, Relaxed);
    }

    /// Request that the looper discard its buffer.
    pub fn request_clear(&self) {
        self.clear.store(true, Relaxed);
    }

    /// Request that the looper restore the pre-overdub layer.
    pub fn request_undo(&self) {
        self.undo.store(true, Relaxed);
    }

    /// Flip the overdub level; returns the new value.
    pub fn toggle_overdub(&self) -> bool {
        let now = !self.overdub.load(Relaxed);
        self.overdub.store(now, Relaxed);
        now
    }

    /// Whether overdub is enabled (input is layered while playing).
    pub fn overdub_enabled(&self) -> bool {
        self.overdub.load(Relaxed)
    }

    /// The last transport state published by the audio thread.
    pub fn state(&self) -> LoopState {
        LoopState::from_u8(self.state.load(Relaxed))
    }

    /// The recorded loop length in frames.
    pub fn len(&self) -> u32 {
        self.len.load(Relaxed)
    }

    /// True when the looper holds no recorded material.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Audio-thread looper voice. All buffers are allocated in [`Looper::new`] and
/// indexed in place, so [`Looper::process`] never allocates.
pub struct Looper {
    /// The loop buffer, `max_len` frames long.
    buf: Vec<f32>,
    /// The undo layer: a snapshot of `buf` taken when overdubbing begins.
    prev: Vec<f32>,
    /// Recorded length in frames.
    len: usize,
    /// Play/record head, always `0..=len` (or `0..=buf.len()` while recording).
    cursor: usize,
    /// Current transport state.
    state: LoopState,
    /// Whether input is layered while playing (mirrors the control).
    overdub: bool,
    /// Whether `prev` currently holds a recoverable layer.
    have_prev: bool,
    /// Whether the current playback pass has already snapshotted into `prev`.
    snapshotted: bool,
}

impl Looper {
    /// Build a looper at the engine rate and reset the shared control to idle.
    /// Allocates the two fixed buffers, so call this before the streams start.
    pub fn new(sample_rate: f32, ctl: &LooperControl) -> Self {
        let max_len = (MAX_LOOP_SECS * sample_rate).round().max(1.0) as usize;
        ctl.state.store(LoopState::Idle.as_u8(), Relaxed);
        ctl.len.store(0, Relaxed);
        Self {
            buf: vec![0.0; max_len],
            prev: vec![0.0; max_len],
            len: 0,
            cursor: 0,
            state: LoopState::Idle,
            overdub: false,
            have_prev: false,
            snapshotted: false,
        }
    }

    /// Drain the UI's transport commands and publish the resulting state. Called
    /// once per block, before any [`Looper::process`] calls for that block.
    pub fn poll_controls(&mut self, ctl: &LooperControl) {
        if ctl.clear.swap(false, Relaxed) {
            self.clear();
        }
        if ctl.undo.swap(false, Relaxed) {
            self.undo();
        }
        if ctl.record.swap(false, Relaxed) {
            self.record();
        }
        if ctl.stop.swap(false, Relaxed) {
            self.stop();
        }
        self.overdub = ctl.overdub.load(Relaxed);
        self.publish(ctl);
    }

    /// Publish the current state/length to the shared control. Cheap and
    /// lock-free, so it is also called after a block to reflect mid-block changes
    /// (recording growing, auto-stop at max length).
    pub fn publish(&self, ctl: &LooperControl) {
        ctl.state.store(self.state.as_u8(), Relaxed);
        ctl.len.store(self.len as u32, Relaxed);
    }

    /// Advance one frame: capture `input` while recording, emit the loop sample
    /// while playing, or return silence when idle.
    pub fn process(&mut self, input: f32) -> f32 {
        match self.state {
            LoopState::Idle => 0.0,
            LoopState::Recording => {
                if self.cursor < self.buf.len() {
                    self.buf[self.cursor] = if input.is_finite() { input } else { 0.0 };
                    self.cursor += 1;
                    self.len = self.cursor;
                }
                if self.cursor >= self.buf.len() {
                    self.finish_recording();
                }
                0.0
            }
            LoopState::Playing => {
                if self.len == 0 {
                    return 0.0;
                }
                let mut out = self.buf[self.cursor];
                if self.overdub {
                    if !self.snapshotted {
                        self.prev[..self.len].copy_from_slice(&self.buf[..self.len]);
                        self.have_prev = true;
                        self.snapshotted = true;
                    }
                    out = (out + input).clamp(-1.0, 1.0);
                    self.buf[self.cursor] = out;
                }
                self.cursor += 1;
                if self.cursor >= self.len {
                    self.cursor = 0;
                }
                out
            }
        }
    }

    /// The record button. While recording this finalizes the take; while playing
    /// it starts a fresh take; while idle it resumes a held loop or, if empty,
    /// begins recording.
    fn record(&mut self) {
        match self.state {
            LoopState::Recording => self.finish_recording(),
            LoopState::Playing => self.start_recording(),
            LoopState::Idle => {
                if self.len > 0 {
                    self.state = LoopState::Playing;
                    self.cursor = 0;
                    self.snapshotted = false;
                } else {
                    self.start_recording();
                }
            }
        }
    }

    /// The stop button: finalize a take or pause playback, keeping the buffer.
    fn stop(&mut self) {
        match self.state {
            LoopState::Recording => self.finish_recording(),
            LoopState::Playing => {
                self.state = LoopState::Idle;
                self.cursor = 0;
            }
            LoopState::Idle => {}
        }
    }

    /// Arm a new take, discarding the previous length.
    fn start_recording(&mut self) {
        self.state = LoopState::Recording;
        self.len = 0;
        self.cursor = 0;
        self.snapshotted = false;
    }

    /// Stop recording: an empty take returns to idle, otherwise the loop plays
    /// from the top. Also the auto-stop path when the buffer fills.
    fn finish_recording(&mut self) {
        if self.cursor == 0 {
            self.state = LoopState::Idle;
        } else {
            self.len = self.cursor;
            self.state = LoopState::Playing;
            self.cursor = 0;
            self.snapshotted = false;
        }
    }

    /// Discard the loop. The buffer bytes are left in place but `len == 0` keeps
    /// them silent until the next take overwrites them.
    fn clear(&mut self) {
        self.len = 0;
        self.cursor = 0;
        self.state = LoopState::Idle;
        self.have_prev = false;
        self.snapshotted = false;
    }

    /// Restore the layer captured when the first overdub began.
    fn undo(&mut self) {
        if !self.have_prev {
            return;
        }
        self.buf[..self.len].copy_from_slice(&self.prev[..self.len]);
        self.have_prev = false;
        self.snapshotted = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A high sample rate would allocate a 30 s buffer; tests use a tiny rate so
    /// the buffer (and its max length) stay small and fast.
    const SR: f32 = 10.0;

    fn looper() -> (LooperControl, Looper) {
        let ctl = LooperControl::new();
        let l = Looper::new(SR, &ctl);
        (ctl, l)
    }

    #[test]
    fn idle_returns_silence() {
        let (_ctl, mut l) = looper();
        for _ in 0..100 {
            assert_eq!(l.process(0.3), 0.0);
        }
        assert_eq!(l.len, 0);
    }

    #[test]
    fn record_length_is_right() {
        let (_ctl, mut l) = looper();
        l.start_recording();
        for i in 0..7 {
            l.process(i as f32 * 0.1);
        }
        l.finish_recording();
        assert_eq!(l.len, 7);
        assert_eq!(l.state, LoopState::Playing);
    }

    #[test]
    fn cursor_wraps_exactly_at_len() {
        let (_ctl, mut l) = looper();
        l.start_recording();
        for i in 0..5 {
            l.process((i + 1) as f32 * 0.1);
        }
        l.finish_recording();
        assert_eq!(l.len, 5);
        // Ten playback frames are two exact passes of the five-frame loop.
        let out: Vec<f32> = (0..10).map(|_| l.process(0.0)).collect();
        assert_eq!(&out[..5], &out[5..], "loop did not wrap at len");
        assert_eq!(l.cursor, 0);
    }

    #[test]
    fn recording_auto_stops_at_max_length() {
        let (ctl, mut l) = looper();
        // At SR frames/s the buffer holds SR * MAX_LOOP_SECS frames.
        assert_eq!(l.buf.len(), (MAX_LOOP_SECS * SR) as usize);
        ctl.request_record();
        l.poll_controls(&ctl);
        for _ in 0..10_000 {
            l.process(0.2);
        }
        assert_eq!(l.state, LoopState::Playing, "did not auto-stop when full");
        assert_eq!(l.len, l.buf.len());
    }

    #[test]
    fn overdub_accumulates_and_is_bounded() {
        let (_ctl, mut l) = looper();
        l.start_recording();
        for _ in 0..4 {
            l.process(0.5);
        }
        l.finish_recording();

        // First overdub pass: 0.5 + 0.25 = 0.75.
        l.overdub = true;
        let first: Vec<f32> = (0..4).map(|_| l.process(0.25)).collect();
        assert!(first.iter().all(|&x| (x - 0.75).abs() < 1e-6), "{first:?}");

        // Second pass would exceed unity; it clamps.
        let second: Vec<f32> = (0..4).map(|_| l.process(0.9)).collect();
        assert!(second.iter().all(|&x| (x - 1.0).abs() < 1e-6), "{second:?}");
    }

    #[test]
    fn undo_restores_the_previous_layer() {
        let (_ctl, mut l) = looper();
        l.start_recording();
        for _ in 0..4 {
            l.process(0.5);
        }
        l.finish_recording();

        l.overdub = true;
        for _ in 0..4 {
            l.process(0.25);
        }
        assert!(l.buf[..4].iter().all(|&x| (x - 0.75).abs() < 1e-6));

        l.undo();
        assert!(
            l.buf[..4].iter().all(|&x| (x - 0.5).abs() < 1e-6),
            "undo did not restore the pre-overdub layer: {:?}",
            &l.buf[..4]
        );
        // A second undo with nothing captured is a no-op.
        l.undo();
        assert!(l.buf[..4].iter().all(|&x| (x - 0.5).abs() < 1e-6));
    }

    #[test]
    fn control_commands_drive_transport() {
        let (ctl, mut l) = looper();

        ctl.request_record();
        l.poll_controls(&ctl);
        assert_eq!(ctl.state(), LoopState::Recording);

        for _ in 0..10 {
            l.process(0.2);
        }
        ctl.request_stop();
        l.poll_controls(&ctl);
        assert_eq!(ctl.state(), LoopState::Playing);
        assert_eq!(ctl.len(), 10);

        ctl.request_clear();
        l.poll_controls(&ctl);
        assert_eq!(ctl.state(), LoopState::Idle);
        assert!(ctl.is_empty());

        // Record again, finalize, then toggle overdub through the control.
        ctl.request_record();
        l.poll_controls(&ctl);
        for _ in 0..3 {
            l.process(0.1);
        }
        ctl.request_record();
        l.poll_controls(&ctl);
        assert_eq!(ctl.state(), LoopState::Playing);
        assert!(ctl.toggle_overdub());
        l.poll_controls(&ctl);
        assert!(ctl.overdub_enabled());
        assert!(l.overdub);
    }

    #[test]
    fn clear_drops_the_held_layer() {
        let (ctl, mut l) = looper();
        ctl.request_record();
        l.poll_controls(&ctl);
        for _ in 0..6 {
            l.process(0.4);
        }
        ctl.request_stop();
        l.poll_controls(&ctl);
        assert_eq!(l.len, 6);
        assert_eq!(l.state, LoopState::Playing);

        // Pause to idle, then record resumes playback instead of wiping.
        ctl.request_stop();
        l.poll_controls(&ctl);
        assert_eq!(l.state, LoopState::Idle);
        l.record();
        assert_eq!(l.state, LoopState::Playing);

        ctl.request_clear();
        l.poll_controls(&ctl);
        assert_eq!(l.len, 0);
        assert_eq!(l.process(0.5), 0.0);
    }

    /// The hot path indexes fixed buffers only, so processing a long stretch
    /// never grows them.
    #[test]
    fn process_does_not_grow_buffers() {
        let (_ctl, mut l) = looper();
        let cap = l.buf.capacity();
        l.start_recording();
        for i in 0..20_000 {
            l.process((i % 7) as f32 * 0.1);
        }
        assert_eq!(l.buf.capacity(), cap);
        assert_eq!(l.prev.capacity(), cap);
    }
}
