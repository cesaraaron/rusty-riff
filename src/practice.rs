//! Practice transport bridge and offline track decoding.
//!
//! [`Practice`] is the lock-free handshake between the UI thread (which writes
//! the transport and hands over decoded tracks) and the audio thread (which
//! reads a [`Transport`] snapshot per callback and mixes every installed track).
//! It holds only the shared transport — the canonical track list lives in
//! [`crate::session::Session`], and decoded buffers are caches installed into
//! bounded audio slots (see [`crate::audio`]).
//!
//! Decoding a backing file or a recorded take happens here, entirely off the
//! audio thread; the finished [`PlayerTrack`] is installed into the engine
//! lock-free.

use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering::Relaxed};

use anyhow::{Context, Result, anyhow};
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::DecoderOptions;
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

use crate::dsp::player::{PlayerTrack, Transport};
use crate::dsp::resample::resample;

/// Shared practice transport. The UI writes every field except `position`; the
/// audio thread reads them and writes `position`.
pub struct Practice {
    /// UI → audio: whether the timeline is playing.
    pub playing: AtomicBool,
    /// Audio → UI: current timeline frame.
    pub position: AtomicU64,
    /// UI → audio: a pending seek in frames; `-1` means none. Consumed on read.
    pub seek: AtomicI64,
    pub loop_enabled: AtomicBool,
    pub loop_start: AtomicU64,
    pub loop_end: AtomicU64,
}

impl Default for Practice {
    fn default() -> Self {
        Self::new()
    }
}

impl Practice {
    pub fn new() -> Self {
        Self {
            playing: AtomicBool::new(false),
            position: AtomicU64::new(0),
            seek: AtomicI64::new(-1),
            loop_enabled: AtomicBool::new(false),
            loop_start: AtomicU64::new(0),
            loop_end: AtomicU64::new(0),
        }
    }

    /// Read a consistent-enough transport snapshot for one audio callback,
    /// consuming any pending seek. All loads are relaxed: the worst case is one
    /// buffer of staleness on a knob move, which is inaudible.
    pub fn snapshot(&self) -> Transport {
        let seek = self.seek.swap(-1, Relaxed);
        Transport {
            playing: self.playing.load(Relaxed),
            seek: if seek >= 0 { Some(seek as usize) } else { None },
            loop_enabled: self.loop_enabled.load(Relaxed),
            loop_start: self.loop_start.load(Relaxed) as usize,
            loop_end: self.loop_end.load(Relaxed) as usize,
        }
    }

    /// Clear the transport back to its startup state. Called when a fresh engine
    /// starts (including a device change); the session itself is preserved by
    /// the caller and its tracks are re-installed.
    pub fn reset(&self) {
        self.playing.store(false, Relaxed);
        self.position.store(0, Relaxed);
        self.seek.store(-1, Relaxed);
        self.loop_enabled.store(false, Relaxed);
        self.loop_start.store(0, Relaxed);
        self.loop_end.store(0, Relaxed);
    }

    pub fn store_position(&self, frame: usize) {
        self.position.store(frame as u64, Relaxed);
    }

    pub fn position(&self) -> usize {
        self.position.load(Relaxed) as usize
    }

    pub fn request_seek(&self, frame: usize) {
        self.seek.store(frame as i64, Relaxed);
    }
}

/// A decoded track plus the source metadata needed to describe its asset.
#[derive(Debug)]
pub struct DecodedTrack {
    pub track: PlayerTrack,
    /// Sample rate of the source file before rate matching.
    pub source_sample_rate: u32,
    /// Channel count of the source file as decoded.
    pub source_channels: u16,
}

/// Decode an audio file (MP3 / WAV / FLAC) to a stereo [`PlayerTrack`] at the
/// engine rate, starting at timeline frame 0. Runs off the audio thread — file
/// IO, decode and an offline resample.
pub fn decode_track(path: impl AsRef<Path>, target_sr: f32) -> Result<DecodedTrack> {
    let path = path.as_ref();
    let file = std::fs::File::open(path)
        .with_context(|| format!("opening audio file {}", path.display()))?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());

    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }

    let probed = symphonia::default::get_probe()
        .format(
            &hint,
            mss,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .with_context(|| format!("unsupported or corrupt audio file {}", path.display()))?;
    let mut format = probed.format;

    let track = format
        .default_track()
        .ok_or_else(|| anyhow!("audio file has no playable track"))?;
    let track_id = track.id;
    let src_sr = track.codec_params.sample_rate.unwrap_or(44_100) as f32;

    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .with_context(|| format!("no decoder for audio file {}", path.display()))?;

    let mut sample_buf: Option<SampleBuffer<f32>> = None;
    let mut l: Vec<f32> = Vec::new();
    let mut r: Vec<f32> = Vec::new();
    let mut source_channels: u16 = 1;

    loop {
        let packet = match format.next_packet() {
            Ok(packet) => packet,
            // Normal end of stream.
            Err(SymphoniaError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                break;
            }
            Err(e) => return Err(e).context("reading audio packet"),
        };
        if packet.track_id() != track_id {
            continue;
        }
        match decoder.decode(&packet) {
            Ok(decoded) => {
                let spec = *decoded.spec();
                let capacity = decoded.capacity();
                source_channels = spec.channels.count().max(1) as u16;
                // (Re)allocate the scratch buffer only when the codec asks for more.
                if sample_buf.as_ref().is_none_or(|b| b.capacity() < capacity) {
                    sample_buf = Some(SampleBuffer::<f32>::new(capacity as u64, spec));
                }
                let buf = sample_buf
                    .as_mut()
                    .ok_or_else(|| anyhow!("sample buffer missing"))?;
                buf.copy_interleaved_ref(decoded);

                let channels = spec.channels.count().max(1);
                for frame in buf.samples().chunks(channels) {
                    let c0 = frame.first().copied().unwrap_or(0.0);
                    let c1 = frame.get(1).copied().unwrap_or(c0);
                    l.push(c0);
                    r.push(c1);
                }
            }
            // A corrupt packet is skipped rather than aborting the whole load.
            Err(SymphoniaError::DecodeError(_)) => continue,
            Err(e) => return Err(e).context("decoding audio packet"),
        }
    }

    if l.is_empty() {
        return Err(anyhow!("audio file decoded to no samples"));
    }

    if (src_sr - target_sr).abs() > 0.5 {
        let ratio = target_sr / src_sr;
        l = resample(&l, ratio);
        r = resample(&r, ratio);
    }

    Ok(DecodedTrack {
        track: PlayerTrack { l, r, start: 0 },
        source_sample_rate: src_sr as u32,
        source_channels,
    })
}

/// Peak-envelope resolution for a track of `frames` at `sample_rate`: roughly
/// 500 buckets per second (2 ms), clamped to a sane memory range. Fine enough to
/// stay sharp when the timeline is zoomed to a sub-second window.
pub fn peak_buckets(frames: usize, sample_rate: f32) -> usize {
    let per_bucket = (sample_rate / 500.0).max(1.0);
    ((frames as f32 / per_bucket).round() as usize).clamp(2_048, 262_144)
}

/// Peak envelope over `buckets` buckets: (min, max) of the mono sum per bucket.
/// Runs off the audio thread.
pub fn peaks(track: &PlayerTrack, buckets: usize) -> Vec<(f32, f32)> {
    let n = track.frames();
    if n == 0 || buckets == 0 {
        return Vec::new();
    }
    let mut out = Vec::with_capacity(buckets);
    for b in 0..buckets {
        let start = b * n / buckets;
        let end = (((b + 1) * n) / buckets).max(start + 1).min(n);
        let (mut lo, mut hi) = (0.0f32, 0.0f32);
        for i in start..end {
            let s = 0.5 * (track.l[i] + track.r[i]);
            lo = lo.min(s);
            hi = hi.max(s);
        }
        out.push((lo, hi));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seek_is_consumed_once() {
        let p = Practice::new();
        assert_eq!(p.snapshot().seek, None);
        p.request_seek(1234);
        assert_eq!(p.snapshot().seek, Some(1234));
        assert_eq!(p.snapshot().seek, None, "a seek must not fire twice");
    }

    #[test]
    fn reset_clears_the_transport() {
        let p = Practice::new();
        p.playing.store(true, Relaxed);
        p.request_seek(10);
        p.loop_enabled.store(true, Relaxed);
        p.reset();
        let t = p.snapshot();
        assert!(!t.playing);
        assert_eq!(t.seek, None);
        assert!(!t.loop_enabled);
        assert_eq!(p.position(), 0);
    }

    #[test]
    fn peak_buckets_scales_with_length_and_is_clamped() {
        // ~500 buckets/s, with a floor and a memory cap.
        assert_eq!(peak_buckets(96_000, 48_000.0), 2_048); // 2 s → floor
        assert_eq!(peak_buckets(2_880_000, 48_000.0), 30_000); // 1 min
        assert_eq!(peak_buckets(48_000 * 60 * 60 * 10, 48_000.0), 262_144); // cap
    }

    #[test]
    fn decode_rejects_a_missing_file() {
        let err = decode_track("/nonexistent/rusty-riff-nope.mp3", 48_000.0).unwrap_err();
        assert!(
            err.to_string().contains("opening"),
            "unexpected error: {err}"
        );
    }

    /// Regression: our own dry captures are 32-bit float WAVs. The `wav` feature
    /// alone is only the RIFF *reader*; without the `pcm` codec feature every WAV
    /// fails to decode. This guards the Cargo feature set.
    #[test]
    fn decodes_a_32bit_float_wav() {
        let path = std::env::temp_dir().join(format!(
            "rusty-riff-decode-float-{}.wav",
            std::process::id()
        ));
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 48_000,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        };
        let mut w = hound::WavWriter::create(&path, spec).expect("wav");
        for i in 0..256 {
            w.write_sample((i as f32 * 0.001).sin()).expect("sample");
        }
        w.finalize().expect("finalize");

        let decoded = decode_track(&path, 48_000.0).expect("decode float wav");
        assert_eq!(decoded.track.frames(), 256);
        let _ = std::fs::remove_file(&path);
    }
}
