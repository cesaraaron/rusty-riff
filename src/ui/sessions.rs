//! Session (project) browser modal: New / Save / Save As / Load / Delete for
//! portable session folders under `~/.config/rusty-riff/sessions/`, plus
//! recovery of dry takes abandoned in `~/.config/rusty-riff/recovery/`.
//!
//! The recovery section is meant for takes left behind by a crash. A normal exit
//! saves or discards the running session's takes (which cleans its recovery
//! folder), so the only folders that survive are crash leftovers. The browser is
//! told which recovery assets the live session already owns and hides them, so a
//! take that is simply part of the current session is never offered back.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crossterm::event::KeyCode;
use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Paragraph},
};

use super::styles::{ACCENT, AMBER, CHROME, DIM, HOT, SAFE, WARN, panel_style};
use crate::project::{self, RecoveryTake, SessionEntry};

/// Which page is showing.
#[derive(Clone, Copy, PartialEq, Eq)]
enum View {
    /// Pick a saved session, a recoverable take, or an action.
    List,
    /// Type a name for Save As.
    NameInput,
}

/// One selectable row: either a saved session or a recoverable take.
enum RowKind {
    Session(PathBuf),
    Recovery(RecoveryTake),
}

struct Row {
    label: String,
    detail: String,
    kind: RowKind,
}

/// What the UI loop should do in response to a keypress.
pub(super) enum Action {
    None,
    New,
    /// Save to the current session folder, or prompt when it has none.
    Save,
    /// Save As the typed name.
    SaveAs(String),
    Load(PathBuf),
    Delete(PathBuf),
    /// Bring an abandoned take into the current session.
    Restore(RecoveryTake),
    /// Delete an abandoned take.
    Discard(RecoveryTake),
}

/// Session-browser state, kept on the UI thread.
pub(super) struct SessionBrowser {
    pub open: bool,
    view: View,
    cursor: usize,
    rows: Vec<Row>,
    /// Live filter over the row list while `/` search is active.
    filter: String,
    searching: bool,
    name_input: String,
    pub message: Option<String>,
    /// The running session's own recovery folder, hidden from the recovery list.
    live_dir: Option<PathBuf>,
    /// Recovery WAVs the running session already references, hidden likewise.
    in_use: HashSet<PathBuf>,
}

impl SessionBrowser {
    pub(super) fn new() -> Self {
        Self {
            open: false,
            view: View::List,
            cursor: 0,
            rows: Vec::new(),
            filter: String::new(),
            searching: false,
            name_input: String::new(),
            message: None,
            live_dir: None,
            in_use: HashSet::new(),
        }
    }

    /// Record which recovery assets belong to the live session so they are not
    /// listed as crash leftovers. `live_dir` is the session's recovery folder;
    /// `in_use` is the recovery WAVs its tracks reference.
    fn set_context(&mut self, live_dir: Option<&Path>, in_use: &HashSet<PathBuf>) {
        self.live_dir = live_dir.map(Path::to_path_buf);
        self.in_use = in_use.clone();
    }

    /// True when `take` must stay hidden: it lives in the live session's recovery
    /// folder (covers an in-progress recording, whose track has no asset yet) or a
    /// live-session track already references it (a crash take already restored).
    fn is_excluded(&self, take: &RecoveryTake) -> bool {
        self.live_dir.as_deref() == Some(take.dir.as_path()) || self.in_use.contains(&take.wav)
    }

    /// Open the modal, rescanning saved sessions and recoverable takes.
    pub(super) fn open(&mut self, live_dir: Option<&Path>, in_use: &HashSet<PathBuf>) {
        self.set_context(live_dir, in_use);
        self.reload();
        self.cursor = 0;
        self.view = View::List;
        self.filter.clear();
        self.searching = false;
        self.name_input.clear();
        self.message = None;
        self.open = true;
    }

    /// Rebuild the row list from disk, keeping the cursor in range.
    pub(super) fn refresh(&mut self, live_dir: Option<&Path>, in_use: &HashSet<PathBuf>) {
        self.set_context(live_dir, in_use);
        self.reload();
        let n = self.visible_rows().len();
        self.cursor = self.cursor.min(n.saturating_sub(1));
    }

    /// Indices into `rows` visible under the active filter, in list order.
    fn visible_rows(&self) -> Vec<usize> {
        if self.filter.is_empty() {
            return (0..self.rows.len()).collect();
        }
        let needle = self.filter.to_lowercase();
        self.rows
            .iter()
            .enumerate()
            .filter(|(_, r)| {
                r.label.to_lowercase().contains(&needle)
                    || r.detail.to_lowercase().contains(&needle)
            })
            .map(|(i, _)| i)
            .collect()
    }

    /// The row under the cursor, honoring the active filter.
    fn selected_row(&self) -> Option<&Row> {
        self.visible_rows().get(self.cursor).map(|&i| &self.rows[i])
    }

    fn activate_selected(&self) -> Action {
        match self.selected_row().map(|r| &r.kind) {
            Some(RowKind::Session(dir)) => Action::Load(dir.clone()),
            Some(RowKind::Recovery(take)) => Action::Restore(take.clone()),
            None => Action::None,
        }
    }

    fn reload(&mut self) {
        let sessions = project::list_sessions();
        let recovery = project::list_recovery()
            .into_iter()
            .filter(|t| !self.is_excluded(t))
            .collect();
        self.rows = build_rows(sessions, recovery);
    }

    /// Switch to the name prompt for Save As (or Save without a folder).
    pub(super) fn prompt_name(&mut self, initial: &str) {
        self.view = View::NameInput;
        self.name_input = initial.to_owned();
    }

    /// Return to the session list view.
    pub(super) fn view_list(&mut self) {
        self.view = View::List;
    }

    pub(super) fn handle_key(&mut self, code: KeyCode) -> Action {
        match self.view {
            View::List => self.handle_list_key(code),
            View::NameInput => self.handle_name_key(code),
        }
    }

    fn handle_list_key(&mut self, code: KeyCode) -> Action {
        if self.searching {
            return match code {
                KeyCode::Esc => {
                    self.filter.clear();
                    self.cursor = 0;
                    self.searching = false;
                    Action::None
                }
                KeyCode::Backspace => {
                    self.filter.pop();
                    self.cursor = 0;
                    Action::None
                }
                KeyCode::Char(c) => {
                    self.filter.push(c);
                    self.cursor = 0;
                    Action::None
                }
                KeyCode::Up => {
                    self.cursor = self.cursor.saturating_sub(1);
                    Action::None
                }
                KeyCode::Down => {
                    let n = self.visible_rows().len();
                    self.cursor = (self.cursor + 1).min(n.saturating_sub(1));
                    Action::None
                }
                KeyCode::Enter => self.activate_selected(),
                _ => Action::None,
            };
        }
        match code {
            KeyCode::Up => {
                self.cursor = self.cursor.saturating_sub(1);
                Action::None
            }
            KeyCode::Down => {
                let n = self.visible_rows().len();
                self.cursor = (self.cursor + 1).min(n.saturating_sub(1));
                Action::None
            }
            KeyCode::Enter => self.activate_selected(),
            KeyCode::Char('/') => {
                self.filter.clear();
                self.cursor = 0;
                self.searching = true;
                Action::None
            }
            KeyCode::Char('n') | KeyCode::Char('N') => Action::New,
            KeyCode::Char('s') | KeyCode::Char('S') => Action::Save,
            KeyCode::Char('a') | KeyCode::Char('A') => {
                self.prompt_name("");
                Action::None
            }
            KeyCode::Char('d') | KeyCode::Char('D') => match self.selected_row().map(|r| &r.kind) {
                Some(RowKind::Session(dir)) => Action::Delete(dir.clone()),
                Some(RowKind::Recovery(take)) => Action::Discard(take.clone()),
                None => Action::None,
            },
            KeyCode::Esc | KeyCode::Char('j') | KeyCode::Char('J') => {
                self.open = false;
                Action::None
            }
            _ => Action::None,
        }
    }

    fn handle_name_key(&mut self, code: KeyCode) -> Action {
        match code {
            KeyCode::Esc => {
                self.view = View::List;
                Action::None
            }
            KeyCode::Backspace => {
                self.name_input.pop();
                Action::None
            }
            KeyCode::Char(c) => {
                self.name_input.push(c);
                Action::None
            }
            KeyCode::Enter => {
                let name = self.name_input.trim().to_owned();
                if name.is_empty() {
                    self.message = Some("Session name cannot be empty".to_owned());
                    Action::None
                } else {
                    Action::SaveAs(name)
                }
            }
            _ => Action::None,
        }
    }

    /// Render the modal.
    pub(super) fn render(&self, f: &mut Frame) {
        let area = centered_rect(64, f.area());
        f.render_widget(Clear, area);
        let title = match self.view {
            View::List if !self.filter.is_empty() => {
                format!(" S E S S I O N S   filter: {} ", self.filter)
            }
            View::List => " S E S S I O N S ".to_owned(),
            View::NameInput => " S A V E   S E S S I O N ".to_owned(),
        };
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

        match self.view {
            View::List => self.render_list(f, inner),
            View::NameInput => self.render_name(f, inner),
        }
    }

    fn render_list(&self, f: &mut Frame, area: ratatui::layout::Rect) {
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(1), Constraint::Length(2)])
            .split(area);

        let mut lines: Vec<Line> = Vec::with_capacity(self.rows.len() + 1);
        let visible_idx = self.visible_rows();
        if self.rows.is_empty() {
            lines.push(Line::from(Span::styled(
                "  (no saved sessions — press S to save the current one)",
                Style::default().fg(DIM),
            )));
        } else if visible_idx.is_empty() {
            lines.push(Line::from(Span::styled(
                "  (no matching sessions)",
                Style::default().fg(DIM),
            )));
        }
        let visible = rows[0].height as usize;
        let offset = self.cursor.saturating_sub(visible.saturating_sub(1));
        let mut last_recovery = false;
        for (pos, &i) in visible_idx.iter().enumerate() {
            let row = &self.rows[i];
            let recovery = matches!(row.kind, RowKind::Recovery(_));
            if recovery && !last_recovery {
                lines.push(Line::from(Span::styled(
                    "  ── recoverable takes ──",
                    Style::default().fg(WARN).add_modifier(Modifier::BOLD),
                )));
            }
            last_recovery = recovery;
            let selected = pos == self.cursor;
            let (prefix, style) = if selected {
                (
                    "▶ ",
                    Style::default()
                        .fg(ACCENT)
                        .add_modifier(Modifier::BOLD | Modifier::REVERSED),
                )
            } else {
                ("  ", Style::default().fg(CHROME))
            };
            lines.push(Line::from(vec![
                Span::styled(prefix.to_owned(), Style::default().fg(ACCENT)),
                Span::styled(format!("{:<26}", row.label), style),
                Span::styled(format!("  {}", row.detail), Style::default().fg(DIM)),
            ]));
        }
        let visible_lines: Vec<Line> = lines.into_iter().skip(offset).take(visible).collect();
        f.render_widget(Paragraph::new(visible_lines), rows[0]);

        let footer = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(1), Constraint::Length(1)])
            .split(rows[1]);
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("↑/↓", Style::default().fg(AMBER)),
                Span::styled(" navigate  ", Style::default().fg(DIM)),
                Span::styled("Enter", Style::default().fg(AMBER)),
                Span::styled(" load/restore  ", Style::default().fg(DIM)),
                Span::styled("N", Style::default().fg(AMBER)),
                Span::styled(" new  ", Style::default().fg(DIM)),
                Span::styled("S", Style::default().fg(AMBER)),
                Span::styled(" save  ", Style::default().fg(DIM)),
                Span::styled("A", Style::default().fg(AMBER)),
                Span::styled(" save as  ", Style::default().fg(DIM)),
                Span::styled("D", Style::default().fg(HOT)),
                Span::styled(" delete/discard  ", Style::default().fg(DIM)),
                Span::styled("/", Style::default().fg(AMBER)),
                Span::styled(" search  ", Style::default().fg(DIM)),
                Span::styled("Esc / J", Style::default().fg(AMBER)),
                Span::styled(" close", Style::default().fg(DIM)),
            ]))
            .alignment(Alignment::Center),
            footer[0],
        );
        let status = if self.searching {
            Line::from(vec![
                Span::styled("filter ", Style::default().fg(DIM)),
                Span::styled(self.filter.clone(), Style::default().fg(CHROME)),
                Span::styled("▌", Style::default().fg(ACCENT)),
            ])
        } else {
            Line::from(Span::styled(
                self.message.clone().unwrap_or_default(),
                Style::default().fg(SAFE),
            ))
        };
        f.render_widget(
            Paragraph::new(status).alignment(Alignment::Center),
            footer[1],
        );
    }

    fn render_name(&self, f: &mut Frame, area: ratatui::layout::Rect) {
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1),
                Constraint::Length(2),
                Constraint::Min(1),
                Constraint::Length(1),
            ])
            .split(area);
        f.render_widget(
            Paragraph::new(Span::styled(
                "Session name:",
                Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
            )),
            rows[0],
        );
        f.render_widget(
            Paragraph::new(Span::styled(
                format!("{}█", self.name_input),
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ))
            .block(
                Block::default()
                    .borders(Borders::BOTTOM)
                    .border_style(Style::default().fg(ACCENT)),
            ),
            rows[1],
        );
        if let Some(msg) = &self.message {
            f.render_widget(
                Paragraph::new(Span::styled(msg.clone(), Style::default().fg(HOT))),
                rows[2],
            );
        }
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("Enter", Style::default().fg(AMBER)),
                Span::styled(" save  ", Style::default().fg(DIM)),
                Span::styled("Esc", Style::default().fg(AMBER)),
                Span::styled(" back", Style::default().fg(DIM)),
            ]))
            .alignment(Alignment::Center),
            rows[3],
        );
    }
}

fn build_rows(sessions: Vec<SessionEntry>, recovery: Vec<RecoveryTake>) -> Vec<Row> {
    let mut rows = Vec::with_capacity(sessions.len() + recovery.len());
    for s in sessions {
        rows.push(Row {
            label: s.name,
            detail: format!("{} tracks", s.tracks),
            kind: RowKind::Session(s.dir),
        });
    }
    for take in recovery {
        let secs = if take.meta.source_sample_rate > 0 {
            take.meta.frames as f64 / f64::from(take.meta.source_sample_rate)
        } else {
            0.0
        };
        let state = if take.meta.overflowed {
            "incomplete"
        } else {
            "ready"
        };
        rows.push(Row {
            label: take.meta.name.clone(),
            detail: format!("{secs:.1}s · {state} · Enter to restore"),
            kind: RowKind::Recovery(take),
        });
    }
    rows
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

    fn row(label: &str, detail: &str) -> Row {
        Row {
            label: label.into(),
            detail: detail.into(),
            kind: RowKind::Session(PathBuf::from(format!("/s/{label}"))),
        }
    }

    #[test]
    fn session_filter_matches_label_and_detail() {
        let mut b = SessionBrowser::new();
        b.rows = vec![row("Rock", "3 tracks"), row("Jazz", "5 tracks")];
        assert_eq!(b.visible_rows(), vec![0, 1]);

        b.filter = "jazz".into();
        assert_eq!(b.visible_rows(), vec![1]);
        assert_eq!(b.selected_row().map(|r| r.label.as_str()), Some("Jazz"));

        b.filter = "3 tracks".into();
        assert_eq!(b.visible_rows(), vec![0]);

        b.filter = "zzz".into();
        assert!(b.visible_rows().is_empty());
        assert!(b.selected_row().is_none());
    }

    fn take(dir: &str, stem: &str) -> RecoveryTake {
        RecoveryTake {
            wav: PathBuf::from(dir).join(format!("{stem}.wav")),
            dir: PathBuf::from(dir),
            meta: project::RecoveryMeta {
                version: project::RECOVERY_META_VERSION,
                id: 1,
                name: stem.to_owned(),
                start_ticks: 0,
                project_sample_rate: 48_000,
                source_sample_rate: 48_000,
                frames: 0,
                overflowed: false,
            },
        }
    }

    /// A take in the live session's recovery folder, or one a live-session track
    /// already references, is hidden; a genuine crash orphan is shown.
    #[test]
    fn recovery_hides_the_live_sessions_own_takes() {
        let mut b = SessionBrowser::new();
        let live = take("/rec/111", "take-1");
        let orphan = take("/rec/222", "take-2");

        b.set_context(Some(Path::new("/rec/111")), &HashSet::new());
        assert!(b.is_excluded(&live), "a take in the live folder is hidden");
        assert!(!b.is_excluded(&orphan), "a crash orphan is listed");

        // A crash take already restored into the session is hidden too.
        let mut in_use = HashSet::new();
        in_use.insert(orphan.wav.clone());
        b.set_context(Some(Path::new("/rec/111")), &in_use);
        assert!(b.is_excluded(&orphan), "a restored take is hidden");
    }
}
