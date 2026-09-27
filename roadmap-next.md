# rusty-riff — next-roads plan

This is the **off-path roadmap**: work deliberately outside the fidelity plan
([`fidelity-plan.md`](fidelity-plan.md) / [`fidelity-implement.md`](fidelity-implement.md)).

The fidelity path stays the primary track. This document captures product and
engineering directions so they don't get lost, and is the acceptance record for
when each is picked up. Nothing here is committed to a release order by existing;
the ordering at the bottom is a recommendation.

Each item states **why**, **scope**, **audio-thread constraints**, and
**acceptance**, mirroring how the fidelity docs are written.

---

## Track 1 — Playability and performance control

### 1.1 MIDI input + expression-pedal control

**Why.** The wah is currently an *auto-wah* (`src/dsp/effects/wah.rs`); every
preset comment that claims a "rocked wah" describes something the software can't
do. An expression treadle also turns the rig into a playable instrument rather
than a knob-only emulator. This is the single highest playability payoff.

**Scope.**

- A MIDI backend on the control thread (no allocation or lock scope on the audio
  thread). Learn/bind screen: map a CC to a target (wah position, amp gain,
  delay mix, master width, …).
- Continuous targets are already `Arc<AtomicF32>`; a bound CC writes them. The
  audio thread keeps reading the same atomics — no new hot-path code.
- Wah gains a **manual mode**: position comes from the bound control instead of
  the envelope follower. Keep auto-wah as a mode, not a replacement.
- Add a **learn** workflow (move the pedal, it binds) and persistence in the
  config dir.

**Audio-thread constraints.** MIDI parsing and binding live off the callback;
only atomic stores cross the boundary (same discipline as A/B and the timeline).

**Acceptance.**

- A bound CC moves the wah smoothly through its range in manual mode; auto mode
  is unchanged when unbound.
- Learn/bind persists across restarts; unbinding restores auto/default.
- Unit tests for the mapping; a manual pass with a real controller.

**Status — done (CC bind via config; no learn UI yet).** The wah gained a
**manual treadle mode** (`wah mode = 1`) with a `POSITION` knob (250 Hz–2.5 kHz),
and `src/midi.rs` (new `midir` dependency) connects to the first MIDI input and
drives `wah_position` from the **Expression CC (11)**, switching the wah to
manual when the pedal moves. Bindings live in `~/.config/rusty-riff/midi.conf`
(`enabled`, the legacy `cc = 11` shorthand, and `<cc> = <target>` for
`wah_position`, `delay_mix`, `reverb_mix`, `boost_gain`, …). No hardware was
available to verify the live path here — the parsing/mapping are unit-tested, and
the app runs fine with no controller. A learn/bind **screen** remains.

### 1.2 Tap-tempo and MIDI clock sync

**Why.** Delay times are set by a normalized knob; players set delay by tapping
or from the session clock.

**Scope.** A tap-tempo binding computes BPM and writes `delay_time`; optional MIDI
clock → `delay_time`. Keep the knob authoritative when tapped.

**Acceptance.** Tap a few beats and the delay locks; MIDI clock changes track;
tests for the tap-tempo windowing.

**Status — tap-tempo done; MIDI clock remains.** `;` taps; two or more steady
taps set the delay `TIME` to the tapped interval (clamped to 0–500 ms) and show
the BPM in the footer. `src/tap_tempo.rs` is a tested pure state machine
(too-fast taps ignored, slow taps restart the count). MIDI Clock → `delay_time`
is not built.

---

## Track 2 — Practice and composition

### 2.1 Looper / overdub

**Why.** The practice player already decodes, resamples, and plays backing
tracks against a shared timeline (`src/practice.rs`, `src/dsp/player.rs`). A
looper is the natural next step and makes the app a writing tool.

**Scope.**

- Record a **loop region** on the timeline: capture the calibrated dry guitar
  (or the processed mix — decide), then play it back layered.
- **Overdub**: layer successive passes; **undo** the last layer.
- Reuse the existing lock-free install/dispose discipline for new buffers
  (`rtrb`), and the monitor-only bus so loops are never captured in a rendered WAV.

**Audio-thread constraints.** Loop buffers are preallocated on the control thread
and installed lock-free; displaced buffers ship back for disposal (same as
practice tracks, plugin inserts, external IRs).

**Acceptance.** Record → loop → overdub → undo round-trips; loop timing stays
sample-aligned; the recording tap excludes loops; tests for the loop cursor math.

**Status — done.** `src/looper.rs` implements a monitor-only looper (record →
loop → overdub → undo) with preallocated buffers and lock-free control; the loop
bus rides the monitor path after the recording tap, like the metronome. Keys: `Y`
record/pause/resume, `F` clear, `,` overdub, `.` undo; state shows in the footer.
Unit tests cover cursor wrap, overdub, undo and no-grow; a whole-bank
integration test (`tests/bundled_presets.rs`) guards preset renders.

### 2.2 Timeline UX & navigation

**Why.** The practice timeline needed a readable, navigable waveform while
writing and recording.

**Scope / acceptance.** See [`timeline-ux-plan.md`](timeline-ux-plan.md) (F1–F11):
empty-timeline navigation, a time ruler on a fixed gutter, fractional seek steps,
a live recording waveform, taller rows with `Tab` row-zoom, adaptive + braille
waveforms, playhead-centred time zoom, and waveform gain modes. **Status — done**;
as-built as increments D3–D4 in
[`timeline-sessions-implement.md`](timeline-sessions-implement.md).

### 2.3 Loop-region export

**Why.** The offline export was whole-session only; capturing just a good loop
meant exporting everything and trimming by hand.

**Scope.** The `E` dialog gains a `Tab` range control: **full session** (default,
unchanged) or exactly the **loop region** `[loop_start, loop_end)` (no appended
tail; the chain is warmed from frame 0 for continuous state). **Status — done**;
as-built D5, resolving the old timeline limitation #10.

---

## Track 3 — UX and product

### 3.1 Preset browser: search, tags, favorites, A/B compare

**Why.** 17 bundled presets already fill the modal; users will have many more.
Finding and comparing tones should not be scrolling.

**Scope.** Fuzzy search over name/description; tag and favorite fields
(backward-compatible TOML, defaulted); an A/B compare that flips between two
selected presets without leaving the modal.

**Status — favorites + search + tags done.** `F` stars the selected preset (a `★`
column), persisted by name in `~/.config/rusty-riff/favorites.txt`.
**Type-to-filter** matches name/description/**tags** (printable keys filter,
`Backspace` deletes, `Esc` clears then closes; commands are the upper-case
letters). The `tags = [...]` preset field is defaulted and shown in the list; the
17 bundled presets are tagged (artist/role). **A/B compare** is in: `X` in the
browser flips between the two most recently applied presets, keeping the modal
open. **Track 3.1 is complete.**

**Acceptance.** Snapshot tests for the modal; loading/search round-trips;
favorites persist.

### 3.2 Onboarding and device profiles

**Why.** First-run asks device → channel → output and then input calibration. A
saved per-interface profile (channels, trim, pickup class) removes repeats.

**Scope.** Persist a named profile keyed to the interface name; a one-screen
setup that reuses it.

**Acceptance.** Relaunch reuses the profile; changing interface prompts once.

**Status — effectively done (pre-existing).** Device selection is remembered by
*name* in `~/.config/rusty-riff/audio.conf` (survives re-enumeration), and
`input-calibration.toml` stores **one calibration row per
`device + channels + channel`** (trim, pickup class, reference version) that is
matched on startup — i.e. a per-interface profile already. A dedicated
one-screen "profiles" setup is not needed; scope stays as-is.

### 3.3 Theming and layout polish

**Why.** The TUI is fixed-colour and can crowd small terminals.

**Scope.** A couple of colour themes, a compact mode, and graceful narrow-width
layout. Keep the ratatui snapshot tests meaningful.

**Acceptance.** Screenshots/snapshots per theme; no clipping at a small size.

**Status — not started (deferred).** Making the palette runtime-selectable means
threading a theme through ~537 colour-constant uses in `src/ui/**` (the `PEDALS`
table itself embeds `PEDAL_*` colours). That is a large, mostly mechanical
refactor with real regression risk for a cosmetic win, so it is deferred to its
own dedicated change. Narrow-width layout polish remains available in the
meantime.

---

## Track 4 — Engineering and quality

### 4.1 CI

**Why.** The gates (`fmt`, `clippy -D warnings`, `test --all-features`, harness
`--check`) should run on a clean clone, not by hand.

**Scope.** A GitHub Actions workflow running the gates on push/PR with a Rust
toolchain cache, warn-free and fast enough to be useful.

**Status — done (and extended).** `.github/workflows/test.yml` already ran
`fmt --check`, `clippy --all-features -D warnings`, `cargo test`, and a
pending-snapshot guard on Linux. **Added a macOS job** that runs
`cargo test --all-features` (covering the AudioUnit host, which only compiles on
macOS) and the **fidelity harness** `--check` against
`docs/fidelity/baseline-synth-48k.toml` (generated on aarch64-apple-darwin), so
preset/DSP voicing drift now fails CI.

**Acceptance.** Green on `main`; a deliberately broken commit fails the right gate.

**Caveat (needs a maintainer action).** Push/PR triggers are **not creating runs**
on this repo/account right now — a manual `gh workflow run test.yml` runs both
jobs green, including the fidelity `--check`. Until the push trigger is restored
(Settings → Actions), CI must be run manually or via the weekly schedule.

### 4.2 Benchmarks

**Why.** The audio path and cab construction have real budgets (the docs already
note `DspChain::new` is ~1.7 s in debug). Regressions should be visible.

**Scope.** Benchmarks for the callback cost per stage and for `CabBank::new`
(construction), runnable locally and (optionally) in CI with generous thresholds.

**Status — done.** `examples/bench.rs` (dependency-free) times `DspChain::new`,
`CabBank::new`, and `process_block` on representative presets, printing µs/block
and % of the 10 ms realtime budget. Measured on the maintainer rig: chain build
~222 ms, cab build ~221 ms, `process_block` 3.3–3.6 % of realtime. **Not yet a CI
gate** (thresholds would be machine-dependent).

### 4.3 Docs site or generated user guide

**Why.** The Eleventy site was removed in the de-fork; the user guide is now the
README plus the in-app `K` overlay. A generated guide would help onboarding.

**Scope.** Either a small static docs build from Markdown, or a `cargo doc`/
mdBook guide mirroring the README + `K` reference. No docs-parity rule beyond
keeping the README honest.

**Status — done.** `docs/guide.md` covers build/run, calibration (`N`), the
signal chain, presets (browser, favorites, search, save/export/import, config
paths), practice/recording, and plugins; linked from the README. A hosted docs
site was not re-added.

---

## Recommended order

_Done: 4.1 CI, 4.2 benchmarks, 4.3 guide, 3.1 presets, 3.2 onboarding profiles,
1.1 MIDI expression + CC→knob config, 1.2 tap-tempo, 2.1 looper, 2.2 timeline UX,
2.3 loop-region export. See above._

Remaining, in order:

1. **1.2 MIDI clock** — the other half of the MIDI transport story (tap-tempo is
   in).
2. **3.3 Theming / narrow-width layout** — a large, mostly mechanical palette
   refactor; deferred to its own change.
3. **1.1 MIDI learn/bind screen** — config already maps CCs; the screen is UI
   polish.

The **fidelity path is closed**: the amp effects loop (Phase 2 item 5) was
retired as out-of-scope, the TS-808 was corrected against the circuit (no 340 Hz
input HP; 720 Hz gain shelf), and the built-in↔AU toggle is now clickless. Only
the measurement-bound Phase 3 items (measured-IR / cab-capture match) remain, and
those are blocked on re-amp/mic captures.
