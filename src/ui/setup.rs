use std::io::Stdout;
use std::time::Duration;

use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyModifiers};
use ratatui::{
    Terminal,
    backend::CrosstermBackend,
    layout::{Alignment, Constraint, Direction, Layout},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Paragraph},
};

use crate::audio::DeviceInfo;
use crate::dsp::{Levels, Params};

use super::draw::draw;
use super::styles::{ACCENT, AMBER, CHROME, DIM, panel_style};

pub struct Selection {
    pub input_idx: usize,
    pub guitar_ch: usize,
    pub output_idx: usize,
}

enum Step {
    InputDevice {
        cursor: usize,
    },
    InputChannel {
        input_idx: usize,
        cursor: usize,
    },
    OutputDevice {
        input_idx: usize,
        guitar_ch: usize,
        cursor: usize,
    },
}

/// Runs the device picker. Returns `Ok(None)` when the user quits (Ctrl-C) and
/// `Ok(Some(selection))` once they confirm.
///
/// `notice` is an optional message shown above the list — used to explain why the
/// picker reopened (e.g. the last device could not be opened), so a failed start
/// never leaves the user stranded with no explanation.
pub fn run(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    devices: &DeviceInfo,
    params: &Params,
    levels: &Levels,
    notice: Option<&str>,
) -> Result<Option<Selection>> {
    let mut step = Step::InputDevice { cursor: 0 };

    loop {
        let step_ref = &step;
        terminal.draw(|f| {
            draw(
                f,
                params,
                levels,
                // No engine yet, so no calibration to show on the picker.
                &crate::audio::calibration::InputCalibration::default(),
                None,
                &[],
                false,
                false,
                None,
                None,
                None,
                None,
                super::config::Panels::all_visible(),
                crate::dsp::ChainStage::Amp,
                None,
            );
            match step_ref {
                Step::InputDevice { cursor } => {
                    render_list_modal(
                        f,
                        " S E L E C T  I N P U T  D E V I C E ",
                        &devices
                            .inputs
                            .iter()
                            .map(|d| (d.name.as_str(), format!("{} ch", d.channels)))
                            .collect::<Vec<_>>(),
                        *cursor,
                        "↑/↓ navigate  Enter select  Ctrl-C quit",
                        notice,
                    );
                }
                Step::InputChannel { input_idx, cursor } => {
                    let n = devices.inputs[*input_idx].channels;
                    let items: Vec<(String, String)> = (1..=n)
                        .map(|i| (format!("Channel {i}"), String::new()))
                        .collect();
                    render_list_modal(
                        f,
                        " S E L E C T  G U I T A R  C H A N N E L ",
                        &items
                            .iter()
                            .map(|(a, b)| (a.as_str(), b.clone()))
                            .collect::<Vec<_>>(),
                        *cursor,
                        "↑/↓ navigate  Enter select  Ctrl-C quit",
                        notice,
                    );
                }
                Step::OutputDevice { cursor, .. } => {
                    let items: Vec<(&str, String)> = devices
                        .outputs
                        .iter()
                        .map(|name| (name.as_str(), String::new()))
                        .collect();
                    render_list_modal(
                        f,
                        " S E L E C T  O U T P U T  D E V I C E ",
                        &items,
                        *cursor,
                        "↑/↓ navigate  Enter select  Ctrl-C quit",
                        notice,
                    );
                }
            }
        })?;

        if !event::poll(Duration::from_millis(30))? {
            continue;
        }
        let Event::Key(key) = event::read()? else {
            continue;
        };

        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return Ok(None);
        }

        match &mut step {
            Step::InputDevice { cursor } => {
                let n = devices.inputs.len();
                match key.code {
                    KeyCode::Up => *cursor = cursor.saturating_sub(1),
                    KeyCode::Down => *cursor = (*cursor + 1).min(n.saturating_sub(1)),
                    KeyCode::Enter if n > 0 => {
                        let input_idx = *cursor;
                        step = Step::InputChannel {
                            input_idx,
                            cursor: 0,
                        };
                    }
                    _ => {}
                }
            }
            Step::InputChannel { input_idx, cursor } => {
                let n = devices.inputs[*input_idx].channels;
                match key.code {
                    KeyCode::Up => *cursor = cursor.saturating_sub(1),
                    KeyCode::Down => *cursor = (*cursor + 1).min(n.saturating_sub(1)),
                    KeyCode::Enter => {
                        let guitar_ch = *cursor;
                        step = Step::OutputDevice {
                            input_idx: *input_idx,
                            guitar_ch,
                            cursor: 0,
                        };
                    }
                    _ => {}
                }
            }
            Step::OutputDevice {
                input_idx,
                guitar_ch,
                cursor,
            } => {
                let n = devices.outputs.len();
                match key.code {
                    KeyCode::Up => *cursor = cursor.saturating_sub(1),
                    KeyCode::Down => *cursor = (*cursor + 1).min(n.saturating_sub(1)),
                    KeyCode::Enter if n > 0 => {
                        return Ok(Some(Selection {
                            input_idx: *input_idx,
                            guitar_ch: *guitar_ch,
                            output_idx: *cursor,
                        }));
                    }
                    _ => {}
                }
            }
        }
    }
}

fn render_list_modal(
    f: &mut ratatui::Frame,
    title: &str,
    items: &[(&str, String)],
    cursor: usize,
    footer_text: &str,
    notice: Option<&str>,
) {
    let area = centered_rect(62, f.area());
    f.render_widget(Clear, area);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Double)
        .border_style(Style::default().fg(ACCENT))
        .title(Span::styled(
            title,
            Style::default().fg(AMBER).add_modifier(Modifier::BOLD),
        ))
        .style(panel_style());

    let inner = block.inner(area);
    f.render_widget(block, area);

    // With a notice, reserve a two-line warning band above the list (wrapped, so
    // a long backend error still reads); otherwise the list takes the full body.
    let constraints: Vec<Constraint> = if notice.is_some() {
        vec![
            Constraint::Length(2),
            Constraint::Min(1),
            Constraint::Length(1),
        ]
    } else {
        vec![Constraint::Min(1), Constraint::Length(1)]
    };
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(inner);

    let (list_area, footer_area) = if notice.is_some() {
        if let Some(text) = notice {
            let warn = Paragraph::new(text)
                .style(Style::default().fg(ratatui::style::Color::Rgb(235, 110, 90)))
                .wrap(ratatui::widgets::Wrap { trim: true });
            f.render_widget(warn, rows[0]);
        }
        (rows[1], rows[2])
    } else {
        (rows[0], rows[1])
    };

    let visible = list_area.height as usize;
    let offset = if cursor >= visible {
        cursor - visible + 1
    } else {
        0
    };

    let lines: Vec<Line> = items
        .iter()
        .enumerate()
        .skip(offset)
        .map(|(i, (name, hint))| {
            let selected = i == cursor;
            let (prefix, name_style, hint_style) = if selected {
                (
                    "▶ ",
                    Style::default()
                        .fg(ACCENT)
                        .add_modifier(Modifier::BOLD | Modifier::REVERSED),
                    Style::default().fg(ACCENT).add_modifier(Modifier::REVERSED),
                )
            } else {
                ("  ", Style::default().fg(CHROME), Style::default().fg(DIM))
            };
            let label = if hint.is_empty() {
                name.to_string()
            } else {
                format!("{name}  ")
            };
            Line::from(vec![
                Span::styled(
                    prefix,
                    Style::default().fg(if selected { ACCENT } else { DIM }),
                ),
                Span::styled(label, name_style),
                Span::styled(hint.clone(), hint_style),
            ])
        })
        .collect();

    f.render_widget(Paragraph::new(lines), list_area);

    // Parse footer_text into alternating key/description spans
    let footer_spans: Vec<Span> = footer_text
        .split("  ")
        .enumerate()
        .flat_map(|(i, part)| {
            if let Some(pos) = part.find(' ') {
                let key = &part[..pos];
                let desc = &part[pos..];
                vec![
                    Span::styled(key.to_string(), Style::default().fg(AMBER)),
                    Span::styled(
                        if i == 0 {
                            desc.to_string()
                        } else {
                            format!("  {desc}")
                        },
                        Style::default().fg(DIM),
                    ),
                ]
            } else {
                vec![Span::styled(part.to_string(), Style::default().fg(AMBER))]
            }
        })
        .collect();

    f.render_widget(
        Paragraph::new(Line::from(footer_spans)).alignment(Alignment::Center),
        footer_area,
    );
}

fn centered_rect(percent_x: u16, area: ratatui::layout::Rect) -> ratatui::layout::Rect {
    let width = area.width * percent_x / 100;
    let x = (area.width - width) / 2;
    let height = (area.height * 60 / 100).max(6);
    let y = (area.height - height) / 2;
    ratatui::layout::Rect {
        x: area.x + x,
        y: area.y + y,
        width,
        height,
    }
}
