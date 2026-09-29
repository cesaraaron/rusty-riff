//! Input-calibration wizard modal (`N`).
//!
//! A small state machine over the shared [`InputCalibration`] state: intro →
//! noise → capture → result/error. The audio thread only applies the trim and
//! pushes raw window stats; all decision-making and IO happen here and in
//! [`crate::audio::calibration`].

use crossterm::event::KeyCode;
use ratatui::{
    Frame,
    layout::{Alignment, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Paragraph, Wrap},
};
use std::sync::atomic::Ordering::Relaxed;
use std::time::Instant;

use crate::analysis::metrics::db;
use crate::audio::AudioEngine;
use crate::audio::calibration::{
    CalError, CalResult, CalWarning, CalibrationEntry, InputCalibration, InputIdentity,
    PickupClass, REFERENCE_VERSION, WindowStat, compute_calibration,
};

use super::styles::{ACCENT, AMBER, CHROME, DIM, HOT, SAFE, WARN, panel_style};

/// Seconds the noise-floor capture runs.
const NOISE_SECS: f32 = 2.0;
/// Seconds the playing capture runs.
const CAPTURE_SECS: f32 = 8.0;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Step {
    Intro,
    Noise,
    Capture,
    Result,
    Error,
}

pub(super) struct CalibrationUi {
    pub open: bool,
    step: Step,
    pickup: PickupClass,
    noise: Vec<WindowStat>,
    play: Vec<WindowStat>,
    scratch: Vec<WindowStat>,
    result: Option<CalResult>,
    error: Option<CalError>,
    started: Option<Instant>,
    previous_trim_db: f32,
}

impl CalibrationUi {
    pub fn new() -> Self {
        Self {
            open: false,
            step: Step::Intro,
            pickup: PickupClass::Humbucker,
            noise: Vec::new(),
            play: Vec::new(),
            scratch: Vec::new(),
            result: None,
            error: None,
            started: None,
            previous_trim_db: 0.0,
        }
    }

    /// Open the wizard. Returns `false` (and does nothing) while a take is
    /// recording, so a calibration can't disturb an in-flight capture.
    pub fn open(&mut self, cal: &InputCalibration, recording: bool) -> bool {
        if recording {
            return false;
        }
        self.open = true;
        self.step = Step::Intro;
        self.noise.clear();
        self.play.clear();
        self.scratch.clear();
        self.result = None;
        self.error = None;
        self.started = None;
        self.previous_trim_db = cal.trim_db.load(Relaxed);
        true
    }

    fn begin_measure(&mut self, cal: &InputCalibration) {
        cal.raw_clip.store(false, Relaxed);
        cal.stats_overflow.store(false, Relaxed);
        cal.measuring.store(true, Relaxed);
        self.noise.clear();
        self.play.clear();
        self.scratch.clear();
        self.result = None;
        self.error = None;
        self.started = Some(Instant::now());
        self.step = Step::Noise;
    }

    fn close(&mut self, cal: &InputCalibration, restore: bool) {
        cal.measuring.store(false, Relaxed);
        if restore {
            cal.trim_db.store(self.previous_trim_db, Relaxed);
        }
        self.open = false;
    }

    /// Drain the audio thread's window ring and advance the timed steps. Call
    /// once per UI tick.
    pub fn tick(&mut self, engine: &mut AudioEngine, cal: &InputCalibration) {
        if !self.open {
            return;
        }
        self.scratch.clear();
        engine.drain_cal_stats(&mut self.scratch);
        match self.step {
            Step::Noise => self.noise.append(&mut self.scratch),
            Step::Capture => self.play.append(&mut self.scratch),
            _ => {}
        }

        let Some(started) = self.started else {
            return;
        };
        let elapsed = started.elapsed().as_secs_f32();
        match self.step {
            Step::Noise if elapsed >= NOISE_SECS => {
                self.started = Some(Instant::now());
                self.play.clear();
                self.scratch.clear();
                self.step = Step::Capture;
            }
            Step::Capture if elapsed >= CAPTURE_SECS => {
                cal.measuring.store(false, Relaxed);
                let clipped = cal.raw_clip.load(Relaxed);
                let overflow = cal.stats_overflow.load(Relaxed);
                match compute_calibration(&self.play, &self.noise, self.pickup, clipped, overflow) {
                    Ok(r) => {
                        self.result = Some(r);
                        self.step = Step::Result;
                    }
                    Err(e) => {
                        self.error = Some(e);
                        self.step = Step::Error;
                    }
                }
            }
            _ => {}
        }
    }

    pub fn handle_key(&mut self, code: KeyCode, cal: &InputCalibration, identity: &InputIdentity) {
        match self.step {
            Step::Intro => match code {
                KeyCode::Esc => self.close(cal, false),
                KeyCode::Left | KeyCode::Char('-') => self.pickup = self.pickup.prev(),
                KeyCode::Right | KeyCode::Char('+') | KeyCode::Char('=') => {
                    self.pickup = self.pickup.next()
                }
                KeyCode::Enter => self.begin_measure(cal),
                _ => {}
            },
            Step::Noise | Step::Capture => {
                if code == KeyCode::Esc {
                    self.close(cal, false);
                }
            }
            Step::Result => match code {
                KeyCode::Esc => self.close(cal, true),
                KeyCode::Char('r') | KeyCode::Char('R') => self.begin_measure(cal),
                KeyCode::Char('0') => {
                    cal.trim_db.store(0.0, Relaxed);
                    crate::audio::calibration::remove_calibration(identity).ok();
                    self.close(cal, false);
                }
                KeyCode::Char(' ') => {
                    let proposed = self.result.as_ref().map_or(0.0, |r| r.trim_db);
                    let now = cal.trim_db.load(Relaxed);
                    let next = if (now - proposed).abs() < f32::EPSILON {
                        self.previous_trim_db
                    } else {
                        proposed
                    };
                    cal.trim_db.store(next, Relaxed);
                }
                KeyCode::Enter => {
                    if let Some(r) = &self.result {
                        let entry = CalibrationEntry {
                            device: identity.device.clone(),
                            channels: identity.channels,
                            channel: identity.channel,
                            trim_db: r.trim_db,
                            pickup: self.pickup,
                            measured_peak_dbfs: r.measured_peak_dbfs,
                            target_peak_dbfs: r.target_peak_dbfs,
                            reference_version: REFERENCE_VERSION,
                            calibrated_unix: std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .map_or(0, |d| d.as_secs()),
                            note: String::new(),
                        };
                        cal.trim_db.store(r.trim_db, Relaxed);
                        crate::audio::calibration::save_calibration(&entry).ok();
                    }
                    self.close(cal, false);
                }
                _ => {}
            },
            Step::Error => match code {
                KeyCode::Esc => self.close(cal, false),
                KeyCode::Char('r') | KeyCode::Char('R') => self.begin_measure(cal),
                _ => {}
            },
        }
    }

    pub fn render(&self, f: &mut Frame, cal: &InputCalibration) {
        let area = centered_rect(64, 62, f.area());
        f.render_widget(Clear, area);
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Double)
            .border_style(Style::default().fg(ACCENT))
            .title(Span::styled(
                " I N P U T   C A L I B R A T I O N ",
                Style::default().fg(AMBER).add_modifier(Modifier::BOLD),
            ))
            .style(panel_style());
        let inner = block.inner(area);
        f.render_widget(block, area);
        f.render_widget(
            Paragraph::new(self.lines(cal))
                .alignment(Alignment::Left)
                .wrap(Wrap { trim: false }),
            inner,
        );
    }

    fn lines(&self, cal: &InputCalibration) -> Vec<Line<'static>> {
        let text = |s: &str| Line::from(Span::styled(s.to_string(), Style::default().fg(CHROME)));
        let dim = |s: &str| Line::from(Span::styled(s.to_string(), Style::default().fg(DIM)));
        let hint = |pairs: &[(&str, &str)]| {
            let mut spans = Vec::new();
            for (i, (k, d)) in pairs.iter().enumerate() {
                if i > 0 {
                    spans.push(Span::styled("  ", Style::default().fg(DIM)));
                }
                spans.push(Span::styled((*k).to_string(), Style::default().fg(AMBER)));
                spans.push(Span::styled(format!(" {d}"), Style::default().fg(DIM)));
            }
            Line::from(spans)
        };
        let elapsed = self.started.map_or(0.0, |s| s.elapsed().as_secs_f32());

        match self.step {
            Step::Intro => vec![
                text("Match the engine input level to a fixed reference."),
                dim("Set the interface gain so hard strums don't clip, and note it."),
                dim("Guitar volume 10, bridge pickup."),
                Line::from(""),
                Line::from(vec![
                    Span::styled("Pickup:  ", Style::default().fg(DIM)),
                    Span::styled(
                        format!("◀ {} ▶", self.pickup.label()),
                        Style::default().fg(SAFE).add_modifier(Modifier::BOLD),
                    ),
                ]),
                dim(format!(
                    "Target: {:.1} dBFS P99 window peak",
                    crate::audio::calibration::target_peak_dbfs(self.pickup)
                )
                .as_str()),
                Line::from(""),
                hint(&[("Enter", "start"), ("←/→", "pickup"), ("Esc", "close")]),
            ],
            Step::Noise => vec![
                text("Don't play — mute the strings."),
                Line::from(""),
                Line::from(vec![
                    Span::styled("Noise capture: ", Style::default().fg(DIM)),
                    Span::styled(
                        format!("{:.1}s", (NOISE_SECS - elapsed).max(0.0)),
                        Style::default().fg(SAFE).add_modifier(Modifier::BOLD),
                    ),
                ]),
                Line::from(""),
                hint(&[("Esc", "cancel")]),
            ],
            Step::Capture => {
                let peak = self.play.last().map_or(0.0, |w| w.peak_raw);
                let clip = cal.raw_clip.load(Relaxed);
                vec![
                    text("Strum open E hard, four times, let it ring."),
                    Line::from(""),
                    Line::from(vec![
                        Span::styled("Capture: ", Style::default().fg(DIM)),
                        Span::styled(
                            format!("{:.1}s", (CAPTURE_SECS - elapsed).max(0.0)),
                            Style::default().fg(SAFE).add_modifier(Modifier::BOLD),
                        ),
                    ]),
                    Line::from(vec![
                        Span::styled("Level:   ", Style::default().fg(DIM)),
                        Span::styled(
                            if peak > 0.0 {
                                format!("{:.1} dBFS", db(peak))
                            } else {
                                "—".to_string()
                            },
                            Style::default().fg(CHROME),
                        ),
                        Span::styled(
                            if clip {
                                "   CLIP — lower the gain"
                            } else {
                                ""
                            }
                            .to_string(),
                            Style::default().fg(HOT).add_modifier(Modifier::BOLD),
                        ),
                    ]),
                    Line::from(""),
                    hint(&[("Esc", "cancel")]),
                ]
            }
            Step::Result => {
                let r = self.result.as_ref();
                let mut lines = vec![text("Calibration measured."), Line::from("")];
                if let Some(r) = r {
                    lines.push(Line::from(vec![
                        Span::styled("Measured P99: ", Style::default().fg(DIM)),
                        Span::styled(
                            format!("{:.1} dBFS", r.measured_peak_dbfs),
                            Style::default().fg(CHROME),
                        ),
                    ]));
                    lines.push(Line::from(vec![
                        Span::styled("Noise floor:  ", Style::default().fg(DIM)),
                        Span::styled(
                            format!("{:.1} dBFS", r.noise_floor_dbfs),
                            Style::default().fg(CHROME),
                        ),
                    ]));
                    lines.push(Line::from(vec![
                        Span::styled("Target:       ", Style::default().fg(DIM)),
                        Span::styled(
                            format!("{:.1} dBFS", r.target_peak_dbfs),
                            Style::default().fg(CHROME),
                        ),
                    ]));
                    lines.push(Line::from(vec![
                        Span::styled("Trim:         ", Style::default().fg(DIM)),
                        Span::styled(
                            format!("{:+.1} dB", r.trim_db),
                            Style::default().fg(SAFE).add_modifier(Modifier::BOLD),
                        ),
                    ]));
                    for w in &r.warnings {
                        lines.push(Line::from(Span::styled(
                            format!("⚠ {}", warning_text(*w)),
                            Style::default().fg(WARN),
                        )));
                    }
                }
                lines.push(Line::from(""));
                lines.push(hint(&[
                    ("Enter", "save"),
                    ("R", "retry"),
                    ("0", "reset"),
                    ("Space", "A/B"),
                    ("Esc", "cancel"),
                ]));
                lines
            }
            Step::Error => {
                let msg = self.error.map_or("Unknown error.", error_text);
                vec![
                    Line::from(Span::styled(
                        msg.to_string(),
                        Style::default().fg(HOT).add_modifier(Modifier::BOLD),
                    )),
                    Line::from(""),
                    hint(&[("R", "retry"), ("Esc", "close")]),
                ]
            }
        }
    }
}

fn error_text(e: CalError) -> &'static str {
    match e {
        CalError::Clipped => "It clipped. Lower the interface gain and retry.",
        CalError::NoSignal => "No signal detected. Check the input and retry.",
        CalError::TooShort => "Capture was too short. Retry.",
        CalError::Overflow => "Measurement windows were dropped. Retry.",
    }
}

fn warning_text(w: CalWarning) -> &'static str {
    match w {
        CalWarning::HighTrim => "large trim (> +18 dB)",
        CalWarning::LowSnr => "low signal-to-noise (< 40 dB)",
        CalWarning::Clamped => "clamped to ±24 dB",
    }
}

fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let width = (area.width * percent_x / 100).max(40).min(area.width);
    let height = (area.height * percent_y / 100).max(16).min(area.height);
    Rect {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.y + (area.height.saturating_sub(height)) / 2,
        width,
        height,
    }
}
