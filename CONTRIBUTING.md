# Development notes

Working notes for developing rusty-riff. This is a personal project — there is no
external pull-request process. Keep the tree green and the docs (where they still
exist) in sync with the code.

## Getting started

```bash
cargo build
cargo run --release   # use --release for realistic audio performance
```

A debug build can underrun the audio callback, so always test audio with
`--release`.

## Project layout

```txt
src/
  main.rs           — entry point: builds shared state and starts the UI
  lib.rs            — module tree + legacy-config-dir migration
  preset.rs         — preset load/save/serialization
  recording.rs      — WAV recording of the processed output
  export.rs         — offline render of guitar takes to WAV
  project.rs        — portable session-folder format
  session.rs        — runtime session/track state
  practice.rs       — shared transport + backing-track decode
  audio/            — cpal device selection and the real-time audio callback
  host/             — third-party plugin hosting (CLAP insert, macOS AU amp)
  dsp/              — all signal processing
    amp/            — amp models (marshall, mesa, plexi, vox, fender, randall, hiwatt)
    cab/            — cabinet IR convolution and mic models
    effects/        — pedals & rack effects + their shared building blocks
      mod.rs        — OnePoleLp, ThreeBandEq, dB/param helpers (shared logic)
      compressor.rs, fuzz.rs, tube_screamer.rs, distortion.rs, metal_core.rs,
      preamp_eq.rs, graphic_eq.rs, parametric_eq.rs, delay.rs, reverb.rs,
      chorus.rs, flanger.rs, phaser.rs, tremolo.rs, uni_vibe.rs, wah.rs,
      pitch.rs, noise_gate.rs
    biquad.rs       — biquad filter building block
    tonestack.rs    — passive FMV tone stack
    oversample.rs   — polyphase N× oversampling (with a `process(x, f)` clip helper)
    mod.rs          — Params, DspChain, and the bypass-stage macros that wire it
  ui/
    mod.rs          — UI loop, board (on-board pedal) state, modal wiring
    draw.rs         — ratatui rendering (rig tile grid + detail editor)
    input.rs        — keyboard handling and board-aware navigation
    setup.rs        — device-selection modals shown before audio starts
    presets.rs      — preset browser overlay
    ir_browser.rs   — external-IR browser overlay
    plugins.rs      — CLAP plugin browser overlay
    amp_plugins.rs  — AU amp-plugin browser overlay
    practice.rs     — timeline pane + track browser
    sessions.rs     — session save/load/recovery browser
    metronome.rs    — metronome modal
    tuner.rs        — tuner modal
    config.rs       — KNOBS + PEDALS tables and knob-range constants
    styles.rs       — colour palette and styles
presets/            — bundled read-only presets (TOML)
examples/           — standalone DSP analysis utilities
docs/               — fidelity reference matrix
```

Design/investigation notes live in `docs/`: `fidelity-plan.md`,
`fidelity-implement.md`, `fidelity-references.md`, and the open
`dsp-findings-2026-09.md` audit. The export and session-storage contracts live in
the module docs of `src/export.rs` / `src/project.rs`; the session invariants live
in `src/session.rs`.

## Code style

The project enforces strict Clippy lints. Before committing:

```bash
cargo fmt
cargo clippy --all-targets --all-features -- -D warnings
```

Comments should only explain *why*, not *what*. Well-named identifiers carry the
what. Clippy denies `unwrap`/`expect`/`panic`/`exit`/`mem_forget` in non-test code:
use `?`, `unwrap_or_else`, and `anyhow` errors.

## DSP conventions

The audio callback runs on a dedicated real-time thread. Code that touches the
signal path must be **allocation-free, lock-free, and panic-free** at runtime.

- Process samples as `f32`
- Oversampled stages live inside `oversample.rs`; add new ones there
- Biquad filters are built with `biquad.rs` helpers; do not implement raw filter math inline
- The FMV tone stack is in `tonestack.rs` — reuse it for tube amp models
- Cabinet IRs are synthesized in-code (no external `.wav` files); see `cab/` for the pattern
- Shared control state is atomics or preallocated `rtrb` rings created on the control thread
- Never allocate, block, or free on the audio thread; hand displaced buffers back to a worker/control queue

### Adding a new DSP effect

Effects live in `src/dsp/effects/`. Each is a self-contained `struct` with a
`process` method; logic shared between several effects lives in
`src/dsp/effects/mod.rs` so the individual files stay focused on their *voicing*:

- `OnePoleLp` — the variable low-pass behind passive tone controls (TS, fuzz).
- `ThreeBandEq` — low-shelf / mid-peak / high-shelf trio shared by the pre-amp and
  parametric EQs.
- `db_to_lin` / `lin_to_db` — decibel conversions for dynamics stages.
- `param_changed` — the "did this knob move enough to rebuild coefficients?" test.
- `Oversampler::process(x, f)` (in `oversample.rs`) — runs a clipper closure at the
  oversampled rate; reuse it instead of re-writing the up/map/down loop.

To add one:

1. Create `src/dsp/effects/<effect>.rs` with a `struct` that holds state and a
   `process(sample: f32) -> f32` (or stereo equivalent) method. Reuse the shared
   helpers above rather than re-implementing tone LPs, EQ trios or dirty-checks.
2. Declare and re-export it in `src/dsp/effects/mod.rs`.
3. Add a field to `DspChain` and a line in `DspChain::process` using the
   `mono_stage!` / `stereo_stage!` macro (in `src/dsp/mod.rs`) so it bypasses
   cleanly and reads its knobs from the shared `Params`.
4. Add the `Params` fields + defaults in `src/dsp/mod.rs` — the per-knob
   `Arc<AtomicF32>` values **and** the `<fx>_enabled: Arc<AtomicBool>` bypass flag
   (the macro stage and the UI both read it).
5. In `src/ui/config.rs`, add the knob-range constants (`<FX>_START` / `<FX>_END`)
   and the matching `KNOBS` entries. Keep them in the same top-to-bottom order as
   the rest — `←`/`→` navigation walks the `KNOBS` array linearly, so the order
   *is* the on-screen layout.
6. Register the pedal in the `PEDALS` table (also `src/ui/config.rs`): one entry
   with its name, livery colour (add a `PEDAL_<NAME>` colour to `src/ui/styles.rs`),
   knob range, and `enabled`-flag accessor. The rig tile grid, the detail editor,
   the `+ ADD` picker, and `D`-to-remove all derive from this table — no `draw.rs`
   layout code is needed.
7. Update the chain of effects in the header in `src/ui/draw.rs` — `render_header`.
8. Add the pedal's `enabled` flag to the `toggle_pedal` match in `src/ui/input.rs`
   so `Space` can bypass it while it's on the board.
9. Add preset fields in `src/preset.rs` (including the on/off state so a preset can
   place the pedal on the board — `sync_board` reads the enabled flags after load).
10. Add a `#[cfg(test)]` module in the effect file — every effect carries unit tests
    (finite/bounded output, and that each knob moves the band/level it should).
11. Update the UI tests your `config.rs` edit touches: bump the counts in
    `table_sizes_are_stable` (`src/ui/config.rs`), then re-bless the golden
    snapshots (`INSTA_UPDATE=always cargo test --all-features`) and commit the updated
    `src/ui/snapshots/*.snap` — the new pedal appears in the default board and the
    add-pedal modal. See [Testing](#testing).

### Adding a new amp model

Amp models live in `src/dsp/amp/`. Each model is a struct implementing the
`Amplifier` trait (gain stages, tone stack call, sag, and speaker interaction)
plus a `KNOBS` descriptor listing its front-panel controls. `process` takes a
fixed `&[f32; AMP_MAX]` array decoded positionally, so each model can expose its
own control set (JCM800's six knobs, a Plexi's two channel volumes, a Twin's
reverb and tremolo). Use the existing `marshall.rs`/`mesa.rs`/`plexi.rs` (tube)
or `randall.rs` (solid-state) files as a template — the shared building blocks
(`FrontEnd`, `Bloom`, `CathodeBias`, `BrightCap`, `OutputTransformer`,
`SpeakerLoad`, `VoiceBalance`, `Cached`/`ToneCache`) live in
`src/dsp/amp/mod.rs`.

To add one:

1. Create `src/dsp/amp/<model>.rs` (the `Amplifier` impl + a `KNOBS` descriptor)
   and declare/re-export it in `src/dsp/amp/mod.rs`; add a field to `AmpBank`
   and a match arm in each of `AmpBank::new`/`AmpBank::process`.
2. Register the model in the `AmpModel` enum in `src/dsp/mod.rs`: add the
   variant, append it to `ALL`, the `from_u8` discriminant, the `controls()` arm
   returning its `KNOBS`, `name()`/`short_name()` display strings, and splice it
   into the `next()`/`prev()` cycle ring. If it exposes more controls than any
   current model, bump `AMP_MAX` in `src/dsp/amp/mod.rs` and add the matching
   accessor rows/ranges in `src/ui/config.rs`.
3. The amp panel (`src/ui/draw.rs`) and the `A` browser modal are model-aware
   (driven by `controls()`/`ALL`), so no selector edits are needed; update the
   `amp_choices` count assertions in `src/ui/input.rs`.
4. Add the model's string in `src/preset.rs` — the `AmpSection.model` match arms
   in both `Preset::from_params` and `Preset::apply`.
5. Add the new model to `each_amp()` (and `tube_amps()`, if it's a tube model)
   in `src/dsp/amp/mod.rs`'s test module, and tune its output trim until
   `amps_are_loudness_matched` passes.
6. Re-bless the UI golden snapshot (see [Working with snapshots](#working-with-snapshots)).

The `add-amp-model` skill (`.claude/skills/add-amp-model/`) operationalizes this
checklist end to end.

### Adding a new cabinet model

Cabinet models live in `src/dsp/cab/`. Each model synthesizes its own IR (voiced
EQ skeleton + comb reflections + modal resonances). Follow the existing
Mesa/Marshall/Orange pattern; pass a `CabLayout` to `BlendedCab::new`
(`FourByTwelve` for a closed 4×12, `TwoByTwelve` for an open-back 2×12) so the
neighbour-cone interference matches the box.

Register in the `CabModel` enum (`src/dsp/mod.rs`), the `CabBank` in
`src/dsp/cab/mod.rs`, the preset `"marshall"`/`"orange"`/`"wem"` match arms in
`src/preset.rs`, and the cabinet-selector arrays in `src/ui/draw.rs`/`src/ui/input.rs`;
add the new cab's `tests` module and re-bless the UI snapshot.

## Adding a bundled preset

Drop a `.toml` file in `presets/`. Only `name`, `[tube_screamer]`, `[amp]`, and
`[reverb]` are required; every other section is optional. Test it by running the
app and pressing `P`.

Bundled presets cannot be deleted by users, so only add presets that are genuinely
useful and well-tuned.

## Testing

Run the whole suite with:

```bash
cargo test --all-features
```

It runs on every push in CI (`cargo fmt --all -- --check`, then
`cargo clippy --all-targets --all-features -- -D warnings`, then `cargo test`).

### DSP tests

Every effect, amp and cabinet model carries `#[cfg(test)]` tests covering
stability (finite, bounded output) and behaviour (each control moves the
band/level it should). New DSP code must come with tests in the same file.

### UI tests

The terminal UI is tested at three levels, all as `#[cfg(test)]` modules inside
`src/ui/` (they need the modules' `pub(super)` internals):

- **Table invariants** (`config.rs`) — the `KNOBS` ↔ `PEDALS` contract: pedal
  knob-ranges tile contiguously, `pedal_of` round-trips, and a deliberate
  `table_sizes_are_stable` tripwire pins the pedal/knob counts so a table change
  is always a conscious edit.
- **Navigation & state** (`input.rs`) — Tab/`←→` cycling, off-board pedals being
  skipped, knob clamping, amp/cab cycling, and add/remove/toggle board logic.
- **Rendering** (`draw.rs`) — the screen is drawn to an in-memory
  [ratatui](https://ratatui.rs) `TestBackend`. Assertion tests confirm every
  pedal/amp/cabinet renders its name and controls; **golden snapshots**
  ([insta](https://insta.rs)) lock the exact glyphs of the default screen and each
  modal (add-pedal, presets, save, plugin browser, external-IR browser).

#### Working with snapshots

Snapshots are committed under `src/ui/snapshots/*.snap` — that file *is* the
expected output, and CI diffs the freshly rendered screen against it. When a change
alters the UI on purpose, regenerate and review the golden, then commit it:

```bash
cargo insta test --review     # or: INSTA_UPDATE=always cargo test --all-features
```

Two things to know:

- **Never set `INSTA_UPDATE` in CI** — that would auto-accept and mask regressions.
  A stray pending `*.snap.new` also fails CI, so finish the re-bless and commit the
  `.snap`.
- The golden tests are gated on the default `clap` feature, because the help footer
  renders a `clap`-only key. Regenerate with default features on.
- Snapshot filenames are derived from the crate name (`rusty_riff__…`). If the
  crate is ever renamed, rename the files to match.

### Manual audio pass

The automated tests don't listen, so an audio change still needs an ear:

1. `cargo run --release` with an audio interface connected
2. Verify the effect under test across the full gain range
3. Confirm no audible clicks or artifacts when toggling bypass or switching models
4. Confirm the preset round-trips correctly (save → reload → values match)

## Fidelity harness

`examples/fidelity_render.rs` renders presets offline through the real live
chain and reports loudness/tone/level metrics, so a voicing change can be
compared before/after with numbers, not just by ear. The measurement helpers and
the deterministic synthetic DI corpus live in `src/analysis/`.

```bash
# Synthetic Karplus-Strong corpus, all bundled presets, into target/fidelity/<ts>/
cargo run --release --example fidelity_render -- --presets all

# A subset, a fixed output dir, and the neutral master width
cargo run --release --example fidelity_render -- \
    --presets pink_floyd_time_solo,eagles_hotel_california_solo --out /tmp/fid --width 1.0

# A user DI corpus: <dir>/manifest.toml lists the WAVs (see below)
cargo run --release --example fidelity_render -- --di ~/.config/rusty-riff/fidelity/di

# Reference excerpts (<dir>/<preset-stem>.wav), LUFS-matched before comparison
cargo run --release --example fidelity_render -- --ref /path/to/refs

# Level-matched blind A/B/X files + key + listening-notes template
cargo run --release --example fidelity_render -- --abx pink_floyd_time_solo:eagles_hotel_california_solo

# CPU: realtime factor per preset at 44.1/48/96 kHz (warn above rtf 0.5)
cargo run --release --example fidelity_render -- --bench

# Baseline drift check (and how to regenerate after an intended voicing change)
cargo run --release --example fidelity_render -- --check docs/fidelity/baseline-synth-48k.toml
cargo run --release --example fidelity_render -- --presets all \
    --write-baseline docs/fidelity/baseline-synth-48k.toml
```

Outputs land under `--out`: `<preset>/<di>.wav`, `report.toml` (full per-render
metrics + third-octave LTAS) and `summary.csv` (scalar columns). A DI manifest
looks like:

```toml
[[di]]
file              = "strat_neck_bends.wav"
guitar            = "Strat, neck single-coil"
calibrated        = true              # recorded through rusty-riff after `N`
trim_db           = 0.0               # applied by the harness only if not calibrated
reference_version = 1
```

To record your own DI: calibrate (`N`), press `R` for a dry take, save the
session (`J`); the take WAV in the session folder is already calibrated.

The baseline covers the `chugs` phrase; `--check` tolerates
lufs_i ±0.1, crest ±0.2 dB, correlation ±0.02, centroid ±1 %, and ±0.25 dB per
LTAS band. Regenerate it in the same commit as any intended voicing change.

## Reference calibration (maintainers)

There are two different "calibrations" — don't conflate them:

- **User calibration** (`N`) is a per-device **trim** stored in
  `~/.config/rusty-riff/input-calibration.toml`. Any user, any guitar; run `N` and
  save. It makes whatever they plug in hit the fixed engine reference.
- **Engine reference** is the project-wide set of `target_peak_dbfs` values in
  `src/audio/calibration.rs` plus `REFERENCE_VERSION` and the harness baseline.
  It only changes when the *reference rig itself* changes.

### When to re-measure

| Situation | Action |
| --- | --- |
| New guitar, already-measured class (humbucker / single-coil) | Just `N` + save. No code change. |
| New guitar, a class never measured (e.g. first P90) | `N` on it; if its P99 is > ~1 dB off the provisional target, set that class's target (below). |
| Different interface or normal gain | Just `N` + save — the trim adapts to the fixed reference. |
| Decide to re-standardize the engine level (re-voicing, Phase 5) | Full procedure below. |

### The procedure

1. On the **reference rig** (interface + gain position + guitar the presets are
   voiced on), run `N` for each pickup class and write down the **Measured P99**
   from the Result screen. Use the same interface gain for every class so the
   numbers reflect the pickups, not the gain.
2. For each class that differs from the current target by more than ~1 dB, set
   `target_peak_dbfs` in `src/audio/calibration.rs` to the measured value and bump
   `REFERENCE_VERSION` by 1. (The synthetic corpus is normalized from the
   humbucker target automatically — `src/analysis/synth.rs::humbucker_peak()`.)
3. Regenerate the baseline in the same commit:
   `cargo run --release --example fidelity_render -- --presets all --write-baseline docs/fidelity/baseline-synth-48k.toml`,
   then `--check` it.
4. Record the rig and the measured numbers in `docs/fidelity-implement.md`.
5. Existing saved calibrations with an older `reference_version` will toast
   "input calibration is from an older reference — recalibrate (N)"; re-running
   `N` fixes them.

Calibration is **hardware** state: it is never written into presets or sessions
and is never reset by `Params::reset`.

## License

rusty-riff is released under the [Apache 2.0 license](LICENSE). It is a modified
fork of [rusty-amp](https://github.com/danylokravchenko/rusty-amp) by Danylo
Kravchenko; see [NOTICE](NOTICE).
