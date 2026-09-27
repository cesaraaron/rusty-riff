//! MIDI input — drive the wah treadle (and, later, other targets) from a
//! controller.
//!
//! A `midir` input connection reads MIDI on its own thread. The callback runs
//! **off the audio thread** and only performs atomic stores into [`Params`], so
//! there is no allocation, lock, or wait anywhere near the callback — the same
//! discipline as the UI/control thread.
//!
//! The bound controller is the standard **Expression** CC (11) by default; it can
//! be changed or disabled in `~/.config/rusty-riff/midi.conf`:
//!
//! ```text
//! enabled = true
//! cc = 11
//! ```
//!
//! A matching CC writes `wah_position` and switches the wah to manual mode, so an
//! expression pedal overrides a preset's auto-wah as soon as it moves.

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

/// Parse `midi.conf` → `(enabled, cc)`. Unknown lines are ignored; a missing or
/// malformed file yields the defaults (enabled, CC 11).
fn parse_midi_conf(text: &str) -> (bool, u8) {
    let mut enabled = true;
    let mut cc = DEFAULT_CC;
    for line in text.lines() {
        let line = line.trim();
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim();
        match key.trim() {
            "enabled" => enabled = !matches!(value, "false" | "0" | "no" | "off"),
            "cc" => {
                if let Ok(v) = value.parse::<u8>()
                    && v <= 127
                {
                    cc = v;
                }
            }
            _ => {}
        }
    }
    (enabled, cc)
}

fn midi_config() -> (bool, u8) {
    let text = dirs::home_dir()
        .map(|h| h.join(".config").join("rusty-riff").join("midi.conf"))
        .and_then(|p| std::fs::read_to_string(p).ok())
        .unwrap_or_default();
    parse_midi_conf(&text)
}

/// Connect to the first available MIDI input and drive the wah from the bound
/// Expression CC. Returns `None` (silently) if MIDI is disabled, no input exists,
/// or the connection fails — the app must run fine without a controller.
pub fn start(params: Arc<Params>) -> Option<MidiHandle> {
    let (enabled, cc) = midi_config();
    if !enabled {
        return None;
    }

    let mut input = MidiInput::new("rusty-riff").ok()?;
    input.ignore(Ignore::None);
    let port = input.ports().into_iter().next()?;
    let name = input
        .port_name(&port)
        .unwrap_or_else(|_| "unknown".to_string());

    let p = params.clone();
    let conn = input
        .connect(
            &port,
            "rusty-riff-wah",
            move |_ts, msg, _| {
                if let Some((controller, value)) = parse_cc(msg)
                    && controller == cc
                {
                    p.wah_position.store(cc_to_unit(value), Relaxed);
                    // An expression pedal overrides a preset's auto-wah.
                    p.wah_mode.store(1.0, Relaxed);
                }
            },
            (),
        )
        .ok()?;

    crate::audio::log_line(&format!("MIDI input '{name}' bound to wah CC {cc}"));
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
    fn midi_conf_parses_enabled_and_cc() {
        assert_eq!(parse_midi_conf(""), (true, 11));
        assert_eq!(parse_midi_conf("cc = 4\n"), (true, 4));
        assert_eq!(parse_midi_conf("enabled = false\n"), (false, 11));
        assert_eq!(parse_midi_conf("enabled=0\ncc = 7"), (false, 7));
        // Out-of-range / junk values fall back.
        assert_eq!(parse_midi_conf("cc = 200"), (true, 11));
        assert_eq!(parse_midi_conf("nonsense\n"), (true, 11));
    }
}
