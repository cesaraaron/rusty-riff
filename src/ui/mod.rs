#[cfg(all(feature = "au", target_os = "macos"))]
mod amp_plugins;
mod calibration;
mod config;
mod draw;
mod input;
mod ir_browser;
mod metronome;
#[cfg(feature = "clap")]
mod plugins;
mod practice;
mod presets;
mod sessions;
mod setup;
mod styles;
mod tuner;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::TryRecvError;

use crate::export::{self as exporter, ExportHandle};
use std::time::Duration;

use anyhow::Result;
use crossterm::{
    event::{self, Event, KeyCode, KeyModifiers},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};

use crate::dsp::{ChainStage, Levels, Metronome, Params, Tuner};
use crate::looper::{LoopState, LooperControl};
use crate::practice::Practice;
use crate::preset::Preset;
use crate::recording::CaptureState;

use config::{ADD_TILE, AMP_END, AMP_START, CHAIN_TILE, PEDALS, PRACTICE_TILE, Panels, pedal_of};
use draw::{
    draw, render_add_pedal_modal, render_amp_modal, render_cab_modal, render_export_progress,
    render_help_modal,
};
use input::{
    PanelMemory, add_pedal, amp_choices, cab_choices, cycle_panel, ensure_focus_visible,
    init_amp_cursor, init_cab_cursor, jump_group, move_selected_stage, nudge, nudge_master,
    panel_of, press_number, remove_pedal, select_amp, select_cab, step_header, step_knob_in_panel,
    toggle_pedal, toggle_stage,
};
use practice::{PracticeUi, SaveContext};
use presets::{
    PathDialogKind, render_path_dialog, render_preset_modal, render_save_dialog, selected_preset,
    visible_len,
};
use sessions::{Action as SessionAction, SessionBrowser};

/// Board membership derived from the live enabled flags (one entry per pedal).
fn sync_board(params: &Params) -> Vec<bool> {
    PEDALS
        .iter()
        .map(|p| (p.enabled)(params).load(std::sync::atomic::Ordering::Relaxed))
        .collect()
}

/// Reset the rig to factory defaults and repair the board + focus, mirroring the
/// preset browser's "Default values" row. Used at startup and for a new session.
fn apply_factory_defaults(
    params: &Params,
    board: &mut Vec<bool>,
    focus: &mut Option<usize>,
    panels: &Panels,
) {
    params.reset_to_defaults();
    *board = sync_board(params);
    if let Some(i) = *focus
        && let Some(pi) = pedal_of(i)
        && !board[pi]
    {
        *focus = Some(AMP_START);
    }
    *focus = ensure_focus_visible(*focus, board, panels, &params.chain_slots());
}

/// Lists devices, logs them, and returns the user's choice — either the saved
/// selection (unless `force_prompt`) or a fresh pick from the modal. Returns
/// `Ok(None)` if the user quits from the picker. `notice` is shown atop the modal
/// when it reopens after a failed audio start.
fn select_devices(
    terminal: &mut ratatui::Terminal<ratatui::backend::CrosstermBackend<std::io::Stdout>>,
    params: &Params,
    levels: &Levels,
    force_prompt: bool,
    notice: Option<&str>,
) -> Result<Option<setup::Selection>> {
    let devices = crate::audio::list_devices()?;
    // Same facts as the modals, in the log file. Never stderr: `run` already owns
    // the alternate screen here, so printing would smear the list over the UI.
    for (i, d) in devices.inputs.iter().enumerate() {
        let line = format!("Input {}: '{}' ({} ch)", i + 1, d.name, d.channels);
        crate::audio::log_line(&line);
    }
    for (i, name) in devices.outputs.iter().enumerate() {
        let line = format!("Output {}: '{name}'", i + 1);
        crate::audio::log_line(&line);
    }

    // Reuse the last good selection unless asked to prompt (`RUSTY_AMP_DEVICE_PROMPT`
    // or the in-app device-change hotkey). Delete `~/.config/rusty-riff/audio.conf`
    // to be prompted again.
    let saved = if force_prompt || std::env::var_os("RUSTY_AMP_DEVICE_PROMPT").is_some() {
        None
    } else {
        crate::audio::load_selection(&devices)
    };

    if let Some((input_idx, guitar_ch, output_idx)) = saved {
        let input = &devices.inputs[input_idx];
        let output = &devices.outputs[output_idx];
        let line = format!(
            "Audio: reusing saved devices — in '{}' ({} ch) ch {} -> out '{}'",
            input.name,
            input.channels,
            guitar_ch + 1,
            output,
        );
        crate::audio::log_line(&line);
        return Ok(Some(setup::Selection {
            input_idx,
            guitar_ch,
            output_idx,
        }));
    }

    let Some(selection) = setup::run(terminal, &devices, params, levels, notice)? else {
        return Ok(None);
    };
    crate::audio::save_selection(
        &devices,
        selection.input_idx,
        selection.guitar_ch,
        selection.output_idx,
    );
    Ok(Some(selection))
}

/// Gather the non-session state a save needs and run it, returning a status
/// message for the session modal.
#[allow(clippy::too_many_arguments)]
fn save_current_session(
    practice_ui: &mut PracticeUi,
    dir: &Path,
    params: &Params,
    practice: &Practice,
    metronome: &Metronome,
    ir_browser: &ir_browser::IrBrowser,
    clap: Option<crate::project::ClapSpec>,
    au: Option<crate::project::AuSpec>,
) -> Option<String> {
    let ctx = SaveContext {
        dir,
        params,
        practice,
        metronome,
        external_ir: ir_browser.loaded_path().map(PathBuf::as_path),
        external_ir_active: params
            .cab_external_active
            .load(std::sync::atomic::Ordering::Relaxed),
        clap,
        au,
    };
    match practice_ui.save_session(ctx) {
        Ok(0) => Some(format!("Saved {}", dir.display())),
        Ok(n) => Some(format!(
            "Saved {} ({n} track(s) without a source were skipped)",
            dir.display()
        )),
        Err(e) => Some(format!("Save failed: {e:#}")),
    }
}

/// Resolve a leading `~` against the home directory.
fn expand_tilde(input: &str) -> PathBuf {
    if let Some(home) = dirs::home_dir() {
        if input == "~" {
            return home;
        }
        if let Some(rest) = input.strip_prefix("~/") {
            return home.join(rest);
        }
    }
    PathBuf::from(input)
}

/// Which span the timeline `E` export renders.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ExportRange {
    /// Tick 0 → last unmuted take + capped effect tail (the original behavior).
    Full,
    /// Exactly the loop region `[loop_start, loop_end)`.
    Loop,
}

impl ExportRange {
    fn toggled(self) -> Self {
        match self {
            Self::Full => Self::Loop,
            Self::Loop => Self::Full,
        }
    }
}

/// Validate an export request and start the render worker. Returns a
/// user-facing error when a faithful render cannot be guaranteed.
fn start_export(
    practice_ui: &PracticeUi,
    input: &str,
    params: &Params,
    ir_browser: &ir_browser::IrBrowser,
    insert: Option<exporter::BuildExternal>,
    amp: Option<exporter::BuildExternal>,
    range_frames: Option<(usize, usize)>,
) -> std::result::Result<ExportHandle, String> {
    let input = input.trim();
    if input.is_empty() {
        return Err("Enter a destination path".to_owned());
    }
    // An AU is loaded but its state could not be snapshotted: refuse rather than
    // render silently through the built-in amp.
    if params
        .amp_external_loaded
        .load(std::sync::atomic::Ordering::Relaxed)
        && amp.is_none()
    {
        return Err(
            "An AU amp is loaded but its state could not be captured; export would not match. Clear it or switch to the built-in amp."
                .to_owned(),
        );
    }
    let dest = expand_tilde(input);
    let ir_path = ir_browser.loaded_path().cloned();
    let ir_active = params
        .cab_external_active
        .load(std::sync::atomic::Ordering::Relaxed);
    let job = practice_ui.build_export_job(
        dest,
        params,
        ir_path,
        ir_active,
        insert,
        amp,
        range_frames,
    )?;
    Ok(exporter::spawn(job))
}

#[allow(clippy::too_many_arguments)]
pub fn run(
    params: Arc<Params>,
    levels: Arc<Levels>,
    tuner: Arc<Tuner>,
    metronome: Arc<Metronome>,
    presets: Vec<Preset>,
    capture: Arc<CaptureState>,
    practice: Arc<Practice>,
    calibration: Arc<crate::audio::InputCalibration>,
    looper: Arc<LooperControl>,
    opaque: bool,
) -> Result<()> {
    // Panel background: transparent (terminal default) unless `--opaque` was
    // passed. Set once here; every panel/modal reads it via `styles::panel_style`.
    styles::set_opaque(opaque);
    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = ratatui::backend::CrosstermBackend::new(stdout);
    let mut terminal = ratatui::Terminal::new(backend)?;

    // ── Session state (persists across a device change) ───────────────────────
    // Focus starts on the practice timeline (the app opens with only that panel
    // and the ever-visible chain ribbon shown).
    let mut focus: Option<usize> = Some(PRACTICE_TILE);
    // Selected stage within the ribbon; follows its stage through moves.
    let mut chain_cursor: ChainStage = ChainStage::Amp;
    // Panel 1: whether the header cursor is on the master-output cell.
    let mut header_on_master = false;
    // Last-focused knob per amp/mic/pedal group, so panel-local `Tab` returns
    // to where you left off.
    let mut panel_mem = PanelMemory::new();
    // Board membership: a pedal is on the board iff it is enabled. Off-board
    // pedals are bypassed in the DSP and hidden from the rig. Rebuilt with
    // `sync_board` whenever a preset rewrites the enabled flags.
    let mut board: Vec<bool> = sync_board(&params);
    let mut add_open = false;
    let mut add_cursor = 0usize;
    // Amp/cab picker modals (`A`/`C`): cursor preselected on the current pick.
    let mut amp_open = false;
    let mut amp_cursor = 0usize;
    let mut cab_open = false;
    let mut cab_cursor = 0usize;
    let mut preset_open = false;
    let mut preset_cursor = 0usize;
    // Type-to-filter query for the preset browser (empty = show everything).
    let mut preset_filter = String::new();
    // `/` search mode: while active, every printable key appends to the filter
    // and the S/D/E/I/F/X commands are disabled.
    let mut preset_search = false;
    // A/B compare: the two most recently applied presets (by name), so `X` in the
    // browser flips between them without leaving the modal.
    let mut applied_preset: Option<String> = None;
    let mut prev_preset: Option<String> = None;
    // Tap-tempo (`;`): sets the delay TIME from the tapped interval.
    let mut tap_tempo = crate::tap_tempo::TapTempo::new();
    let mut presets = presets;
    let mut favorites = crate::preset::load_favorites();
    let mut save_open = false;
    let mut save_name = String::new();
    let mut save_desc = String::new();
    let mut save_field = 0usize; // 0 = name, 1 = description
    let mut save_error: Option<String> = None;
    // Typed-path dialog for preset import/export (opened from the browser).
    let mut path_open: Option<PathDialogKind> = None;
    let mut path_input = String::new();
    let mut path_error: Option<String> = None;
    let mut tick: u64 = 0;
    let mut save_msg: Option<(String, std::time::Instant)> = None;
    let mut tuner_open = false;
    let mut metronome_open = false;
    // Input-calibration wizard (`N`).
    let mut cal_ui = calibration::CalibrationUi::new();
    // Keybinding cheat-sheet modal, toggled with K.
    let mut help_open = false;
    // Which top-level panels are shown (session-only; toggled with 1/2/3). The
    // app opens with only the timeline (plus the always-visible ribbon).
    let mut panels = Panels::timeline_only();
    // Open on the factory rig.
    apply_factory_defaults(&params, &mut board, &mut focus, &panels);
    // Canonical timeline project, owned by the UI and preserved across device
    // changes; decoded playback buffers are rebuilt per engine.
    let mut practice_ui = PracticeUi::new();
    // Session (project) browser modal, toggled with `J`.
    let mut session_browser = SessionBrowser::new();
    // Timeline export (`E`): typed destination + background render worker.
    let mut export_open = false;
    let mut export_input = String::new();
    let mut export_error: Option<String> = None;
    let mut export_handle: Option<ExportHandle> = None;
    let mut export_range = ExportRange::Full;

    // ── Session loop: (re)select devices, start the engine, run the UI ─────────
    // The `O` key drops the engine and loops back here so the picker runs again —
    // that is how input/output/channel can be changed at runtime on any platform.
    let mut force_prompt = false;
    // Surfaced atop the picker when audio fails to start, so the user learns why it
    // reopened and can choose a working device instead of being dropped out.
    let mut start_error: Option<String> = None;
    // Show the recovery prompt once per launch, after the first engine starts.
    let mut recovery_prompted = false;
    'session: loop {
        let selection = match select_devices(
            &mut terminal,
            &params,
            &levels,
            force_prompt,
            start_error.as_deref(),
        )? {
            Some(selection) => selection,
            // User quit from the picker.
            None => {
                disable_raw_mode()?;
                execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
                return Ok(());
            }
        };
        // Any later re-entry must show the picker rather than reuse the saved set.
        force_prompt = true;
        // The picker has now consumed last iteration's error.
        start_error = None;

        // ── Start audio engine ────────────────────────────────────────────────────
        #[cfg_attr(not(feature = "clap"), allow(unused_mut, unused_variables))]
        let mut engine = match crate::audio::start(
            selection.input_idx,
            selection.guitar_ch,
            selection.output_idx,
            Arc::clone(&params),
            Arc::clone(&levels),
            Arc::clone(&capture),
            Arc::clone(&tuner),
            Arc::clone(&metronome),
            Arc::clone(&practice),
            Arc::clone(&calibration),
            Arc::clone(&looper),
        ) {
            Ok(engine) => engine,
            Err(err) => {
                // A saved (or freshly picked) device that won't open must not lock
                // the user out. Log it, reopen the picker with the reason, and let
                // them pick another device or quit. `start` owns no engine on
                // failure, so looping back is safe.
                let msg = format!(
                    "Could not start audio (input #{}, channel {}, output #{}): {err} — pick another device.",
                    selection.input_idx + 1,
                    selection.guitar_ch + 1,
                    selection.output_idx + 1,
                );
                crate::audio::log_line(&msg);
                start_error = Some(msg);
                continue 'session;
            }
        };

        // Apply the saved input calibration for this device. Startup and the `O`
        // device-change flow both pass through here.
        if let Some(entry) = crate::audio::calibration::load_calibration(&engine.input_identity()) {
            calibration
                .trim_db
                .store(entry.trim_db, std::sync::atomic::Ordering::Relaxed);
            if entry.reference_version != crate::audio::calibration::REFERENCE_VERSION {
                save_msg = Some((
                    "input calibration is from an older reference — recalibrate (N)".to_string(),
                    std::time::Instant::now(),
                ));
            }
        }

        // ── Plugin browser (CLAP insert) ──────────────────────────────────────────
        #[cfg(feature = "clap")]
        let mut browser =
            plugins::PluginBrowser::new(engine.sample_rate(), crate::audio::MAX_BLOCK as u32);

        // ── Cabinet-IR browser (external .wav IRs) ────────────────────────────────
        let mut ir_browser = ir_browser::IrBrowser::new(engine.sample_rate());

        // ── Amp-plugin browser (AU amp-position override, macOS) ──────────────────
        #[cfg(all(feature = "au", target_os = "macos"))]
        let mut amp_browser =
            amp_plugins::AmpBrowser::new(engine.sample_rate(), crate::audio::MAX_BLOCK as u32);

        // ── Timeline: reset the transport, keep the session, re-install tracks ─────
        practice.reset();
        practice_ui.attach(&mut engine);

        // Offer recovery of abandoned dry takes once, after the first engine start.
        if !recovery_prompted {
            recovery_prompted = true;
            if !crate::project::list_recovery().is_empty() {
                session_browser.open();
                session_browser.message =
                    Some("Recoverable takes found — Enter restore · D discard".to_owned());
            }
        }

        // ── Main UI loop ──────────────────────────────────────────────────────────
        let mut change_device = false;
        loop {
            tick = tick.wrapping_add(1);
            let blink = (tick / 15).is_multiple_of(2);
            let rec_active = practice_ui.is_recording();

            // Install finished background decodes / capture results before drawing.
            practice_ui.poll(&mut engine, &practice, &capture, &calibration);

            // Poll a running export; reinsert the handle while it is still going.
            if let Some(handle) = export_handle.take() {
                match handle.rx.try_recv() {
                    Ok(Ok(path)) => {
                        save_msg = Some((
                            format!("Exported: {}", path.display()),
                            std::time::Instant::now(),
                        ));
                    }
                    Ok(Err(e)) => {
                        save_msg = Some((format!("Export failed: {e}"), std::time::Instant::now()));
                    }
                    Err(TryRecvError::Empty) => export_handle = Some(handle),
                    Err(TryRecvError::Disconnected) => {
                        save_msg = Some((
                            "Export worker stopped".to_owned(),
                            std::time::Instant::now(),
                        ));
                    }
                }
            }

            // Clear save message after 4 seconds
            if let Some((_, ts)) = &save_msg
                && ts.elapsed().as_secs() >= 4
            {
                save_msg = None;
            }

            // Looper transport readout (footer, since the looper has no panel). It
            // takes precedence over a transient save message so an active loop is
            // always visible; an idle, empty looper shows nothing at all.
            let loop_status = match looper.state() {
                LoopState::Idle if looper.is_empty() => None,
                state => {
                    let secs = looper.len() as f32 / engine.sample_rate().max(1.0);
                    Some(match state {
                        LoopState::Recording => format!("◉ LOOP REC {secs:.1}s — Y stop"),
                        LoopState::Playing if looper.overdub_enabled() => {
                            format!("▶ LOOP {secs:.1}s + DUB — Y stop · . undo")
                        }
                        LoopState::Playing => format!("▶ LOOP {secs:.1}s — Y stop · . undo"),
                        LoopState::Idle => format!("❙❙ LOOP {secs:.1}s — Y play · F clear"),
                    })
                }
            };
            let status = loop_status
                .as_deref()
                .or_else(|| save_msg.as_ref().map(|(msg, _)| msg.as_str()));
            // The loaded plugin (if any) is shown in the header, not the status line, so
            // the help/status footer stays intact while a plugin is active.
            #[cfg(feature = "clap")]
            let plugin_name = browser.loaded_name();
            #[cfg(not(feature = "clap"))]
            let plugin_name: Option<&str> = None;

            // The external IR name is shown in the header (replacing the built-in cab
            // label) only while it is the active cab.
            let ext_cab_name = if params
                .cab_external_active
                .load(std::sync::atomic::Ordering::Relaxed)
            {
                ir_browser.loaded_name()
            } else {
                None
            };

            // The active external amp name replaces the built-in amp label in the header,
            // mirroring the external IR. Only meaningful on the AU-capable build.
            #[cfg(all(feature = "au", target_os = "macos"))]
            let ext_amp_name = if params
                .amp_external_active
                .load(std::sync::atomic::Ordering::Relaxed)
            {
                amp_browser.loaded_name()
            } else {
                None
            };
            #[cfg(not(all(feature = "au", target_os = "macos")))]
            let ext_amp_name: Option<&str> = None;

            terminal.draw(|f| {
                draw(
                    f,
                    &params,
                    &levels,
                    &calibration,
                    focus,
                    &board,
                    rec_active,
                    blink,
                    status,
                    plugin_name,
                    ext_cab_name,
                    ext_amp_name,
                    panels,
                    chain_cursor,
                    header_on_master,
                    Some((&practice, &practice_ui)),
                );
                if add_open {
                    let available: Vec<usize> = (0..PEDALS.len()).filter(|&i| !board[i]).collect();
                    render_add_pedal_modal(f, &available, add_cursor);
                }
                if amp_open {
                    #[cfg(all(feature = "au", target_os = "macos"))]
                    let (au_loaded, au_name) = (
                        params
                            .amp_external_loaded
                            .load(std::sync::atomic::Ordering::Relaxed),
                        amp_browser.loaded_name(),
                    );
                    #[cfg(not(all(feature = "au", target_os = "macos")))]
                    let (au_loaded, au_name): (bool, Option<&str>) = (false, None);
                    render_amp_modal(f, &params, au_name, au_loaded, amp_cursor);
                }
                if cab_open {
                    render_cab_modal(
                        f,
                        &params,
                        ir_browser.loaded_name(),
                        params
                            .cab_external_loaded
                            .load(std::sync::atomic::Ordering::Relaxed),
                        cab_cursor,
                    );
                }
                if preset_open {
                    render_preset_modal(f, &presets, preset_cursor, &favorites, &preset_filter);
                }
                if save_open {
                    render_save_dialog(
                        f,
                        &save_name,
                        &save_desc,
                        save_field,
                        save_error.as_deref(),
                    );
                }
                if let Some(kind) = path_open {
                    render_path_dialog(f, kind, &path_input, path_error.as_deref(), None);
                }
                if export_open {
                    let range_label = practice_ui
                        .export_range_label(&practice, export_range == ExportRange::Loop);
                    render_path_dialog(
                        f,
                        PathDialogKind::SessionExport,
                        &export_input,
                        export_error.as_deref(),
                        Some(&range_label),
                    );
                }
                #[cfg(feature = "clap")]
                if browser.open {
                    browser.render(f);
                }
                if ir_browser.open {
                    use std::sync::atomic::Ordering::Relaxed;
                    // The IR is inert only when an AU is active *and* supplying its own cab
                    // (amp+cab mode); in amp-only mode the built-in cab/IR is in the path.
                    let amp_bypasses_cab = params.amp_external_active.load(Relaxed)
                        && !params.amp_external_amp_only.load(Relaxed);
                    ir_browser.render(f, amp_bypasses_cab);
                }
                #[cfg(all(feature = "au", target_os = "macos"))]
                if amp_browser.open {
                    amp_browser.render(
                        f,
                        params
                            .amp_external_amp_only
                            .load(std::sync::atomic::Ordering::Relaxed),
                    );
                }
                if practice_ui.browser_open {
                    practice_ui.render_browser(f);
                }
                if practice_ui.gain_open() {
                    practice_ui.render_gain_modal(f);
                }
                if practice_ui.move_open() {
                    practice_ui.render_move_modal(f);
                }
                if session_browser.open {
                    session_browser.render(f);
                }
                if tuner_open {
                    tuner::render_tuner(f, &tuner);
                }
                if metronome_open {
                    metronome::render_metronome(f, &metronome, blink);
                }
                if cal_ui.open {
                    cal_ui.render(f, &calibration);
                }
                if help_open {
                    render_help_modal(f);
                }
                if let Some(handle) = &export_handle {
                    render_export_progress(f, handle.percent());
                }
            })?;

            // Drain calibration windows and advance the wizard's timed steps.
            cal_ui.tick(&mut engine, &calibration);

            if event::poll(Duration::from_millis(30))?
                && let Event::Key(key) = event::read()?
            {
                // While an export runs, only Esc (cancel) is accepted.
                if let Some(handle) = &export_handle {
                    if key.code == KeyCode::Esc {
                        handle.cancel();
                    }
                    continue;
                }

                #[cfg(feature = "clap")]
                if browser.open {
                    browser.handle_key(key.code, &mut engine);
                    continue;
                }

                if ir_browser.open {
                    ir_browser.handle_key(key.code, &mut engine, &params);
                    continue;
                }

                #[cfg(all(feature = "au", target_os = "macos"))]
                if amp_browser.open {
                    amp_browser.handle_key(key.code, &mut engine, &params);
                    continue;
                }

                if practice_ui.browser_open {
                    practice_ui.handle_browser_key(key.code, &practice);
                    continue;
                }

                if practice_ui.gain_open() {
                    practice_ui.handle_gain_key(key.code, &mut engine);
                    continue;
                }

                if practice_ui.move_open() {
                    practice_ui.handle_move_key(key.code, &mut engine);
                    continue;
                }

                if export_open {
                    match key.code {
                        KeyCode::Esc => {
                            export_open = false;
                            export_error = None;
                        }
                        KeyCode::Tab | KeyCode::BackTab => {
                            export_range = export_range.toggled();
                            export_error = None;
                        }
                        KeyCode::Backspace => {
                            export_input.pop();
                            export_error = None;
                        }
                        KeyCode::Char(c) => {
                            export_input.push(c);
                            export_error = None;
                        }
                        KeyCode::Enter => {
                            #[allow(unused_mut)]
                            let mut insert: Option<
                                exporter::BuildExternal,
                            > = None;
                            #[allow(unused_mut)]
                            let mut amp: Option<
                                exporter::BuildExternal,
                            > = None;
                            #[cfg(feature = "clap")]
                            if browser.loaded_name().is_some() {
                                match browser.build_export_processor() {
                                    Ok(build) => insert = Some(build),
                                    Err(e) => export_error = Some(e),
                                }
                            }
                            #[cfg(all(feature = "au", target_os = "macos"))]
                            if params
                                .amp_external_loaded
                                .load(std::sync::atomic::Ordering::Relaxed)
                            {
                                match amp_browser.build_export_processor(&params) {
                                    Ok(build) => amp = Some(build),
                                    Err(e) => export_error = Some(e),
                                }
                            }
                            if export_error.is_some() {
                                continue;
                            }
                            let range_frames = if export_range == ExportRange::Loop {
                                match practice_ui.loop_region_frames(&practice) {
                                    Some(r) => Some(r),
                                    None => {
                                        export_error = Some(
                                            "No loop region set — use [ and ] first".to_owned(),
                                        );
                                        continue;
                                    }
                                }
                            } else {
                                None
                            };
                            match start_export(
                                &practice_ui,
                                &export_input,
                                &params,
                                &ir_browser,
                                insert,
                                amp,
                                range_frames,
                            ) {
                                Ok(handle) => {
                                    export_handle = Some(handle);
                                    export_open = false;
                                    export_error = None;
                                }
                                Err(e) => export_error = Some(e),
                            }
                        }
                        _ => {}
                    }
                    continue;
                }

                if session_browser.open {
                    // Capture external-plugin identity/state for a save.
                    #[allow(unused_mut)]
                    let mut clap_spec: Option<crate::project::ClapSpec> = None;
                    #[allow(unused_mut)]
                    let mut au_spec: Option<crate::project::AuSpec> = None;
                    #[cfg(feature = "clap")]
                    if browser.loaded_name().is_some() {
                        clap_spec = browser.export_spec();
                    }
                    #[cfg(all(feature = "au", target_os = "macos"))]
                    if params
                        .amp_external_loaded
                        .load(std::sync::atomic::Ordering::Relaxed)
                    {
                        au_spec = amp_browser.export_spec(&params);
                    }
                    let action = session_browser.handle_key(key.code);
                    match action {
                        SessionAction::None => {}
                        SessionAction::New => {
                            practice_ui.new_session(&mut engine, &practice, &metronome, &capture);
                            apply_factory_defaults(&params, &mut board, &mut focus, &panels);
                            session_browser.refresh();
                            session_browser.open = false;
                        }
                        SessionAction::Save => {
                            if let Some(dir) = practice_ui.session.saved_dir().cloned() {
                                let msg = save_current_session(
                                    &mut practice_ui,
                                    &dir,
                                    &params,
                                    &practice,
                                    &metronome,
                                    &ir_browser,
                                    clap_spec,
                                    au_spec,
                                );
                                session_browser.message = msg;
                                session_browser.refresh();
                            } else {
                                let name = practice_ui.session.name().to_owned();
                                session_browser.prompt_name(&name);
                            }
                        }
                        SessionAction::SaveAs(name) => {
                            let dir = crate::project::default_session_dir(&name)
                                .unwrap_or_else(|| PathBuf::from("./sessions").join(name.clone()));
                            practice_ui.session.set_name(name);
                            let msg = save_current_session(
                                &mut practice_ui,
                                &dir,
                                &params,
                                &practice,
                                &metronome,
                                &ir_browser,
                                clap_spec,
                                au_spec,
                            );
                            session_browser.message = msg;
                            session_browser.refresh();
                            session_browser.view_list();
                        }
                        SessionAction::Load(dir) => {
                            match practice_ui.load_session(
                                &dir,
                                &mut engine,
                                &params,
                                &practice,
                                &metronome,
                                &capture,
                            ) {
                                Ok(external) => {
                                    if let Some(ext) = external {
                                        #[cfg(feature = "clap")]
                                        if let Some(spec) = ext.clap {
                                            browser.restore(spec, &mut engine);
                                        }
                                        #[cfg(all(feature = "au", target_os = "macos"))]
                                        if let Some(spec) = ext.au {
                                            amp_browser.restore(spec, &mut engine, &params);
                                        }
                                        #[cfg(not(any(
                                            feature = "clap",
                                            all(feature = "au", target_os = "macos")
                                        )))]
                                        let _ = ext;
                                    }
                                }
                                Err(e) => {
                                    practice_ui.set_message(format!("Load failed: {e:#}"));
                                }
                            }
                            session_browser.open = false;
                        }
                        SessionAction::Delete(dir) => {
                            match std::fs::remove_dir_all(&dir) {
                                Ok(()) => {
                                    session_browser.message =
                                        Some(format!("Deleted {}", dir.display()));
                                }
                                Err(e) => {
                                    session_browser.message = Some(format!("Delete failed: {e}"));
                                }
                            }
                            session_browser.refresh();
                        }
                        SessionAction::Restore(take) => {
                            practice_ui.restore_recovery(&take);
                            session_browser.open = false;
                        }
                        SessionAction::Discard(take) => {
                            // Never delete a recovery WAV a session track still
                            // points at: that leaves the track with a dangling asset
                            // and the next save fails copying the missing file.
                            if practice_ui.references_recovery(&take.wav) {
                                session_browser.message = Some(
                                    "That take is in the session — remove it there first"
                                        .to_owned(),
                                );
                            } else {
                                crate::project::discard_recovery_file(&take.wav);
                                session_browser.message =
                                    Some(format!("Discarded {}", take.label()));
                            }
                            session_browser.refresh();
                        }
                    }
                    continue;
                }

                if cal_ui.open {
                    cal_ui.handle_key(key.code, &calibration, &engine.input_identity());
                } else if help_open {
                    match key.code {
                        KeyCode::Esc | KeyCode::Char('k') | KeyCode::Char('K') => {
                            help_open = false;
                        }
                        _ => {}
                    }
                } else if tuner_open {
                    match key.code {
                        KeyCode::Esc
                        | KeyCode::Char('t')
                        | KeyCode::Char('T')
                        | KeyCode::Char('q') => {
                            tuner_open = false;
                            tuner
                                .active
                                .store(false, std::sync::atomic::Ordering::Relaxed);
                        }
                        _ => {}
                    }
                } else if metronome_open {
                    // The metronome keeps running after the modal is closed, so the
                    // player can play along; only `active` is toggled here.
                    match key.code {
                        KeyCode::Esc | KeyCode::Char('m') | KeyCode::Char('M') => {
                            metronome_open = false;
                        }
                        KeyCode::Char(' ') | KeyCode::Enter => {
                            metronome.toggle();
                        }
                        KeyCode::Right | KeyCode::Up | KeyCode::Char('+') | KeyCode::Char('=') => {
                            metronome.nudge_bpm(1);
                        }
                        KeyCode::Left | KeyCode::Down | KeyCode::Char('-') => {
                            metronome.nudge_bpm(-1);
                        }
                        _ => {}
                    }
                } else if save_open {
                    match key.code {
                        KeyCode::Esc => {
                            save_open = false;
                            save_error = None;
                        }
                        KeyCode::Tab => {
                            save_field = 1 - save_field;
                        }
                        KeyCode::Enter => {
                            if save_name.trim().is_empty() {
                                save_error = Some("Name cannot be empty".to_string());
                            } else {
                                let preset = crate::preset::Preset::from_params(
                                    save_name.trim().to_string(),
                                    if save_desc.trim().is_empty() {
                                        None
                                    } else {
                                        Some(save_desc.trim().to_string())
                                    },
                                    &params,
                                );
                                match preset.save_to_user_dir() {
                                    Ok(_) => {
                                        presets = crate::preset::load_all();
                                        save_open = false;
                                        save_name.clear();
                                        save_desc.clear();
                                        save_error = None;
                                    }
                                    Err(e) => {
                                        save_error = Some(format!("Save failed: {e}"));
                                    }
                                }
                            }
                        }
                        KeyCode::Backspace => {
                            if save_field == 0 {
                                save_name.pop();
                            } else {
                                save_desc.pop();
                            }
                            save_error = None;
                        }
                        KeyCode::Char(c) => {
                            if save_field == 0 {
                                save_name.push(c);
                            } else {
                                save_desc.push(c);
                            }
                            save_error = None;
                        }
                        _ => {}
                    }
                } else if path_open.is_some() {
                    match key.code {
                        KeyCode::Esc => {
                            path_open = None;
                            path_error = None;
                        }
                        KeyCode::Enter => {
                            let input = path_input.trim();
                            if input.is_empty() {
                                path_error = Some("Enter a file path".to_string());
                            } else if path_open == Some(PathDialogKind::Export) {
                                match selected_preset(&presets, &preset_filter, preset_cursor) {
                                    Some(p) => match p.export_to(&PathBuf::from(input)) {
                                        Ok(dest) => {
                                            path_open = None;
                                            path_error = None;
                                            save_msg = Some((
                                                format!("Exported: {}", dest.display()),
                                                std::time::Instant::now(),
                                            ));
                                        }
                                        Err(e) => {
                                            path_error = Some(format!("Export failed: {e:#}"));
                                        }
                                    },
                                    None => {
                                        path_error = Some("No preset selected".to_string());
                                    }
                                }
                            } else {
                                match crate::preset::Preset::import_from(&PathBuf::from(input)) {
                                    Ok(dest) => {
                                        presets = crate::preset::load_all();
                                        // Land the cursor on the imported preset.
                                        if let Some(pos) = presets
                                            .iter()
                                            .position(|p| p.path.as_ref() == Some(&dest))
                                        {
                                            preset_cursor = pos + 1;
                                        }
                                        path_open = None;
                                        path_error = None;
                                        save_msg = Some((
                                            format!("Imported: {}", dest.display()),
                                            std::time::Instant::now(),
                                        ));
                                    }
                                    Err(e) => {
                                        path_error = Some(format!("Import failed: {e:#}"));
                                    }
                                }
                            }
                        }
                        KeyCode::Backspace => {
                            path_input.pop();
                            path_error = None;
                        }
                        KeyCode::Char(c) => {
                            path_input.push(c);
                            path_error = None;
                        }
                        _ => {}
                    }
                } else if preset_open {
                    // Commands are the upper-case letters (S/D/E/I/F/P); lower-case
                    // and other printable keys append to the type-to-filter query.
                    match key.code {
                        KeyCode::Up => {
                            preset_cursor = preset_cursor.saturating_sub(1);
                        }
                        KeyCode::Down => {
                            let total = visible_len(&presets, &preset_filter) + 1;
                            preset_cursor = (preset_cursor + 1).min(total - 1);
                        }
                        KeyCode::Enter => {
                            let mut applied: Option<String> = None;
                            if preset_cursor == 0 {
                                params.reset_to_defaults();
                            } else if let Some(p) =
                                selected_preset(&presets, &preset_filter, preset_cursor)
                            {
                                p.apply(&params);
                                applied = Some(p.name.clone());
                            }
                            prev_preset = applied_preset.take();
                            applied_preset = applied;
                            // The preset rewrote the enabled flags (and maybe the
                            // chain order), so rebuild the board and repair
                            // focus if it landed on a removed pedal or a
                            // hidden panel.
                            board = sync_board(&params);
                            if let Some(i) = focus
                                && let Some(pi) = pedal_of(i)
                                && !board[pi]
                            {
                                focus = Some(AMP_START);
                            }
                            focus =
                                ensure_focus_visible(focus, &board, &panels, &params.chain_slots());
                            preset_open = false;
                            preset_filter.clear();
                        }
                        // `/` starts a fresh search (while searching, `/` is a
                        // literal filter character, handled by the catch-all).
                        KeyCode::Char('/') if !preset_search => {
                            preset_search = true;
                            preset_filter.clear();
                            preset_cursor = 0;
                        }
                        KeyCode::Char('X') if !preset_search => {
                            // A/B: flip to the other of the two most recently applied
                            // presets (or apply the highlighted one if none yet),
                            // keeping the browser open.
                            let target = if let Some(prev) = prev_preset.clone() {
                                presets.iter().find(|p| p.name == prev)
                            } else {
                                selected_preset(&presets, &preset_filter, preset_cursor)
                            };
                            if let Some(p) = target {
                                p.apply(&params);
                                let new_applied = p.name.clone();
                                prev_preset = applied_preset.take();
                                applied_preset = Some(new_applied);
                                board = sync_board(&params);
                                if let Some(i) = focus
                                    && let Some(pi) = pedal_of(i)
                                    && !board[pi]
                                {
                                    focus = Some(AMP_START);
                                }
                                focus = ensure_focus_visible(
                                    focus,
                                    &board,
                                    &panels,
                                    &params.chain_slots(),
                                );
                            }
                        }
                        KeyCode::Char('S') if !preset_search => {
                            preset_open = false;
                            preset_search = false;
                            save_open = true;
                            save_name.clear();
                            save_desc.clear();
                            save_field = 0;
                            save_error = None;
                        }
                        KeyCode::Char('D') if preset_cursor > 0 && !preset_search => {
                            if let Some(p) =
                                selected_preset(&presets, &preset_filter, preset_cursor)
                                && p.source == crate::preset::PresetSource::User
                            {
                                let _ = p.delete();
                                presets = crate::preset::load_all();
                                preset_cursor =
                                    preset_cursor.min(visible_len(&presets, &preset_filter));
                            }
                        }
                        KeyCode::Char('E') if preset_cursor > 0 && !preset_search => {
                            if let Some(p) =
                                selected_preset(&presets, &preset_filter, preset_cursor)
                            {
                                let stem: String = p
                                    .name
                                    .to_lowercase()
                                    .chars()
                                    .map(|c| if c.is_alphanumeric() { c } else { '_' })
                                    .collect();
                                path_input = format!("./{stem}.toml");
                                path_error = None;
                                path_open = Some(PathDialogKind::Export);
                            }
                        }
                        KeyCode::Char('I') if !preset_search => {
                            path_input.clear();
                            path_error = None;
                            path_open = Some(PathDialogKind::Import);
                        }
                        KeyCode::Char('F') if preset_cursor > 0 && !preset_search => {
                            if let Some(p) =
                                selected_preset(&presets, &preset_filter, preset_cursor)
                            {
                                let name = p.name.clone();
                                if !favorites.remove(&name) {
                                    favorites.insert(name);
                                }
                                crate::preset::save_favorites(&favorites);
                            }
                        }
                        KeyCode::Char('P') if !preset_search => {
                            preset_open = false;
                            preset_filter.clear();
                            preset_search = false;
                        }
                        KeyCode::Esc => {
                            if preset_search {
                                // Leave search mode and drop the filter.
                                preset_filter.clear();
                                preset_cursor = 0;
                                preset_search = false;
                            } else if preset_filter.is_empty() {
                                preset_open = false;
                            } else {
                                preset_filter.clear();
                                preset_cursor = 0;
                            }
                        }
                        KeyCode::Backspace => {
                            if preset_filter.pop().is_some() {
                                preset_cursor =
                                    preset_cursor.min(visible_len(&presets, &preset_filter));
                            }
                        }
                        // Lower-case (and any printable char while searching)
                        // appends to the filter; the upper-case command arms
                        // above are disabled during search.
                        KeyCode::Char(c)
                            if (!c.is_ascii_uppercase() || preset_search)
                                && !key.modifiers.contains(KeyModifiers::CONTROL)
                                && !key.modifiers.contains(KeyModifiers::ALT) =>
                        {
                            preset_filter.push(c);
                            preset_cursor =
                                preset_cursor.min(visible_len(&presets, &preset_filter));
                        }
                        _ => {}
                    }
                } else if add_open {
                    let available: Vec<usize> = (0..PEDALS.len()).filter(|&i| !board[i]).collect();
                    match key.code {
                        KeyCode::Up => add_cursor = add_cursor.saturating_sub(1),
                        KeyCode::Down if !available.is_empty() => {
                            add_cursor = (add_cursor + 1).min(available.len() - 1);
                        }
                        KeyCode::Enter => {
                            if let Some(&pi) = available.get(add_cursor) {
                                add_pedal(&params, &mut board, pi);
                                focus = Some(PEDALS[pi].start);
                            }
                            add_open = false;
                        }
                        KeyCode::Esc => add_open = false,
                        _ => {}
                    }
                } else if amp_open {
                    let total = amp_choices(
                        params
                            .amp_external_loaded
                            .load(std::sync::atomic::Ordering::Relaxed),
                    );
                    match key.code {
                        KeyCode::Up => amp_cursor = amp_cursor.saturating_sub(1),
                        KeyCode::Down => {
                            amp_cursor = (amp_cursor + 1).min(total.saturating_sub(1));
                        }
                        KeyCode::Enter => {
                            select_amp(
                                &params,
                                amp_cursor,
                                params
                                    .amp_external_loaded
                                    .load(std::sync::atomic::Ordering::Relaxed),
                            );
                            // A new model may expose fewer knobs than the old one;
                            // move a now-hidden amp focus to its last real knob.
                            let hidden = focus.filter(|&f| {
                                (AMP_START..AMP_END).contains(&f)
                                    && f - AMP_START >= params.amp_knob_count()
                            });
                            if hidden.is_some() {
                                focus = Some(AMP_START + params.amp_knob_count() - 1);
                            }
                            amp_open = false;
                        }
                        KeyCode::Esc | KeyCode::Char('a') | KeyCode::Char('A') => {
                            amp_open = false;
                        }
                        _ => {}
                    }
                } else if cab_open {
                    let total = cab_choices(
                        params
                            .cab_external_loaded
                            .load(std::sync::atomic::Ordering::Relaxed),
                    );
                    match key.code {
                        KeyCode::Up => cab_cursor = cab_cursor.saturating_sub(1),
                        KeyCode::Down => {
                            cab_cursor = (cab_cursor + 1).min(total.saturating_sub(1));
                        }
                        KeyCode::Enter => {
                            select_cab(
                                &params,
                                cab_cursor,
                                params
                                    .cab_external_loaded
                                    .load(std::sync::atomic::Ordering::Relaxed),
                            );
                            cab_open = false;
                        }
                        KeyCode::Esc | KeyCode::Char('c') | KeyCode::Char('C') => {
                            cab_open = false;
                        }
                        _ => {}
                    }
                } else {
                    match key.code {
                        // ── Timeline (when the pane owns focus) ────────────────────
                        KeyCode::Char(' ') if focus == Some(PRACTICE_TILE) => {
                            practice_ui.toggle_play(&practice);
                        }
                        KeyCode::Char('m') | KeyCode::Char('M') if focus == Some(PRACTICE_TILE) => {
                            practice_ui.toggle_selected_mute(&mut engine);
                        }
                        KeyCode::Enter if focus == Some(PRACTICE_TILE) => {
                            practice_ui.go_to_start(&practice);
                        }
                        // Shift+↑/↓ zoom the time axis around the playhead. (No
                        // Ctrl bindings: Kitty resizes fonts on Ctrl+ +/-, and
                        // macOS claims Ctrl+arrows.)
                        KeyCode::Up
                            if focus == Some(PRACTICE_TILE)
                                && key.modifiers.contains(KeyModifiers::SHIFT) =>
                        {
                            practice_ui.zoom_in();
                        }
                        KeyCode::Down
                            if focus == Some(PRACTICE_TILE)
                                && key.modifiers.contains(KeyModifiers::SHIFT) =>
                        {
                            practice_ui.zoom_out();
                        }
                        KeyCode::Up if focus == Some(PRACTICE_TILE) => {
                            practice_ui.move_selection(false);
                        }
                        KeyCode::Down if focus == Some(PRACTICE_TILE) => {
                            practice_ui.move_selection(true);
                        }
                        KeyCode::Left if focus == Some(PRACTICE_TILE) => {
                            practice_ui.seek_by(&practice, -1);
                        }
                        KeyCode::Right if focus == Some(PRACTICE_TILE) => {
                            practice_ui.seek_by(&practice, 1);
                        }
                        KeyCode::Char('+') | KeyCode::Char('=') if focus == Some(PRACTICE_TILE) => {
                            practice_ui.cycle_seek_step(1);
                        }
                        KeyCode::Char('-') if focus == Some(PRACTICE_TILE) => {
                            practice_ui.cycle_seek_step(-1);
                        }
                        KeyCode::Char('g') | KeyCode::Char('G') if focus == Some(PRACTICE_TILE) => {
                            practice_ui.open_gain_edit();
                        }
                        KeyCode::Char('h') | KeyCode::Char('H') if focus == Some(PRACTICE_TILE) => {
                            practice_ui.open_move_edit();
                        }
                        KeyCode::Char('[') if focus == Some(PRACTICE_TILE) => {
                            practice_ui.set_loop_start(&practice);
                        }
                        KeyCode::Char(']') if focus == Some(PRACTICE_TILE) => {
                            practice_ui.set_loop_end(&practice);
                        }
                        KeyCode::Char('l') | KeyCode::Char('L') if focus == Some(PRACTICE_TILE) => {
                            practice_ui.toggle_loop(&practice);
                        }
                        // `z` cycles the selected row's height; `v` cycles the
                        // waveform glyph style. These were `Tab`/`Shift+Tab` until
                        // `Tab` became the global panel walk, so they are
                        // timeline-local now and shadow global `Z`/`V` only while
                        // the timeline has focus -- the same pattern as `g`/`h`/`m`.
                        KeyCode::Char('z') | KeyCode::Char('Z') if focus == Some(PRACTICE_TILE) => {
                            practice_ui.tab_zoom();
                        }
                        KeyCode::Char('v') | KeyCode::Char('V') if focus == Some(PRACTICE_TILE) => {
                            practice_ui.cycle_glyphs();
                        }
                        // `Shift+A` toggles the waveform's amplitude mapping
                        // (normalized / absolute); plain `A` still opens the amp
                        // browser elsewhere.
                        KeyCode::Char('a') | KeyCode::Char('A')
                            if focus == Some(PRACTICE_TILE)
                                && key.modifiers.contains(KeyModifiers::SHIFT) =>
                        {
                            practice_ui.cycle_gain();
                        }
                        KeyCode::Delete | KeyCode::Backspace if focus == Some(PRACTICE_TILE) => {
                            practice_ui.delete_selected(&mut engine, &practice, &capture);
                        }
                        // ── Panels: number keys focus first, show second, hide ──
                        // `1` live order · `2` amp/cab · `3` timeline · `4` pedals.
                        KeyCode::Char('b') | KeyCode::Char('B') => {
                            practice_ui.open_browser();
                        }
                        KeyCode::Char('j') | KeyCode::Char('J') => {
                            session_browser.open();
                        }
                        KeyCode::Char('e') | KeyCode::Char('E') => {
                            // Timeline export. (Preset export is handled inside the
                            // preset browser and takes precedence there.)
                            let stem: String = practice_ui
                                .session
                                .name()
                                .to_lowercase()
                                .chars()
                                .map(|c| if c.is_alphanumeric() { c } else { '_' })
                                .collect();
                            export_input = practice_ui
                                .session
                                .saved_dir()
                                .map(|d| d.join(format!("{stem}.wav")))
                                .map_or_else(
                                    || format!("./{stem}.wav"),
                                    |p| p.to_string_lossy().into_owned(),
                                );
                            export_error = None;
                            export_range = ExportRange::Full;
                            export_open = true;
                        }
                        KeyCode::Char('1') => {
                            (panels, focus) =
                                press_number(1, focus, &board, &panels, &params.chain_slots());
                        }
                        KeyCode::Char('2') => {
                            (panels, focus) =
                                press_number(2, focus, &board, &panels, &params.chain_slots());
                        }
                        KeyCode::Char('3') => {
                            (panels, focus) =
                                press_number(3, focus, &board, &panels, &params.chain_slots());
                        }
                        KeyCode::Char('4') => {
                            (panels, focus) =
                                press_number(4, focus, &board, &panels, &params.chain_slots());
                        }
                        KeyCode::Char('q') => {
                            if practice_ui.is_recording() {
                                practice_ui.abort_capture(&capture);
                            }
                            break;
                        }
                        KeyCode::Char('k') | KeyCode::Char('K') => {
                            help_open = true;
                        }
                        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            if practice_ui.is_recording() {
                                practice_ui.abort_capture(&capture);
                            }
                            break;
                        }
                        // Reopen the device picker at runtime. The engine is dropped
                        // when this session loops, freeing the devices for the picker.
                        KeyCode::Char('o') | KeyCode::Char('O') => {
                            use std::sync::atomic::Ordering::Relaxed;
                            // A fresh engine starts with no external plugin/IR/amp, so
                            // clear the flags that advertise them. Any in-flight take is
                            // aborted rather than finalized at the wrong rate.
                            if practice_ui.is_recording() {
                                practice_ui.abort_capture(&capture);
                            }
                            params.cab_external_loaded.store(false, Relaxed);
                            params.cab_external_active.store(false, Relaxed);
                            params.amp_external_loaded.store(false, Relaxed);
                            params.amp_external_active.store(false, Relaxed);
                            params.amp_external_amp_only.store(false, Relaxed);
                            change_device = true;
                            break;
                        }
                        KeyCode::Char('r') | KeyCode::Char('R') => {
                            practice_ui.arm_or_stop(&mut engine, &practice, &capture);
                        }
                        KeyCode::Char('p') | KeyCode::Char('P') => {
                            preset_open = true;
                            preset_cursor = 0;
                            preset_filter.clear();
                        }
                        KeyCode::Char('t') | KeyCode::Char('T') => {
                            tuner_open = true;
                            tuner
                                .active
                                .store(true, std::sync::atomic::Ordering::Relaxed);
                        }
                        KeyCode::Char('m') | KeyCode::Char('M') => {
                            metronome_open = true;
                        }
                        KeyCode::Char('n') | KeyCode::Char('N') => {
                            if !cal_ui.open(&calibration, practice_ui.is_recording()) {
                                save_msg = Some((
                                    "Can't calibrate while recording".to_string(),
                                    std::time::Instant::now(),
                                ));
                            }
                        }
                        // Looper (monitor-only). `Y` is the record/pause/resume
                        // transport; `F` clears, `,` toggles overdub, `.` undoes
                        // the last overdub layer.
                        KeyCode::Char('y') | KeyCode::Char('Y') => match looper.state() {
                            LoopState::Recording | LoopState::Playing => looper.request_stop(),
                            LoopState::Idle => looper.request_record(),
                        },
                        KeyCode::Char('f') | KeyCode::Char('F') => looper.request_clear(),
                        KeyCode::Char(',') => {
                            looper.toggle_overdub();
                        }
                        KeyCode::Char('.') => looper.request_undo(),
                        // Tap-tempo: `;` taps; two or more steady taps set the
                        // delay TIME (0–500 ms) to the tapped interval.
                        KeyCode::Char(';') => {
                            if let Some(bpm) = tap_tempo.tap(std::time::Instant::now()) {
                                let secs = 60.0 / bpm;
                                params.delay_time.store(
                                    (secs / 0.5).clamp(0.0, 1.0),
                                    std::sync::atomic::Ordering::Relaxed,
                                );
                                save_msg = Some((
                                    format!("TAP {bpm:.1} BPM  (delay {:.0} ms)", secs * 1000.0),
                                    std::time::Instant::now(),
                                ));
                            }
                        }
                        #[cfg(feature = "clap")]
                        KeyCode::Char('v') | KeyCode::Char('V') => browser.open(),
                        #[cfg(all(feature = "au", target_os = "macos"))]
                        KeyCode::Char('u') | KeyCode::Char('U') => amp_browser.open(),
                        // Live A/B between the loaded AU amp and the built-in amp (no modal).
                        #[cfg(all(feature = "au", target_os = "macos"))]
                        KeyCode::Char('z') | KeyCode::Char('Z') => {
                            use std::sync::atomic::Ordering::Relaxed;
                            if params.amp_external_loaded.load(Relaxed) {
                                let now = !params.amp_external_active.load(Relaxed);
                                params.amp_external_active.store(now, Relaxed);
                            }
                        }
                        KeyCode::Char('i') | KeyCode::Char('I') => ir_browser.open(),
                        // Live A/B between the loaded IR and the built-in cab (no modal).
                        KeyCode::Char('x') | KeyCode::Char('X') => {
                            use std::sync::atomic::Ordering::Relaxed;
                            if params.cab_external_loaded.load(Relaxed) {
                                let now = !params.cab_external_active.load(Relaxed);
                                params.cab_external_active.store(now, Relaxed);
                            }
                        }
                        // Studio-master width: neutral reference ↔ the historic
                        // studio widening. The output limiter stays on either way.
                        KeyCode::Char('w') | KeyCode::Char('W') => {
                            use std::sync::atomic::Ordering::Relaxed;
                            let (width, label) =
                                crate::dsp::toggle_master_width(params.master_width.load(Relaxed));
                            params.master_width.store(width, Relaxed);
                            save_msg =
                                Some((format!("Master width: {label}"), std::time::Instant::now()));
                        }
                        KeyCode::Char('s') | KeyCode::Char('S') => {
                            save_open = true;
                            save_name.clear();
                            save_desc.clear();
                            save_field = 0;
                            save_error = None;
                        }
                        // Quick-add: on the pedalboard, `a` opens the add-pedal
                        // modal; elsewhere `a` still opens the amp browser.
                        KeyCode::Char('a') | KeyCode::Char('A') if panel_of(focus) == 4 => {
                            add_open = true;
                            add_cursor = 0;
                        }
                        KeyCode::Char('a') | KeyCode::Char('A') => {
                            amp_open = true;
                            amp_cursor = init_amp_cursor(&params);
                        }
                        KeyCode::Char('c') | KeyCode::Char('C') => {
                            cab_open = true;
                            cab_cursor = init_cab_cursor(&params);
                        }
                        // `Tab` / `Shift+Tab` cycle focus through the visible
                        // panels (1 ribbon, 2 amp/cab, 3 timeline, 4 pedalboard),
                        // skipping hidden ones. Panels are shown/hidden with the
                        // number keys; arrows move within a panel.
                        KeyCode::Tab => {
                            focus = cycle_panel(
                                focus,
                                &board,
                                &panels,
                                &params.chain_slots(),
                                1,
                                &mut panel_mem,
                                params.amp_knob_count(),
                            );
                        }
                        KeyCode::BackTab => {
                            focus = cycle_panel(
                                focus,
                                &board,
                                &panels,
                                &params.chain_slots(),
                                -1,
                                &mut panel_mem,
                                params.amp_knob_count(),
                            );
                        }
                        // Shift+←/→ jumps between sub-groups within the focused
                        // panel: amp <-> cab/mic in panel 2, pedal-to-pedal (and
                        // `+ ADD`) in panel 4. The ribbon and timeline own their
                        // arrows, so it is a no-op there.
                        KeyCode::Right if key.modifiers.contains(KeyModifiers::SHIFT) => {
                            focus = jump_group(
                                focus,
                                &board,
                                &params.chain_slots(),
                                1,
                                params.amp_knob_count(),
                            );
                        }
                        KeyCode::Left if key.modifiers.contains(KeyModifiers::SHIFT) => {
                            focus = jump_group(
                                focus,
                                &board,
                                &params.chain_slots(),
                                -1,
                                params.amp_knob_count(),
                            );
                        }
                        // ←/→ inside the focused panel: the ribbon walks its
                        // stages and then the master cell, panels 2/4 walk their
                        // knobs, and the timeline's seek arms above already
                        // claimed it there.
                        KeyCode::Right if focus == Some(CHAIN_TILE) => {
                            (chain_cursor, header_on_master) = step_header(
                                &params.chain_slots(),
                                &board,
                                chain_cursor,
                                header_on_master,
                                1,
                            );
                        }
                        KeyCode::Left if focus == Some(CHAIN_TILE) => {
                            (chain_cursor, header_on_master) = step_header(
                                &params.chain_slots(),
                                &board,
                                chain_cursor,
                                header_on_master,
                                -1,
                            );
                        }
                        KeyCode::Right => {
                            focus = step_knob_in_panel(
                                focus,
                                &board,
                                &params.chain_slots(),
                                1,
                                params.amp_knob_count(),
                            );
                        }
                        KeyCode::Left => {
                            focus = step_knob_in_panel(
                                focus,
                                &board,
                                &params.chain_slots(),
                                -1,
                                params.amp_knob_count(),
                            );
                        }
                        KeyCode::Up | KeyCode::Char('+') | KeyCode::Char('=') => match focus {
                            Some(CHAIN_TILE) if header_on_master => nudge_master(&params, 0.05),
                            Some(ADD_TILE | PRACTICE_TILE | CHAIN_TILE) | None => {}
                            Some(i) => nudge(&params, i, 0.05),
                        },
                        KeyCode::Down | KeyCode::Char('-') => match focus {
                            Some(CHAIN_TILE) if header_on_master => nudge_master(&params, -0.05),
                            Some(ADD_TILE | PRACTICE_TILE | CHAIN_TILE) | None => {}
                            Some(i) => nudge(&params, i, -0.05),
                        },
                        KeyCode::Enter if focus == Some(ADD_TILE) => {
                            add_open = true;
                            add_cursor = 0;
                        }
                        KeyCode::Char('d') | KeyCode::Char('D') => {
                            if let Some(i) = focus
                                && let Some(pi) = pedal_of(i)
                            {
                                remove_pedal(&params, &mut board, pi);
                                focus = Some(ADD_TILE);
                            }
                        }
                        // Reorder the chain from the ribbon: move the selected
                        // stage one slot earlier / later. Only while a stage (not
                        // the master cell) is selected. The timeline's `[`/`]`
                        // arms above match first while it owns focus.
                        KeyCode::Char('[') if focus == Some(CHAIN_TILE) && !header_on_master => {
                            move_selected_stage(&params, &board, chain_cursor, -1);
                        }
                        KeyCode::Char(']') if focus == Some(CHAIN_TILE) && !header_on_master => {
                            move_selected_stage(&params, &board, chain_cursor, 1);
                        }
                        // Space bypasses the selected stage; on the master cell it
                        // resets the rig output to unity, the one neutral value.
                        KeyCode::Char(' ') if focus == Some(CHAIN_TILE) && !header_on_master => {
                            toggle_stage(&params, &board, chain_cursor);
                        }
                        KeyCode::Char(' ') if focus == Some(CHAIN_TILE) => {
                            params.master_output.store(
                                crate::dsp::DEFAULT_MASTER_OUTPUT,
                                std::sync::atomic::Ordering::Relaxed,
                            );
                            save_msg =
                                Some(("Master: +0.0 dB".to_string(), std::time::Instant::now()));
                        }
                        KeyCode::Char(' ') => match focus {
                            Some(ADD_TILE) => {
                                add_open = true;
                                add_cursor = 0;
                            }
                            Some(PRACTICE_TILE) => {}
                            Some(i) => toggle_pedal(&params, i),
                            None => {}
                        },
                        _ => {}
                    }
                }
            }
        }
        if !change_device {
            break 'session;
        }
    }

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    Ok(())
}
