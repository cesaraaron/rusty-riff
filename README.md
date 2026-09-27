# 🎸 rusty-riff

A guitar amp and pedalboard rig that runs in your terminal. Keyboard-driven, with
live metering and artist-inspired presets.

See the [user guide](docs/guide.md) for a walkthrough of calibration, the signal
chain, presets, practice, and plugins.

## Requirements

- Rust 1.96+
- A guitar and an audio interface with a high-impedance instrument input
- Speakers or headphones

## Build & run

```bash
git clone https://github.com/cesaraaron/rusty-riff
cd rusty-riff
cargo run --release
```

Use `--release`: a debug build can underrun the audio callback. On first launch
the app asks for your input device, input channel, and output device.

## Essential keys

| Key | Action |
| --- | --- |
| `K` | Full in-app key reference |
| `A` / `C` | Amp / cabinet browser |
| `P` | Preset browser |
| `↑` / `↓` or `+` / `-` | Select a knob / adjust its value |
| `Space` | Bypass the focused stage |
| `[` / `]` | Move the selected stage earlier or later |
| `1`–`4` | Focus the chain, amp, timeline, or pedals |
| `R` | Record / stop a dry take |
| `N` | Calibrate the input level |
| `Q` | Quit |

## Input level

Amp breakup depends on how hot your interface drives the input, so calibrate
once per guitar/interface. Press `N`, set the interface gain as low as
practical, pick the pickup class, and follow the prompts. The header shows
`IN uncal` until you calibrate and the applied trim after (e.g. `IN +7.5`).
Recalibrate with `N` if you change the interface gain, guitar, or pickups. The
trim is applied to the dry input before the noise gate, so the tuner, meters,
recordings, and every effect see the calibrated signal.

## Where things live

- Presets: `~/.config/rusty-riff/presets/`
- External IRs: `~/.config/rusty-riff/irs/`

## License

Apache 2.0. See [`LICENSE`](LICENSE) and [`NOTICE`](NOTICE).
