//! Tap-tempo: turn a series of key taps into a tempo (BPM).
//!
//! Kept as a tiny pure state machine (no clock of its own — the caller passes an
//! [`Instant`]) so the interval logic is unit-testable. The UI feeds each tap and
//! writes the resulting delay time.

use std::time::{Duration, Instant};

/// Shortest gap between taps that counts (faster than 300 BPM is a misfire).
const MIN_GAP: Duration = Duration::from_millis(180);
/// Longest gap that still counts as "the same tempo" (~30 BPM); a slower tap
/// starts a fresh count rather than averaging in a half-time interval.
const MAX_GAP: Duration = Duration::from_millis(2000);

/// Tracks taps and reports the implied tempo.
#[derive(Default)]
pub struct TapTempo {
    last: Option<Instant>,
    bpm: Option<f32>,
}

impl TapTempo {
    pub const fn new() -> Self {
        Self {
            last: None,
            bpm: None,
        }
    }

    /// The last tempo derived from taps, if any.
    pub fn bpm(&self) -> Option<f32> {
        self.bpm
    }

    /// Register a tap at `now`. Returns the current BPM estimate after this tap
    /// (the previous estimate if this tap only restarts the count).
    pub fn tap(&mut self, now: Instant) -> Option<f32> {
        if let Some(last) = self.last {
            let gap = now.duration_since(last);
            if (MIN_GAP..=MAX_GAP).contains(&gap) {
                let bpm = 60.0 / gap.as_secs_f32();
                self.bpm = Some(bpm);
            } else if gap > MAX_GAP {
                // Too slow — treat as the first tap of a new count.
                self.bpm = None;
            }
            // A too-fast tap is ignored (keeps `last` so the next real tap works).
            if gap >= MIN_GAP {
                self.last = Some(now);
            }
        } else {
            self.last = Some(now);
        }
        self.bpm
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_tempo_on_a_single_tap() {
        let mut t = TapTempo::new();
        assert_eq!(t.tap(Instant::now()), None);
        assert_eq!(t.bpm(), None);
    }

    #[test]
    fn steady_taps_give_the_tempo() {
        let mut t = TapTempo::new();
        let t0 = Instant::now();
        assert_eq!(t.tap(t0), None);
        // 500 ms apart → 120 BPM.
        let bpm = t.tap(t0 + Duration::from_millis(500)).unwrap();
        assert!((bpm - 120.0).abs() < 0.5, "got {bpm}");
        // Another 500 ms tap keeps it at 120.
        let bpm = t.tap(t0 + Duration::from_millis(1000)).unwrap();
        assert!((bpm - 120.0).abs() < 0.5, "got {bpm}");
    }

    #[test]
    fn too_fast_taps_are_ignored() {
        let mut t = TapTempo::new();
        let t0 = Instant::now();
        t.tap(t0);
        t.tap(t0 + Duration::from_millis(500)); // 120 BPM
        // A bounce 50 ms later must not register as a 1200 BPM tempo.
        let bpm = t.tap(t0 + Duration::from_millis(550)).unwrap();
        assert!((bpm - 120.0).abs() < 0.5, "got {bpm}");
    }

    #[test]
    fn a_slow_tap_restarts_the_count() {
        let mut t = TapTempo::new();
        let t0 = Instant::now();
        t.tap(t0);
        t.tap(t0 + Duration::from_millis(500)); // 120 BPM
        assert_eq!(t.tap(t0 + Duration::from_millis(4000)), None); // reset
        assert_eq!(t.bpm(), None);
    }
}
