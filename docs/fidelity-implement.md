# Fidelity — implementation notes (as-built / handover)

Companion to [`fidelity-plan.md`](fidelity-plan.md) (formerly `plan.md`). The
plan is the **design / acceptance** document; this file is the **as-built**
record: what shipped, review findings, invariants, deviations, and what is still
missing. Evidence for historical gear claims lives in
[`docs/fidelity-references.md`](fidelity-references.md). Read all three
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
| Phase 2 item 5 — real preamp/loop/power-amp split | **Closed — out of scope** (the plan marks it *optional*; see "Findings closed") | plan Phase 2.5 |
| Phase 0 — reference matrix | **Done** (evidence-as-available) | `docs/fidelity-references.md` |
| Phase 0 — offline harness, CPU/latency capture | **Done** (B1, B2) | plan "Next increments" |
| Workstream A — routing hardening (review findings) | **Done** (A1–A5) | increment log |
| Workstream B — input calibration + harness | **Done** (B1–B8) | increment log |
| Phase 3 — amp/cab fidelity | **Plexi rectifier + Hiwatt/Twin audit done**; measured-IR match **blocked** (no re-amp/mic captures) | plan Phase 3 |
| Phase 4 — named pedal/echo/reverb behavior | **Done (models, not captures)**: spring tank, Clean Boost, Supro/Tweed cabs, fuzz-family split + guitar cleanup, Binson Echorec, Phase 90 / Electric Mistress, **TS-808 circuit corrected**, manual-wah expression all in | plan Phase 4 |
| Phase 5 — rebuild the bundled presets | **All 4 batches done** (17 presets audited/rebuilt; **pending maintainer listening**) | increment log |

The Workstream A/B work below changed routing, topology, and documentation only.
Later commits (Phase 3–5 in the increment log) do change voicing; each logs its
before/after evidence.

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

- **Reference scaffold** — new [`docs/fidelity-references.md`](fidelity-references.md):
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

## Findings closed

### TS-808 input-HP — **resolved (circuit-corrected)**

The model used a phantom **340 Hz input coupling HP** (`0.047 µF` into `10 kΩ`).
Per [R.G. Keen, *The Technology of the Tube Screamer*](http://www.geofex.com/Article_Folders/TStech/tsxtech.htm)
(1998; component values match the Ibanez schematic) the real circuit has **no such
high-pass**: the input buffer couples through a **1 µF** cap into the ~10 kΩ bias
network (a few Hz), and the **720 Hz** corner is the *clipping stage's gain
rolloff* — `Zi = 4.7 kΩ + 0.047 µF` to AC ground, so `gain = 1 + Zf/Zi` falls to
unity below 720 Hz. The bass therefore passes **clean and at unity**; only the
mids/highs are boosted and clipped. `src/dsp/effects/tube_screamer.rs` was
rewritten to match: the input HP is gone, the drive gain is applied as a **720 Hz
high-shelf** (unity bass → `1 + (51 kΩ + Drive)/4.7 kΩ` above), and the clipper
is **symmetric** (two anti-parallel silicon diodes — the earlier asymmetric pair
is the SD-1-style mod, not the stock pedal). No bundled preset enables the TS, so
the render baseline is unchanged. `bass_passes_at_unity_while_treble_is_boosted`
pins the corrected topology.

### Genuine amp effects loop — **closed, out of scope**

Phase 2 split `Amp` and `Cab`; the amp DSP is still one block, so an effect
between them models a *virtual load-box / post-power-amp line-level* path, not
the amp's internal loop. The plan calls a real `preamp → loop → power-amp` split
**optional** (Phase 2 item 5) and "a distinct piece of work". Implementing it
means splitting all nine amp models at the tone-stack→power-amp boundary and
threading a loop stage plus preset schema through the chain — a large,
voicing-risky change that no bundled preset requires. **Decision: not
implemented**; the documented virtual-load-box semantics stand. The design is
retained in `fidelity-plan.md` Phase 2 item 5 if it is ever picked up.

### Built-in ↔ AU toggle — **resolved (clickless declick)**

A2 keeps the switch on a block boundary but did not fade, so toggling could step
the waveform. `DspChain::process_block` now **fades the output through zero** on
the currently-sounding path before the route flips, then fades back up
(`DECLICK_SECS = 4 ms`, per-sample ramp), so the change is never heard as a click.
A dual-path *equal-power* crossfade is deliberately **not** used: the two paths
share the pre/post-amp stage state, so running both would advance those filters
twice. Loading a plugin up front adopts the current state with no fade (only live
`amp_external_active` toggles ramp). `builtin_au_toggle_fades_through_zero` covers
it; with no AU loaded the path stays bit-identical (no multiply).

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
| phase5-misc | preset | Phase 5 Batch 4: `van_halen_beat_it_solo` and `guns_n_roses_november_rain_solo` were already sparse and evidence-aligned (Plexi/JCM800 + Marshall, phase/slapback or wide delay, no EQs) — no audio change. Tightened the descriptions: EVH's solo is double-tracked (a single preset can't reproduce it) and Slash's wide delay/reverb is the record's mix as much as the rig. | preset parse; no render/baseline change | Description-only; audio and baseline untouched |
| phase4-echorec | effects/preset | Phase 4 item 4: added a **Binson Echorec** delay mode at `type = 0.5` (multi-head drum echo — sums three fixed playback-head taps into a repeat cluster, recirculates the cluster, band-limits with a 2.4 kHz LP + 90 Hz HP and gentle saturation, mono-on-channel, reused wow/flutter). Backward-compatible selection: `0.0` digital and `1.0` tape are unchanged (the `0.5` midpoint was an unused "tape" value). Wired all six Echorec-labelled Floyd presets to `0.5` and rewrote their echo comments; the matrix echo mapping and flag updated. | new `dsp::effects::delay` cluster/mono tests + backward-compat dry test; full suite (335); baseline regenerated + `--check` | **pending maintainer listening:** level-matched A/Bs at `target/fidelity/echorec-matched2/` (gitignored). Distinct from tape (`ltas` 1.9–3.6 dB, crest −0.4…−1.4 dB) but **brighter** than tape by +30…+420 Hz centroid — the multi-tap cluster adds early repeats; can be darkened further (lower LP) if it reads wrong. Model, not a capture |
| phase4-mod | effects/preset | Phase 4 item 3: added a **TYPE** knob to the phaser and flanger and wired the presets that name hardware. **Phase 90** (`phaser type = 1.0`) collapses to mono and drops the quarter-cycle stereo offset, removes regeneration (script — `feedback` is inert) and widens the sweep (130–2200 Hz); `another_brick_pt2` now uses it. **Electric Mistress** (`flanger type = 1.0`) collapses to mono, shortens the throw (0.4–3 ms) and adds the **Filter Matrix** (DEPTH → 0 freezes the sweep into a static comb); `comfortably_numb_solo_1/2` now use it. New `ph_type`/`fl_type` params, preset `[phaser]`/`[flanger] type` fields, UI `KNOBS` (now 81) + ranges; `format`/presets wired. Matrix flags resolved. | new mono/script/sweep-filter-matrix tests; `dsp::effects` (93); full suite (339); baseline regenerated + `--check` | **pending maintainer listening:** level-matched A/Bs at `target/fidelity/mod-matched/` (gitignored) — `ltas` 3.2–4.5 dB; the Phase 90 is darker (no feedback resonance), the Mistress brighter/more compressed (mono). Device modes are approximations, not measured circuit clones |
| phase4-correctness | effects/preset/docs | Phase 4 item 2 (small correctness): (a) the TS-808 code/doc mismatch was already reconciled — the prose and constructor both say a **~340 Hz** input coupling HP; added `input_coupling_cuts_sub_bass` to pin the passband, and marked the stale plan finding resolved. (b) **truth-in-labelling**: the two preset comments that named a "Dyna Comp" now say "Dyna-Comp-style in character (the DSP is a generic peak-follower)" / "the compressor's squash", so the generic compressor isn't passed off as a modelled circuit. | `input_coupling_cuts_sub_bass`; full suite (340); no audio/baseline change | A real Dyna-Comp voicing (OTA compressor) is **not** built — logged as future work; the compressor remains a generic peak-follower |
| phase4-fuzz-guitar | effects | Phase 4 item 1: added a **GUITAR** knob to the fuzz (guitar volume as the pedal sees it, default 1.0) and a Fuzz Face **cleanup/loading** model: the input is scaled by the guitar volume, the FF gain is biased down with it (`gain × gv²`) and a volume-dependent input HP (70 Hz → up to 1.2 kHz) thins the lows as the pot closes — so rolling the guitar back cleans the fuzz up. Bypassed at full volume, so existing presets are bit-identical. New `fz_guitar` param, `[fuzz] guitar` field, UI `KNOBS` (now 82). | new `fuzz_face_cleans_up_as_guitar_volume_falls` (THD falls as the knob rolls back); `dsp::effects` (95); full suite (341); baseline `--check` OK | Model, not a measured pedal: the pickup/source impedance is assumed, and the load is a fixed HP rather than a true reactive pickup model. The Muff/Tone Bender don't clean up (correct — high input Z); the knob affects level for all voices |
| ts808-circuit | effects/docs | Phase 4 item 2: corrected the TS-808 to the documented circuit (R.G. Keen, *The Technology of the Tube Screamer*). Removed the phantom **340 Hz input-HP**; the input coupling is a **1 µF** cap (~16 Hz). Replaced the fixed **720 Hz mid-peak** with the real mechanism — a **720 Hz high-shelf** applying the clipping-stage gain `1 + (51 kΩ + Drive·500 kΩ)/4.7 kΩ` (bass at unity, mids/highs boosted then clipped). Made the clipper **symmetric** (two anti-parallel silicon diodes; the old asymmetric pair is the SD-1 mod). Added `Biquad::set_high_shelf` (in-place, click-free knob moves). | rewritten `tube_screamer` tests incl. `bass_passes_at_unity_while_treble_is_boosted`, `clip_is_symmetric`, `finite_and_bounded_across_controls`; full suite (386); baseline `--check` OK (no preset enables the TS) | Model, not a capture; the absolute pickup→diode gain staging and the 51 pF feedback rolloff remain approximations. `fidelity-implement.md` "Findings closed" updated; the `340 Hz` note and `input_coupling_cuts_sub_bass` test removed. |
| au-declick | dsp/docs | Closed the last deferred routing finding: a live built-in↔AU toggle now **fades through zero** (4 ms per-sample ramp) on the current path before the `BlockRoute` flips, then fades back in — no click. Loading/clearing a plugin up front adopts the current state without a fade (only a live `amp_external_active` change ramps). No AU loaded ⇒ no multiply and the path stays bit-identical. | new `builtin_au_toggle_fades_through_zero`; existing AU routing tests (`route_truth_table`, `amp_only_…`, `per_sample_process_keeps_the_cab…`) unchanged and green; full suite (386) | A dual-path equal-power crossfade is not used (shared pre/post-amp stage state would double-advance); documented in "Findings closed". |

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
[`docs/fidelity-references.md`](fidelity-references.md) records **recording
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

---

## Phase 6 — live-safe tone (Phase A: IR-first, low-latency-first)

Built on top of Phases 0–5. Constraint from the maintainer: gigging
low-latency first (128 frames @48 kHz, RTF < 0.5), studio second. Voicing
changes are intentional; the committed `docs/fidelity/baseline-synth-48k.toml`
is NOT regenerated until maintainer listening approves. Candidate baseline at
`target/fidelity/phaseA-candidate-baseline.toml` (gitignored).

### A1 — external IR first-class (`fix(dsp): external IR live trim + offline tails`)

- `src/dsp/cab/external.rs`: added `LIVE_MAX_IR_LEN` (8192 ≈ 170 ms @48 kHz,
  the direct + early-room window inside the 128-frame gigging budget) and
  `OFFLINE_MAX_IR_LEN` (32768, full tails for export). `MAX_IR_LEN` kept as the
  live alias. `load_ir` unchanged otherwise (offline resample + tail fade +
  DC-remove + unit-energy normalize, all off-audio-thread).
- `ExternalIrCab` now runs `SpeakerDrive → conv → MicPosition post-EQ →
  mic_sat × level`. `blend`/`room` stay inert (baked into the capture);
  `mic_pos` is a live edge↔centre trim instead of a dead knob. Added
  `set_level` trim.
- `src/export.rs` loads with `OFFLINE_MAX_IR_LEN`; `src/ui/ir_browser.rs` and
  `src/ui/practice.rs` load with `LIVE_MAX_IR_LEN`.
- `irs/README.md`: 4-rig starter set (Marshall Greenback, WEM Fane, Fender
  Jensen, Mesa V30), live-vs-offline table, open-license guidance.
- Tests: `offline_cap_exceeds_live_cap`,
  `cab_is_finite_bounded_blend_room_inert_mic_pos_live` (blend/room inert,
  mic_pos centre > edge at 6 kHz).

### A2 — power-stage 8× oversampling (`fix(dsp): oversample the power clipper`)

- `src/dsp/amp/mod.rs`: new shared `PowerOs` (independent `Oversampler8`;
  sag/ripple envelope held per base-rate sample, only the memoryless clip runs
  hot; series group delay ≈ M base samples, inside budget).
- Migrated `marshall.rs`, `plexi.rs`, `hiwatt.rs` (14/17 bundled presets):
  envelope → supply gain at base rate, `tube_clip_asym` at 8×. OT kept at base
  rate (LF-only tanh, minimal alias — full-OT oversampling deferred to Phase B).
- Measured: `--bench` RTF 0.06–0.08 @48 kHz (budget 0.5); `--check` drift is
  expected and directional (e.g. `pink_floyd_time_solo` centroid 3520 → 3372 Hz,
  less alias fizz; 2.2 kHz pocket −7.2 → −9.5 dB, less harsh).
- Remaining 6 models keep base-rate power clips; migrate after listening.

### A3 — excursion-aware speaker load (`fix(dsp): displacement bloom in SpeakerLoad`)

- `src/dsp/amp/mod.rs` `SpeakerLoad`: added 100 Hz displacement estimator +
  8 ms / 90 ms excursion envelope feeding up to +0.35 resonance amount on top
  of the sag term. Palm-mute lows bloom then recover instead of hanging.
  Local to the amp (no amp↔cab feedback loop), ~10 lines of state.

### A4 — reverb predelay + breathing (`fix(dsp): 15 ms predelay + 0.5 Hz wet LFO`)

- `src/dsp/effects/reverb.rs`: fixed 15 ms (guitar-plate) predelay ring on the
  mono-summed input + ±6% opposite-phase 0.5 Hz breathing on wet L/R. No new
  knobs, no extra latency beyond the musical predelay, existing
  `fully_dry_is_passthrough` / `produces_decaying_stereo_tail` tests still pass.
- Note: predelay audibly moves LTAS on wet presets (e.g. `hotel_clean`); this
  is the intended de-masking of pick attack, confirm by ear.

### Verification

- `cargo test --release --lib`: 400 passed (was 399; +1 new IR test).
- `cargo clippy --release --lib`: clean.
- `--bench` (2 presets): RTF ≤ 0.08 @48 kHz, ≤ 0.20 @96 kHz.
- `--check` vs committed baseline: 54 violations on 3-preset subset — expected
  voicing drift (documented above), NOT a regression gate until listening.
- Candidate baseline: `target/fidelity/phaseA-candidate-baseline.toml`
  (17 presets × 7 DIs @48 kHz). Promote to `docs/fidelity/` only after
  maintainer A/B on `acdc_back_in_black`, `pink_floyd_time_solo`,
  `hotel_california_clean`.

### Phase A promoted + A2 completed for all amps (maintainer-approved)

- `docs/fidelity/baseline-synth-48k.toml` regenerated and `--check` green.
- `PowerOs` migrated to the remaining models: `mesa` (silicon clip), `vox`,
  `fender`, `supro`, `tweed` (tube clips). The Randall has no tube power clip;
  its base-rate rail tanh `(x·1.85).tanh()` is now wrapped in `PowerOs` too, so
  all 9 amps are alias-suppressed end to end.
- Randall trim stays `1.10`: lowering it to join the amp-only loudness cluster
  starved the cab drive point (SpeakerDrive breakup + mic saturation) and
  buried E2's fundamental (`funDom 0.191 < 0.20`) while breaking the DS-chain
  level match (1.64× > 1.6×). Both failures reverted with the trim. The +4 dB
  exception is load-bearing — re-tuning it means re-tuning the cab.

### Phase B finding — push-pull deferred to Phase C (needs a re-voice pass)

Tried: long-tail PI + differential push-pull pair at 8× in the power stage.
Two designs failed the voicing contract, and the failure is instructive:

1. Asymmetric differential (0.90–0.97 second side + bias offsets) *cancels*
   incoming even harmonics (~10× attenuation measured on the full Marshall
   rig: hard-drive h2/h1 0.021 → 0.005) and breaks
   `tube_amps_are_touch_sensitive`.
2. Odd-symmetric pair (even order cancels in the transformer, as a real
   class-AB stage does) preserves levels perfectly but still fails Marshall
   touch for the same reason: at this operating point the test's h2 growth
   comes from the *power knee*, and a symmetric power stage cannot generate
   it — physically correct, but the whole rig (drives, trims, tests,
   presets, baseline) is tuned as a system around single-ended power curves.

Verdict: keep the single-ended power curves (now all 8× oversampled). A real
PI/push-pull needs per-amp drive/trim re-voicing with listening ears —
tracked as Phase C, not attempted blind. The `PowerOs` helper stays as the
landing point for it.

### Phase B shipped — 3 new presets close the rig-coverage gap

The bank was Floyd-heavy (9× hiwatt+wem) with no Mesa, Vox, or JCM800 tones:

- `mesa_modern_metal.toml` — TS tightener → Recto Modern → Mesa 4×12, dry.
- `vox_chime_clean.toml` — Top Boost edge-of-breakup → Vox 2×12 + room.
- `marshall_hard_rock_rhythm.toml` — hot JCM800 → Marshall 4×12, dry.
- `tests/bundled_presets.rs` count 17 → 20; harness levels verified sane
  (peaks < 0.8, family-consistent LUFS); baseline regenerated for 20 presets,
  `--check` green.
- `irs/README.md` maps the 4-rig starter IR set onto these presets.

### Verification (final)

- `cargo test --release --lib`: 400 passed; `bundled_presets`: pass (20).
- `cargo clippy --release --lib`: clean.
- `--bench`: RTF ≤ 0.082 @48 kHz, ≤ 0.21 @96 kHz (budget 0.5) — the two extra
  8× round trips per sample cost ~0.02 RTF total.

---

## Phase 7 — the preset knobs were compensating model bugs (maintainer-driven)

The maintainer reported two bundled presets running hot/muddy live
(`comfortably_numb_solo_2`, `time_solo`) and, crucially, doubted that Gilmour
recorded those solos with bass/mid near zero. He was right: the workaround
proved a model bug. With his live-matched corrections as ground truth, the
audit found ~+10–14 dB of *structural* (non-knob) low buildup at 100–150 Hz:

| Stage | Contribution | File |
|---|---|---|
| Hiwatt `VoiceBalance` body | hard +4.0 dB @160 Hz, *after* the tone stack — no knob can remove it | `amp/hiwatt.rs:154` |
| WEM SM57 skeleton | +4 shelf@120, +5 hump@112, +3.6@210, +4.2@500 | `cab/wem.rs:169-173` |
| WEM ribbon blend | +5 @105 Hz and shelves on top | `cab/wem.rs:193-196` |
| SpeakerLoad resonance + excursion | dynamic, up to +0.35 on 0.04–0.09 bases | `amp/mod.rs` |

…against docstrings claiming a *flat* Hiwatt and a *lean* Fane. The FMV
`Components::HIWATT` values themselves are schematic-correct — the pile-up is
all in the voicing layers around them.

### What shipped

- **Model lean (all documented at the edit sites):** WEM SM57 lows
  +4/+5/+3.6/+4.2 → +1.5/+2.5/+2/+2.5 dB, ribbon +2.2/+5/+4.5 → +1/+2.5/+2.5
  with room shelf +2.6 → +1.2; Hiwatt body lift +4.0 → +1.5 dB; excursion cap
  +0.35 → +0.10 with release 90 → 50 ms (sits after every control, so it must
  stay feel, not loudness).
- **Joint re-voice, nulled against the maintainer's live matches.** His
  `~/.config` saves (outro: fuzz 0.64→0.39, level 0.55→0.25, bass 0.42→0.10,
  mid 0.60→0.15, treble →0.65, master 0.58→0.40; time: level 0.55→0.40, bass
  →0.12, mid →0.20, master 0.55→0.30) were baked in, rendered as ground truth
  (`target/fidelity/phase7-ground-truth`, gitignored), then the model was
  leaned and knobs walked back toward noon over 3 rounds (R1 overshot +4.5 dB
  @88 Hz — the FMV bass is potent). Final null on `chugs`: outro LUFS −0.01 dB
  / centroid −0.4% / residual ±2 dB narrow-shape; time LUFS −0.05 dB /
  centroid −0.1% / 3 bands marginally >1 dB. Final knobs: outro
  bass 0.20 / mid 0.28, time bass 0.25 / mid 0.33 — near noon again, as on the
  real rig. Chasing past ±2 dB on normalized synth DI would overfit; the
  maintainer's rig is the real gate.
- **Deliberately unchanged:** fuzz/drive staging (player's choice), Hiwatt
  pre/stage HPs, OT, NFB presence, FMV components, all other presets' knobs
  (siblings `shine_on`/`another_brick`/`money`/`whole_lotta`/solo 1 keep their
  values pending their own live matches — no blind copying).
- Baseline regenerated (20 presets), `--check` green.

### Verification

- `cargo fmt --check`, `clippy --all-targets --all-features -D warnings`: clean.
- `cargo test --release --lib`: 400 passed; `bundled_presets` (20): pass.
- `--bench`: RTF ≤ 0.080 @48 kHz; `bench` example still ~4% of the 10 ms
  callback budget — the lean changes nothing measurable in CPU.
- Open: maintainer listening on his rig against the isolated tracks; LIMIT
  indicator + hot-DI probe + control-authority test still proposed (not built).

---

## Phase 7 follow-up — re-voice the remaining Hiwatt/WEM presets

Phase 7 leaned the Hiwatt/WEM model and re-voiced only `numb_solo_2` and
`time_solo`; the other 7 Hiwatt+WEM presets kept their pre-Phase-7 knob banks and
so render leaner/brighter than the anchors (the Phase 7 table above). This
follow-up walks each sibling toward the appropriate anchor's *delta pattern* by
dirt character — not a blind copy — in maintainer-listened batches. No model,
routing or schema change: presets only.

### Batch 1 — Big Muff family (anchor: `numb_solo_2`)

- `pink_floyd_another_brick_pt2`: fuzz 0.62→0.39, level 0.56→0.25; amp bass
  0.42→0.20, mid 0.58→0.28, master 0.56→0.40, treble 0.60→0.65.
- `pink_floyd_comfortably_numb_solo_1`: fuzz level 0.50→0.30 (kept fuzz 0.40 /
  tone 0.68 — the brighter, lower-gain first solo); amp bass 0.42→0.20, mid
  0.60→0.30, master 0.58→0.42.
- Baseline moves: `another_brick` LUFS −12.52→−14.09, centroid 3269→3532 Hz;
  `numb_solo_1` LUFS −13.64→−14.94, centroid 3182→3474 Hz — both now sit near
  the anchors' centroid rather than the pre-lean voicing.
- Baseline regenerated (20 presets), `--check` green. Maintainer listened and
  approved before commit.

### Batch 2 — Fuzz Face family (anchor: `time_solo`)

- `pink_floyd_money`: fuzz 0.60→0.55, level 0.55→0.40 (tone kept); amp bass
  0.42→0.25, mid 0.58→0.33, master 0.56→0.32. Wah untouched.
- `pink_floyd_shine_on_crazy_diamond`: fuzz 0.66→0.55, level 0.58→0.42 (tone
  kept); amp bass 0.42→0.25, mid 0.58→0.33, master 0.56→0.32. Boost untouched.
- Baseline moves: `money` LUFS −14.35→−18.16, centroid 1771→1901 Hz;
  `shine_on` LUFS −12.59→−14.24, centroid 3118→3399 Hz. `money` drops further
  than the other siblings (no compressor in that preset, so the amp-master cut
  lands directly) — flagged for a possible master bump if it reads quiet live;
  maintainer listened and approved as-is.
- Baseline regenerated (20 presets), `--check` green.

### Batch 3 — no/disabled-fuzz family (amp-only lean)

No dirt here, so only the amp lows/mids move; the fuzz blocks are already off or
absent and were left alone.

- `pink_floyd_have_a_cigar_solo`: amp bass 0.42→0.26, mid 0.60→0.34,
  master 0.55→0.38.
- `pink_floyd_mother_solo`: amp bass 0.42→0.26, mid 0.60→0.34, master 0.55→0.38.
- `pink_floyd_time_chorus`: amp bass 0.40→0.28, mid 0.54→0.38; **master kept at
  0.62** — it is a clean rhythm part, not a driven lead, so it stays level.
- Baseline moves: `have_a_cigar` LUFS −13.02→−14.35, centroid 3571→3708 Hz;
  `mother_solo` −13.06→−14.38, centroid 3510→3647 Hz; `time_chorus`
  −14.05→−14.13, centroid 3711→3760 Hz (largely unchanged, as intended).
- Baseline regenerated (20 presets), `--check` green. Maintainer listened and
  approved before commit.

**Follow-up complete:** all 7 remaining Hiwatt+WEM presets now use the same
leaned model as the Phase 7 anchors. Remaining note from Batch 2: `money` reads
~3–4 dB below the sibling cluster and may want its `master` nudged back up later.

---

## TS-808 — first-order RC shelf and 51 pF feedback pole (maintainer-driven)

The Phase 4 TS correction modelled the clipping-stage gain shape as a 2nd-order
RBJ **high-shelf** and omitted the feedback capacitor, which made the pedal read
thin: everything above 720 Hz was boosted by up to +41 dB while the bass passed at
unity, with no treble roll-off as drive rose.

- **First-order RC shelf (`src/dsp/effects/tube_screamer.rs`).** The clipping
  stage is `1 + Zf/Zi` with `Zi = 4.7 kΩ + 1/(s·0.047 µF)`, i.e. unity at DC
  rising 6 dB/oct toward `1 + Zf/R`. It is now applied exactly as
  `x + (Zf/R)·HP₇₂₀(x)`, so the low end and low-mids are lifted (the pedal is no
  longer thin) instead of staying at unity under a clipped mid hump. The old
  2nd-order shelf over-emphasised the region just above the corner.
- **51 pF feedback pole.** `Zf`'s parallel cap puts a pole at
  `1/(2π·Zf·51 pF)` — ~61 kHz at minimum drive falling to ~5.7 kHz at maximum —
  restored as a drive-dependent one-pole LP after the clipper. The pedal now
  loses treble as gain rises, as the circuit does.
- **Tests:** `feedback_pole_falls_with_drive`,
  `feedback_pole_reduces_the_high_drive_treble_ratio`, and
  `shelf_rises_from_unity_toward_the_drive_gain` (replaces the old
  `bass_passes_at_unity...`, whose premise was the 2nd-order approximation).
- **Measured (`mesa_modern_metal`, the only bundled TS user):** low-mids lift
  ~+7–9 dB through 140–706 Hz, centroid 3314→3120 Hz, LUFS −13.88→−14.31. The
  maintainer A/B'd and approved ("so much better"). Baseline regenerated (20
  presets), `--check` green.

---

## Phase 8 — DSP audit remediation (findings A1…, from `dsp-findings-2026-09.md`)

A review of the DSP graph, chain-order machinery and output/export path landed
as [`docs/dsp-findings-2026-09.md`](dsp-findings-2026-09.md). Every item is
tagged **DEFECT** (contradicts its own docs, its own test, or arithmetic),
**GAP** (self-consistent but does not model something that matters) or **RISK**
(correct today, load-bearing on an unenforced invariant). Remediation runs
phase A–F; each phase re-blesses the baseline in its own commit and records the
directional drift here.

**Baseline state on entry.** `--check` was already failing with **164 tolerance
violations** before this phase began: commit `2527c61` re-voiced amps and
presets without re-blessing. That drift is inherited, not introduced here.

### A1 — the output limiter was not a limiter

`soft_limit` was `0.95 + e / (1 + 5e)` with `e = |x| - 0.95`. That fraction
tends to `1/5 = 0.2`, so the asymptote was `0.95 + 0.2 = **1.15**` — the
"limiter" *raised* the ceiling by **+1.21 dBFS**. Computed on the old curve:
`|x| = 2.0 → 1.118` (+0.97 dBFS), `|x| = 100 → 1.1496` (+1.21 dBFS).

It survived because the only ceiling test asserted `max_abs < 1.2` **and fed a
0.8-amplitude sine**, which never reaches the 0.95 knee. The bound permitted
exactly the overshoot the function was supposed to prevent.

Fix: `KNEE_SHAPE = 20.0`, putting the asymptote at `0.95 + 0.05 = 1.0`. The
curve is C1 at the knee (the derivative of `e/(1+ke)` is 1 at `e = 0`, matching
the unity region), so there is no slope discontinuity where the knee opens.

- **Tests:** `soft_limit_holds_a_hard_ceiling_of_unity` (new) pins sub-knee
  passthrough, the ceiling at inputs up to `1e9`, odd symmetry, monotonicity,
  and that the knee actually compresses rather than merely touching unity. The
  existing `master_bus_...` bound is tightened from `< 1.2` to `<= 1.0`. Verified
  the new test *fails* against the old `5.0` shape (`ceiling exceeded at 1.5:
  1.0966667`).
- **Measured:** no preset reaches the knee at these levels, so the re-bless is
  driven entirely by the inherited `2527c61` drift, not by this change. LUFS,
  crest, correlation and centroid are unchanged by A1. The gain-limit
  (F1) side — stereo-linked gain reduction, release time, oversampling of the
  limiter itself — is **not** in this increment; the master bus is documented
  stateless and `process_block_matches_per_sample` asserts bit-exact parity
  between the per-sample and per-block paths, so that restructuring lands with
  F3.
- **Baseline regenerated** (20 presets), `--check` green.

### A3 — live biquad retunes discarded filter state (the click on every knob move)

`Biquad::from_coeffs` sets `z1 = z2 = 0`. A constructor therefore *always*
discards the recursion state, and for a shelving or EQ filter the discarded `z1`
is a large fraction of signal amplitude. `Biquad::set_high_shelf` existed
specifically to avoid this ("so a live control change does not click") and had
**zero call sites** — the TS-808 commit that added it wired it into that one
pedal's constructor path only.

Nine live paths rebuilt instead of retuning, resetting 24 filter states in
total:

| Site | Filters | Trigger |
| --- | --- | --- |
| `effects/graphic_eq.rs` `rebuild` | **14** | GE-7, on *any* band move |
| `effects/mod.rs` `ThreeBandEq::set_gains_db` | 6 | param EQ **and** pre-amp EQ |
| `cab/mod.rs` `MicChannel::retune` | 2 | MIC knob, 0.05 per keypress |
| `amp/randall.rs` | 4 | bass / mid / treble / presence |
| `effects/distortion.rs` `update_tone` | 2 | DS-1 TONE |
| `effects/metal_core.rs` | 2 | ML-2 LOW / HIGH (±15 dB shelves) |
| `effects/clean_boost.rs` `set_tone` | 2 | boost BASS / TREBLE |
| `effects/fuzz.rs` `set_guitar` | 1 | fuzz GUITAR |
| `amp/vox.rs` `set_cut`, `amp/tweed.rs` `update_tone` | 2 | Vox CUT, Tweed tone |

Fix: every design now has a `set_*` twin that recomputes coefficients **in
place**, preserving `z1`/`z2`. The cookbook maths moved into one `*_coeffs`
helper per design, shared by the constructor and the setter, so the two cannot
drift apart. Also dropped a stale `#[allow(dead_code)]` from `lowpass`, which is
used in eight places.

- **Tests:** `setters_match_their_constructors_bit_for_bit` (all five designs,
  so a setter is provably the same filter as its constructor);
  `every_setter_preserves_state`; `set_high_shelf_preserves_state_across_a_live_change`;
  `designs_hit_their_cookbook_targets` (the five designs still hit their
  cookbook targets, and a 0 dB shelf is confirmed *exactly* unity — which is
  what made `vox.rs:124`'s presence filter a mathematical no-op, see C5).
- **Pedal-level regression:** `a_live_fader_move_does_not_click` drives a real
  1 kHz tone through the GE-7 while walking the 1 kHz fader in 0.05 keypress
  steps, and bounds the worst sample-to-sample step. Measured: **4.4× the sine
  slew with the fix, 17.6× without**; the bound is 10×. Verified the test fails
  against the old `rebuild`.
- **On the smoothness claim.** Preserving state is the right behaviour for a
  knob-sized step, not for an arbitrary one: a 24 dB jump in a single sample
  leaves stale state that is wrong for the new transfer function too. The first
  draft of this test used such a jump and measured the *opposite* ordering, so
  the test now uses a realistic monotonic drag and says why in its doc comment.
- **Baseline unchanged** — `--check` green without a re-bless. The harness
  renders static presets, so a live-retune defect is invisible to it by
  construction. This is a gap in the harness's coverage, not evidence the
  change was a no-op.

### A6 — the Phase 90 and the Electric Mistress folded the rack to mono when dry

Both mono-type modulation pedals computed the mono fold **before** the wet/dry
mix:

```rust
let (in_l, in_r) = if phase90 { let m = 0.5 * (l + r); (m, m) } else { (l, r) };
...
in_l * (1.0 - mix) + wet_l * mix,
in_r * (1.0 - mix) + wet_r * mix,
```

So at `mix = 0` — the setting that means "this pedal does nothing" — a Phase 90
or a Mistress output `(m, m)`: a hard mono fold of the entire downstream rack.
Turning a pedal *down* narrowed the stereo image, which is the opposite of what
a dry effect should do.

The fold also ran **before** the flanger's regeneration write, so the Mistress's
feedback loop was summing a pre-folded signal too.

Fix: fold only the **wet**. The dry is now the caller's own L/R, and the wet is
the pedal's mono signal. At `mix = 0` both pedals are wire-transparent; at
`mix = 1` both are mono, which is the real pedal's behaviour.

- **Two existing tests encoded the old contract** and had to be rewritten, not
  just extended: `phase90_mode_is_mono_and_script` and `mistress_mode_is_mono`
  both asserted L == R at `mix = 0.5`. They now assert the pair that actually
  defines the fix — mono at `mix = 1`, full stereo difference preserved at
  `mix = 0` (input `(x, 0.3x)` with `x` peaking at 1.0, so the channels must
  still differ by 0.7). `fully_dry_is_passthrough` in both files was also only
  exercising `kind = 0.0`; it now loops over both kinds.
- **New:** `phase_90_folds_the_wet_but_not_the_dry` and
  `mistress_folds_the_wet_but_not_the_dry` use a hard-panned, uncorrelated
  input — the shape that makes a collapse unmistakable — and assert both ends of
  the mix range.
- **Measured (3 of 20 presets drift, and the drift is exactly the mechanism):**

  | Preset | Mono pedal | mix | correlation | LUFS-i |
  | --- | --- | --- | --- | --- |
  | `pink_floyd_another_brick_pt2` | Phase 90 | 0.42 | 0.9410 → **0.8952** | −14.091 → **−13.986** |
  | `pink_floyd_comfortably_numb_solo_1` | Mistress | 0.20 | 0.9572 → **0.8320** | −14.940 → **−14.642** |
  | `pink_floyd_comfortably_numb_solo_2` | Mistress | 0.26 | 0.9165 → **0.8062** | −16.615 → **−16.350** |

  Every preset that moved uses a mono-type phaser/flanger at `mix < 1`, and
  correlation falls as the dry regains its width — the wider the image, the
  bigger the fall, so `numb_solo_1` (mix 0.20, most dry) moves most. The other
  17 presets are byte-identical: the generic stereo phaser and flanger were
  never folding anything. Baseline regenerated (20 presets), `--check` green.

### A5 — the tremolo added 4 ms of latency, and the Fender Twin inherited it

`Tremolo` had **no dry path**. The output was always a read of the delay line:

```rust
let del_ms = CENTER_MS + pitch_depth * SWING_MS * sine;   // CENTER_MS = 4.0
let wet_l = Self::read(&self.buf_l, self.write, del);
...
(wet_l * gain, wet_r * gain)
```

At `pitch_depth == 0` — pure tremolo, which is the **default**
(`DEFAULT_TREM_MODE = 0.0`) — the tap length is *constant*, so it contributes no
pitch movement at all. It only added `CENTER_MS` = **4 ms = 192 samples @48 kHz**
to the whole rig, unconditionally, with nothing to bypass it back out.

It leaked out of the pedal entirely: `fender.rs:202` runs the Twin's onboard
bias tremolo through this module with `mode = 0.0`, so **the Fender Twin model
was permanently 4 ms behind the other eight amps**. Switching amp models on
stage produced a 4 ms time jump — audible as a slapback against a reverb or the
take bus.

Fix: with `pitch_depth == 0` the input is taken directly and only the gain is
applied. Vibrato still uses the tap, and its 4 ms is a real vibrato pedal's
behaviour, not an artifact. The buffer is written either way so the tap is warm
when the user reaches for MODE.

- **Tests:** `tremolo_mode_adds_no_latency` (impulse response peaks at the
  input sample, at depth 0/0.5/1.0), `zero_depth_tremolo_is_wire_transparent`
  (bit-exact), `vibrato_mode_does_delay` (pins the tap's intended latency
  *band*, `CENTER_MS ± pitch_depth·SWING_MS`, so a future "optimisation" cannot
  quietly drop it).
- **New amp-level test:** `all_amp_models_are_latency_aligned`, across all nine
  models.
- **On that test's metric**, since two earlier attempts were wrong. These amps
  are non-linear (sag, bias offsets, saturators), so transfer-function group
  delay is undefined, and an **energy centroid measures the response's tail, not
  its arrival** — measured that way the spread is 27–118 samples and says
  nothing about alignment. The test uses the first sample crossing 1% of the
  impulse response's own peak, with an 8-sample (0.17 ms) tolerance: that absorbs
  the filter-shape differences between models (the Tweed's treble-cut lowpass
  rises ~4 samples slower than the others' shelves) while being 24× tighter than
  the defect. Verified it fails against the old code:
  **onset spread 15..208, Fender at 208 against 15 for every other model.**
- **Measured (1 of 20 presets):** `eagles_hotel_california_clean` is the only
  Fender user, and it is the only preset that moves — LTAS 2822 Hz +0.30 dB,
  5644 Hz +0.35 dB, 7112 Hz −0.98 dB. The shift is the removed 4 ms changing how
  the amp's bias-tremolo envelope lines up with the signal content. Baseline
  regenerated (20 presets), `--check` green.

### A8 — the noise gate opened in 0.21 ms, and its RELEASE knob was dead code

Three separate problems in `noise_gate.rs`:

1. **`gain_coeff = 0.9` on opening.** A one-pole with `coeff = 0.9` has a
   10-sample time constant — **0.21 ms @48 kHz**. The gate was therefore opening
   essentially instantly on a non-zero waveform, which is a hard cut and clicks
   on every note. Real gates open in roughly 0.5–2 ms.
2. **The detector release was hardcoded at 100 ms**, built in `new()` and
   ignoring the knob entirely.
3. **The RELEASE knob did nothing.** `release_ms` was computed and then thrown
   away:
   ```rust
   let release_ms = 10.0 + release * 490.0;
   let _ = release_ms; // used for future hold extension
   ```
   and the only thing the knob reached was the gain-smoothing coefficient
   `0.999 - release * 0.009` (a 1 ms → 0.9 ms range, inaudible).

The hold the comment described is now implemented: RELEASE sets how long the
gate stays open after the signal falls below the threshold (10–500 ms), which is
what stops a gate chomping the gap before a reverb tail starts.

**The detector release had to get faster for the hold to be audible.** With the
old 100 ms detector, the envelope took ~480 ms to fall from a note's level to
below the threshold, so the hold timer was always expired before the gate could
begin closing and the knob was inaudible. The detector now uses a fast 10 ms
release (1 ms attack unchanged) and the hold carries the knob.

- **Tests:** `opening_the_gate_does_not_click`, `release_knob_is_a_hold_time`,
  plus the pre-existing `passes_loud_and_gates_quiet` unchanged.
- **On the click test's probe — this took three attempts and the reason matters.**
  It measures the worst sample-to-sample step as the gate opens, and the obvious
  probe is a 220 Hz tone. That does not work: the tone's own slew is 0.023 of
  its amplitude per sample, the same order as the step under test, so the
  measurement is dominated by the probe — **both** the old 0.21 ms ramp and the
  new 1 ms ramp measured 0.0230, and the test passed against the buggy code.
  A constant probe has zero slew, so the only step is the gate's. Measured:
  **0.0800 with the old `coeff = 0.9`** (exactly `amp * (1 - 0.9)`, as a
  one-pole should) and 0.0168 with the 1 ms ramp; the bound is 0.03. Verified
  the test fails against the old coefficient.
- The hold test also probes with a small sub-threshold constant rather than
  silence, because the gate's output is `input * gain` — with a zero input the
  output is zero whatever the gain is doing, and the first draft of this test
  measured a hold time of 0.0 ms for every knob setting.
- **Measured:** 44 LTAS violations across the 15 presets that enable the gate,
  every one **≤ 0.6 dB** and in the third-octave bands around the note decay
  (88–1411 Hz mostly). Direction: the gate now passes more sustain, which
  raises the mean, and `ltas_third_octave` is mean-normalized, so bands show a
  small relative dip. Baseline regenerated (20 presets), `--check` green.

### A2 + F3 — the output path: a final ceiling, and a master-bus DC blocker

**A2 — the monitor buses were summed after the limiter.** `chain.process_block`
ends in `master_bus` (widen → limit), and the take bus runs a *whole second*
`DspChain` with its own `master_bus`. Both were then added together, and the
metronome click (`CLICK_GAIN = 0.5`) and the looper were mixed into the live bus
on top, with nothing after them. Two bounded-at-1.0 buses sum to +3 dB and a
click takes it to 1.5, straight into the converter — and it grew with every take
layer.

Fix: a final `output_stage(live_l, live_r, take_l, take_r)` applying the *same*
`soft_limit` ceiling, so live and exported audio are conditioned identically.
This covers the tuner bypass for free, since that path writes into the same
buffers.

**F3 — nothing removed DC on the full-rig AU path.** Every built-in amp ends with
a ~12 Hz high-pass, but that is not unconditional: with a **full-rig AU** the
built-in amp is skipped entirely (`skip_cab`), so the only stage that would have
removed DC — the cab's 70–105 Hz high-pass — is exactly the one being skipped. A
plugin's DC offset reached the output raw. The tuner bypass had the same gap.

Fix: a first-order `DcBlocker` (`y[n] = x[n] - x[n-1] + R·y[n-1]`, 8 Hz corner)
in the master bus, plus a pair on the tuner path. It also gives the **offline
export** the same guarantee as the live path, since export drives
`chain.process_block` directly.

**On making the master bus stateful.** It was documented as stateless, and
`process_block_matches_per_sample` asserts *bit-exact* parity between the
per-sample and per-block paths. The blockers are the first state in it. Parity
holds because each path drives its own blockers one sample at a time and
deterministically — the test is unchanged and still passes, and its doc comment
now records why.

- **Tests:** `master_bus_removes_a_dc_offset` (both the blocker directly and
  through the real bus; the mean is measured *after* a 0.25 s warmup, because
  the blocker's initial step has a finite area — `sum(x·R^n) ≈ x/(1-R)` — so a
  window including it reads the transient, not leakage),
  `dc_blocker_is_transparent_in_the_guitar_range` (a quadrature measurement at
  82.41 Hz through 4 kHz, so a phase shift cannot masquerade as gain),
  `summed_monitor_buses_cannot_exceed_the_ceiling`, and
  `final_stage_is_transparent_at_normal_levels`.
- **Measured:** no baseline drift at all — `--check` green without a re-bless.
  The built-in rigs were already DC-free, and the harness's `ltas_third_octave` is
  mean-normalized, so removing a residual the renders barely had is invisible.
- **CPU:** no measurable cost. Bench, measured serially (running the fidelity
  harness alongside it produced a bogus 5.3% reading from contention):
  **4.27% / 4.50%** of the realtime budget with this change vs **4.28% / 4.52%**
  at the previous commit. Six float ops per sample against 8× oversampled amp
  stages is not measurable. Note both are above the 3.3–3.6% in `roadmap-next.md`
  §4.2 — that figure predates the `2527c61` amp re-voice, not this phase.
- **Still open (F1), deliberately:** `soft_limit` bounds peaks but applies no
  *gain reduction*, so it squashes rather than turns down — no stereo-linked
  detector (a hard-panned peak still shifts the image), no release time, no
  true-peak/inter-sample detection, and it is a base-rate nonlinearity so it
  generates its own alias products. Replacing it changes the level of every
  preset, so it belongs in the output-gain discussion rather than in a bug fix.
  What shipped here is the ceiling guarantee, which is what the A2 defect was.

---

## Preset cull and a vanilla boot rig (2026-09-29)

Twelve of the twenty bundled presets were retired, and the factory default
became a neutral starting rig. Recorded here because the increment log above
still names the retired presets in its Phase 5/6/7 entries — those are
historical and deliberately left as written.

**What shipped.**

- **Retired (12):** `acdc_back_in_black`, `acdc_highway_to_hell`,
  `guns_n_roses_november_rain_solo`, `led_zeppelin_whole_lotta_love`,
  `marshall_hard_rock_rhythm`, `mesa_modern_metal`,
  `pink_floyd_another_brick_pt2`, `pink_floyd_have_a_cigar_solo`,
  `pink_floyd_money`, `pink_floyd_shine_on_crazy_diamond`,
  `van_halen_beat_it_solo`, `vox_chime_clean`.
- **Survivors (8):** the two Eagles presets, the Stairway solo, the two
  Comfortably Numb solos, Mother, and the Time chorus and solo.
- **Reason:** the tone models changed underneath them — A1/A3/A5/A6/A8 above,
  and the amp architecture rework is still ahead. Their knob values were tuned
  against models that no longer exist. [`retired-presets.md`](retired-presets.md)
  indexes each one with its last-touching commit and the recovery command.
- **Vanilla default:** `DEFAULT_AMP_MODEL`/`DEFAULT_CAB_MODEL` are now
  Plexi + Marshall (Greenback) instead of Mesa + Mesa, the noise gate, TS-808
  and reverb all start off, and `DEFAULT_MASTER_WIDTH` drops `1.3 → 1.0`. The
  board boots empty. `1.3` was kept for back-compat under review finding R8, but
  every bundled preset already set `width = 1.0`, so nothing depended on it.
- **Baseline regenerated** for 8 presets, `--check` green.

**The cost, stated plainly.** The survivors use only **4 of 9 amp models**
(Hiwatt, Fender, Supro, Tweed) and **4 of 8 cabs**. Marshall, Mesa, Randall,
Vox and Plexi — and the Marshall/Mesa/Orange/Vox cabs — have no bundled preset,
so they are no longer exercised *end-to-end through a preset* by the harness.
Their unit tests in `dsp/amp` and `dsp/cab` still cover them directly. The
harness went from 20 independent tone checks to 8, which is its thinnest point,
and the upcoming Phase C amp rework is the change most likely to disturb the
rigs it can no longer see. The three retired generic rig showcases
(`marshall_hard_rock_rhythm`, `mesa_modern_metal`, `vox_chime_clean`) are the
cheapest way back if that matters.

**Test fallout from the new default.** Four tests encoded the old boot state and
were corrected rather than deleted:

| Test | Problem |
| --- | --- |
| `ui::input::nudge_moves_only_the_targeted_knob` | Nudged amp slot 2 assuming the Mesa's defaults; the Plexi's BASS sits at `1.0`, so `+0.05` clamped and the test measured the clamp. Now parks the knob mid-scale first. |
| `ui::input::select_amp_external_row_activates_the_loaded_au` | Asserted the boot model was `Mesa` after a garbage pick. Now asserts the model is *unchanged* by the garbage pick, which is the actual intent. |
| `ui::draw::snapshot_default_screen` | `expect("a default pedal")` — the board is now empty by design. Mirrors the app's own focus fallback (land on the amp tile). |
| `tests/bundled_presets` + `preset::RENDER_SUBSET` | Count 20 → 8; two subset entries and two hardcoded `acdc_back_in_black` paths in `analysis/render.rs` and `tests/fidelity_harness.rs` repointed to survivors. |

Ten UI goldens were re-blessed; the diffs are the rig and the now-empty ribbon
(`AMP ──▶ CAB ──▶ OUTPUT`), nothing else.

**Also fixed here:** `src/analysis/render.rs` and `docs/fidelity-references.md`
carried a stale inventory — `led_zeppelin_stairway_solo` was listed as
`plexi | marshall` after Phase 5 had already rebuilt it onto the Supro. Corrected
against the TOML.

### A7 — the export did not render what you monitored

Two independent causes, both silent, both "my export doesn't sound like what I
heard".

**The external IR differed between live and offline.** The realtime path capped a
loaded IR at `LIVE_MAX_IR_LEN` (8192 taps ≈ 170 ms) and the exporter at
`OFFLINE_MAX_IR_LEN` (32768 ≈ 683 ms). `load_ir` ends with `normalize_pair`,
which **unit-energy-normalises**, so the two caps produced genuinely *different
impulse responses* for any IR longer than 170 ms: the export normalised over more
energy (so it was quieter) and was missing the tail (so it was duller). You
monitored one cab and rendered a different one, with nothing surfacing it until
after the render.

Fix: the caps converged. `OFFLINE_MAX_IR_LEN` is gone; both paths use
`LIVE_MAX_IR_LEN`. This **removes** a capability — the exporter no longer keeps
full room tails past 170 ms — and that is the intended trade. An export has to
reproduce what you monitored, and a longer tail you cannot hear in monitoring is
not worth an output you cannot trust. For the common case (cab IRs of 512–2048
taps) nothing changed at all; only IRs between 170 ms and 683 ms were ever
affected.

- **Test:** `a_long_ir_loads_identically_for_live_and_offline` drives an IR three
  times the cap through both paths and asserts they agree sample-for-sample, and
  that the normalisation really is unit energy (the mechanism the old split
  exploited). Replaces `offline_cap_exceeds_live_cap`.

**The export started from a cold chain.** `render_with_chain` began writing at
frame 0 with every stateful stage at rest: the amps' rectifier-sag and
dynamic-bias envelopes at zero, the cab convolver's delay line empty, the reverb
and delay buffers silent. The first pluck of a take was rendered into a rig that
had not yet found its operating point. The offline harness *did* preroll
(`analysis/render.rs`, `preroll_s = 0.5`), so the harness and the exporter were
measuring different things.

Fix: `PREROLL_SECS = 0.5` of silence through the chain before the first written
frame, matching the harness.

- **Test:** `preroll_warms_the_chain_without_offsetting_the_render`. The
  *dangerous* half of a preroll is that it leaks into the output and shifts every
  take 24 000 samples later than it was recorded, so the test measures the onset
  of a pluck placed at a known input offset and asserts it lands at that offset
  plus only the cab convolver's 128-sample latency — with a second assertion that
  the shift is under a tenth of a preroll.
- Baseline unchanged: `--check` green, 8 presets. The harness already prerolled
  and the export path is not what the bundled-preset renders go through.

### A4 — unsmoothed gains, and a compressor that was never unity

Every output-scaling control in `src/dsp/effects/` was applied as an
instantaneous per-sample multiply. Added `SmoothedGain` (a one-pole, 8 ms) and
routed every such coefficient through it: the TS-808's drive shelf, the clean
boost's gain, the compressor's output level, the GE-7's output level, and the
wet/dry mix on the whammy, pitch shifter, chorus, flanger, phaser, Uni-Vibe,
delay and reverb.

**What made this more than cosmetic:** `MidiTarget` (`src/midi.rs`) binds an
expression-pedal CC to `delay_mix`, `reverb_mix`, `chorus_mix`, `flanger_mix`,
`phaser_mix`, `boost_gain`, `ts_drive`, `ds_drive`, `wah_position` and more. An
expression pedal writes a *new* value every few samples, so the unsmoothed gain
was an audio-rate staircase. The 41 dB TS-808 shelf step and 24 dB boost step
made it audible from a keyboard too.

**The compressor had a second defect.** Its auto-makeup was
`db_to_lin(-thresh_db * (1-1/ratio) * 0.5)`. At `sustain = 0` (threshold −6 dB,
ratio 2:1) that is `db_to_lin(1.5)` = **+1.5 dB** — so engaging the compressor at
its most transparent setting was itself a level step, and bypassing it dropped
that step. The makeup is now `1 + (full - 1) * sustain`: **unity at sustain = 0
and exactly the old value at sustain = 1**, so the top of the knob, where the
makeup does its actual job, is untouched.

A first attempt scaled the dB directly by `sustain` instead. That also fixes the
zero but pulled ~4.7 dB out of the mid-range and produced **96** baseline
violations; the linear fade is the gentlest smooth interpolation between unity
and the original curve and cut that to **83**, with much smaller level moves.

- **Design note — the first `set` snaps, later ones ramp.** The constructed
  value is only a placeholder (the effect has not been given a control value
  yet), so the first value is adopted verbatim. Without that, a
  freshly-constructed effect driven at `mix = 0` would ramp in from the default
  mix and fail bit-exact dry transparency for its first few ms — which is what
  the first run of this change did, failing 10 `fully_dry_is_passthrough` tests.
- The compressor's gain-computer constants (`thresh_db`, `ratio`, the detector
  coefficients, the makeup) are now recomputed on a knob move rather than every
  sample, removing a `lin_to_db`, a `db_to_lin` and two `powf` from the hot path.
- `graphic_eq`'s output level was compared with an **exact** `!=` while its bands
  used `param_changed(ε)`, so any float jitter in `level` retriggered the whole
  14-filter bank. Now uses `param_changed` like the bands.
- **Tests:** five for `SmoothedGain` (ramped not stepped, transparent when
  static, settles in 5–40 ms, staircase-free under a 100 Hz CC sweep, and the
  first-value snap); `zero_sustain_is_level_transparent`,
  `auto_makeup_still_compensates_at_high_sustain` and `level_knob_moves_smoothly`
  for the compressor.
- **Measured.** The **smoothing is transparent in steady state**: the five
  presets with no compressor (`stairway_solo`, `numb_solo_1/2`,
  `hotel_california_solo`, `time_chorus`) show no LUFS, crest, correlation or
  centroid movement at all — only LTAS bands within ~1.5 dB, which is the first
  ~30 ms of ramp inside a multi-second render. The level moves are all from the
  compressor fix, and only on the three presets that use one:

  | Preset | sustain | LUFS-i | crest | centroid |
  | --- | --- | --- | --- | --- |
  | `eagles_hotel_california_clean` | 0.34 | −11.106 → **−11.843** | — | 3277 → **3326 Hz** |
  | `pink_floyd_mother_solo` | 0.40 | −14.374 → **−15.594** | 16.73 → **17.04 dB** | 3648 → **3692 Hz** |
  | `pink_floyd_time_solo` | 0.42 | −13.902 → **−14.077** | — | — |

  These three are up to **1.2 dB quieter** and slightly brighter. That is the
  defect fix, not a regression: the old curve was paying up to +7.9 dB of
  automatic makeup mid-way on the knob. If the levels are preferred as they
  were, the fix is one `level` value per preset — say so and it is a
  three-line change. Baseline regenerated (8 presets), `--check` green.
- **CPU:** 5.09% / 4.61% of the realtime budget, against 4.27% / 4.50% measured
  in A2/F3 — but on a **different preset pair** (`bench` was repointed to
  survivors when the other twelve were retired), so the two numbers are not
  comparable. The added cost is one multiply-add per smoothed control per
  sample.

### Output ceiling visibility — a true peak and a `LIM` indicator

Raised by a live observation: a rig was audibly "kinda clipped" while the
interface's meters never reached the top. Measuring the rendered presets showed
their peak at **0.27–0.56 FS**, i.e. the output limiter (knee 0.95) was not
engaging at all — so the sound was saturation *upstream* (see C1/C3), and the
meter was telling the truth. But two things made this genuinely hard to see:

1. **`Levels::output` is a follower, not a peak meter.** A 1 ms attack / 300 ms
   decay envelope tracks loudness but does not *hold* a peak, so a transient
   that touched the ceiling could be gone before the next redraw.
2. **It is computed before the take bus is added** (`audio/mod.rs` updates
   `out_env` inside the loop that sums the metronome/player/looper, and the
   take-bus rig runs afterwards), so the OUT meter never reflected the take
   layer either.

Added two output-only overlays:

- **`output_peak`** — true peak of the *final*, post-ceiling signal, instant
  attack with a ~1.5 s decay, drawn as a `▏` tick over the bar. Because it draws
  over the filled/empty cells it needs no extra width.
- **`limiting`** — a 0–1 value set to 1 whenever the knee actually engages
  (either summed bus exceeded 0.95) and decaying over ~0.4 s, shown as a `LIM`
  badge that fades rather than latching.

- **Test:** `ceiling_indicator_tracks_only_real_limiting` pins that the knee
  opens at exactly 0.95, that sub-knee samples pass through bit-exact, that
  above-knee samples are bounded *and* altered, and that two already-limited
  buses summing still engages — which is the case that motivated the final
  output stage in A2.
- Snapshots re-blessed; the diff is the `LIM` badge on the OUT row and nothing
  else. Baseline unchanged — this is measurement, not signal.

### A9 — gate `sample_peak` and DC in the harness, and finally measure aliasing

**`sample_peak` is now a gate.** It was *recorded* and never checked, which is
precisely why A1 survived: a limiter that raised its own ceiling to +1.21 dBFS
looked fine for months. It is a **hard bound, not a drift tolerance** — the
ceiling is a guarantee (`soft_limit` asymptotes to 1.0, and the engine's final
output stage re-applies it after the take-bus sum), so nothing above it can reach
the converter.

> **Its honest limitation.** Measured across all 8 presets × 7 DIs, the highest
> `sample_peak` is **0.72**. Nothing in the bundled set reaches the 0.95 knee, so
> the gate currently has no leverage: reintroducing the old `KNEE_SHAPE = 5.0` did
> **not** trip it, because the limiter never engages. It protects against a preset
> or export *starting* to exceed the ceiling. The limiter's shape itself is covered
> by `soft_limit_holds_a_hard_ceiling_of_unity`, which I verified does fail
> against the old curve. Both layers exist for different failure modes.

**DC offset is now a metric and a gate.** New `dc_ratio` = mean ÷ RMS of the
render. Nothing else in the harness could see a static offset: `ltas_third_octave`
is mean-normalized, and its lowest band is 70 Hz — above where most of the amp
DC blockers are working. Measured max across all renders: **1.1e-7**, i.e. the
master-bus DC blocker from F3 is doing its job completely. Gate at 2% of RMS.

**Aliasing is now measured, and the measurement found a real gap.**

The existing cab alias check (`breakup_stays_clean_and_musical`) drives **220 Hz**,
so its harmonics are 440/660 Hz — far below every lowpass corner in the cab. The
band it probes (6.5–12 kHz) *cannot* contain a folded harmonic of a 220 Hz tone.
The check is structurally incapable of seeing aliasing; it would still pass with
`cone_breakup` replaced by a hard clip.

Driving a **7 kHz** tone instead — whose harmonics run past Nyquist and fold back
to 1, 6, 8, 13, 15 and 20 kHz, none of which is a harmonic of 7 kHz — measures,
relative to the drive tone:

| harmonic folds to | product | measured |
| --- | --- | --- |
| 7F = 49 kHz | **1 kHz** | **0.43** |
| 6F = 42 kHz | 6 kHz | 0.010 |
| 8F = 56 kHz | 8 kHz | 0.002 |
| 5F = 35 kHz | 13 kHz | 0.002 |
| 9F = 63 kHz | 15 kHz | 0.0002 |
| 4F = 28 kHz | 20 kHz | 0.0002 |

Out-of-band fold-back is negligible. The problem is the **in-band** product: the
cab's base-rate nonlinearities put a tone at **1 kHz at 43% of the drive tone's
amplitude** — deep in the passband, so it gets the full benefit of the cab's
response.

This is finding **D4** confirmed by measurement. The fix is to wrap `SpeakerDrive`
in the existing `Oversampler4`, which is a Phase D item with a real audio-thread
CPU cost — deliberately not done here. `hf_input_folds_back_into_the_passband_documented`
therefore sets its bounds just above the measured values so they catch a
*regression* rather than assert a target the code does not meet, and the doc
comment carries the full table so the bounds can be tightened when D4 lands. The
fold-back frequencies are asserted exactly, so if the Nyquist relationships change
the test fails loudly rather than silently measuring the wrong thing.

### B1 — bypass toggles clicked, and the fix was hiding in the macros

Toggling any of the 21 chainable stages was a **hard cut** from dry to wet (or
back). That is a step in the waveform, and the step is as large as the difference
between the stage's two signals. For a flanger at `feedback = 0.9, mix = 0.5` the
wet runs well above the dry, so engaging it was a full-scale discontinuity — the
familiar "pop" when you stomp a pedal in a live rig.

**The fix.** `DspChain` now carries a `bypass_ramp: [f32; CHAIN_LEN]` and a
per-sample `bypass_step` (`BYPASS_DECLICK_SECS = 0.006`). `run_ordered_stage`
walks the ramp toward the target every sample and crossfades
`dry + (wet - dry) * g`. It is deliberately staged at the **stage boundary**
rather than inside each effect:

- Every bypassable stage gets the same treatment, including the ones I would not
  have thought to audit (there are 21).
- The effect's own internals stay untouched, so their CPU cost is unchanged.
- Fully bypassed (`g <= 0`) returns the dry signal *without running the effect*,
  so a bypassed stage still costs nothing and its state stays frozen.
  Fully engaged (`g >= 1`) returns the wet signal exactly as before.

6 ms, not 4 ms like `DECLICK_SECS`: a bypass step can be larger than a
built-in↔AU path switch (a flanger's wet is an order of magnitude above its dry),
so the ramp has more ground to cover. Still far below the ~20 ms where a fade
starts to read as a swell.

**The part that was not obvious.** `mono_stage!`/`stereo_stage!` each branched on
the stage's own `*_enabled` flag and returned the dry signal in the `else`. That
looked like free CPU, but it silently defeated the ramp: `run_ordered_stage`
computed `wet = dry` because the macro had already discarded the effect, so the
crossfade blended two copies of the dry path and the click survived at full size.
The macros now process unconditionally; the ramp is the single place that decides
bypass. The saving is not lost — `run_ordered_stage` returns *before* calling them
when the ramp is fully out.

> **How the test found it.** `bypass_toggle_does_not_click` measures the worst
> sample-to-sample step across a toggle and compares three runs of the *same*
> rig: an untoggled control, the ramp, and the ramp forced to one sample. The A/B
> is what makes the probe shape irrelevant — a tone's own slew is identical in
> all three runs and cancels out. (A DC probe does not work here at all: the
> flanger is a delay comb, so on a constant input its delay line fills with DC,
> its wet equals its dry, and toggling measured a step of **6e-6** — it would have
> "passed" a useless test.) Measured worst step: **control 0.018**, **ramp
> 0.082**, **hard cut 0.561** — a 6.8x improvement, landing within 4.6x of the
> signal's own natural slew.

Bypass **transparency** is unaffected and still asserted: a fully bypassed stage
returns the input untouched, because at `g = 0` the wet is never even computed.
