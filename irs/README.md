# IRs — external cabinet captures (Phase A1)

Drop third-party / self-recorded `.wav` cabinet IRs here for auditioning, or into
`~/.config/rusty-riff/irs/` (same layout, survives reinstalls). The in-app IR
browser (`I`) scans the config dir; project saves copy the chosen file into
`<project>/irs/`.

## Recommended 4-rig starter set (covers 15/17 bundled presets + metal gap)

| Rig | What to load | Presets it serves |
|-----|--------------|-------------------|
| Marshall 4x12 Greenback (SM57 + R121) | `marshall-412-greenback-57.wav`, `marshall-412-greenback-r121.wav` | `acdc_*`, `whole_lotta_love`, `beat_it`, `november_rain` |
| WEM 4x12 Fane (SM57) | `wem-412-fane-57.wav` | all 9 `pink_floyd_*` (hiwatt+wem) |
| Fender Twin 2x12 Jensen (SM57 + room) | `fender-twin-212-57.wav` | `hotel_california_clean` |
| Mesa 4x12 V30 (SM57 + R121) | `mesa-412-v30-57.wav` | new metal presets (Phase B) |

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
