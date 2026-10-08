use std::sync::atomic::Ordering::Relaxed;

use crate::dsp::{
    AmpModel, CHAIN_LEN, CabModel, ChainStage, Params, amp_precedes_cab,
    stereo_stages_follow_the_amp,
};

use super::config::{
    ADD_TILE, AMP_END, AMP_START, CHAIN_TILE, KNOBS, MIC_END, MIC_START, PEDALS, PRACTICE_TILE,
    Panels, pedal_of,
};

/// A knob is reachable only if it belongs to the amp (only the active model's
/// first `amp_count` controls) or to a pedal currently on the board.
fn knob_visible(knob: usize, board: &[bool], amp_count: usize) -> bool {
    if (AMP_START..AMP_END).contains(&knob) {
        return knob - AMP_START < amp_count;
    }
    match pedal_of(knob) {
        Some(p) => board[p],
        None => true,
    }
}

/// Panel owning a focus: 1 = live-order ribbon, 2 = amp/cab (selectors plus
/// amp/mic knobs), 3 = practice timeline, 4 = pedalboard (pedal knobs + ADD).
pub(super) fn panel_of(focus: Option<usize>) -> u8 {
    match focus {
        Some(i) if i == CHAIN_TILE => 1,
        Some(i) if i == PRACTICE_TILE => 3,
        Some(i) if i == ADD_TILE => 4,
        Some(i) if pedal_of(i).is_some() => 4,
        _ => 2,
    }
}

fn panel_visible(panels: &Panels, panel: u8) -> bool {
    match panel {
        1 => true, // the ribbon is always visible
        2 => panels.amp,
        3 => panels.timeline,
        _ => panels.rig,
    }
}

fn set_panel_visible(panels: &mut Panels, panel: u8, visible: bool) {
    match panel {
        2 => panels.amp = visible,
        3 => panels.timeline = visible,
        4 => panels.rig = visible,
        _ => {}
    }
}

/// Rendered chain stages as `(slot, stage)` pairs: on-board pedals in chain
/// order with the amp and cab stages at their slots. This is what the ribbon
/// draws, what the cursor steps through, and what moves swap.
pub(super) fn rendered_stages(order: &[u8; CHAIN_LEN], board: &[bool]) -> Vec<(usize, ChainStage)> {
    order
        .iter()
        .enumerate()
        .filter_map(|(slot, &raw)| {
            let stage = ChainStage::from_u8(raw)?;
            if matches!(stage, ChainStage::Amp | ChainStage::Cab) {
                return Some((slot, stage));
            }
            let pi = stage.pedal_index()?;
            board
                .get(pi)
                .copied()
                .unwrap_or(false)
                .then_some((slot, stage))
        })
        .collect()
}

/// Where `Tab` lands when entering a panel: the ribbon, the selectors, the
/// timeline, or the first on-board pedal in chain order (`+ADD` when the board
/// is empty).
pub(super) fn panel_entry(panel: u8, board: &[bool], order: &[u8; CHAIN_LEN]) -> Option<usize> {
    match panel {
        1 => Some(CHAIN_TILE),
        2 => Some(AMP_START),
        3 => Some(PRACTICE_TILE),
        _ => order
            .iter()
            .filter_map(|&raw| ChainStage::from_u8(raw))
            .find_map(|stage| {
                let pi = stage.pedal_index()?;
                board
                    .get(pi)
                    .copied()
                    .unwrap_or(false)
                    .then_some(PEDALS[pi].start)
            })
            .or(Some(ADD_TILE)),
    }
}

/// Number of top-level panels `Tab` can cycle (1 ribbon, 2 amp/cab, 3 timeline,
/// 4 pedalboard). Panel 1 is always visible; 2-4 are toggled with the number keys.
const PANEL_COUNT: usize = 4;

/// Last-focused cell per panel, so `Tab` back into a panel lands where you left
/// off instead of snapping to its first cell. Owned by the UI thread.
pub(super) struct PanelMemory {
    last: [Option<usize>; PANEL_COUNT + 1],
}

impl PanelMemory {
    pub(super) fn new() -> Self {
        Self {
            last: [None; PANEL_COUNT + 1],
        }
    }

    fn remember(&mut self, focus: Option<usize>) {
        if let Some(k) = focus {
            let p = panel_of(Some(k));
            if (1..=PANEL_COUNT as u8).contains(&p) {
                self.last[p as usize] = Some(k);
            }
        }
    }

    /// The remembered cell for `panel`, if it still belongs to that panel and is
    /// still reachable (its pedal may have left the board, or the amp model may
    /// expose fewer knobs than the one it was set on).
    fn recall(&self, panel: u8, board: &[bool], amp_count: usize) -> Option<usize> {
        self.last[panel as usize]
            .filter(|&k| panel_of(Some(k)) == panel && focus_reachable(k, board, amp_count))
    }
}

/// A focus target is reachable if it is a sentinel tile or a visible knob.
fn focus_reachable(k: usize, board: &[bool], amp_count: usize) -> bool {
    match k {
        CHAIN_TILE | PRACTICE_TILE | ADD_TILE => true,
        _ => knob_visible(k, board, amp_count),
    }
}

/// `Tab` / `Shift+Tab` cycle focus forward/backward through the **visible**
/// panels (1 ribbon, 2 amp/cab, 3 timeline, 4 pedalboard), wrapping and skipping
/// hidden ones. Each panel is revisited at its remembered cell, or its entry
/// when there is nothing to recall.
pub(super) fn cycle_panel(
    focus: Option<usize>,
    board: &[bool],
    panels: &Panels,
    order: &[u8; CHAIN_LEN],
    dir: i32,
    mem: &mut PanelMemory,
    amp_count: usize,
) -> Option<usize> {
    let visible: Vec<u8> = (1..=PANEL_COUNT as u8)
        .filter(|&p| panel_visible(panels, p))
        .collect();
    if visible.is_empty() {
        return focus;
    }
    let cur = panel_of(focus);
    let pos = visible.iter().position(|&p| p == cur).unwrap_or(0) as i32;
    let n = visible.len() as i32;
    let next = visible[(((pos + dir.signum()) % n + n) % n) as usize];
    mem.remember(focus);
    mem.recall(next, board, amp_count)
        .or_else(|| panel_entry(next, board, order))
}

/// First entry scanning panels 1→2→3→4, for focus repair after a panel hides.
fn first_visible_entry(panels: &Panels, board: &[bool], order: &[u8; CHAIN_LEN]) -> Option<usize> {
    for p in 1..=4 {
        if panel_visible(panels, p) {
            return panel_entry(p, board, order);
        }
    }
    Some(CHAIN_TILE)
}

/// Number-key tri-state for panels 1–4: a hidden panel is shown and focused; a
/// visible but unfocused panel is focused; the focused panel hides (except the
/// ribbon, which only focuses). Other keys leave everything untouched.
pub(super) fn press_number(
    n: u8,
    focus: Option<usize>,
    board: &[bool],
    panels: &Panels,
    order: &[u8; CHAIN_LEN],
) -> (Panels, Option<usize>) {
    if !(1..=4).contains(&n) {
        return (*panels, focus);
    }
    let mut panels = *panels;
    if !panel_visible(&panels, n) {
        set_panel_visible(&mut panels, n, true);
        return (panels, panel_entry(n, board, order));
    }
    if panel_of(focus) == n {
        if n == 1 {
            return (panels, focus);
        }
        set_panel_visible(&mut panels, n, false);
        return (panels, focus_after_hide(&panels, board, order, n));
    }
    (panels, panel_entry(n, board, order))
}

/// Where focus lands after hiding panel `hidden`: the practice timeline when it
/// is open (it is the main workspace), otherwise the first visible panel's
/// entry. Hiding the timeline itself is not a candidate for its own return.
fn focus_after_hide(
    panels: &Panels,
    board: &[bool],
    order: &[u8; CHAIN_LEN],
    hidden: u8,
) -> Option<usize> {
    if hidden != 3 && panels.timeline {
        return Some(PRACTICE_TILE);
    }
    first_visible_entry(panels, board, order)
}

// `Tab` / `Shift-Tab` cycle the visible panels (see `cycle_panel`).
// switched with the number keys, so the old global panel walk is gone.

/// The cells `←`/`→` walk within the focused panel, in order. Movement is one
/// flat walk that crosses sub-groups and wraps at the ends:
/// - panel 2: the active model's amp knobs, the master output, then cab/mic knobs;
/// - panel 4: every on-board pedal's knobs in chain order, then `+ ADD`.
///
/// Panels 1 and 3 own their arrows elsewhere (chain cursor, seek), so they return
/// no stops and the arrows leave their focus untouched.
fn panel_stops(
    focus: Option<usize>,
    board: &[bool],
    order: &[u8; CHAIN_LEN],
    amp_count: usize,
) -> Vec<usize> {
    match panel_of(focus) {
        2 => {
            let mut stops: Vec<usize> = (AMP_START..AMP_START + amp_count).collect();
            stops.extend(MIC_START..MIC_END);
            stops
        }
        4 => {
            let mut stops: Vec<usize> = Vec::new();
            for &(_, stage) in &rendered_stages(order, board) {
                if let Some(pi) = stage.pedal_index() {
                    stops.extend(PEDALS[pi].start..PEDALS[pi].end);
                }
            }
            stops.push(ADD_TILE);
            stops
        }
        _ => Vec::new(),
    }
}

/// `←`/`→` inside the focused panel: walk its cells continuously, wrapping at the
/// ends. A focus with no stops (the ribbon or the timeline, which own their
/// arrows) is left untouched.
pub(super) fn step_knob_in_panel(
    focus: Option<usize>,
    board: &[bool],
    order: &[u8; CHAIN_LEN],
    dir: i32,
    amp_count: usize,
) -> Option<usize> {
    let stops = panel_stops(focus, board, order, amp_count);
    match focus.and_then(|c| stops.iter().position(|&s| s == c)) {
        Some(pos) => {
            let n = stops.len() as i32;
            Some(stops[(((pos as i32 + dir.signum()) % n + n) % n) as usize])
        }
        // A stale focus re-anchors at the panel's first cell.
        None => stops.first().copied().or(focus),
    }
}

/// Keep `focus` on a visible panel after one hides: focus inside a hidden panel
/// jumps to the first visible panel's entry, everything else stays put.
pub(super) fn ensure_focus_visible(
    focus: Option<usize>,
    board: &[bool],
    panels: &Panels,
    order: &[u8; CHAIN_LEN],
) -> Option<usize> {
    if panel_visible(panels, panel_of(focus)) {
        focus
    } else {
        first_visible_entry(panels, board, order)
    }
}

/// Step the header selection (panel 1) one cell, wrapping across the rendered
/// chain stages and the rig master output that sits after them. Returns the new
/// stage cursor plus whether the master cell is now selected.
///
/// The master is a single extra cell, so it is adjacent to both ends of the wrap:
/// stepping off the first or last stage selects it, and stepping off it returns to
/// the opposite end of the chain.
pub(super) fn step_header(
    order: &[u8; CHAIN_LEN],
    board: &[bool],
    cursor: ChainStage,
    on_master: bool,
    dir: i32,
) -> (ChainStage, bool) {
    let stages: Vec<ChainStage> = rendered_stages(order, board)
        .into_iter()
        .map(|(_, s)| s)
        .collect();
    if stages.is_empty() {
        return (cursor, true);
    }
    if on_master {
        return if dir > 0 {
            (stages[0], false)
        } else {
            (stages[stages.len() - 1], false)
        };
    }
    match stages.iter().position(|&s| s == cursor) {
        Some(p) => {
            let np = p as i32 + dir.signum();
            if np < 0 || np >= stages.len() as i32 {
                (cursor, true) // stepped off either end -> the master cell
            } else {
                (stages[np as usize], false)
            }
        }
        // A stale cursor re-anchors at the first stage.
        None => (stages[0], false),
    }
}

/// Nudge the rig master output by `delta` (normalized), clamped to its range.
pub(super) fn nudge_master(params: &Params, delta: f32) {
    let v = params.master_output.load(Relaxed);
    params
        .master_output
        .store((v + delta).clamp(0.0, 1.0), Relaxed);
}

/// Move the ribbon cursor to the neighbouring rendered stage (wrapping around
/// the ends). A stale cursor (its pedal left the board) re-anchors at the
/// nearest end instead.
/// Move the cursor's stage one rendered slot earlier (`dir < 0`, `[`) or later
/// (`dir > 0`, `]`). The cursor follows its stage. Off-board stages hold their
/// slots silently; the ends refuse, and a move that would place the cab before
/// its amp is rejected. Returns true when something moved.
pub(super) fn move_selected_stage(
    params: &Params,
    board: &[bool],
    cursor: ChainStage,
    dir: i32,
) -> bool {
    let order = params.chain_slots();
    let rendered = rendered_stages(&order, board);
    let Some(pos) = rendered.iter().position(|&(_, s)| s == cursor) else {
        return false;
    };
    let other = pos as i32 + dir.signum();
    if other < 0 || other >= rendered.len() as i32 {
        return false;
    }
    let (a, _) = rendered[pos];
    let (b, _) = rendered[other as usize];
    let mut order = order;
    order.swap(a, b);
    // The cab must always follow its amp.
    if !amp_precedes_cab(&order) {
        return false;
    }
    // And nothing stereo may move ahead of it. The Amp folds the chain to mono,
    // so a rack pedal or cab placed in front of it is silently downmixed and the
    // whole rack loses its stereo image -- a much larger change than "move"
    // implies, and one the user gets no warning about. Refused rather than
    // toasted, matching the cab/amp refusal above: the move has no upside to
    // trade against the downside, so there is nothing to decide.
    if !stereo_stages_follow_the_amp(&order) {
        return false;
    }
    params.set_chain_order(&order);
    true
}

/// Bypass/un-bypass the cursor's pedal on the ribbon. The amp and cab stages and
/// off-board stages are a no-op. Returns true when a flag flipped.
pub(super) fn toggle_stage(params: &Params, board: &[bool], cursor: ChainStage) -> bool {
    let Some(pi) = cursor.pedal_index() else {
        return false;
    };
    if !board.get(pi).copied().unwrap_or(false) {
        return false;
    }
    toggle_pedal(params, PEDALS[pi].start);
    true
}

pub(super) fn nudge(params: &Params, idx: usize, delta: f32) {
    let atom = (KNOBS[idx].param)(params);
    let new = (atom.load(Relaxed) + delta).clamp(0.0, 1.0);
    atom.store(new, Relaxed);
}

/// Apply an amp-modal pick: a built-in index selects that model and returns to
/// built-in; the trailing index (present only when `au_loaded`) activates the
/// loaded AU instead. Out-of-range picks are ignored.
pub(super) fn select_amp(params: &Params, index: usize, au_loaded: bool) {
    if index < AmpModel::ALL.len() {
        params.amp_model.store(AmpModel::ALL[index] as u8, Relaxed);
        params.amp_external_active.store(false, Relaxed);
    } else if au_loaded && index == AmpModel::ALL.len() {
        params.amp_external_active.store(true, Relaxed);
    }
}

/// Apply a cab-modal pick: a built-in index selects that model and returns to
/// built-in (the IR stays loaded, so `X` can re-engage it); the trailing index
/// (present only when `ir_loaded`) activates the loaded IR instead.
pub(super) fn select_cab(params: &Params, index: usize, ir_loaded: bool) {
    if index < CabModel::ALL.len() {
        params.cab_model.store(CabModel::ALL[index] as u8, Relaxed);
        params.cab_external_active.store(false, Relaxed);
    } else if ir_loaded && index == CabModel::ALL.len() {
        params.cab_external_active.store(true, Relaxed);
    }
}

/// Modal list lengths: built-ins plus one external row when loaded.
pub(super) fn amp_choices(au_loaded: bool) -> usize {
    AmpModel::ALL.len() + usize::from(au_loaded)
}

/// Modal list lengths: built-ins plus one external row when loaded.
pub(super) fn cab_choices(ir_loaded: bool) -> usize {
    CabModel::ALL.len() + usize::from(ir_loaded)
}

/// Cursor the amp modal opens with: the external row while an AU is active,
/// else the current built-in model.
pub(super) fn init_amp_cursor(params: &Params) -> usize {
    if params.amp_external_active.load(Relaxed) && params.amp_external_loaded.load(Relaxed) {
        AmpModel::ALL.len()
    } else {
        let current = params.amp_model.load(Relaxed);
        AmpModel::ALL
            .iter()
            .position(|&m| m as u8 == current)
            .unwrap_or(0)
    }
}

/// Cursor the cab modal opens with: the external row while an IR is active,
/// else the current built-in model.
pub(super) fn init_cab_cursor(params: &Params) -> usize {
    if params.cab_external_active.load(Relaxed) && params.cab_external_loaded.load(Relaxed) {
        CabModel::ALL.len()
    } else {
        let current = params.cab_model.load(Relaxed);
        CabModel::ALL
            .iter()
            .position(|&m| m as u8 == current)
            .unwrap_or(0)
    }
}

pub(super) fn toggle_pedal(params: &Params, knob_idx: usize) {
    // Amp and mic sections have no on/off toggle, so `pedal_of` returns `None`.
    if let Some(p) = pedal_of(knob_idx) {
        let flag = (PEDALS[p].enabled)(params);
        flag.store(!flag.load(Relaxed), Relaxed);
    }
}

/// Put a pedal on the board and engage it (LED on).
pub(super) fn add_pedal(params: &Params, board: &mut [bool], pedal: usize) {
    board[pedal] = true;
    (PEDALS[pedal].enabled)(params).store(true, Relaxed);
}

/// Take a pedal off the board and bypass it in the DSP chain.
pub(super) fn remove_pedal(params: &Params, board: &mut [bool], pedal: usize) {
    board[pedal] = false;
    (PEDALS[pedal].enabled)(params).store(false, Relaxed);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsp::{AmpModel, CabModel};

    fn board(on: bool) -> Vec<bool> {
        vec![on; PEDALS.len()]
    }

    fn order() -> [u8; CHAIN_LEN] {
        ChainStage::default_order()
    }

    fn knob(params: &Params, idx: usize) -> f32 {
        (KNOBS[idx].param)(params).load(Relaxed)
    }

    /// Amp knob count the nav tests exercise (the default model, Mesa, exposes 6).
    const AMP_KNOBS: usize = 6;

    /// Test-local wrapper pinning the default model's amp-knob count.
    fn cycle_panel(
        focus: Option<usize>,
        board: &[bool],
        panels: &Panels,
        order: &[u8; CHAIN_LEN],
        dir: i32,
        mem: &mut PanelMemory,
    ) -> Option<usize> {
        super::cycle_panel(focus, board, panels, order, dir, mem, AMP_KNOBS)
    }

    fn step_knob_in_panel(
        focus: Option<usize>,
        board: &[bool],
        order: &[u8; CHAIN_LEN],
        dir: i32,
    ) -> Option<usize> {
        super::step_knob_in_panel(focus, board, order, dir, AMP_KNOBS)
    }

    // ── navigation ────────────────────────────────────────────────────────────

    /// `Tab` cycles through the visible panels and wraps at both ends.
    #[test]
    fn tab_cycles_visible_panels() {
        let b = board(true);
        let o = order();
        let all = Panels::all_visible();
        let mut mem = PanelMemory::new();
        assert_eq!(
            cycle_panel(Some(CHAIN_TILE), &b, &all, &o, 1, &mut mem),
            Some(AMP_START),
            "1 -> 2"
        );
        assert_eq!(
            cycle_panel(Some(AMP_START), &b, &all, &o, 1, &mut mem),
            Some(PRACTICE_TILE),
            "2 -> 3"
        );
        assert_eq!(
            cycle_panel(Some(PRACTICE_TILE), &b, &all, &o, 1, &mut mem),
            Some(PEDALS[0].start),
            "3 -> 4"
        );
        assert_eq!(
            cycle_panel(Some(PEDALS[0].start), &b, &all, &o, 1, &mut mem),
            Some(CHAIN_TILE),
            "4 -> 1 wraps"
        );
        assert_eq!(
            cycle_panel(Some(CHAIN_TILE), &b, &all, &o, -1, &mut mem),
            Some(PEDALS[0].start),
            "1 -> 4 backwards wraps"
        );
    }

    /// `Tab` skips hidden panels, and is inert when only the ribbon is left.
    #[test]
    fn tab_skips_hidden_panels() {
        let b = board(true);
        let o = order();
        let mut mem = PanelMemory::new();
        let no_timeline = Panels {
            amp: true,
            rig: true,
            timeline: false,
        };
        assert_eq!(
            cycle_panel(Some(AMP_START), &b, &no_timeline, &o, 1, &mut mem),
            Some(PEDALS[0].start),
            "2 -> 4 when the timeline is hidden"
        );
        let ribbon_only = Panels {
            amp: false,
            rig: false,
            timeline: false,
        };
        assert_eq!(
            cycle_panel(Some(CHAIN_TILE), &b, &ribbon_only, &o, 1, &mut mem),
            Some(CHAIN_TILE),
            "with only the ribbon visible Tab stays put"
        );
    }

    /// A full lap of the panels returns to the cell each was left on.
    #[test]
    fn tab_remembers_the_cell_per_panel() {
        let b = board(true);
        let o = order();
        let all = Panels::all_visible();
        let mut mem = PanelMemory::new();
        let on_amp = AMP_START + 2;
        let mut f = Some(on_amp);
        for _ in 0..4 {
            f = cycle_panel(f, &b, &all, &o, 1, &mut mem);
        }
        assert_eq!(f, Some(on_amp), "a full lap must recall each panel's knob");
    }

    /// A remembered cell that is no longer reachable is not recalled.
    #[test]
    fn panel_memory_recall_rejects_stale_cells() {
        let mut b = board(true);
        let mut mem = PanelMemory::new();
        mem.remember(Some(PEDALS[0].start));
        assert_eq!(mem.recall(4, &b, AMP_KNOBS), Some(PEDALS[0].start));
        b[0] = false; // the gate leaves the board
        assert_eq!(
            mem.recall(4, &b, AMP_KNOBS),
            None,
            "a stale pedal must not be recalled"
        );

        // A knob past the active model's control count is also rejected.
        let mut mem = PanelMemory::new();
        mem.remember(Some(AMP_START + 5));
        assert_eq!(mem.recall(2, &b, 3), None);
        assert_eq!(mem.recall(2, &b, AMP_KNOBS), Some(AMP_START + 5));
    }

    /// The header master cell adjusts the rig output and clamps at both ends.
    #[test]
    fn nudge_master_moves_and_clamps() {
        let p = Params::new();
        p.master_output
            .store(crate::dsp::DEFAULT_MASTER_OUTPUT, Relaxed);
        nudge_master(&p, 0.1);
        assert!((p.master_output.load(Relaxed) - 0.6).abs() < 1e-6);
        nudge_master(&p, -0.5);
        assert!((p.master_output.load(Relaxed) - 0.1).abs() < 1e-6);
        // Clamps rather than wrapping or overshooting.
        for _ in 0..40 {
            nudge_master(&p, 0.1);
        }
        assert_eq!(p.master_output.load(Relaxed), 1.0);
        for _ in 0..40 {
            nudge_master(&p, -0.1);
        }
        assert_eq!(p.master_output.load(Relaxed), 0.0);
    }

    #[test]
    fn panel_entry_lands_on_first_onboard_pedal_in_chain_order() {
        let mut b = board(false);
        b[3] = true; // only COMP is on the board
        // Reversed chain: entry must still find COMP (order-independent lookup).
        let mut rev: Vec<u8> = ChainStage::default_order().into_iter().collect();
        rev.reverse();
        let rev: [u8; CHAIN_LEN] = rev.try_into().unwrap();
        assert_eq!(
            panel_entry(4, &b, &rev),
            Some(PEDALS[3].start),
            "panel 4 must enter on the only on-board pedal"
        );
        assert_eq!(
            panel_entry(4, &board(false), &rev),
            Some(ADD_TILE),
            "empty board enters at +ADD"
        );
    }

    /// Panel 2 `←`/`→` walks the amp knobs and the cab/mic knobs as one
    /// continuous, wrapping run.
    #[test]
    fn arrows_walk_the_amp_panel_continuously() {
        let b = board(true);
        let o = order();
        assert_eq!(
            step_knob_in_panel(Some(AMP_START), &b, &o, 1),
            Some(AMP_START + 1)
        );
        // The last amp knob leads straight into the cab/mic block.
        assert_eq!(
            step_knob_in_panel(Some(AMP_START + AMP_KNOBS - 1), &b, &o, 1),
            Some(MIC_START),
            "the last amp knob must lead into the cab/mic knobs"
        );
        // And the mic block wraps back to the first amp knob.
        assert_eq!(
            step_knob_in_panel(Some(MIC_END - 1), &b, &o, 1),
            Some(AMP_START),
            "the last mic knob must wrap to the first amp knob"
        );
        // Backwards crosses the same boundary.
        assert_eq!(
            step_knob_in_panel(Some(MIC_START), &b, &o, -1),
            Some(AMP_START + AMP_KNOBS - 1)
        );
        assert_eq!(
            step_knob_in_panel(Some(AMP_START), &b, &o, -1),
            Some(MIC_END - 1)
        );
        // A stale focus re-anchors at the panel's first cell.
        assert_eq!(step_knob_in_panel(None, &b, &o, 1), Some(AMP_START));
    }

    /// Panel 4 `←`/`→` walks every on-board pedal's knobs in chain order, crosses
    /// pedal boundaries, visits `+ ADD`, and wraps.
    #[test]
    fn arrows_walk_every_pedal_continuously() {
        let b = board(true);
        let o = order();
        let gate = PEDALS[0].start;
        assert_eq!(step_knob_in_panel(Some(gate), &b, &o, 1), Some(gate + 1));
        // The last knob of the first on-board pedal leads into the next pedal.
        assert_eq!(
            step_knob_in_panel(Some(PEDALS[0].end - 1), &b, &o, 1),
            Some(PEDALS[1].start),
            "the walk must cross from one pedal into the next"
        );
        // The last pedal leads to +ADD, which wraps to the first pedal.
        let last_end = PEDALS[PEDALS.len() - 1].end;
        assert_eq!(
            step_knob_in_panel(Some(last_end - 1), &b, &o, 1),
            Some(ADD_TILE)
        );
        assert_eq!(step_knob_in_panel(Some(ADD_TILE), &b, &o, 1), Some(gate));
        assert_eq!(step_knob_in_panel(Some(gate), &b, &o, -1), Some(ADD_TILE));
        // Ribbon and timeline own their arrows.
        assert_eq!(
            step_knob_in_panel(Some(CHAIN_TILE), &b, &o, 1),
            Some(CHAIN_TILE)
        );
        assert_eq!(
            step_knob_in_panel(Some(PRACTICE_TILE), &b, &o, -1),
            Some(PRACTICE_TILE)
        );
    }

    /// The walk is confined to the focused panel: an off-board pedal is skipped,
    /// and panel 2 never leaks into the pedalboard.
    #[test]
    fn arrows_stay_inside_the_focused_panel() {
        let mut b = board(false);
        b[3] = true; // only COMP is on the board
        let o = order();
        // An off-board pedal's focus re-anchors on the only on-board pedal.
        assert_eq!(
            step_knob_in_panel(Some(PEDALS[0].start), &b, &o, 1),
            Some(PEDALS[3].start),
            "an off-board pedal's arrows must re-anchor on the board"
        );
        // Panel 2 loops within amp + mic.
        for k in AMP_START..MIC_END {
            let f = step_knob_in_panel(Some(k), &b, &o, 1);
            assert!(
                f.is_some_and(|x| (AMP_START..MIC_END).contains(&x)),
                "amp/mic knob {k} leaked out of panel 2: {f:?}"
            );
        }
    }

    #[test]
    fn number_keys_focus_first_show_second_hide_third() {
        let b = board(true);
        let o = order();
        let p = Panels::all_visible();
        // Visible but unfocused panel 4 → focuses its entry (first pedal).
        let (p2, f) = press_number(4, None, &b, &p, &o);
        assert!(p2.rig);
        assert_eq!(f, Some(PEDALS[0].start));
        // Focused panel 4 → hides it; the open timeline takes focus (not the
        // ribbon).
        let (p3, f) = press_number(4, f, &b, &p2, &o);
        assert!(!p3.rig);
        assert_eq!(f, Some(PRACTICE_TILE));
        // Hidden panel 4 → shows and focuses it again.
        let (p4, f) = press_number(4, f, &b, &p3, &o);
        assert!(p4.rig);
        assert_eq!(f, Some(PEDALS[0].start));
        // Panel 1 only focuses, never hides.
        let (p5, f) = press_number(1, f, &b, &p4, &o);
        assert_eq!(f, Some(CHAIN_TILE));
        assert!(p5.rig && p5.amp && p5.timeline);
        // Out-of-range keys are ignored.
        let (p6, f) = press_number(9, f, &b, &p5, &o);
        assert!(p6.rig && p6.amp && p6.timeline);
        assert_eq!(f, Some(CHAIN_TILE));
    }

    /// Without an open timeline, closing a panel keeps the old behavior (ribbon);
    /// closing the timeline itself also lands on the ribbon.
    #[test]
    fn closing_a_panel_falls_back_to_the_ribbon_without_a_timeline() {
        let b = board(true);
        let o = order();
        let no_timeline = Panels {
            timeline: false,
            ..Panels::all_visible()
        };
        let (panels, f) = press_number(4, Some(PEDALS[0].start), &b, &no_timeline, &o);
        assert!(!panels.rig);
        assert_eq!(f, Some(CHAIN_TILE), "no timeline → ribbon");

        let (panels, f) = press_number(3, Some(PRACTICE_TILE), &b, &Panels::all_visible(), &o);
        assert!(!panels.timeline);
        assert_eq!(f, Some(CHAIN_TILE), "closing the timeline → ribbon");
    }

    #[test]
    fn hiding_the_focused_panel_repairs_focus() {
        let b = board(true);
        let o = order();
        // Focus on a pedal knob while the board hides → ribbon.
        let p = Panels {
            rig: false,
            ..Panels::all_visible()
        };
        assert_eq!(
            ensure_focus_visible(Some(PEDALS[0].start), &b, &p, &o),
            Some(CHAIN_TILE)
        );
        // Visible panels keep focus untouched, even mid-panel.
        assert_eq!(
            ensure_focus_visible(Some(PEDALS[0].start + 1), &b, &Panels::all_visible(), &o),
            Some(PEDALS[0].start + 1)
        );
    }

    // ── knob edits ──────────────────────────────────────────────────────────────

    #[test]
    fn nudge_clamps_to_the_unit_range() {
        let p = Params::new();
        nudge(&p, 0, 5.0);
        assert_eq!(knob(&p, 0), 1.0, "nudge above 1.0 must clamp");
        nudge(&p, 0, -5.0);
        assert_eq!(knob(&p, 0), 0.0, "nudge below 0.0 must clamp");
    }

    #[test]
    fn nudge_moves_only_the_targeted_knob() {
        let p = Params::new();
        // Park the target mid-scale before sampling. The test used to assume the
        // default amp model, but the models have different knob defaults and
        // count — the Plexi's BASS (slot 2) sits at 1.0, so a +0.05 nudge clamped
        // and the test measured the clamp, not the nudge.
        (KNOBS[2].param)(&p).store(0.5, Relaxed);
        let before: Vec<f32> = (0..KNOBS.len()).map(|k| knob(&p, k)).collect();
        nudge(&p, 2, 0.05);
        for (k, knob_before) in before.iter().enumerate().take(KNOBS.len()) {
            if k == 2 {
                assert!((knob(&p, k) - (knob_before + 0.05)).abs() < 1e-6);
            } else {
                assert_eq!(knob(&p, k), *knob_before, "knob {k} moved unexpectedly");
            }
        }
    }

    // ── amp / cab modal picks ─────────────────────────────────────────────────

    #[test]
    fn select_amp_stores_builtin_and_returns_to_builtin() {
        let p = Params::new();
        p.amp_external_active.store(true, Relaxed);
        p.amp_external_loaded.store(true, Relaxed);
        select_amp(&p, 3, true); // Vox
        assert_eq!(AmpModel::from_u8(p.amp_model.load(Relaxed)), AmpModel::Vox);
        assert!(
            !p.amp_external_active.load(Relaxed),
            "picking a built-in must return to the built-in amp"
        );
    }

    #[test]
    fn select_amp_external_row_activates_the_loaded_au() {
        let p = Params::new();
        p.amp_external_loaded.store(true, Relaxed);
        select_amp(&p, AmpModel::ALL.len(), true);
        assert!(p.amp_external_active.load(Relaxed));
        // Without a loaded AU the trailing index is a no-op.
        let q = Params::new();
        select_amp(&q, AmpModel::ALL.len(), false);
        assert!(!q.amp_external_active.load(Relaxed));
        // Garbage indices never touch anything. Assert against whatever model
        // was selected above, not the boot default, so the test does not encode
        // which amp the app starts on.
        select_amp(&q, 3, true);
        let picked = AmpModel::from_u8(q.amp_model.load(Relaxed));
        select_amp(&q, 99, true);
        assert_eq!(
            AmpModel::from_u8(q.amp_model.load(Relaxed)),
            picked,
            "a garbage pick must not change the model"
        );
    }

    #[test]
    fn select_cab_stores_builtin_and_returns_to_builtin() {
        let p = Params::new();
        p.cab_external_active.store(true, Relaxed);
        p.cab_external_loaded.store(true, Relaxed);
        select_cab(&p, 2, true); // Orange
        assert_eq!(
            CabModel::from_u8(p.cab_model.load(Relaxed)),
            CabModel::Orange
        );
        assert!(
            !p.cab_external_active.load(Relaxed),
            "picking a built-in must return to the built-in cab"
        );
    }

    #[test]
    fn select_cab_external_row_activates_the_loaded_ir() {
        let p = Params::new();
        p.cab_external_loaded.store(true, Relaxed);
        select_cab(&p, CabModel::ALL.len(), true);
        assert!(p.cab_external_active.load(Relaxed));
        let q = Params::new();
        select_cab(&q, CabModel::ALL.len(), false);
        assert!(!q.cab_external_active.load(Relaxed));
        select_cab(&q, 99, true);
        assert!(!q.cab_external_active.load(Relaxed));
    }

    #[test]
    fn modal_cursors_preselect_the_current_pick() {
        let p = Params::new();
        p.amp_model.store(AmpModel::Vox as u8, Relaxed);
        p.cab_model.store(CabModel::Wem as u8, Relaxed);
        assert_eq!(init_amp_cursor(&p), 3);
        assert_eq!(init_cab_cursor(&p), 3);
        assert_eq!(amp_choices(false), 9);
        assert_eq!(amp_choices(true), 10);
        assert_eq!(cab_choices(false), 8);
        assert_eq!(cab_choices(true), 9);
        // Active externals point at their trailing rows.
        p.amp_external_active.store(true, Relaxed);
        p.amp_external_loaded.store(true, Relaxed);
        p.cab_external_active.store(true, Relaxed);
        p.cab_external_loaded.store(true, Relaxed);
        assert_eq!(init_amp_cursor(&p), 9);
        assert_eq!(init_cab_cursor(&p), 8);
    }

    // ── board membership & toggles ──────────────────────────────────────────────

    #[test]
    fn add_then_remove_pedal_round_trips_board_and_enabled_flag() {
        let p = Params::new();
        let mut b = board(false);
        // Force the flag off first so the default-on pedals don't mask the test.
        (PEDALS[5].enabled)(&p).store(false, Relaxed);

        add_pedal(&p, &mut b, 5);
        assert!(b[5], "add_pedal must put the pedal on the board");
        assert!(
            (PEDALS[5].enabled)(&p).load(Relaxed),
            "add_pedal must engage the LED"
        );

        remove_pedal(&p, &mut b, 5);
        assert!(!b[5], "remove_pedal must take it off the board");
        assert!(
            !(PEDALS[5].enabled)(&p).load(Relaxed),
            "remove_pedal must bypass it"
        );
    }

    #[test]
    fn toggle_pedal_flips_the_enabled_flag_for_a_pedal_knob() {
        let p = Params::new();
        let flag = (PEDALS[0].enabled)(&p);
        let before = flag.load(Relaxed);
        toggle_pedal(&p, PEDALS[0].start);
        assert_eq!(flag.load(Relaxed), !before);
    }

    #[test]
    fn toggle_pedal_is_a_noop_for_amp_and_mic_knobs() {
        let p = Params::new();
        // Amp/mic knobs own no enable flag; toggling must not panic or flip anything.
        let flags_before: Vec<bool> = PEDALS
            .iter()
            .map(|pd| (pd.enabled)(&p).load(Relaxed))
            .collect();
        toggle_pedal(&p, AMP_START);
        toggle_pedal(&p, MIC_START);
        let flags_after: Vec<bool> = PEDALS
            .iter()
            .map(|pd| (pd.enabled)(&p).load(Relaxed))
            .collect();
        assert_eq!(flags_before, flags_after);
    }

    // ── ribbon cursor moves ───────────────────────────────────────────────────

    /// `←`/`→` in panel 1 walk the rendered stages and then the master cell,
    /// wrapping; stepping off either end of the chain selects the master.
    #[test]
    fn header_cursor_walks_stages_then_master_and_wraps() {
        let b = board(true);
        let o = order();
        // Full board: every slot renders, so the cursor walks raw order.
        assert_eq!(
            step_header(&o, &b, ChainStage::Gate, false, 1),
            (ChainStage::Whammy, false)
        );
        // Stepping back from the first stage selects the master, not the last stage.
        assert_eq!(
            step_header(&o, &b, ChainStage::Gate, false, -1),
            (ChainStage::Gate, true),
            "stepping off the front selects the master cell"
        );
        // From the master, → goes to the first stage and ← to the last.
        assert_eq!(
            step_header(&o, &b, ChainStage::Gate, true, 1),
            (ChainStage::Gate, false)
        );
        assert_eq!(
            step_header(&o, &b, ChainStage::Gate, true, -1),
            (ChainStage::Reverb, false)
        );
        // The last stage steps onto the master.
        assert_eq!(
            step_header(&o, &b, ChainStage::Reverb, false, 1),
            (ChainStage::Reverb, true)
        );

        // Sparse board: rendered is COMP (slot 3), AMP (10), CAB (11).
        let mut sparse = board(false);
        sparse[3] = true;
        assert_eq!(
            step_header(&o, &sparse, ChainStage::Amp, false, 1),
            (ChainStage::Cab, false)
        );
        assert_eq!(
            step_header(&o, &sparse, ChainStage::Cab, false, 1),
            (ChainStage::Cab, true),
            "the last rendered stage leads to the master"
        );
        assert_eq!(
            step_header(&o, &sparse, ChainStage::Cab, true, 1),
            (ChainStage::Comp, false),
            "the master wraps to the first rendered stage"
        );
        // Stale cursor (pedal left the board) re-anchors at the first stage.
        assert_eq!(
            step_header(&o, &sparse, ChainStage::Fuzz, false, 1),
            (ChainStage::Comp, false)
        );
    }

    #[test]
    fn move_selected_stage_swaps_with_rendered_neighbour() {
        let p = Params::new();
        let b = board(true);
        // COMP is at slot 3 with FUZZ right after it.
        assert!(move_selected_stage(&p, &b, ChainStage::Comp, 1));
        let order = p.chain_slots();
        assert_eq!(order[3], ChainStage::Fuzz as u8);
        assert_eq!(order[4], ChainStage::Comp as u8);
        // …and back again.
        assert!(move_selected_stage(&p, &b, ChainStage::Comp, -1));
        assert_eq!(p.chain_slots(), ChainStage::default_order());
    }

    /// Moving a stereo stage in front of the amp must be refused.
    ///
    /// The Amp folds the chain to mono (`Sig::into_mono`), so a rack pedal or the
    /// cab placed ahead of it is silently downmixed and the whole rack loses its
    /// stereo image — with no warning and nothing on screen to explain the change
    /// afterwards. The move has no upside (a pre-amp "stereo" pedal is exactly as
    /// mono as a mono pedal), so it is refused outright.
    #[test]
    fn move_selected_stage_refuses_to_leap_a_stereo_stage_before_the_amp() {
        // Walk the reverb leftwards one slot at a time. Every step up to the amp
        // is refused; the ones before it are not, which is what makes this a test
        // of the boundary rather than of a blanket "no moves".
        let p = Params::new();
        let b = board(true);

        let start = p.chain_slots();
        let amp_at = start
            .iter()
            .position(|&v| v == ChainStage::Amp as u8)
            .expect("amp present");
        assert!(
            start[amp_at..].contains(&(ChainStage::Reverb as u8)),
            "the reverb should start after the amp"
        );

        // The reverb starts last in the ribbon, so a handful of leftward moves
        // walks it back toward the amp. It must stop *at* the amp, not at the
        // start of the ribbon -- asserting only that some move was refused would
        // pass for the wrong reason if it had simply hit the left-hand end.
        for _ in 0..amp_at + 2 {
            if !move_selected_stage(&p, &b, ChainStage::Reverb, -1) {
                break;
            }
        }
        let order = p.chain_slots();
        let rev_at = order
            .iter()
            .position(|&v| v == ChainStage::Reverb as u8)
            .expect("reverb present");
        assert_eq!(
            rev_at,
            amp_at + 1,
            "the reverb should come to rest immediately after the amp, not \
             earlier: {order:?}"
        );
        assert!(
            stereo_stages_follow_the_amp(&order),
            "a stereo stage ended up ahead of the amp: {order:?}"
        );
        // The refusal must be inert: the chain is still a complete permutation,
        // and the cab still follows the amp.
        let mut sorted = order;
        sorted.sort_unstable();
        assert_eq!(sorted.as_slice(), (0..CHAIN_LEN as u8).collect::<Vec<_>>());
        assert!(amp_precedes_cab(&order));
    }

    /// The mirror image, which must stay allowed: a **mono** pedal moving ahead of
    /// the amp is the entire point of the pre-amp section and costs nothing.
    #[test]
    fn move_selected_stage_still_allows_mono_pedals_before_the_amp() {
        let p = Params::new();
        let b = board(true);
        // Fuzz is a mono pedal well ahead of the amp by default; nudge it one
        // rendered slot earlier and check it actually went there, so the test is
        // not passing on a refused move.
        let before = p
            .chain_slots()
            .iter()
            .position(|&v| v == ChainStage::Fuzz as u8)
            .expect("fuzz present");
        assert!(move_selected_stage(&p, &b, ChainStage::Fuzz, -1));

        let order = p.chain_slots();
        let after = order
            .iter()
            .position(|&v| v == ChainStage::Fuzz as u8)
            .expect("fuzz present");
        assert_eq!(
            after,
            before - 1,
            "the fuzz did not move earlier: {order:?}"
        );
        assert!(
            stereo_stages_follow_the_amp(&order),
            "a mono pedal should still be allowed ahead of the amp: {order:?}"
        );
    }

    #[test]
    fn move_selected_stage_moves_amp_and_cab_separately() {
        let p = Params::new();
        let b = board(true);
        // AMP sits at slot 11 with BOOST before it. Move it earlier — legal, since
        // its cab (slot 12) still follows it.
        assert!(move_selected_stage(&p, &b, ChainStage::Amp, -1));
        let order = p.chain_slots();
        assert_eq!(order[10], ChainStage::Amp as u8);
        assert_eq!(order[11], ChainStage::Boost as u8);
        assert!(move_selected_stage(&p, &b, ChainStage::Amp, 1));
        assert_eq!(p.chain_slots(), ChainStage::default_order());

        // Moving the CAB forward past its AMP must be refused.
        assert!(!move_selected_stage(&p, &b, ChainStage::Cab, -1));
        assert_eq!(p.chain_slots(), ChainStage::default_order());

        // Moving the CAB later (past GEQ) is legal.
        assert!(move_selected_stage(&p, &b, ChainStage::Cab, 1));
        let order = p.chain_slots();
        assert_eq!(order[12], ChainStage::Geq as u8);
        assert_eq!(order[13], ChainStage::Cab as u8);
        assert!(amp_precedes_cab(&order));
    }

    #[test]
    fn move_selected_stage_refuses_the_ends_and_stale_cursors() {
        let p = Params::new();
        let b = board(true);
        // GATE is first: nothing earlier. REVERB is last: nothing later.
        assert!(!move_selected_stage(&p, &b, ChainStage::Gate, -1));
        assert!(!move_selected_stage(&p, &b, ChainStage::Reverb, 1));
        assert_eq!(p.chain_slots(), ChainStage::default_order());
        // Stale cursor (off-board pedal) cannot move.
        assert!(!move_selected_stage(&p, &board(false), ChainStage::Fuzz, 1));
    }

    #[test]
    fn toggle_stage_flips_onboard_pedals_only() {
        let p = Params::new();
        let mut b = board(true);
        let flag = (PEDALS[3].enabled)(&p);
        let before = flag.load(Relaxed);
        assert!(toggle_stage(&p, &b, ChainStage::Comp));
        assert_eq!(flag.load(Relaxed), !before);
        // The amp and cab are no-ops, and so is an off-board pedal.
        assert!(!toggle_stage(&p, &b, ChainStage::Amp));
        assert!(!toggle_stage(&p, &b, ChainStage::Cab));
        b[3] = false;
        assert!(!toggle_stage(&p, &b, ChainStage::Comp));
        assert_eq!(flag.load(Relaxed), !before);
    }
}
