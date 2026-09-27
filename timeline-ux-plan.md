# Timeline UX & navigation — plan

Status: **proposed / in progress**. Companion as-built notes land in
[`timeline-sessions-implement.md`](timeline-sessions-implement.md) (increment
log) once implemented.

This is a focused follow-up to
[`timeline-sessions-plan.md`](timeline-sessions-plan.md). It collects the
navigation, readability, and recording-feedback gripes found while using the
practice timeline, and specifies the fix for each.

## Goals

1. Make the empty timeline navigable so the first import lands where the user
   expects.
2. Give the timeline a numeric time axis (a ruler) so the cursor position is
   readable at a glance.
3. Make clip placement less tedious with a `0.5 s` fine step and an explicit
   step control while moving.
4. Show the waveform **while** a take records, not only when it stops.
5. Make rows taller by default and add a one-key zoom to inspect a track's
   waveform full-panel.

Non-goals: drag-and-drop, mouse input, offline export changes, per-track
automation.

---

## F1. Empty-timeline navigation & first-import placement

**Problem.** With no tracks, `←`/`→` does nothing and the playhead is frozen
wherever it was last left. Because an import is placed at the playhead, the
first track can land far ahead of zero with no way to bring it back.

**Root cause.** `PracticeUi::seek_by` (`src/ui/practice.rs`) clamps to
`Session::extent_ticks()`, which is `0` on an empty session:

```rust
let total = self.session.ticks_to_frames(self.session.extent_ticks(), self.sample_rate) as i64;
let next = (cur + delta * dir).clamp(0, total.max(0));
```

So every seek clamps to frame `0`.

**Behavior.** Allow stepping into empty space. The upper bound becomes
`max(extent_frames, position + step_frames)`: backward always reaches `0`,
forward always advances at least one step and extends the (virtual) extent.
Imports keep honoring the playhead, which is now controllable from an empty
timeline. The transport header displays `max(extent, position)` as the total so
a position beyond the last clip is still legible.

**Acceptance.**
- On an empty session, `→` advances by the current step and `←` returns to `0`.
- Importing the first track after seeking places it at the sought playhead.
- With tracks present, seeking still clamps backward at `0`.

---

## F2. Time ruler

**Problem.** The only time readout is the transport line; the waveform rows
have no axis, so there is no spatial sense of where the cursor or a clip sits.

**Behavior.** Add a one-line ruler directly beneath the transport, spanning the
waveform area. It shows tick labels (`m:ss`, or `m:ss.d` when the extent is
short) at a spacing chosen so labels never overlap, and a bold playhead marker
over the exact current column. The ruler and all waveform rows share one fixed
left gutter so columns line up.

To make alignment exact, standardize the row gutter to a fixed width and move
the `uncal` flag inside it (see F5) instead of letting it shift the wave area by
six columns.

**Acceptance.**
- Tick labels are legible and never collide at short and long extents.
- The ruler's playhead marker column equals the marker drawn on every track row.

---

## F3. Fractional seek steps & move control

**Problem.** Steps are `1/5/10/30 s` only; `0.5 s` is missing. Moving a clip is
done with a hardcoded `0.1 s` nudge (`←`/`→`) and the seek step (`↑`/`↓`), so
placement is either too coarse or too tedious, and there is no way to change the
move step from inside the move modal.

**Behavior.**
- `SEEK_STEPS` becomes `[0.5, 1, 5, 10, 30]` seconds. `+`/`-` cycle through them
  (wrapping); the transport shows the active step.
- While moving a clip (`H`), `←`/`→` nudge by the current step and `+`/`-` cycle
  that same step, so the step is adjustable without leaving the modal. `R`
  returns the clip to the top.

**Serialization.** `Session::seek_seconds` becomes `f32`, and
`project::TransportSection.seek_seconds` keeps its name but gains a
`deserialize_with` that accepts either an integer (`5`, old manifests) or a
float (`0.5`), so existing projects load unchanged. Saving writes a float.

**Acceptance.**
- Cycle order is `0.5 → 1 → 5 → 10 → 30 → 0.5`.
- A manifest with `seek_seconds = 5` loads as `5.0 s`; `0.5` round-trips.
- In the move modal, changing the step changes the `←`/`→` nudge size.

---

## F4. Live waveform while recording

**Problem.** A take's waveform is empty until `R` stops the capture; peaks are
computed only in the writer worker's finalize path.

**Root cause.** `recording.rs::run_capture` accumulates samples and computes
`peaks()` once, immediately before sending `CaptureResult`. Nothing is shared
with the UI while the take is open.

**Behavior.** Add a `LivePeaks` feed shared between the capture writer worker
and the UI thread:

- Fixed time resolution (buckets of a few milliseconds), storing a running
  min/max per bucket plus the total sample count.
- The writer feeds each drained sample into it as it writes the WAV; the UI
  holds it only while a take is armed and renders the `Recording` row from it
  (guarded by `try_lock`, so a busy worker can never stall a redraw).
- On finalize the authoritative `CaptureResult::peaks` replace the live feed,
  and the feed is dropped.

Only the worker and UI thread touch the feed; the audio callback is unchanged
and still only pushes dry samples onto the existing ring.

**Acceptance.**
- While recording, the take row grows a waveform from the first samples.
- The finalized row's waveform is unchanged from today's output.
- A busy/slow writer never blocks the UI tick (no blocking lock in the render
  path).

---

## F5. Taller rows & `Tab` zoom

**Problem.** Each track is a single line, so the waveform is a tiny sparkline;
there is no way to inspect where the highs and lows sit.

**Behavior.**
- Default track row height is **3 lines**, drawn as a symmetric min/max envelope
  (real vertical resolution) rather than a bottom-anchored sparkline. The
  header (LED / tag / `uncal` / name / gain) occupies the fixed gutter on the
  first line; the waveform spans the aligned columns on every line.
- `Tab` (currently inert on the timeline, see `src/ui/input.rs`) cycles a
  row-zoom state for the focused track:
  - `Normal → Expanded` (the focused row takes the whole track-list area),
  - `Expanded(row A)` + `Tab` on a **different** row → `Split(A, B)` (50/50),
  - `Tab` on an already expanded/split row → back to `Normal`,
  - `Tab` on the transport, or on an empty list, does nothing.
- Scrolling keeps the selected row visible under variable row heights.

**Acceptance.**
- Tracks render three lines by default with a centered envelope.
- `Tab` walks `Normal → Expanded → Split → Normal` as described and leaves
  other panels' `Tab` behavior untouched.
- The ruler (F2) stays aligned with every row's waveform gutter.

---

## F6. Waveform resolution & envelope

**Problem.** The waveform reads as a thick block with occasional notches: a pick
followed by a quieter note merges into one tall rectangle. Imports stored only
512 peak buckets and takes 1024 (~0.5 s each on a few-minute track), and the
renderer sampled a *single* bucket at each column's left edge, so most
transients were never looked at.

**Behavior.**
- Store peaks adaptively at ~100 buckets/s (10 ms), clamped to `[1024, 65536]`
  (`practice::peak_buckets`), for imports and finalized takes alike. Live
  captures already publish ~5 ms buckets.
- Each waveform column aggregates the **min of `lo` and max of `hi`** over every
  bucket its time span covers (clipped to the clip's frames), and draws the true
  centered min/max envelope rather than a peak-to-peak magnitude. Silence
  collapses to the dim center baseline.

Horizontal time zoom (millisecond detail) remains a possible follow-up; the
higher bucket resolution is chosen to support it later.

---

## F7. Braille waveform

**Behavior.** Each character cell is rendered as a braille block (U+2800): two
dot-columns and four dot-rows, so one text line gives four amplitude rows and
one cell covers two time steps — 2× the time and 4× the amplitude resolution of
half-blocks. The filled min/max envelope is unchanged; silent cells keep the dim
centre baseline and the playhead stays a solid `│`.

**Tradeoffs.** The look is a dot-matrix (small dots can read faint on some
fonts); braille cells are single-colour, so the loop tint is cell-granular; no
memory or meaningful CPU cost. `braille_bit`/`braille_glyph` are small and
self-contained, so reverting to half-blocks is easy if a terminal renders
braille poorly.

---

## F8. Playhead-centred time zoom

**Behavior.** A viewport `(start, len)` replaces the whole-timeline mapping.
Fit (`None`) covers the span; otherwise the window comes from
`ZOOM_WINDOWS = [60, 30, 10, 5, 2, 1, 0.5, 0.2] s`, centred on the playhead and
clamped so it never runs past the span. `Ctrl+↑` zooms in, `Ctrl+↓` out; zooming
out past 60 s returns to fit. Because `←`/`→` seek, seeking pans the window.
The ruler draws sub-second ticks (`m:ss.d` / `m:ss.dd`) and the transport shows
the active window.

**Supporting change.** Stored peaks rise to ~500 buckets/s
(`practice::peak_buckets`, clamp `[2048, 262144]`) and `LivePeaks` to ~2 ms
buckets, so the window stays sharp to about 0.2 s before aliasing.

**Tradeoffs.** Memory up to ~2 MB per long track at the cap; viewport state
touches the ruler, waveform mapping and playhead/loop clipping; off-screen clips
need seeking to reach. Zoom resets to fit on a new/loaded session.

---

## Affected files

| Area | Files |
| --- | --- |
| Session model / steps | `src/session.rs` |
| Project serialization | `src/project.rs` |
| Capture peaks | `src/recording.rs` |
| Timeline UI | `src/ui/practice.rs`, `src/ui/draw.rs`, `src/ui/mod.rs` |
| Docs | `docs/guide.md`, help modal in `src/ui/draw.rs` |

## Verification

- `cargo test`, `cargo clippy --all-targets`, `cargo fmt --check`.
- Unit tests: fractional step cycle and manifest compatibility (`session.rs`,
  `project.rs`); empty-timeline seek and move-step nudge (`ui/practice.rs`);
  live peak bucketing (`recording.rs`).
- Headless render test (`TestBackend`) for the ruler alignment and the three
  row-zoom states.
