# Fidelity — implementation notes (as-built / handover)

Companion to [`fidelity-plan.md`](fidelity-plan.md) (formerly `plan.md`). The
plan is the **design / acceptance** document; this file is the **as-built**
record: what shipped, review findings, invariants, deviations, and what is still
missing. Evidence for historical gear claims lives in
[`docs/fidelity-references.md`](docs/fidelity-references.md). Read all three
before reviewing or continuing. (This file was formerly
`IMPLEMENTATION-NOTES.md`.)

> **De-fork note (2026-09-25).** This repository is a modified fork renamed
> **rusty-riff**. The Eleventy docs site (`site/`) that earlier entries below
> reference was removed in the cleanup; current documentation is the README,
> `CONTRIBUTING.md`, and the in-app `K` help. Historical `site/*` mentions below
> describe what those past commits did at the time.

## Status

| Area | State | Where |
| --- | --- | --- |
| De-fork cleanup — docs site removed, project renamed `rusty-riff` | **Done** | increment log |
| Phase 1 — bypass transparency, order snapshot, route tests, studio-master width | **Done** (see review: the order snapshot needs A1) | below |
| Phase 2 items 1–4 — Amp/Cab split, migration, UI | **Done** | below |
| Phase 2 item 5 — real preamp/loop/power-amp split | Not started | plan Phase 2.5 |
| Phase 0 — reference matrix | **Scaffold only**: no sources logged | `docs/fidelity-references.md` |
| Phase 0 — offline harness, CPU/latency capture | **Done** (B1, B2) | plan "Next increments" |
| Workstream A — routing hardening (review findings) | **Done** (A1–A5) | increment log |
| Workstream B — input calibration + harness | **Done** (B1–B8) | increment log |
| Phases 3–5 — amp/cab fidelity, named pedals, preset rebuild | Not started | plan Phases 3–5 |

The work below changed routing, topology, and documentation only. **No voicing
has changed yet**, so none of the bundled Pink Floyd / Eagles / Led Zeppelin
presets is closer to the records than before this roadmap started. That is
expected: the plan requires references and a measurement harness first.

Fidelity phases (0, 3, 4, 5) — see
[Open gaps](#open-gaps-for-a-following-agent) for the references and decisions
each one still needs.

Scope agreed with the maintainer: **Phase 1 routing patch (including item 4,
the configurable studio master), then Phase 2 amp/cab split**, delivered as
small commits with all findings recorded here.

---

## What changed

### 1. Bypass is now wire-transparent — `fix(dsp): make bypassed stages wire-transparent`

- File: `src/dsp/mod.rs`
- Added `Params::stage_enabled(stage) -> bool` (mirrors each stage's
  `*_enabled` atomic; `Amp`/`Cab` are always live).
- `DspChain::run_ordered_stage` (`src/dsp/mod.rs`) now returns the incoming
  `Sig` **before** any mono↔stereo bridging when the stage is disabled.
- Tests added: `bypassed_mono_stage_preserves_stereo`,
  `bypassed_stereo_stage_preserves_mono`, `stage_enabled_mirrors_bypass_flags`.

**Bug fixed:** the dispatch used to bridge domains first and consult the bypass
flag second. A disabled mono pedal on a stereo feed still evaluated
`0.5*(l+r)` and duplicated the result, collapsing L/R; a disabled stereo pedal
on mono promoted the signal early. Both are gone.

### 2. Whole-order snapshot via seqlock — `fix(dsp): publish the chain order as one coherent snapshot`

- File: `src/dsp/mod.rs`
- Added `Params::chain_seq: Arc<AtomicU64>` guarding the existing
  `chain_order: Arc<[AtomicU8; CHAIN_LEN]>`.
- `set_chain_order` publishes under an odd/even sequence; `chain_slots` reads
  the slots and retries unless the sequence is stable and even.
- `process_block` and `process` snapshot **once**; `process_core` takes
  `&order`. The old per-**sample** 19-atomic read is gone.
- Test added: `chain_order_snapshot_is_never_torn_under_concurrent_swaps`
  (200k concurrent writes, 200k reads, every read must be a whole permutation).

**Deviation from the roadmap's suggestion.** The plan proposed an `rtrb`
command ring carrying the full order by value. A `SeqCst` seqlock was chosen
instead because it keeps the existing shared-`Arc<Params>` architecture and
needs no constructor/ring plumbing at the ~15 `DspChain::new` call sites. It is
allocation-free and reads once per block.

> **Review correction (2026-09-25).** The original note called the seqlock
> "blocking-free". It is not: the reader spins **without bound** while the
> sequence is odd, so a preempted UI writer stalls the audio callback (priority
> inversion), and it is correct only with a single writer, which nothing
> enforces. The spin cost is not the problem; the unbounded wait is. Fix planned
> as **A1** (bounded `try_chain_slots` + audio-owned last-good order + a
> writer-only mutex). Also: `CHAIN_LEN` is now 20, not 19, since the amp/cab
> split.

### 3. Route-domain test matrix — `test(dsp): cover route-domain transparency and coherent reorder`

- File: `src/dsp/mod.rs`
- `bypassed_mono_stage_moved_after_live_stereo_is_identical` — end-to-end: with
  Chorus + Reverb live, moving a bypassed Wah from before the amp into the
  stereo region is bit-identical.
- `live_mono_stage_after_stereo_downsamples_to_mono` — a **live** mono stage
  still sums a stereo feed to dual mono, as documented.
- `rapid_reorder_keeps_a_complete_permutation` — 1000 adjacent swaps never
  yield a duplicate/missing stage.
- Existing `swapped_chain_order_changes_output` (noncommuting enabled effects
  differ) and `preset_chain_order_applies_and_round_trips` (save/load) already
  cover the remaining roadmap bullets and were kept.

### 4. Docs — this commit

- `site/presets.md` — replaced the false blanket claim ("omitting a section
  leaves that effect's current state unchanged"). Real `Preset::apply`
  semantics (`src/preset.rs:545`), now documented:
  - Required: `[tube_screamer]`, `[amp]`, `[reverb]`.
  - Omitted optional effect section ⇒ that effect is **turned off** (knob values
    retained but bypassed).
  - Exceptions that **retain** current state when omitted: `[noise_gate]`,
    `[cabinet]`.
  - Omitted `[chain]` ⇒ chain order resets to the shipped default.
- `site/how-it-works.md` — documented that only a **live** effect converts
  mono↔stereo; a bypassed stage is wire-transparent.
- This file.

---

## Phase 2 — amp/cab split

Commit: `feat(dsp): split the amp+cab block into separate Amp and Cab stages`

- **Enum** (`src/dsp/mod.rs`): `ChainStage::AmpCab` (one slot) became
  `Amp` + `Cab`, so `CHAIN_LEN` is now 20 (10 pre + amp + cab + 8 rack).
  `pedal_index`/`from_pedal_index` remapped; the rack pedals keep their 0–17
  indices.
- **Dispatch**: `run_pre`/`run_post`/`amp_cab`/`ampcab_index` were replaced by a
  single-pass `run_range` + `run_ordered_stage`. `Amp` always runs and folds its
  input to mono; `Cab` runs mono→stereo and is skipped only when
  `ext_amp_supplies_cab()` (a live full-rig AU) is true. Default-order rendering
  is bit-identical to Phase 1.
- **External amp path** (`process_block`): stages before `Amp` run per sample
  (mono), the hosted plugin processes the block, then stages after `Amp`
  (including `Cab` and any effects left between amp and cab) run per sample.
- **Sanitizing**: `sanitize_chain_order` now also repairs a cab placed before
  its amp (swap); `amp_precedes_cab` is the shared predicate.
- **Presets** (`src/preset.rs`): legacy `"ampcab"` in `[chain]` expands to
  consecutive `"amp"`, `"cab"` at the same position; new saves write the two
  names. Tests: `preset_legacy_ampcab_expands_to_amp_cab`,
  `preset_chain_order_applies_and_round_trips`.
- **UI**: the ribbon renders separate `AMP` / `CAB` tiles (`AU: NAME` replaces
  `AMP`; `IR: NAME` / `AU CAB` on the cab tile). `move_selected_stage` rejects a
  move that would put the cab before its amp
  (`move_selected_stage_moves_amp_and_cab_separately`). Golden snapshots
  re-blessed; the only change is the ribbon line splitting `AMP+CAB` into
  `AMP ──▶ CAB`.
- **Docs**: `site/presets.md`, `pedals.md`, `how-it-works.md`, `plugins.md`,
  `amps-cabs.md`, `index.md` updated for the two-stage model and the
  amp-before-cab constraint.

**Boundary semantics (item 2 of the roadmap).** Effects may be placed between
`Amp` and `Cab`; that region is documented/treated as line-level processing in a
virtual load box, not a pedal in the speaker cable. Ordinary pedals are never
*defaulted* there (the shipped order keeps every pedal before the amp or after
the cab). The richer "named regions" UI (labels/tooltips) and the optional real
preamp/loop/power-amp refactor (Phase 2 item 5) are **not** done.

---

## Phase 1 item 4 — configurable studio master

Commit: `feat(dsp): make the master-bus widener a configurable studio width`

- `Params::master_width` (`Arc<AtomicF32>`) drives `master_bus(l, r, width)`;
  the output soft limiter is applied independently of the width, so protection
  never depends on the setting. `DEFAULT_MASTER_WIDTH = 1.3` keeps every existing
  preset/recording sounding exactly as before.
- Read once per block, not per sample (same discipline as the chain order).
- `W` cycles neutral (`1.0`) ↔ studio wide (`1.3`) live, with a status toast;
  documented in the `K` help modal and the site key table.
- Preset `[master] width` round-trips; an omitted `[master]` resets to `1.3` for
  deterministic loading (`site/presets.md` documents it).
- Tests: `master_bus_width_is_neutral_at_one_and_limiter_is_independent`,
  `master_width_changes_the_output`,
  `preset_master_width_round_trips_and_defaults`.

**Decision (worth revisiting with the maintainer):** the roadmap prefers a
*neutral default* for a reference mode, but neutral-by-default would change the
sound of every bundled preset and the marketed "studio-grade stereo". The
default was therefore kept at the historic `1.3`, with neutral one keypress away
— the "explicit compatibility setting" the roadmap also allows. If the project
wants neutral-by-default, flip `DEFAULT_MASTER_WIDTH` to `1.0` and re-bless the
snapshots (no other change needed).

---

## Phase 0 foundation + preset accuracy cleanups

Commits: `docs: add the Phase 0 fidelity reference scaffold`,
`test(presets): assert bundled presets load deterministically`,
`docs(dsp): reconcile the TS-808 input-HP prose…`,
`docs(presets): make descriptions match the enabled signal path`.

- **Reference scaffold** — new [`docs/fidelity-references.md`](docs/fidelity-references.md):
  the code-derived inventory of all 17 presets (amp, cab, enabled effects in
  signal order, delay/fuzz mode), a description-vs-path flag table, the
  per-preset reference checklist, and a source-log template. It makes no
  historical claim; it is the baseline a following agent annotates.
- **Determinism regression** — `bundled_presets_load_deterministically` applies
  each preset onto a hostile prior state and compares a sound-determining
  fingerprint to a fresh load. Verified it fails when a preset omits
  `[noise_gate]`. All 17 already set `[noise_gate]`/`[cabinet]` and omit
  `[chain]`, so this only locks behavior.
- **TS-808 prose** — the module header now says 340 Hz (matching the constructor
  and `site/how-it-works.md`), flagged as an RC-derived estimate pending a
  measured response. **No DSP change.**
- **Preset descriptions** — fixed contradictions provable from the TOML, no
  parameter/tone changes:
  - AC/DC ×2 said "no pedals" while enabling pre-EQ + parametric EQ + reverb →
    "no drive pedals" + names the shaping.
  - Stairway solo said "small-amp" while selecting Plexi + Greenback 4×12, and
    its site blurb said "Echoplex slap" while the preset's `[delay]` omitted
    `type`, resolving to **digital ping-pong**. Description fixed; `type = 0.0`
    pinned explicitly (behavior-identical). The intended echo device is now a
    flagged reference question.
  - Pink Floyd `Echorec` / `Electric Mistress` / `Phase 90` are hedged as
    `-style` approximations (the DSP implements a generic tape echo / flanger /
    phaser), and Money's wah is described as what it is (auto-wah).
  - Van Halen presets named the enabled Tube Screamer (those two presets were
    later removed from the bundle — see the bundle-change increment-log row).
  - Mirrored in the `site/presets.md` bundled table.

---

## Findings not yet actioned

### TS-808 input-HP value still unverified

`src/dsp/effects/tube_screamer.rs` now documents the input coupling HP as
**340 Hz** (`0.047 µF` into `10 kΩ`). That is an RC estimate, not a measured
value; the code, the 720 Hz feedback-network peak, and the two EQ stages in the
Floyd presets may still be compensating for each other. Per the roadmap, do
**not** retune without a schematic or a measured TS-808 frequency response
(Phase 4). The prose contradiction itself is resolved.

### Not an actual effects loop yet

Phase 2 split `Amp` and `Cab` but the amp DSP is still one block. A genuine
amp effects loop (preamp → loop send/return → power amp) is Phase 2 item 5 and
remains unimplemented. Effects between `Amp` and `Cab` model a *virtual
load-box / post-power-amp line-level* path, not the amp's internal loop.

### No crossfade on the built-in ↔ AU toggle

A2 guarantees the built-in/AU switch lands on a **block boundary** (one coherent
`BlockRoute` per block), but it does not crossfade: a mid-block toggle is simply
deferred to the next block. A clickless crossfade remains out of scope.

---

## Review 2026-09-25

A code review of everything above. Each finding lists evidence and the plan
item that resolves it (see `fidelity-plan.md` → *Next increments*). Mark a
finding **resolved** here, with the commit, when its item ships.

### Code findings

| # | Severity | Finding | Evidence | Resolved by | Status |
| --- | --- | --- | --- | --- | --- |
| R1 | **High** | The chain-order seqlock reader spins without bound on the audio thread while a writer holds the sequence odd; a preempted UI writer stalls the callback. Single-writer is assumed, not enforced. | `Params::chain_slots` / `set_chain_order`, `src/dsp/mod.rs` ~1110–1140; called from `process` / `process_block` | A1 | **resolved** (A1) |
| R2 | Medium | Routing state is read at inconsistent rates: `use_ext_amp` per block, but the Cab stage's `ext_amp_supplies_cab()` per sample. A mid-block AU toggle can run the built-in amp with no cab for the rest of the block. `process()` never runs the AU yet skips the cab when a full-rig AU is flagged active. | `process_block` ~1697 vs `run_ordered_stage` ~1605; `ext_amp_supplies_cab` ~1379 | A2 | **resolved** (A2) |
| R3 | Low | With a **full-rig** AU, stages placed between AMP and CAB process the AU's already-miked output, not a line-level signal; the docs describe that region only as "virtual load box". | `run_ordered_stage` Cab arm; in-app `K` help | A4 | **resolved** (A4) |
| R4 | Low | `amp_stage` doc says it is bypassed when an external amp is active (only `process_block` does that); comments still say "19 byte stores" although `CHAIN_LEN = 20`. | `src/dsp/mod.rs` ~1107, ~1386 | A1, A2 | **resolved** (A1, A2) |
| R5 | Low | Relative links in `docs/fidelity-references.md` pointed at `plan.md` / `IMPLEMENTATION-NOTES.md` inside `docs/` (files that do not exist there). | `docs/fidelity-references.md` lines 3, 27, 86, 164 | Doc reorganization (this commit) | **resolved** |
| R6 | Low | The determinism test fingerprint omits amp knobs, fuzz/delay `type`, and knob values of enabled stages, so it would not catch a regression there (loading is currently correct: `apply` resets amp knobs to model defaults and serde defaults the types). | `rig_fingerprint`, `src/preset.rs` ~967 | A3 | **resolved** (A3) |
| R7 | Low | `on_input` resizes/extends buffers when a callback exceeds `MAX_BLOCK = 4096` frames — an allocation on the audio thread (rare). | `src/audio/mod.rs` ~835–849, `MAX_BLOCK` line 80 | A5 | **resolved** (A5) |
| R8 | Decision | `DEFAULT_MASTER_WIDTH = 1.3` kept for compatibility. For period-accurate presets, set `[master] width = 1.0` per preset (guitar on these records is a mono track) rather than flipping the global default. | `src/dsp/mod.rs` ~475 | Preset phase | **resolved** (2026-09-25): all 17 bundled presets now set `[master] width = 1.0` |
| R9 | Gap | There is **no input-level calibration** anywhere: amp breakup depends on the user's interface gain, so presets tuned on one interface are under/over-driven on another. Likely cause of the "rescue" TS + two EQs in several presets. | `rg -i "input_gain\|trim\|calibrat" src/` finds nothing relevant | B3–B5, B8 | **resolved in code** (B3–B5; B8 retunes the provisional targets) |
| R10 | Gap | Phase 0 step 3 (offline harness) was not built. Analysis helpers (`db`, `rms`, `goertzel`, `ltas`, `envelope`, `percentile`) are duplicated across `examples/`. The timeline's raw takes + offline export (`src/export.rs`) already provide most of the rendering machinery. | `examples/di_compare.rs`, `drive_analysis.rs`, `amp_analysis.rs`, `knob_match.rs` | B1, B2 | **resolved** (B1, B2) |

Verified correct during the review (no action): bypass wire-transparency;
Amp/Cab split and `"ampcab"` migration; `sanitize_chain_order` repair;
`Preset::apply` determinism for amp knobs and types; the take chain and live
chain each snapshot the order once per block; audio buffers are preallocated at
`MAX_BLOCK`; dry captures are 32-bit float.

### Deferred findings for the fidelity phases

Historical claims below are **commonly reported, not verified** — log sources
in `docs/fidelity-references.md` before changing any preset on their basis.

- **Anachronisms — tooling done (Phase 5 to fix the cases).** Every bundled preset
  now carries `year` metadata, and `no_new_device_anachronisms` fails if an enabled
  device with a known debut (TS-808 1979, DS-1 1978, ML-2 2004) postdates the
  recording. The three known TS-808 cases — `led_zeppelin_stairway_solo` (1971),
  `pink_floyd_shine_on_crazy_diamond` (1975), `eagles_hotel_california_solo`
  (1976) — are listed in `KNOWN_ANACHRONISMS` for the Phase 5 rebuild to remove.
  (The removed `van_halen_*` presets were also flagged; the added 1982/1991
  presets keep the TS off.)
- **Stairway solo.** The cleanup changed the description *toward the code*
  (Plexi + Greenback 4×12 + TS). The commonly reported session rig is a
  Telecaster into a small Supro combo — the earlier "small-amp" wording was
  probably closer. Flag the code, not the description; no Supro-like model
  exists yet.
- **Hotel California.** The solo preset still says Plexi is "the cranked
  non-master head the Eagles actually used" — an unhedged claim the description
  pass missed. Commonly reported: Les Paul (Felder) into small Fender tweed
  amps. The intro is commonly reported as a 12-string acoustic, so the clean
  Twin + chorus + digital delay + two reverbs preset may not correspond to a
  recorded part.
- **Plexi rectifier — resolved (Phase 3).** The GZ34 tube-rectifier assumption
  fits a JTM45, not the model 1959 the presets target. The Unicord 1970 1959
  schematic shows a solid-state bridge (`4 X DIODE`) and drtube dates the GZ34
  phase-out to ~1966, so the supply was retuned to a stiff, fast-recovering
  silicon rail (see the Phase 3 component references and the `fix(amp)` commit).
  The amp's bloom now comes from the output transformer/speaker, not rectifier
  sag.
- **Hiwatt DR103** silicon-rectified supply claim is consistent with the
  hardware; the Hiwatt/WEM pair dominates every Floyd preset, so it stays
  first in Phase 3.
- **Missing components with the largest expected impact** for Pink Floyd /
  Eagles / Led Zeppelin: Binson Echorec (multi-head drum echo; currently the
  EP-3 tape mode), small tweed-Deluxe-style combos (`add-amp-model` skill), and a
  pickup/guitar-volume input model (single-coil vs humbucker loading; Fuzz Face
  cleanup). Now built: the Supro-style combo, the Twin spring tank, and a
  Colorsound Power Boost-style clean boost as the period-correct alternative to
  the TS.

---

## Increment log

Append one row per commit from Workstreams A/B onward.

| Commit | Item | Summary | Tests | Deviations |
| --- | --- | --- | --- | --- |
| `e77b04d` | docs | Renamed `plan.md` → `fidelity-plan.md`, `IMPLEMENTATION-NOTES.md` → `fidelity-implement.md`; added Workstreams A/B to the plan and this review; fixed links (R5). | n/a | — |
| `a100885` | chore | Removed the Eleventy docs site (`site/`), `.eleventy.js`, `package.json`, and the Pages workflow; kept `demo.gif`/`screenshot.png` as `assets/`. | n/a | site docs retired |
| `d6b8b53` | refactor | Renamed the crate/binary, config dir, CLAP host id, workspace file, release assets, and UI snapshots from `rusty-amp` to `rusty-riff`; added a one-time `~/.config/rusty-amp` → `~/.config/rusty-riff` migration. | `cargo test --all-features` (snapshot re-blessed) | — |
| `5c0fa6f` | docs | Replaced the README with a rusty-riff version; added `NOTICE` crediting the original project. | n/a | — |
| `26359e2` | docs | Rewrote `CONTRIBUTING.md` as local development notes (no fork/PR/site flow). | n/a | — |
| `3a13c5f` | docs | Updated and renamed `Agents.md` → `AGENTS.md`; dropped the docs-site section and old config paths. | n/a | — |
| `52d3195` | docs | Rewrote `.claude/skills/add-*` without the docs-site steps or PR flow. | n/a | — |
| `f923640` | routing | Bounded audio-thread chain-order read: `try_chain_slots` (≤`CHAIN_READ_ATTEMPTS` tries) plus an audio-owned `last_order` fallback; a writer-only mutex serializes `set_chain_order`; `chain_slots()` is now control-thread-only. `process`/`process_block` use `snapshot_order`. R1 resolved. | `audio_read_never_waits_for_a_stalled_writer`, `concurrent_writers_never_tear_the_order`; existing torn/rapid order tests | `debug_assert` checks a pure permutation, not `sanitize_chain_order(order) == *order`: amp-before-cab is a UI-level constraint and `rapid_reorder` legitimately swaps across that boundary |
| `7784ec8` | routing | One `BlockRoute` snapshot (order, `use_ext_amp`, `skip_cab`, `width`) taken per block/call and threaded through `process_core`/`run_full`/`run_range`/`run_ordered_stage`; the Cab decision is no longer re-read per sample, and `process()` never runs an AU or skips the cab. Deleted `ext_amp_supplies_cab`; fixed the `amp_stage` doc. R2 and R4 resolved. | `route_truth_table`, `per_sample_process_keeps_the_cab_with_a_full_rig_au_flagged`; existing `process_block_matches_per_sample` and AU full-rig/amp-only tests | built-in↔AU switch lands on a block boundary only (no crossfade; documented under "Findings not yet actioned") |
| `1a8284b` | tests | `rig_fingerprint` now includes the active model's used amp knobs, `fz_type`/`delay_type`, and every knob of each enabled stage; `hostile_params` scrambles every knob, the amp banks, and the types. New `bundled_presets_render_identically_after_hostile_state` renders fresh vs hostile through a real `DspChain`. R6 resolved. | `bundled_presets_load_deterministically` (strengthened), `bundled_presets_render_identically_after_hostile_state` | render gate covers a 3-preset representative subset at 48 kHz, not all 17, because `DspChain::new` is ~1.7 s in debug (construction, not sample count, dominates); padding amp slots beyond `controls().len()` are excluded as inaudible. The strengthened fingerprint first flagged a false positive on padding slots, now fixed |
| `5355842` | docs | The `K` help modal now states that stages between AMP and CAB are line-level, and process the already-miked output of a full-rig AU (`post-mic`). R3 resolved. | `snapshot_help_modal` (re-blessed) | the README is intentionally a text-only quickstart, so the caveat lives in the help modal only; the optional `post-mic` ribbon dim was not added (the CAB tile is already dimmed to `AU CAB` when a full-rig AU is active) |
| `604b4b1` | audio | `on_input` now splits an oversized callback into `block_ranges(frames, MAX_BLOCK)` chunks, each processed by a new `on_input_chunk` helper; the `resize`/`extend` allocation path is gone. Level and timeline-position stores happen once per callback. R7 resolved. | `block_ranges_splits_into_bounded_pieces`, `block_ranges_handles_zero_max` | `InputState` is built inline in `build_engine` from live cpal streams, so it could not be constructed in a unit test; per the plan fallback the chunking was extracted into the pure `block_ranges` and unit-tested, and `on_input_chunk` carries a `debug_assert!(frames <= MAX_BLOCK)`. Per-chunk transport/metronome/route snapshots are intentional |
| B1 | analysis | New `src/analysis/` (compiled into the lib, never on the audio thread): `metrics` (BS.1770-4 LUFS, third-octave LTAS, crest, correlation, side/mid, noise floor, window peaks, treble-mod, centroid), `synth` (deterministic Karplus-Strong corpus normalized to −3 dBFS), and `render` (offline `render_preset` + WAV IO). `pub mod analysis` added to `lib.rs`. | BS.1770 calibration at 44.1/48/96 kHz, correlation/crest/centroid, phrase determinism + calibration, render determinism, and finite/≤1.0 render for every bundled preset | BS.1770 K-weighting is implemented from the libebur128 analog-prototype coefficients (not `Biquad::high_shelf`); spectral centroid uses a Goertzel probe bank. No behavioral change to the app |
| B3 | audio | New `src/audio/calibration.rs`: shared lock-free `InputCalibration` (trim dB, measuring, sticky raw-clip/overflow), `TrimState`, and the pure `condition_block` per-sample trim/window loop. `on_input_chunk` applies the smoothed trim to the dry guitar after deinterleave and before the tuner, so the tuner, input meter, dry capture, live chain and take bus all see the calibrated signal; raw clipping is flagged from the pre-trim value. `audio::start`/`build_engine`/`ui::run` gained a calibration parameter, and `AudioEngine::drain_cal_stats` drains the window ring. | `zero_db_is_bit_identical`, `plus_six_db_converges_without_overshoot`, `window_stats_match_a_known_sine`, `clipping_flagged_from_raw_with_negative_trim`, `full_ring_reports_overflow` | 0 dB is the default and bit-identical; the wizard (B4) and persistence (B5) come next |
| B5 | audio | Calibration persistence in `src/audio/calibration.rs`: `REFERENCE_VERSION`, `PickupClass` + `target_peak_dbfs`, `InputIdentity`, `CalibrationEntry`/`CalibrationFile`, and `load`/`save`/`remove` to `~/.config/rusty-riff/input-calibration.toml` (temp-write + rename, malformed ⇒ absent). `AudioEngine::input_identity()` exposes the running input; `ui::run` re-applies the saved trim after every engine start (startup and `O`) and toasts a stale reference version. | `calibration_round_trips_two_identities`, `replacing_one_entry_keeps_the_other`, `remove_drops_only_that_entry`, `malformed_file_is_treated_as_absent` | persistence uses `_at(path, …)` test seams so tests never touch the real config dir; the saved trim is applied just after the engine starts, so it ramps in over `TRIM_SMOOTH_MS` rather than being pre-loaded at construction (the identity is only known after device negotiation) |
| B4 | ui | New `src/ui/calibration.rs`: the `N` wizard (Intro → Noise 2 s → Capture 8 s → Result/Error), pickup selection, live meter + clip lamp, and Save/Retry/Reset/A-B/Cancel. The pure core (`compute_calibration`, `CalResult`, `CalError`, `CalWarning`) plus `PickupClass::{next,prev,label}` live in `src/audio/calibration.rs`. The header now reads `IN +x.x dB` / `IN uncal`, and the `K` help lists `N`. | `calibration_errors_cover_each_branch`, `nominal_humbucker_calibration`, `pickup_targets_shift_the_trim`, `clamping_and_low_snr_warn`; UI snapshots re-blessed | the pure compute lives in the calibration domain module rather than the UI module; the header shows the calibrated trim but not an auto-clearing live `CLIP` lamp (the wizard shows `CLIP`); `N` is refused with a toast while a take is recording |
| B6 | session | Raw takes now record the calibration they were captured under: `session::Track` and `project::TrackSection` gain `input_trim_db`/`calibration_ref` (serde default + `skip_serializing_if`), set in `poll_capture`, written by `save_session`, and restored by `Manifest::into_session`. The timeline shows a dim `uncal` tag on raw takes with no trim. | `track_trim_metadata_round_trips_and_old_manifests_load`; existing `manifest_round_trips_through_a_project_folder` | recovery takes still carry no metadata (left `None`, as designed); "uncalibrated" is detected as `|trim| < 1e-3` because `poll_capture` has no device identity |
| B2 | harness | New `examples/fidelity_render.rs` (hand-rolled CLI: `--presets`, `--synth`/`--di`, `--sr`, `--out`, `--width`, `--ref`, `--abx`, `--bench`, `--write-baseline`, `--check`): renders presets over the synthetic corpus or a DI manifest, writes per-preset WAVs plus `report.toml`/`summary.csv`, does LUFS-matched reference comparisons, ABX files (seeded X + key + notes), a realtime-factor bench, and baseline drift checks. `tests/fidelity_harness.rs` covers the render path. Committed `docs/fidelity/baseline-synth-48k.toml` (17 presets, chugs). | `harness_render_is_deterministic_finite_and_loud`; baseline `--check` OK (17 presets); bench rtf ≈ 0.15–0.18 | the baseline covers the `chugs` phrase only; `--check` tolerances are lufs_i ±0.1, crest ±0.2, correlation ±0.02, centroid ±1 %, each LTAS band ±0.25 dB |
| B8 | calibration | Measured the reference rig and set the engine targets to the measured P99 peaks: **humbucker −19.7 dBFS**, **single-coil −24.6 dBFS**; P90 stays provisional at −4.5 (no P90 guitar). `REFERENCE_VERSION` 1 → 2; `analysis::synth::humbucker_peak()` now derives the corpus peak from `target_peak_dbfs(Humbucker)`; calibration tests derive their expectations from the targets. Baseline regenerated and `--check` passes. Added a maintainer runbook to `CONTRIBUTING.md` ("Reference calibration"). | calibration tests (13); `--check` OK (17 presets); `tests/fidelity_harness.rs` passes at the new level | full re-measure only needed if the **reference rig** changes (new interface/normal gain/re-voicing); a new guitar of an already-measured class just needs `N`; a new pickup class needs `N` and possibly a target bump (documented in the runbook) |
| B7 | docs | README gains an "Input level" section and the `N` key; `CONTRIBUTING.md` gains a "Fidelity harness" section (commands, DI manifest, DI recording, baseline check); `AGENTS.md` lists `src/analysis/`, `src/audio/calibration.rs`, and `examples/fidelity_render.rs`; `docs/fidelity-references.md`'s source-log `Listen/measure` field cites a harness `report.toml`. The `K` help `N` row shipped in B4. | n/a | the README is kept to a quickstart, so the harness details live in `CONTRIBUTING.md` rather than the README |
| bundle | presets | Removed `van_halen_brown_sound`/`van_halen_aint_talkin_bout_love`; added `van_halen_beat_it_solo`, `guns_n_roses_november_rain_solo`, `pink_floyd_mother_solo`, `pink_floyd_have_a_cigar_solo` (inspired-by, uncited; Phase 0 will source them). Updated `RENDER_SUBSET`, test samples, harness examples, the inventories (15 → 17), and regenerated the baseline. | `all_bundled_presets_parse`, `bundled_presets_load_deterministically`; baseline `--check` (17) | the four new presets are inspired-by pending Phase 0; `[master] width` stays at the default pending the period-preset width pass |
| phase0 | docs | `docs/fidelity-references.md` §2 now carries the recording dates/studios/producers/credits per preset group, cited to Wikipedia (secondary) and marked `documented`, plus the Phase-5 contradiction list. Gear remains unsourced (`plausible`/`unknown`, `Source: _TBD_`). | n/a | only secondary sources so far; gear needs primary sourcing |
| phase0-gear | docs | Gear link pass: added **verified secondary** gear sources (gilmourish for Floyd; Guitar World for Eagles/Zeppelin; MusicRadar/Mixdown for Slash; musicradar/Guitar World for Page/EVH) and marked each `plausible`; unverified rows stay `TBD`. **Phase 0 closed (evidence-as-available)** — all presets are *inspired by*. Also recorded **Phase 3 component references** (Marshall 1959 silicon bridge; Hiwatt BYX94 + passive TMB; Twin solid-state + Jensen C12N; Greenback G12M specs) in the Phase 3 section. | n/a | full session-gear sourcing is out of scope; the amp/cab work will rely on component refs + the harness, not session history |
| plexi-rect | amp | The model 1959 Super Lead is silicon-rectified (Unicord 1970 schematic; GZ34 phased out ~1966), so `Plexi::power_amp` was retuned from valve-style sag to a stiff solid-state rail (attack 150→220/s, release 5→6.7/s, sag depth 1.8→1.3, ripple depth 0.05→0.035) and the doc comments corrected; bloom is now attributed to the output transformer/speaker. The stale "tube-rectified Marshall" note in `hiwatt.rs` was fixed. | all `dsp::amp` tests incl. `amps_are_loudness_matched`; harness before/after `--check` | only `lufs_i` moved — **+0.15–0.19 dB** on the six Plexi presets (less supply compression); crest, correlation, centroid and LTAS unchanged. `docs/fidelity/baseline-synth-48k.toml` regenerated in this commit. |
| phase3-audit | docs | Audited Hiwatt/WEM and Twin/Jensen against the component refs: both already match (Hiwatt passive FMV-style TMB + stiff silicon supply; WEM cab = Fane Crescendo; Twin passive Fender stack + solid-state rectifier; Fender cab = Jensen-style open 2×12; Greenback cab consistent with G12M specs). Reworded the Hiwatt doc ("passive TMB", not Baxandall) and the Twin doc (solid-state rectifier in every revision). No voicing changes. Remaining Phase 3 work is measurement-bound (re-amp/mic captures). | n/a (comments/docs only) | the models are *plausible* against refs, not verified against captures |
| cheap-pass | presets | Preset-honesty pass: added `year` metadata to all 17 bundled presets plus a `no_new_device_anachronisms` test (the three known TS-808 cases are allowlisted for Phase 5); set `[master] width = 1.0` on every preset (period guitar is a mono track — R8); hedged the over-claiming descriptions (Stairway small-amp vs Plexi, Hotel California 12-string / two harmonized players, and "the real rig" wording). Regenerated the baseline. | `no_new_device_anachronisms`, `all_bundled_presets_parse`; baseline `--check` (17) | `width = 1.0` lowers `lufs_i` ~0.3–0.7 dB and raises correlation (less side energy) across all presets; removable per preset or live via `W` |
| supro-amp | amp | Added the **Supro-style small combo** (`src/dsp/amp/supro.rs`, `AmpModel::Supro`): a two-knob (Volume/Tone) valve-rectified small American combo, built from the shared blocks (tube rectifier-style sag, small output transformer, small-cab speaker load, passive stack Tone). Wired through `AmpBank`, the `AmpModel` enum/`ALL`/`controls`/cycle, the preset model strings, and the UI counts; amp-modal snapshot re-blessed. | all `dsp::amp` tests (touch-sensitive, bright-cap, loudness-matched) + `ui::` tests | `AMP_MAX` unchanged (2 knobs); the model is an **approximation**, not schematic-exact; no preset uses it yet (the Phase 5 Stairway rebuild is next) |
| stairway-supro | preset | Rebuilt `led_zeppelin_stairway_solo` on the Supro combo through a Fender open 2×12, dropped the anachronistic TS-808 and the compensating pre-EQ/parametric EQ (sparse gate → Supro → cab → slap → room), updated the description; removed its `KNOWN_ANACHRONISMS` entry. | `all_bundled_presets_parse`, `no_new_device_anachronisms`; harness before/after + level-matched A/B | **pending maintainer listening:** brighter, more dynamic and ~3–5 dB quieter than the old Plexi+TS+2-EQ chain; A/B under `target/fidelity/stairway-matched/` (gitignored) |
| spring-tank | effects | Replaced the Twin's Freeverb-derived onboard "spring" with `SpringReverb` (`src/dsp/effects/spring.rs`): a 20-stage first-order-allpass dispersion cascade (lows delayed more than highs → downward chirp) into two unequal damped recirculating spring lines (27/41 ms), HP 180 Hz at both ends, LP 5 kHz, decay-normalized so a long tail does not also mean a loud reverb. `Fender` now uses it in the same slot (post-voice, before the bias tremolo/power amp); the panel knob is the recovery level. | `dsp::effects::spring` (silence, decaying tail, dispersion group-delay, full-decay stability); all `dsp::amp` + full suite (327) | model, not a capture; the wet is additive (no dry attenuation); baseline for `eagles_hotel_california_clean` regenerated in this commit (spring vs old Freeverb: +0.55 dB `lufs_i`, crest/correlation near-unchanged) |
| clean-boost | effects/chain | Added the **Clean Boost** (`src/dsp/effects/clean_boost.rs`, `ChainStage::Boost`): a linear Power-Boost-style front-end gain (Gain 0→+24 dB; Bass/Treble ±12 dB shelves at 120 Hz / 3 kHz) with no clipping, placed as the last pedal before the amp. This is the period-correct replacement for the anachronistic TS-808. New chain slot: `CHAIN_LEN` 20 → 21 and all `ChainStage` ids ≥ Amp shift by one (presets persist by name, so `[chain]` is unaffected). Wired through `Params`/`DspChain`, the preset `[boost]` section, the UI `KNOBS`/`PEDALS` tables (new `PEDAL_MINT` livery), and the header ribbon; `add_pedal_modal` snapshot re-blessed. | `dsp::effects::clean_boost` (knob response, unity/clean at minimum gain); all `dsp::amp` + full suite (329); UI table tripwires | off by default, so default renders are unchanged (baseline not regenerated); model — a single linear gain + two shelves, not the pedal's interactive tone network; no preset uses it yet (the Phase 5 TS-rebuild presets are next) |
| supro-drive | amp/preset | Fixed the Supro's under-scaled drive: preamp `pregain` 12 → 22, power-section drive `*1.4·0.70` → `*1.8·0.66`, and output trim `5.0` → `4.6` to stay loudness-matched (Supro now sits with Marshall/Plexi, spread still 2.42×). The Stairway preset's `gain` 0.78 → 0.88. Now a cranked small combo reaches the thick edge-of-breakup its doc claims, without the removed TS-808. | `amps_are_loudness_matched` (2.42×), `tube_amps_are_touch_sensitive`, `bright_cap_brightens_low_gain_settings`; full suite (329); baseline regenerated + `--check` | **accepted (maintainer listening):** tone signed off. Raw render: +2.5–3.1 dB `lufs_i`, crest −0.4–0.9 dB (more saturation); the model remains an approximation |
| supro-cab | cab/preset | Added the **Supro 1×10 small-combo cab** (`src/dsp/cab/supro.rs`, `CabModel::Supro`, appended last): a single-speaker open-back with a new `CabLayout::Single` (no neighbour cone → the spread stage is an exact passthrough), a small-cone voicing (HP 105, boxy 250 Hz, 1.3 kHz body fill, top LP 5.2 kHz), higher box modes and a ~2.8 kHz cone mode. Wired through `CabBank`/`CabModel`/`preset` cab strings; `cab_modal` snapshot re-blessed; the Supro/Supro pairing added to the base-tone `RIGS`. The Stairway preset's cab switched from the Fender 2×12 stand-in to the Supro 1×10, and `gain` 0.88 → 1.00 (Volume 10). | new plausible/symmetry/modes + stability-sweep coverage; `no_note_is_harsh_or_ice_picky` (a 1318 Hz comb null was filled with a 1.3 kHz body peak), other base-tone bounds; full suite (331) | **approximation, not a capture** (no reference IR; Phase 3 measurement match still blocked). **accepted (maintainer listening):** the small-cone character was signed off |
| perceptual-levels | amp/cab/tests | The per-amp output trims were tuned by `amps_are_loudness_matched` on a **110 Hz-sine raw RMS**, which a bass-heavy voicing inflates — the amps were perceptually up to **14 dB** apart (Mesa/Supro loose, Fender/Hiwatt quiet), so small combos sounded low. Replaced the test with a **mid-band (300 Hz–5 kHz) RMS** on a chug DI, then re-tuned all amp trims to a common mid target. Added a **per-cab level trim** (`BlendedCab::set_level`) because the cab captures were not level-normalized either (mid-band cab spread ~7.5 dB). Result: amp-only mid-band within **1.6×** (Randall 4 dB up — see deviation), cab mid within ~0.1 dB, full-rig spread roughly halved. `examples/rig_loudness.rs` extended to all amps + a per-cab sweep. | new `amps_are_loudness_matched` (mid-band), updated `power_chord_low_end_...` level check; cab stability sweep; full suite (331); baseline regenerated + `--check` | **Randall** is left ~4 dB up: lowering it buries the low-E fundamental against the Orange cab (`fundamental_is_not_buried`), and no bundled preset uses Randall. The **cab** trims flatten the physical 4×12-vs-1×10 loudness difference (a product-loudness choice, not physics). Amp-only trims still can't make *every* amp+cab spectrum identical, so full-rig mid-band spread is ~7 dB, not ~0 |
| phase5-shine-on | preset | Phase 5 Batch 1: rebuilt `pink_floyd_shine_on_crazy_diamond` on the Phase 0 evidence — **Fuzz Face** (`fuzz type` 0 → 0.5; Gilmour's fuzz through 1975, Big Muff from ~1977) + a **Colorsound Power Boost** (`[boost]`, heavily used on WYWH) instead of the anachronistic TS-808, Hiwatt/WEM kept, the two compensating EQs and the Uni-Vibe dropped (Vibe is a *Dark Side* device). Removed its `KNOWN_ANACHRONISMS` entry; description rewritten and the tape echo labelled a Binson-Echorec stand-in. | `no_new_device_anachronisms` (only `eagles_hotel_california_solo` remains), preset parse; full suite (331); baseline regenerated + `--check` | **pending maintainer listening:** level-matched A/B at `target/fidelity/shine-matched/` (gitignored) — the Fuzz Face + dropped EQs make it ~550 Hz darker, `ltas` differs 6.8 dB vs the Muff+TS+2-EQ path. No Echorec model yet, so the echo is still an approximation |
| phase5-acdc | preset/docs | Phase 5 Batch 1: made the AC/DC "no pedals" claim true — removed the compensating pre-EQ, parametric EQ and reverb from `acdc_back_in_black` and `acdc_highway_to_hell`; both are now gate → Plexi → Greenbacks (guitar straight in). Descriptions rewritten; the evidence matrix inventory and description-vs-path flags updated, and the (already-shipped) Stairway and Shine On flags marked resolved. | preset parse, `no_new_device_anachronisms`; full suite (331); baseline regenerated + `--check` | **pending maintainer listening:** A/B at `target/fidelity/acdc-matched/` (gitignored). The change is small (`ltas` 1.4–1.6 dB, centroid ≈ flat) — the Plexi/Marshall carried the tone; the EQs were mostly compensation. AC/DC gear evidence remains `unknown` (no verified source), so the sparse Plexi/Marshall is a labelled approximation |
| tweed-comp | amp/cab | Phase 5 Batch 1b: added the **Tweed Deluxe** amp (`src/dsp/amp/tweed.rs`, `AmpModel::Tweed`, a two-knob Volume/Tone ~15 W cathode-biased 6V6 combo — 5Y3 sag, small output transformer, a single-knob treble-cut tone) and a **Tweed 1×12** open-back cab (`src/dsp/cab/tweed.rs`, `CabModel::Tweed`, `CabLayout::Single`, per-cab level trim). Wired through `AmpBank`/`CabBank`, the `AmpModel`/`CabModel` enums, preset model strings, UI counts (`amp_choices`/`cab_choices`) and the amp/cab modal snapshots. Added `(Tweed, Tweed)` to the base-tone `RIGS`. | `dsp::amp` (all 19, incl. touch-sensitive + new mid-band loudness), `dsp::cab` (39), `dsp::tests` base-tone bounds, full suite (333) | amp + cab are **approximations** (no reference captures). The amp's dynamic bias needed a larger `bloom` coefficient than the Supro's to pass `tube_amps_are_touch_sensitive`; the cab level trim lands it at the common mid target. No preset used them yet (next: `eagles_hotel_california_solo`) |
| phase5-hotel-solo | preset | Phase 5 Batch 1b: rebuilt `eagles_hotel_california_solo` on the new **Tweed Deluxe + 1×12** (Felder: 1959 Les Paul → cranked tweed), guitar straight in — removed the anachronistic TS-808, the compressor (the cranked tweed's own compression carries it) and the two EQs. Description rewritten to name both cited lead rigs. Removed the **last** `KNOWN_ANACHRONISMS` entry (the allowlist is now empty, so any enabled device newer than the preset's year fails). | `no_new_device_anachronisms` (empty allowlist), preset parse; full suite (333); baseline regenerated + `--check` | **pending maintainer listening:** A/B at `target/fidelity/hotel-matched/` (gitignored) — crest **+3.0 dB** (more dynamic, less compressed than the TS+2-EQ path), `ltas` 5.6 dB, centroid ≈ flat. The preset is one lead voice; the outro is two harmonized takes |
| phase5-darkside | preset | Phase 5 Batch 2: dropped the doubling EQ stack from the *Dark Side* trio — `pink_floyd_time_solo` and `pink_floyd_time_chorus` lose both the pre-amp and parametric EQs, `pink_floyd_money` loses the parametric EQ, per the audit ("compare the dry Hiwatt/WEM before enabling two EQs"). The rigs are otherwise already evidence-correct (Fuzz Face on Time/Money, Hiwatt/WEM, Uni-Vibe, tape delay standing in for the Echorec). Matrix inventory updated. | preset parse, `no_new_device_anachronisms`; full suite (333); baseline regenerated + `--check` | **pending maintainer listening:** level-matched A/Bs at `target/fidelity/ds-matched/` (gitignored). Small changes (`ltas` 0.9–1.6 dB, centroid −151…+77 Hz) — the EQs were gentle compensation. No Binson model, so the echo stays a tape approximation |
| phase5-wall | preset | Phase 5 Batch 3: dropped the doubling EQ stack from the five *Wall*/WYWH Floyd presets (`comfortably_numb_solo_1/2`, `another_brick_pt2`, `mother_solo`, `have_a_cigar_solo`) — both the pre-amp and parametric EQs off. Rigs are otherwise unchanged (Big Muff for 1979, Hiwatt/WEM, Mistress/Phase/echo already labelled as approximations). Matrix inventory updated. | preset parse, `no_new_device_anachronisms`; full suite (333); baseline regenerated + `--check` | **pending maintainer listening:** level-matched A/Bs at `target/fidelity/wall-matched/` (gitignored). Small changes (`ltas` 1.3–1.6 dB, centroid −90…−301 Hz). Named hardware (Echorec/Mistress/Phase 90) remains an approximation |

### Reference rig (B8)

- **Interface:** Focusrite Scarlett Solo, 4th gen — **INST on** (correct high-Z
  instrument input for a passive guitar).
- **Gain position:** 9 o'clock.
- **Guitar:** Donner DST-152.
  - Humbucker: bridge, coil-split **off** → measured P99 **−19.7 dBFS** (noise
    floor −88.4, SNR ≈ 69 dB).
  - Single-coil: **neck** (the guitar has no bridge single-coil) → measured P99
    **−24.6 dBFS** (noise floor −77.9, SNR ≈ 53 dB).
- No P90 guitar was available, so the P90 target stays provisional.

Because the presets were voiced uncalibrated on this rig, their effective input
level is ≈ −20 dBFS; the trim on this rig is therefore ≈ 0 dB and other
interfaces normalize to it. (This is why several presets carry a lot of
gain/boost — a Phase 5 cleanup target.)

### Workstream A close-out

Gate met:

- **No unbounded wait or allocation reachable from the audio callback.** The
  order read is bounded (`try_chain_slots`, A1) and an oversized `on_input`
  callback is chunked into preallocated pieces (A5).
- **`process()` and `process_block()` agree on routing.** Both take one
  `BlockRoute` per block/call; the per-sample path intentionally never runs a
  hosted AU and never skips the cab (A2).
- **Bundled presets are render-deterministic.** The strengthened fingerprint
  covers all 17 presets cheaply; fresh-vs-hostile audio is compared through a real
  `DspChain` for a representative subset (A3).

Review findings: R1–R10 resolved (R5 in the earlier doc reorganization; R9–R10
in Workstream B); R8 is a deferred per-preset decision.

### Workstream B close-out

Code and docs met the gate:

- **0 dB is bit-identical.** The trim defaults to 0 dB and `zero_db_is_bit_identical`
  pins `x * 1.0 == x`; the signal only changes once calibrated.
- **The harness is deterministic** and `--check` passes against the committed
  `docs/fidelity/baseline-synth-48k.toml`.
- **The callback gained no allocation, lock, or unbounded loop.** Calibration is
  an `rtrb` ring plus atomics (B3); A1/A5 already bound the order read and chunk
  oversized callbacks.
- **Calibrated takes record their trim** (B6), so a take re-amps/exports
  identically and the level is auditable.

**B8 done (human):** the reference rig was measured and the targets were set to
the measured values (humbucker −19.7, single-coil −24.6; P90 provisional),
`REFERENCE_VERSION` bumped to 2, and the baseline regenerated. See the B8
increment row and "Reference rig (B8)" above; the maintainer runbook is
`CONTRIBUTING.md` → "Reference calibration (maintainers)".

---

## Open gaps for a following agent

The roadmap's reference-dependent phases were skipped. Each needs external
material that cannot be invented from code:

### Phase 0 — reference matrix

**Closed (evidence-as-available).**
[`docs/fidelity-references.md`](docs/fidelity-references.md) records **recording
dates, studios, producers and guitar credits** for every preset group (Wikipedia,
secondary, marked `documented`) **plus verified secondary gear sources where
found** (gilmourish for Floyd; Guitar World for Eagles/Zeppelin; MusicRadar/
Mixdown for Slash; musicradar/Guitar World for Page and EVH). Sourcing every
session gear detail is explicitly **out of scope**, so all presets are *inspired
by*, not exact rigs. The per-preset contradiction list is the Phase 5 worklist.
Unverified gear rows stay `Source: _TBD_`.

### Phase 3 — component references

Priority order from shipped presets: Hiwatt DR103 + WEM/Fane, Marshall
Super Lead/Plexi + Greenback 4×12, Fender Twin + Jensen 2×12. Verified references
gathered for the models (secondary; sufficient for a revision/rectifier check, not
for a measured-IR match):

- **Marshall 1959 Super Lead — silicon rectifier.** Unicord 1970 schematic shows a
  solid-state bridge (`4 X DIODE`), and drtube states the GZ34 was phased out
  (~1966) in favour of solid-state rectifiers. So the Plexi model's tube-rectifier
  sag is **not historically right** for late-60s/70s Super Leads.
  <https://www.drtube.com/schematics/marshall/1959u.gif>,
  <https://www.drtube.com/marshall.htm>
- **Hiwatt DR103 — silicon rectifier + passive TMB.** PSU shows a `4 BYX94`
  silicon bridge (`http://hiwatt.org/tech.html`: "meant for a full wave bridge");
  the tone stack is a passive TMB ("TONE CONTROLS TREBLE MIDDLE BASS PRESENCE"),
  **not** an active Baxandall. <https://www.drtube.com/schematics/hiwatt/hwpsu1.gif>,
  <http://hiwatt.org/tech2.html>
- **Fender Twin Reverb — silicon rectifier + Jensen.** AA769 schematic; Wikipedia:
  "All Twin Reverbs feature a solid-state rectifier"; stock speakers include
  **Jensen C12N** (reissue uses C-12K).
  <https://schematicheaven.net/fenderamps/twin_reverb_aa769_schem.pdf>,
  <https://en.wikipedia.org/wiki/Fender_Twin>, <https://www.jensentone.com/vintage-ceramic/c12k>
- **Celestion Greenback G12M:** 25 W, ceramic, 35 oz magnet, 98 dB, 75–5000 Hz,
  Fs 75 Hz. <https://celestion.com/product/g12m-greenback/>

Still needed for a measured match: re-amp captures and compatible mic/IR captures.

### Phase 3 — audit results (2026-09-25)

Checked the models against the references above:

- **Marshall Plexi — corrected.** The valve-rectifier sag was wrong; `Plexi::power_amp`
  now uses a stiff solid-state rail (see the `plexi-rect` increment). Measured delta:
  only `lufs_i`, +0.15–0.19 dB on the six Plexi presets.
- **Hiwatt DR103 — already aligned.** The model already uses `ToneStack` with
  `Components::HIWATT` (a passive FMV-style TMB, not an active Baxandall) and a
  stiff, silicon-rectified supply (`sag` 0.45, ~90 ms release). The doc was reworded
  to match the schematic ("passive TMB", not "Baxandall"). The WEM cab is already
  modelled as **Fane Crescendo** (bright, efficient upper-mids, leaner low end) —
  matching the cited WEM Super Starfinder + Fane rig. No voicing change.
- **Fender Twin — already aligned.** The model uses `Components::FENDER` (passive
  Fender stack) with a stiff supply (solid-state rectified in every Twin revision);
  the doc now states this explicitly. The Fender cab is an open-back 2×12 with a
  Jensen-style ceramic voicing (sub-HP 82 Hz, presence 2.5–4 kHz, rolloff ~7–8 kHz)
  — consistent with the stock Jensen C12N. No voicing change.
- **Greenback cab — consistent.** The Marshall cab holds level through 3–5 kHz and
  rolls off at 6.6–7 kHz, consistent with the G12M's 75–5000 Hz rated range plus a
  deliberate fizz cut. No change without a measured capture.

**Remaining Phase 3 work is measurement-bound:** matching magnitude/phase/decay and
control sweeps needs re-amp captures and mic/IR captures, which we do not have.
Until then the amp/cab models are *plausible*, not verified against captures.

### Phase 4 — named pedal/echo/reverb circuits

Needs circuit data + measured sweeps for: Muff vs Fuzz Face vs Tone Bender
(`src/dsp/effects/fuzz.rs`), TS-808 high-pass (above), Dyna Comp vs the generic
compressor, manual-wah vs `wah.rs` auto-wah (needs an expression input), Phase
90 / MXR flanger / Electric Mistress / Uni-Vibe modulation trajectories, and a
real Binson Echorec model if evidence calls for it.

**Done:** the Twin's onboard Freeverb-derived "spring" was replaced with a
dedicated mono spring-tank model (`src/dsp/effects/spring.rs`,
`SpringReverb`): a dispersive allpass cascade (lows delayed more than highs, so
a transient chirps downward) feeding two damped, unequal recirculating spring
lines, high-passed at both ends and low-passed at 5 kHz. It keeps the Twin's
original placement (post-voice, before the bias tremolo and power amp). Model,
not a capture.

### Phase 5 — rebuild bundled presets

Use the Phase 0 matrix; start sparse and add only source-backed effects.
Preset descriptions claiming a session rig must be updated to match
the enabled signal path in the same change (now in the README and the preset
`description` fields).

No historical/session claim in this repository should be treated as verified by
the Phase 1–2 work here; only the routing, topology, and documentation defects
above were addressed.

---

## How to verify this work

```bash
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
```

All tests pass (262 at the time of writing). The routing/topology fixes are
covered by the named tests in `src/dsp/mod.rs`, `src/preset.rs`, and
`src/ui/input.rs`. There is no listening test: Phases 1–2 change routing,
coherence, and topology, not voicing, and the default-order render is
bit-identical to before the split.
