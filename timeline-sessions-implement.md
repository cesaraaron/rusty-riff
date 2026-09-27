# Timeline & sessions — implementation notes (handover)

Companion to [`timeline-sessions-plan.md`](timeline-sessions-plan.md). The plan is
the **design / acceptance** document; this file is the **as-built** record:
what exists, the invariants to preserve, the known limitations, and what is still
missing. Read both before reviewing or continuing.

Branch: `timeline-sessions`, cut from `main`. Commits so far:

```
79692e9 fix(session): don't truncate assets when re-saving in place
e51f32b feat(session): save and load portable project folders
eabd816 feat(timeline): mirror the external rig onto the take bus
9b86bc7 feat(timeline): multitrack playback and re-ampable dry raw takes
```

---

## 1. Architecture at a glance

```text
        ┌────────────────────────── control / UI thread ──────────────────────────┐
        │  Session (src/session.rs)      canonical project, project ticks          │
        │  PracticeUi (src/ui/practice.rs) owns the Session, decoding, capture     │
        │  SessionBrowser (src/ui/sessions.rs) save/load/recovery modal            │
        │  IrBrowser / PluginBrowser / AmpBrowser  build plugin + IR instances     │
        └───────────────┬──────────────────────────────────────────────┬──────────┘
                        │ rtrb command rings (lock-free)                │ std::mpsc
                        ▼                                               ▼
        ┌────────────────────────── audio thread ──────────┐   ┌──── worker threads ────┐
        │ InputState::on_input                              │   │ capture writer (WAV)   │
        │   PlayerVoice (bounded ID-addressed track table)  │   │ decode workers         │
        │   live DspChain  → live guitar                    │   └────────────────────────┘
        │   take_chain     → summed raw takes (2nd rig)     │
        │   import bus + metronome → monitor only           │
        │   dry capture ring                                │
        └───────────────────────────────────────────────────┘
```

Two audio-thread rig instances share `Arc<Params>` but hold independent DSP state:
`chain` (live input) and `take_chain` (summed raw takes). Imports and the
metronome are monitor-only.

### Key files / symbols

| Area | File | Notes |
| --- | --- | --- |
| Runtime project model | `src/session.rs` | `Session`, `Track`, `TrackKind`, `TrackLifecycle`, `ticks_to_frames` |
| Persistence | `src/project.rs` | `Manifest`, `write_session`, `read_manifest`, `list_sessions`, `Manifest::into_session` |
| Multitrack playback | `src/dsp/player.rs` | `PlayerVoice`, `TrackSlot`, `MAX_TRACKS = 16`, `Frame { take, import_l, import_r }` |
| Engine / realtime | `src/audio/mod.rs` | `TrackCommand`/`TrackAck`, `InputState::on_input`, take-bus rings, dry capture |
| Dry capture | `src/recording.rs` | `CaptureState`, SPSC ring, writer worker, `CaptureResult` |
| Timeline UI | `src/ui/practice.rs` | `PracticeUi`, `SaveContext`, decodes, capture lifecycle, session save/load |
| Session modal | `src/ui/sessions.rs` | `SessionBrowser`, `Action` |
| Plugin/IR browsers | `src/ui/{plugins,amp_plugins,ir_browser}.rs` | twin live/take instances |
| Rig snapshot | `src/preset.rs` | reused as the session `[rig]` (unchanged) |

---

## 2. What is implemented (by increment)

### D1 — multitrack playback + dry raw takes
- `Session` is the canonical track list; decoded buffers are **caches** in
  bounded audio slots, addressed by `TrackId` with a `generation` guard.
- `PlayerVoice` renders two buses: `take` (mono sum of unmuted takes) and
  `import` (stereo, monitor only). Per-track gain + mute.
- The take bus is processed by a **second `DspChain`** with independent state.
- Raw capture is **dry** (pre-gate/pedals/amp), pushed into an `rtrb` ring by the
  callback and drained by a writer worker to a mono float WAV under
  `~/.config/rusty-riff/recovery/<session-temp-id>/`.
- `R` arms a new row (auto-plays if paused) / stops; loop out-point auto-stops
  capture **before** wrap. Stale worker results are rejected by generation.
- Scrollable timeline rows, import-at-playhead (`B`), `+`/`-` seek step,
  `G` gain modal, `Del` remove.

### D2 — external-rig mirroring
- The active external **IR**, **AU amp** and **CLAP insert** are instantiated a
  **second time** for the take bus (`set_*_take` engine methods and parallel
  rings). Parameters are fanned to both instances by the browsers.
- If the second instance cannot be created, the live one still loads and the UI
  reports that takes fall back to the built-in rig.

### Persistence — sessions
- A session is a portable folder `~/.config/rusty-riff/sessions/<name>/`:
  `session.toml`, `audio/track-<id>.<ext>`, `irs/cabinet.<ext>`.
- `J` opens the browser: `N` new, `S` save, `A` save as, `Enter` load, `D` delete.
- Save copies imported originals + dry takes into `audio/`, the active IR into
  `irs/`, writes the manifest atomically (temp + rename), then **retargets each
  track at its project copy**.
- Load applies the built-in rig and external IR, replaces the track list,
  decodes/installs every asset, and restores playhead/loop/seek-step/metronome.

### Recovery — abandoned dry takes
- Every finalized capture writes a metadata sidecar; `list_recovery` surfaces
  unsaved takes; the `J` browser offers **restore** (into the current session) or
  **discard**; a session save that incorporates a take deletes its recovery copy.

### Offline export
- `src/export.rs`: worker renders the **unmuted raw takes** through a fresh
  `DspChain` built from a frozen `Preset` snapshot, at the project sample rate,
  to a stereo 32-bit float WAV (temp + rename, `ExportHandle` progress/cancel).
  Tail is included and capped; imports/metronome/live input are excluded.
- External IR is re-loaded at the export rate. A loaded CLAP insert is
  re-instantiated with opaque state (`PluginState` save/load, `load_with_state`)
  and a loaded AU amp with a parameter snapshot + routing/latency, via
  `BuildExternal` closures built on the UI thread and run on the export worker.
- Timeline `E` opens a destination dialog; a progress modal runs; `Esc` cancels.
- Dependency fix: added symphonia's `pcm` codec feature — `wav` alone is only the
  RIFF reader, so no WAV ever decoded before (including captures).

### D3 — timeline UX (see [`timeline-ux-plan.md`](timeline-ux-plan.md))
- **Empty-timeline navigation.** `seek_by` no longer clamps to `extent_ticks()`
  (zero on an empty session); it clamps only at `0`, so `←`/`→` step into empty
  space and the first import lands where the playhead is. The transport total is
  `max(extent, position)`.
- **Time ruler.** A one-line ruler under the transport shows `m:ss` ticks at a
  spacing chosen to avoid collisions, plus a `▼` playhead marker, aligned to a
  fixed 27-column waveform gutter shared by every row (the old `uncal` tag that
  shifted rows by six columns is now a one-column `!` flag).
- **Fractional steps.** `SEEK_STEPS = [0.5, 1, 5, 10, 30]`; `Session::seek_seconds`
  is `f32`, and `TransportSection.seek_seconds` deserializes both the old integer
  form and the new float form. The clip-move modal nudges by the step and its
  `+`/`-` cycle it.
- **Live waveform.** `recording::LivePeaks` (~5 ms buckets) is shared between the
  capture writer and the UI; a recording row draws the growing envelope. The
  writer only ever touches it alongside the existing WAV drain, and the UI reads
  it with `try_lock`, so the audio callback and the redraw are unaffected.
- **Taller rows + `Tab` zoom.** Rows default to three lines drawn as a symmetric
  min/max envelope. `Tab` on the timeline cycles `Normal → Expanded(one row fills
  the pane) → Split(two rows 50/50) → Normal`; deleted rows sanitize the zoom.
- **Waveform resolution (F6).** Peaks are stored adaptively at ~100 buckets/s
  (`practice::peak_buckets`, clamped `[1024, 65536]`) for imports and takes, and
  each render column aggregates the min/max over its whole time span, so
  transients are no longer skipped. The envelope is drawn from the real `lo`/`hi`
  around zero instead of a peak-to-peak rectangle. New sessions default to the
  1 s seek step (`DEFAULT_SEEK_STEP`).

---

## 3. Invariants — do not break

- **RT safety (`AGENTS.md`).** No allocation, blocking, filesystem IO, or
  `Vec`/plugin drop in the audio callback. Displaced tracks/plugins are returned
  via rings and dropped on the control thread. The capture ring is fixed-size;
  overflow is flagged, never silently truncated.
- **Coherence per callback.** All `TrackCommand`s are drained at the top of
  `on_input`, so a block sees a whole track set. Install success/failure is
  reported back via `TrackAck`.
- **Generation guard.** A decode/capture for id `X` at generation `G` must not
  populate a row that has been replaced; `finish_decode` uses the pending
  generation, `poll_capture` rejects mismatched generations.
- **Take alignment uses `CaptureState::start_frame`** (the exact project frame of
  the first sample), **not** the arm-time playhead. (This was buggy: the value
  was read from an always-zero `CaptureResult.start_frame`; fixed — `result.start_frame`
  was removed and `poll_capture` reads the shared atomic.)
- **Project ticks, not engine frames, are persisted.** `Session::ticks_to_frames`
  (round half away from zero) is the single conversion shared by playback and the
  future exporter. The project rate is adopted from the first engine and kept
  across device changes (`PracticeUi::rate_adopted`).
- **Portable paths only.** `resolve_asset` rejects absolute paths and `..`, so a
  session cannot reference files outside its folder.
- **Re-saving in place is safe.** `write_session` skips copying a file onto
  itself; without this, loading then saving would truncate the asset.

---

## 4. Known limitations / warnings

1. **Plugin state is captured (CLAP opaque, AU parameters).** Sessions save the
   loaded CLAP insert (`plugins/insert.state`) and AU amp
   (`plugins/amp.params` + routing) and restore them on load into both chains and
   the browser. AU opaque `ClassInfo` state is **not** captured — only exposed
   parameters — so AUs with non-parameter state may not restore exactly. Plugin
   binaries are not portable: a missing/renamed bundle degrades to the built-in
   rig with a message.
2. **Offline export includes the external rig, best-effort.** `src/export.rs`
   renders the unmuted takes through a fresh rig; a loaded CLAP insert is rebuilt
   from its captured opaque state and a loaded AU amp from its parameter snapshot.
   Export refuses only if an AU is loaded but its state cannot be captured. Range
   is tick 0 → last unmuted take + a capped tail; stereo 32-bit float at the
   project rate. Explicit loop/selection export and stem options are not offered.
3. **Recovery is best-effort.** Unsaved takes are indexed and offered for
   restore/discard at launch (§5). A take is GC'd from recovery once a session
   save incorporates it. `abort_capture` deletes an in-progress partial file.
   There is no periodic recovery-folder cleanup beyond incorporation/discard.
4. **Clip move is keyboard-only.** Imports/takes can be moved with the timeline
   `H` modal (0.1 s / seek-step nudges, reset to top); there is no drag-and-drop,
   and moving is applied via a `TrackCommand::SetStart` in-place update.
5. **Take bus loads two plugin instances.** CPU/plugin count roughly doubles
   while an external plugin/IR is loaded. Benchmarked by design, not measured;
   watch for CPU in release builds.
6. **`MAX_TRACKS = 16`.** A hard cap with a UI message; no long-track streaming.
   A 10-minute stereo f32 decode is ~230 MB, so eager decoding of many long files
   is bounded by this limit.
7. **Session name is not unique-enforced.** Saving two sessions with the same
   sanitized name overwrites in place; Save As derives the folder from the name.
8. **Recovery dirs are per-`Session` (`temp_id`), not per-launch.** Loading or
   New creates a new temp id, so recovery folders can accumulate.
9. **`take bus` plugin latency** is not compensated between the live and take
   buses; only the built-in-vs-AU compensation inside one chain exists.
10. **No loop/region export selection.** Monitoring loop does not (yet) bound an
    export.

---

## 5. Done — recovery indexing + prompt

Abandoned dry takes are now discoverable and restorable (§7):

- Each finalized capture writes a sidecar `recovery/<temp-id>/take-<id>.toml`
  (`project::RecoveryMeta`: id, name, `start_ticks`, project/source rate, frames,
  overflowed). A WAV without a sidecar still surfaces with a default record.
- `project::list_recovery()` scans the recovery root into `RecoveryTake` records;
  `discard_recovery_file()` removes a take + sidecar and prunes an empty folder;
  `is_recovery_asset()` identifies incorporated sources for GC.
- The `J` browser lists recoverable takes under a **"recoverable takes"** header:
  `Enter` restores one into the current session (a `RawTake` row at its saved
  start tick, decoded off-thread); `D` discards it.
- A session **save** that incorporates a recovery file deletes that file
  (verified incorporation). The row now points at the project copy.
- On the first engine start each launch, if recovery entries exist, the browser
  opens with a prompt.

---

## 6. Testing & verification

- Unit tests: `src/session.rs` (tick conversion, selection, seek steps),
  `src/dsp/player.rs` (bus split, offset, loop, `set_start`, full-table
  rejection), `src/recording.rs` (writer finalize / abort / empty take),
  `src/project.rs` (round-trip, sanitizer, path-escape, in-place resave, plugin
  state sidecars), `src/export.rs` (float-WAV render, clip placement, empty-job
  refusal), `src/practice.rs` (32-bit float WAV decode), `src/ui/practice.rs`
  (export job filters muted/imports), `src/dsp/cab/external.rs` (IR duplicate).
- UI snapshots (insta): `src/ui/snapshots/` — update deliberately with
  `INSTA_UPDATE=always cargo test` or `cargo insta review`.
- CI commands: `cargo fmt --all -- --check`,
  `cargo clippy --all-targets --all-features -- -D warnings`, `cargo test`.
  `cargo build --release` and `cargo check --no-default-features` are also kept
  green.

### Verification status

| Check | Status |
| --- | --- |
| `cargo fmt --check` / `clippy -D warnings` / `cargo test` (292 tests) | green |
| `cargo build --release` (macOS, `clap`+`au`+`pipewire`) | green |
| `cargo check --no-default-features` | green |
| Hardware monitoring, loop-aligned capture, twin/export plugin CPU, device change | **not yet run** — needs `cargo run --release` on real hardware |
- **Hardware-only checks (not automated):** real audio monitoring, capture
  alignment under a loop, plugin CPU with twin instances, device changes.
  Run `cargo run --release` for these.
- Known slow test: the `dsp::` suite takes ~2.5 minutes in debug (unrelated to
  this work).

---

## 7. Where to look first when reviewing

1. `src/audio/mod.rs` → `InputState::on_input` — the realtime contract and the
   exact output mixing order.
2. `src/ui/practice.rs` → `poll`, `finish_decode`, `poll_capture`, `arm`,
   `save_session`, `load_session`.
3. `src/project.rs` — manifest shape, atomic write, path safety.
4. `src/session.rs` — the time base and conversion rule.
5. `src/ui/sessions.rs`, `src/ui/plugins.rs`, `src/ui/amp_plugins.rs`,
   `src/ui/ir_browser.rs` — modal/action wiring and twin-instance lifecycle.

### Open review questions (from the plan §10)

- **Q1 shared take bus vs per-take rig:** decided **shared bus** (implemented).
- **Q2 transport after stop / loop end:** **decided** — stopping a take (manual
  `R` or loop end) finalizes it and **pauses** the transport, parking the playhead
  on the take. `Space` plays/pauses from any timeline row; `M` mutes; `Enter`
  jumps to the loop in-point or frame 0.
- **Q3 export range/format/tail cap:** **decided** — tick 0 → last unmuted take
  + capped tail (12 s cap, 250 ms silence hold), stereo 32-bit float at the
  project rate. Implemented.
- **Q4 bindings:** `G` gain, `H` clip move, `J` sessions and timeline `E` export
  are in and settled.
- **Q5 non-restorable plugin policy:** **decided for export** — refuse with an
  actionable message rather than silently substituting a built-in rig. Plugin
  *state* capture remains the open work to lift the refusal.
