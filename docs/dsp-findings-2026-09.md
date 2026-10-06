# DSP, chain-order, output & export findings — 2026-09

A review of `src/dsp/**`, `src/audio/**`, `src/export.rs` and the chain-order
machinery, written before the rework starts. Everything here is either a
**verified defect** (reproduced or traced to a specific line) or a **modelling
gap** (a real thing the code does not model). Nothing has been implemented yet.

**Status of the working tree when this was written.** `HEAD` is
`2527c61 chore: wip amp dsp and preset revoice before transparency work`, which
re-voiced amps and presets **without** re-blessing the fidelity baseline. The
gate currently reports **164 tolerance violations** — that is pre-existing, not
caused by anything in this document. Phase 0 below is the re-bless.

---

## How to read the severity tags

| Tag | Meaning |
| --- | --- |
| **DEFECT** | Behaviour contradicts its own documentation, its own test, or arithmetic. Not a taste question. |
| **GAP** | The code is self-consistent but does not model something that materially affects the result. |
| **RISK** | Correct today, but load-bearing on an invariant nothing enforces. |

---

# Phase A — verified defects

No tone change. These land first, each with the test that should exist and does
not.

## A1 — The output limiter overshoots to +1.21 dBFS · **DEFECT**

`src/dsp/mod.rs:2043`

```rust
fn soft_limit(x: f32) -> f32 {
    let a = x.abs();
    if a < 0.95 { x }
    else {
        let excess = a - 0.95;
        x.signum() * (0.95 + excess / (1.0 + excess * 5.0))
    }
}
```

`excess / (1 + 5*excess)` tends to `1/5 = 0.2` as `excess → ∞`, so the
**asymptote is 1.15, not 1.0**. Computed:

| input | output | dBFS |
| --- | --- | --- |
| 0.90 | 0.900 | −0.92 |
| 0.95 | 0.950 | −0.45 |
| 1.00 | 0.990 | −0.09 |
| 2.00 | 1.118 | **+0.97** |
| 10.0 | 1.1457 | **+1.18** |
| 1e6 | 1.1500 | **+1.21** |

The existing test at `dsp/mod.rs:2114` asserts `max_abs <= 1.05` but only feeds
a **0.8-amplitude** sine, which never reaches the knee — so the ceiling has
never actually been exercised. The doc comment "Transparent soft limiter" and
the harness's ungated `sample_peak` are why this survived.

**Fix.** Raise the denominator coefficient so the asymptote is 1.0
(`0.95 + excess / (1.0 + excess * 20.0)` → `0.95 + 0.05`). Add a test that
drives `|x| = 4.0` and asserts `≤ 1.0`.

**Secondary.** `soft_limit` is a memoryless nonlinearity at **base rate**, so it
generates its own alias products on the final output. It should be wrapped in
`Oversampler4` (already available at `src/dsp/oversample.rs`).

## A2 — Monitor buses are summed *after* the limiter · **DEFECT**

`src/audio/mod.rs:976` runs `chain.process_block`, whose master bus is
`widen → soft_limit` (`dsp/mod.rs:2024-2051`). Then:

- `audio/mod.rs:1047-1048` adds the metronome click (`CLICK_GAIN = 0.5`,
  `dsp/metronome.rs:25`), the practice player, and the looper.
- `audio/mod.rs:1087-1088` adds the take bus — which went through **its own**
  `master_bus` and its own `soft_limit` inside `take_chain` (`audio/mod.rs:1074`).

Two independently-limited buses are then summed with **no final limit**. At
1.15 each that is **+2.3 dBFS** into the converter, and it grows with every take
layer. The metronome click alone takes 1.15 + 0.5 = 1.65.

Separately, the tuner bypass (`audio/mod.rs:962-974`) writes the raw
post-calibration input straight to the output with **no `master_bus` at all** —
no widener, no limiter, no DC blocker.

**Fix.** Add a final per-channel `soft_limit` after the sums at `audio/mod.rs:1089`,
and route the tuner path through `master_bus`. (Moving the sums *pre*-master-bus
is cleaner but changes how the click is levelled, so the final-limit approach is
the lower-risk one.)

## A3 — `Biquad` coefficient rebuilds zero the filter state · **DEFECT**

`src/dsp/biquad.rs:16` — `from_coeffs` hard-sets `z1: 0.0, z2: 0.0`.
`Biquad::set_high_shelf` (`biquad.rs:89`) exists specifically to recompute
coefficients *in place* — its doc says "so a live control change does not click"
— and it **has zero call sites**. Every live path instead reassigns a fresh
`Biquad`, discarding two samples of filter memory:

| Site | Filters reset | Trigger |
| --- | --- | --- |
| `distortion.rs:104-105` | 2 | DS-1 TONE, every 0.001 of knob travel |
| `fuzz.rs:82` | 1 | fuzz GUITAR |
| `metal_core.rs:107,113` | 2 | ML-2 LOW / HIGH, ±15 dB shelves |
| `dsp/mod.rs:152-154` | **6** | param EQ **and** pre-amp EQ |
| `graphic_eq.rs:67-68` | **14** | GE-7, on *any* band move |
| `clean_boost.rs:45-46` | 2 | boost BASS / TREBLE, ±12 dB |
| `cab/mod.rs:635` | 2 | MIC knob, 0.05 per keypress |
| `randall.rs:127-129,135` | 4 | Randall bass/mid/treble/presence |
| `vox.rs:215`, `tweed.rs:101` | 2 | Vox CUT, Tweed tone |

For a ±12 dB shelf the discarded state `z1` is a large fraction of signal
amplitude, so each is an audible zipper. This directly contradicts the
`AGENTS.md` constraint "Biquad state must be preserved across buffer boundaries".

**Fix.** Add `set_low_shelf` / `set_peak_eq` / `set_highpass` / `set_lowpass`
mirroring `set_high_shelf`, and route every live rebuild through them.

**Highest-impact single fix in this document.** Nine call sites, one primitive.

## A4 — Every mix / level / gain is an unsmoothed per-sample multiply · **DEFECT**

There is no smoothing anywhere in `src/dsp/effects/`. Worst offenders:

| Site | Step size |
| --- | --- |
| `tube_screamer.rs:101` | `shelf_gain` reaches **117×** (41 dB) as DRIVE moves, applied raw |
| `clean_boost.rs:58` | 24 dB GAIN step, per sample |
| `compressor.rs:38,52-53` | recomputed per sample with **no** `param_changed` guard |
| `graphic_eq.rs:93` | `level != self.last_level` is an **exact float compare** while bands use `param_changed(ε=0.001)` — any jitter rebuilds 14 filters |
| `pitch.rs:85,113` | varispeed read rate + wet gain |
| `wah.rs:119`, `reverb.rs:202`, `flanger.rs:126`, `chorus.rs:83`, `phaser.rs:153`, `delay.rs:123,146` | wet/dry crossfade |

**A related defect:** the compressor is **never unity**, even at its most
transparent setting. `compressor.rs:52` —
`auto_makeup = db_to_lin(-thresh_db * (1 - 1/ratio) * 0.5)`. At `sustain = 0`:
`thresh_db = −6`, `ratio = 2`, so `auto_makeup = db_to_lin(1.5) = 1.19`.
Engaging the compressor is a hard **+1.5 dB step** before any compression
happens, and `level * 2.0` compounds it to as much as +6 dB.

**Fix.** A shared one-pole gain smoother (`OnePoleLp` already exists at
`effects/mod.rs:88`) on every output-scaling coefficient. Make the compressor's
auto-makeup unity at its most transparent setting. Use `param_changed`
consistently in `graphic_eq`.

## A5 — Tremolo has no dry path: 4 ms latency, and it leaks into the Fender · **DEFECT**

`tremolo.rs:89`

```rust
let del_ms = CENTER_MS + pitch_depth * SWING_MS * sine;   // CENTER_MS = 4.0
```

At `pitch_depth = 0` — which is pure tremolo, the **default**
(`DEFAULT_TREM_MODE = 0.0`, `dsp/mod.rs:637`) — the tap is a **constant 4 ms**,
and the output is `wet_l * gain` with **no dry path at all**
(`tremolo.rs:103`). So the effect always imposes 4 ms, and bypassing it removes
4 ms instantly.

Worse: `amp/fender.rs:202` calls

```rust
let (tl, tr) = self.trem.process(x, x, speed, intensity, 0.0, 0.0);
```

with `mode = 0.0` — so **the Fender Twin model is permanently 4 ms late relative
to the other eight amps**. Switching amp models produces a 4 ms jump, audible as
slapback against any reverb and against the take bus.

**Fix.** Add a dry path, and give the Fender a dry path so the nine amp models
stay latency-aligned. Add a test that all nine are within 1 sample of each other
on an impulse.

## A6 — Phase 90 and Electric Mistress collapse stereo at mix = 0 · **DEFECT**

`phaser.rs:127-132` and `flanger.rs:100-105` fold the input **before** the mix:

```rust
let (in_l, in_r) = if phase90 { let m = 0.5 * (l + r); (m, m) } else { (l, r) };
...
in_l * (1.0 - mix) + wet_l * mix,
```

At `mix = 0` a fully-transparent Electric Mistress outputs `(m, m)` — a **hard
mono fold of the entire stereo rack**, before it does anything else. The tests
(`phaser.rs:192`, `flanger.rs:141`) only exercise `kind = 0.0` (the generic
stereo mode), so the mono variants are completely untested.

**Fix.** Keep the un-folded pair for the dry path; use the mono sum only to
drive the wet taps. Add `kind = 1.0` bypass-transparency tests.

## A7 — Export does not match what you heard · **DEFECT**

Four independent causes, largest first:

1. **IR truncation + energy normalisation.** Live loads `LIVE_MAX_IR_LEN = 8192`
   (`ui/practice.rs:1278`); export loads `OFFLINE_MAX_IR_LEN = 32768`
   (`export.rs:176`). `load_ir` → `normalize_pair` (`external.rs:144`)
   **energy-normalises**, so the same file at two lengths has different gain and
   a different tail. For any IR longer than ~171 ms this is several dB.
2. **No preroll.** `render_with_chain` starts cold at `written = 0`
   (`export.rs:280`). Every amp's rectifier sag, the cab convolution, the DC
   blockers, the reverb and the oversampler histories are uninitialised. The
   harness *does* preroll (`analysis/render.rs:62`, `preroll_s = 0.5`) — so the
   harness and the export measure different things. Every export also opens with
   **128 leading zeros** from the convolver.
3. **Sample rate.** Export renders at `job.sample_rate` (project rate), live at
   the device rate. Legitimate — it is a re-render, not a capture — but it should
   be *said* in the export dialog.
4. **Take-bus staging.** Live sums two separately-limited buses post-limiter
   (A2); export runs one `master_bus`.

**Fix.** Add the preroll. Normalise both paths to a common reference — prefer
mid-band loudness over energy, which is what the harness already treats as
perceived level. Warn in the export dialog when live truncation is in effect and
when the render rate differs from the live rate.

## A8 — The noise gate opens in 0.21 ms · **DEFECT**

`noise_gate.rs:49`

```rust
let gain_coeff = if target > self.gain { 0.9 } else { 0.999 - release * 0.009 };
```

`coeff = 0.9` is a one-pole with a 10-sample time constant ≈ **0.21 ms @48 kHz**.
Gate *opening* is therefore effectively a hard gate on a non-zero waveform —
a click on every note. Real gates open in 0.5–2 ms for exactly this reason.

Also at `noise_gate.rs:37-39`, `release_ms` is computed and then discarded:

```rust
let release_ms = 10.0 + release * 490.0;
let _ = release_ms; // used for future hold extension
```

and the *detector* release is hardcoded to 100 ms in `new()`, ignoring the knob
entirely. The RELEASE knob does not do what the panel says.

**Fix.** Realistic attack constant. Either implement hold or remove the dead
code and the misleading knob range.

## A9 — The fidelity harness cannot catch A1 · **GAP**

`examples/fidelity_render.rs` **records but does not gate** `sample_peak`,
`noise_floor_db`, `lufs_st_max`, `side_to_mid_db`, `punch_p95_med`,
`treble_mod`. `sample_peak` being ungated is precisely why A1 survived.

Also: `ltas_third_octave` is **mean-normalized** (`metrics.rs:75`), so it is
structurally blind to broadband level error — a uniform +6 dB gain regression
would pass.

**Fix.** Add a `sample_peak ≤ 1.0` gate. Add checks for **DC offset** and
**aliasing** (neither is measured anywhere today). Promote the
recorded-but-ungated scalars to gated with tolerances justified by a
measurement run.

---

# Phase B — chain-order safety

## B1 — Bypass is a hard per-sample cut with no ramp · **RISK**

`dsp/mod.rs:1328-1347` (`mono_stage!` / `stereo_stage!`): when a stage is
disabled, `process()` **is not called at all**. That was the right call for
*bypass transparency* (fixed previously and verified by a test matrix), but it
has two consequences:

- **Frozen state.** Delay buffers, all-pass states, oversampler histories,
  envelope followers and LFO phases all stop at their pre-bypass values, so
  re-enabling resumes from a stale state — a step discontinuity at re-enable.
  Amplified by: a flanger at `feedback = 0.9, mix = 0.5` can reach ~12× the dry
  (bounded by the test at `flanger.rs:162`); reverb wet is **3× dry**
  (`SCALE_WET = 3.0`, `reverb.rs:10`).
- **No ramp.** A bypass toggle is a hard cut of a large-signal waveform. The
  engine has exactly one declick ramp and it is for the built-in↔AU amp switch
  (`DECLICK_SECS = 0.004`, `dsp/mod.rs:1393`).

**Fix.** Short (3–5 ms) declick ramps at the *stage boundary*, mirroring the
AU declick. This preserves the transparency property while removing the click.

**Related doc defect.** The `BlockRoute` doc (`dsp/mod.rs:1374`) claims routing
is "read **once** per block … so a toggle landing mid-block cannot change the
topology partway through". In fact `stage_enabled` is read **per sample**
(`dsp/mod.rs:1828`) and every knob is re-read per sample inside each effect.
The guarantee holds for the *routing* decisions (amp/cab/`skip_cab`) but not for
bypass. Either narrow the doc or extend the snapshot.

## B2 — Reordering gives the user no warning about three destructive consequences · **RISK**

Pressing `]` on a stage can silently:

1. **Destroy all stereo and lose up to 3 dB.** Anything stereo placed before
   the Amp is folded by `Sig::into_mono` (`dsp/mod.rs:1362`), and the Amp always
   runs (`run_ordered_stage:1817`). Move the Reverb, Chorus, Flanger, Phaser,
   Trem, Delay or GE-7 before the Amp and the whole rack collapses to mono.
2. **Downmix the rest of the chain to dual mono.** A *mono* pedal placed after
   the Cab emits `Sig::Stereo(y, y)` (`dsp/mod.rs:1834-1837`), killing cab
   decorrelation and the reverb tail for everything downstream — permanently.
3. **Feed post-mic signal to line-level effects.** With a full-rig AU,
   `skip_cab` is true (`dsp/mod.rs:1578`), so anything between AMP and CAB
   processes the AU's already-miked output. This is finding **R3** from the
   2026-09-25 review, marked resolved, but resolved only by a help-modal row
   (`ui/draw.rs:1312-1315`) — there is no code-level guard.

None of this is surfaced at the time of the move. The behaviour is deliberate
and tested (`live_mono_stage_after_stereo_downsamples_to_mono`,
`dsp/mod.rs:2303`), but it is a far bigger sound change than "move" implies.

**Fix.** Toast on a move that places a stereo rack pedal before the Amp, or a
mono pedal after the Cab. Consider **rejecting** (not just warning) the
stereo-before-Amp case, matching the existing `amp_precedes_cab` refusal at
`ui/input.rs:384`.

## B3 — The two ordering invariants are enforced inconsistently · **RISK**

`amp_precedes_cab` is checked in the UI move (`ui/input.rs:384`) and repaired in
`sanitize_chain_order` (`dsp/mod.rs:495`), but `set_chain_order`'s only guard is
a `debug_assert!` (`dsp/mod.rs:1260`). In a **release** build, a
cab-before-amp order would be accepted: the Cab would make the signal stereo and
the Amp would fold it back to mono, so **the entire rack would run mono**. It
would not crash — it would just quietly sound wrong.

**Fix.** Promote the invariant to a runtime check inside `set_chain_order`. It
runs on the control thread under the writer mutex, so the cost is free.

## B4 — No gain compensation on reorder · **GAP**

Moving the GE-7 or param EQ pre-amp puts up to **+12 / +15 dB per band** into
the amp's clipping stage. Moving the Compressor post-cab applies
`auto_makeup × level*2` to an already-clipped signal, with no headroom
awareness. Nothing in the dispatcher applies a per-position trim.

There is **no test** that a reordered chain keeps a sane level —
`swapped_chain_order_changes_output` (`dsp/mod.rs:2152`) only asserts the output
*differs*.

**Fix.** A test over a set of legal permutations asserting the level stays in a
sane band. Consider a documented per-stage trim for the EQs and compressor when
they sit outside their intended region.

## B5 — Reordering does not reset state · **RISK**

A moved stateful stage carries its buffers, envelopes, and LFO phases into a
completely different signal regime. `DspChain` owns exactly one instance of
each effect (`dsp/mod.rs:1395-1416`) and the order is a pure index walk. Moving
the Reverb before the Amp means the amp clips the reverb's already-ringing tail
and its DC-blocking stages see a very different spectrum.

`AGENTS.md` explicitly protects amp sag state across model switches, so the safe
scope is **feedback-loop stages only** (reverb, delay, flanger, phaser). Decide
explicitly, and document whichever way it goes.

## B6 — A single `]` can leap a stage past an invisible one · **RISK**

`move_selected_stage` (`ui/input.rs:364-389`) swaps with the next **rendered**
neighbour, but off-board stages keep their slots (`rendered_stages`,
`ui/input.rs:55-72`). So one keypress can move a stage past a hidden one —
confirmed as *intended* by `move_selected_stage_moves_amp_and_cab_separately`
(`ui/input.rs:1051`), but surprising when the pedal is later re-added.

**Fix.** Show hidden stage positions in the ribbon, or refuse to leap.

---

# Phase C — amp architecture rework

The single biggest tone win, and the largest blast radius. Sequenced so each
sub-step is independently listenable and re-blessable.

## C1 — There is no gain staging; `gain` is a compression control · **GAP**

Each clip stage is normalised `clip(u*g) / sqrt(g)`. So a stage's small-signal
gain is `0.6366*sqrt(g)` and its **saturated output is `1/sqrt(g)`**.

I hand-computed the Marshall at max gain, where `g1 = 12.81`, `g2 = 7.00`:

| quantity | value |
| --- | --- |
| nominal `pregain` | 40 |
| small-signal cascade gain | **+11.7 dB** |
| *fully clipped* preamp output | **0.105 — 19.5 dB below unity** |

Transfer at 0.5-peak input:

| gain knob | output | dB |
| --- | --- | --- |
| 0.0 | 0.253 | −5.9 |
| 0.5 | 0.276 | −5.2 |
| 1.0 | 0.254 | −5.9 |

**~0.8 dB of level authority, and it is non-monotonic.** `gain` is a
drive/compression control; total level is fixed by `master × trim`.

Compounding it: every `atan` stage costs **−3.9 dB** at small signal, two
stages = −7.9 dB, and that insertion loss is absorbed by fixed `VoiceBalance`
shelves of up to **+9 dB** (`mesa.rs:148`) that the user **cannot dial out** and
that partially cancel their own bass/treble moves. `hiwatt.rs:153-156`
documents reducing this from +4 dB to +1.5 dB for exactly that reason.

**Target.** Real small-signal gain per stage; compensate the `atan` insertion
loss explicitly rather than with a fixed shelf. This is what makes `gain` mean
gain and gives the master knob somewhere to go.

## C2 — There is no negative feedback · **GAP**

No loop gain, no gain reduction, no damping factor anywhere. The only NF-adjacent
artefact is `DynamicPresence` (`amp/mod.rs:351-384`): a level-dependent HF shelf
placed **after** the output transformer and the speaker load, driven by the
pre-sag *level envelope* rather than by a supply or loop variable.

Consequences:

- presence cannot tighten the midrange the way a real NFB circuit does;
- the presence range does not narrow when the amp is cranked (real NF does
  exactly this, because the loop loses authority *and* the stage gains less);
- the master pot has **no** interaction with the loop — on a real JCM800 the
  master *is* part of the feedback divider;
- the LF reduction of loop effect that produces the classic "scooped, tight"
  character when the loop is closed is absent.

The Vox AC30 is where this hurts most. `vox.rs:49-51` documents that the real
AC30 has **no global NFB loop** and therefore sags readily — the model
implements that as a shallower sag *constant* (0.75) plus a bigger speaker bloom,
not as a topology.

**Target.** A feedback divider around the power stage, with the **master pot in
the divider**, LF-shaped so closing the loop produces the scoop, and loop
authority falling as the supply sags. Move presence into the divider.

**This is the sub-step that will change the presets most.** C1 and C2 together
will make the `VoiceBalance` shelves wrong, because those shelves were tuned to
compensate for exactly what C1 and C2 add. Budget a second listening pass.

## C3 — Sag *reduces* clipping under load · **DEFECT (inverted model)**

Across all 8 tube models, `power_amp` computes `clip(u * supply * drive)` where
`supply = sag < 1` under load (e.g. `marshall.rs:217`,
`os_power.shape(x, |u| tube_clip_asym(u * supply * 2.2) * 0.62)`).

A sagging supply pushes **less signal into the clipper**, so clipping
*decreases* under load. A real rail sag **lowers the clipping threshold** and the
amp compresses **harder**. As written this is a 1/k-slope level compressor
dressed as a power supply, and it is backwards on the one axis that matters most
for feel.

Also missing: the sag is a scalar attenuator rather than a rail voltage, so it
never shifts the operating point and produces no additional asymmetry or bias
shift — the two defining audible consequences of rail collapse. And it is
frequency-independent, driven by `|x|` of the pre-power-amp signal, with no
dependence on output current and no LF/HF split.

**Target.** Model the sag as a rail voltage. A collapsing supply should
compress harder, shift the bias, and drop the loop gain (coupling this to C2).
Make the envelope output-current dependent.

## C4 — One clipper function for five tube types · **GAP**

`tube_clip_asym` is **byte-identical** in marshall / mesa / plexi / vox / hiwatt
/ fender / supro / tweed (I diffed them; the only difference is one comment line
and a trailing period):

```rust
if x >= 0.0 { FRAC_2_PI * x.atan() }
else        { FRAC_2_PI * (x * 1.1).atan() }
```

A 12AX7, an EL84, a 6V6, a 6L6 and a KT77 all clip through the same curve with
the same hard-coded **1.1** asymmetry. Models differ only by scalar drive
constants, so the **harmonic fingerprint** of the JCM800 and the AC30 differ
only in level.

Related, in the same family:

- **No 3/2 power law.** The defining valve relationship (h2 ∝ drive²,
  h3 ∝ drive³) is absent. All even harmonics come from injected DC offsets.
- **No grid-conduction limit.** `GridBlock` (`amp/mod.rs:299-338`) reduces gain
  on hard positives but never clamps them, so there is no flat-top.
- **The dominant asymmetry mechanism is not a tube property.** It is `Bloom`
  injecting a level-proportional DC offset: Marshall `0.06`, Tweed `0.18` —
  **32×** the `CathodeBias` depth. The touch-sensitivity test
  (`tube_amps_are_touch_sensitive`, `amp/mod.rs:1329`) passes because of that
  offset, and `fidelity-implement.md:385` records that the Tweed value was
  *raised* to make the test pass. The rework must preserve touch sensitivity
  through the mechanism, not the offset.
- **Power stages reuse the h2-rich preamp curve.** A push-pull transformer output
  is odd-harmonic dominant.
- **The Mesa's power stage uses the *diode* exponential** (`mesa.rs:194`, rails
  +1.0 / −0.785) — the same curve its own comment describes as a **preamp**
  stage (`mesa.rs:283-288`). A Recto's discrete push-pull output hard-clips at
  the rail.

**Target.** A parameterised tube curve (knee/μ, conduction angle, grid-conduction
flat-top, perveance) with a per-model instance, plus the 3/2 relationship.

## C5 — Per-model defects and gaps · **DEFECT + GAP**

- **`vox.rs:124` is a mathematically dead filter.**
  `presence_shelf: Biquad::high_shelf(sr, 4500.0, 0.0)` is never updated and has
  no knob. A 0 dB RBJ high shelf is *exactly* unity (`b0=1, b1=a1, b2=a2`).
- **`marshall.rs:55`** documents "inter-stage coupling HP at ~720 Hz". The code
  is **300 Hz** (`marshall.rs:119`), with a comment at `:115-118` explaining the
  deliberate drop. The class doc was never updated.
- **`marshall.rs:254-255`** says "the preamp gain is split across the two
  triodes (g1·g2 = pregain)". It is `1.4 * 1.6 * pregain = 2.24 * pregain`.
- **No hum or noise generator anywhere.** `rg "hum|noise"` over `src/dsp/amp/`
  returns nothing; a silent input produces bit-exact zero out of every model.
  `SupplyRipple` (`amp/mod.rs:256-283`) modulates the *signal*, producing
  ghost-note sidebands but **zero hum in silence**. Only 5 of 9 models have
  ripple at all — Vox, Hiwatt, Fender and Randall have none, and all four are
  hum-bearing in reality (the Vox most of all).
- **`OutputTransformer`'s `tanh` (`amp/mod.rs:421`) is the only in-amp
  nonlinearity still at base rate** — every other one is at 8×.
- **Six amps share three `Components` sets** (`tonestack.rs:36-83`): Mesa uses
  `FENDER` (`mesa.rs:142`), Supro uses `FENDER` (`supro.rs:85`).
- **Tone stack has zero test coverage.** `tonestack.rs` has no `#[test]` at all
  and no test anywhere references the scoop depth or mid frequency.
- **Tweed** has no tone stack (correct for a 5E3) and no presence.
- **Master volume is a post-clip linear multiply** (`marshall.rs:304`
  `x * master * 6.73`), applied *after* the DC blocker. It does not reduce power
  drive, does not reduce sag, does not change the clipping amount, and does not
  interact with presence. Coupled to C2.
- **Detail is very unevenly distributed.** Marshall 16 mechanisms, Mesa 16,
  Plexi 14, Hiwatt 13, Vox 12, Fender 12, Supro 11, Tweed 10, **Randall 9**.
  Randall is architecturally different (no passive stack, no OT, no sag, no
  ripple, no dynamic presence) and is the only model with an active Baxandall
  EQ — the exact topology `tonestack.rs:2-9` argues against.
- **`SpeakerLoad` is passed the level envelope while its parameter is named
  `sag`** (`amp/mod.rs:135` vs `marshall.rs:291`).

---

# Phase D — cabinets

## D1 — Cab selection does not change the amp's speaker load · **DEFECT — FIXED**

`SpeakerLoad` (`amp/mod.rs:94-151`) hard-codes a specific cabinet's resonance
**per amp model**:

| Amp | fs | Q | comments in code |
| --- | --- | --- | --- |
| Marshall | 95 Hz | 1.0 | "4×12" |
| Mesa | 100 Hz | 1.0 | |
| Randall | 90 Hz | 1.0 | no dynamics at all |
| Vox | 85 Hz | 1.3 | "2×12 Alnico" |
| Hiwatt | 90 Hz | 0.9 | "WEM" |
| Fender | 80 Hz | 0.9 | "open 2×12" |
| Supro | 110 Hz | 1.0 | "1×10" |
| Tweed | 95 Hz | 1.0 | |

`AmpBank::process` never receives the `CabModel`. So a JCM800 into an Orange
PPC412 still applies a 95 Hz "4×12" resonance, and a Tweed into a Greenback
still applies a 4×12. `fidelity-implement.md:614` states this outright:
"Local to the amp (no amp↔cab feedback loop)".

**DONE.** `CabModel::speaker_load()` is the table and `SpeakerLoad::set_load` retunes
the resonance in place. The bundled baseline came back **byte-identical**, because every
preset already pairs each amp with a matching cab — but cross-pairings move ±0.5 to
±2.4 dB at the speaker's fundamental. See `fidelity-implement.md`.

## D2 — No driver resonance and no port model · **GAP**

The "resonant sub HP" in every cab voicing is **Q 1.1–1.2**, which is a **0.3 dB
peak** — not resonant. The perceived low-octave rise is entirely the following
`peak_eq`, i.e. a *voicing* choice.

What is missing:

| Real phenomenon | Status |
| --- | --- |
| Driver Fs peak — 3–6 dB, **Q 3–6** on every 12" driver | absent (Q 1.2 HP instead) |
| Port tuning / Helmholtz transfer, and the ~180° phase reversal above tuning | absent |
| 3-peak impedance shape (Fs / ~400 Hz baffle / ~2.5 kHz breakup) | absent |
| Cabinet / edge / baffle diffraction | absent; `TEX_REFL` times are hand-authored |
| Internal standing waves | cosmetic — two sinusoids at gain **0.004** (−48 dB, inaudible) |
| Air loading / box compliance | absent |
| Power response (SPL per watt) | absent — a 1×10 differs from a 4×12 only by a highpass and a level trim |

**The driver Fs peak is the single biggest difference from a real 4×12 in the
low octave.**

## D3 — Loudness matching is hand-tuned and untested · **GAP**

The cab trims span **0.708 (Mesa) to 1.82 (Tweed)** — a 12.9 dB raw spread,
deliberately flattened. `fidelity-implement.md:382` records this as "a
product-loudness choice, not physics". That part is defensible. The problems are
downstream of the choice:

- **Energy normalisation is not loudness normalisation.** `ir.rs:125-132`
  normalises the IR *body* to its *skeleton* energy. An LF-heavy and an
  HF-heavy voicing land at very different perceived levels.
- **The trim is applied after `mic_sat`** (`cab/mod.rs:773`), so the cabs'
  nonlinear operating points differ by up to 4.4× in relative terms.
- **`ExternalIrCab` has no `level` field at all** (`external.rs:200-206`) and
  returns raw `mic_sat(l), mic_sat(r)`. Switching to an external IR — or loading
  a preset that uses one — can jump the level. `f23bbb4` removed a *dead* trim
  here but left no replacement.
- **The two paths normalise differently**: built-ins match body-to-skeleton
  energy; external IRs are unit-energy over the whole pair. Different references.
- **No test.** `dsp/mod.rs:3268` covers **three** cabs with a 1.6× bound. The
  8-cab claim rests on `examples/rig_loudness.rs` output, which is not a gate.
- **No user-facing cab level knob** exists anywhere.

**Fix.** Normalise on mid-band energy (what the harness already treats as
perceived level), add an 8-cab loudness test, and give `ExternalIrCab` a
matching level so the paths are interchangeable.

## D4 — The cab's nonlinearities are not oversampled · **DEFECT — FIXED (drive stage)**

`rg oversampl src/dsp/cab/` returns nothing. The cab contains **five** nonlinear
or time-varying elements, all at base rate (`cab/mod.rs`):

| Element | Line |
| --- | --- |
| `cone_breakup` `tanh` | `:63-67` |
| Bl droop `1/(1+Kd²)` | `:201` |
| thermal compression | `:208` |
| Doppler fractional-delay modulation | `:210-220` |
| `mic_sat` `tanh` | `:711-713` |

The existing alias test (`cab/mod.rs:1034-1045`) drives a **220 Hz** sine and
probes 6.5–12 kHz. Its 2nd/3rd harmonics (440/660 Hz) land far below every LP
corner, so the test **cannot detect** harmonics generated by `cone_breakup`
aliasing. It would not catch a bright, high-gain amp input.

The Doppler delay line is also only 8 samples (0.17 ms @48k) and is
length-modulated by a base-rate signal — a linear time-varying filter, which
produces HF images.

**DONE.** The drive stage runs at `Oversampler4`: the 7 kHz -> 1 kHz fold-back went
from **0.43 to 0.00011** of the fundamental (−72 dB), and the alias test's bounds went
from descriptive (0.50) to a real gate (0.005). `mic_sat` was left at base rate on
purpose — it is transparent to 0.25% at full scale. See `fidelity-implement.md`.

## D5 — Cab re-selection re-emits stale convolution · **DEFECT — FIXED (incl. ramp)**

`CabBank` (`cab/mod.rs:778-823`) keeps all 8 cabs alive so state survives model
switches — but the **inactive** cab's `FftConvolver` is never advanced *nor*
reset. `FftConvolver::process` (`conv.rs:149-160`) returns `out_buf[fill]`
before pushing the new sample, so `out_buf` still holds the last block computed
while that cab *was* selected.

Mesa → Marshall → Mesa therefore re-emits **~128 stale samples (2.7 ms)**, then
a stale `fill` phase, with the FDL full of foreign input for a further ~93 ms
(`ir_len`). Frozen state also persists in `SpeakerDrive.env`, `dop_pos`,
`ConeSpread.pos`/`buf`, `GrilleEcho.pos`/`buf`, `MicPosition.last_pos`.

There is **no declick ramp for cab switching** — `declick_*` is AU-only — and
`params.cab_model()` is read **per sample** (`dsp/mod.rs:1608`), so a change can
land mid-buffer.

**Fix.** Reset on selection, plus the B1 ramp.

**DONE** (see `fidelity-implement.md`): `CabBank` tracks the live cab and clears
the incoming one on every transition, via new `clear()` methods down to
`FftConvolver`. The subtle part was that clearing the *time-domain* buffers is not
enough — the frequency-domain delay line (`x_re`/`x_im`, a ring of `k` past input
spectra) is what actually carries the ~93 ms of foreign input, and it is invisible
in a reading of `process`. The declick ramp is also in (worst step across a switch: 0.033 ramped vs 0.268
hard-cut, 8.2x), reusing `BYPASS_DECLICK_SECS` from B1. Both cabs run for the 6 ms
fade, which also stops the outgoing one going stale while it fades.

## D6 — Cab latency is invisible · **GAP — PARTLY FIXED, headline claim was wrong**

The convolver adds exactly **128 samples (2.67 ms @48k, 2.90 ms @44.1k)** and it
is exposed nowhere. `const P: usize = 128` is **private** (`conv.rs:38`); there
is no accessor. `rg -i latency src/` finds no reference to the cab's own
latency. Three consequences:

1. **Built-in ↔ AU A/B is misaligned by 2.67 ms.** `dsp/mod.rs:1980-1987`
   delays the built-in path by exactly `amp_external_latency` and does **not**
   subtract the built-in cab's own 128 samples, so a toggle jumps in time. The
   4 ms declick hides the click, not the timing.
2. **Every exported WAV begins with 128 zeros** (2.7 ms of leading silence).
3. **No user-visible latency readout**, despite the machinery existing and
   `ui/amp_plugins.rs:386` already showing a *host plugin's* `latency_ms`.

**PARTLY DONE.** `P` is now `pub` with a `pub const fn latency()` and a test pinning
128 / 2.67 ms / 2.90 ms.

**The claimed misalignment does not exist.** It says `comp_delay` fails to subtract
the cab's 128 samples. Traced both cases: for an amp-only AU our cab runs on the
AU's output, so both paths carry the same 128 samples and they cancel in the
difference; for a full-rig AU the cab is skipped entirely and the AU's reported
latency already covers its own cab. `delay = amp_external_latency` is correct in
both. I implemented the subtraction, found it wrong, and reverted it with the
reasoning left in the code so nobody repeats it.

Still open: the UI readout, and the export's 128 leading zeros — the latter is a
consistent 2.67 ms offset on every render, so trimming it would re-bless everything
for no benefit.

## D7 — Smaller cab items · **DEFECT / RISK — first 3 fixed**

- `cab/mod.rs:592-606` `MicBlend::recombine` runs on the **audio thread** and
  does 2×4458 multiply-adds plus `2K` forward FFT(256) — **~70 FFT(256) ≈
  0.3–0.5 ms**, 12–19% of a 128-frame budget in a single call. Bounded today
  (UI steps 0.05, no MIDI target), but with no rate limit or per-block dedupe.
  **Hoist to the control thread.** *(rate-limited to one per 128-sample block, which
  bounds the worst case. A frequency-domain blend that removes all FFTs from this path
  was built and measured **slower** — it streams six ~110 KB spectra per call where the
  FFT scratch stays in L1 — so it is reverted. See `fidelity-implement.md`.)*
- `cab/mod.rs:592` indexes `ribbon_l`/`room_l` with `close_l.len()` as the
  bound. A ragged IR would **panic on the audio thread**.
- `ir.rs:138-146` calls `exp()` and `sin()` per mode per sample —
  ~10.5 M transcendental pairs at startup, the bulk of the measured 221 ms
  `CabBank::new`. Replace with a complex-oscillator recurrence.
- `DEFAULT_MIC_BLEND = 0.0` / `DEFAULT_MIC_ROOM = 0.0` (`dsp/mod.rs:509-511`)
  mean a fresh start plays **only** the two SM57 IRs; 4 of the 6 synthesized
  IRs per cab are built and never used. (Bundled presets do use them.)
- `cab/mod.rs:448` `GrilleEcho::frac_tap`'s "`d ≥ 1`, so this never reads the
  just-written x" is a comment, not a `debug_assert`.
- `mic_pos` is wired to `0.0` on the external-IR path (`dsp/mod.rs:1605`) even
  though `external.rs:198-237` documents and tests it as live, and commit
  `2ba7306` deliberately keeps the knob lit in the UI. **A lit knob that does
  nothing.**
- `GrilleEcho`'s normalisation is computed from the unfiltered tap gains, giving
  an unconditional **−0.181 dB broadband** tilt including the LF where its
  filtered taps contribute nothing. *(fixed; the existing test was pinning it)*

## D8 — What is genuinely good here

Worth stating, because the gaps above understate it:

- The FFT convolver (`conv.rs`) is a real **UPOLS** engine, allocation-free on
  the audio path, and **validated against direct convolution** (`conv.rs:310-337`).
- The neighbour-cone (`cab/mod.rs:276-354`) and grille/dust-cap
  (`cab/mod.rs:420-469`) models are genuinely derived from geometry, with
  test-enforced delay windows so a constant cannot silently drift.
- `ir.rs`'s `assert_plausible` / `assert_lr_symmetry` / `assert_modes_realized`
  enforce that every shipped texture is realizable in the IR window, that L/R
  stays symmetric, and that every authored mode actually rings. Real rigour.
- The dynamic behaviour tests (`loud_signal_is_power_compressed`,
  `transient_punches_through_thermal_compression`,
  `cone_breakup_thickens_with_level`, `bass_modulates_treble_only_when_driven`)
  verify drive-dependent behaviour, not just EQ.
- The body-before-modes normalisation ordering (`ir.rs:117-132`) is a subtle bug
  correctly avoided, and the comment explains why.

---

# Phase E — pedals

| # | Item | Site | Severity |
| --- | --- | --- | --- |
| E1 | **Fuzz oversampling is too low for the harshest nonlinearity in the pedals.** Near-square, 2–3 cascaded stages at up to `1 + fuzz*120` gain — while the strictly less aggressive ML-2 uses 8×. | `fuzz.rs:57` (4×) → `Oversampler8` | GAP |
| E2 | **`tape_sat` is an un-oversampled `tanh` on the delay's *feedback* path.** Alias products recirculate through the loop and can self-sustain. | `delay.rs:118-119,134-135` | DEFECT |
| E3 | **Per-sample transcendentals.** `uni_vibe.rs:64,73,74` runs **11 per sample** — and the four stage frequencies only change at ≤8 Hz. `phaser.rs` 7, `compressor.rs` `log10`+2 `powf`, `noise_gate.rs:35` `powf`, `pitch.rs:85` `exp2`+2 `cos`. | `src/dsp/effects/*` | GAP |
| E4 | **Linear wet/dry crossfades** (`in*(1-mix) + wet*mix`) in flanger, chorus, phaser, vibe, delay, reverb — a ~3 dB dip at centre for uncorrelated paths. Equal-power where the wet is not phase-correlated. | `flanger.rs:126`, `chorus.rs:83`, `phaser.rs:153`, `uni_vibe.rs:87`, `delay.rs:123,146`, `reverb.rs:202` | GAP |
| E5 | **Linear-delay interpolation zippers**, worst where the wet is loud: flanger at `fb=0.9, mix=0.5` reaches ~12× dry. Makes B1's ramps matter most here. | `flanger.rs`, `chorus.rs`, `pitch.rs` | GAP |
| E6 | **`mic_pos` dead on the external-IR cab path** — a lit UI knob wired to `0.0`. | `dsp/mod.rs:1605` | DEFECT |
| E7 | **No guitar→amp input impedance / cable-capacitance model.** The Fuzz Face has a GUITAR knob, but the amp does not, so a passive volume pot cannot clean up the amp the way it should — and the Vox "Top Boost" is a `BrightCap` (a 2200 Hz HP tap) rather than the real network that changes input impedance and interacts with the volume pot. | `vox.rs:114`, amp front end | GAP |
| E8 | **Noise gate** — see A8. | `noise_gate.rs` | DEFECT |

### Bypass behaviour, in one place

Every pedal is a hard, per-sample, zero-latency bypass (`dsp/mod.rs:1328-1347`).
When disabled, `process()` is not called — see **B1** for the consequences.

---

# Phase F — output and export

| # | Item | Site | Severity |
| --- | --- | --- | --- |
| F1 | Real ceiling + oversampled limiter. Also: per-channel gain reduction means a hard-panned peak shifts the image; no release time; no true-peak/inter-sample detection. | `dsp/mod.rs:2043` | DEFECT |
| F2 | Final limit after the monitor sums; tuner path through `master_bus`. | `audio/mod.rs:1047,1087,962` | DEFECT |
| F3 | **Master-bus DC blocker.** DC relies on each amp's 12 Hz `out_hp`. With `skip_cab` (full-rig AU) an AU's DC reaches the output unfiltered. Add ~5–10 Hz HP after `soft_limit`. | `dsp/mod.rs:2025-2028` | DEFECT |
| F4 | **The widener is not power-normalized** — side content gains **+2.3 dB in power** at the shipped `DEFAULT_MASTER_WIDTH = 1.3` — and runs **pre-limiter**, so widening is not level-consistent at peaks. Note R8: 1.3 was kept for compatibility and all bundled presets now set `width = 1.0`, so normalising by `sqrt(width)` is low-risk. | `dsp/mod.rs:2034-2038`, `:515` | GAP |
| F5 | Export preroll + common IR normalisation + a "rendered at X Hz, live at Y Hz" note. | `export.rs:176,280` | DEFECT |
| F6 | Export writes 32-bit float, so no dither is needed — but the file **contains samples above 1.0**, which is legal in float and hard-clips on any 16-bit bounce. Self-resolving once A1 lands; add a clamp regardless. Optionally offer int16 + TPDF dither. | `export.rs:264-269` | RISK |
| F7 | Gate `sample_peak`; add DC and aliasing checks to the harness. | `examples/fidelity_render.rs` | GAP |

### The recording tap, for the record

`audio/mod.rs:1026` pushes `self.in_buf[i]` — the **post-calibration-trim,
pre-everything** mono guitar sample. So a take is:

> post-trim dry input, pre-gate, pre-pedals, pre-amp, pre-cab, pre-master-bus.

That is the right design (a re-ampable DI) and the metronome, player and looper
are all correctly excluded — they are summed at `:1043-1050` and `:1080-1088`,
after the tap. Ring overflow is *reported*, not silently truncated
(`audio/mod.rs:1028-1030`). **The only issue is that the limiter is in the same
place as the tap (A2).**

---

# Ordering and process

| Phase | Content | Baseline re-bless | Risk |
| --- | --- | --- | --- |
| A | Defects only | no | low |
| B | Chain-order safety | no | low |
| C | Amp architecture | **yes, per sub-step** | **high** |
| D | Cabs | yes, per sub-step | medium |
| E | Pedals | yes, per sub-step | low |
| F | Output/export | yes | low |

Per `CONTRIBUTING.md`, each phase runs:

```bash
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --release --lib
cargo run --release --example bench
cargo run --release --example fidelity_render -- \
    --presets all --write-baseline docs/fidelity/baseline-synth-48k.toml
```

with the baseline regenerated **in the same commit** and the directional drift
recorded in `docs/fidelity-implement.md`'s increment log.

**C is sequenced C1 → C2 → C3 → C4 → C5 with a re-bless and a listening pass
after each.** C2 (NFB) and C3 (sag direction) each change character enough that
doing them together makes a regression impossible to attribute.

---

# Open questions for the maintainer

1. **C1 changes the meaning of every `gain` knob in all 8 bundled presets.**
   (Eight, not the twenty this audit was written against: twelve were retired on
   2026-09-29 — see [`retired-presets.md`](retired-presets.md).)
   Do you want the knobs re-voiced to preserve each preset's *audible* character
   (mechanical and safe, numbers change a lot), or the gain semantics fixed and
   then each preset re-tuned by ear toward the reference (slower, and what
   actually gets closer to the records)?

2. **C2 will make the `VoiceBalance` shelves wrong** — they were tuned to
   compensate for the missing gain staging and the `atan` insertion loss, i.e.
   for exactly what C1 and C2 add. Plan is to shrink them toward zero and let
   the tone stack do the work, as a second listening pass over all 8 presets.
   Confirm that is the intent.

3. **Phase 0 is re-blessing a broken baseline.** `HEAD` has 164 violations from
   the unre-blessed WIP commit. Should I re-bless that first so the gate is green
   before starting, or fold the re-bless into A1 (the first commit that moves
   the output) and accept a red gate in between?

4. **D1 (cab-aware `SpeakerLoad`) is a 1-day fix with a large audible effect** —
   but it will move every preset, including the pairs that are currently
   *wrong* (JCM800 + Orange, Tweed + Greenback). Worth doing before C, or after,
   so the C listening passes are not fighting a moving cab baseline?
