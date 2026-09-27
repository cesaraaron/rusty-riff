# rusty-riff user guide

A practical walkthrough of the terminal guitar rig. For the exhaustive key list
press `K` in the app; this guide covers the workflows most people need.

## Build & run

Requires Rust 1.96+, a guitar, an audio interface with a high-impedance
instrument input, and speakers or headphones.

```bash
git clone https://github.com/cesaraaron/rusty-riff
cd rusty-riff
cargo run --release
```

Use `--release`: a debug build can underrun the audio callback. On first launch
the app prompts for your **input device**, then **input channel**, then **output
device**. The processed signal is written to all output channels.

## Panels

The UI is split into panels you focus with the number keys; pressing the same
number again hides the panel:

| Key | Panel |
| --- | --- |
| `1` | Chain / signal flow |
| `2` | Amp |
| `3` | Practice timeline |
| `4` | Pedals |

`Tab` / `Shift-Tab` cycle options inside the amp (2) or pedals (4) panel,
`←`/`→` move within the focused panel, and `Space` bypasses the stage, pedal, or
transport under focus.

## Calibrate the input

Amp breakup depends on how hard your interface drives the input, so calibrate
once per guitar/interface combination.

1. Press `N`.
2. Set the interface gain as low as practical (so hard strums don't clip) and
   leave it there.
3. Choose the **pickup class** with `←`/`→`.
4. `Enter` to start. Mute the strings for the ~2 s noise capture, then strum an
   open E hard four times for the ~8 s level capture.
5. Review the measured P99 peak, noise floor, and proposed trim. `Enter` saves,
   `R` retries, `0` resets to no trim, `Space` A/Bs the proposed trim.

The header reads `IN uncal` until you calibrate and shows the applied trim after
(e.g. `IN +7.5`). The trim is applied to the dry input **before** the noise gate,
so the tuner, meters, recordings, and every effect see the calibrated signal.
Recalibrate with `N` whenever you change the interface gain, guitar, or pickups.

## Signal chain

The default order is:

```text
NOISE GATE → WHAMMY → WAH → COMP → FUZZ → TS-808 → DS-1 → ML-2 METAL CORE
  → PRE-AMP EQ → UNI-VIBE → CLEAN BOOST → AMP → CAB
  → GRAPHIC EQ → PARAMETRIC EQ → FLANGER → CHORUS → PHASER → TREMOLO/VIBRATO
  → DELAY → SPRING REVERB
```

Stages are **reorderable**: focus the chain (`1`) and use `[` / `]` to move the
selected stage earlier or later. The shipped order keeps the amp and cab
consecutive; rack effects normally sit after the cab. A stage placed between
`AMP` and `CAB` is processed at line level, as if in a virtual load box or
effects loop — it is not a pedal wired into the speaker cable.

Amp and cabinet are switchable: `A` opens the amp model browser and `C` the
cabinet browser. Each built-in amp has its own front-panel knobs; the cabinet
stage adds mic position, dynamic/ribbon blend, and room amount. `I` opens the
external IR browser and `X` bypasses the IR.

`W` toggles the master-bus stereo width (neutral / wide).

## Expression pedal (MIDI)

MIDI is on by default: rusty-riff connects to the first MIDI input and binds the
standard **Expression** CC (11). Moving the pedal drives the wah's treadle and
switches it to manual mode, overriding a preset's auto-wah. You can also set the
wah's `MODE` knob to manual and play `POSITION` by hand. Change the CC, bind more knobs, or turn the
input off in `~/.config/rusty-riff/midi.conf` (`<cc> = <target>` maps another
CC; targets include `wah_position`, `wah_mode`, `delay_time`, `delay_mix`,
`reverb_mix`, `boost_gain`, `master_width`, …):

```text
enabled = true
cc = 11                 # wah treadle
20 = delay_mix
21 = reverb_mix
```

## Presets

A preset is a snapshot of the whole rig. Press `P` to open the browser:

| Key | Action |
| --- | --- |
| `↑` / `↓`, `Enter` | Navigate / apply (audio stays uninterrupted) |
| type | Filter by name, description, or tag (`Backspace`; `Esc` clears) |
| `S` | Save the current rig as a preset |
| `E` / `I` | Export / import a preset file |
| `F` | Favorite the selected preset |
| `X` | A/B: swap the last two applied presets |
| `D` | Delete (user presets only) |

Applying a preset switches the rig without stopping audio. Favorites are marked
with a star and tags (artist / genre / role) show in brackets; bundled presets
ship with the app and your saved ones add a `user` tag.

Where things live:

- Presets: `~/.config/rusty-riff/presets/`
- External IRs: `~/.config/rusty-riff/irs/`
- Sessions: `~/.config/rusty-riff/sessions/`
- Recoverable takes: `~/.config/rusty-riff/recovery/`

Sessions bundle the rig plus timeline tracks (and clips the referenced IR) into a
portable folder. Press `J` for the session browser: `N` new, `S` save, `A` save
as, `Enter` load, `D` delete. Abandoned dry takes also appear here as
"recoverable takes" you can restore into the current session.

## Practice & recording

- **Metronome** — `M` opens it. `←`/`→` set the tempo, `Space` starts/stops. The
  metronome is monitor-only and is never recorded.
- **Backing track** — `B` opens the import browser (MP3 / WAV / FLAC). Selecting
  a file adds it as a track at the playhead.
- **Dry-take recording** — `R` arms and stops a dry (pre-rig) raw take. The take
  auto-plays and lands on the timeline. Backing tracks and recorded takes are
  summed into the output **after** the recording tap, so they are never captured
  into a rendered WAV.
- **Timeline** — focus it with `3`. `Space` plays/pauses any row, `↑`/`↓`
  select a track, `←`/`→` seek, `+`/`−` pick the seek step (1/5/10/30 s), `M`
  mutes, `G` sets track gain, `H` moves a clip, `[`/`]`/`L` set the loop
  in/out and toggle looping, `Del` removes a track, and `E` exports the unmuted
  raw takes as a WAV.
- **Tuner** — `T` opens a chromatic tuner with a cents meter and spectrum. The
  rig is bypassed while it is open so you tune the dry signal.

## Plugins

The default build includes plugin hosting; both features can be disabled at
compile time.

- **CLAP effect insert** (`clap` feature) — press `V` to scan and browse
  installed CLAP plugins. `Enter` loads the selected plugin as the chain's
  stereo insert (the first row clears it); `Tab` opens its parameter editor,
  where `↑`/`↓` select and `←`/`→` adjust.
- **macOS Audio Unit amp override** (`au` feature, macOS only) — press `U` to
  scan and browse installed AUs. `Enter` loads one in the amp position and
  activates it (the built-in amp and cab are bypassed by default); `Z` toggles
  the override live, and inside the browser `C` switches between the plugin's
  own cab and feeding the built-in cab / external IR. On non-macOS this feature
  is a no-op.

Loaded plugins and AUs are captured in sessions and included in offline take
renders.

## License

Apache 2.0. See [`LICENSE`](../LICENSE) and [`NOTICE`](../NOTICE).
