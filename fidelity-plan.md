# Fidelity and real-world routing roadmap

**Companion documents.** This file is the **design / acceptance** document. The
as-built record (what shipped, review findings, invariants, increment log) is
[`fidelity-implement.md`](fidelity-implement.md); the evidence matrix for
historical gear claims is [`docs/fidelity-references.md`](docs/fidelity-references.md).
Read all three before continuing. Where shipped code differs from this plan,
`fidelity-implement.md` is authoritative. (Formerly `plan.md` /
`IMPLEMENTATION-NOTES.md`.)

**Progress at a glance (2026-09-25).** Phase 1 and Phase 2 items 1–4: done.
Phase 0: scaffold only (no sources logged, no harness). Phases 3–5: not started.
The next work is [Next increments](#next-increments--workstreams-a-and-b) below — **Workstream A** (routing hardening found in review)
and **Workstream B** (input calibration + offline reference harness), which are
prerequisites for credible Phase 3–5 fidelity work.

Status: implementation plan, not a claim that the existing presets reproduce the original recordings. This plan was prepared from a code and bundled-preset review; the proposed historical equipment choices still require source checking and listening comparisons. It is intentionally detailed so changes can be shipped and verified in small increments.

## Goals and definitions

1. Make the named amps, cabinets, and pedals respond as plausibly as their actual hardware counterparts, particularly the models used by bundled presets.
2. Make a preset's enabled devices and order match a *documented recording-era rig* where possible. Keep uncertain claims explicitly marked as uncertain; don't infer a session rig from a later touring board or an artist's general preferences.
3. Compare *rig accuracy* and *audible match to the finished record* separately. Studio EQ, double tracking, room/mics, mixing, mastering, guitar/pickups, playing, and input level can make an authentic rig sound unlike the master recording. Conversely, extra EQ and effects can mimic the recording while no longer representing the original rig.
4. Let users hear meaningful order differences: pickup → effects → preamp → power amp → load/cab → microphone → studio effects → output. Preserve old presets and the existing real-time performance guarantees.
5. Prefer a small number of explainable components in bundled presets. Correct the underlying amp/cab/pedal first; add a corrective EQ or additional pedal only when evidence or a repeatable A/B comparison supports it.

**No defensible percentage of similarity can be assigned from reading DSP code.** For each reference, capture or obtain a comparable dry DI, match input and output levels, compare with an appropriate excerpt (ideally an isolated guitar track, where available), and listen blind in addition to measuring. Record an evidence/confidence rating for historical claims and separate listening/measurement findings rather than collapsing them into a single score.

## Current architecture and concrete review findings

| Area | Current behavior / evidence | Why it matters and intended action |
| --- | --- | --- |
| Chain | `ChainStage::default_order()` in `src/dsp/mod.rs` is gate → whammy → wah → compressor → fuzz → TS-808 → DS-1 → ML-2 → pre-EQ → Uni-Vibe → **AmpCab** → GE-7 → EQ → flanger → chorus → phaser → tremolo → delay → reverb. The UI can swap stages with `[` / `]` (`src/ui/input.rs`). | Reordering exists; the missing feature is separate amp/cab boundaries and a clear physical interpretation of stage placement. Most post-cab effects currently mean *studio processing of a mic signal*, not ordinary pedals in a guitar amplifier's speaker cable. |
| Stereo/bypass bug | `run_ordered_stage` in `src/dsp/mod.rs` folds stereo to mono for any mono stage after stereo conversion, including an **off** pedal. An off stereo stage before the amp also promotes mono to stereo, which is subsequently folded. | Bypass should never change signal domain or stereo image. Add an enabled check before bridging domains, plus regression tests with deliberately different L/R signals. Document and test the intentional conversion when a **live** mono effect is placed on a stereo feed. |
| Live order consistency | `Params::set_chain_order` writes one relaxed `AtomicU8` per stage; `chain_slots` reads each slot independently, and normal processing snapshots it per sample (`src/dsp/mod.rs`). | A concurrent swap can temporarily duplicate/omit a stage in the audio thread's view. Publish a *whole* prevalidated order using a realtime-safe atomic snapshot/double-buffer handoff with explicit ownership/lifetime rules, and read once per audio block (or another clearly defined boundary). No allocation, lock contention, or destruction on the callback. Test concurrent updates and complete permutations. |
| Amp/cab boundary | `DspChain::amp_cab` calls `amp_stage` then `cab_stage` as one `AmpCab` slot. External AU amp and cab-only IR have special paths; a CLAP stereo insert runs after all onboard effects. | Split amp and cab stages while preserving old `ampcab` preset semantics. Define whether a hosted AU contains a cab and where its output joins. Keep the CLAP insert's current behavior until insert placement is explicitly designed and tested. |
| Mic/cab fidelity | Built-in IRs are **synthesized**, not recorded captures (`src/dsp/cab/ir.rs`); `src/dsp/cab/mod.rs` adds speaker drive, cone interference, grille echoes, mic EQ/comb, and saturation. External captures go through speaker drive but skip the built-in mic coloration (`src/dsp/cab/external.rs`). | Compare each named cab/mic combination to real, licensed IR references; assess whether the extra synthesized effects improve the match or double-count what the captured IR already contains. Provide level-matched, phase-aware comparisons. Preserve the distinction between speaker behavior, cabinet filtering, and mic/room capture. |
| Amp fidelity | The amp models use model-specific filters and gain stages plus shared abstractions (`src/dsp/amp/`), with 8× oversampled nonlinear sections. Hiwatt, Plexi, and Twin recur across every shipped artist preset. Plexi source calls its 1959 Super Lead *tube rectified* and tunes deep sag/ripple around that assumption (`src/dsp/amp/plexi.rs`). | Verify the **specific model year/revision and schematic** before treating rectifier behavior, channel controls, negative feedback, sag, or speaker coupling as hardware facts. The tube-rectifier assertion in the Plexi model is a high-priority check/correction; do not retune by guesswork. Tests for finite output, control direction, and harmonic distortion do not establish equivalence to the named amplifier. **Plexi rectifier resolved (Phase 3, 2026-09-25):** the model 1959 is silicon-rectified (Unicord 1970 schematic; GZ34 phased out ~1966), and the supply is now retuned to a stiff solid-state rail. |
| Named effects | The Fuzz pedal shares input/tone plumbing between Muff, Fuzz Face, and Tone Bender (`src/dsp/effects/fuzz.rs`). Wah is an **envelope auto-wah**, not a controllable treadle (`src/dsp/effects/wah.rs`). The phaser is a stereo four-stage generalization with feedback; the flanger is a stereo, quarter-cycle-offset swept delay. | Compare circuits and control laws individually. Fuzz Face pickup loading and guitar-volume cleanup, correct fuzz-to-wah interactions, mono Phase 90 behavior, and Mistress/MXR-specific flange responses deserve targeted attention if historical evidence places those devices on a recording. |
| Echo/reverb | Delay has digital ping-pong and EP-3-style single-time tape modes (`src/dsp/effects/delay.rs`); many Pink Floyd preset comments call the latter *Echorec-style*. Twin onboard "spring" invokes the generic Freeverb implementation (`src/dsp/amp/fender.rs` and `src/dsp/effects/reverb.rs`). | Binson Echorec (magnetic drum/multiple selectable heads), Echoplex tape, spring tank, and digital/studio hall are different devices. Implement distinct modes where evidence and sonic impact justify them; otherwise label current ones as approximations. Avoid using an extra hall as a substitute for a spring without an audible reason. |
| TS-808 documentation | Corrected to the circuit (R.G. Keen, *The Technology of the Tube Screamer*, 1998): **no 340 Hz input HP** (the input couples through 1 µF, a few Hz); **720 Hz is the clipping stage's gain rolloff** (`Zi = 4.7 kΩ + 0.047 µF`), modelled as a high-shelf to unity bass and the drive gain above; symmetric diode clip. | **Resolved (Phase 4):** `input_coupling_cuts_sub_bass` (which pinned the wrong 340 Hz cut) was removed and replaced by `bass_passes_at_unity_while_treble_is_boosted`; the exact response still awaits a measured TS-808 sweep. |
| Output bus | `master_bus` applies fixed 1.3× mid/side widening and a soft limiter after the rack/plugin (`src/dsp/mod.rs`). | An always-on widener is a studio aesthetic rather than part of a historical guitar → amp → mic rig. Add a neutral or controllable reference mode, avoid unexpected mono changes, and assess limiter operation at level-matched settings. Keep a protective output ceiling. |
| Preset application | `src/preset.rs` requires `[tube_screamer]`, `[amp]`, `[reverb]`. Most omitted optional effects turn **off** on apply, but omitted `[noise_gate]` and `[cabinet]` retain prior settings; `[chain]` absent resets the order. The docs' broad claim that omitted sections leave current settings unchanged is inaccurate. | Make bundled tones deterministic regardless of the previously loaded preset, while retaining documented compatibility for existing user TOML. Clarify omit/disable semantics in the README and in-app help; add repeatability tests. Fewer enabled pedals need not imply huge TOML files: off sections may be omitted where safe. |

### Order-of-operations caveats

- **Guitar/instrument level:** wah, fuzz, compressor, boost, overdrive, phaser, flanger, and some echo pedals may be placed before an amp; order changes clipping, envelope tracking, and impedance/loading. Fuzz Face-like circuits can be particularly sensitive to what precedes them. The current audio-interface input is already buffered; software cannot recreate passive-pickup loading without an explicit pickup/impedance model and appropriate input assumptions.
- **Effects loop:** a normal amplifier loop is *after the preamp but before the power amp*, at a suitable line/instrument level. It is not the wire between the output transformer and the speaker. Implement a genuine loop by splitting the amp model internally into preamp / loop send-return / power amp; do not label an `Amp` → pedal → `Cab` stage as a standard amp effects loop.
- **Between full amp and cab:** only model this as a *virtual load box / attenuator line-level tap and re-amp*, or a post-power-amp/studio processing path with an explicit load representation. Never imply a conventional pedal is physically connected to the speaker output. Speaker-load response is already represented inside amp models; relocating it may be necessary when formalizing this boundary.
- **After mic/cab:** EQ, compression, modulation, delay, and reverb here are recording/mixing or amp-sim processing. This is a valid sound-design option, but not proof that an artist placed those pedals after the physical speaker.
- **Mono/stereo:** make domain conversions visible. A real stereo effect before a mono amp must be intentionally downmixed/selected; a live mono effect after stereo mic/FX must intentionally sum or use two instances. Off effects must be wire-equivalent. Account for phase cancellation when summing a decorrelated stereo source.

## Implementation phases

### Phase 0 — establish references before changing tones

1. Inventory all 17 `presets/*.toml`: for each, record song/section/album era, proposed guitar and pickups, effects **with order**, amplifier revision/channel, speakers/cab, mic and room, known studio processing, and exact sources. Use contemporaneous interviews, studio/session notes, identifiable equipment photos, schematics/manuals, and reputable measured IRs. Separate *documented*, *plausible*, and *unknown*; avoid stating "the real rig" where evidence is incomplete.
2. Save a versioned reference matrix (recommended `docs/fidelity-references.md` or a `docs/fidelity/` table in the later implementation work) with attribution, recording-vs-live context, dates, access to reference media, assumptions, and the change each source supports. Don't treat all Pink Floyd, Van Halen, etc. recordings as having one universal rig.
3. Establish an offline harness using the existing analysis examples and DSP entry points: repeatable dry DI at known interface level, matched sample rate, same guitar performance through each candidate, rendered stereo WAV, spectral/time-envelope/impulse/THD and IMD comparisons, stereo correlation, and measured integrated/short-term loudness. Keep listening notes from level-matched, preferably blind A/B tests. Test clean, edge-of-breakup, chords, palm mutes, and lead sustain; test guitar-volume roll-off separately when pickup loading is modeled.
4. Capture CPU usage/latency at 44.1/48/96 kHz and ensure the live path remains allocation- and blocking-free. Measurements and analysis scripts may allocate **off** the audio thread.

**Gate:** there is a credible, cited reference for each firm gear claim; where reference material is unavailable, the preset is labeled *inspired by* rather than advertised as an exact session rig. No arbitrary "90% faithful" number.

**Closure (2026-09-25).** Phase 0 is closed **evidence-as-available**:
`docs/fidelity-references.md` cites recording dates/credits (Wikipedia) and the
secondary gear sources that could be verified, and marks everything else
`unknown`/`Source: TBD`. Sourcing every session-gear detail is explicitly out of
scope, so **all presets are *inspired by*** and none is advertised as an exact
rig. Component references for Phase 3 (schematics/specs) are recorded in
`fidelity-implement.md`.

### Phase 1 — make routing and bypass trustworthy

1. In `src/dsp/mod.rs`, change ordered dispatch so a bypassed mono/stereo stage passes the existing `Sig` through untouched. Verify the amp/cab stage still runs once; prevent duplicate/missing stages during UI changes. If a bypassed effect's tails should continue on re-enable, specify that independently of audible passthrough.
2. Replace the per-slot order handoff with an atomic **full-chain snapshot** appropriate for the callback; publish valid permutations only. A concrete option is a preallocated `rtrb` command ring carrying the complete fixed-size order **by value**, consumed at block boundaries into an audio-thread-owned array; keep a single control-thread order for UI/editing and define what happens if the bounded ring fills. This avoids mutating memory still being read by the callback and avoids retiring heap snapshots there. Reuse one order through both built-in and external-AU paths; the per-sample `process` API can snapshot at its own call boundary. No allocation, blocking, or heap-object destruction on the audio thread.
3. Add route-domain tests: off mono effect after stereo IR/room leaves L/R intact; off stereo effect before amp leaves mono intact; live mono after stereo has the documented downmix; two noncommuting *enabled* effects differ when swapped; no stale duplicate stages under fast reorder; save/load preserves the exact order.
4. Move hardwired stereo widening to a configurable **studio master** setting with a neutral reference default or an explicit compatibility setting for old presets; keep output protection independent of coloration. Verify recording and monitor buses still tap at the intended points (`src/audio/mod.rs`).

**Gate:** bypass is transparent, whole-order snapshots are valid, rendered audio reflects a changed order, and the callback keeps its no-allocation/no-blocking guarantees.

### Phase 2 — split amp/cab while preserving historical physical meaning

1. Replace the *logical* `AmpCab` stage with `Amp` and `CabMic` stages in `src/dsp/mod.rs`, with explicit paths for built-in head, external AU amp-only, external AU amp+cab, built-in cab, and loaded IR. Keep the **default audio topology equivalent** while migrating. Treat a full-rig AU as already having a cab/mic, so the separate cab stage is intentionally bypassed; an amp-only AU must still feed the selected cab/IR. If a hosted amp emits stereo, document/select the mono fold-down before a mono IR; don't silently destroy an unrelated stereo path.
2. Define safe, named regions in the UI: guitar/front-end, between amp and cab as *virtual load-box line-level processing*, and mic/studio side. Maintain exactly one `Amp` and one `CabMic` in that order; reject or repair a move placing the cab before its amp. Do not default any ordinary pedal to the speaker-side region. Use visual labels/tooltips to make the semantics obvious, and handle valid/invalid moves explicitly. Continue allowing experimental orders only where their signal-domain and physical meaning is disclosed.
3. Migrate `src/preset.rs` and `[chain]`: absent chain remains the historical default; legacy `"ampcab"` expands to consecutive `"amp"`, `"cab"` at the same location; new saves use the new names; mixed/duplicate/unknown stage lists are sanitized deterministically. Preserve all existing amp/cab model identifiers and user preset values. Add round-trip tests for old bundled and user-shaped TOML plus new placements. Do not silently reinterpret a formerly post-cab pedal as pre-mic.
4. Update the selector/ribbon/board handlers in `src/ui/input.rs`, `src/ui/draw.rs`, `src/ui/mod.rs`, and `src/ui/config.rs` so the amp and cabinet can be picked/moved separately. Keep external-amp, external-IR, tuner bypass, monitoring, and plugin indicators accurate. Preserve interactive response and avoid clicks on changes of routing/model where practical.
5. **Optional next boundary, needed for an authentic loop:** refactor amp DSP into `preamp -> loop -> power amp/output transformer/speaker load` while retaining sag/filter state on model switches and across blocks. Add a line-level send/return stage with controllable gain; put suitable time/modulation effects there for rigs that actually used a loop. This is a distinct piece of work, not accomplished merely by separating `Amp` and `CabMic`. **Status (2026-09-27): closed — not implemented.** It is optional, no bundled preset requires it, and splitting all nine amp models at the tone-stack→power-amp boundary is a large, voicing-risky change; the documented virtual-load-box semantics for the Amp→Cab region stand. Revisit only with a preset that needs a pre-power-amp loop.

**Gate:** legacy presets render the same topology as before migration (except separately documented fixes); independently moving the amp and cab yields predictable, documented sound; a pedal cannot be mistakenly presented as if it were inserted directly in a speaker cable.

### Phase 3 — measured amplifier and cabinet fidelity

Prioritize by shipped-preset usage: **Hiwatt DR103 + WEM/Fane**, **Marshall Super Lead/Plexi + Greenback 4×12**, **Fender Twin Reverb + Jensen 2×12**. Maintain the other amps/cabs, then audit them against their own named hardware as a second pass.

1. For each amp (`src/dsp/amp/{hiwatt,plexi,fender,marshall,mesa,randall,vox}.rs`, `tonestack.rs`), lock down revision, channel/switch settings, actual panel controls, approximate analog operating levels, rectifier/power supply and negative-feedback topology, gain-stage placement, tone stack and speaker/load interaction. Compare control sweeps and dynamic response against hardware or trustworthy re-amp captures under the same DI/input level. Distinguish preamp gain from power-amp drive and master/output trim; don't make a "dimed" preset by just adding a TS-808 because the model is insufficiently driven.
2. Check and correct the Plexi rectifier/sag assumption against the chosen 1959 revision, and audit any other model-specific hardware claim before encoding it as a fixed DSP rule. Preserve bias/sag state when switching models as required by `AGENTS.md`. **Done (2026-09-25):** the 1959 is silicon-rectified from ~1966 (Unicord 1970 schematic), and the Plexi supply was retuned from valve-style sag to a stiff solid-state rail.
3. In `src/dsp/cab/{ir,mod,wem,marshall,fender,mesa,orange,vox}.rs`, benchmark magnitude, phase, early/late decay, mic-position response, room ratio, mono image, and apparent loudness against compatible speaker/cab/mic captures. Verify each modeled mic position corresponds to a real capture or a defensible interpolation. Consider a measured/licensed IR as a reference or an optional built-in replacement when redistribution permits; otherwise accurately label a cab as an approximation rather than a captured SM57/R121 setup.
4. Test each modeled speaker nonlinearity against reference captures under quiet/loud drives; avoid layering synthetic cone spread, grille echoes, and off-axis comb over a recorded IR that already contains those effects. Measure whether the fixed master widener and extra room mics make a mono guitar recording artificially wide.
5. Add tests that check *reference-derived tolerances* (frequency-response and control trajectories, dynamic compression, phase/latency, sensible level behavior), not just non-NaN, harmonic-vs-alias, and model-distinctness checks. Document source files and measurement method for tolerances.

**Gate:** the priority amp/cab pairs behave plausibly with no rescue EQ enabled, at matched loudness and representative input levels; reference data and residual differences are recorded.

### Phase 4 — named pedal, echo, and reverb behavior

Work in order of impact on verified preset rigs; do not add a new circuit just because a preset comment currently names it.

1. **Fuzz and pickup interaction:** separate or better parameterize Muff, Fuzz Face, and Tone Bender circuits (`src/dsp/effects/fuzz.rs`), including input filtering, clipping topology, tone-network behavior, level, output impedance, and cleanup at guitar volume. For Fuzz Face-like models, plan explicit pickup/source-impedance assumptions or a suitable model of the buffered interface input; check fuzz/wah order against the documented session rig. Compare clean-to-dirty trajectories and sustain, not only saturated tones.
2. **TS-808, DS-1, compressor, wah:** reconcile TS high-pass code/documentation (`tube_screamer.rs`) against circuit data; measure drive/level frequency response and gain staging. **TS-808 resolved (2026-09-27):** the circuit data (R.G. Keen) shows no input HP and a 720 Hz *gain* rolloff to unity; the model now matches (see the doc table and `fidelity-implement.md`). A general peak-follower compressor is not automatically a Dyna Comp (the presets are labelled honestly; a real OTA compressor is not built). `wah.rs` now supports a manual treadle plus auto mode, driven by keyboard/MIDI expression.
3. **Modulation:** establish correct mono/stereo mode, sweep range, feedback, waveshape, and position for a documented Phase 90, MXR flanger, Electric Mistress, and Uni-Vibe. Compare measured sweep/notch trajectories (`phaser.rs`, `flanger.rs`, `uni_vibe.rs`). Don't describe a generic four-stage stereo phaser as a precise Phase 90 or a quarter-cycle-offset stereo flange as an Electric Mistress without evidence.
4. **Echo:** retain existing digital ping-pong and EP-3 tape options; introduce a distinct Echorec-like mode *only if reference evidence calls for it*. Model selectable head combinations/spacing, feedback and band limitation as appropriate; the shipped `time` control currently maps `0..1` to `0..500 ms`, so comments that say 310/450 ms can be checked exactly. Keep the old preset `type` mapping backward-compatible; new modes need new values/schema and tests for repeats, feedback, drift, and stereo/mono behavior.
5. **Spring/room:** replace or clearly label the Fender Twin's generic Freeverb-derived "spring" with a model or captured response that has the characteristic spring attack/dispersion/decay and correct location within the amp. Treat the rack Freeverb as a studio room/hall aesthetic unless measured as a named device. Eliminate unnecessary reverb stacking in period-correct presets.

**Gate:** each explicitly named pedal mode is sufficiently different in measured and audible behavior to justify its hardware label; legacy sound and serialized type values change only through documented migrations.

### Phase 5 — rebuild and document the 17 bundled presets

Use the Phase 0 evidence matrix to decide what is on each recording, *including what is unknown*. Start with a sparse period-correct chain and the best-matching amp/cab; then make small, level-matched changes one at a time. Put known tape/room/mix effects on the appropriate side of the mic or in a true loop, and do not add a second EQ to compensate for an uncorrected first EQ/cab. Keep optional "record mix" processing in a clearly identified variant only if it audibly improves a controlled comparison and the product needs that variant; do not silently blend the two goals.

| Presets | Priority audit questions (not assertions about the historical recordings) |
| --- | --- |
| `pink_floyd_time_solo.toml`, `pink_floyd_time_chorus.toml`, `pink_floyd_money.toml` | Verify *Dark Side* session-specific fuzz/boost, wah, Uni-Vibe and echo use and their positions. Check whether an auto-wah can represent any claimed manually rocked wah. Compare dry Hiwatt/WEM/Fane response before enabling two EQs or extra ambience. |
| `pink_floyd_shine_on_crazy_diamond.toml` | Verify the actual *Wish You Were Here* era/session fuzz, boosts, modulation and echo instead of assuming a Ram's-Head Muff + Tube Screamer + Uni-Vibe stack; the current file enables all three plus compression and two EQs. |
| `pink_floyd_comfortably_numb_solo_1.toml`, `pink_floyd_comfortably_numb_solo_2.toml`, `pink_floyd_another_brick_pt2.toml` | Check the two solo sections and *Wall* sessions independently: fuzz choice, compression, Electric Mistress/phase, echo hardware and placement. The existing "Echorec" labels select tape mode, and "Mistress" selects the generic flanger. Avoid assuming the same chain across different takes/songs. |
| `pink_floyd_mother_solo.toml`, `pink_floyd_have_a_cigar_solo.toml` | Verify the *Wall* (1979) and *Wish You Were Here* (1975) leads: amp/cab and any fuzz/boost/echo, and whether the current sparse Hiwatt/WEM starting point matches. |
| `van_halen_beat_it_solo.toml` | Verify EVH's 1982 guest solo (Michael Jackson, "Beat It"): amp, cab, phaser/echo, and double-tracking. The rhythm is not EVH. |
| `guns_n_roses_november_rain_solo.toml` | Verify Slash's *Use Your Illusion* (1991) lead: Les Paul + which Marshall/cab, any wah/boost, and whether the wide delay/reverb is the record's mix rather than the rig. |
| `acdc_back_in_black.toml`, `acdc_highway_to_hell.toml` | Compare the sparse guitar-to-Marshall approach with evidence for each album, rather than assuming identical Plexi setups. `back_in_black` currently claims "no pedals" while still enabling pre-EQ, post-EQ and reverb: decide which are studio processing and make the description precise. |
| `led_zeppelin_stairway_solo.toml`, `led_zeppelin_whole_lotta_love.toml` | Verify session amp, guitar, cabinet, and use (or absence) of Tone Bender, TS, slap delay and compression **per track**. In particular, `stairway_solo` describes a "small-amp" voice while selecting a Plexi/Greenback 4×12 and a TS boost; resolve source evidence before changing the preset. |
| `eagles_hotel_california_clean.toml`, `eagles_hotel_california_solo.toml` | Verify clean/lead amp and guitar parts in the recorded multi-guitar arrangement; the current clean preset layers onboard "spring", chorus, digital delay and rack reverb, and the lead uses compressor + TS + two EQs. A single mono preset cannot reproduce two separately recorded/harmonized lead performances. |

For every edited preset: list only intended enabled effects, explicitly set any persistent state needed for deterministic loading, use `[chain]` only if deviating from default, set amp-specific `[amp.knobs]` where necessary, verify unit/range meanings (especially echo times and fuzz `type`), and replace overconfident "real rig" claims with sourced or qualified language. Update the bundled-presets documentation (README) and any TOML examples in the same change.

**Gate:** loading any bundled preset after any other bundled preset gives the same full rig; its description matches its enabled signal path; a listener can turn off each optional effect and identify why it was included.

## Cross-cutting compatibility, docs, and verification

- Keep old preset files and saved knob values loadable. `ampcab` migration, `type` enums, model names, absent `[chain]`, amp-specific knobs and mic settings need explicit regression fixtures. Don't accidentally override user work or personal presets under `~/.config/rusty-riff/presets/`.
- Update the README, `CONTRIBUTING.md`, and the in-app `K` help whenever the relevant routing/claims/UI change. Follow `AGENTS.md` and `CONTRIBUTING.md`: model/control/preset changes should update those docs in the same commit. (The Eleventy docs site was removed in the de-fork cleanup — there is no website to update.)
- Verify block and single-sample paths where applicable, AU full-rig vs amp-only mode, external IR, CLAP insert, recording tap versus monitor-only practice buses, tuner bypass, mono fold-down, output peak safety, and live parameter/model switches. Maintain preallocation, lock-free control handoff, and stateful filters/sag; do not free displaced IRs/plugins on the callback.
- Run targeted tests after each phase, then `cargo test`, `cargo fmt --check`, and the repository's normal Clippy checks on relevant feature combinations. Perform audio/performance evaluation using a release build (`cargo run --release`) and real interface hardware when available. Repeat broad checks only when later work changes covered code.

## Suggested delivery sequence

1. **Routing correctness patch:** bypass transparency, coherent order snapshots, associated tests and docs.
2. **Amp/cab split + migration patch:** new boundaries, AU/IR handling, UI labels and legacy preset round-trips. Follow with a separate *real effects loop* patch if reference rigs require it.
3. **Reference and fidelity patches:** Hiwatt/WEM, Plexi/Greenback, Twin/Jensen, then other named amp/cab combinations and source-backed pedal modes. Benchmark each change using the same dry DI and comparable output loudness.
4. **Preset patches grouped by recording era/artist:** sparse, sourced settings and updated docs, with controlled before/after audio examples when possible.

Each patch should state which hardware/reference it targets, what was measured or heard, which routing/serialized behavior changes, how legacy presets behave, and what still differs from the reference. The success condition is *documented, repeatable improvement* in rig and audio fidelity—not a larger signal chain or an unsupported promise of an exact match to a mastered recording.

---

## Next increments — Workstreams A and B

Added 2026-09-25 after a code review of the Phase 1–2 work (findings and their
evidence are logged in [`fidelity-implement.md`](fidelity-implement.md#review-2026-09-25)).
Both workstreams are **prerequisites** for Phases 3–5: Workstream A removes
realtime and routing defects that would contaminate any A/B; Workstream B makes
input level reproducible and gives every later tone change a measurable,
repeatable before/after.

Decisions already taken with the maintainer (do not re-litigate without asking):

| Decision | Choice |
| --- | --- |
| Doc layout | `fidelity-plan.md` (design) + `fidelity-implement.md` (as-built) + `docs/fidelity-references.md` (evidence). |
| Dry takes vs input trim | Takes are captured **after** the calibration trim (f32 WAV, cannot clip) so they re-amp/export identically on any interface; the trim is recorded in the session manifest for auditing. |
| Reference input level | Ship **provisional** targets now (trim defaults to 0 dB ⇒ nothing changes until a user calibrates); a later maintainer step (B8) measures the rig the presets were tuned on and bumps `REFERENCE_VERSION`. |

Global rules for every commit in this section:

- Audio-thread code: no allocation, no locks, no unbounded waits, no frees. New
  shared state is atomics or preallocated `rtrb` rings, created on the control
  thread.
- Every commit passes: `cargo fmt --check`,
  `cargo clippy --all-targets --all-features -- -D warnings`,
  `cargo test --all-features`.
- Clippy denies `unwrap`/`expect`/`panic`/`exit` in non-test code: use `?`,
  `unwrap_or_else`, and `anyhow` errors (examples return `anyhow::Result<()>` from
  `main` instead of calling `std::process::exit`).
- Update `fidelity-implement.md` (increment log + any deviation) in the same
  commit as the code.
- Recommended commit order: **A1 → A2 → A3 → A4 → A5 → A6 → B1 → B3 → B5 → B4 →
  B6 → B2 → B7**, then the human step **B8**. (B2 depends on B1; B4 depends on B3
  and B5; B6 depends on B3.)

### Workstream A — routing hardening

#### A1. Bounded chain-order read on the audio thread

**Problem.** `Params::chain_slots()` (`src/dsp/mod.rs`, ~line 1110) spins
without bound while `chain_seq` is odd. It is called from `DspChain::process`
and `DspChain::process_block` on the audio thread. If the UI thread is preempted
between the two `fetch_add`s in `set_chain_order`, the callback waits for the UI
thread to be rescheduled — a priority inversion that violates the "no blocking
on the audio thread" rule. The seqlock is also only correct with a single
writer, which is assumed but not enforced (writers: `src/ui/input.rs:372`,
`src/preset.rs:835/838`, `src/ui/draw.rs:2293`, `Params::reset`).

**Changes (`src/dsp/mod.rs`):**

1. Add to `Params`:
   ```rust
   /// Serializes *writers* of the chain order so the seqlock stays single-writer.
   /// Only `set_chain_order` takes it; the audio thread never touches it.
   chain_write: std::sync::Mutex<()>,
   ```
   (Initialize in `Params::new`. It is not `Arc`-shared separately; `Params` is
   already shared through `Arc<Params>`.)
2. `set_chain_order`: hold `let _guard = self.chain_write.lock().unwrap_or_else(std::sync::PoisonError::into_inner);`
   for the whole odd→stores→even sequence. Keep the existing behavior of
   skipping invalid ids, but add `debug_assert!` that the incoming order is a
   permutation (`sanitize_chain_order(order) == *order`).
3. Add the non-blocking reader:
   ```rust
   /// Audio-thread reader: at most `CHAIN_READ_ATTEMPTS` tries, never waits for
   /// a writer. `None` means "a write is in flight — keep your last order".
   pub fn try_chain_slots(&self) -> Option<[u8; CHAIN_LEN]>
   ```
   with `const CHAIN_READ_ATTEMPTS: usize = 4;`. Each attempt: load seq; if odd,
   `spin_loop()` and continue; read the slots; reload seq; return on equality.
4. Keep `chain_slots()` (blocking until a clean read) but document it as
   **control-thread only** (UI drawing, preset save, tests). Implement it as a
   loop over `try_chain_slots()`.
5. `DspChain` gets `last_order: [u8; CHAIN_LEN]`, initialized in `DspChain::new`
   from `params.chain_slots()` (control thread — fine), and:
   ```rust
   #[inline]
   fn snapshot_order(&mut self) -> [u8; CHAIN_LEN] {
       if let Some(o) = self.params.try_chain_slots() { self.last_order = o; }
       self.last_order
   }
   ```
   `process()` and `process_block()` call `snapshot_order()` instead of
   `params.chain_slots()`.
6. Fix stale comments: every "19 byte stores" / "19-atomic" becomes "`CHAIN_LEN`
   byte stores" (the split made `CHAIN_LEN = 20`).

**Tests (in `src/dsp/mod.rs` tests):**

- `audio_read_never_waits_for_a_stalled_writer`: build a chain, render one block
  (so `last_order` is set), then `params.chain_seq.fetch_add(1, SeqCst)` to fake a
  writer stuck mid-publish. Run `process_block` on a spawned thread and
  `recv_timeout(Duration::from_secs(2))` its result; it must return, and its output
  must equal a reference chain rendering the same input with the same (previous)
  order. Restore the sequence with another `fetch_add` at the end.
- `concurrent_writers_never_tear_the_order`: two writer threads alternately
  publishing two different valid permutations 50k times each, one reader calling
  `chain_slots()` 100k times; every read must be one of the two permutations
  (not merely *a* permutation).
- Existing `chain_order_snapshot_is_never_torn_under_concurrent_swaps` and
  `rapid_reorder_keeps_a_complete_permutation` must still pass.

**Acceptance:** no audio-thread call site of `chain_slots()` remains
(`rg "chain_slots\(" src/dsp src/audio` shows only control-thread/test uses).

#### A2. One route snapshot per block

**Problem.** `process_block` reads `amp_external_active` once per block
(`use_ext_amp`), but the `Cab` arm of `run_ordered_stage` calls
`ext_amp_supplies_cab()`, which re-reads `amp_external_active` and
`amp_external_amp_only` **per sample**. A toggle landing mid-block can run the
built-in amp with the cab skipped for the rest of the block (an unfiltered,
fizzy burst). The per-sample `process()` path never runs the AU, yet still skips
the cab when a full-rig AU is flagged active, so it renders the built-in amp
with no cab. The `amp_stage` doc comment also claims it is "bypassed when an
external amp is active", which is only true of `process_block`.

**Changes (`src/dsp/mod.rs`):**

1. Add
   ```rust
   /// Everything routing-related, read once per block (or per `process` call).
   #[derive(Clone, Copy)]
   struct BlockRoute {
       order: [u8; CHAIN_LEN],
       /// The hosted AU replaces the built-in amp for this block.
       use_ext_amp: bool,
       /// The separate Cab stage is skipped (a live full-rig AU supplies it).
       skip_cab: bool,
       width: f32,
   }
   ```
   and `fn route_for_block(&mut self, allow_ext_amp: bool) -> BlockRoute` that
   calls `snapshot_order()` (A1), reads `amp_external_active`,
   `amp_external_amp_only`, and `master_width` once, and computes
   `use_ext_amp = allow_ext_amp && self.ext_amp.is_some() && active` and
   `skip_cab = use_ext_amp && !amp_only`.
2. `process_block` uses `route_for_block(true)`; `process()` uses
   `route_for_block(false)` and is documented as "always renders the built-in
   rig; never runs a hosted AU".
3. Thread `&BlockRoute` through `process_core`, `run_full`, `run_range`,
   `run_ordered_stage`. The `Cab` arm checks `route.skip_cab`. Delete
   `ext_amp_supplies_cab()` (or keep it only as the helper used by
   `route_for_block`).
4. Fix the `amp_stage` doc comment to say the dispatch (not the stage) swaps in
   the AU, and only in `process_block`.

**Tests:**

- `route_truth_table`: for every combination of {AU loaded, active, amp_only}
  × {`allow_ext_amp` true/false}, assert `use_ext_amp`/`skip_cab`.
- `per_sample_process_keeps_the_cab_with_a_full_rig_au_flagged`: a chain with a
  fake `StereoInsert` AU installed, `amp_external_active = true`,
  `amp_only = false`; `process()` output must be bit-identical to a chain with no
  AU at all (currently fails: the cab is skipped).
- Existing `process_block_matches_per_sample` and the AU full-rig/amp-only
  tests (~lines 2216–2394) must still pass.

**Out of scope (note in `fidelity-implement.md`):** a crossfade when toggling
built-in ↔ AU. A2 only guarantees the switch lands on a block boundary.

#### A3. Render-based preset determinism

**Problem.** `bundled_presets_load_deterministically` (`src/preset.rs` ~1033)
fingerprints the chain order, amp/cab/mic/width selectors and on/off flags only.
`Preset::apply` does reset amp knobs to model defaults and serde defaults
`[fuzz] type` / `[delay] type`, so loading is deterministic today, but the test
would not catch a regression in any of those.

**Changes (`src/preset.rs` tests only):**

1. Extend `rig_fingerprint` with: all `amp_params[model]` knobs of the active
   model; `fz_type`, `delay_type`; and every knob of each **enabled** stage
   (read through the same fields the `mono_stage!`/`stereo_stage!` macros use).
2. Make `hostile_params()` also scramble every knob (e.g. set all to `0.93`),
   `fz_type = 1.0`, `delay_type = 1.0`, and every amp model's knob bank.
3. New `bundled_presets_render_identically_after_hostile_state`: for each
   bundled preset, apply it to a fresh `Params` and to `hostile_params()`, build a
   fresh `DspChain` (48 kHz) for each, render the same 4800-sample deterministic
   signal (e.g. a decaying 110 Hz + 440 Hz sum at 0.5 peak) via `process_block`,
   and assert bit-identical L/R. Keep it under ~2 s in debug; if slower, render
   2400 samples.

#### A4. Document what "between AMP and CAB" means with a full-rig AU

**Changes:** the README and the in-app `K` help — one short
paragraph: with an **amp-only** AU (or the built-in amp), stages between
AMP and CAB are line-level/virtual-load-box processing; with a **full-rig** AU
the CAB stage is skipped, so those stages process the AU's *already-miked*
output (equivalent to post-cab). Add the same caveat to the `[ / ]` row of the
help modal only if it fits one line; otherwise leave the modal alone.
`src/ui/draw.rs` ribbon: optional — when `skip_cab` is true, render the CAB tile
as `AU CAB` (already done) and dim the tiles between AMP and CAB with a `post-mic`
hint. If implemented, re-bless snapshots (`cargo insta review`).

#### A5. Oversized callbacks must not allocate

**Problem.** `InputState::on_input` (`src/audio/mod.rs` ~835–849) calls
`resize` on `out_l/out_r/take_l/take_r/take_in` and `extend` on `in_buf` when
a callback delivers more than the preallocated `MAX_BLOCK = 4096` frames. That
allocates on the audio thread. Rare (large ALSA periods), but a rule violation.

**Changes:** process `data` in chunks of at most `MAX_BLOCK` frames
(`data.chunks(MAX_BLOCK * self.in_channels)`), moving the current body into an
`on_input_chunk` helper; remove the `resize` calls (replace with
`debug_assert!(frames <= MAX_BLOCK)` inside the helper). Per-block snapshots
(transport, metronome, route) are then taken per chunk — acceptable and
documented. Level stores (`levels.input/output`, practice position) happen once
at the end of `on_input`.

**Test:** if `InputState` can be constructed in tests, feed a 10 000-frame
callback and assert the output ring received 10 000 frames per channel and no
buffer's capacity changed (`Vec::capacity` before/after). If construction is
too entangled, extract the chunking into a pure `fn block_ranges(frames, max)`
and unit-test that, and say so in `fidelity-implement.md`.

#### A6. Close-out

Record A1–A5 in `fidelity-implement.md` (increment log, deviations, test
names). Mark the corresponding review findings as resolved.

**Workstream A gate:** no unbounded wait or allocation reachable from
`on_input`; `process()` and `process_block()` agree on routing; all bundled
presets render bit-identically regardless of prior state.

### Workstream B — input calibration and the reference harness

Why: every amp model's breakup depends on input level, and today the level is
whatever the user's interface gain happens to be (there is no trim anywhere in
`src/`). Presets tuned on one interface are therefore under- or over-driven on
another, which is the most likely reason several presets lean on a Tube Screamer
plus two EQs. Without a fixed input reference and a repeatable offline render,
no Phase 3–5 change can be measured.

#### Definitions

- **Engine reference level.** The in-engine digital level a defined performance
  should read after calibration: *hard open-E strums on the bridge pickup,
  guitar volume on 10*, measured as the **99th percentile of 10 ms window peaks**
  over the capture (robust to a single spike). Per pickup class:

  | `PickupClass` | Target (P99 window peak) |
  | --- | --- |
  | `Humbucker` | −19.7 dBFS (measured, B8) |
  | `P90` | −4.5 dBFS (provisional) |
  | `SingleCoil` | −24.6 dBFS (measured, B8) |

  Rationale: the targets are measured on the **reference rig the bundled presets
  were voiced on** (see B8 and `fidelity-implement.md`) so calibration preserves
  the existing preset sound (trim ≈ 0 there) and normalizes other interfaces and
  guitars to the same engine level; the single-coil offset keeps the output
  difference between pickup classes instead of normalizing it away. Verified at
  `REFERENCE_VERSION = 2`; P90 keeps its provisional value until a P90 guitar is
  measured.
- **Trim.** `trim_db = target − measured`, clamped to `[−24, +24]` dB, applied
  as a linear gain to the guitar input before anything else.
- **Uncalibrated.** `trim_db = 0.0` — bit-identical to today.

#### B1. Offline analysis library — `src/analysis/`

Library code for examples/tests only (never called from the audio thread; may
allocate). Add `pub mod analysis;` to `src/lib.rs`.

- `src/analysis/mod.rs` — re-exports.
- `src/analysis/metrics.rs`:
  - Move/generalize (copy, don't delete from examples in this patch) from
    `examples/di_compare.rs`: `db`, `rms`, `goertzel(s, f, sr)`,
    `ltas_third_octave(s, sr) -> Vec<(f32, f32)>` (70 Hz–8 kHz, mean-normalized,
    semitone-grid energy per band as in `di_compare.rs::ltas`),
    `envelope(s, sr, ms)`, `percentile(sorted, p)`, `treble_mod_depth(s, sr)`.
    All take `sr` explicitly (the examples hardcode `SR`).
  - New: `peak`, `crest_db`, `spectral_centroid(s, sr)`,
    `stereo_correlation(l, r)` (Pearson, returns 1.0 for identical, −1.0 for
    inverted, 0.0 for silence), `side_to_mid_db(l, r)`,
    `noise_floor_db(s, sr)` (mean of the quietest 5 % of 50 ms RMS windows),
    `window_peaks(s, sr, ms) -> Vec<f32>`.
  - Loudness per ITU-R BS.1770-4: `lufs_integrated(l, r, sr)` (K-weighting
    pre-filter + RLB high-pass, 400 ms blocks with 75 % overlap, absolute gate
    −70 LUFS, relative gate −10 LU) and `lufs_short_term_max(l, r, sr)` (3 s
    windows). Compute the two K-weighting biquads from the analog-prototype
    formulas used by libebur128/pyloudnorm so any sample rate works — do **not**
    reuse `Biquad::high_shelf` (no Q parameter; BS.1770 needs Q ≈ 0.7072 at
    1681.97 Hz, +3.9998 dB, and the RLB HP at 38.135 Hz, Q ≈ 0.5003).
- `src/analysis/synth.rs` — deterministic Karplus-Strong DI performances, moved
  from `di_compare.rs` (`Lcg`, `pluck`) with `sr` as a parameter, plus named
  phrases returned by `fn phrase(kind: Phrase, sr: f32) -> Vec<f32>`:
  `Chugs` (existing bar 1/3), `Chords` (power chord ring-out), `Lick`
  (existing bar 4), `CleanArpeggio` (open E/A/D/G strings, 0.4 peak),
  `SustainedLead` (single notes held 2 s — sustain/decay), `Dynamics` (the same
  phrase at −12, −6, 0 dB — edge-of-breakup behavior), `PalmMutes`. `fn corpus(sr)`
  returns all of them with names. Peak-normalize each phrase to the humbucker
  reference (−3 dBFS) so the synthetic corpus is "calibrated" by construction.
- `src/analysis/render.rs`:
  ```rust
  pub struct RenderOpts { pub sr: f32, pub preroll_s: f32 /*0.5*/, pub max_tail_s: f32 /*4.0*/,
                          pub width_override: Option<f32>, pub block: usize /*512*/ }
  pub struct Render { pub l: Vec<f32>, pub r: Vec<f32>, pub sr: f32 }
  pub fn render_preset(preset: &Preset, di: &[f32], opts: &RenderOpts) -> Render
  ```
  Fresh `Arc<Params>` → `preset.apply` → optional `master_width` override →
  `DspChain::new(sr, params)` → render `preroll_s` of silence and discard it (so
  filters, sag and bloom settle) → render `di` in `block`-sized chunks with
  `process_block` → render tail until 0.25 s below 1e-4 peak or `max_tail_s`
  (same logic as `src/export.rs::render_with_chain`).
  Also `pub fn write_wav_f32_stereo(path, &Render) -> anyhow::Result<()>` and
  `pub fn read_wav_mono(path, sr) -> anyhow::Result<Vec<f32>>` (use
  `crate::practice::decode_track(path, sr)` for decode + resample, then sum to
  mono exactly as `export.rs` does).

**Tests (`src/analysis/*` `#[cfg(test)]`):**

- BS.1770 sanity: a 997 Hz sine, amplitude 1.0 in **both** channels, 10 s at
  48 kHz reads `0.0 ± 0.1` LUFS; the same in one channel only reads
  `−3.01 ± 0.1`; at 44.1 kHz and 96 kHz the same values hold.
- `stereo_correlation`: identical = 1.0, inverted = −1.0 (±1e-6).
- `crest_db` of a long sine = 3.01 ± 0.05 dB.
- `render_preset` is bit-deterministic (two runs, same preset, same DI).
- `render_preset` output is finite and ≤ ~1.0 peak for every bundled preset on
  `Phrase::Chugs` (short, 1 s) — guards the harness itself.

#### B2. Reference harness — `examples/fidelity_render.rs`

A thin CLI over B1 (argument parsing by hand — `clap` would also collide in name
with the CLAP feature). Prints a table and writes files; returns
`anyhow::Result<()>`.

```text
cargo run --release --example fidelity_render -- [OPTIONS]

  --presets all | <stem>[,<stem>…]   bundled presets from ./presets (default: all)
  --synth                             use analysis::synth::corpus (default if no --di)
  --di <dir>                          user DI corpus with manifest.toml
                                      (default dir: ~/.config/rusty-riff/fidelity/di/)
  --sr <hz>                           render rate (default 48000)
  --out <dir>                         default: target/fidelity/<unix-seconds>/
  --width preset | <float>            keep preset master width or override (e.g. 1.0)
  --ref <dir>                         reference excerpts: <preset-stem>.wav
  --abx <stemA>:<stemB>               level-matched blind A/B/X files
  --bench                             realtime factor per preset @ 44.1/48/96 kHz
  --write-baseline <file>             write synthetic-corpus metrics
  --check <file>                      compare against a baseline; error on drift
```

**DI manifest** (`<di dir>/manifest.toml`; WAVs are never committed — add
`fidelity/di/*.wav` to `.gitignore` if a repo-local corpus dir is ever used):

```toml
[[di]]
file              = "strat_neck_bends.wav"
guitar            = "Strat, neck single-coil"
pickup            = "single_coil"     # single_coil | p90 | humbucker
calibrated        = true              # recorded through rusty-riff after `N` calibration
trim_db           = 0.0               # extra trim applied by the harness if not calibrated
reference_version = 1
content           = ["lead", "bends", "sustain"]
notes             = "Gilmour-style phrasing, guitar vol 10"
```

The harness warns when a DI's `reference_version` differs from the current
`REFERENCE_VERSION` or `calibrated = false` with `trim_db = 0`.

**How to record a DI with rusty-riff itself** (document in the README):
calibrate (`N`), press `R` to record a dry take, save the session (`J`); the
take WAV in the session folder is a calibrated DI — copy it into the DI dir and
add a manifest entry.

**Outputs** under `--out`:

- `<preset>/<di>.wav` — 32-bit float stereo render.
- `report.toml` — per render: preset, di, sr, width, `lufs_i`, `lufs_st_max`,
  `sample_peak`, `rms_db`, `crest_db`, `centroid_hz`, `correlation`,
  `side_to_mid_db`, `punch_p95_med`, `treble_mod`, `noise_floor_db`, and the
  third-octave LTAS array. Written with `toml` + `serde::Serialize` structs.
- `summary.csv` — the scalar columns, one row per render.
- With `--ref`: for each preset with `<ref>/<stem>.wav`, level-match render and
  reference by integrated LUFS, then report `ltas_rms_diff_db` (RMS of per-band
  differences, 80 Hz–8 kHz), `centroid_delta_hz`, `crest_delta_db`,
  `correlation_delta`; write `<preset>/ref_matched.wav` and
  `<preset>/render_matched.wav`. **Never** print a single "similarity %".
- With `--abx a:b`: `abx/A.wav`, `abx/B.wav`, `abx/X.wav` (X = A or B chosen by a
  seeded LCG, seed printed), all LUFS-matched; the answer in `abx/key.toml`; and
  `abx/listening-notes.md` (template: date, listener, playback system, trials,
  answers, confidence, free-text).
- With `--bench`: per preset and rate, `rtf = wall_seconds / audio_seconds`
  rendering 10 s of `Phrase::Chugs`, plus the budget for a 128-frame callback
  (`128 / sr` s) vs the mean per-block time. Warn if any `rtf > 0.5`.
- With `--write-baseline`/`--check`: baseline file
  `docs/fidelity/baseline-synth-48k.toml` (commit it). Tolerances:
  `lufs_i ±0.1`, each LTAS band `±0.25 dB`, `crest_db ±0.2`,
  `correlation ±0.02`, `centroid_hz ±1 %`. `--check` prints every violation and
  returns an error if any. Every intended voicing change regenerates the
  baseline in the same commit, and the commit message states which metrics moved
  and why.

**Integration test** `tests/fidelity_harness.rs`: one bundled preset × a 1 s
synthetic phrase through `analysis::render::render_preset`; asserts
determinism, finite output, and that `lufs_integrated` is within −40..0 LUFS.
(Do not spawn the example binary.)

#### B3. Input trim stage — `src/audio/calibration.rs`

New submodule (`mod calibration;` in `src/audio/mod.rs`, `pub use` the shared
type).

```rust
/// Shared, lock-free input-calibration state (control thread ⇄ audio thread).
pub struct InputCalibration {
    /// Trim applied to the guitar input, in dB. 0.0 = uncalibrated (bit-identical).
    pub trim_db: AtomicF32,
    /// UI sets while the wizard is capturing; the audio thread then pushes window stats.
    pub measuring: AtomicBool,
    /// Sticky: a raw (pre-trim) sample reached |x| >= 0.999. UI clears it.
    pub raw_clip: AtomicBool,
    /// Sticky: the stats ring was full and a window was dropped. UI clears it.
    pub stats_overflow: AtomicBool,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct WindowStat { pub peak_raw: f32, pub sum_sq_raw: f32, pub frames: u32 }

pub const CAL_WINDOW_MS: f32 = 10.0;
pub const CAL_RING_CAPACITY: usize = 4096;
pub const CLIP_THRESHOLD: f32 = 0.999;
pub const TRIM_SMOOTH_MS: f32 = 20.0;
```

- Construction: `audio::start` gains a parameter `calibration: Arc<InputCalibration>`
  (next to `tuner`), forwarded to `build_engine`. `build_engine` creates an
  `rtrb::RingBuffer::<WindowStat>::new(CAL_RING_CAPACITY)`; the producer goes into
  `InputState`, the consumer into `AudioEngine` behind
  `pub fn drain_cal_stats(&mut self, out: &mut Vec<WindowStat>)` (UI thread).
- `InputState` new fields: `cal: Arc<InputCalibration>`, `cal_tx: Producer<WindowStat>`,
  `trim_gain: f32` (initialized to `db_to_lin(cal.trim_db)` so there is no
  startup ramp), `trim_coeff: f32` (one-pole coefficient for `TRIM_SMOOTH_MS` at
  `sr`), `win: WindowStat` (current accumulating window), `win_len: u32`
  (frames per 10 ms window at `sr`).
- In `on_input` (per chunk after A5), **immediately after deinterleaving into
  `in_buf`** and before the tuner branch:
  1. `let target = db_to_lin(self.cal.trim_db.load(Relaxed));` (once per block;
     `db_to_lin` from `crate::dsp::effects`).
  2. `let measuring = self.cal.measuring.load(Relaxed);`
  3. For each sample `x` in `in_buf`: update clip (`x.abs() >= CLIP_THRESHOLD`
     ⇒ set a local flag), and if `measuring`, accumulate `win` on the **raw** `x`
     and push/reset it every `win_len` frames (`push` failure ⇒ local overflow
     flag). Then `self.trim_gain += self.trim_coeff * (target - self.trim_gain);`
     and `*x *= self.trim_gain`.
  4. After the loop store the sticky flags once if set.
  - When `trim_db == 0` and settled, `trim_gain == 1.0` exactly and
    `x * 1.0 == x`, so output is bit-identical. Add a test for this.
  - When not measuring, reset `win` so a later capture starts clean.
- Consequences (documented): the tuner, the input VU meter, the dry capture
  (`capture_tx`), the live chain and the take bus all see the calibrated signal;
  `export.rs` is unchanged (takes are already calibrated).
- `Levels.input` stays the calibrated meter. Clip warning comes from `raw_clip`.

**Tests:** extract the per-sample loop into a pure
`fn condition_block(buf: &mut [f32], state: &mut TrimState, target: f32, measuring: bool, sink: &mut impl FnMut(WindowStat)) -> Flags`
and unit-test: 0 dB is bit-identical; a step from 0 → +6 dB converges to 2.0×
within ~5 × `TRIM_SMOOTH_MS` without overshoot; window stats sum to the
expected energy of a known sine; clipping sets the flag from the raw value even
when trim is negative.

#### B4. Calibration wizard — `src/ui/calibration.rs` (key `N`)

`N` is free globally (only used inside the session modal). Add to the help
modal's "Tools" group: `N — calibrate input level`.

State machine (`enum Step { Intro, Noise, Capture, Result, Error }`), modal
rendered like the tuner/IR browser (`centered_rect`, double border, amber keys):

1. **Intro.** Text: set the interface gain as low as practical (note it), guitar
   volume 10, bridge pickup. `←/→` choose `PickupClass`; `Enter` next; `Esc`
   close. Refuse to open (status toast) while a take is recording
   (`capture.active`).
2. **Noise (2 s).** "Don't play — mute the strings." Set `measuring = true`,
   drain stats each UI tick into a `Vec<WindowStat>` (UI thread may allocate).
3. **Capture (8 s).** "Strum open E hard, four times, let it ring." Live raw
   meter from the latest window peaks, a clip lamp from `raw_clip`, and a
   countdown. Then `measuring = false`.
4. **Result.** Shows measured P99 peak (dBFS), noise floor, SNR, target, proposed
   trim, warnings. Keys: `Enter` accept + save (B5), `R` retry, `0` reset trim to
   0 dB (and remove the saved entry), `Space` A/B (live-toggle `trim_db`
   between the proposed and the previous value), `Esc` cancel (restore previous).
5. **Error.** `Clipped` ("lower the interface gain and retry"), `NoSignal`,
   `Overflow` (stats dropped — retry). `R` retry, `Esc` close.

Pure core (unit-tested, no UI):

```rust
pub enum PickupClass { SingleCoil, P90, Humbucker }
pub const REFERENCE_VERSION: u32 = 1;
pub fn target_peak_dbfs(p: PickupClass) -> f32;  // −9.0 / −4.5 / −3.0
pub struct CalResult { pub measured_peak_dbfs: f32, pub noise_floor_dbfs: f32,
                       pub target_peak_dbfs: f32, pub trim_db: f32, pub warnings: Vec<CalWarning> }
pub enum CalError { Clipped, NoSignal, TooShort, Overflow }
pub enum CalWarning { HighTrim /* > +18 dB */, LowSnr /* < 40 dB */, Clamped }
pub fn compute_calibration(play: &[WindowStat], noise: &[WindowStat],
                           pickup: PickupClass, clipped: bool, overflow: bool)
                           -> Result<CalResult, CalError>;
```

Rules: `clipped` ⇒ `Clipped`; fewer than 200 windows (2 s) of play ⇒ `TooShort`;
P99 of play window peaks below −50 dBFS ⇒ `NoSignal`; `trim_db = target − P99`,
clamped to ±24 (⇒ `Clamped` warning); `HighTrim` above +18 dB; `LowSnr` when
P99 − noise-floor RMS < 40 dB. Tests cover every branch plus the nominal case
(a −15 dBFS-peak sine as humbucker ⇒ +12.0 dB ± 0.1).

Header indicator (`src/ui/draw.rs::render_header` or the VU row): `IN +7.5 dB`
when calibrated, `IN uncal` otherwise, and a red `CLIP` while `raw_clip` is set
(cleared after ~1 s by the UI). Re-bless insta snapshots and mention it in the
commit message.

#### B5. Persistence — `~/.config/rusty-riff/input-calibration.toml`

In `src/audio/calibration.rs` (control-thread functions):

```toml
version = 1

[[inputs]]
device             = "Scarlett 2i2 USB"
channels           = 2
channel            = 0            # guitar channel, 0-based (same as audio.conf)
trim_db            = 7.5
pickup             = "humbucker"
measured_peak_dbfs = -10.5
target_peak_dbfs   = -3.0
reference_version  = 1
calibrated_unix    = 1790000000
note               = ""
```

- `serde` structs with `#[serde(default)]` on everything except the identity
  keys; unknown fields ignored.
- `load_calibration(identity) -> Option<Entry>`, `save_calibration(entry)`
  (replace the entry with the same identity, write `*.toml.tmp` then `rename`),
  `remove_calibration(identity)`. Best-effort like `audio::save_selection`: a
  failed read/write never blocks startup; errors go to the audio log / a toast.
- Identity = `(device name, channel count, guitar channel)`. Add
  `AudioEngine::input_identity() -> InputIdentity` (store the name/channels that
  `start` already computes around lines 512–523).
- Apply on startup after the engine starts, and again after the `O` device
  change. If the loaded entry's `reference_version` differs from
  `REFERENCE_VERSION`, still apply it but toast "input calibration is from an
  older reference — recalibrate (N)".
- Calibration is **hardware** state: never written into presets or sessions and
  never reset by `Params::reset`.

Tests: round-trip a file with two identities; replacing one keeps the other;
malformed file ⇒ `None`, no panic.

#### B6. Takes record their trim

- `src/project.rs::TrackSection`: add
  `#[serde(default, skip_serializing_if = "Option::is_none")] input_trim_db: Option<f32>`
  and `calibration_ref: Option<u32>` (same attributes). Mirror on the runtime
  `session::Track` for raw takes.
- Set both when a capture completes (the UI owns the capture lifecycle in
  `src/ui/practice.rs`): read `calibration.trim_db` (unchanged during the take,
  because the wizard refuses to open while recording) and `REFERENCE_VERSION`
  (or `None` when uncalibrated, i.e. trim 0 and no saved entry).
- Recovery takes (`~/.config/rusty-riff/recovery`) carry no metadata today; leave
  them `None` and note it.
- Timeline row: show a dim `uncal` tag for raw takes with `input_trim_db == None`.
- Tests: manifest round-trip with and without the fields; an old manifest (no
  fields) still loads.

#### B7. Documentation

- README: new section "Calibrate your input level" (why, steps, pickup
  classes, what "uncal" means, how to recalibrate after changing interface
  gain).
- README / `CONTRIBUTING.md`: "Fidelity harness" (commands above, DI manifest,
  recording a DI with `R`, reading `report.toml`, ABX workflow, baseline check).
- README: an "Input conditioning" note before the noise gate in the
  signal-chain description.
- Help modal: `N` row. `AGENTS.md`: add `src/analysis/`,
  `src/audio/calibration.rs`, and `examples/fidelity_render.rs` to the module
  table and the sound-analysis tools list.
- `docs/fidelity-references.md`: in the source-log template, the
  `Listen/measure` field now cites a harness output (`report.toml` path, DI
  names, width, `REFERENCE_VERSION`).

#### B8. Maintainer step — measure the tuning rig (human)

1. On the interface, gain setting and guitar the bundled presets were tuned
   with, run the wizard for each pickup class available and write down the
   measured P99 peaks.
2. If they differ from the provisional targets by more than ~1 dB, set
   `target_peak_dbfs` to the measured values, bump `REFERENCE_VERSION` to 2,
   regenerate `docs/fidelity/baseline-synth-48k.toml` (synthetic phrases are
   normalized to the humbucker target), and update the docs table.
3. Record the rig (interface model, gain position, guitar, pickups) in
   `fidelity-implement.md`.

**Workstream B gate:** with trim 0 dB, live output is bit-identical to before
B3; the harness is deterministic and `--check` passes against the committed
baseline; a calibrated take re-amps identically on the take bus and through
`E` export; the audio callback gained no allocation, lock or unbounded loop.

### After A and B (not planned in detail yet)

Tracked in [`fidelity-implement.md`](fidelity-implement.md#deferred-findings-for-the-fidelity-phases):
anachronism metadata/test per preset, period presets at `[master] width = 1.0`,
the Stairway/Hotel California claims, and the missing components most relevant to
Pink Floyd / Eagles / Led Zeppelin (Binson Echorec, spring reverb, a small
Supro/tweed-style combo, a pickup/guitar-volume input model, a Power
Boost-style boost). Each will get its own section here
before implementation, using the Workstream B harness for before/after
evidence.
