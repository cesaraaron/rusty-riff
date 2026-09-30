# Retired bundled presets

Twelve bundled presets were removed from `presets/` on 2026-09-29. **They are not
lost** — every one is recoverable from git history with a single command (below),
and this page is the index.

## Why they were retired

**The tone models changed underneath them.** These voicings were authored against
the amp, cab and pedal models as they stood before the DSP rework tracked in
[`dsp-findings-2026-09.md`](dsp-findings-2026-09.md) and its as-built record in
[`fidelity-implement.md`](fidelity-implement.md) (Phase 8 onward). Since then:

- the output limiter's ceiling was corrected (it was overshooting to +1.21 dBFS);
- the tremolo stopped adding 4 ms of unconditional latency, which the Fender Twin
  model had been inheriting;
- the Phase 90 and Electric Mistress stopped folding the dry signal to mono;
- the noise gate gained a real hold time and stopped opening in 0.21 ms;
- every live knob move stopped resetting biquad state.

And, more substantially, a **full amplifier architecture rework is still ahead**:
real gain staging, a negative-feedback loop, rectifier sag that compresses harder
under load instead of reducing clipping, and per-tube-type clipper curves. That
will move the amp models again, substantially.

Restoring a preset from this list therefore gives you a **starting point for
re-voicing, not a finished tone**. The knob values were tuned against models that
no longer exist. Expect to re-listen and re-dial, especially gain, presence and
master.

## The eight that remain

| Preset | Rig |
| --- | --- |
| `eagles_hotel_california_clean` | Fender Twin / Jensen 2×12 |
| `eagles_hotel_california_solo` | Tweed / Tweed 1×12 |
| `led_zeppelin_stairway_solo` | Supro / Supro 1×10 |
| `pink_floyd_comfortably_numb_solo_1` | Hiwatt / WEM |
| `pink_floyd_comfortably_numb_solo_2` | Hiwatt / WEM |
| `pink_floyd_mother_solo` | Hiwatt / WEM |
| `pink_floyd_time_chorus` | Hiwatt / WEM |
| `pink_floyd_time_solo` | Hiwatt / WEM |

## Known consequence: thinner rig coverage

The eight survivors use only **4 of the 9 amp models** (Hiwatt, Fender, Supro,
Tweed) and **4 of the 8 cabinets** (WEM, Fender, Tweed, Supro). Marshall, Mesa,
Randall, Vox and Plexi, and the Marshall/Mesa/Orange/Vox cabinets, have **no
bundled preset**.

Those models are still covered by their own unit tests (`src/dsp/amp/mod.rs`,
`src/dsp/cab/mod.rs`) and by the rig matrix in `src/dsp/mod.rs`'s tone tests, but
they are no longer exercised **end-to-end through a preset** by the fidelity
harness. This is the weakest point in the current test setup and the thing to
remember when a Phase C change shows drift the harness cannot explain.

## Recovering one

`Last touched` is the commit that last modified the file — use it to pull the
version as it stood at retirement.

```bash
# recover a preset as it was at retirement
git show 723ecb3:presets/pink_floyd_money.toml > presets/pink_floyd_money.toml

# the full edit history of a preset, for seeing how it was arrived at
git log --oneline -- presets/pink_floyd_money.toml

# a specific earlier revision
git show <rev>:presets/pink_floyd_money.toml > presets/pink_floyd_money.toml
```

If you restore one, the fidelity baseline
(`docs/fidelity/baseline-synth-48k.toml`) will fail the `--check` gate until it
is regenerated:

```bash
cargo run --release --example fidelity_render -- --presets all \
    --write-baseline docs/fidelity/baseline-synth-48k.toml
```

## The retired twelve

| File | Name | Year | Rig | Last touched |
| --- | --- | --- | --- | --- |
| `acdc_back_in_black.toml` | AC/DC — Back in Black | 1980 | Plexi / Greenback | `2527c61` |
| `acdc_highway_to_hell.toml` | AC/DC — Highway to Hell | 1979 | Plexi / Greenback | `2527c61` |
| `guns_n_roses_november_rain_solo.toml` | Guns N' Roses — November Rain (Solo) | 1991 | JCM800 / Greenback | `bfd6631` |
| `led_zeppelin_whole_lotta_love.toml` | Led Zeppelin — Whole Lotta Love | 1969 | Plexi / Greenback | `2527c61` |
| `marshall_hard_rock_rhythm.toml` | Hard Rock — JCM800 Rhythm | — | JCM800 / Greenback | `2527c61` |
| `mesa_modern_metal.toml` | Modern Metal — Recto Rhythm | — | Dual Rect / Mesa | `2ef8dbd` |
| `pink_floyd_another_brick_pt2.toml` | Pink Floyd — Another Brick in the Wall, Pt. 2 | 1979 | Hiwatt / WEM | `df056be` |
| `pink_floyd_have_a_cigar_solo.toml` | Pink Floyd — Have a Cigar (Solo) | 1975 | Hiwatt / WEM | `cbe668f` |
| `pink_floyd_money.toml` | Pink Floyd — Money | 1973 | Hiwatt / WEM | `723ecb3` |
| `pink_floyd_shine_on_crazy_diamond.toml` | Pink Floyd — Shine On You Crazy Diamond | 1975 | Hiwatt / WEM | `723ecb3` |
| `van_halen_beat_it_solo.toml` | Van Halen — Beat It (Solo) | 1982 | Plexi / Greenback | `bfd6631` |
| `vox_chime_clean.toml` | British Chime — AC30 Clean | — | AC30 / Vox 2×12 | `2ef8dbd` |

Three of these (`marshall_hard_rock_rhythm`, `mesa_modern_metal`,
`vox_chime_clean`) were **not** artist presets — they were generic rig showcases
that existed to give the JCM800, the Dual Rectifier and the AC30 a bundled
presence. Their loss is the direct cause of the coverage gap above.

> **Decision (2026-09-29, maintainer): do not bring them back.** The coverage gap
> is accepted deliberately. The eight surviving artist presets are the point of
> the set; re-adding non-artist rig showcases to give internal models a bundled
> presence is not wanted. Those five amp models and four cabs remain covered by
> their own unit tests in `src/dsp/amp` and `src/dsp/cab` — what they lose is
> *end-to-end* coverage through a preset. Keep that in mind during the amp
> rework (Phase C), which moves the very rigs the harness can no longer see.
