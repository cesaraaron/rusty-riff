//! rusty-riff — a guitar amplifier and effects simulator.
//!
//! The crate is split into a library (this file) and a thin binary (`main.rs`) so
//! the DSP, preset, recording, and UI modules can be unit-tested and reused
//! without going through the executable entry point.

/// Offline analysis (metrics, synthetic DI corpus, preset rendering) for the
/// fidelity harness and tests — never used on the audio thread.
pub mod analysis;
pub mod audio;
pub mod dsp;
pub mod export;
/// Third-party plugin hosting: CLAP effects as a stereo insert (behind the `clap`
/// feature) and macOS Audio Units as an amp-position override (behind `au`).
#[cfg(any(feature = "clap", feature = "au"))]
pub mod host;
pub mod midi;
pub mod practice;
pub mod preset;
pub mod project;
pub mod recording;
pub mod session;
pub mod ui;

/// One-time migration of the legacy `~/.config/rusty-amp` directory to
/// `~/.config/rusty-riff`, so existing presets, external IRs, sessions, and
/// recovery takes survive the project rename. Best-effort and intended to run
/// once on the control thread at startup.
pub fn migrate_legacy_config_dir() {
    let Some(home) = dirs::home_dir() else {
        return;
    };
    let config = home.join(".config");
    let old = config.join("rusty-amp");
    let new = config.join("rusty-riff");
    if new.exists() || !old.exists() {
        return;
    }
    if std::fs::rename(&old, &new).is_ok() {
        return;
    }
    if copy_dir_recursive(&old, &new).is_ok() {
        std::fs::remove_dir_all(&old).ok();
    }
}

fn copy_dir_recursive(src: &std::path::Path, dst: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let target = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir_recursive(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}
