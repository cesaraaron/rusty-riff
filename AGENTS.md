# Agents.md — rusty-riff

## Project overview

rusty-riff is a real-time guitar amplifier emulator that runs in the terminal. It captures audio from an audio interface, processes it through a full signal chain (noise gate → overdrive pedals → amp model → cabinet simulation → EQ → flanger → delay → reverb), and writes the processed signal back out. The UI is a ratatui TUI with live VU meters, knob sections, and a preset browser.

**Platform:** multiplatform (via cpal)
**Language:** Rust
**Minimum Rust:** 1.96+

---

## Architecture

### Signal chain (in order)

```text
Guitar input
  → Noise Gate          (envelope follower + gain ramp)
  → Compressor          (peak-follower detector → hard-knee gain computer)
  → Fuzz                (Big Muff style: DC block → 70 Hz HP → two cascaded soft-clips → mid scoop → variable tone LP)
  → TS-808 Tube Screamer (DC block → 340 Hz HP → asymmetric diode soft-clip → variable tone LP)
  → DS-1 Distortion     (DC block → 80 Hz HP → silicon diode hard-clip → active tone LP/HP blend)
  → Pre-amp EQ          (low shelf 100 Hz / mid peak 650 Hz / high shelf 3 kHz — shapes what the amp clips)
  → Clean boost         (linear Power-Boost-style front-end gain; Bass/Treble shelves, no clipping)
  → Amp model           (switchable: Marshall JCM800 | Mesa Dual Rectifier | Randall Warhead | Vox AC30 | Hiwatt DR103 | Marshall Plexi | Fender Twin | Supro Combo | Tweed Deluxe — 8× oversampled)
  → Cabinet sim         (switchable: Mesa 4×12 | Marshall 4×12 | Orange PPC412 | WEM 4×12 | Vox 2×12 | Fender 2×12 | Supro 1×10 | Tweed 1×12 — multi-mic IR)
  → Parametric EQ       (low shelf 120 Hz / mid peak 800 Hz Q 1.5 / high shelf 5 kHz)
  → Flanger             (stereo LFO-swept comb: 0.5–5 ms delay, 0.05–5 Hz rate, feedback capped at 90%, L/R a quarter-cycle apart)
  → Delay               (three modes: digital ping-pong | tape/EP-3 | Binson Echorec drum echo; 0–500 ms)
  → Stereo Reverb       (dual decorrelated Freeverb cores: 8 parallel combs → 4 series allpasses each)
  → Master-bus widener  (stereo mid/side enhancement)
  → Output limiter      (per-channel soft-clip)
```

Every bypassable stage can be toggled independently with `Space`.

**Monitor-only buses.** The practice metronome and the practice player (backing
track + recorded take) are summed into the output **after** the recording tap (see
`audio/mod.rs`), so none of them is ever captured in a rendered WAV. The practice
player reads two pre-decoded stereo tracks against a shared timeline cursor with a
loop region; decoded buffers are installed lock-free and displaced buffers are
disposed of on the control thread.

### Key modules to know

| Area | What it does |
| ------ | ------------- |
| Audio engine | Real-time processing loop — latency-sensitive, no allocations on the hot path |
| Amp models | Three distinct DSP paths (tube soft-clip, silicon clip, solid-state rail-clip) with per-model tone stacks and rectifier sag simulation |
| Cabinet sim | Multi-stage biquad EQ chains that model close-mic'd 4×12 responses |
| TUI | ratatui-based UI: selector row (amp + cabinet), pedals row, amp/FX row, VU meters, practice timeline pane, preset browser overlay; the pedalboard, amp panel, and timeline can be shown/hidden with `1`/`2`/`3` |
| Preset system | TOML files loaded from `./presets/` and `~/.config/rusty-riff/presets/` |
| Practice / jam-along | `src/practice.rs` (shared transport + offline decode via symphonia), `src/dsp/player.rs` (audio-thread `PlayerVoice`), `src/ui/practice.rs` (timeline pane + track browser), `src/dsp/resample.rs` (shared windowed-sinc resampler) |
| Sessions / export | `src/session.rs` (runtime session/track state), `src/project.rs` (portable project folders), `src/export.rs` (offline take render) |
| Plugin hosting | `src/host/` — CLAP effect insert (`clap` feature) and macOS Audio Unit amp override (`au` feature) |
| Analysis / calibration | `src/analysis/` — offline metrics (BS.1770 LUFS, LTAS), the deterministic synthetic DI corpus, and offline preset rendering; `examples/fidelity_render.rs` is the harness; `src/audio/calibration.rs` is the input trim/wizard state |

---

## Running the project

```bash
cargo run            # debug build
cargo run --release  # release build (use this when testing audio — debug can underrun)
```

Startup prompts for input device → input channel → output device. The processed signal is written to all output channels.

---

## Sound analysis tools

The repository includes several standalone analysis utilities under `examples/` for inspecting DSP behavior, cabinet response, and signal-chain characteristics. These are useful when debugging amp models, cabinet simulation, and tone-shaping stages:

- `examples/amp_analysis.rs` — analyzes amp-model behavior
- `examples/cab_analysis.rs` — inspects cabinet response curves
- `examples/cab_spectrum.rs` — visualizes cabinet frequency content
- `examples/au_cab_extract.rs` and `examples/au_params.rs` — inspect AudioUnit cabinet and parameter data
- `examples/cone_interference.rs`, `examples/di_compare.rs`, `examples/knob_match.rs`, and `examples/rig_loudness.rs` — compare signal paths, input stages, and loudness behavior
- `examples/fidelity_render.rs` — the fidelity harness: renders presets offline over the synthetic corpus or a DI manifest, reports loudness/tone/level metrics, and checks against `docs/fidelity/baseline-synth-48k.toml` (see `CONTRIBUTING.md` → "Fidelity harness")

External impulse responses (IRs) for cabinet simulation are stored in `~/.config/rusty-riff/irs/`.

---

## Documentation

The user-facing guide is the in-app help overlay (`K`) plus the README. The
design/as-built notes live at the repository root (`fidelity-plan.md`,
`fidelity-implement.md`, `timeline-sessions-plan.md`,
`timeline-sessions-implement.md`), and the off-path product/engineering roadmap
is `roadmap-next.md`. There is no separate docs website.

---

## Design docs (keep in sync)

- `fidelity-plan.md` is the design/acceptance document; `fidelity-implement.md`
  is the as-built record. Log every routed/topology commit in its increment log.
- `docs/fidelity-references.md` is the evidence matrix for historical gear claims.
- Reference input calibration (the `target_peak_dbfs` values, `REFERENCE_VERSION`,
  and the harness baseline) is a maintainer task with its own runbook in
  `CONTRIBUTING.md` → "Reference calibration (maintainers)"; the measured rig is
  recorded in `fidelity-implement.md`. Don't change those targets without
  following that procedure.
- There is no docs-parity rule for a website; user-facing behavior is documented in
  the README and the in-app `K` reference.

---

## DSP constraints (important for agents touching audio code)

- **No heap allocations on the audio thread.** All buffers must be pre-allocated. Do not introduce `Vec::push`, `Box::new`, channels that allocate, or any other allocating call inside the real-time callback.
- **No blocking on the audio thread.** No mutexes that can be held by the UI thread while the audio thread waits. Use lock-free primitives (atomics, `Arc`, single-producer channels) for communication between the UI and audio threads.
- **Biquad state must be preserved across buffer boundaries.** Filter state lives outside the processing loop and is passed in by reference each call.
- **Knob values are normalized 0.0–1.0 internally.** The TUI displays them as 0–10; conversion happens at the UI layer.
- **Rectifier sag is stateful.** The sag envelope has attack and release times that differ per amp model; do not reset this state on model switch unless explicitly tested.
- **Practice tracks are never freed on the audio thread.** Decode/resample offline on the control thread, hand the finished track over through an `rtrb` ring, and ship any displaced track back for disposal (the same discipline as plugin inserts and external IRs). The transport is a per-callback `Transport` snapshot of relaxed atomics.
