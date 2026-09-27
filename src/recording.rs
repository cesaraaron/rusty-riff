//! Dry raw-take capture.
//!
//! The audio callback must never allocate, block, or touch the filesystem, so a
//! capture is split between three pieces:
//!
//! - [`CaptureState`] — the shared atomics the UI uses to arm/disarm a take and
//!   read its progress.
//! - An `rtrb` SPSC ring ([`CaptureRing`]) the callback pushes dry input samples
//!   into, one per project frame.
//! - A writer worker thread ([`spawn_capture_worker`]) that owns the ring's
//!   consumer, drains it into a mono 32-bit float WAV under the recovery
//!   directory, computes display peaks, and hands a ready [`PlayerTrack`] back to
//!   the UI over an `mpsc` channel.
//!
//! The captured signal is the **dry selected input channel**, taken before the
//! gate/pedals/amp/cab so a finished take can later be re-amped by the current
//! rig. Imports, clicks and existing FX returns are monitor-only and can never
//! reach this ring.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering::Relaxed};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rtrb::{Consumer, Producer, RingBuffer};

use crate::dsp::player::PlayerTrack;
use crate::practice::{peak_buckets, peaks};

/// How many seconds of dry samples the ring can hold before the writer has to
/// catch up. Generous: the worker only runs during a take.
const CAPTURE_RING_SECONDS: usize = 2;

/// Shared capture flags. Cloned (`Arc`) into both the audio thread and the UI.
pub struct CaptureState {
    /// UI → audio: push samples to the capture ring.
    pub active: AtomicBool,
    /// Audio → UI: a valid loop out-point was reached; stop and finalize.
    pub auto_stop: AtomicBool,
    /// Identifies the current take so a late result can be discarded.
    pub generation: AtomicU64,
    /// Audio → UI: timeline frame of the first captured sample.
    pub start_frame: AtomicU64,
    /// Audio → UI: samples pushed so far (progress display).
    pub frames: AtomicU64,
    /// Audio → UI: the ring filled and samples were dropped; take incomplete.
    pub overflowed: AtomicBool,
    /// Engine sample rate for the take.
    pub sample_rate: AtomicU32,
}

impl Default for CaptureState {
    fn default() -> Self {
        Self::new()
    }
}

impl CaptureState {
    pub fn new() -> Self {
        Self {
            active: AtomicBool::new(false),
            auto_stop: AtomicBool::new(false),
            generation: AtomicU64::new(0),
            start_frame: AtomicU64::new(0),
            frames: AtomicU64::new(0),
            overflowed: AtomicBool::new(false),
            sample_rate: AtomicU32::new(44_100),
        }
    }

    pub fn arm(&self, generation: u64) {
        self.generation.store(generation, Relaxed);
        self.start_frame.store(0, Relaxed);
        self.frames.store(0, Relaxed);
        self.overflowed.store(false, Relaxed);
        self.auto_stop.store(false, Relaxed);
        self.active.store(true, Relaxed);
    }

    /// Stop the audio side pushing immediately. The caller then asks the writer
    /// worker to finalize.
    pub fn disarm(&self) {
        self.active.store(false, Relaxed);
    }
}

/// Create the capture sample ring. Returns `(producer_for_audio, consumer_for_worker)`.
pub fn capture_ring(sample_rate: f32) -> (Producer<f32>, Consumer<f32>) {
    let cap = (sample_rate.max(1.0) as usize)
        .saturating_mul(CAPTURE_RING_SECONDS)
        .max(1024);
    RingBuffer::<f32>::new(cap)
}

/// Bucketed min/max feed published by the capture writer while a take records,
/// so the timeline can draw a growing waveform instead of waiting for finalize.
///
/// Touched only by the writer worker (producer) and the UI thread (reader) — the
/// audio callback never sees it. The UI reads through `try_lock` so a busy
/// writer can never stall a redraw.
pub struct LivePeaks {
    buckets: Vec<(f32, f32)>,
    samples_per_bucket: usize,
    cur_min: f32,
    cur_max: f32,
    cur_count: usize,
}

impl LivePeaks {
    /// ~2 ms per bucket: fine enough for the zoomed timeline, and bounded memory
    /// for a long take.
    pub fn new(sample_rate: u32) -> Self {
        let samples_per_bucket = (sample_rate as usize / 500).max(1);
        Self {
            buckets: Vec::new(),
            samples_per_bucket,
            cur_min: f32::INFINITY,
            cur_max: f32::NEG_INFINITY,
            cur_count: 0,
        }
    }

    /// Fold one sample into the running bucket.
    pub fn push(&mut self, sample: f32) {
        self.cur_min = self.cur_min.min(sample);
        self.cur_max = self.cur_max.max(sample);
        self.cur_count += 1;
        if self.cur_count >= self.samples_per_bucket {
            self.buckets.push((self.cur_min, self.cur_max));
            self.cur_min = f32::INFINITY;
            self.cur_max = f32::NEG_INFINITY;
            self.cur_count = 0;
        }
    }

    /// Completed buckets; the in-progress tail is not included.
    pub fn peaks(&self) -> &[(f32, f32)] {
        &self.buckets
    }

    /// Samples consumed so far, including the in-progress bucket.
    pub fn frames(&self) -> usize {
        self.buckets.len() * self.samples_per_bucket + self.cur_count
    }
}

/// A finished capture, sent from the writer worker back to the UI.
pub struct CaptureResult {
    pub generation: u64,
    pub path: PathBuf,
    pub frames: usize,
    pub overflowed: bool,
    pub peaks: Vec<(f32, f32)>,
    /// `None` when the take was empty, aborted, or failed.
    pub track: Option<PlayerTrack>,
    pub error: Option<String>,
}

/// Commands the UI sends to the writer worker.
pub enum CaptureCommand {
    /// Start writing a new take to `path`. The worker replies on `result` and
    /// publishes a growing waveform through `live` while it records.
    Begin {
        generation: u64,
        path: PathBuf,
        sample_rate: u32,
        result: Sender<CaptureResult>,
        live: Arc<Mutex<LivePeaks>>,
    },
    /// Flush and finalize the active take.
    End,
    /// Discard the active take and remove its partial file.
    Abort,
}

/// Spawn the writer worker and return the command sender.
pub fn spawn_capture_worker(mut consumer: Consumer<f32>) -> Sender<CaptureCommand> {
    let (tx, rx) = mpsc::channel::<CaptureCommand>();
    std::thread::spawn(move || worker_loop(&mut consumer, &rx));
    tx
}

fn worker_loop(consumer: &mut Consumer<f32>, cmds: &Receiver<CaptureCommand>) {
    loop {
        let Ok(cmd) = cmds.recv() else {
            // The UI dropped the sender (engine teardown); exit.
            return;
        };
        match cmd {
            CaptureCommand::Begin {
                generation,
                path,
                sample_rate,
                result,
                live,
            } => run_capture(
                consumer,
                cmds,
                generation,
                path,
                sample_rate,
                &result,
                &live,
            ),
            CaptureCommand::End | CaptureCommand::Abort => {}
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn run_capture(
    consumer: &mut Consumer<f32>,
    cmds: &Receiver<CaptureCommand>,
    generation: u64,
    path: PathBuf,
    sample_rate: u32,
    result: &Sender<CaptureResult>,
    live: &Arc<Mutex<LivePeaks>>,
) {
    let mut reply = CaptureResult {
        generation,
        path: path.clone(),
        frames: 0,
        overflowed: false,
        peaks: Vec::new(),
        track: None,
        error: None,
    };

    if let Some(parent) = path.parent()
        && let Err(e) = std::fs::create_dir_all(parent)
    {
        reply.error = Some(format!("creating capture dir {}: {e}", parent.display()));
        let _ = result.send(reply);
        return;
    }

    let spec = hound::WavSpec {
        channels: 1,
        sample_rate,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    let writer = match hound::WavWriter::create(&path, spec) {
        Ok(w) => w,
        Err(e) => {
            reply.error = Some(format!("creating capture file {}: {e}", path.display()));
            let _ = result.send(reply);
            return;
        }
    };
    let mut writer = Some(writer);

    let mut samples: Vec<f32> = Vec::new();
    let mut aborted = false;
    loop {
        {
            let mut live = live.lock().unwrap_or_else(|e| e.into_inner());
            while let Ok(s) = consumer.pop() {
                if let Some(w) = writer.as_mut()
                    && w.write_sample(s).is_err()
                {
                    reply.error = Some("writing capture samples failed".to_owned());
                    aborted = true;
                    break;
                }
                live.push(s);
                samples.push(s);
            }
        }
        if aborted {
            break;
        }
        match cmds.try_recv() {
            Ok(CaptureCommand::End) => {
                // Drain whatever the callback left in the ring, then finalize.
                let mut live = live.lock().unwrap_or_else(|e| e.into_inner());
                while let Ok(s) = consumer.pop() {
                    if let Some(w) = writer.as_mut()
                        && w.write_sample(s).is_err()
                    {
                        reply.error = Some("writing capture samples failed".to_owned());
                        break;
                    }
                    live.push(s);
                    samples.push(s);
                }
                break;
            }
            Ok(CaptureCommand::Abort) => {
                aborted = true;
                break;
            }
            // A new Begin while one is active is a protocol error; ignore it so
            // the active take stays intact.
            Ok(CaptureCommand::Begin { result: late, .. }) => {
                drop(late);
            }
            Err(TryRecvError::Empty) => std::thread::sleep(Duration::from_millis(1)),
            Err(TryRecvError::Disconnected) => {
                aborted = true;
                break;
            }
        }
    }

    if let Some(w) = writer.take()
        && let Err(e) = w.finalize()
        && reply.error.is_none()
    {
        reply.error = Some(format!("finalizing capture file: {e}"));
    }

    if aborted || reply.error.is_some() {
        let _ = std::fs::remove_file(&path);
        let _ = result.send(reply);
        return;
    }

    reply.frames = samples.len();
    if samples.is_empty() {
        let _ = std::fs::remove_file(&path);
        let _ = result.send(reply);
        return;
    }

    // A mono take is stored as a stereo track with identical channels so the
    // rest of the playback path is unchanged; the take bus sums it back to mono.
    let l = samples.clone();
    let track = PlayerTrack {
        r: samples,
        start: 0,
        l,
    };
    reply.peaks = peaks(&track, peak_buckets(reply.frames, sample_rate as f32));
    reply.track = Some(track);
    let _ = result.send(reply);
}

/// Convenience bundle shared with the engine at startup.
pub type SharedCapture = Arc<CaptureState>;

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("rusty-riff-capture-test-{name}.wav"))
    }

    #[test]
    fn writer_captures_and_finalizes_a_take() {
        let (mut producer, consumer) = capture_ring(48_000.0);
        let tx = spawn_capture_worker(consumer);
        let (result_tx, result_rx) = mpsc::channel();
        let path = temp_path("ok");
        let _ = std::fs::remove_file(&path);

        tx.send(CaptureCommand::Begin {
            generation: 7,
            path: path.clone(),
            sample_rate: 48_000,
            result: result_tx,
            live: Arc::new(Mutex::new(LivePeaks::new(48_000))),
        })
        .ok();

        for i in 0..1000 {
            let _ = producer.push((i as f32 / 1000.0).sin());
        }
        tx.send(CaptureCommand::End).ok();

        let res = result_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("worker must reply");
        assert!(res.error.is_none(), "unexpected error: {:?}", res.error);
        assert_eq!(res.frames, 1000);
        assert!(res.track.is_some());
        assert!(path.exists());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn abort_removes_the_partial_file() {
        let (mut producer, consumer) = capture_ring(48_000.0);
        let tx = spawn_capture_worker(consumer);
        let (result_tx, result_rx) = mpsc::channel();
        let path = temp_path("abort");
        let _ = std::fs::remove_file(&path);

        tx.send(CaptureCommand::Begin {
            generation: 1,
            path: path.clone(),
            sample_rate: 48_000,
            result: result_tx,
            live: Arc::new(Mutex::new(LivePeaks::new(48_000))),
        })
        .ok();
        let _ = producer.push(1.0);
        tx.send(CaptureCommand::Abort).ok();

        let res = result_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("worker must reply");
        assert!(res.track.is_none());
        assert!(!path.exists());
    }

    #[test]
    fn empty_take_yields_no_track() {
        let (_producer, consumer) = capture_ring(48_000.0);
        let tx = spawn_capture_worker(consumer);
        let (result_tx, result_rx) = mpsc::channel();
        let path = temp_path("empty");
        let _ = std::fs::remove_file(&path);

        tx.send(CaptureCommand::Begin {
            generation: 2,
            path: path.clone(),
            sample_rate: 48_000,
            result: result_tx,
            live: Arc::new(Mutex::new(LivePeaks::new(48_000))),
        })
        .ok();
        tx.send(CaptureCommand::End).ok();

        let res = result_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("worker must reply");
        assert_eq!(res.frames, 0);
        assert!(res.track.is_none());
        assert!(!path.exists());
    }

    #[test]
    fn live_peaks_bucket_and_count() {
        // 48 kHz → 96 samples per ~2 ms bucket.
        let mut lp = LivePeaks::new(48_000);
        assert_eq!(lp.frames(), 0);
        assert!(lp.peaks().is_empty());

        for _ in 0..96 {
            lp.push(0.5);
        }
        assert_eq!(lp.peaks().len(), 1);
        assert_eq!(lp.frames(), 96);
        assert_eq!(lp.peaks()[0], (0.5, 0.5));

        // A partial bucket advances the sample count but not the bucket list yet.
        for _ in 0..48 {
            lp.push(-0.25);
        }
        assert_eq!(lp.peaks().len(), 1, "tail is not a completed bucket");
        assert_eq!(lp.frames(), 144);
    }
}
