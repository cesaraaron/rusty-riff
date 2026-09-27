use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Paragraph},
};
use std::sync::atomic::Ordering::Relaxed;

use crate::audio::calibration::InputCalibration;
use crate::dsp::{AmpModel, CabModel, ChainStage, Levels, Params};
use crate::practice::Practice;

use super::config::{
    ADD_TILE, AMP_END, AMP_START, CHAIN_TILE, KNOBS, MIC_END, MIC_START, PEDALS, PRACTICE_TILE,
    Panels, Pedal, PedalUi,
};
use super::input::rendered_stages;
use super::practice::PracticeUi;
use super::styles::*;

#[allow(clippy::too_many_arguments)]
pub(super) fn draw(
    f: &mut Frame,
    params: &Params,
    levels: &Levels,
    cal: &InputCalibration,
    focus: Option<usize>,
    board: &[bool],
    recording: bool,
    blink: bool,
    status: Option<&str>,
    plugin: Option<&str>,
    ext_cab: Option<&str>,
    ext_amp: Option<&str>,
    panels: Panels,
    chain_cursor: ChainStage,
    timeline: Option<(&Practice, &PracticeUi)>,
) {
    let area = f.area();

    // No outer frame: the only bordered boxes on screen are the four panels
    // (chain, amp, timeline, rig). Layout runs directly on the full area.
    let inner = area;

    // Build the vertical layout from the visible panels. Rows are tracked by index
    // so a hidden panel simply contributes no constraint and no render call.
    let show_timeline = panels.timeline && timeline.is_some();
    let mut cons: Vec<Constraint> = vec![
        Constraint::Length(3), // chain box + mini input/output bars
    ];
    if panels.amp {
        // Two side-by-side boxes (amp + cab, 10 rows each): knobs, bottom
        // margin, grille strip inside each box. Model picking lives in the
        // `A`/`C` browser modals; the titles show the active models.
        cons.push(Constraint::Length(10)); // amplifier + cabinet/mic
    }
    if show_timeline {
        // Fixed 10-row strip (transport + two 3-row tracks + hint); any leftover
        // terminal space flows here via Min, never into the pedal knobs.
        cons.push(Constraint::Min(10));
    }
    if panels.rig {
        // Fixed height: rig border (2) + tile grid + the 6-row detail editor
        // (livery box + title LED + knobs, no ON/OFF foot row). Leftover space
        // stays in the timeline / empty.
        cons.push(Constraint::Length(rig_outer_height(
            area.width.saturating_sub(2),
            board,
        )));
    }
    cons.push(Constraint::Length(1)); // help (K for keybindings)

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints(cons)
        .split(inner);

    let mut i = 0usize;
    render_header(
        f,
        rows[i],
        params,
        levels,
        cal,
        board,
        plugin,
        ext_cab,
        ext_amp,
        focus == Some(CHAIN_TILE),
        chain_cursor,
    );
    i += 1;
    if panels.amp {
        render_amp_panel(f, rows[i], params, focus, ext_cab, ext_amp);
        i += 1;
    }
    if show_timeline {
        if let Some((practice, ui)) = timeline {
            ui.render(
                f,
                rows[i],
                practice,
                focus == Some(PRACTICE_TILE),
                blink,
                recording,
            );
        }
        i += 1;
    }
    if panels.rig {
        render_rig(f, rows[i], params, board, focus);
        i += 1;
    }
    render_help(f, rows[i], status);
}

#[allow(clippy::too_many_arguments)]
fn render_header(
    f: &mut Frame,
    area: Rect,
    params: &Params,
    levels: &Levels,
    cal: &InputCalibration,
    board: &[bool],
    plugin: Option<&str>,
    ext_cab: Option<&str>,
    ext_amp: Option<&str>,
    focused: bool,
    cursor: ChainStage,
) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Plain)
        .border_style(border_style(focused))
        .title(Line::from(Span::styled("[ ] move", Style::default().fg(DIM))).right_aligned())
        .style(Style::default().bg(Color::Black));
    let inner = block.inner(area);
    f.render_widget(block, area);

    // The amp and cab are separate stages. An active external amp (hosted AU)
    // replaces the amp label; a full-rig AU also supplies the cab, so the cab
    // stage is shown dimmed (its processing is skipped). An active external IR
    // is noted on the cab tile. `ext_*` names are only passed while active (see
    // `ui::mod`), so `Some` implies live.
    let cab_from_amp = ext_amp.is_some() && !params.amp_external_amp_only.load(Relaxed);
    let amp_label = match ext_amp {
        Some(name) => format!("AU: {}", name.to_uppercase()),
        None => "AMP".to_owned(),
    };
    let cab_label = if cab_from_amp {
        "AU CAB".to_owned()
    } else {
        match ext_cab {
            Some(name) => format!("IR: {}", name.to_uppercase()),
            None => "CAB".to_owned(),
        }
    };

    let arrow = Span::styled(" ──▶ ", Style::default().fg(DIM));

    // The ribbon shows the live signal path in chain order: engaged stages are
    // always lit, and bypassed on-board pedals appear dimmed while the ribbon
    // owns focus (so the cursor stays visible while toggling) but collapse away
    // otherwise — keeping the unfocused ribbon a compact readout. Off-board
    // pedals stay hidden. With focus, `←`/`→` move the cursor, `[`/`]` move its
    // stage, `Space` bypasses it.
    let mut chain: Vec<Span> = vec![Span::raw("  ")];
    let push_stage = |chain: &mut Vec<Span>, label: String, style: Style| {
        if chain.len() > 1 {
            chain.push(arrow.clone());
        }
        chain.push(Span::styled(label, style));
    };
    // Reversed + bold, mirroring the selected amp-model chip.
    let selected = |color: Color| {
        Style::default()
            .fg(color)
            .add_modifier(Modifier::BOLD | Modifier::REVERSED)
    };

    for &(_, slot_stage) in &rendered_stages(&params.chain_slots(), board) {
        let selected_here = focused && cursor == slot_stage;
        let (label, lit, color) = match slot_stage {
            ChainStage::Amp => (amp_label.clone(), true, AMBER),
            ChainStage::Cab => (cab_label.clone(), !cab_from_amp, AMBER),
            _ => {
                let Some((label, on)) = pedal_stage_state(params, slot_stage) else {
                    continue;
                };
                if !on && !focused {
                    continue;
                }
                (label.to_owned(), on, ACCENT)
            }
        };
        let style = if selected_here {
            selected(color)
        } else if lit {
            Style::default().fg(color)
        } else {
            Style::default().fg(DIM)
        };
        push_stage(&mut chain, label, style);
    }
    // The hosted plugin insert (if any) runs post-rack, pre-master.
    if let Some(name) = plugin {
        push_stage(&mut chain, format!("🔌 {name}"), Style::default().fg(AMBER));
    }
    push_stage(&mut chain, "OUTPUT".to_owned(), Style::default().fg(CHROME));

    // Half / half-ish: live order on the left (70%), single-line input/output
    // mini-bars on the right (30%). The chain clips at its boundary on long
    // boards; the bars stay vertically centered on the box's content row.
    let halves = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Ratio(7, 10), Constraint::Ratio(3, 10)])
        .split(inner);
    f.render_widget(Paragraph::new(Line::from(chain)), halves[0]);

    let meter_rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(0),
            Constraint::Length(1),
            Constraint::Min(0),
        ])
        .split(halves[1]);
    let bars = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Ratio(1, 2), Constraint::Ratio(1, 2)])
        .split(meter_rows[1]);
    render_vu_row(f, bars[0], &in_label(cal), levels.input.load(Relaxed));
    render_vu_row(f, bars[1], "OUT ", levels.output.load(Relaxed));
}

/// Ribbon label + live on/off for a pedal stage (`None` for the amp and cab).
fn pedal_stage_state(params: &Params, stage: ChainStage) -> Option<(&'static str, bool)> {
    let on = |f: &std::sync::atomic::AtomicBool| f.load(Relaxed);
    match stage {
        ChainStage::Gate => Some(("GATE", on(&params.ng_enabled))),
        ChainStage::Whammy => Some(("WHAMMY", on(&params.pitch_enabled))),
        ChainStage::Wah => Some(("WAH", on(&params.wah_enabled))),
        ChainStage::Comp => Some(("COMP", on(&params.cmp_enabled))),
        ChainStage::Fuzz => Some(("FUZZ", on(&params.fz_enabled))),
        ChainStage::Ts => Some(("TS-808", on(&params.ts_enabled))),
        ChainStage::Ds => Some(("DS-1", on(&params.ds_enabled))),
        ChainStage::Metal => Some(("ML-2", on(&params.ml_enabled))),
        ChainStage::PreEq => Some(("PRE-EQ", on(&params.peq_enabled))),
        ChainStage::Vibe => Some(("VIBE", on(&params.uv_enabled))),
        ChainStage::Boost => Some(("BOOST", on(&params.boost_enabled))),
        ChainStage::Geq => Some(("G-EQ", on(&params.geq_enabled))),
        ChainStage::Eq => Some(("EQ", on(&params.eq_enabled))),
        ChainStage::Flanger => Some(("FLANGER", on(&params.fl_enabled))),
        ChainStage::Chorus => Some(("CHORUS", on(&params.ch_enabled))),
        ChainStage::Phaser => Some(("PHASER", on(&params.ph_enabled))),
        ChainStage::Trem => Some(("TREM", on(&params.trem_enabled))),
        ChainStage::Delay => Some(("DELAY", on(&params.delay_enabled))),
        ChainStage::Reverb => Some(("REVERB", on(&params.rev_enabled))),
        ChainStage::Amp | ChainStage::Cab => None,
    }
}

/// The header's input label: the applied trim when calibrated, `uncal`
/// otherwise. A live `CLIP` is shown by the calibration wizard, not here.
fn in_label(cal: &InputCalibration) -> String {
    let trim = cal.trim_db.load(Relaxed);
    if trim.abs() < 0.05 {
        "IN uncal".to_string()
    } else {
        format!("IN {trim:+.1}")
    }
}

/// One single-line level bar for the header mini-meters. Deliberately subdued
/// header chrome: every color is shaded toward black (terminals have no alpha,
/// so this is the transparency stand-in) and the label is un-bolded. Same fill
/// math and green/amber/red thresholds as always — just quieter.
fn render_vu_row(f: &mut Frame, area: Rect, label: &str, level: f32) {
    /// Dim factor for meter chrome: hush the bars without losing the hues.
    const METER_DIM: f32 = 0.45;
    let db = amp_to_db(level);
    let fill = ((db + 60.0) / 60.0).clamp(0.0, 1.0) as f64;

    let bar_width = (area.width as usize).saturating_sub(label.len() + 2 + 10);
    let filled = (fill * bar_width as f64) as usize;

    let green_end = (bar_width as f64 * 0.72) as usize;
    let yellow_end = (bar_width as f64 * 0.88) as usize;

    let mut spans = vec![
        Span::styled(label, Style::default().fg(shade(CHROME, METER_DIM))),
        Span::styled("▐", Style::default().fg(shade(DIM, METER_DIM))),
    ];

    for i in 0..bar_width {
        let ch = if i < filled { '█' } else { '░' };
        let color = if i < filled {
            if i < green_end {
                shade(SAFE, METER_DIM)
            } else if i < yellow_end {
                shade(WARN, METER_DIM)
            } else {
                shade(HOT, METER_DIM)
            }
        } else {
            shade(Color::Rgb(30, 30, 30), METER_DIM)
        };
        spans.push(Span::styled(ch.to_string(), Style::default().fg(color)));
    }
    spans.push(Span::styled(
        "▌",
        Style::default().fg(shade(DIM, METER_DIM)),
    ));
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

// ── Amplifier head + cabinet/mic ──────────────────────────────────────────────
// Panel 2 as two side-by-side boxes, each with its own 4 borders: the amp box
// (tone-stack knobs, blank bottom margin, grille) and the cab box (mic knobs,
// bottom margin, grille). Model picking lives in the `A`/`C` browser modals;
// the titles show the active models.
fn render_amp_panel(
    f: &mut Frame,
    area: Rect,
    params: &Params,
    focus: Option<usize>,
    ext_cab: Option<&str>,
    ext_amp: Option<&str>,
) {
    let amp_active = focus.is_some_and(|i| (AMP_START..AMP_END).contains(&i));
    let mic_active = focus.is_some_and(|i| (MIC_START..MIC_END).contains(&i));
    // Each box lights up while its own knob row owns focus.
    let amp_box_active = amp_active;
    let cab_box_active = mic_active;
    let dim = |active: bool| {
        if active {
            Modifier::empty()
        } else {
            Modifier::DIM
        }
    };

    // Side-by-side boxes filling the whole area; each owns its grille row.
    let boxes = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Ratio(7, 10), Constraint::Ratio(3, 10)])
        .split(area);

    render_amp_box(
        f,
        boxes[0],
        params,
        focus,
        ext_amp,
        amp_box_active,
        dim(amp_box_active),
    );
    render_cab_box(
        f,
        boxes[1],
        params,
        focus,
        ext_cab,
        cab_box_active,
        dim(cab_box_active),
    );
}

/// Left box: amp tone-stack knobs, blank bottom margin, grille strip.
#[allow(clippy::too_many_arguments)]
fn render_amp_box(
    f: &mut Frame,
    area: Rect,
    params: &Params,
    focus: Option<usize>,
    ext_amp: Option<&str>,
    box_active: bool,
    dim: Modifier,
) {
    // The amp box reflects the active amp: a loaded AU's name (with the tone-stack
    // knobs inert) or the built-in amp model.
    let amp_name = match ext_amp {
        Some(name) => format!("AU: {name}"),
        None => params.amp_model().name().to_uppercase(),
    };
    let border_color = border_glyph(box_active);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Thick)
        .border_style(border_style(box_active))
        .title(Line::from(vec![
            Span::styled("┤ ", Style::default().fg(border_color)),
            Span::styled(
                amp_name,
                Style::default()
                    .fg(AMBER)
                    .add_modifier(Modifier::BOLD)
                    .add_modifier(dim),
            ),
            Span::styled(" ├", Style::default().fg(border_color)),
        ]))
        .style(Style::default().bg(Color::Black));
    let inner = block.inner(area);
    f.render_widget(block, area);

    // Knobs, blank bottom margin, grille strip inside the box. Model picking
    // lives in the `A` browser modal; the title shows the active amp.
    let parts = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(6),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .split(inner);

    // Amp front-panel knobs: the active model's own controls, count and labels.
    let controls = params.amp_model().controls();
    let count = controls.len();
    let knob_cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints(
            (0..count)
                .map(|_| Constraint::Ratio(1, count as u32))
                .collect::<Vec<_>>(),
        )
        .split(parts[0]);
    // The tone-stack knobs drive the built-in amp; a loaded AU brings its own gain and
    // tone controls (edited in the AU modal), so they are dimmed while it is active —
    // exactly as the mic knobs are while an external IR is up.
    let amp_live = ext_amp.is_none();
    for (i, knob) in controls.iter().enumerate() {
        let ki = AMP_START + i;
        let val = (KNOBS[ki].param)(params).load(Relaxed);
        render_compact_knob(
            f,
            knob_cols[i],
            knob.label,
            val,
            focus == Some(ki),
            amp_live,
            AMBER,
            !box_active,
        );
    }
    // parts[1] stays blank: bottom margin below the knobs.
    render_grille(
        f,
        parts[2],
        shade(ACCENT, if box_active { 0.45 } else { 0.18 }),
    );
}

/// Right box: cab mic knobs, bottom margin, grille strip.
#[allow(clippy::too_many_arguments)]
fn render_cab_box(
    f: &mut Frame,
    area: Rect,
    params: &Params,
    focus: Option<usize>,
    ext_cab: Option<&str>,
    box_active: bool,
    dim: Modifier,
) {
    // The cab box reflects the active cab. An external amp supplying its own
    // cab (amp+cab mode) bypasses the whole cab stage; otherwise a loaded IR or the
    // built-in cab model is shown.
    let cab_bypassed = cab_bypassed_by_amp(params);
    let cab_name = if cab_bypassed {
        "PLUGIN CAB".to_owned()
    } else {
        match ext_cab {
            Some(name) => format!("IR: {name}"),
            None => params.cab_model().short_name().to_owned(),
        }
    };
    let border_color = border_glyph(box_active);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Thick)
        .border_style(border_style(box_active))
        .title(
            Line::from(vec![
                Span::styled("┤ 🎙 ", Style::default().fg(border_color)),
                Span::styled(
                    cab_name,
                    Style::default()
                        .fg(CHROME)
                        .add_modifier(Modifier::BOLD)
                        .add_modifier(dim),
                ),
                Span::styled(" ├", Style::default().fg(border_color)),
            ])
            .right_aligned(),
        )
        .style(Style::default().bg(Color::Black));
    let inner = block.inner(area);
    f.render_widget(block, area);

    // Mic knobs, blank bottom margin, grille strip inside the box.
    let parts = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(6),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .split(inner);

    // Cabinet mics (position, dynamic↔ribbon blend, room) in front of the cabinet.
    let mic_count = MIC_END - MIC_START;
    let mic_cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints(
            (0..mic_count)
                .map(|_| Constraint::Ratio(1, mic_count as u32))
                .collect::<Vec<_>>(),
        )
        .split(parts[0]);
    // The mic knobs only colour the built-in cab's multi-mic blend; they are inert when
    // a finished IR is the cab, or when an external amp supplies its own cab.
    let mic_live = ext_cab.is_none() && !cab_bypassed;
    for (i, ki) in (MIC_START..MIC_END).enumerate() {
        let val = (KNOBS[ki].param)(params).load(Relaxed);
        render_compact_knob(
            f,
            mic_cols[i],
            KNOBS[ki].label,
            val,
            focus == Some(ki),
            mic_live,
            CHROME,
            !box_active,
        );
    }

    // parts[1] stays blank: bottom margin mirroring the amp box.
    render_grille(
        f,
        parts[2],
        shade(ACCENT, if box_active { 0.45 } else { 0.18 }),
    );
}

fn render_grille(f: &mut Frame, area: Rect, color: Color) {
    let w = area.width as usize;
    let lines: Vec<Line> = (0..area.height as usize)
        .map(|row| {
            let s: String = (0..w)
                .map(|col| if (row + col) % 2 == 0 { '▚' } else { '▞' })
                .collect();
            Line::from(Span::styled(s, Style::default().fg(color)))
        })
        .collect();
    f.render_widget(Paragraph::new(lines), area);
}

// ── Guitar rig (pedalboard) ───────────────────────────────────────────────────
// Master–detail layout: a boxed tile per pedal (name + LED + values) across
// the top, and a fixed 7-row dial editor for the focused pedal below (open
// top facing the focused tile's open bottom, 6 rows of controls). The focused
// tile drops its bottom border so the two read as one connected flow. Only the
// rig's outer box, the tiles and the editor carry borders; extra terminal space
// flows to the timeline, never into bigger knobs. Screen cost is flat in pedal
// count — adding pedals grows the tile grid, not the editor.
/// Outer height of the detail editor: rule row (1) + 6 knob rows + bottom
/// border (no top border — the rule row is the top edge).
const RIG_DETAIL_H: u16 = 8;
/// Tile height: borders (2) + values row. The LED is the on/off indicator.
const RIG_TILE_H: u16 = 3;

/// Full outer height of the rig panel for the root layout: rig border (2) +
/// tile grid + fixed detail editor. Mirrors the `render_rig` split so the
/// root `Length` matches what the panel actually draws.
fn rig_outer_height(rig_inner_width: u16, board: &[bool]) -> u16 {
    let on_board = (0..PEDALS.len())
        .filter(|&i| board.get(i).copied().unwrap_or(false))
        .count();
    let tile_count = on_board + 1;
    let cols = ((rig_inner_width / 16).max(1) as usize).min(tile_count);
    let tile_rows = tile_count.div_ceil(cols);
    2 + tile_rows as u16 * RIG_TILE_H + RIG_DETAIL_H
}
fn render_rig(f: &mut Frame, area: Rect, params: &Params, board: &[bool], focus: Option<usize>) {
    // The rig is "active" whenever focus is on one of its pedals (or the + ADD
    // tile); otherwise it recedes with the other inactive panels.
    let rig_active = focus
        .is_some_and(|i| i == ADD_TILE || PEDALS.iter().any(|p| (p.start..p.end).contains(&i)));
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Plain)
        .border_style(border_style(rig_active))
        .title(Line::from(Span::styled(
            " GUITAR RIG ",
            Style::default()
                .fg(border_glyph(rig_active))
                .add_modifier(Modifier::BOLD),
        )))
        .style(Style::default().bg(Color::Black));
    let inner = block.inner(area);
    f.render_widget(block, area);

    // Only on-board pedals get tiles, in chain order so the grid mirrors the
    // signal flow; the last tile is always "+ ADD". `board` may be shorter than
    // PEDALS (e.g. the device-setup screen passes an empty slice), so treat
    // missing entries as off-board.
    let on_board: Vec<usize> = rendered_stages(&params.chain_slots(), board)
        .into_iter()
        .filter_map(|(_, stage)| stage.pedal_index())
        .collect();
    let tile_count = on_board.len() + 1;

    // Tiles up top (fixed height), amp-height editor below (fixed, like the amp).
    const TILE_H: u16 = RIG_TILE_H;
    let cols = ((inner.width / 16).max(1) as usize).min(tile_count);
    let tile_rows = tile_count.div_ceil(cols);
    let parts = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(tile_rows as u16 * TILE_H),
            Constraint::Length(RIG_DETAIL_H),
        ])
        .split(inner);

    let grid_rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints(vec![Constraint::Length(TILE_H); tile_rows])
        .split(parts[0]);

    // Notch span for the editor's top edge: the focused tile's cell, but only
    // when it sits in the last grid row (directly above the editor).
    let mut notch: Option<(u16, u16)> = None;
    for r in 0..tile_rows {
        let base = r * cols;
        let n = cols.min(tile_count - base);
        let cells = Layout::default()
            .direction(Direction::Horizontal)
            .constraints(vec![Constraint::Ratio(1, cols as u32); cols])
            .split(grid_rows[r]);
        for (c, cell) in cells.iter().take(n).enumerate() {
            match on_board.get(base + c) {
                Some(&pi) => {
                    let pedal = &PEDALS[pi];
                    let focused_here = focus.is_some_and(|i| (pedal.start..pedal.end).contains(&i));
                    if focused_here && r + 1 == tile_rows {
                        notch = Some((cell.x, cell.width));
                    }
                    render_pedal_tile(f, *cell, pedal, focus, params, !rig_active)
                }
                None => {
                    if focus == Some(ADD_TILE) && r + 1 == tile_rows {
                        notch = Some((cell.x, cell.width));
                    }
                    render_add_tile(f, *cell, focus == Some(ADD_TILE))
                }
            }
        }
    }

    render_pedal_detail(f, parts[1], params, focus, notch);
}

/// The "+ ADD" tile: an empty slot inviting the user to add a pedal.
fn render_add_tile(f: &mut Frame, area: Rect, focused: bool) {
    let color = if focused { ACCENT } else { shade(ACCENT, 0.5) };
    let dim = if focused {
        Modifier::empty()
    } else {
        Modifier::DIM
    };
    // Like a focused pedal tile: open bottom facing the editor's open top.
    let add_borders = if focused {
        Borders::TOP | Borders::LEFT | Borders::RIGHT
    } else {
        Borders::ALL
    };
    let block = Block::default()
        .borders(add_borders)
        .border_type(BorderType::Plain)
        .border_style(Style::default().fg(color).add_modifier(dim))
        .title(Line::from(Span::styled(
            " + ADD ",
            Style::default()
                .fg(color)
                .add_modifier(Modifier::BOLD)
                .add_modifier(dim),
        )))
        .style(Style::default().bg(Color::Black));
    let inner = block.inner(area);
    f.render_widget(block, area);

    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                "＋",
                Style::default()
                    .fg(color)
                    .add_modifier(Modifier::BOLD)
                    .add_modifier(dim),
            ),
            Span::styled(
                if focused { "  Enter" } else { "" },
                Style::default().fg(DIM).add_modifier(dim),
            ),
        ]))
        .alignment(Alignment::Center),
        inner,
    );
}

/// A compact pedal tile: name + LED in the title, all knob values on one line.
/// The focused pedal's tile lights up to its full livery; every
/// other tile is heavily faded — and fades further when the whole rig is unfocused
/// — so the focused region reads at a glance.
fn render_pedal_tile(
    f: &mut Frame,
    area: Rect,
    pedal: &Pedal,
    focus: Option<usize>,
    params: &Params,
    rig_dim: bool,
) {
    let on = (pedal.enabled)(params).load(Relaxed);
    let active = focus.is_some_and(|i| (pedal.start..pedal.end).contains(&i));
    // Unfocused tiles are dimmed via the `DIM` attribute; the shade adds a second,
    // colour-level fade so an inactive tile truly recedes (a terminal has no alpha).
    let dim = if active {
        Modifier::empty()
    } else {
        Modifier::DIM
    };
    let body = if active {
        pedal.color
    } else if rig_dim {
        shade(pedal.color, 0.12)
    } else if on {
        shade(pedal.color, 0.55)
    } else {
        shade(pedal.color, 0.25)
    };
    let name_color = if active {
        pedal.color
    } else if on {
        shade(pedal.color, 0.7)
    } else {
        shade(pedal.color, 0.4)
    };

    let led = if on {
        Span::styled(
            "◉",
            Style::default()
                .fg(Color::Rgb(255, 70, 70))
                .add_modifier(Modifier::BOLD)
                .add_modifier(dim),
        )
    } else {
        Span::styled("○", Style::default().fg(OFF).add_modifier(dim))
    };

    let title = Line::from(vec![
        Span::styled(
            format!(" {} ", pedal.name),
            Style::default()
                .fg(name_color)
                .add_modifier(Modifier::BOLD)
                .add_modifier(dim),
        ),
        led,
        Span::raw(" "),
    ]);

    // The focused tile drops its bottom border: open bottom facing the editor's
    // open top reads as one connected flow, and the side borders run the full
    // cell height toward the body.
    let tile_borders = if active {
        Borders::TOP | Borders::LEFT | Borders::RIGHT
    } else {
        Borders::ALL
    };
    let block = Block::default()
        .borders(tile_borders)
        .border_type(BorderType::Plain)
        .border_style(Style::default().fg(body).add_modifier(dim))
        .title(title)
        .style(Style::default().bg(Color::Black));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let values: String = (pedal.start..pedal.end)
        .map(|ki| format!("{:.1}", (KNOBS[ki].param)(params).load(Relaxed) * 10.0))
        .collect::<Vec<_>>()
        .join("  ");
    let value_color = if active {
        pedal.color
    } else if on {
        shade(pedal.color, 0.8)
    } else {
        OFF
    };
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            values,
            Style::default()
                .fg(value_color)
                .add_modifier(Modifier::BOLD)
                .add_modifier(dim),
        )))
        .alignment(Alignment::Center),
        inner,
    );
}

/// The detail editor: full-size dials for whichever pedal currently has focus.
/// When focus is elsewhere (amp/mic/selectors) it shows a hint instead.
/// The tile above already names the pedal, so the editor carries no title: an
/// open-topped box (left/right/bottom only) facing the focused tile's open
/// bottom, knobs filling the whole interior. The editor takes on the focused
/// pedal's livery.
/// Top edge of the detail editor, drawn on the rule row *inside* the side
/// borders: a full `├──┤` rule, or — when `gap` carries the focused tile's
/// `(lo, hi)` column range in rule-local coordinates — the same rule with a
/// gap exactly under the tile, joined with `┘`/`└`. A degenerate gap falls
/// back to the full rule.
fn editor_top_edge(rule: Rect, gap: Option<(usize, usize)>, color: Color) -> Line<'static> {
    let style = Style::default().fg(color);
    let w = rule.width as usize;
    if w == 0 {
        return Line::from(Span::raw(""));
    }
    let full = || {
        let mut s = String::with_capacity(w);
        s.push('├');
        for _ in 1..w.saturating_sub(1) {
            s.push('─');
        }
        if w > 1 {
            s.push('┤');
        }
        s
    };
    let Some((gx0, gx1)) = gap else {
        return Line::from(Span::styled(full(), style));
    };
    // Clamp the gap into the rule row; a degenerate gap means a full rule.
    let (gx0, gx1) = (gx0.min(w), gx1.min(w));
    if gx1 <= gx0 {
        return Line::from(Span::styled(full(), style));
    }
    let mut out = String::with_capacity(w);
    for col in 0..w {
        let ch = if col < gx0 || col >= gx1 {
            if col == 0 {
                '├'
            } else if col + 1 == w {
                '┤'
            } else {
                '─'
            }
        } else if col == gx0 && gx0 > 0 {
            '┘'
        } else if col + 1 == gx1 && gx1 < w {
            '└'
        } else {
            ' '
        };
        out.push(ch);
    }
    Line::from(Span::styled(out, style))
}

fn render_pedal_detail(
    f: &mut Frame,
    area: Rect,
    params: &Params,
    focus: Option<usize>,
    notch: Option<(u16, u16)>,
) {
    let pedal = PEDALS
        .iter()
        .find(|p| focus.is_some_and(|i| (p.start..p.end).contains(&i)));

    // The editor takes on the focused pedal's livery; otherwise it stays dim.
    let border_color = pedal.map_or(DIM, |p| p.color);
    let adding = focus == Some(ADD_TILE);

    let block = Block::default()
        .borders(Borders::LEFT | Borders::RIGHT | Borders::BOTTOM)
        .border_type(BorderType::Plain)
        .border_style(Style::default().fg(border_color))
        .style(Style::default().bg(Color::Black));
    let inner = block.inner(area);
    f.render_widget(block, area);

    // Rule row on top (inside the side borders), knobs below: the rule never
    // shares a row with knob boxes, so the notch can't be overwritten.
    let body = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(6)])
        .split(inner);
    // Notch span into rule-row coordinates as an explicit interval, so the far
    // joint lands exactly under the tile's right edge (width-preserving shifts
    // would drift it by a column at the box edge).
    let gap = notch.map(|(x, w)| {
        let rx = body[0].x as usize;
        let rw = body[0].width as usize;
        (
            (x as usize).saturating_sub(rx),
            (x as usize + w as usize).saturating_sub(rx).min(rw),
        )
    });
    f.render_widget(
        Paragraph::new(editor_top_edge(body[0], gap, border_color)),
        body[0],
    );

    let Some(pedal) = pedal else {
        // No pedal under focus: one-line state label, hint beneath it.
        let (label, hint) = if adding {
            (
                "ADD A PEDAL — Enter",
                "Press Enter to add a pedal to the board.",
            )
        } else {
            (
                "SELECT A PEDAL — ←→",
                "Press 4 for the pedalboard, 1 for the chain.",
            )
        };
        let parts = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(1), Constraint::Min(0)])
            .split(body[1]);
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                label,
                Style::default().fg(if adding { AMBER } else { DIM }),
            ))),
            parts[0],
        );
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(hint, Style::default().fg(DIM))))
                .alignment(Alignment::Center),
            parts[1],
        );
        return;
    };

    let on = (pedal.enabled)(params).load(Relaxed);
    let count = pedal.end - pedal.start;
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints(vec![Constraint::Ratio(1, count as u32); count])
        .split(body[1]);

    for (i, ki) in (pedal.start..pedal.end).enumerate() {
        let val = (KNOBS[ki].param)(params).load(Relaxed);
        let render = match pedal.ui {
            PedalUi::Knobs => render_compact_knob,
            PedalUi::Sliders => render_compact_fader,
        };
        render(
            f,
            cols[i],
            KNOBS[ki].label,
            val,
            focus == Some(ki),
            on,
            pedal.color,
            false,
        );
    }
}

/// Knob cell frame: every knob permanently reserves a 1-cell border footprint
/// on all four sides, and the focused knob alone draws its full box — a
/// `Plain` ACCENT frame. Geometry is identical boxed or not, so moving focus
/// never shifts the layout and dial art stays the same size.
fn knob_cell(f: &mut Frame, area: Rect, focused: bool) -> Rect {
    if area.height < 3 || area.width < 3 {
        return area; // too squeezed for a frame: content full-bleed
    }
    if focused {
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Plain)
            .border_style(Style::default().fg(ACCENT).add_modifier(Modifier::BOLD))
            .style(Style::default().bg(Color::Black));
        let inner = block.inner(area);
        f.render_widget(block, area);
        inner
    } else {
        Rect::new(
            area.x.saturating_add(1),
            area.y.saturating_add(1),
            area.width.saturating_sub(2),
            area.height.saturating_sub(2),
        )
    }
}

#[allow(clippy::too_many_arguments)]
fn render_compact_knob(
    f: &mut Frame,
    area: Rect,
    label: &str,
    value: f32,
    focused: bool,
    active: bool,
    accent: Color,
    dimmed: bool,
) {
    let fade = if dimmed {
        Modifier::DIM
    } else {
        Modifier::empty()
    };
    // Reserved border footprint + focused-only box (see `knob_cell`).
    let area = knob_cell(f, area, focused);
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(2), Constraint::Length(1)])
        .split(area);

    let dial_color = if focused {
        ACCENT
    } else if active {
        accent
    } else {
        OFF
    };

    let dial_h = (rows[0].height as usize).clamp(2, 5);
    let art: Vec<Line> = build_dial(value, focused, dial_h)
        .iter()
        .map(|l| {
            Line::from(Span::styled(
                l.clone(),
                Style::default().fg(dial_color).add_modifier(fade),
            ))
        })
        .collect();
    f.render_widget(Paragraph::new(art).alignment(Alignment::Center), rows[0]);

    let num = value * 10.0;
    let label_color = if focused {
        ACCENT
    } else if active {
        DIM
    } else {
        OFF
    };
    let value_color = if focused {
        ACCENT
    } else if active {
        accent
    } else {
        OFF
    };

    let label_line = Line::from(vec![
        Span::styled(
            format!("{label} "),
            Style::default()
                .fg(label_color)
                .add_modifier(Modifier::BOLD)
                .add_modifier(fade),
        ),
        Span::styled(
            format!("{num:.1}"),
            Style::default()
                .fg(value_color)
                .add_modifier(Modifier::BOLD)
                .add_modifier(fade),
        ),
    ]);
    f.render_widget(
        Paragraph::new(label_line).alignment(Alignment::Center),
        rows[1],
    );
}

/// A single graphic-EQ band control drawn as a vertical fader instead of a rotary
/// knob — the natural idiom for a slider bank. Same call signature as
/// [`render_compact_knob`] so the detail editor can pick either per pedal.
#[allow(clippy::too_many_arguments)]
fn render_compact_fader(
    f: &mut Frame,
    area: Rect,
    label: &str,
    value: f32,
    focused: bool,
    active: bool,
    accent: Color,
    dimmed: bool,
) {
    let fade = if dimmed {
        Modifier::DIM
    } else {
        Modifier::empty()
    };
    // Reserved border footprint + focused-only box (see `knob_cell`).
    let area = knob_cell(f, area, focused);
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(2), Constraint::Length(1)])
        .split(area);

    let track_color = if focused {
        ACCENT
    } else if active {
        accent
    } else {
        OFF
    };

    // Fixed editor height: cap the track like the dial so extra space (e.g. a
    // short terminal squeezing the layout) never stretches the faders.
    let track_h = (rows[0].height as usize).clamp(2, 4);
    let art: Vec<Line> = build_fader(value, track_h)
        .into_iter()
        .map(|l| {
            Line::from(Span::styled(
                l,
                Style::default().fg(track_color).add_modifier(fade),
            ))
        })
        .collect();
    f.render_widget(Paragraph::new(art).alignment(Alignment::Center), rows[0]);

    let num = value * 10.0;
    let label_color = if focused {
        ACCENT
    } else if active {
        DIM
    } else {
        OFF
    };
    let value_color = if focused {
        ACCENT
    } else if active {
        accent
    } else {
        OFF
    };
    let label_line = Line::from(vec![
        Span::styled(
            format!("{label} "),
            Style::default()
                .fg(label_color)
                .add_modifier(Modifier::BOLD)
                .add_modifier(fade),
        ),
        Span::styled(
            format!("{num:.1}"),
            Style::default()
                .fg(value_color)
                .add_modifier(Modifier::BOLD)
                .add_modifier(fade),
        ),
    ]);
    f.render_widget(
        Paragraph::new(label_line).alignment(Alignment::Center),
        rows[1],
    );
}

/// Builds an ASCII vertical fader `rows` lines tall: a slotted track with a handle
/// that rides from the bottom (`value` 0) to the top (`value` 1). The midpoint
/// (0.5 = flat, an EQ's "no change" detent) is marked so a centred band reads as
/// neutral at a glance.
fn build_fader(value: f32, rows: usize) -> Vec<String> {
    let rows = rows.max(2);
    let last = (rows - 1) as f32;
    // Row 0 is the top (value 1.0); the handle drops as the value falls.
    let handle = ((1.0 - value.clamp(0.0, 1.0)) * last).round() as usize;
    let mid = (last / 2.0).round() as usize;
    (0..rows)
        .map(|r| {
            if r == handle {
                "━█━".to_string()
            } else if r == mid {
                " ┿ ".to_string()
            } else {
                " │ ".to_string()
            }
        })
        .collect()
}

/// Single-row footer. The full key list lives in the `K` cheat-sheet modal, so
/// this only advertises it (plus quit); transient status messages take over the
/// row while they are shown.
fn render_help(f: &mut Frame, area: Rect, status: Option<&str>) {
    if let Some(msg) = status {
        let help = Paragraph::new(Line::from(vec![Span::styled(
            format!(" {msg} "),
            Style::default().fg(SAFE).add_modifier(Modifier::BOLD),
        )]))
        .alignment(Alignment::Center)
        .style(Style::default().bg(Color::Black));
        f.render_widget(help, area);
        return;
    }

    let help = Paragraph::new(Line::from(vec![
        Span::styled("K", Style::default().fg(AMBER)),
        Span::styled(" keybindings  ", Style::default().fg(DIM)),
        Span::styled("Q", Style::default().fg(AMBER)),
        Span::styled(" quit", Style::default().fg(DIM)),
    ]))
    .alignment(Alignment::Center)
    .style(Style::default().bg(Color::Black));
    f.render_widget(help, area);
}

/// Full keybinding cheat-sheet, opened with `K`. Sections mirror the footer
/// rows the modal replaces, plus the context keys for the preset browser,
/// the practice timeline, and the metronome.
/// A small centered modal showing offline-export progress.
pub(super) fn render_export_progress(f: &mut Frame, percent: u32) {
    let area = f.area();
    let width = 46.min(area.width);
    let height = 5.min(area.height);
    let x = (area.width.saturating_sub(width)) / 2;
    let y = (area.height.saturating_sub(height)) / 2;
    let rect = Rect {
        x: area.x + x,
        y: area.y + y,
        width,
        height,
    };
    f.render_widget(Clear, rect);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Double)
        .border_style(Style::default().fg(SAFE))
        .title(Span::styled(
            " E X P O R T I N G ",
            Style::default().fg(AMBER).add_modifier(Modifier::BOLD),
        ))
        .style(Style::default().bg(Color::Black));
    let inner = block.inner(rect);
    f.render_widget(block, rect);

    let bar_w = inner.width.saturating_sub(2) as usize;
    let filled = (percent as usize * bar_w / 100).min(bar_w);
    let bar = format!("{}{}", "█".repeat(filled), "·".repeat(bar_w - filled));
    let text = vec![
        Line::from(Span::styled(
            format!("{percent:>3}%"),
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(bar, Style::default().fg(SAFE))),
        Line::from(Span::styled("Esc to cancel", Style::default().fg(DIM))),
    ];
    f.render_widget(Paragraph::new(text), inner);
}

pub(super) fn render_help_modal(f: &mut Frame) {
    // Modal-local description gray: brighter than the shared `DIM` (which is
    // near-invisible on black at this size) but still a step below `CHROME`
    // so the key → description hierarchy survives.
    const HELP_DESC: Color = Color::Rgb(0xA6, 0xA6, 0xA6);
    let key = |k: &'static str| Span::styled(k, Style::default().fg(AMBER));
    let desc = |d: &'static str| Span::styled(d, Style::default().fg(HELP_DESC));
    let head = |h: &'static str| {
        Line::from(vec![Span::styled(
            h,
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        )])
    };
    let row = |k: &'static str, d: &'static str| Line::from(vec![key(k), desc(d)]);

    let mut lines: Vec<Line> = vec![
        head("Panels & chain"),
        row(
            "  1 / 2 / 3 / 4",
            "  focus chain, amp, timeline, pedals (again: hide)",
        ),
        row(
            "  Tab / Shift-Tab",
            "  cycle amp/cab (2) or pedals (4) in the panel",
        ),
        row("  ←/→", "  move inside the focused panel"),
        row("  [ / ]", "  move the ribbon's stage earlier / later"),
        row(
            "  note",
            "  between AMP and CAB = line-level; a full-rig AU is post-mic",
        ),
        row("  Space", "  bypass stage, pedal, or transport under focus"),
        head("Play & edit"),
        row("  ↑/↓  +/−", "  knob / adjust value"),
        row("  D", "  remove pedal from the board"),
        row("  A", "  amp model browser"),
        row("  C", "  cabinet model browser"),
        row("  I / X", "  IR browser / IR bypass"),
        row("  O", "  change audio devices"),
        row("  W", "  studio-master width: neutral / wide"),
    ];
    #[cfg(feature = "au")]
    lines.push(row("  Z / U", "  amp-plugin bypass / browser"));
    lines.extend([
        head("Tools"),
        row("  P", "  preset browser (rig snapshot)"),
        row("  S", "  save current rig as preset"),
        row("  J", "  session browser: new / save / save as / load"),
        row("  T", "  chromatic tuner"),
        row("  M", "  practice metronome (+/− tempo, Space on/off)"),
        row("  N", "  calibrate input level"),
    ]);
    #[cfg(feature = "clap")]
    lines.push(row("  V", "  plugin browser"));
    lines.extend([
        row(
            "  R",
            "  arm / stop a dry raw take (auto-plays; lands on the timeline)",
        ),
        head("Looper (monitor only)"),
        row("  Y", "  record / pause / resume the loop"),
        row("  F", "  clear the loop"),
        row("  , / .", "  toggle overdub / undo the last overdub layer"),
        row("  Q / Ctrl-C", "  quit"),
        head("Panels"),
        row("  B", "  import a track at the playhead (MP3 / WAV / FLAC)"),
        head("Preset browser"),
        row(
            "  type",
            "  filter by name / description (Backspace; Esc clears)",
        ),
        row("  ↑/↓  Enter", "  navigate / apply (audio uninterrupted)"),
        row("  S / E / I", "  save / export / import"),
        row("  F", "  favorite the selected preset"),
        row("  X", "  A/B: swap the last two applied presets"),
        row("  D", "  delete (user presets only)"),
        head("Timeline (focused)"),
        row("  Space", "  play / pause (any row)"),
        row("  M", "  mute the selected track"),
        row("  Enter", "  jump to start (loop in-point if looped)"),
        row("  ↑/↓  ←/→", "  select row / seek by the step"),
        row("  +/−", "  seek step: 1 / 5 / 10 / 30 s"),
        row("  G", "  selected track gain"),
        row("  H", "  move selected clip"),
        row("  E", "  export unmuted raw takes (WAV)"),
        row("  [ / ]  L", "  loop in-point / out-point, toggle loop"),
        row("  Del", "  remove selected track"),
    ]);

    let width = (f.area().width * 55 / 100).max(40);
    let height = ((lines.len() as u16 + 6).min(f.area().height)).max(8);
    let area = Rect {
        x: f.area().x + (f.area().width - width) / 2,
        y: f.area().y + (f.area().height - height) / 2,
        width,
        height,
    };
    f.render_widget(Clear, area);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Double)
        .border_style(Style::default().fg(ACCENT))
        .title(Span::styled(
            " K E Y B I N D I N G S ",
            Style::default().fg(AMBER).add_modifier(Modifier::BOLD),
        ))
        .style(Style::default().bg(Color::Black));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(inner);
    f.render_widget(Paragraph::new(lines), rows[0]);
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("Esc / K", Style::default().fg(AMBER)),
            Span::styled(" close", Style::default().fg(HELP_DESC)),
        ]))
        .alignment(Alignment::Center),
        rows[1],
    );
}

/// Modal listing the pedals not currently on the board. `available` holds their
/// `PEDALS` indices; `cursor` is the highlighted row.
pub(super) fn render_add_pedal_modal(f: &mut Frame, available: &[usize], cursor: usize) {
    let area = {
        let a = f.area();
        let width = (a.width * 45 / 100).max(24);
        let height = ((available.len() as u16 + 4).min(a.height)).max(6);
        Rect {
            x: a.x + (a.width - width) / 2,
            y: a.y + (a.height - height) / 2,
            width,
            height,
        }
    };
    f.render_widget(Clear, area);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Double)
        .border_style(Style::default().fg(ACCENT))
        .title(Span::styled(
            " A D D   P E D A L ",
            Style::default().fg(AMBER).add_modifier(Modifier::BOLD),
        ))
        .style(Style::default().bg(Color::Black));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(inner);

    if available.is_empty() {
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "All pedals are on the board.",
                Style::default().fg(DIM),
            )))
            .alignment(Alignment::Center),
            rows[0],
        );
    } else {
        let lines: Vec<Line> = available
            .iter()
            .enumerate()
            .map(|(i, &pi)| {
                let p = &PEDALS[pi];
                let selected = i == cursor;
                let (prefix, style) = if selected {
                    (
                        "▶ ",
                        Style::default()
                            .fg(p.color)
                            .add_modifier(Modifier::BOLD | Modifier::REVERSED),
                    )
                } else {
                    ("  ", Style::default().fg(p.color))
                };
                Line::from(vec![
                    Span::styled(
                        prefix,
                        Style::default().fg(if selected { ACCENT } else { DIM }),
                    ),
                    Span::styled(p.name, style),
                ])
            })
            .collect();
        f.render_widget(Paragraph::new(lines), rows[0]);
    }

    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("↑/↓", Style::default().fg(AMBER)),
            Span::styled(" navigate  ", Style::default().fg(DIM)),
            Span::styled("Enter", Style::default().fg(AMBER)),
            Span::styled(" add  ", Style::default().fg(DIM)),
            Span::styled("Esc", Style::default().fg(AMBER)),
            Span::styled(" close", Style::default().fg(DIM)),
        ]))
        .alignment(Alignment::Center),
        rows[1],
    );
}

/// Shared modal shell for the model pickers: centered `Double` box over `Clear`.
fn picker_shell(f: &mut Frame, title: &str, rows: usize) -> Rect {
    let area = {
        let a = f.area();
        let width = (a.width * 45 / 100).max(30);
        let height = ((rows as u16 + 4).min(a.height)).max(6);
        Rect {
            x: a.x + (a.width - width) / 2,
            y: a.y + (a.height - height) / 2,
            width,
            height,
        }
    };
    f.render_widget(Clear, area);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Double)
        .border_style(Style::default().fg(ACCENT))
        .title(Span::styled(
            title.to_owned(),
            Style::default().fg(AMBER).add_modifier(Modifier::BOLD),
        ))
        .style(Style::default().bg(Color::Black));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(inner);
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("↑/↓", Style::default().fg(AMBER)),
            Span::styled(" navigate  ", Style::default().fg(DIM)),
            Span::styled("Enter", Style::default().fg(AMBER)),
            Span::styled(" select  ", Style::default().fg(DIM)),
            Span::styled("Esc", Style::default().fg(AMBER)),
            Span::styled(" close", Style::default().fg(DIM)),
        ]))
        .alignment(Alignment::Center),
        rows[1],
    );
    rows[0]
}

/// One picker row: `▶` cursor plus label, `REVERSED+BOLD` on the cursor row,
/// dimmed when the row's option is currently bypassed elsewhere.
fn picker_row(label: String, color: Color, selected: bool, dimmed: bool) -> Line<'static> {
    let (prefix, name_style) = if selected {
        (
            "▶ ",
            Style::default()
                .fg(color)
                .add_modifier(Modifier::BOLD | Modifier::REVERSED),
        )
    } else {
        ("  ", Style::default().fg(color))
    };
    let style = if dimmed {
        name_style.add_modifier(Modifier::DIM)
    } else {
        name_style
    };
    Line::from(vec![
        Span::styled(
            prefix.to_owned(),
            Style::default().fg(if selected { ACCENT } else { DIM }),
        ),
        Span::styled(label, style),
    ])
}

/// Amp picker modal (`A`): built-in models plus the loaded AU (when any).
/// `cursor` is preselected on the current pick; built-ins dim while an AU is
/// active. Picking a built-in returns to the built-in amp.
pub(super) fn render_amp_modal(
    f: &mut Frame,
    params: &Params,
    au_name: Option<&str>,
    au_loaded: bool,
    cursor: usize,
) {
    let rows = AmpModel::ALL.len() + usize::from(au_loaded);
    let list = picker_shell(f, " S E L E C T   A M P ", rows);
    let amp_ext_active = params.amp_external_active.load(Relaxed);

    let lines: Vec<Line> = AmpModel::ALL
        .iter()
        .enumerate()
        .map(|(i, m)| picker_row(m.name().to_owned(), AMBER, i == cursor, amp_ext_active))
        .chain(au_loaded.then(|| {
            picker_row(
                format!("AU: {}", au_name.unwrap_or("external amp")),
                AMBER,
                AmpModel::ALL.len() == cursor,
                false,
            )
        }))
        .collect();
    f.render_widget(Paragraph::new(lines), list);
}

/// Cabinet picker modal (`C`): built-in models plus the loaded IR (when any).
/// `cursor` is preselected on the current pick. Picking a built-in returns to
/// the built-in cab (the IR stays loaded for `X`). While an external amp
/// supplies its own cab the built-ins dim with a note — picks still apply for
/// when the built-in path runs again.
pub(super) fn render_cab_modal(
    f: &mut Frame,
    params: &Params,
    ir_name: Option<&str>,
    ir_loaded: bool,
    cursor: usize,
) {
    let bypassed = cab_bypassed_by_amp(params);
    let rows = CabModel::ALL.len() + usize::from(ir_loaded) + usize::from(bypassed);
    let list = picker_shell(f, " S E L E C T   C A B ", rows);

    let lines: Vec<Line> = CabModel::ALL
        .iter()
        .enumerate()
        .map(|(i, m)| picker_row(m.name().to_owned(), CHROME, i == cursor, bypassed))
        .chain(ir_loaded.then(|| {
            picker_row(
                format!("IR: {}", ir_name.unwrap_or("external IR")),
                CHROME,
                CabModel::ALL.len() == cursor,
                false,
            )
        }))
        .chain(bypassed.then(|| {
            Line::from(Span::styled(
                " external amp supplies the cab — picks apply on return",
                Style::default().fg(DIM),
            ))
        }))
        .collect();
    f.render_widget(Paragraph::new(lines), list);
}

/// Builds an ASCII rotary knob `rows` lines tall: a hub, a dotted rim, and a
/// pointer "hand" that swings 270° (7 o'clock → 5 o'clock) as `value` goes
/// 0.0 → 1.0. The hand is a real line whose glyph and direction track the
/// value, so even neighbouring settings look visibly different.
fn build_dial(value: f32, focused: bool, rows: usize) -> Vec<String> {
    use std::f32::consts::PI;

    let rows = rows.max(2);
    let cols = rows * 2 - 1;

    let start_deg = 225.0_f32;
    let sweep = 270.0_f32;
    let deg = start_deg - value.clamp(0.0, 1.0) * sweep;
    let angle = deg * PI / 180.0;

    // rx:ry = 2:1 compensates for terminal char aspect ratio (~2x taller than wide)
    let cx = (cols as f32 - 1.0) / 2.0;
    let cy = (rows as f32 - 1.0) / 2.0;
    let rx = cx.max(1.0);
    let ry = cy.max(0.5);

    let mut grid = vec![vec![' '; cols]; rows];
    let put = |grid: &mut Vec<Vec<char>>, x: isize, y: isize, ch: char| {
        if x >= 0 && (x as usize) < cols && y >= 0 && (y as usize) < rows {
            grid[y as usize][x as usize] = ch;
        }
    };

    // Dotted rim, drawn only across the live 270° sweep.
    if rows >= 3 {
        for row in 0..rows as isize {
            for col in 0..cols as isize {
                let dx = (col as f32 - cx) / rx;
                let dy = (row as f32 - cy) / ry;
                let dist = (dx * dx + dy * dy).sqrt();
                if (dist - 1.0).abs() < 0.35 {
                    let a = (-(row as f32 - cy)).atan2(col as f32 - cx).to_degrees();
                    let rel = (start_deg - a).rem_euclid(360.0);
                    if rel <= sweep {
                        put(&mut grid, col, row, '·');
                    }
                }
            }
        }
    }

    // Pointer. Small (pedal) dials draw a full "hand" line from the hub to the
    // rim so their limited resolution still reads as rotation; larger (amp)
    // dials stay clean with just a tip marker at the rim.
    let tip = if focused { '◆' } else { '◇' };

    let x = (cx + angle.cos() * rx).round() as isize;
    let y = (cy - angle.sin() * ry).round() as isize;
    put(&mut grid, x, y, tip);

    // Center hub last so it always shows.
    put(&mut grid, cx.round() as isize, cy.round() as isize, '●');

    grid.into_iter().map(|r| r.into_iter().collect()).collect()
}

/// Scales an RGB color toward black by `factor` (0.0 = black, 1.0 = unchanged).
fn shade(c: Color, factor: f32) -> Color {
    match c {
        Color::Rgb(r, g, b) => Color::Rgb(
            (f32::from(r) * factor) as u8,
            (f32::from(g) * factor) as u8,
            (f32::from(b) * factor) as u8,
        ),
        other => other,
    }
}

/// Border style for a structural panel: full-bright [`ACCENT`] when the region is
/// active/focused, otherwise a heavily faded accent with the terminal's `DIM`
/// attribute. A terminal has no alpha channel, so `DIM` + a dark shade is the
/// closest we get to a "transparent" inactive border — it recedes so the focused
/// region stands out.
fn border_style(active: bool) -> Style {
    if active {
        Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)
    } else {
        Style::default()
            .fg(shade(ACCENT, 0.35))
            .add_modifier(Modifier::DIM)
    }
}

/// Accent color for a region's on-border glyphs (titles), matching [`border_style`].
fn border_glyph(active: bool) -> Color {
    if active { ACCENT } else { shade(ACCENT, 0.35) }
}

/// Whether an external amp is active *and* supplying its own cab (amp+cab mode), so the
/// built-in cabinet stage — model selector and mic knobs — is bypassed. False in
/// amp-only mode, where the built-in cab/IR stays in the path.
fn cab_bypassed_by_amp(params: &Params) -> bool {
    params.amp_external_active.load(Relaxed) && !params.amp_external_amp_only.load(Relaxed)
}

fn amp_to_db(amp: f32) -> f32 {
    if amp < 1e-6 {
        -120.0
    } else {
        20.0 * amp.log10()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsp::{AmpModel, CabModel, Levels};
    use ratatui::{Terminal, backend::TestBackend};

    /// A comfortably large canvas so nothing the tests assert on is clipped by a
    /// small terminal. The layout is responsive, so exact dimensions only matter
    /// for the golden snapshot (which is re-blessed with `cargo insta review`).
    const W: u16 = 170;
    const H: u16 = 55;

    /// Flatten the rendered cells into plain text, one row per line. Styles are
    /// dropped — layout invariants live in the config tests, this checks the
    /// visible glyphs.
    fn screen_text(term: &Terminal<TestBackend>) -> String {
        let buf = term.backend().buffer();
        let area = buf.area();
        let mut out = String::new();
        for y in 0..area.height {
            for x in 0..area.width {
                out.push_str(buf[(x, y)].symbol());
            }
            out.push('\n');
        }
        out
    }

    /// Render one knob cell into a scratch area and return its glyphs.
    fn knob_text(focused: bool) -> String {
        let mut term = Terminal::new(TestBackend::new(24, 8)).expect("test backend");
        term.draw(|f| {
            render_compact_knob(f, f.area(), "GAIN", 0.5, focused, true, AMBER, false);
        })
        .expect("draw");
        screen_text(&term)
    }

    /// The focused knob alone draws a full box (top, sides, bottom); an
    /// unfocused knob renders the same content with blank insets and no box.
    #[test]
    fn focused_knob_gets_a_border_box() {
        let on = knob_text(true);
        assert!(
            on.contains('┌') && on.contains('┐') && on.contains('└') && on.contains('┘'),
            "focused knob draws no box:\n{on}"
        );
        assert!(on.contains("GAIN"), "focused knob lost its label");
        let off = knob_text(false);
        assert!(
            !off.contains('┌') && !off.contains('└'),
            "unfocused knob draws a box:\n{off}"
        );
        assert!(off.contains("GAIN"), "unfocused knob lost its label");
    }

    /// Flatten a [`Line`] to plain text for glyph assertions.
    fn line_text(line: Line<'_>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    /// The editor top edge is a full rule without a notch, and a gapped rule
    /// with `┘`/`└` joints exactly under the focused tile's span with one.
    /// (The rule row lives inside the side borders, so its ends join them
    /// with `├`/`┤`.)
    #[test]
    fn editor_top_edge_notches_under_the_focused_tile() {
        use ratatui::layout::Rect;
        // Rule row 20 wide: gap is a rule-local (lo, hi) interval.
        let rule = Rect::new(30, 0, 20, 1);
        // No notch: full rule joining both sides.
        assert_eq!(
            line_text(editor_top_edge(rule, None, ACCENT)),
            "├──────────────────┤"
        );
        // Gap at columns 5..13: joints meet the tile sides.
        assert_eq!(
            line_text(editor_top_edge(rule, Some((5, 13)), ACCENT)),
            "├────┘      └──────┤"
        );
        // Gap flush with an edge drops that join; degenerate gaps fall back.
        assert_eq!(
            line_text(editor_top_edge(rule, Some((0, 4)), ACCENT)),
            "   └───────────────┤"
        );
        assert_eq!(
            line_text(editor_top_edge(rule, Some((40, 44)), ACCENT)),
            "├──────────────────┤"
        );
        assert_eq!(
            line_text(editor_top_edge(rule, Some((5, 5)), ACCENT)),
            "├──────────────────┤"
        );
    }

    /// Render the main screen with the given board/focus and return its glyphs.
    fn render(board: &[bool], focus: Option<usize>) -> (Terminal<TestBackend>, String) {
        let params = Params::new();
        let text = render_with(&params, board, focus, |_| {});
        (
            Terminal::new(TestBackend::new(W, H)).expect("test backend"),
            text,
        )
    }

    /// Render the main screen for `params`, then let `overlay` draw a modal on top
    /// (exactly as the event loop composites modals over the board), and return the
    /// flattened glyphs.
    fn render_with(
        params: &Params,
        board: &[bool],
        focus: Option<usize>,
        overlay: impl FnOnce(&mut Frame),
    ) -> String {
        let levels = Levels::new();
        let mut term = Terminal::new(TestBackend::new(W, H)).expect("test backend");
        term.draw(|f| {
            draw(
                f,
                params,
                &levels,
                &InputCalibration::default(),
                focus,
                board,
                false,
                false,
                None,
                None,
                None,
                None,
                Panels::all_visible(),
                ChainStage::Amp,
                None,
            );
            overlay(f);
        })
        .expect("draw");
        screen_text(&term)
    }

    fn board_all(on: bool) -> Vec<bool> {
        vec![on; PEDALS.len()]
    }

    /// The board the app actually boots with, derived from the default enabled
    /// flags (TS-808 + Noise Gate on) exactly like `sync_board` does at startup.
    /// Only the `clap`-gated golden tests use it.
    #[cfg(feature = "clap")]
    fn default_board(params: &Params) -> Vec<bool> {
        use std::sync::atomic::Ordering::Relaxed;
        PEDALS
            .iter()
            .map(|p| (p.enabled)(params).load(Relaxed))
            .collect()
    }

    /// Golden snapshot of a realistic default screen — the board the app boots
    /// with (TS-808 + Noise Gate tiles), focused on the first on-board pedal so
    /// the detail editor's dials are captured too. This is the tripwire for the
    /// overall layout: any unintended change to spacing, labels, tiles, or dials
    /// shows up as a diff; intentional changes are re-blessed with
    /// `cargo insta accept`.
    ///
    /// Gated on `clap`: the help footer's `V plugins` key is `clap`-only, so the
    /// rendered chrome (and thus every golden below) is specific to the default
    /// build the snapshots were captured in. CI runs default features.
    #[cfg(feature = "clap")]
    #[test]
    fn snapshot_default_screen() {
        let params = Params::new();
        let board = default_board(&params);
        let first_on = board.iter().position(|&on| on).expect("a default pedal");
        let text = render_with(&params, &board, Some(PEDALS[first_on].start), |_| {});
        insta::assert_snapshot!("default_screen", text);
    }

    /// Rendering must never panic across a spread of states: empty board, full
    /// board, focus on a pedal knob, focus on the +ADD tile, and recording on.
    #[test]
    fn rendering_is_panic_free_across_states() {
        let params = Params::new();
        let levels = Levels::new();
        let cases: [(Vec<bool>, Option<usize>, bool); 4] = [
            (board_all(false), None, false),
            (board_all(true), Some(PEDALS[0].start), false),
            (board_all(true), Some(ADD_TILE), true),
            (board_all(false), Some(AMP_START), true),
        ];
        for (board, focus, rec) in cases {
            let mut term = Terminal::new(TestBackend::new(W, H)).expect("test backend");
            term.draw(|f| {
                draw(
                    f,
                    &params,
                    &levels,
                    &InputCalibration::default(),
                    focus,
                    &board,
                    rec,
                    true,
                    Some("REC…"),
                    None,
                    None,
                    None,
                    Panels::all_visible(),
                    ChainStage::Amp,
                    None,
                );
            })
            .expect("draw");
        }
    }

    /// Every pedal must render intact: focusing each one shows the detail editor
    /// titled with that pedal's full name and every one of its knob labels.
    #[test]
    fn every_pedal_renders_with_its_name_and_knob_labels() {
        for (pi, pedal) in PEDALS.iter().enumerate() {
            let mut board = board_all(false);
            board[pi] = true;
            let (_term, text) = render(&board, Some(pedal.start));
            assert!(
                text.contains(pedal.name),
                "{} name missing from the detail editor",
                pedal.name
            );
            for knob in KNOBS.iter().take(pedal.end).skip(pedal.start) {
                assert!(
                    text.contains(knob.label),
                    "{}: knob label {:?} missing",
                    pedal.name,
                    knob.label
                );
            }
        }
    }

    /// Every amp model must be pickable: the amp modal lists each by name.
    #[test]
    fn every_amp_model_renders_its_name() {
        let params = Params::new();
        let mut term = Terminal::new(TestBackend::new(W, H)).expect("test backend");
        term.draw(|f| {
            render_amp_modal(f, &params, None, false, 0);
        })
        .expect("draw");
        let text = screen_text(&term);
        for model in AmpModel::ALL {
            assert!(
                text.contains(model.name()),
                "amp {:?} missing from the picker modal",
                model.name()
            );
        }
    }

    /// An active external amp (hosted AU) must be surfaced in the header in place of
    /// the built-in amp model — the visual counterpart to the external-IR "IR:" label.
    #[test]
    fn external_amp_name_shows_in_ribbon() {
        let params = Params::new();
        params
            .amp_external_active
            .store(true, std::sync::atomic::Ordering::Relaxed);
        let levels = Levels::new();
        let mut term = Terminal::new(TestBackend::new(W, H)).expect("test backend");
        term.draw(|f| {
            draw(
                f,
                &params,
                &levels,
                &InputCalibration::default(),
                None,
                &board_all(false),
                false,
                false,
                None,
                None,
                None,
                Some("Silver Jubilee"),
                Panels::all_visible(),
                ChainStage::Amp,
                None,
            );
        })
        .expect("draw");
        let text = screen_text(&term);
        assert!(
            text.contains("AU: SILVER JUBILEE"),
            "external amp name not shown in the header"
        );
        // The built-in amp model label must not also be shown as the active amp.
        assert!(
            !text.contains("MARSHALL JCM800"),
            "built-in amp label shown while an external amp is active"
        );
    }

    /// Every built-in cabinet must be pickable: the cab modal lists each by name,
    /// plus the loaded IR as an extra row when one is installed.
    #[test]
    fn every_cab_model_renders_its_name() {
        let params = Params::new();
        let mut term = Terminal::new(TestBackend::new(W, H)).expect("test backend");
        term.draw(|f| {
            render_cab_modal(f, &params, Some("Greenback"), true, 0);
        })
        .expect("draw");
        let text = screen_text(&term);
        for model in CabModel::ALL {
            assert!(
                text.contains(model.name()),
                "cab {:?} missing from the picker modal",
                model.name()
            );
        }
        assert!(
            text.contains("IR: Greenback"),
            "loaded IR missing from the picker modal"
        );
    }

    /// The +ADD pedal modal lists every off-board pedal by name.
    #[test]
    fn add_pedal_modal_lists_available_pedals() {
        let params = Params::new();
        let levels = Levels::new();
        let board = board_all(false);
        let available: Vec<usize> = (0..PEDALS.len()).collect();
        let mut term = Terminal::new(TestBackend::new(W, H)).expect("test backend");
        term.draw(|f| {
            draw(
                f,
                &params,
                &levels,
                &InputCalibration::default(),
                None,
                &board,
                false,
                false,
                None,
                None,
                None,
                None,
                Panels::all_visible(),
                ChainStage::Amp,
                None,
            );
            render_add_pedal_modal(f, &available, 0);
        })
        .expect("draw");
        let text = screen_text(&term);
        for &pi in &available {
            assert!(
                text.contains(PEDALS[pi].name),
                "{} missing from the add-pedal modal",
                PEDALS[pi].name
            );
        }
    }

    // ── modal golden snapshots ──────────────────────────────────────────────────
    // Each modal is composited over the realistic default board, just like the
    // event loop draws it. Inputs are fixed in-test (no filesystem), so the goldens
    // are deterministic across machines and CI.

    /// The +ADD picker, over the default board (so its list is the off-board
    /// pedals), cursor on the first entry.
    #[cfg(feature = "clap")]
    #[test]
    fn snapshot_add_pedal_modal() {
        let params = Params::new();
        let board = default_board(&params);
        let available: Vec<usize> = (0..PEDALS.len()).filter(|&i| !board[i]).collect();
        let text = render_with(&params, &board, None, |f| {
            render_add_pedal_modal(f, &available, 0);
        });
        insta::assert_snapshot!("add_pedal_modal", text);
    }

    /// The amp picker over the default board, cursor preselected on the current
    /// model (Mesa by default), no AU loaded.
    #[cfg(feature = "clap")]
    #[test]
    fn snapshot_amp_modal() {
        let params = Params::new();
        let board = default_board(&params);
        let text = render_with(&params, &board, None, |f| {
            render_amp_modal(
                f,
                &params,
                None,
                false,
                crate::ui::input::init_amp_cursor(&params),
            );
        });
        insta::assert_snapshot!("amp_modal", text);
    }

    /// The cab picker over the default board with a loaded (inactive) IR, so
    /// the trailing external row renders.
    #[cfg(feature = "clap")]
    #[test]
    fn snapshot_cab_modal() {
        let params = Params::new();
        let board = default_board(&params);
        let text = render_with(&params, &board, None, |f| {
            render_cab_modal(f, &params, Some("Greenback"), true, 0);
        });
        insta::assert_snapshot!("cab_modal", text);
    }

    /// The preset picker with a fixed System + User preset, cursor on the user
    /// entry (which reveals the `D delete` footer hint and the `[user]` tag).
    #[cfg(feature = "clap")]
    #[test]
    fn snapshot_preset_modal() {
        use crate::preset::{Preset, PresetSource};
        let params = Params::new();
        let mut system = Preset::from_params(
            "Clean Combo".to_string(),
            Some("sparkly cleans".to_string()),
            &params,
        );
        system.source = PresetSource::System;
        let mut user = Preset::from_params(
            "My Lead".to_string(),
            Some("saved rig".to_string()),
            &params,
        );
        user.source = PresetSource::User;
        let presets = vec![system, user];
        let board = default_board(&params);
        // Entries: [Default values, Clean Combo, My Lead] → cursor 2 = the user one.
        let text = render_with(&params, &board, None, |f| {
            crate::ui::presets::render_preset_modal(
                f,
                &presets,
                2,
                &std::collections::HashSet::new(),
                "",
            );
        });
        insta::assert_snapshot!("preset_modal", text);
    }

    /// The preset picker with an active type-to-filter query: "Clean Combo" is
    /// hidden and the title carries the filter text.
    #[cfg(feature = "clap")]
    #[test]
    fn snapshot_preset_modal_filtered() {
        use crate::preset::{Preset, PresetSource};
        let params = Params::new();
        let mut system = Preset::from_params(
            "Clean Combo".to_string(),
            Some("sparkly cleans".to_string()),
            &params,
        );
        system.source = PresetSource::System;
        let mut user = Preset::from_params(
            "My Lead".to_string(),
            Some("saved rig".to_string()),
            &params,
        );
        user.source = PresetSource::User;
        let presets = vec![system, user];
        let board = default_board(&params);
        // Only "My Lead" matches; it becomes visible row 1.
        let text = render_with(&params, &board, None, |f| {
            crate::ui::presets::render_preset_modal(
                f,
                &presets,
                1,
                &std::collections::HashSet::new(),
                "lead",
            );
        });
        insta::assert_snapshot!("preset_modal_filtered", text);
    }

    /// The keybinding cheat-sheet over the default board.
    #[cfg(feature = "clap")]
    #[test]
    fn snapshot_help_modal() {
        let params = Params::new();
        let board = default_board(&params);
        let text = render_with(&params, &board, None, |f| {
            render_help_modal(f);
        });
        insta::assert_snapshot!("help_modal", text);
    }

    /// The save-preset dialog with both fields filled, focus on the name field.
    #[cfg(feature = "clap")]
    #[test]
    fn snapshot_save_preset_dialog() {
        let params = Params::new();
        let board = default_board(&params);
        let text = render_with(&params, &board, None, |f| {
            crate::ui::presets::render_save_dialog(f, "My Lead", "warm mid-gain", 0, None);
        });
        insta::assert_snapshot!("save_preset_dialog", text);
    }

    /// The CLAP plugin browser (Browse view). The list is left empty on purpose —
    /// `open = true` shows the modal WITHOUT calling `open()`, which would scan the
    /// filesystem and make the golden machine-dependent.
    #[cfg(feature = "clap")]
    #[test]
    fn snapshot_plugin_browser_modal() {
        let params = Params::new();
        let mut browser = crate::ui::plugins::PluginBrowser::new(48_000.0, 512);
        browser.open = true;
        let board = default_board(&params);
        let text = render_with(&params, &board, None, |f| browser.render(f));
        insta::assert_snapshot!("plugin_browser_modal", text);
    }

    /// The external-IR browser. As with the plugin browser, the file list is left
    /// empty (no scan) so the golden captures the deterministic empty state.
    #[cfg(feature = "clap")]
    #[test]
    fn snapshot_ir_browser_modal() {
        let params = Params::new();
        let mut browser = crate::ui::ir_browser::IrBrowser::new(48_000.0);
        browser.open = true;
        let board = default_board(&params);
        let text = render_with(&params, &board, None, |f| browser.render(f, false));
        insta::assert_snapshot!("ir_browser_modal", text);
    }

    /// With an external amp active, the IR browser must warn that IRs have no effect
    /// (the AU replaces the built-in amp+cab) rather than silently doing nothing.
    #[test]
    fn ir_browser_warns_when_external_amp_active() {
        let params = Params::new();
        let mut browser = crate::ui::ir_browser::IrBrowser::new(48_000.0);
        browser.open = true;
        let text = render_with(&params, &board_all(false), None, |f| {
            browser.render(f, true)
        });
        assert!(
            text.contains("External amp active"),
            "IR browser should warn while an external amp is active"
        );
    }

    /// Render the main screen with an (empty) practice timeline, for the given panel
    /// visibility, and return its glyphs.
    fn render_with_practice(panels: Panels, recording: bool) -> String {
        let params = Params::new();
        let levels = Levels::new();
        let practice = crate::practice::Practice::new();
        let ui = crate::ui::practice::PracticeUi::new();
        let mut term = Terminal::new(TestBackend::new(W, H)).expect("test backend");
        term.draw(|f| {
            draw(
                f,
                &params,
                &levels,
                &InputCalibration::default(),
                None,
                &board_all(false),
                recording,
                true,
                None,
                None,
                None,
                None,
                panels,
                ChainStage::Amp,
                Some((&practice, &ui)),
            );
        })
        .expect("draw");
        screen_text(&term)
    }

    /// The practice timeline pane renders its empty state (no track loaded) and is
    /// left out entirely when the panel is hidden with `3`.
    #[test]
    fn practice_pane_renders_and_hides() {
        let shown = render_with_practice(Panels::all_visible(), false);
        assert!(
            shown.contains("T I M E L I N E"),
            "practice pane missing when shown"
        );
        insta::assert_snapshot!("practice_pane", shown);

        let hidden = render_with_practice(
            Panels {
                timeline: false,
                ..Panels::all_visible()
            },
            false,
        );
        assert!(
            !hidden.contains("T I M E L I N E"),
            "practice pane still drawn while hidden"
        );
    }

    /// While recording, the practice transport shows the REC lamp (the header
    /// ON AIR row is gone); silent otherwise.
    #[test]
    fn practice_pane_shows_rec_lamp_while_recording() {
        let rec = render_with_practice(Panels::all_visible(), true);
        assert!(
            rec.contains("●REC") || rec.contains("○REC"),
            "no REC lamp while recording"
        );
        let idle = render_with_practice(Panels::all_visible(), false);
        assert!(
            !idle.contains("●REC") && !idle.contains("○REC"),
            "REC lamp shown while idle"
        );
    }

    /// The ribbon mirrors the chain order: moving COMP after the cab moves its
    /// ribbon stage after CAB too.
    #[test]
    fn ribbon_follows_chain_order() {
        use std::sync::atomic::Ordering::Relaxed;
        let params = Params::new();
        params.cmp_enabled.store(true, Relaxed);
        params.fz_enabled.store(true, Relaxed);
        let board = board_all(true);

        // The ribbon renders above every panel, so the first occurrence of each
        // label in the flattened screen is the ribbon's.
        let line = render_with(&params, &board, None, |_| {});
        let (comp, fuzz, amp, cab) = (
            line.find("COMP").expect("COMP in ribbon"),
            line.find("FUZZ").expect("FUZZ in ribbon"),
            line.find("AMP").expect("AMP in ribbon"),
            line.find("CAB").expect("CAB in ribbon"),
        );
        assert!(
            comp < fuzz && fuzz < amp && amp < cab,
            "default ribbon order wrong: COMP@{comp} FUZZ@{fuzz} AMP@{amp} CAB@{cab}"
        );

        // Move COMP last: the ribbon must follow.
        let mut v: Vec<u8> = ChainStage::default_order().into_iter().collect();
        v.retain(|&x| x != ChainStage::Comp as u8);
        v.push(ChainStage::Comp as u8);
        params.set_chain_order(&v.try_into().unwrap());
        let line = render_with(&params, &board, None, |_| {});
        let (comp, cab) = (
            line.find("COMP").expect("COMP in ribbon"),
            line.find("CAB").expect("CAB in ribbon"),
        );
        assert!(
            cab < comp,
            "ribbon did not follow COMP move: CAB@{cab} COMP@{comp}"
        );
    }
}
