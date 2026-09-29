# IRs — external cabinet captures (Phase A1)

Drop third-party / self-recorded `.wav` cabinet IRs here for auditioning, or into
`~/.config/rusty-riff/irs/` (same layout, survives reinstalls). The in-app IR
browser (`I`) scans the config dir; project saves copy the chosen file into
`<project>/irs/`.

## Recommended starter set

Rewritten 2026-09-29: twelve bundled presets were retired (see
[`../docs/retired-presets.md`](../docs/retired-presets.md)), so this table
previously aimed at rigs that no longer have a preset. What survives is
almost entirely Hiwatt/WEM, plus a clean Fender and a Tweed.

| Rig | What to load | Presets it serves |
|-----|--------------|-------------------|
| WEM 4x12 Fane (SM57) | `wem-412-fane-57.wav` | all 6 `pink_floyd_*` (hiwatt + wem) |
| Fender Twin 2x12 Jensen (SM57 + room) | `fender-twin-212-57.wav` | `hotel_california_clean` |
| Marshall 4x12 Greenback (SM57 + R121) | `marshall-412-greenback-57.wav`, `marshall-412-greenback-r121.wav` | the new vanilla default (Plexi + Greenback) |
| Tweed 1x12 | *(none shipped)* | `hotel_california_solo` |
| Mesa 4x12 V30 (SM57 + R121) | `mesa-412-v30-57.wav` | no bundled preset — auditioning only |

The Marshall and Mesa entries used to serve `acdc_*`, `whole_lotta_love`,
`beat_it` and `november_rain`. Keep the files; they still audition the rigs
directly and are the obvious thing to load if those presets come back.

Mono files are duplicated to L/R; stereo files keep their image. Any sample rate
works — loading resamples offline to the engine rate.

## Live vs offline

* Live (gigging, 128 frames @48 kHz): truncated to `LIVE_MAX_IR_LEN` = 8192 taps
  (≈170 ms) — direct + early-room energy, stays in RT budget.
* Export / fidelity render: `OFFLINE_MAX_IR_LEN` = 32768 taps — full tails.

## Open-license sources

Prefer permissively-licensed packs (e.g. MIT/CC0 community IRs). Do not commit
proprietary commercial IRs — keep those in `~/.config/rusty-riff/irs/` only.

## Knobs with an external IR

* `mic_pos`: live post-EQ trim (edge↔centre) — stays musical.
* `blend` / `room`: inert by design (capture already bakes mic + room).
* Speaker drive (cone breakup + thermal compression) + mic saturation still apply
  pre/post convolution so loaded IRs feel as alive as built-ins.
