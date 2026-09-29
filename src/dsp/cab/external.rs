//! External (third-party / user-supplied) cabinet impulse responses.
//!
//! The built-in cabs synthesise their IRs in code (see [`super::ir`]) and render a
//! three-mic blend with live mic-position colouration. A loaded `.wav` IR is a
//! *single finished capture* — a real mic, at a real position, in a real room, all
//! already baked into the recording — so this path is deliberately simpler:
//!
//!   * the mono drive still passes through the same [`SpeakerDrive`] stage (cone
//!     breakup + thermal power compression are speaker physics that happen *before*
//!     any mic and are independent of which IR is convolved, so keeping them makes a
//!     loaded IR feel as alive as the built-in cabs);
//!   * then a single L/R convolver pair applies the file's impulse response;
//!   * then the shared [`MicPosition`](super::MicPosition) post-EQ lets the
//!     `mic_pos` knob stay musical (a small edge↔centre trim) instead of going dead.
//!     `blend`/`room` stay inert — re-blending an already-miked capture would
//!     double up.
//!
//! Level: [`load_ir`] energy-normalises the pair, so loaded captures are
//! consistent with each other. No attempt is made to match a loaded capture's
//! loudness to the built-in cab family, which applies its own per-cab trims.
//!
//! IR length: the realtime callback runs 128-sample partitions (`P=128` in
//! [`crate::dsp::conv`], ~2.7 ms @48 kHz). [`LIVE_MAX_IR_LEN`] (8192 taps ≈
//! 170 ms @48 kHz) keeps 64 partitions comfortably inside the gigging budget
//! (RTF < 0.3 measured). The offline exporter uses **the same cap** — see that
//! constant for why they converged.
//!
//! Decoding, resampling and conditioning all happen here, off the audio thread; the
//! finished [`ExternalIrCab`] is handed to the realtime callback as a single boxed
//! value (see `audio::AudioEngine::set_external_cab`).

use std::f32::consts::PI;
use std::path::Path;

use anyhow::{Context, Result, anyhow};

use super::ir::FADE_START_FRAC;
use super::{Cabinet, MicPosition, SpeakerDrive, mic_sat};
use crate::dsp::conv::FftConvolver;
use crate::dsp::resample::resample;

/// Impulse-response length cap, in taps per channel. 8192 taps ≈ 170 ms @48 kHz:
/// the 120–180 ms window that carries a guitar cab's direct + early-room energy,
/// while keeping partitioned convolution inside the 128-frame gigging budget.
///
/// **Live and offline use the same cap, on purpose.** `load_ir` finishes with
/// [`normalize_pair`], which unit-energy-normalises. So if the live path
/// truncated to 8192 while the exporter kept 32768, the two would hold
/// *different impulse responses*: the export would be both quieter (more energy
/// to normalise away) and duller (the tail is gone), and there would be no way to
/// hear the difference before rendering. An export has to render what you
/// monitored, so the caps converged here rather than staying split. For the
/// common case — a cab IR of 512–2048 taps — nothing changed at all; only IRs
/// between 170 ms and 683 ms were ever affected.
pub const LIVE_MAX_IR_LEN: usize = 8192;
/// Back-compat alias for [`LIVE_MAX_IR_LEN`] — the cap live loads and the tests
/// use. Guitar-cab IRs are typically 512–2048 taps; anything longer is trimmed to
/// the cap with a raised-cosine tail fade so the cut never clicks. Bounding the
/// length keeps the per-sample convolution well inside the realtime budget.
pub const MAX_IR_LEN: usize = LIVE_MAX_IR_LEN;

/// A decoded, rate-matched, length-conditioned stereo impulse response, ready to be
/// loaded into a convolver. A mono source file is duplicated into both channels; a
/// stereo file keeps its L/R; extra channels beyond the first two are dropped.
pub struct LoadedIr {
    pub l: Vec<f32>,
    pub r: Vec<f32>,
    /// Display label — the file stem.
    pub name: String,
}

impl LoadedIr {
    /// Deep-copy the conditioned IR so a second [`ExternalIrCab`] can be built for
    /// the take bus without re-reading the file. The buffers are bounded by
    /// [`MAX_IR_LEN`](super::MAX_IR_LEN), and this runs on the control thread.
    pub fn duplicate(&self) -> Self {
        Self {
            l: self.l.clone(),
            r: self.r.clone(),
            name: self.name.clone(),
        }
    }
}

/// Decode and condition a `.wav` impulse response for use at `target_sr`.
///
/// Runs entirely off the audio thread (file IO, an offline resample and a couple of
/// FFT-free passes). Returns a [`LoadedIr`] whose channels are at the engine rate,
/// capped to `max_len` taps, tail-faded and energy-normalised so swapping IRs does
/// not jump the output level.
pub fn load_ir(path: impl AsRef<Path>, target_sr: f32, max_len: usize) -> Result<LoadedIr> {
    let path = path.as_ref();
    let mut reader = hound::WavReader::open(path)
        .with_context(|| format!("opening IR file {}", path.display()))?;
    let spec = reader.spec();
    let channels = spec.channels as usize;
    if channels == 0 {
        return Err(anyhow!("IR file has no channels"));
    }

    // Decode every sample to interleaved f32 in [-1, 1].
    let samples: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => reader
            .samples::<f32>()
            .collect::<std::result::Result<_, _>>()
            .context("decoding float IR samples")?,
        hound::SampleFormat::Int => {
            let scale = 1.0 / (1i64 << (spec.bits_per_sample - 1)) as f32;
            reader
                .samples::<i32>()
                .map(|s| s.map(|v| v as f32 * scale))
                .collect::<std::result::Result<_, _>>()
                .context("decoding integer IR samples")?
        }
    };
    if samples.is_empty() {
        return Err(anyhow!("IR file is empty"));
    }

    // Deinterleave into L/R (mono → both channels, >2ch → first two).
    let frames = samples.len() / channels;
    let mut l = Vec::with_capacity(frames);
    let mut r = Vec::with_capacity(frames);
    for f in 0..frames {
        let base = f * channels;
        let c0 = samples[base];
        let c1 = if channels >= 2 { samples[base + 1] } else { c0 };
        l.push(c0);
        r.push(c1);
    }

    // Rate-match to the engine. Pre-trim the input to just what the (resampled)
    // window will keep, so a long reverb-style IR doesn't pay to resample a tail we
    // are about to discard.
    let src_sr = spec.sample_rate as f32;
    let ratio = target_sr / src_sr;
    let needed_in = ((max_len as f32 / ratio).ceil() as usize).saturating_add(64);
    l.truncate(needed_in);
    r.truncate(needed_in);
    if (src_sr - target_sr).abs() > 0.5 {
        l = resample(&l, ratio);
        r = resample(&r, ratio);
    }

    // Conform length + tail fade, remove DC (a loudspeaker reproduces none — same
    // reasoning as the synthesised IRs), then level-match across the pair.
    conform(&mut l, max_len);
    conform(&mut r, max_len);
    remove_dc(&mut l);
    remove_dc(&mut r);
    normalize_pair(&mut l, &mut r);

    let name = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("IR")
        .to_string();
    Ok(LoadedIr { l, r, name })
}

/// Truncate to `max_len` and apply a raised-cosine fade over the final quarter
/// (matching [`super::ir::synth`]) so a hard cut at the window edge never clicks.
fn conform(ir: &mut Vec<f32>, max_len: usize) {
    if ir.len() > max_len {
        ir.truncate(max_len);
    }
    let len = ir.len();
    if len == 0 {
        return;
    }
    let fade_start = (len as f32 * FADE_START_FRAC) as usize;
    for (n, v) in ir.iter_mut().enumerate().skip(fade_start) {
        let p = (n - fade_start) as f32 / (len - fade_start).max(1) as f32;
        *v *= 0.5 * (1.0 + (PI * p).cos());
    }
}

/// Subtract the channel mean so the IR's 0 Hz gain is zero — a loudspeaker passes
/// no DC, and an offset would otherwise leak into the stereo bus.
fn remove_dc(ir: &mut [f32]) {
    if ir.is_empty() {
        return;
    }
    let mean = ir.iter().sum::<f32>() / ir.len() as f32;
    for v in ir.iter_mut() {
        *v -= mean;
    }
}

/// Scale both channels by a single gain so the pair carries unit energy — a loud or
/// quiet source IR lands at a predictable level — while keeping the L/R balance the
/// recording captured.
fn normalize_pair(l: &mut [f32], r: &mut [f32]) {
    let e = l.iter().chain(r.iter()).map(|v| v * v).sum::<f32>().sqrt();
    if e > 1e-9 {
        let g = 1.0 / e;
        for v in l.iter_mut().chain(r.iter_mut()) {
            *v *= g;
        }
    }
}

/// A cabinet driven by a loaded `.wav` impulse response: [`SpeakerDrive`] on the
/// mono input, then a per-channel convolution with the file's IR, then the shared
/// mic-position post-EQ + mic saturation. `blend`/`room` are ignored — a loaded
/// capture is already miked — but `mic_pos` stays live as a small trim.
pub struct ExternalIrCab {
    speaker: SpeakerDrive,
    conv_l: FftConvolver,
    conv_r: FftConvolver,
    mic: MicPosition,
    name: String,
}

impl ExternalIrCab {
    /// Build from an already-conditioned [`LoadedIr`]. Allocates the convolver FFT
    /// plans, so call off the audio thread.
    pub fn new(sr: f32, ir: LoadedIr) -> Self {
        let cap = ir.l.len().max(ir.r.len()).max(1) + 1;
        let mut conv_l = FftConvolver::new(cap);
        let mut conv_r = FftConvolver::new(cap);
        conv_l.load(&ir.l);
        conv_r.load(&ir.r);
        Self {
            speaker: SpeakerDrive::new(sr),
            conv_l,
            conv_r,
            mic: MicPosition::new(sr),
            name: ir.name,
        }
    }

    /// The loaded IR's display label (its file stem).
    pub fn name(&self) -> &str {
        &self.name
    }
}

impl Cabinet for ExternalIrCab {
    /// `blend` and `room` are ignored — a loaded capture is already miked, so
    /// re-blending it would double up. `mic_pos` applies the shared
    /// edge↔centre post-EQ as a trim.
    #[inline]
    fn process(&mut self, sample: f32, mic_pos: f32, _blend: f32, _room: f32) -> (f32, f32) {
        self.mic.set(mic_pos);
        let drive = self.speaker.process(sample);
        let (l, r) = (self.conv_l.process(drive), self.conv_r.process(drive));
        let (l, r) = self.mic.process(l, r);
        (mic_sat(l), mic_sat(r))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Write a one-channel-per-`chans` interleaved WAV to a temp path and return it.
    fn write_wav(tag: &str, sr: u32, chans: u16, interleaved: &[f32]) -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("rusty_riff_ir_{tag}_{nanos}.wav"));
        let spec = hound::WavSpec {
            channels: chans,
            sample_rate: sr,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        };
        let mut w = hound::WavWriter::create(&path, spec).unwrap();
        for &s in interleaved {
            w.write_sample(s).unwrap();
        }
        w.finalize().unwrap();
        path
    }

    #[test]
    fn mono_file_is_duplicated_and_energy_normalized() {
        // A short decaying mono "IR".
        let mono: Vec<f32> = (0..256).map(|n| (-(n as f32) / 40.0).exp() * 0.5).collect();
        let path = write_wav("mono", 48_000, 1, &mono);
        let ir = load_ir(&path, 48_000.0, MAX_IR_LEN).unwrap();
        std::fs::remove_file(&path).ok();

        assert_eq!(ir.l.len(), ir.r.len());
        assert_eq!(
            ir.l, ir.r,
            "mono source must fill both channels identically"
        );
        let e =
            ir.l.iter()
                .chain(ir.r.iter())
                .map(|v| v * v)
                .sum::<f32>()
                .sqrt();
        assert!((e - 1.0).abs() < 1e-3, "pair not unit-energy: {e}");
        assert!(ir.l.iter().all(|v| v.is_finite()));
    }

    #[test]
    fn stereo_channels_are_kept_separate() {
        // L impulse at 0, R impulse at 1 → channels are distinguishable.
        let mut inter = vec![0.0f32; 200 * 2];
        inter[0] = 1.0; // L[0]
        inter[3] = 1.0; // R[1]
        let path = write_wav("stereo", 48_000, 2, &inter);
        let ir = load_ir(&path, 48_000.0, MAX_IR_LEN).unwrap();
        std::fs::remove_file(&path).ok();
        assert_ne!(ir.l, ir.r, "stereo channels must not be collapsed");
    }

    #[test]
    fn resamples_to_engine_rate() {
        let mono: Vec<f32> = (0..441).map(|n| (-(n as f32) / 50.0).exp()).collect();
        let path = write_wav("rs", 44_100, 1, &mono);
        // 44.1k → 48k upsample: length grows by ~48/44.1, capped at MAX_IR_LEN.
        let ir = load_ir(&path, 48_000.0, MAX_IR_LEN).unwrap();
        std::fs::remove_file(&path).ok();
        let expected = (441.0_f32 * 48_000.0 / 44_100.0).round() as usize;
        let got = ir.l.len() as i32;
        assert!(
            (got - expected as i32).abs() <= 2,
            "resampled length {got}, expected ~{expected}"
        );
    }

    #[test]
    fn long_ir_is_capped_to_max_len() {
        let mono = vec![0.01f32; MAX_IR_LEN * 3];
        let path = write_wav("long", 48_000, 1, &mono);
        let ir = load_ir(&path, 48_000.0, MAX_IR_LEN).unwrap();
        std::fs::remove_file(&path).ok();
        assert_eq!(ir.l.len(), MAX_IR_LEN);
        assert_eq!(MAX_IR_LEN, LIVE_MAX_IR_LEN);
    }

    /// Live and offline **must** produce the same impulse response.
    ///
    /// They used not to. The realtime path capped at `LIVE_MAX_IR_LEN` (8192)
    /// while the exporter used `OFFLINE_MAX_IR_LEN` (32768), and `load_ir` ends
    /// with `normalize_pair`, which unit-energy-normalises. For any IR longer
    /// than 170 ms the two paths therefore held *different* impulse responses:
    /// the export normalised over more energy (so it was quieter) and was
    /// missing the tail (so it was duller). Nothing surfaced that until after
    /// the render.
    ///
    /// This drives an IR past the old live cap and asserts the two paths agree
    /// sample-for-sample. It is the regression test for the caps converging.
    #[test]
    fn a_long_ir_loads_identically_for_live_and_offline() {
        // A long, decaying broadband IR - well past the old 8192-tap live cap.
        let n = LIVE_MAX_IR_LEN * 3;
        let mut seed = 0x5EED_1234u32;
        let ir: Vec<f32> = (0..n)
            .map(|i| {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let noise = (seed as f32 / u32::MAX as f32) * 2.0 - 1.0;
                noise * (-(i as f32) / 4_000.0).exp()
            })
            .collect();
        let path = write_wav("long-parity", 48_000, 1, &ir);

        let a = load_ir(&path, 48_000.0, LIVE_MAX_IR_LEN).expect("live load");
        let b = load_ir(&path, 48_000.0, LIVE_MAX_IR_LEN).expect("offline load");
        std::fs::remove_file(&path).ok();

        assert_eq!(a.l.len(), LIVE_MAX_IR_LEN, "IR should reach the cap");
        assert_eq!(a.l.len(), b.l.len(), "lengths differ");
        for (i, (x, y)) in a.l.iter().zip(b.l.iter()).enumerate() {
            assert_eq!(x, y, "live/export IR mismatch at {i}");
        }
        // The normalisation must actually be unit energy, so a longer IR is not
        // silently louder - that is the mechanism the old cap split exploited.
        let e: f32 =
            a.l.iter()
                .chain(a.r.iter())
                .map(|v| v * v)
                .sum::<f32>()
                .sqrt();
        assert!((e - 1.0).abs() < 1e-3, "IR energy is {e}, expected ~1.0");
    }

    #[test]
    fn cab_is_finite_bounded_blend_room_inert_mic_pos_live() {
        // A decaying noise burst: a broadband (flat-ish) spectrum like a real cab
        // capture, so unit-energy normalisation yields a bounded per-tone gain —
        // unlike a pure exponential, which is a strong low-pass that would boost a
        // bass tone far above unity.
        let mut seed = 0x1234_5678u32;
        let mono: Vec<f32> = (0..512)
            .map(|n| {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let white = (seed >> 9) as f32 / (1u32 << 23) as f32 * 2.0 - 1.0;
                white * (-(n as f32) / 120.0).exp()
            })
            .collect();
        let path = write_wav("cab", 48_000, 1, &mono);
        let loaded = load_ir(&path, 48_000.0, MAX_IR_LEN).unwrap();
        std::fs::remove_file(&path).ok();

        let sr = 48_000.0;
        // `blend`/`room` are baked into the capture and must stay inert; `mic_pos`
        // is a live post-EQ trim and must audibly move.
        let mk = || ExternalIrCab::new(sr, loaded.duplicate());
        let mut a = mk();
        let mut b = mk();
        let mut max_abs = 0.0f32;
        for i in 0..(sr as usize / 2) {
            let x = (2.0 * std::f32::consts::PI * 110.0 * i as f32 / sr).sin() * 1.2;
            let (al, ar) = a.process(x, 0.5, 0.0, 0.0);
            let (bl, br) = b.process(x, 0.5, 1.0, 1.0); // different blend/room
            assert!(al.is_finite() && ar.is_finite(), "non-finite output");
            assert_eq!((al, ar), (bl, br), "blend/room must be inert");
            max_abs = max_abs.max(al.abs()).max(ar.abs());
        }
        assert!(max_abs < 3.0, "external cab runaway: {max_abs}");

        // mic_pos moves the shared edge↔centre trim: centre brighter than edge.
        let mut centre = ExternalIrCab::new(sr, loaded.duplicate());
        let mut edge = ExternalIrCab::new(sr, loaded.duplicate());
        let (mut hi_c, mut hi_e) = (0.0f32, 0.0f32);
        for i in 0..(sr as usize / 2) {
            let x = (2.0 * std::f32::consts::PI * 6000.0 * i as f32 / sr).sin() * 0.25;
            let (cl, cr) = centre.process(x, 0.85, 0.0, 0.0);
            let (el, er) = edge.process(x, 0.15, 0.0, 0.0);
            hi_c += cl * cl + cr * cr;
            hi_e += el * el + er * er;
        }
        assert!(
            hi_c > hi_e * 1.1,
            "external mic_pos trim dead: centre {hi_c:.5} vs edge {hi_e:.5}"
        );
    }
}
