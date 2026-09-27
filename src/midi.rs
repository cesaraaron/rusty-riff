//! MIDI input — drive pedals/knobs from a controller.
//!
//! A `midir` input connection reads MIDI on its own thread. The callback runs
//! **off the audio thread** and only performs atomic stores into [`Params`], so
//! there is no allocation, lock, or wait anywhere near the callback — the same
//! discipline as the UI/control thread.
//!
//! Control Changes are bound to targets in `~/.config/rusty-riff/midi.conf`. The
//! legacy single binding (`cc = 11`) drives the wah treadle; add more as
//! `<cc> = <target>`:
//!
//! ```text
//! enabled = true
//! cc = 11                 # wah treadle (legacy shorthand)
//! 20 = delay_mix
//! 21 = reverb_mix
//! ```
//!
//! A bound CC that drives `wah_position` also switches the wah to manual mode, so
//! an expression pedal overrides a preset's auto-wah as soon as it moves. A
//! learn/bind screen is not built yet — bindings are edited in the config file.

use std::sync::Arc;
use std::sync::atomic::Ordering::Relaxed;

use midir::{Ignore, MidiInput};

use crate::dsp::Params;

/// MIDI standard "Expression" controller, used when no config overrides it.
const DEFAULT_CC: u8 = 11;

/// Keeps the MIDI connection alive for the process lifetime; dropping it closes
/// the port.
pub struct MidiHandle {
    _conn: midir::MidiInputConnection<()>,
    /// The input port we connected to (for logging/UI).
    pub port: String,
}

/// A knob/parameter a CC can drive.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MidiTarget {
    WahPosition,
    WahMode,
    DelayTime,
    DelayFeedback,
    DelayMix,
    ReverbMix,
    BoostGain,
    CompSustain,
    TsDrive,
    DsDrive,
    ChorusMix,
    FlangerMix,
    PhaserMix,
    MasterWidth,
}

impl MidiTarget {
    /// Parse a config target name.
    fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "wah_position" => Self::WahPosition,
            "wah_mode" => Self::WahMode,
            "delay_time" => Self::DelayTime,
            "delay_feedback" => Self::DelayFeedback,
            "delay_mix" => Self::DelayMix,
            "reverb_mix" => Self::ReverbMix,
            "boost_gain" => Self::BoostGain,
            "comp_sustain" => Self::CompSustain,
            "ts_drive" => Self::TsDrive,
            "ds_drive" => Self::DsDrive,
            "chorus_mix" => Self::ChorusMix,
            "flanger_mix" => Self::FlangerMix,
            "phaser_mix" => Self::PhaserMix,
            "master_width" => Self::MasterWidth,
            _ => return None,
        })
    }

    /// Store the mapped value (0–1) into the target parameter.
    fn write(self, p: &Params, value: f32) {
        let value = value.clamp(0.0, 1.0);
        let target = match self {
            Self::WahPosition => &p.wah_position,
            Self::WahMode => &p.wah_mode,
            Self::DelayTime => &p.delay_time,
            Self::DelayFeedback => &p.delay_feedback,
            Self::DelayMix => &p.delay_mix,
            Self::ReverbMix => &p.rev_mix,
            Self::BoostGain => &p.boost_gain,
            Self::CompSustain => &p.cmp_sustain,
            Self::TsDrive => &p.ts_drive,
            Self::DsDrive => &p.ds_drive,
            Self::ChorusMix => &p.ch_mix,
            Self::FlangerMix => &p.fl_mix,
            Self::PhaserMix => &p.ph_mix,
            Self::MasterWidth => &p.master_width,
        };
        target.store(value, Relaxed);
        // An expression pedal overrides a preset's auto-wah.
        if self == Self::WahPosition {
            p.wah_mode.store(1.0, Relaxed);
        }
    }
}

/// Extract `(controller, value)` from a 3-byte Control Change message.
pub fn parse_cc(msg: &[u8]) -> Option<(u8, u8)> {
    if msg.len() == 3 && msg[0] & 0xF0 == 0xB0 {
        Some((msg[1] & 0x7F, msg[2] & 0x7F))
    } else {
        None
    }
}

/// Map a 7-bit CC value to 0–1.
pub fn cc_to_unit(value: u8) -> f32 {
    (value & 0x7F) as f32 / 127.0
}

/// Parse `midi.conf` → `(enabled, bindings)`. Unknown lines/targets are ignored;
/// a missing or empty file yields the default (enabled, CC 11 → wah treadle).
fn parse_midi_conf(text: &str) -> (bool, Vec<(u8, MidiTarget)>) {
    let mut enabled = true;
    let mut bindings: Vec<(u8, MidiTarget)> = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let (key, value) = (key.trim(), value.trim());
        match key {
            "enabled" => enabled = !matches!(value, "false" | "0" | "no" | "off"),
            // Legacy shorthand: `cc = <number>` binds the wah treadle.
            "cc" => {
                if let Ok(cc) = value.parse::<u8>()
                    && cc <= 127
                {
                    bindings.push((cc, MidiTarget::WahPosition));
                }
            }
            _ => {
                if let Ok(cc) = key.parse::<u8>()
                    && cc <= 127
                    && let Some(target) = MidiTarget::parse(value)
                {
                    bindings.push((cc, target));
                }
            }
        }
    }
    if bindings.is_empty() {
        bindings.push((DEFAULT_CC, MidiTarget::WahPosition));
    }
    (enabled, bindings)
}

fn midi_config() -> (bool, Vec<(u8, MidiTarget)>) {
    let text = dirs::home_dir()
        .map(|h| h.join(".config").join("rusty-riff").join("midi.conf"))
        .and_then(|p| std::fs::read_to_string(p).ok())
        .unwrap_or_default();
    parse_midi_conf(&text)
}

/// Connect to the first available MIDI input and drive the bound targets. Returns
/// `None` (silently) if MIDI is disabled, no input exists, or the connection
/// fails — the app must run fine without a controller.
pub fn start(params: Arc<Params>) -> Option<MidiHandle> {
    let (enabled, bindings) = midi_config();
    if !enabled || bindings.is_empty() {
        return None;
    }

    let mut input = MidiInput::new("rusty-riff").ok()?;
    input.ignore(Ignore::None);
    let port = input.ports().into_iter().next()?;
    let name = input
        .port_name(&port)
        .unwrap_or_else(|_| "unknown".to_string());

    let p = params.clone();
    let summary: Vec<String> = bindings.iter().map(|(cc, _)| format!("CC{cc}")).collect();
    let conn = input
        .connect(
            &port,
            "rusty-riff-midi",
            move |_ts, msg, _| {
                if let Some((controller, value)) = parse_cc(msg) {
                    let unit = cc_to_unit(value);
                    for &(cc, target) in &bindings {
                        if cc == controller {
                            target.write(&p, unit);
                        }
                    }
                }
            },
            (),
        )
        .ok()?;

    crate::audio::log_line(&format!(
        "MIDI input '{name}' bound to {}",
        summary.join(", ")
    ));
    Some(MidiHandle {
        _conn: conn,
        port: name,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_control_change_only() {
        assert_eq!(parse_cc(&[0xB0, 11, 64]), Some((11, 64))); // CC on channel 0
        assert_eq!(parse_cc(&[0xB3, 11, 127]), Some((11, 127))); // any channel
        assert_eq!(parse_cc(&[0x90, 60, 100]), None); // note-on
        assert_eq!(parse_cc(&[0xB0, 11]), None); // too short
        assert_eq!(parse_cc(&[0xB0, 11, 64, 0]), None); // too long
    }

    #[test]
    fn cc_value_maps_to_unit_range() {
        assert_eq!(cc_to_unit(0), 0.0);
        assert_eq!(cc_to_unit(127), 1.0);
        assert!((cc_to_unit(64) - 64.0 / 127.0).abs() < 1e-6);
    }

    #[test]
    fn midi_conf_defaults_to_wah_on_cc11() {
        assert_eq!(
            parse_midi_conf(""),
            (true, vec![(11, MidiTarget::WahPosition)])
        );
        assert!(
            !parse_midi_conf("enabled = false\n").0,
            "enabled flag ignored"
        );
    }

    #[test]
    fn midi_conf_parses_legacy_and_named_bindings() {
        let (en, b) = parse_midi_conf("cc = 4\n");
        assert!(en);
        assert_eq!(b, vec![(4, MidiTarget::WahPosition)]);

        let (_, b) = parse_midi_conf("20 = delay_mix\n21 = reverb_mix\n");
        assert_eq!(
            b,
            vec![(20, MidiTarget::DelayMix), (21, MidiTarget::ReverbMix)]
        );

        // Mixed legacy + named; unknown target/cc ignored.
        let (_, b) = parse_midi_conf("cc = 11\n7 = boost_gain\n8 = nope\n200 = delay_mix\n");
        assert_eq!(
            b,
            vec![(11, MidiTarget::WahPosition), (7, MidiTarget::BoostGain)]
        );
    }
}
