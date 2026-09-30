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

/// Group ids for [`NavMemory`]: the amp knob block, the cab/mic knob block, and
/// one group per `PEDALS` entry (`2 + pi`).
const GROUP_AMP: usize = 0;
const GROUP_MIC: usize = 1;
const GROUP_COUNT: usize = 2 + PEDALS.len();

fn group_pedal(pi: usize) -> usize {
    2 + pi
}

/// Last-focused knob per group, so `Tab` back into a section lands where you
/// left off instead of snapping to its first knob. Owned by the UI thread.
pub(super) struct NavMemory {
    last: [Option<usize>; GROUP_COUNT],
}

impl NavMemory {
    pub(super) fn new() -> Self {
        Self {
            last: [None; GROUP_COUNT],
        }
    }

    /// The group a knob belongs to (amp / mic / pedal), or `None` for the
    /// sentinel focuses.
    fn group_of(focus: Option<usize>) -> Option<usize> {
        let k = focus?;
        if (AMP_START..AMP_END).contains(&k) {
            Some(GROUP_AMP)
        } else if (MIC_START..MIC_END).contains(&k) {
            Some(GROUP_MIC)
        } else {
            pedal_of(k).map(group_pedal)
        }
    }

    fn remember(&mut self, focus: Option<usize>) {
        if let Some(group) = Self::group_of(focus) {
            self.last[group] = focus;
        }
    }

    /// The remembered knob for `group`, if it is still reachable (its pedal may
    /// have left the board, or the amp model may expose fewer knobs), else `None`.
    fn recall(&self, group: usize, board: &[bool], amp_count: usize) -> Option<usize> {
        self.last[group].filter(|&k| knob_visible(k, board, amp_count))
    }
}

/// First knob of a group, used when there is nothing to recall.
fn group_first(group: usize) -> Option<usize> {
    match group {
        GROUP_AMP => Some(AMP_START),
        GROUP_MIC => Some(MIC_START),
        g => PEDALS.get(g.checked_sub(2)?).map(|p| p.start),
    }
}

/// `Tab` / `Shift-Tab` inside the focused panel:
/// - panel 1 (ribbon) and panel 3 (timeline): inert;
/// - panel 2: toggle between the amp and cab/mic knob groups;
/// - panel 4: step to the next/previous on-board pedal in chain order, ending
///   on the `+ ADD` tile and wrapping.
///
/// The current group's knob is remembered first, so returning to a section
/// lands on the knob you last touched there.
pub(super) fn tab_in_panel(
    focus: Option<usize>,
    board: &[bool],
    order: &[u8; CHAIN_LEN],
    dir: i32,
    mem: &mut NavMemory,
    amp_count: usize,
) -> Option<usize> {
    match panel_of(focus) {
        1 | 3 => focus,
        2 => {
            mem.remember(focus);
            let current = NavMemory::group_of(focus).unwrap_or(GROUP_AMP);
            let target = if current == GROUP_AMP {
                GROUP_MIC
            } else {
                GROUP_AMP
            };
            mem.recall(target, board, amp_count)
                .or_else(|| group_first(target))
        }
        _ => {
            mem.remember(focus);
            // On-board pedals in chain order, then the +ADD tile.
            let pedals: Vec<usize> = order
                .iter()
                .filter_map(|&raw| ChainStage::from_u8(raw))
                .filter_map(|stage| stage.pedal_index())
                .filter(|&pi| board.get(pi).copied().unwrap_or(false))
                .collect();
            let cur = match focus {
                Some(ADD_TILE) => Some(pedals.len()),
                Some(k) => pedal_of(k).and_then(|pi| pedals.iter().position(|&p| p == pi)),
                None => None,
            };
            let next = match cur {
                Some(pos) => {
                    let n = (pedals.len() + 1) as i32;
                    (((pos as i32 + dir.signum()) % n + n) % n) as usize
                }
                None if dir < 0 => pedals.len(),
                None => 0,
            };
            match pedals.get(next) {
                Some(&pi) => mem
                    .recall(group_pedal(pi), board, amp_count)
                    .or_else(|| group_first(group_pedal(pi))),
                None => Some(ADD_TILE),
            }
        }
    }
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

// `Tab` / `Shift-Tab` are panel-local now (see `tab_in_panel`); panels are
// switched with the number keys, so the old global panel walk is gone.

/// Knob range for the section a focus owns scoped `←`/`→` navigation in:
/// panel 2 the amp or mic group containing the focus, panel 4 the focused
/// pedal's own knobs. `None` (no section owns the arrows) leaves focus alone.
fn scoped_knob_range(focus: Option<usize>, amp_count: usize) -> Option<(usize, usize)> {
    match panel_of(focus) {
        2 => {
            if focus.is_some_and(|k| (MIC_START..MIC_END).contains(&k)) {
                Some((MIC_START, MIC_END))
            } else {
                Some((AMP_START, AMP_START + amp_count))
            }
        }
        4 => {
            let pi = focus.and_then(pedal_of)?;
            Some((PEDALS[pi].start, PEDALS[pi].end))
        }
        _ => None,
    }
}

/// `←`/`→` inside the focused panel: cycle knobs within the focused amp/mic
/// section, or within the focused pedal, wrapping around that section only. The
/// ribbon and timeline own their arrows; a focus with no section (the `+ ADD`
/// tile, a stale focus) is left untouched.
pub(super) fn step_knob_in_panel(
    focus: Option<usize>,
    board: &[bool],
    _order: &[u8; CHAIN_LEN],
    dir: i32,
    amp_count: usize,
) -> Option<usize> {
    let Some((start, end)) = scoped_knob_range(focus, amp_count) else {
        return focus;
    };
    let stops: Vec<usize> = (start..end)
        .filter(|&k| knob_visible(k, board, amp_count))
        .collect();
    match focus.and_then(|c| stops.iter().position(|&s| s == c)) {
        Some(pos) => {
            let n = stops.len() as i32;
            stops
                .get((((pos as i32 + dir) % n + n) % n) as usize)
                .copied()
        }
        // A stale focus re-anchors at the section's first reachable knob, and
        // stays put if the whole section is unreachable.
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

/// Move the ribbon cursor to the neighbouring rendered stage (wrapping around
/// the ends). A stale cursor (its pedal left the board) re-anchors at the
/// nearest end instead.
pub(super) fn move_chain_cursor(
    order: &[u8; CHAIN_LEN],
    board: &[bool],
    cursor: ChainStage,
    dir: i32,
) -> ChainStage {
    let rendered = rendered_stages(order, board);
    if rendered.is_empty() {
        return cursor;
    }
    if let Some(pos) = rendered.iter().position(|&(_, s)| s == cursor) {
        let n = rendered.len() as i32;
        rendered[(((pos as i32 + dir) % n + n) % n) as usize].1
    } else if dir < 0 {
        rendered.last().map(|&(_, s)| s).unwrap_or(cursor)
    } else {
        rendered.first().map(|&(_, s)| s).unwrap_or(cursor)
    }
}

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

    /// Test-local wrappers pin the default model's amp-knob count so the
    /// navigation tests keep reading without threading it through every call.
    fn tab_in_panel(
        focus: Option<usize>,
        board: &[bool],
        order: &[u8; CHAIN_LEN],
        dir: i32,
        mem: &mut NavMemory,
    ) -> Option<usize> {
        super::tab_in_panel(focus, board, order, dir, mem, AMP_KNOBS)
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

    /// `Tab` is inert on the ribbon (panel 1) and the timeline (panel 3).
    #[test]
    fn tab_is_inert_on_ribbon_and_timeline() {
        let b = board(true);
        let o = order();
        let mut mem = NavMemory::new();
        assert_eq!(
            tab_in_panel(Some(CHAIN_TILE), &b, &o, 1, &mut mem),
            Some(CHAIN_TILE)
        );
        assert_eq!(
            tab_in_panel(Some(CHAIN_TILE), &b, &o, -1, &mut mem),
            Some(CHAIN_TILE)
        );
        assert_eq!(
            tab_in_panel(Some(PRACTICE_TILE), &b, &o, 1, &mut mem),
            Some(PRACTICE_TILE)
        );
    }

    /// Panel 2: `Tab` toggles amp ↔ cab, and each section remembers the knob it
    /// was last left on.
    #[test]
    fn tab_toggles_amp_and_cab_and_remembers_knobs() {
        let b = board(true);
        let o = order();
        let mut mem = NavMemory::new();
        // From an amp knob, first Tab enters the cab group at its first knob.
        let cab_first = tab_in_panel(Some(AMP_START + 2), &b, &o, 1, &mut mem).unwrap();
        assert_eq!(cab_first, MIC_START);
        // Move within the cab, then Tab back: the amp knob we left is restored.
        let cab_moved = step_knob_in_panel(Some(cab_first), &b, &o, 1).unwrap();
        assert_eq!(cab_moved, MIC_START + 1);
        let back = tab_in_panel(Some(cab_moved), &b, &o, 1, &mut mem).unwrap();
        assert_eq!(back, AMP_START + 2, "amp section must recall its last knob");
        // And the cab section recalls MIC_START + 1.
        assert_eq!(
            tab_in_panel(Some(back), &b, &o, 1, &mut mem),
            Some(MIC_START + 1)
        );
    }

    /// Panel 4: `Tab` steps pedals in chain order, ends on `+ ADD`, wraps, and
    /// remembers each pedal's last knob.
    #[test]
    fn tab_cycles_pedals_then_add_and_remembers_knobs() {
        let b = board(true);
        let o = order();
        let mut mem = NavMemory::new();
        // The first on-board pedal leads the chain (Gate).
        let gate = PEDALS[0].start;
        let second = tab_in_panel(Some(gate), &b, &o, 1, &mut mem).unwrap();
        assert_eq!(second, PEDALS[1].start, "Tab advances to the next pedal");
        // Leave the second pedal on a later knob, step away and back: restored.
        let moved = PEDALS[1].start + 1;
        let third = tab_in_panel(Some(moved), &b, &o, 1, &mut mem).unwrap();
        assert_eq!(third, PEDALS[2].start);
        assert_eq!(
            tab_in_panel(Some(third), &b, &o, -1, &mut mem),
            Some(moved),
            "a pedal must recall its last knob"
        );
        // The last pedal's successor is +ADD; from +ADD it wraps both ways.
        let last = PEDALS[PEDALS.len() - 1].start;
        assert_eq!(
            tab_in_panel(Some(last), &b, &o, 1, &mut mem),
            Some(ADD_TILE),
            "the pedal cycle must end on + ADD"
        );
        assert_eq!(
            tab_in_panel(Some(ADD_TILE), &b, &o, 1, &mut mem),
            Some(gate)
        );
        assert_eq!(
            tab_in_panel(Some(ADD_TILE), &b, &o, -1, &mut mem),
            Some(last)
        );
    }

    /// With an empty board, panel 4's `Tab` sits on `+ ADD` (the only step).
    #[test]
    fn tab_on_empty_board_stays_on_add() {
        let b = board(false);
        let o = order();
        let mut mem = NavMemory::new();
        assert_eq!(
            tab_in_panel(Some(ADD_TILE), &b, &o, 1, &mut mem),
            Some(ADD_TILE)
        );
        assert_eq!(
            tab_in_panel(Some(ADD_TILE), &b, &o, -1, &mut mem),
            Some(ADD_TILE)
        );
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

    /// Panel 2 `←`/`→` stays within the amp or mic group, wrapping there only.
    #[test]
    fn arrows_cycle_within_the_focused_amp_or_cab_section() {
        let b = board(true);
        let o = order();
        // Amp group: 6 knobs.
        assert_eq!(
            step_knob_in_panel(Some(AMP_START), &b, &o, 1),
            Some(AMP_START + 1)
        );
        assert_eq!(
            step_knob_in_panel(Some(AMP_START + AMP_KNOBS - 1), &b, &o, 1),
            Some(AMP_START),
            "→ past the last amp knob must wrap inside the amp group"
        );
        assert_eq!(
            step_knob_in_panel(Some(AMP_START), &b, &o, -1),
            Some(AMP_START + AMP_KNOBS - 1),
            "← before the first amp knob must wrap inside the amp group"
        );
        // Cab/mic group: 6..9, never crossing into the amp.
        assert_eq!(
            step_knob_in_panel(Some(MIC_START), &b, &o, -1),
            Some(MIC_END - 1)
        );
        assert_eq!(
            step_knob_in_panel(Some(MIC_END - 1), &b, &o, 1),
            Some(MIC_START)
        );
        // A stale focus re-anchors at the amp group's first knob.
        assert_eq!(step_knob_in_panel(None, &b, &o, 1), Some(AMP_START));
    }

    /// Panel 4 `←`/`→` stays within the focused pedal, wrapping there only.
    #[test]
    fn arrows_cycle_within_the_focused_pedal() {
        let b = board(true);
        let o = order();
        let gate = PEDALS[0].start;
        assert_eq!(step_knob_in_panel(Some(gate), &b, &o, 1), Some(gate + 1));
        assert_eq!(
            step_knob_in_panel(Some(PEDALS[0].end - 1), &b, &o, 1),
            Some(gate),
            "→ past the pedal's last knob must wrap inside the pedal"
        );
        assert_eq!(
            step_knob_in_panel(Some(gate), &b, &o, -1),
            Some(PEDALS[0].end - 1)
        );
        // A three-knob pedal walks only its own three.
        let comp = PEDALS[3].start;
        let mut seen = vec![comp];
        let mut f = Some(comp);
        for _ in 0..4 {
            f = step_knob_in_panel(f, &b, &o, 1);
            seen.push(f.unwrap());
        }
        assert_eq!(
            seen,
            vec![comp, comp + 1, comp + 2, comp, comp + 1],
            "arrows must wrap within the pedal only"
        );
        // The +ADD tile owns no knobs: arrows are inert there.
        assert_eq!(
            step_knob_in_panel(Some(ADD_TILE), &b, &o, 1),
            Some(ADD_TILE)
        );
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

    /// An off-board pedal's arrows re-anchor to its own first knob instead of
    /// leaking onto another pedal.
    #[test]
    fn arrows_never_leave_the_focused_pedal_group() {
        let mut b = board(false);
        b[3] = true; // only COMP is on the board
        let o = order();
        let off = PEDALS[0].start;
        assert_eq!(
            step_knob_in_panel(Some(off), &b, &o, 1),
            Some(off),
            "an off-board pedal's arrows must not reach another pedal"
        );
        // The mic group is unaffected by the board state.
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

    #[test]
    fn cursor_steps_through_rendered_stages_and_wraps() {
        let b = board(true);
        let o = order();
        // Full board: every slot renders, so the cursor walks raw order.
        assert_eq!(
            move_chain_cursor(&o, &b, ChainStage::Gate, 1),
            ChainStage::Whammy
        );
        assert_eq!(
            move_chain_cursor(&o, &b, ChainStage::Gate, -1),
            ChainStage::Reverb,
            "cursor must wrap around the ends"
        );
        // Sparse board: only on-board pedals + the amp and cab render.
        let mut sparse = board(false);
        sparse[3] = true; // COMP only
        // Rendered: COMP (slot 3), AMP (10), CAB (11).
        assert_eq!(
            move_chain_cursor(&o, &sparse, ChainStage::Amp, 1),
            ChainStage::Cab
        );
        assert_eq!(
            move_chain_cursor(&o, &sparse, ChainStage::Cab, 1),
            ChainStage::Comp,
            "cursor must wrap with three rendered stages"
        );
        assert_eq!(
            move_chain_cursor(&o, &sparse, ChainStage::Comp, 1),
            ChainStage::Amp
        );
        // Stale cursor (pedal left the board) re-anchors at the nearest end.
        assert_eq!(
            move_chain_cursor(&o, &sparse, ChainStage::Fuzz, 1),
            ChainStage::Comp
        );
        assert_eq!(
            move_chain_cursor(&o, &sparse, ChainStage::Fuzz, -1),
            ChainStage::Cab
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
