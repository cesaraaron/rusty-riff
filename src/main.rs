use anyhow::Result;
use std::sync::Arc;

use rusty_riff::{audio, dsp, looper, practice, preset, recording, ui};

fn main() -> Result<()> {
    rusty_riff::migrate_legacy_config_dir();

    let params = Arc::new(dsp::Params::new());
    let levels = Arc::new(dsp::Levels::new());
    let tuner = Arc::new(dsp::Tuner::new());
    let metronome = Arc::new(dsp::Metronome::new());
    let presets = preset::load_all();
    let capture = Arc::new(recording::CaptureState::new());
    let practice = Arc::new(practice::Practice::new());
    let calibration = Arc::new(audio::InputCalibration::new());
    let looper = Arc::new(looper::LooperControl::new());

    // Optional MIDI expression input for the wah; disabled if no controller or
    // `midi.conf` says so. The handle keeps the connection open for the run.
    let _midi = rusty_riff::midi::start(params.clone());

    // TUI starts immediately; device selection happens inside via modals.
    ui::run(
        params,
        levels,
        tuner,
        metronome,
        presets,
        capture,
        practice,
        calibration,
        looper,
    )?;

    Ok(())
}
