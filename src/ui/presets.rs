use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Direction, Layout},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Paragraph},
};

use super::styles::{ACCENT, AMBER, CHROME, DIM};
use std::collections::HashSet;

use crate::preset::{Preset, PresetSource};

const GREEN: ratatui::style::Color = ratatui::style::Color::Rgb(80, 200, 120);

pub(super) fn render_preset_modal(
    f: &mut Frame,
    presets: &[Preset],
    cursor: usize,
    favorites: &HashSet<String>,
    filter: &str,
) {
    let visible = filter_indices(presets, filter);
    let on_user_preset = cursor
        .checked_sub(1)
        .and_then(|i| visible.get(i))
        .is_some_and(|&i| presets[i].source == PresetSource::User);
    let area = centered_rect(60, f.area());

    f.render_widget(Clear, area);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Double)
        .border_style(Style::default().fg(ACCENT))
        .title(Span::styled(
            if filter.is_empty() {
                " P R E S E T S ".to_string()
            } else {
                format!(" P R E S E T S   filter: {filter} ")
            },
            Style::default().fg(AMBER).add_modifier(Modifier::BOLD),
        ))
        .style(Style::default().bg(ratatui::style::Color::Black));

    let inner = block.inner(area);
    f.render_widget(block, area);

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(2)])
        .split(inner);

    let mut lines: Vec<Line> = Vec::with_capacity(visible.len() + 1);

    let entry = |name: &str, desc: &str, tag: Option<&str>, selected: bool, fav: bool| -> Line {
        let (prefix, name_style, desc_style) = if selected {
            (
                "▶ ",
                Style::default()
                    .fg(ACCENT)
                    .add_modifier(Modifier::BOLD | Modifier::REVERSED),
                Style::default().fg(AMBER).add_modifier(Modifier::REVERSED),
            )
        } else {
            ("  ", Style::default().fg(CHROME), Style::default().fg(DIM))
        };

        let label = if desc.is_empty() {
            name.to_string()
        } else {
            format!("{name}  ")
        };

        let mut spans = vec![
            Span::styled(
                if fav { "★ " } else { "  " },
                Style::default()
                    .fg(if fav { AMBER } else { DIM })
                    .add_modifier(if fav {
                        Modifier::BOLD
                    } else {
                        Modifier::empty()
                    }),
            ),
            Span::styled(
                prefix.to_string(),
                Style::default().fg(if selected { ACCENT } else { DIM }),
            ),
            Span::styled(label, name_style),
            Span::styled(desc.to_string(), desc_style),
        ];

        if let Some(t) = tag {
            let tag_style = if selected {
                Style::default()
                    .fg(GREEN)
                    .add_modifier(Modifier::BOLD | Modifier::REVERSED)
            } else {
                Style::default().fg(GREEN).add_modifier(Modifier::DIM)
            };
            spans.push(Span::styled(format!("  [{t}]"), tag_style));
        }

        Line::from(spans)
    };

    lines.push(entry("Default values", "", None, cursor == 0, false));
    for (pos, &i) in visible.iter().enumerate() {
        let p = &presets[i];
        let desc = p.description.as_deref().unwrap_or("");
        // Combine the source marker with the preset's own tags into one label.
        let mut labels: Vec<&str> = Vec::new();
        if p.source == PresetSource::User {
            labels.push("user");
        }
        labels.extend(p.tags.iter().map(String::as_str));
        let tag_string = labels.join(" · ");
        let tag = if tag_string.is_empty() {
            None
        } else {
            Some(tag_string.as_str())
        };
        lines.push(entry(
            &p.name,
            desc,
            tag,
            cursor == pos + 1,
            favorites.contains(&p.name),
        ));
    }

    let visible = rows[0].height as usize;
    let offset = if cursor >= visible {
        cursor - visible + 1
    } else {
        0
    };
    let visible_lines: Vec<Line> = lines.into_iter().skip(offset).collect();

    f.render_widget(Paragraph::new(visible_lines), rows[0]);

    let footer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Length(1)])
        .split(rows[1]);

    let mut footer_spans = vec![
        Span::styled("↑/↓", Style::default().fg(AMBER)),
        Span::styled(" navigate  ", Style::default().fg(DIM)),
        Span::styled("Enter", Style::default().fg(AMBER)),
        Span::styled(" apply  ", Style::default().fg(DIM)),
        Span::styled("S", Style::default().fg(AMBER)),
        Span::styled(" save  ", Style::default().fg(DIM)),
        Span::styled("E", Style::default().fg(AMBER)),
        Span::styled(" export  ", Style::default().fg(DIM)),
        Span::styled("I", Style::default().fg(AMBER)),
        Span::styled(" import  ", Style::default().fg(DIM)),
        Span::styled("F", Style::default().fg(AMBER)),
        Span::styled(" favorite  ", Style::default().fg(DIM)),
        Span::styled("X", Style::default().fg(AMBER)),
        Span::styled(" A/B  ", Style::default().fg(DIM)),
    ];
    if on_user_preset {
        footer_spans.push(Span::styled(
            "D",
            Style::default()
                .fg(ratatui::style::Color::Red)
                .add_modifier(Modifier::BOLD),
        ));
        footer_spans.push(Span::styled(" delete  ", Style::default().fg(DIM)));
    }
    footer_spans.push(Span::styled("Esc / P", Style::default().fg(AMBER)));
    footer_spans.push(Span::styled(" close", Style::default().fg(DIM)));

    f.render_widget(
        Paragraph::new(Line::from(footer_spans)).alignment(Alignment::Center),
        footer[0],
    );

    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("● ", Style::default().fg(GREEN)),
            Span::styled("user preset", Style::default().fg(DIM)),
        ]))
        .alignment(Alignment::Center),
        footer[1],
    );
}

/// Whether `p` matches `filter` (case-insensitively) over its name or
/// description. An empty filter matches everything.
fn preset_matches(p: &Preset, filter: &str) -> bool {
    let needle = filter.to_lowercase();
    p.name.to_lowercase().contains(&needle)
        || p.description
            .as_deref()
            .is_some_and(|d| d.to_lowercase().contains(&needle))
        || p.tags.iter().any(|t| t.to_lowercase().contains(&needle))
}

/// Indices into `presets` visible under `filter`, in list order.
pub(super) fn filter_indices(presets: &[Preset], filter: &str) -> Vec<usize> {
    if filter.is_empty() {
        return (0..presets.len()).collect();
    }
    presets
        .iter()
        .enumerate()
        .filter(|(_, p)| preset_matches(p, filter))
        .map(|(i, _)| i)
        .collect()
}

/// Number of preset rows visible under `filter`, excluding the "Default values"
/// row.
pub(super) fn visible_len(presets: &[Preset], filter: &str) -> usize {
    if filter.is_empty() {
        presets.len()
    } else {
        presets.iter().filter(|p| preset_matches(p, filter)).count()
    }
}

/// The preset under the modal `cursor` (`0` is "Default values"), respecting
/// the active `filter`.
pub(super) fn selected_preset<'a>(
    presets: &'a [Preset],
    filter: &str,
    cursor: usize,
) -> Option<&'a Preset> {
    let idx = cursor.checked_sub(1)?;
    filter_indices(presets, filter)
        .get(idx)
        .map(|&i| &presets[i])
}

pub(super) fn render_save_dialog(
    f: &mut Frame,
    name: &str,
    desc: &str,
    active_field: usize,
    error: Option<&str>,
) {
    let area = centered_rect(55, f.area());
    f.render_widget(Clear, area);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Double)
        .border_style(Style::default().fg(GREEN))
        .title(Span::styled(
            " S A V E   P R E S E T ",
            Style::default().fg(GREEN).add_modifier(Modifier::BOLD),
        ))
        .style(Style::default().bg(ratatui::style::Color::Black));

    let inner = block.inner(area);
    f.render_widget(block, area);

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // name label
            Constraint::Length(2), // name input + underline
            Constraint::Length(1), // gap
            Constraint::Length(1), // desc label
            Constraint::Length(2), // desc input + underline
            Constraint::Min(1),    // error/spacer
            Constraint::Length(1), // footer
        ])
        .split(inner);

    let field_style = |active: bool| {
        if active {
            Style::default()
                .fg(ratatui::style::Color::White)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(CHROME)
        }
    };

    let label_style = |active: bool| {
        if active {
            Style::default().fg(GREEN).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(DIM)
        }
    };

    f.render_widget(
        Paragraph::new(Span::styled("Name:", label_style(active_field == 0))),
        rows[0],
    );

    f.render_widget(
        Paragraph::new(Span::styled(
            if active_field == 0 {
                format!("{name}█")
            } else {
                name.to_string()
            },
            field_style(active_field == 0),
        ))
        .block(Block::default().borders(Borders::BOTTOM).border_style(
            if active_field == 0 {
                Style::default().fg(GREEN)
            } else {
                Style::default().fg(DIM)
            },
        )),
        rows[1],
    );

    f.render_widget(
        Paragraph::new(Span::styled(
            "Description (optional):",
            label_style(active_field == 1),
        )),
        rows[3],
    );

    f.render_widget(
        Paragraph::new(Span::styled(
            if active_field == 1 {
                format!("{desc}█")
            } else {
                desc.to_string()
            },
            field_style(active_field == 1),
        ))
        .block(Block::default().borders(Borders::BOTTOM).border_style(
            if active_field == 1 {
                Style::default().fg(GREEN)
            } else {
                Style::default().fg(DIM)
            },
        )),
        rows[4],
    );

    if let Some(err) = error {
        f.render_widget(
            Paragraph::new(Span::styled(
                err,
                Style::default()
                    .fg(ratatui::style::Color::Red)
                    .add_modifier(Modifier::BOLD),
            )),
            rows[5],
        );
    }

    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("Tab", Style::default().fg(AMBER)),
            Span::styled(" next field  ", Style::default().fg(DIM)),
            Span::styled("Enter", Style::default().fg(AMBER)),
            Span::styled(" save  ", Style::default().fg(DIM)),
            Span::styled("Esc", Style::default().fg(AMBER)),
            Span::styled(" cancel", Style::default().fg(DIM)),
        ]))
        .alignment(Alignment::Center),
        rows[6],
    );
}

/// Which direction a [`render_path_dialog`] is working in.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum PathDialogKind {
    Import,
    Export,
    /// Export the timeline's raw takes to a WAV file.
    SessionExport,
}

/// Single-field typed-path dialog for preset import (source file) and export
/// (destination file). `~` resolves against the home directory.
pub(super) fn render_path_dialog(
    f: &mut Frame,
    kind: PathDialogKind,
    input: &str,
    error: Option<&str>,
) {
    let (title, label) = match kind {
        PathDialogKind::Import => (" I M P O R T   P R E S E T ", "File to import (.toml):"),
        PathDialogKind::Export => (" E X P O R T   P R E S E T ", "Export to:"),
        PathDialogKind::SessionExport => (
            " E X P O R T   T A K E S ",
            "Render guitar takes to (.wav):",
        ),
    };
    let area = centered_rect(60, f.area());
    f.render_widget(Clear, area);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Double)
        .border_style(Style::default().fg(ACCENT))
        .title(Span::styled(
            title,
            Style::default().fg(AMBER).add_modifier(Modifier::BOLD),
        ))
        .style(Style::default().bg(ratatui::style::Color::Black));

    let inner = block.inner(area);
    f.render_widget(block, area);

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // label
            Constraint::Length(2), // input + underline
            Constraint::Min(1),    // error/spacer
            Constraint::Length(1), // footer
        ])
        .split(inner);

    f.render_widget(
        Paragraph::new(Span::styled(
            label,
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        )),
        rows[0],
    );

    f.render_widget(
        Paragraph::new(Span::styled(
            format!("{input}█"),
            Style::default()
                .fg(ratatui::style::Color::White)
                .add_modifier(Modifier::BOLD),
        ))
        .block(
            Block::default()
                .borders(Borders::BOTTOM)
                .border_style(Style::default().fg(ACCENT)),
        ),
        rows[1],
    );

    if let Some(err) = error {
        f.render_widget(
            Paragraph::new(Span::styled(
                err,
                Style::default()
                    .fg(ratatui::style::Color::Red)
                    .add_modifier(Modifier::BOLD),
            )),
            rows[2],
        );
    }

    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("Enter", Style::default().fg(AMBER)),
            Span::styled(" confirm  ", Style::default().fg(DIM)),
            Span::styled("Esc", Style::default().fg(AMBER)),
            Span::styled(" cancel", Style::default().fg(DIM)),
        ]))
        .alignment(Alignment::Center),
        rows[3],
    );
}

fn centered_rect(percent_x: u16, area: ratatui::layout::Rect) -> ratatui::layout::Rect {
    let width = area.width * percent_x / 100;
    let x = (area.width - width) / 2;

    let height = (area.height * 70 / 100).max(6);
    let y = (area.height - height) / 2;

    ratatui::layout::Rect {
        x: area.x + x,
        y: area.y + y,
        width,
        height,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsp::Params;

    /// The browser filter must match a preset's name, description, or tags,
    /// case-insensitively.
    #[test]
    fn filter_matches_name_description_and_tags() {
        let params = Params::new();
        let mut p =
            Preset::from_params("My Lead".to_string(), Some("singing".to_string()), &params);
        p.tags = vec!["blues".to_string(), "gibson".to_string()];
        assert!(preset_matches(&p, "lead"), "name not matched");
        assert!(preset_matches(&p, "singing"), "description not matched");
        assert!(preset_matches(&p, "BLUES"), "tag not matched (case)");
        assert!(preset_matches(&p, "gib"), "tag substring not matched");
        assert!(!preset_matches(&p, "strat"));
        assert_eq!(filter_indices(std::slice::from_ref(&p), "blues"), vec![0]);
        assert!(filter_indices(std::slice::from_ref(&p), "zzz").is_empty());
        assert_eq!(filter_indices(std::slice::from_ref(&p), ""), vec![0]);
    }
}
