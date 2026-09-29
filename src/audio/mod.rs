use anyhow::{Result, anyhow};
use cpal::{
    Device, FromSample, Sample, Stream, StreamConfig,
    traits::{DeviceTrait, HostTrait, StreamTrait},
};
use rtrb::{Consumer, Producer, RingBuffer};
use std::sync::Arc;
use std::sync::atomic::Ordering::Relaxed;

use crate::dsp::cab::ExternalIrCab;
use crate::dsp::effects::db_to_lin;
use crate::dsp::metronome::{Metronome, MetronomeVoice};
use crate::dsp::player::{PlayerTrack, PlayerVoice, TrackKind};
use crate::dsp::tuner::{Tuner, TunerDetector};
use crate::dsp::{DcBlocker, DspChain, Levels, Params, StereoInsert, soft_limit};
use crate::looper::{Looper, LooperControl};
use crate::practice::Practice;
use crate::recording::{CaptureState, capture_ring};

pub mod calibration;

use calibration::{CAL_RING_CAPACITY, CAL_WINDOW_MS, InputIdentity, TrimState, condition_block};
pub use calibration::{InputCalibration, WindowStat};

/// A swappable plugin insert handed to the audio thread (`Some` to install, `None`
/// to clear). Boxed so the audio thread only ever moves a pointer.
type InsertCommand = Option<Box<dyn StereoInsert>>;

/// A swappable external-IR cab handed to the audio thread (`Some` to install, `None`
/// to clear). Boxed for the same reason as [`InsertCommand`].
type ExtCabCommand = Option<Box<ExternalIrCab>>;

/// A swappable external amp (a hosted plugin) handed to the audio thread (`Some` to
/// install, `None` to clear). Boxed like [`InsertCommand`].
type ExtAmpCommand = Option<Box<dyn StereoInsert>>;

/// How many pending insert swaps / disposals the lock-free rings can hold. Swaps
/// are rare (a user loading/clearing a plugin), so a small buffer is plenty.
const INSERT_QUEUE_CAP: usize = 8;

/// Capacity of the UI → audio track command ring. Generous: gain/mute edits go
/// through here too, at UI frame rate.
const TRACK_QUEUE_CAP: usize = 64;

/// A timeline-track change sent from the control thread to the audio callback.
/// Applied at the top of `on_input`, so a block never sees a half-updated
/// track set.
pub enum TrackCommand {
    Install {
        id: u64,
        generation: u64,
        kind: TrackKind,
        track: PlayerTrack,
        gain: f32,
        muted: bool,
    },
    Remove {
        id: u64,
    },
    SetGain {
        id: u64,
        gain: f32,
    },
    SetMute {
        id: u64,
        muted: bool,
    },
    SetStart {
        id: u64,
        start: usize,
    },
}

/// Audio → control acknowledgement that an install actually landed (or why it
/// did not). Lets the UI reconcile a failed install instead of showing a row
/// that is not playing.
pub struct TrackAck {
    pub id: u64,
    pub generation: u64,
    pub installed: bool,
    pub error: Option<String>,
}

/// Largest block (in frames) the audio thread will ever process at once. Scratch
/// buffers are pre-sized to this, and plugin inserts are activated with it as
/// their maximum block size.
pub const MAX_BLOCK: usize = 4096;

/// Split `frames` interleaved frames into half-open `(start, end)` ranges of at
/// most `max` frames each, in order. An oversized device callback is processed
/// one range at a time so every buffer stays within its preallocated capacity
/// (the callback never resizes or allocates).
fn block_ranges(frames: usize, max: usize) -> impl Iterator<Item = (usize, usize)> {
    let step = max.max(1);
    (0..frames)
        .step_by(step)
        .map(move |start| (start, (start + step).min(frames)))
}
/// Frames per audio callback we request from the OS. 256 ≈ 5.3 ms at 48 kHz:
/// tight enough that playing feels connected (stock DAWs run 64–256), loose
/// enough that a release build never underruns on Apple Silicon. Without an
/// explicit request CoreAudio may hand us 1024+ frames, which feels spongy and
/// makes even a clean DI sound dull and distant. Always test audio on
/// `cargo run --release` — debug builds can underrun at this size.
const LIVE_BUFFER_FRAMES: u32 = 256;

/// ALSA-specific period size. Unlike CoreAudio, cpal's `BufferSize::Fixed` on
/// ALSA sets the transfer *period* (the per-callback chunk), not a total buffer,
/// and cpal's ALSA thread runs without realtime priority — so a 256-frame period
/// (~5.3 ms) is easily missed under load and the capture stream overruns
/// continuously ("A buffer underrun or overrun occurred"). A larger period is
/// far more forgiving and still feels live.
#[cfg(target_os = "linux")]
const ALSA_PERIOD_FRAMES: u32 = 1024;

/// The fixed period to request for this host's streams.
fn requested_frames(host: &cpal::Host) -> u32 {
    #[cfg(target_os = "linux")]
    if host.id() == cpal::HostId::Alsa {
        return ALSA_PERIOD_FRAMES;
    }
    let _ = host;
    LIVE_BUFFER_FRAMES
}

/// Appends one timestamped line to `~/.config/rusty-riff/audio.log`.
///
/// Diagnostics must go here, not stderr: the TUI runs in the terminal's
/// alternate screen, so anything printed while it is active scrolls into a
/// hidden buffer that is discarded on quit and the user never sees it.
/// Logging is best-effort — it must never fail audio startup.
pub fn log_line(msg: &str) {
    let path = dirs::home_dir().map(|h| h.join(".config/rusty-riff/audio.log"));
    if let Some(path) = path {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
        {
            use std::io::Write as _;
            let _ = writeln!(f, "[{secs}] {msg}");
        }
    }
}

pub struct AudioEngine {
    _input_stream: Stream,
    _output_stream: Stream,
    /// Negotiated sample rate of the running streams (Hz).
    sample_rate: f32,
    /// Sends insert swaps to the audio thread (consumed at the top of its callback).
    insert_tx: Producer<InsertCommand>,
    /// Receives inserts the audio thread displaced, so they are dropped here on a
    /// non-audio thread rather than freed in the realtime callback.
    dropped_rx: Consumer<Box<dyn StereoInsert>>,
    /// Sends external-IR cab swaps to the audio thread.
    ext_cab_tx: Producer<ExtCabCommand>,
    /// Receives external-IR cabs the audio thread displaced, for off-thread disposal.
    ext_dropped_rx: Consumer<Box<ExternalIrCab>>,
    /// Sends external-amp swaps to the audio thread.
    ext_amp_tx: Producer<ExtAmpCommand>,
    /// Receives external amps the audio thread displaced, for off-thread disposal.
    ext_amp_dropped_rx: Consumer<Box<dyn StereoInsert>>,
    /// Take-bus mirror of the plugin insert (a second plugin instance).
    take_insert_tx: Producer<InsertCommand>,
    take_insert_dropped_rx: Consumer<Box<dyn StereoInsert>>,
    /// Take-bus mirror of the external-IR cab.
    take_ext_cab_tx: Producer<ExtCabCommand>,
    take_ext_cab_dropped_rx: Consumer<Box<ExternalIrCab>>,
    /// Take-bus mirror of the external amp.
    take_ext_amp_tx: Producer<ExtAmpCommand>,
    take_ext_amp_dropped_rx: Consumer<Box<dyn StereoInsert>>,
    /// Sends timeline track commands to the audio thread.
    track_tx: Producer<TrackCommand>,
    /// Receives tracks the audio thread displaced, for off-thread disposal.
    track_dropped_rx: Consumer<PlayerTrack>,
    /// Receives install acknowledgements from the audio thread.
    track_ack_rx: Consumer<TrackAck>,
    /// The capture ring's consumer, handed to the writer worker by the UI.
    capture_rx: Option<Consumer<f32>>,
    /// Receives raw calibration windows from the audio thread (control thread).
    cal_stats_rx: Consumer<WindowStat>,
    /// Identity of the running input, for calibration persistence.
    identity: InputIdentity,
}

impl AudioEngine {
    /// The sample rate (Hz) the engine negotiated and is running at.
    pub fn sample_rate(&self) -> f32 {
        self.sample_rate
    }

    /// Identity of the input the engine is running with (device, channels,
    /// guitar channel), used to look up and save input calibration.
    pub fn input_identity(&self) -> InputIdentity {
        self.identity.clone()
    }

    /// Drain any captured calibration windows into `out` (control thread only).
    /// The UI calls this each tick while the calibration wizard is measuring.
    pub fn drain_cal_stats(&mut self, out: &mut Vec<WindowStat>) {
        while let Ok(stat) = self.cal_stats_rx.pop() {
            out.push(stat);
        }
    }

    /// Install (`Some`) or clear (`None`) the third-party plugin insert.
    ///
    /// Call this from the UI/control thread, never the audio thread. The actual
    /// swap happens lock-free inside the audio callback; any previously installed
    /// insert is disposed of here, on the caller's thread.
    pub fn set_plugin_insert(&mut self, insert: InsertCommand) -> Result<()> {
        // Dispose of anything the audio thread has handed back since last time.
        while let Ok(old) = self.dropped_rx.pop() {
            drop(old);
        }
        self.insert_tx
            .push(insert)
            .map_err(|_| anyhow!("plugin-insert command queue is full"))
    }

    /// Install (`Some`) or clear (`None`) the external-IR cab.
    ///
    /// Call from the UI/control thread. The swap happens lock-free in the audio
    /// callback; any displaced cab is disposed of here, on the caller's thread, so
    /// its IR/FFT buffers are never freed in the realtime path. Build the
    /// [`ExternalIrCab`] (decode + resample) before calling — that work is offline.
    pub fn set_external_cab(&mut self, cab: ExtCabCommand) -> Result<()> {
        while let Ok(old) = self.ext_dropped_rx.pop() {
            drop(old);
        }
        self.ext_cab_tx
            .push(cab)
            .map_err(|_| anyhow!("external-cab command queue is full"))
    }

    /// Install (`Some`) or clear (`None`) the external amp override (a hosted plugin).
    ///
    /// Call from the UI/control thread. The swap happens lock-free in the audio
    /// callback; any displaced amp is disposed of here, on the caller's thread, so a
    /// plugin is never freed in the realtime path. Build the plugin (via the `host`
    /// module) before calling.
    pub fn set_external_amp(&mut self, amp: ExtAmpCommand) -> Result<()> {
        while let Ok(old) = self.ext_amp_dropped_rx.pop() {
            drop(old);
        }
        self.ext_amp_tx
            .push(amp)
            .map_err(|_| anyhow!("external-amp command queue is full"))
    }

    /// Take-bus counterparts of the three external-rig installers. The UI builds a
    /// **second** plugin/cab instance (never sharing one processor across two
    /// chains) and installs it here so finished raw takes are re-amped through the
    /// same external rig as the live guitar.
    pub fn set_plugin_insert_take(&mut self, insert: InsertCommand) -> Result<()> {
        while let Ok(old) = self.take_insert_dropped_rx.pop() {
            drop(old);
        }
        self.take_insert_tx
            .push(insert)
            .map_err(|_| anyhow!("take plugin-insert command queue is full"))
    }

    pub fn set_external_cab_take(&mut self, cab: ExtCabCommand) -> Result<()> {
        while let Ok(old) = self.take_ext_cab_dropped_rx.pop() {
            drop(old);
        }
        self.take_ext_cab_tx
            .push(cab)
            .map_err(|_| anyhow!("take external-cab command queue is full"))
    }

    pub fn set_external_amp_take(&mut self, amp: ExtAmpCommand) -> Result<()> {
        while let Ok(old) = self.take_ext_amp_dropped_rx.pop() {
            drop(old);
        }
        self.take_ext_amp_tx
            .push(amp)
            .map_err(|_| anyhow!("take external-amp command queue is full"))
    }

    /// Install (or replace) a timeline track.
    ///
    /// Decode and rate-match the file first (see [`crate::practice::decode_track`]) —
    /// that work is offline; this call only hands the finished buffers to the audio
    /// thread lock-free. The displaced track is disposed of here, on the caller's
    /// thread, so its sample buffer is never freed in the realtime callback. An
    /// install acknowledgement is delivered later via [`Self::poll_track_acks`].
    pub fn install_track(
        &mut self,
        id: u64,
        generation: u64,
        kind: TrackKind,
        track: PlayerTrack,
        gain: f32,
        muted: bool,
    ) -> Result<()> {
        while let Ok(old) = self.track_dropped_rx.pop() {
            drop(old);
        }
        self.track_tx
            .push(TrackCommand::Install {
                id,
                generation,
                kind,
                track,
                gain,
                muted,
            })
            .map_err(|_| anyhow!("timeline command queue is full"))
    }

    /// Remove a timeline track.
    pub fn remove_track(&mut self, id: u64) -> Result<()> {
        while let Ok(old) = self.track_dropped_rx.pop() {
            drop(old);
        }
        self.track_tx
            .push(TrackCommand::Remove { id })
            .map_err(|_| anyhow!("timeline command queue is full"))
    }

    /// Set a track's level (monitor volume for imports, pre-rig gain for takes).
    pub fn set_track_gain(&mut self, id: u64, gain: f32) -> Result<()> {
        self.track_tx
            .push(TrackCommand::SetGain { id, gain })
            .map_err(|_| anyhow!("timeline command queue is full"))
    }

    /// Mute/unmute a track.
    pub fn set_track_mute(&mut self, id: u64, muted: bool) -> Result<()> {
        self.track_tx
            .push(TrackCommand::SetMute { id, muted })
            .map_err(|_| anyhow!("timeline command queue is full"))
    }

    /// Move a track's timeline start, in output frames.
    pub fn set_track_start(&mut self, id: u64, start: usize) -> Result<()> {
        self.track_tx
            .push(TrackCommand::SetStart { id, start })
            .map_err(|_| anyhow!("timeline command queue is full"))
    }

    /// Drain any pending install acknowledgements. Allocates a small `Vec` on the
    /// control thread, never the audio thread.
    pub fn poll_track_acks(&mut self) -> Vec<TrackAck> {
        let mut out = Vec::new();
        while let Ok(ack) = self.track_ack_rx.pop() {
            out.push(ack);
        }
        out
    }

    /// Take the capture ring's consumer so the UI can spawn the writer worker.
    /// Only available once.
    pub fn take_capture_consumer(&mut self) -> Option<Consumer<f32>> {
        self.capture_rx.take()
    }
}

pub struct InputInfo {
    pub name: String,
    pub channels: usize,
}

pub struct DeviceInfo {
    pub inputs: Vec<InputInfo>,
    pub outputs: Vec<String>,
}

/// True for the ALSA `null` sink/source, which cpal lists as
/// "Discard all samples (playback) or generate zero samples (capture)".
fn is_null_device(name: &str) -> bool {
    name.contains("Discard all samples") || name.eq_ignore_ascii_case("null")
}

/// Enumerates input devices, dropping the `null` pseudo-device and collapsing
/// entries that share a `(name, channels)` pair. The raw ALSA surface exposes
/// every PCM (`sysdefault`, `front`, `surround*`, …) under the same human name;
/// showing them all is noise, and after filtering the indices stay stable
/// between this list and [`start`].
fn collect_inputs(host: &cpal::Host) -> Result<Vec<(Device, InputInfo)>> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for (i, d) in host.input_devices()?.enumerate() {
        let name = d
            .description()
            .map(|desc| desc.name().to_owned())
            .unwrap_or_else(|_| format!("device-{i}"));
        if is_null_device(&name) {
            continue;
        }
        let channels = d
            .default_input_config()
            .map(|c| c.channels() as usize)
            .unwrap_or(1);
        if seen.insert((name.clone(), channels)) {
            out.push((d, InputInfo { name, channels }));
        }
    }
    Ok(out)
}

/// Output counterpart to [`collect_inputs`]: drops `null` and de-duplicates by
/// device name.
fn collect_outputs(host: &cpal::Host) -> Result<Vec<(Device, String)>> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for (i, d) in host.output_devices()?.enumerate() {
        let name = d
            .description()
            .map(|desc| desc.name().to_owned())
            .unwrap_or_else(|_| format!("device-{i}"));
        if is_null_device(&name) {
            continue;
        }
        if seen.insert(name.clone()) {
            out.push((d, name));
        }
    }
    Ok(out)
}

pub fn list_devices() -> Result<DeviceInfo> {
    let host = cpal::default_host();
    let inputs = collect_inputs(&host)?
        .into_iter()
        .map(|(_, info)| info)
        .collect();
    let outputs = collect_outputs(&host)?
        .into_iter()
        .map(|(_, name)| name)
        .collect();
    Ok(DeviceInfo { inputs, outputs })
}

/// Where the last good device selection is remembered.
fn selection_path() -> Option<std::path::PathBuf> {
    dirs::home_dir().map(|h| h.join(".config/rusty-riff/audio.conf"))
}

/// Persist the chosen devices by *name* rather than index, so the selection
/// survives a USB interface re-enumerating at a different position. Best-effort:
/// a failed write must never block startup.
pub fn save_selection(devices: &DeviceInfo, input_idx: usize, guitar_ch: usize, output_idx: usize) {
    let Some(path) = selection_path() else { return };
    let Some(input) = devices.inputs.get(input_idx) else {
        return;
    };
    let Some(output) = devices.outputs.get(output_idx) else {
        return;
    };
    let body = format!(
        "# rusty-riff device selection — delete this file (or launch with \
         RUSTY_AMP_DEVICE_PROMPT=1) to be prompted again\n\
         input_name = {}\n\
         input_channels = {}\n\
         guitar_channel = {}\n\
         output_name = {}\n",
        input.name, input.channels, guitar_ch, output,
    );
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(path, body);
}

/// Resolve a saved selection against the current device list. Returns `None`
/// when nothing is saved, the file is malformed, the devices are gone, or the
/// saved channel no longer exists — the caller then shows the interactive
/// picker instead.
pub fn load_selection(devices: &DeviceInfo) -> Option<(usize, usize, usize)> {
    let text = std::fs::read_to_string(selection_path()?).ok()?;

    let mut input_name: Option<String> = None;
    let mut input_channels: Option<usize> = None;
    let mut guitar_channel: Option<usize> = None;
    let mut output_name: Option<String> = None;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim();
        match key.trim() {
            "input_name" => input_name = Some(value.to_owned()),
            "input_channels" => input_channels = value.parse::<usize>().ok(),
            "guitar_channel" => guitar_channel = value.parse::<usize>().ok(),
            "output_name" => output_name = Some(value.to_owned()),
            _ => {}
        }
    }

    let input_idx = devices.inputs.iter().position(|d| {
        Some(d.name.as_str()) == input_name.as_deref() && Some(d.channels) == input_channels
    })?;
    let output_idx = devices
        .outputs
        .iter()
        .position(|n| Some(n.as_str()) == output_name.as_deref())?;
    let guitar_channel = guitar_channel?;
    if guitar_channel >= devices.inputs[input_idx].channels {
        return None;
    }
    Some((input_idx, guitar_channel, output_idx))
}

#[allow(clippy::too_many_arguments)]
pub fn start(
    input_idx: usize,
    guitar_ch: usize,
    output_idx: usize,
    params: Arc<Params>,
    levels: Arc<Levels>,
    capture: Arc<CaptureState>,
    tuner: Arc<Tuner>,
    metronome: Arc<Metronome>,
    practice: Arc<Practice>,
    calibration: Arc<InputCalibration>,
    looper: Arc<LooperControl>,
) -> Result<AudioEngine> {
    let host = cpal::default_host();

    let input_device = collect_inputs(&host)?
        .into_iter()
        .nth(input_idx)
        .map(|(d, _)| d)
        .ok_or_else(|| anyhow!("Input device index {input_idx} not found"))?;

    let output_device = collect_outputs(&host)?
        .into_iter()
        .nth(output_idx)
        .map(|(d, _)| d)
        .ok_or_else(|| anyhow!("Output device index {output_idx} not found"))?;

    let (input_cfg, output_cfg, sr, in_fmt, out_fmt) =
        negotiate_configs(&input_device, &output_device)?;

    let in_channels = input_cfg.channels as usize;
    let out_channels = output_cfg.channels as usize;

    let input_name = input_device
        .description()
        .map(|desc| desc.name().to_owned())
        .unwrap_or_else(|_| format!("input-{input_idx}"));
    let output_name = output_device
        .description()
        .map(|desc| desc.name().to_owned())
        .unwrap_or_else(|_| format!("output-{output_idx}"));
    let shown_ch = guitar_ch + 1;
    let shown_sr = sr as u32;
    let frames = requested_frames(&host);
    // Calibration identity: the device name plus the channels actually opened.
    let identity = InputIdentity {
        device: input_name.clone(),
        channels: in_channels as u16,
        channel: guitar_ch as u16,
    };
    let msg = format!(
        "Audio: in '{input_name}' ch {shown_ch}/{in_channels} ({in_fmt}) -> out '{output_name}' ch {out_channels} ({out_fmt}), {shown_sr} Hz, requesting buffer {frames} frames",
    );
    // Log file only: `start` runs while the TUI owns the terminal's alternate
    // screen, so anything written to stderr would be painted over the UI and
    // persist (ratatui only redraws cells it believes changed).
    log_line(&msg);

    // Ask both directions for a small callback first; a device that rejects the
    // fixed size (common with PipeWire's ALSA plugin) gets a second chance with
    // the backend default rather than failing startup outright.
    let fixed = cpal::BufferSize::Fixed(frames);
    match build_engine(
        &input_device,
        with_buffer(&input_cfg, fixed),
        in_channels,
        guitar_ch,
        in_fmt,
        &output_device,
        with_buffer(&output_cfg, fixed),
        out_channels,
        out_fmt,
        sr,
        Arc::clone(&params),
        Arc::clone(&levels),
        Arc::clone(&capture),
        Arc::clone(&tuner),
        Arc::clone(&metronome),
        Arc::clone(&practice),
        Arc::clone(&calibration),
        Arc::clone(&looper),
        identity.clone(),
    ) {
        Ok(engine) => Ok(engine),
        Err(err) => {
            let msg = format!(
                "Audio: stream build with a {frames}-frame request failed ({err}); retrying with backend default buffer",
            );
            log_line(&msg);
            build_engine(
                &input_device,
                with_buffer(&input_cfg, cpal::BufferSize::Default),
                in_channels,
                guitar_ch,
                in_fmt,
                &output_device,
                with_buffer(&output_cfg, cpal::BufferSize::Default),
                out_channels,
                out_fmt,
                sr,
                params,
                levels,
                capture,
                tuner,
                metronome,
                practice,
                calibration,
                looper,
                identity,
            )
        }
    }
}

fn negotiate_configs(
    input: &Device,
    output: &Device,
) -> Result<(
    StreamConfig,
    StreamConfig,
    f32,
    cpal::SampleFormat,
    cpal::SampleFormat,
)> {
    let in_sup = input.default_input_config()?;
    let in_sr = in_sup.sample_rate();
    let in_fmt = in_sup.sample_format();

    // Prefer the output's *default* config when it runs at the input's rate: it is
    // the format the device (or its host) actually wants — e.g. f32 through
    // PipeWire, or native i32 on a raw ALSA `hw` PCM. Falling through to the
    // supported-range list and picking by channel count alone can otherwise land
    // on an exotic format like f64, forcing an expensive plug-layer conversion.
    let out_default = output.default_output_config().ok();
    let preferred_channels = out_default.as_ref().map(|c| c.channels());
    let out_sup = match out_default {
        Some(d) if d.sample_rate() == in_sr => d,
        _ => {
            // ALSA advertises a config range per channel count and the first match
            // is often mono, which would silently collapse the rig's stereo image.
            // Prefer the default channel count at the input's rate, then a
            // widely-supported sample format, then the widest config.
            let chosen = output
                .supported_output_configs()?
                .filter(|r| r.min_sample_rate() <= in_sr && r.max_sample_rate() >= in_sr)
                .max_by_key(|r| {
                    (
                        Some(r.channels()) == preferred_channels,
                        format_rank(r.sample_format()),
                        r.channels(),
                    )
                });
            match chosen {
                Some(range) => range.with_sample_rate(in_sr),
                None => {
                    let default = output.default_output_config().map_err(|e| {
                        anyhow!(
                            "output has no supported config for {in_sr} Hz and its default config is unavailable: {e}"
                        )
                    })?;
                    let fallback_sr = default.sample_rate();
                    let msg = format!(
                        "Audio: output does not support {in_sr} Hz; falling back to its default {fallback_sr} Hz"
                    );
                    log_line(&msg);
                    default
                }
            }
        }
    };
    let out_fmt = out_sup.sample_format();

    // Buffer size is chosen by the caller: `start` first requests the small
    // [`LIVE_BUFFER_FRAMES`] size and falls back to the backend default if the
    // device rejects it.
    let in_cfg: StreamConfig = in_sup.into();
    let out_cfg: StreamConfig = out_sup.into();

    Ok((in_cfg, out_cfg, in_sr as f32, in_fmt, out_fmt))
}

/// Rank sample formats by how cheaply and universally they convert: prefer native
/// integer and f32 formats over f64 (which forces an ALSA plug-layer conversion).
fn format_rank(f: cpal::SampleFormat) -> u8 {
    match f {
        cpal::SampleFormat::F32 => 5,
        cpal::SampleFormat::I32 => 4,
        cpal::SampleFormat::I16 => 3,
        cpal::SampleFormat::U16 => 2,
        cpal::SampleFormat::I24 => 1,
        cpal::SampleFormat::U24 => 1,
        _ => 0,
    }
}

/// Returns a copy of `cfg` with its requested buffer size replaced.
fn with_buffer(cfg: &StreamConfig, buffer_size: cpal::BufferSize) -> StreamConfig {
    StreamConfig {
        buffer_size,
        ..*cfg
    }
}

/// All state owned by the audio *input* callback.
///
/// Bundling it in one struct lets the same block-processing code be compiled for
/// whichever sample format the device requires — some interfaces (e.g. a Focusrite
/// Scarlett Solo) expose `S32_LE` capture rather than `f32`, so hard-coding an f32
/// stream fails to open at all. `on_input` converts the device's samples into the
/// engine's `f32` domain on the way in.
/// Final output stage: sum the live and take buses, then apply the output
/// ceiling.
///
/// Both buses have *already* been through their own `master_bus` (the take bus
/// is a whole second [`DspChain`]), so each is bounded by 1.0 on its own. Their
/// **sum** is not: two buses at the ceiling are +3 dB, and the metronome click
/// and looper are mixed into the live bus before this point with nothing after
/// them. Before this existed, the converter clipped and the overload grew with
/// every take layer.
///
/// The ceiling is the same [`soft_limit`] the master bus uses, so live and
/// exported audio are conditioned identically.
#[inline]
fn output_stage(live_l: f32, live_r: f32, take_l: f32, take_r: f32) -> (f32, f32) {
    (soft_limit(live_l + take_l), soft_limit(live_r + take_r))
}

struct InputState {
    /// The live guitar rig.
    chain: DspChain,
    /// A second rig instance for the summed raw-take bus. It shares the same
    /// [`Params`] (so knob changes apply to both), but owns independent DSP
    /// state, so its reverb/delay/sag cannot leak into the live signal.
    take_chain: DspChain,
    tuner_detector: TunerDetector,
    metro_voice: MetronomeVoice,
    player: PlayerVoice,
    attack: f32,
    release: f32,
    in_env: f32,
    out_env: f32,
    in_channels: usize,
    out_channels: usize,
    guitar_ch: usize,
    in_buf: Vec<f32>,
    out_l: Vec<f32>,
    out_r: Vec<f32>,
    /// Mono sum of unmuted raw takes for this block (input to `take_chain`).
    take_in: Vec<f32>,
    /// Take-bus rig output for this block.
    take_l: Vec<f32>,
    take_r: Vec<f32>,
    /// DC blockers for the tuner bypass, which writes straight to the output
    /// without passing through the chain's master bus.
    tuner_dc_l: DcBlocker,
    tuner_dc_r: DcBlocker,
    insert_rx: Consumer<InsertCommand>,
    dropped_tx: Producer<Box<dyn StereoInsert>>,
    ext_cab_rx: Consumer<ExtCabCommand>,
    ext_dropped_tx: Producer<Box<ExternalIrCab>>,
    ext_amp_rx: Consumer<ExtAmpCommand>,
    ext_amp_dropped_tx: Producer<Box<dyn StereoInsert>>,
    take_insert_rx: Consumer<InsertCommand>,
    take_insert_dropped_tx: Producer<Box<dyn StereoInsert>>,
    take_ext_cab_rx: Consumer<ExtCabCommand>,
    take_ext_cab_dropped_tx: Producer<Box<ExternalIrCab>>,
    take_ext_amp_rx: Consumer<ExtAmpCommand>,
    take_ext_amp_dropped_tx: Producer<Box<dyn StereoInsert>>,
    track_rx: Consumer<TrackCommand>,
    track_dropped_tx: Producer<PlayerTrack>,
    track_ack_tx: Producer<TrackAck>,
    capture_tx: Producer<f32>,
    /// Audio-local latch: set once the first dry sample of the active take has
    /// been pushed, so `start_frame` is captured exactly once.
    capture_started: bool,
    /// Last seen [`CaptureState::generation`], so a re-arm is detected even if
    /// the callback never observed an intermediate disarmed block.
    capture_generation: u64,
    producer: Producer<f32>,
    levels: Arc<Levels>,
    capture: Arc<CaptureState>,
    tuner: Arc<Tuner>,
    metronome: Arc<Metronome>,
    practice: Arc<Practice>,
    /// The looper voice and the shared transport it drains each block.
    looper: Looper,
    looper_ctl: Arc<LooperControl>,
    /// Shared input-calibration state (trim dB + measuring/clip flags).
    cal: Arc<InputCalibration>,
    /// Pushes raw measurement windows to the control thread while calibrating.
    cal_tx: Producer<WindowStat>,
    /// Trim smoother state for the guitar input.
    trim: TrimState,
    /// Window currently being accumulated (raw, pre-trim).
    win: WindowStat,
    /// Frames per calibration window at the engine rate.
    win_len: u32,
}

impl InputState {
    /// Deinterleave, process and fan the input block back out to the ring buffer
    /// the output callback drains. Samples arrive as the device's `T` and are
    /// converted to `f32` on the way in.
    fn on_input<T>(&mut self, data: &[T])
    where
        T: Sample,
        f32: FromSample<T>,
    {
        // Apply any pending insert swaps before processing this block. The old
        // insert is shipped back to the control thread for disposal; if that
        // queue is somehow full we drop it here as a last resort.
        while let Ok(cmd) = self.insert_rx.pop() {
            if let Some(old) = self.chain.replace_insert(cmd) {
                let _ = self.dropped_tx.push(old);
            }
        }
        // Same lock-free discipline for external-IR cab swaps.
        while let Ok(cmd) = self.ext_cab_rx.pop() {
            if let Some(old) = self.chain.replace_external_cab(cmd) {
                let _ = self.ext_dropped_tx.push(old);
            }
        }
        // ...and for external-amp swaps.
        while let Ok(cmd) = self.ext_amp_rx.pop() {
            if let Some(old) = self.chain.replace_ext_amp(cmd) {
                let _ = self.ext_amp_dropped_tx.push(old);
            }
        }
        // The take bus mirrors the same external rig with its own instances.
        while let Ok(cmd) = self.take_insert_rx.pop() {
            if let Some(old) = self.take_chain.replace_insert(cmd) {
                let _ = self.take_insert_dropped_tx.push(old);
            }
        }
        while let Ok(cmd) = self.take_ext_cab_rx.pop() {
            if let Some(old) = self.take_chain.replace_external_cab(cmd) {
                let _ = self.take_ext_cab_dropped_tx.push(old);
            }
        }
        while let Ok(cmd) = self.take_ext_amp_rx.pop() {
            if let Some(old) = self.take_chain.replace_ext_amp(cmd) {
                let _ = self.take_ext_amp_dropped_tx.push(old);
            }
        }
        // Timeline track commands: apply every pending add/remove/gain/mute at
        // once so this block sees a coherent track set. Displaced buffers go back
        // to the control thread; a full return queue defers the swap rather than
        // freeing a large `Vec` in the callback.
        while let Ok(cmd) = self.track_rx.pop() {
            match cmd {
                TrackCommand::Install {
                    id,
                    generation,
                    kind,
                    track,
                    gain,
                    muted,
                } => match self
                    .player
                    .install(id, generation, kind, track, gain, muted)
                {
                    Ok(displaced) => {
                        if let Some(old) = displaced {
                            let _ = self.track_dropped_tx.push(old);
                        }
                        let _ = self.track_ack_tx.push(TrackAck {
                            id,
                            generation,
                            installed: true,
                            error: None,
                        });
                    }
                    Err(e) => {
                        let _ = self.track_ack_tx.push(TrackAck {
                            id,
                            generation,
                            installed: false,
                            error: Some(e.to_string()),
                        });
                    }
                },
                TrackCommand::Remove { id } => {
                    if let Some(slot) = self.player.remove(id) {
                        let _ = self.track_dropped_tx.push(slot.track);
                    }
                }
                TrackCommand::SetGain { id, gain } => {
                    self.player.set_gain(id, gain);
                }
                TrackCommand::SetMute { id, muted } => {
                    self.player.set_muted(id, muted);
                }
                TrackCommand::SetStart { id, start } => {
                    self.player.set_start(id, start);
                }
            }
        }

        // Process in chunks of at most MAX_BLOCK frames. A device callback can ask
        // for more than our preallocated block (large ALSA periods); splitting
        // keeps every buffer within its preallocated capacity so the callback
        // never allocates. Per-chunk transport/metronome/route snapshots are
        // intentional — each chunk is a self-contained block.
        debug_assert!(self.in_channels > 0);
        let frames = data.len() / self.in_channels;
        for (start, end) in block_ranges(frames, MAX_BLOCK) {
            let chunk = &data[start * self.in_channels..end * self.in_channels];
            self.on_input_chunk(chunk);
        }

        // Publish the timeline cursor and meters once per callback (a chunk's
        // worth of playback shows as one position).
        self.practice.store_position(self.player.cursor());
        self.levels.input.store(self.in_env, Relaxed);
        self.levels.output.store(self.out_env, Relaxed);
    }

    /// Process one chunk of at most [`MAX_BLOCK`] interleaved frames. Split out of
    /// [`on_input`](Self::on_input) so an oversized device callback is handled in
    /// preallocated pieces instead of resizing buffers on the audio thread.
    fn on_input_chunk<T>(&mut self, data: &[T])
    where
        T: Sample,
        f32: FromSample<T>,
    {
        let frames = data.len() / self.in_channels;
        debug_assert!(frames <= MAX_BLOCK, "chunk exceeds the preallocated block");

        // Drain the looper's transport commands once per block; the per-frame loop
        // below feeds it the post-trim dry sample and mixes its output into the
        // monitor path only (post-record, like the metronome and practice player).
        self.looper.poll_controls(&self.looper_ctl);

        // Deinterleave the guitar channel into the mono input block, converting
        // from the device's sample type to the engine's f32 domain. `in_buf` is
        // preallocated to MAX_BLOCK, so this never reallocates.
        self.in_buf.clear();
        self.in_buf
            .extend(data.chunks(self.in_channels).map(|frame| {
                f32::from_sample(frame.get(self.guitar_ch).copied().unwrap_or(T::EQUILIBRIUM))
            }));

        // Input conditioning: apply the calibration trim to the dry guitar before
        // anything else, and (while the wizard measures) push raw window stats to
        // the control thread. At 0 dB settled this is bit-identical to no trim.
        let target = db_to_lin(self.cal.trim_db.load(Relaxed));
        let measuring = self.cal.measuring.load(Relaxed);
        let flags = condition_block(
            &mut self.in_buf,
            &mut self.trim,
            &mut self.win,
            self.win_len,
            target,
            measuring,
            |stat| self.cal_tx.push(stat).is_ok(),
        );
        if flags.clipped {
            self.cal.raw_clip.store(true, Relaxed);
        }
        if flags.overflow {
            self.cal.stats_overflow.store(true, Relaxed);
        }

        if self.tuner.active.load(Relaxed) {
            // Bypass the whole rig: clean dry guitar to both channels, and
            // analyse the same signal for pitch and spectrum. The DC blocker
            // matches what the master bus would have done, since this path
            // never reaches it.
            self.tuner_detector.process(&self.in_buf, &self.tuner);
            for ((dst_l, dst_r), &x) in self
                .out_l
                .iter_mut()
                .zip(self.out_r.iter_mut())
                .zip(self.in_buf.iter())
            {
                *dst_l = self.tuner_dc_l.process(x);
                *dst_r = self.tuner_dc_r.process(x);
            }
        } else {
            self.chain
                .process_block(&self.in_buf, &mut self.out_l, &mut self.out_r);
        }

        let metro_active = self.metronome.active.load(Relaxed);
        let metro_bpm = self.metronome.bpm.load(Relaxed);

        // One transport snapshot per block (and any pending seek applied here).
        let transport = self.practice.snapshot();
        self.player.begin(&transport);

        // Capture only while the transport is actually advancing, so the captured
        // frames stay aligned one-to-one with project frames. A disarmed take — or
        // a freshly armed one — clears the start latch.
        let generation = self.capture.generation.load(Relaxed);
        if generation != self.capture_generation {
            self.capture_generation = generation;
            self.capture_started = false;
        }
        let capture_active = self.capture.active.load(Relaxed);
        if !capture_active {
            self.capture_started = false;
        }
        let mut capturing =
            capture_active && !self.capture.auto_stop.load(Relaxed) && transport.playing;
        let mut pushed: u64 = 0;
        let mut capture_overflowed = false;

        for (i, &sample) in self.in_buf.iter().enumerate() {
            let cursor = self.player.cursor();

            // Stop capture *before* a valid loop wraps: one loop pass yields one
            // contiguous take with no duplicated or overwritten frames.
            if capturing
                && transport.loop_enabled
                && transport.loop_end > transport.loop_start
                && cursor >= transport.loop_end
            {
                self.capture.auto_stop.store(true, Relaxed);
                capturing = false;
            }

            if capturing {
                if !self.capture_started {
                    self.capture.start_frame.store(cursor as u64, Relaxed);
                    self.capture.frames.store(0, Relaxed);
                    self.capture_started = true;
                }
                // Dry selected-channel sample, before gate/pedals/amp/cab. A full
                // ring is reported (never silently shortened), not blocked on.
                if self.capture_tx.push(sample).is_ok() {
                    pushed = pushed.saturating_add(1);
                } else {
                    capture_overflowed = true;
                }
            }

            let a = sample.abs();
            self.in_env += if a > self.in_env {
                self.attack
            } else {
                self.release
            } * (a - self.in_env);

            // Metronome click, the import bus, and the looper are mixed into the
            // monitor path only (post-capture), so none ever lands in a take. The
            // looper captures/replays the post-trim dry `sample`.
            let click = self.metro_voice.next_sample(metro_active, metro_bpm);
            let frame = self.player.next_frame(&transport);
            let loop_sample = self.looper.process(sample);
            self.take_in[i] = frame.take;
            let out_left = self.out_l[i] + frame.import_l + click + loop_sample;
            let out_right = self.out_r[i] + frame.import_r + click + loop_sample;
            self.out_l[i] = out_left;
            self.out_r[i] = out_right;

            let mono = 0.5 * (out_left + out_right);
            let a = mono.abs();
            self.out_env += if a > self.out_env {
                self.attack
            } else {
                self.release
            } * (a - self.out_env);
        }

        // Reflect any mid-block transport change (recording grew, or auto-stopped
        // at max length) to the UI without another atomic per frame.
        self.looper.publish(&self.looper_ctl);

        if capture_active && self.capture_started {
            self.capture.frames.fetch_add(pushed, Relaxed);
            if capture_overflowed {
                self.capture.overflowed.store(true, Relaxed);
            }
        }

        // The summed raw-take bus is processed by its own rig instance, then
        // added to the monitor. It is intentionally not part of the live chain.
        self.take_chain.process_block(
            &self.take_in[..frames],
            &mut self.take_l[..frames],
            &mut self.take_r[..frames],
        );

        for ((&live_l, &live_r), (&take_l, &take_r)) in self
            .out_l
            .iter()
            .zip(self.out_r.iter())
            .zip(self.take_l.iter().zip(self.take_r.iter()))
            .take(frames)
        {
            // Final output ceiling. Both buses above were already limited by
            // their *own* `master_bus` (the take bus runs a whole second
            // `DspChain`), so their sum can reach twice the ceiling — a hot rig
            // at 1.0 plus a take layer is +3 dB — and the metronome click
            // (0.5) and the looper ride on top of that with nothing after
            // them. Without this the converter clips, and the overload grows
            // with every take layer.
            let (out_left, out_right) = output_stage(live_l, live_r, take_l, take_r);
            let out_mono = 0.5 * (out_left + out_right);

            // Fan the stereo pair out to the device channels: L→0, R→1, any extra
            // channels get the mono sum; a mono device gets the sum.
            for ch in 0..self.out_channels {
                let s = if self.out_channels == 1 {
                    out_mono
                } else {
                    match ch {
                        0 => out_left,
                        1 => out_right,
                        _ => out_mono,
                    }
                };
                let _ = self.producer.push(s);
            }
        }
    }
}

/// Build the input stream for a concrete device sample type.
fn build_input_stream<T>(
    device: &Device,
    cfg: StreamConfig,
    mut state: InputState,
) -> Result<Stream>
where
    T: cpal::SizedSample,
    f32: FromSample<T>,
{
    let mut err_count = 0u64;
    device
        .build_input_stream(
            cfg,
            move |data: &[T], _| state.on_input(data),
            move |e| {
                // An XRUN is transient and cpal recovers; logging every one floods
                // the log (and does file I/O on the audio thread) during a bad
                // patch, so report the first and then only occasionally.
                err_count += 1;
                if err_count == 1 || err_count.is_multiple_of(200) {
                    let msg = format!("input error: {e} (occurrence {err_count})");
                    log_line(&msg);
                }
            },
            None,
        )
        .map_err(|e| anyhow!("input stream: {e}"))
}

/// Build the output stream for a concrete device sample type.
fn build_output_stream<T>(
    device: &Device,
    cfg: StreamConfig,
    mut consumer: Consumer<f32>,
) -> Result<Stream>
where
    T: cpal::SizedSample + FromSample<f32>,
{
    let mut err_count = 0u64;
    device
        .build_output_stream(
            cfg,
            move |data: &mut [T], _| {
                for s in data.iter_mut() {
                    *s = T::from_sample(consumer.pop().unwrap_or(0.0));
                }
            },
            move |e| {
                err_count += 1;
                if err_count == 1 || err_count.is_multiple_of(200) {
                    let msg = format!("output error: {e} (occurrence {err_count})");
                    log_line(&msg);
                }
            },
            None,
        )
        .map_err(|e| anyhow!("output stream: {e}"))
}

#[allow(clippy::too_many_arguments)]
fn build_engine(
    input_device: &Device,
    input_cfg: StreamConfig,
    in_channels: usize,
    guitar_ch: usize,
    in_fmt: cpal::SampleFormat,
    output_device: &Device,
    output_cfg: StreamConfig,
    out_channels: usize,
    out_fmt: cpal::SampleFormat,
    sr: f32,
    params: Arc<Params>,
    levels: Arc<Levels>,
    capture: Arc<CaptureState>,
    tuner: Arc<Tuner>,
    metronome: Arc<Metronome>,
    practice: Arc<Practice>,
    calibration: Arc<InputCalibration>,
    looper: Arc<LooperControl>,
    identity: InputIdentity,
) -> Result<AudioEngine> {
    capture.sample_rate.store(sr as u32, Relaxed);

    let buf_samples = (sr as usize) / 5 * out_channels * 2;
    let (producer, consumer) = RingBuffer::<f32>::new(buf_samples);

    // The live rig, and a second instance for the raw-take bus. They share the
    // same `Params` so every knob/tone change applies to both, but their DSP
    // state is independent.
    let chain = DspChain::new(sr, Arc::clone(&params));
    let take_chain = DspChain::new(sr, Arc::clone(&params));

    // Tuner: when engaged, the rig is bypassed and the dry guitar feeds both the
    // output (a clean signal to tune against) and the pitch/spectrum detector.
    let tuner_detector = TunerDetector::new(sr);

    // Metronome: when engaged, a click is mixed into the monitor output only —
    // added *after* the recording tap so it is never captured in the WAV.
    let metro_voice = MetronomeVoice::new(sr);

    // Practice player: backing track + recorded take, both mixed into the monitor
    // output only (post-record), like the metronome.
    let player = PlayerVoice::new();

    // Phrase looper: buffers are allocated here (before the streams start), and
    // the loop is mixed into the monitor output only, post-record like the click.
    let looper_voice = Looper::new(sr, &looper);

    // Lock-free handoff for swapping the plugin insert in/out without touching the
    // running stream: commands flow UI → audio, displaced inserts flow back to be
    // dropped off the audio thread.
    let (insert_tx, insert_rx) = RingBuffer::<InsertCommand>::new(INSERT_QUEUE_CAP);
    let (dropped_tx, dropped_rx) = RingBuffer::<Box<dyn StereoInsert>>::new(INSERT_QUEUE_CAP);
    let (ext_cab_tx, ext_cab_rx) = RingBuffer::<ExtCabCommand>::new(INSERT_QUEUE_CAP);
    let (ext_dropped_tx, ext_dropped_rx) = RingBuffer::<Box<ExternalIrCab>>::new(INSERT_QUEUE_CAP);
    let (ext_amp_tx, ext_amp_rx) = RingBuffer::<ExtAmpCommand>::new(INSERT_QUEUE_CAP);
    let (ext_amp_dropped_tx, ext_amp_dropped_rx) =
        RingBuffer::<Box<dyn StereoInsert>>::new(INSERT_QUEUE_CAP);
    // Take-bus mirror rings (a second plugin/IR instance per external rig slot).
    let (take_insert_tx, take_insert_rx) = RingBuffer::<InsertCommand>::new(INSERT_QUEUE_CAP);
    let (take_insert_dropped_tx, take_insert_dropped_rx) =
        RingBuffer::<Box<dyn StereoInsert>>::new(INSERT_QUEUE_CAP);
    let (take_ext_cab_tx, take_ext_cab_rx) = RingBuffer::<ExtCabCommand>::new(INSERT_QUEUE_CAP);
    let (take_ext_cab_dropped_tx, take_ext_cab_dropped_rx) =
        RingBuffer::<Box<ExternalIrCab>>::new(INSERT_QUEUE_CAP);
    let (take_ext_amp_tx, take_ext_amp_rx) = RingBuffer::<ExtAmpCommand>::new(INSERT_QUEUE_CAP);
    let (take_ext_amp_dropped_tx, take_ext_amp_dropped_rx) =
        RingBuffer::<Box<dyn StereoInsert>>::new(INSERT_QUEUE_CAP);
    // Timeline-track handoff: commands flow UI → audio, displaced tracks flow back
    // to the control thread so their buffers are never freed in the callback, and
    // install acknowledgements flow audio → UI.
    let (track_tx, track_rx) = RingBuffer::<TrackCommand>::new(TRACK_QUEUE_CAP);
    let (track_dropped_tx, track_dropped_rx) = RingBuffer::<PlayerTrack>::new(TRACK_QUEUE_CAP);
    let (track_ack_tx, track_ack_rx) = RingBuffer::<TrackAck>::new(TRACK_QUEUE_CAP);
    // Dry capture: the callback pushes samples, the writer worker owns the consumer.
    let (capture_tx, capture_rx) = capture_ring(sr);

    // Calibration windows: the audio thread pushes raw window stats, the UI
    // drains them each tick while the wizard is measuring.
    let (cal_tx, cal_rx) = RingBuffer::<WindowStat>::new(CAL_RING_CAPACITY);
    let win_len = (sr * CAL_WINDOW_MS / 1000.0).round().max(1.0) as u32;

    let attack = 1.0 - (-1.0 / (0.001 * sr)).exp();
    let release = 1.0 - (-1.0 / (0.300 * sr)).exp();

    // Reusable scratch buffers for block processing. Pre-sized generously so the
    // audio thread never reallocates for normal device buffer sizes; the `resize`
    // below only grows them on the rare callback that asks for a larger block.
    let in_buf: Vec<f32> = Vec::with_capacity(MAX_BLOCK);
    let out_l: Vec<f32> = vec![0.0; MAX_BLOCK];
    let out_r: Vec<f32> = vec![0.0; MAX_BLOCK];
    let take_in: Vec<f32> = vec![0.0; MAX_BLOCK];
    let take_l: Vec<f32> = vec![0.0; MAX_BLOCK];
    let take_r: Vec<f32> = vec![0.0; MAX_BLOCK];

    let state = InputState {
        chain,
        take_chain,
        tuner_detector,
        metro_voice,
        player,
        attack,
        release,
        in_env: 0.0,
        out_env: 0.0,
        in_channels,
        out_channels,
        guitar_ch,
        in_buf,
        out_l,
        out_r,
        take_in,
        take_l,
        take_r,
        tuner_dc_l: DcBlocker::new(sr),
        tuner_dc_r: DcBlocker::new(sr),
        insert_rx,
        dropped_tx,
        ext_cab_rx,
        ext_dropped_tx,
        ext_amp_rx,
        ext_amp_dropped_tx,
        take_insert_rx,
        take_insert_dropped_tx,
        take_ext_cab_rx,
        take_ext_cab_dropped_tx,
        take_ext_amp_rx,
        take_ext_amp_dropped_tx,
        track_rx,
        track_dropped_tx,
        track_ack_tx,
        capture_tx,
        capture_started: false,
        capture_generation: 0,
        producer,
        levels,
        capture,
        tuner,
        metronome,
        practice,
        looper: looper_voice,
        looper_ctl: looper,
        cal: Arc::clone(&calibration),
        cal_tx,
        trim: TrimState::new(sr, calibration.trim_db.load(Relaxed)),
        win: WindowStat::default(),
        win_len,
    };

    // Build each stream in the sample format the device actually supports. ALSA
    // exposes the Scarlett's capture as S32_LE, so an f32 stream would fail to
    // open; `on_input`/`build_output_stream` convert to and from the engine's f32
    // domain. Unsupported formats are reported clearly rather than silently
    // mis-configured.
    macro_rules! build_in {
        ($t:ty) => {
            build_input_stream::<$t>(input_device, input_cfg, state)?
        };
    }
    let input_stream = match in_fmt {
        cpal::SampleFormat::F32 => build_in!(f32),
        cpal::SampleFormat::F64 => build_in!(f64),
        cpal::SampleFormat::I8 => build_in!(i8),
        cpal::SampleFormat::I16 => build_in!(i16),
        cpal::SampleFormat::I24 => build_in!(cpal::I24),
        cpal::SampleFormat::I32 => build_in!(i32),
        cpal::SampleFormat::U8 => build_in!(u8),
        cpal::SampleFormat::U16 => build_in!(u16),
        cpal::SampleFormat::U24 => build_in!(cpal::U24),
        cpal::SampleFormat::U32 => build_in!(u32),
        other => return Err(anyhow!("input sample format {other} is not supported")),
    };

    macro_rules! build_out {
        ($t:ty) => {
            build_output_stream::<$t>(output_device, output_cfg, consumer)?
        };
    }
    let output_stream = match out_fmt {
        cpal::SampleFormat::F32 => build_out!(f32),
        cpal::SampleFormat::F64 => build_out!(f64),
        cpal::SampleFormat::I8 => build_out!(i8),
        cpal::SampleFormat::I16 => build_out!(i16),
        cpal::SampleFormat::I24 => build_out!(cpal::I24),
        cpal::SampleFormat::I32 => build_out!(i32),
        cpal::SampleFormat::U8 => build_out!(u8),
        cpal::SampleFormat::U16 => build_out!(u16),
        cpal::SampleFormat::U24 => build_out!(cpal::U24),
        cpal::SampleFormat::U32 => build_out!(u32),
        other => return Err(anyhow!("output sample format {other} is not supported")),
    };

    input_stream.play()?;
    output_stream.play()?;

    Ok(AudioEngine {
        _input_stream: input_stream,
        _output_stream: output_stream,
        sample_rate: sr,
        insert_tx,
        dropped_rx,
        ext_cab_tx,
        ext_dropped_rx,
        ext_amp_tx,
        ext_amp_dropped_rx,
        take_insert_tx,
        take_insert_dropped_rx,
        take_ext_cab_tx,
        take_ext_cab_dropped_rx,
        take_ext_amp_tx,
        take_ext_amp_dropped_rx,
        track_tx,
        track_dropped_rx,
        track_ack_rx,
        capture_rx: Some(capture_rx),
        cal_stats_rx: cal_rx,
        identity,
    })
}

#[cfg(test)]
mod tests {
    use super::{block_ranges, output_stage};

    /// The final output stage must hold the ceiling **no matter how many
    /// already-limited buses are summed into it**.
    ///
    /// The defect: each bus went through its own `master_bus` and was bounded
    /// at 1.0, but nothing bounded their *sum*. Live rig + take bus + metronome
    /// click reached the converter at up to 1.5, and the take bus is a whole
    /// second rig, so the overload grew with every layer.
    #[test]
    fn summed_monitor_buses_cannot_exceed_the_ceiling() {
        // The worst realistic case: both rigs at the ceiling, plus the
        // metronome click (CLICK_GAIN = 0.5) and a full-scale loop.
        const CLICK: f32 = 0.5;
        for &(live, take) in &[
            (1.0f32, 1.0f32),
            (1.0, 0.5),
            (1.0, 0.0),
            (1.5, 1.5),
            (4.0, 4.0),
            (1.0e6, 1.0e6),
        ] {
            let (l, r) = output_stage(live, live, take, take);
            assert!(
                l <= 1.0 && l.is_finite(),
                "L ceiling failed at {l} for {live}+{take}"
            );
            assert!(
                r <= 1.0 && r.is_finite(),
                "R ceiling failed at {r} for {live}+{take}"
            );

            // And with the monitor-only buses riding on the live channel.
            let (l, r) = output_stage(live + CLICK, live + CLICK, take, take);
            assert!(
                l <= 1.0 && r <= 1.0,
                "ceiling failed with the click: ({l}, {r}) for {live}+{take}"
            );
        }
    }

    /// A quiet bus must pass through the final stage untouched, so the stage is
    /// only a safety net and not a permanent colouration.
    #[test]
    fn final_stage_is_transparent_at_normal_levels() {
        for v in [0.0f32, 0.1, 0.5, 0.9] {
            let (l, r) = output_stage(v, -v, 0.0, 0.0);
            assert!((l - v).abs() < 1e-6, "L changed a quiet sample: {v} -> {l}");
            assert!((r + v).abs() < 1e-6, "R changed a quiet sample: {v} -> {r}");
        }
    }

    /// An oversized callback is split into preallocated pieces: contiguous,
    /// in-order, each at most `max` frames, covering every frame exactly once.
    #[test]
    fn block_ranges_splits_into_bounded_pieces() {
        assert_eq!(block_ranges(0, 4096).collect::<Vec<_>>(), vec![]);
        assert_eq!(block_ranges(100, 4096).collect::<Vec<_>>(), vec![(0, 100)]);
        assert_eq!(
            block_ranges(4096, 4096).collect::<Vec<_>>(),
            vec![(0, 4096)]
        );
        assert_eq!(
            block_ranges(10_000, 4096).collect::<Vec<_>>(),
            vec![(0, 4096), (4096, 8192), (8192, 10_000)]
        );
        // Every range is bounded and the union is contiguous.
        let mut covered = 0;
        for (start, end) in block_ranges(10_000, 4096) {
            assert_eq!(start, covered, "ranges must be contiguous");
            assert!(end > start && end - start <= 4096, "range must be bounded");
            covered = end;
        }
        assert_eq!(covered, 10_000, "every frame is covered exactly once");
    }

    /// A zero `max` must not panic or loop forever.
    #[test]
    fn block_ranges_handles_zero_max() {
        assert_eq!(
            block_ranges(3, 0).collect::<Vec<_>>(),
            vec![(0, 1), (1, 2), (2, 3)]
        );
    }
}
