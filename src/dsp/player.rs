//! Multitrack practice playback: mixes a bounded set of decoded stereo/mono
//! tracks into two monitor buses along a shared timeline cursor, with a loop
//! region.
//!
//! Two kinds of row share the transport:
//! - [`TrackKind::Import`] — imported backing/reference audio. Stereo, summed
//!   into the **import bus**, which is monitor-only (never processed by the rig).
//! - [`TrackKind::RawTake`] — dry mono guitar takes. Summed into the **take
//!   bus**, which is processed by its own rig instance (see `crate::audio`).
//!
//! Tracks are decoded, resampled and placed on the timeline off the audio
//! thread; the finished [`PlayerTrack`]s are handed to the realtime callback as
//! boxed values and installed lock-free by [`TrackId`]. Everything here is O(1)
//! per track and allocation-free; the voice never frees a track itself —
//! displaced tracks are shipped back to the control thread for disposal.
//!
//! ## Known limitations
//!
//! - **`MAX_TRACKS` is a hard cap** with a UI message; there is no long-track
//!   streaming. A 10-minute stereo f32 decode is ~230 MB, so eager decoding of
//!   many long files is bounded by this limit.
//! - **The take bus loads its own plugin instances.** CPU and plugin count
//!   roughly double while an external plugin or IR is loaded. That is
//!   deliberate (see [`crate::audio`]'s `take_chain`), not measured — watch CPU
//!   in release builds.
//! - **Take-bus plugin latency is not compensated** between the live and take
//!   buses. Only the built-in-vs-AU compensation inside a single chain exists.

/// A track identifier. Must match [`crate::session::TrackId`]; kept as a plain
/// `u64` so this module has no dependency on the session model.
pub type TrackId = u64;

/// A generation counter paired with a track id so a delayed worker install can
/// be rejected if the row has since been replaced.
pub type Generation = u64;

/// How many concurrent tracks the audio thread can hold.
pub const MAX_TRACKS: usize = 16;

/// What a row's samples feed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TrackKind {
    /// Stereo backing/reference audio → monitor-only import bus.
    Import,
    /// Dry mono guitar take → rig-processed take bus.
    RawTake,
}

/// A decoded, rate-matched track placed on the timeline. Owned by the audio
/// thread once installed.
#[derive(Debug)]
pub struct PlayerTrack {
    pub l: Vec<f32>,
    pub r: Vec<f32>,
    /// Timeline frame where this track begins (project ticks converted to the
    /// engine rate). `0` for a backing track that starts at the top; a take
    /// carries the playhead it was captured against.
    pub start: usize,
}

impl PlayerTrack {
    /// Number of usable frames (channels may differ by one in pathological
    /// files, so take the shorter).
    #[inline]
    pub fn frames(&self) -> usize {
        self.l.len().min(self.r.len())
    }
}

/// One installed row on the audio side.
#[derive(Debug)]
pub struct TrackSlot {
    pub id: TrackId,
    pub generation: Generation,
    pub kind: TrackKind,
    pub track: PlayerTrack,
    pub gain: f32,
    pub muted: bool,
}

/// Why an install was rejected by the audio slot table.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum InstallError {
    /// All [`MAX_TRACKS`] slots are occupied.
    Full,
}

impl std::fmt::Display for InstallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Full => write!(f, "timeline is full ({} track maximum)", MAX_TRACKS),
        }
    }
}

/// A snapshot of the shared transport, read from atomics once per audio callback
/// so the per-sample loop only touches plain fields.
#[derive(Clone, Copy, Default)]
pub struct Transport {
    pub playing: bool,
    /// A pending seek (frames), consumed by [`PlayerVoice::begin`].
    pub seek: Option<usize>,
    pub loop_enabled: bool,
    pub loop_start: usize,
    pub loop_end: usize,
}

/// One timeline frame's contribution from the player, split by destination bus.
#[derive(Clone, Copy, Debug, Default)]
pub struct Frame {
    /// Sum of unmuted raw takes (already gain-scaled), to feed the take-bus rig.
    pub take: f32,
    /// Sum of unmuted imports (gain-scaled), left channel, monitor only.
    pub import_l: f32,
    /// Sum of unmuted imports (gain-scaled), right channel, monitor only.
    pub import_r: f32,
}

/// The audio-thread playback mixer. Holds a fixed table of slots and one shared
/// timeline cursor; every track is indexed against the same cursor, so they stay
/// locked.
pub struct PlayerVoice {
    slots: [Option<TrackSlot>; MAX_TRACKS],
    cursor: usize,
}

impl Default for PlayerVoice {
    fn default() -> Self {
        Self::new()
    }
}

impl PlayerVoice {
    pub fn new() -> Self {
        Self {
            slots: std::array::from_fn(|_| None),
            cursor: 0,
        }
    }

    /// Install a prepared track. Replacing an existing id swaps that slot in
    /// place (preserving its position); otherwise the first free slot is used.
    /// Returns the displaced track, if any, for off-thread disposal.
    pub fn install(
        &mut self,
        id: TrackId,
        generation: Generation,
        kind: TrackKind,
        track: PlayerTrack,
        gain: f32,
        muted: bool,
    ) -> Result<Option<PlayerTrack>, InstallError> {
        let slot = TrackSlot {
            id,
            generation,
            kind,
            track,
            gain,
            muted,
        };
        if let Some(existing) = self
            .slots
            .iter_mut()
            .find(|s| s.as_ref().is_some_and(|slot| slot.id == id))
        {
            let old = existing.replace(slot).map(|s| s.track);
            return Ok(old);
        }
        if let Some(empty) = self.slots.iter_mut().find(|s| s.is_none()) {
            *empty = Some(slot);
            return Ok(None);
        }
        Err(InstallError::Full)
    }

    /// Remove a track by id, returning the slot so its buffers can be dropped
    /// off the audio thread.
    pub fn remove(&mut self, id: TrackId) -> Option<TrackSlot> {
        let idx = self
            .slots
            .iter()
            .position(|s| s.as_ref().is_some_and(|slot| slot.id == id))?;
        self.slots[idx].take()
    }

    /// Update a track's level. Returns `false` if it is not installed.
    pub fn set_gain(&mut self, id: TrackId, gain: f32) -> bool {
        match self
            .slots
            .iter_mut()
            .find(|s| s.as_ref().is_some_and(|slot| slot.id == id))
        {
            Some(Some(slot)) => {
                slot.gain = gain;
                true
            }
            _ => false,
        }
    }

    /// Mute/unmute a track. Returns `false` if it is not installed.
    pub fn set_muted(&mut self, id: TrackId, muted: bool) -> bool {
        match self
            .slots
            .iter_mut()
            .find(|s| s.as_ref().is_some_and(|slot| slot.id == id))
        {
            Some(Some(slot)) => {
                slot.muted = muted;
                true
            }
            _ => false,
        }
    }

    /// Move a track's timeline start (in frames) in place — no re-decode needed,
    /// since the decoded buffer is timeline-relative. Returns `false` if the
    /// track is not installed.
    pub fn set_start(&mut self, id: TrackId, start: usize) -> bool {
        match self
            .slots
            .iter_mut()
            .find(|s| s.as_ref().is_some_and(|slot| slot.id == id))
        {
            Some(Some(slot)) => {
                slot.track.start = start;
                true
            }
            _ => false,
        }
    }

    pub fn contains(&self, id: TrackId) -> bool {
        self.slots
            .iter()
            .any(|s| s.as_ref().is_some_and(|slot| slot.id == id))
    }

    pub fn track_count(&self) -> usize {
        self.slots.iter().filter(|s| s.is_some()).count()
    }

    /// The shared timeline cursor, in frames. The caller stores this for the UI.
    #[inline]
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Apply a block-level seek. Call once per callback before the sample loop.
    #[inline]
    pub fn begin(&mut self, t: &Transport) {
        if let Some(pos) = t.seek {
            self.cursor = pos;
        }
    }

    /// Produce the next frame's per-bus contribution and advance the cursor by
    /// one. Returns silence while stopped, so the rig can be played over a
    /// paused timeline.
    #[inline]
    pub fn next_frame(&mut self, t: &Transport) -> Frame {
        if !t.playing {
            return Frame::default();
        }
        // Loop wrap: only meaningful for a well-formed region.
        if t.loop_enabled && t.loop_end > t.loop_start && self.cursor >= t.loop_end {
            self.cursor = t.loop_start;
        }
        let mut out = Frame::default();
        for slot in self.slots.iter().flatten() {
            if slot.muted {
                continue;
            }
            let (l, r) = track_frame(&slot.track, self.cursor, slot.gain);
            match slot.kind {
                TrackKind::RawTake => out.take += 0.5 * (l + r),
                TrackKind::Import => {
                    out.import_l += l;
                    out.import_r += r;
                }
            }
        }
        self.cursor = self.cursor.saturating_add(1);
        out
    }
}

/// Read one timeline frame from `track` (accounting for its start offset), or
/// silence when out of range / absent. Gain is applied by the caller.
#[inline]
fn track_frame(track: &PlayerTrack, cursor: usize, gain: f32) -> (f32, f32) {
    let Some(idx) = cursor.checked_sub(track.start) else {
        return (0.0, 0.0);
    };
    if idx >= track.frames() {
        return (0.0, 0.0);
    }
    (track.l[idx] * gain, track.r[idx] * gain)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(n: usize, start: usize) -> PlayerTrack {
        PlayerTrack {
            l: (0..n).map(|i| i as f32).collect(),
            r: (0..n).map(|i| -(i as f32)).collect(),
            start,
        }
    }

    fn install_import(v: &mut PlayerVoice, id: TrackId, t: PlayerTrack) {
        let _ = v.install(id, 0, TrackKind::Import, t, 1.0, false);
    }

    fn install_take(v: &mut PlayerVoice, id: TrackId, t: PlayerTrack) {
        let _ = v.install(id, 0, TrackKind::RawTake, t, 1.0, false);
    }

    /// A mono take: both channels identical, so the take-bus sum is the signal.
    fn mono_track(n: usize, start: usize) -> PlayerTrack {
        let v: Vec<f32> = (0..n).map(|i| i as f32).collect();
        PlayerTrack {
            r: v.clone(),
            l: v,
            start,
        }
    }

    #[test]
    fn stopped_is_silent_and_does_not_advance() {
        let mut v = PlayerVoice::new();
        install_import(&mut v, 1, track(100, 0));
        let t = Transport::default(); // playing = false
        for _ in 0..10 {
            let f = v.next_frame(&t);
            assert_eq!((f.take, f.import_l, f.import_r), (0.0, 0.0, 0.0));
        }
        assert_eq!(v.cursor(), 0);
    }

    #[test]
    fn plays_and_advances_in_order() {
        let mut v = PlayerVoice::new();
        install_import(&mut v, 1, track(100, 0));
        let t = Transport {
            playing: true,
            ..Transport::default()
        };
        assert_eq!(v.next_frame(&t).import_l, 0.0);
        assert_eq!(v.next_frame(&t).import_l, 1.0);
        assert_eq!(v.next_frame(&t).import_l, 2.0);
        assert_eq!(v.cursor(), 3);
    }

    #[test]
    fn seek_jumps_the_cursor() {
        let mut v = PlayerVoice::new();
        install_import(&mut v, 1, track(100, 0));
        let t = Transport {
            playing: true,
            seek: Some(10),
            ..Transport::default()
        };
        v.begin(&t);
        assert_eq!(v.next_frame(&t).import_l, 10.0);
        assert_eq!(v.cursor(), 11);
    }

    #[test]
    fn take_and_import_feed_separate_buses() {
        let mut v = PlayerVoice::new();
        install_take(&mut v, 1, track(100, 0));
        install_import(&mut v, 2, track(100, 0));
        let t = Transport {
            playing: true,
            ..Transport::default()
        };
        let f = v.next_frame(&t);
        // Take is the mono sum of the (l, r) pair; import keeps its channels.
        assert_eq!(f.take, 0.0);
        assert_eq!((f.import_l, f.import_r), (0.0, -0.0));
        let f = v.next_frame(&t);
        assert_eq!(f.take, 0.0, "l + r cancel for this synthetic track");
        assert_eq!((f.import_l, f.import_r), (1.0, -1.0));
    }

    #[test]
    fn loop_region_wraps_back() {
        let mut v = PlayerVoice::new();
        install_import(&mut v, 1, track(100, 0));
        let t = Transport {
            playing: true,
            loop_enabled: true,
            loop_start: 4,
            loop_end: 6,
            ..Transport::default()
        };
        let got: Vec<f32> = (0..6).map(|_| v.next_frame(&t).import_l).collect();
        assert_eq!(got, vec![0.0, 1.0, 2.0, 3.0, 4.0, 5.0]);
        assert_eq!(
            v.next_frame(&t).import_l,
            4.0,
            "cursor must wrap to loop_start"
        );
        assert_eq!(v.next_frame(&t).import_l, 5.0);
    }

    #[test]
    fn start_offset_aligns_a_take() {
        let mut v = PlayerVoice::new();
        install_take(&mut v, 1, mono_track(3, 5));
        let t = Transport {
            playing: true,
            seek: Some(5),
            ..Transport::default()
        };
        v.begin(&t);
        assert_eq!(v.next_frame(&t).take, 0.0); // record[0]
        assert_eq!(v.next_frame(&t).take, 1.0); // record[1]
        assert_eq!(v.next_frame(&t).take, 2.0); // record[2]
        assert_eq!(v.next_frame(&t).take, 0.0); // past the take's end
    }

    #[test]
    fn mute_silences_but_still_advances() {
        let mut v = PlayerVoice::new();
        install_import(&mut v, 1, track(100, 0));
        assert!(v.set_muted(1, true));
        let t = Transport {
            playing: true,
            ..Transport::default()
        };
        assert_eq!(v.next_frame(&t).import_l, 0.0);
        assert_eq!(v.cursor(), 1, "a muted track still advances the timeline");
    }

    #[test]
    fn gain_scales_the_output() {
        let mut v = PlayerVoice::new();
        install_import(&mut v, 1, track(100, 0));
        assert!(v.set_gain(1, 0.5));
        let t = Transport {
            playing: true,
            seek: Some(3),
            ..Transport::default()
        };
        v.begin(&t);
        assert_eq!(v.next_frame(&t).import_l, 1.5);
    }

    #[test]
    fn displaced_track_is_returned_for_off_thread_drop() {
        let mut v = PlayerVoice::new();
        assert!(
            v.install(1, 0, TrackKind::Import, track(1, 0), 1.0, false)
                .is_ok_and(|d| d.is_none())
        );
        let old = v.install(1, 1, TrackKind::Import, track(1, 0), 1.0, false);
        assert!(
            old.is_ok_and(|d| d.is_some()),
            "the displaced track must be handed back"
        );
    }

    #[test]
    fn set_start_realigns_a_track_without_redecoding() {
        let mut v = PlayerVoice::new();
        let samples: Vec<f32> = (0..3).map(|i| 10.0 + i as f32).collect();
        let take = PlayerTrack {
            r: samples.clone(),
            l: samples,
            start: 0,
        };
        install_take(&mut v, 1, take);

        let at0 = Transport {
            playing: true,
            seek: Some(0),
            ..Transport::default()
        };
        v.begin(&at0);
        assert_eq!(v.next_frame(&at0).take, 10.0);

        // Move the clip to frame 5 and confirm it is silent before then.
        assert!(v.set_start(1, 5));
        let at0 = Transport {
            playing: true,
            seek: Some(0),
            ..Transport::default()
        };
        v.begin(&at0);
        assert_eq!(
            v.next_frame(&at0).take,
            0.0,
            "clip must not play before its start"
        );
        let at5 = Transport {
            playing: true,
            seek: Some(5),
            ..Transport::default()
        };
        v.begin(&at5);
        assert_eq!(v.next_frame(&at5).take, 10.0, "clip plays at its new start");
    }

    #[test]
    fn removing_a_track_returns_its_slot() {
        let mut v = PlayerVoice::new();
        install_import(&mut v, 7, track(1, 0));
        assert!(v.contains(7));
        let removed = v.remove(7);
        assert!(removed.is_some());
        assert!(!v.contains(7));
        assert!(v.remove(7).is_none());
    }

    #[test]
    fn a_full_table_rejects_installs() {
        let mut v = PlayerVoice::new();
        for id in 0..MAX_TRACKS as TrackId {
            assert!(
                v.install(id, 0, TrackKind::Import, track(1, 0), 1.0, false)
                    .is_ok()
            );
        }
        let err = v.install(999, 0, TrackKind::Import, track(1, 0), 1.0, false);
        assert!(matches!(err, Err(InstallError::Full)));
    }
}
