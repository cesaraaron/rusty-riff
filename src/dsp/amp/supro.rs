use super::{
    AMP_MAX, AmpKnob, Amplifier, Bloom, BrightCap, CathodeBias, FrontEnd, GridBlock,
    OutputTransformer, PowerOs, SpeakerLoad, SupplyRipple, ToneCache, TubeClip, VoiceBalance,
    sagged_rail, split_gain,
};
use crate::dsp::biquad::Biquad;
use crate::dsp::oversample::Oversampler8;
use crate::dsp::tonestack::{Components, ToneStack};

/// Small American combo front-panel controls (DSP order): Volume, Tone. A
/// Supro-style combo has no master volume — the Volume knob is the gain — and a
/// single Tone control (modelled as the treble of a passive stack).
/// Small-signal voltage gain of the preamp per unit of `pregain`.
///
/// `2.0 / pregain_max` puts the top of the gain knob exactly at the stage's
/// clipping rail, so the knob spans clean at the bottom to slammed at the top
/// on every model regardless of how much range its `pregain` law has.
const PREAMP_GAIN_COEFF: f32 = 6.0 / 154.0;

pub const KNOBS: &[AmpKnob] = &[
    AmpKnob {
        label: "VOLUME",
        slug: "gain",
        default: 0.70,
    },
    AmpKnob {
        label: "TONE",
        slug: "treble",
        default: 0.55,
    },
];

/// Small single-ended Supro-style combo — the "Stairway" solo voice.
///
/// Signal path:
///   DC block → input HP → [8× OS: stage-1 tube + inter-stage HP + stage-2 tube] →
///   passive tone stack → voice balance → power amp sag → output transformer →
///   speaker load → output trim.
///
/// Character (an approximation of a small American 6V6 combo, not a
/// schematic-exact model):
///   • **Early, thick breakup.** A small output section has little headroom, so the
///     amp clips and compresses well before a big Marshall would — warm, midrangey
///     and vocal, the voice Led Zeppelin's *Stairway* solo is built on.
///   • **Valve-rectified supply (5Y3-style).** Small combos used a valve rectifier,
///     so the supply sags and recovers slowly under load — the elastic, compressed
///     give of a cranked little amp.
///   • **Small output transformer** that saturates early, adding boxy low-end
///     compression.
///   • **One Tone knob** on a passive stack: warm and dark with a forward midrange,
///     not bright or scooped.
///   • **Small-cab resonance:** a single small speaker has a higher resonance and a
///     nasal, boxy mid — modelled in the speaker load, not a 4×12.
pub struct Supro {
    sr: f32,
    front: FrontEnd,
    os: Oversampler8,
    os_power: PowerOs,
    pre_clip_hp: Biquad,
    stage_hp: Biquad,
    bloom: Bloom,
    // Treble-bleed cap across the Volume pot — a little sparkle at low volume.
    bright: BrightCap,
    tone: ToneStack,
    tone_cache: ToneCache,
    cathode: CathodeBias,
    // Hard grid-blocking when the small first stage is genuinely slammed.
    grid: GridBlock,
    xfmr: OutputTransformer,
    voice: VoiceBalance,
    out_hp: Biquad,
    envelope: f32,
    ripple: SupplyRipple,
    speaker: SpeakerLoad,
}

impl Supro {
    /// Retune the speaker load for the selected cabinet (see
    /// [`SpeakerLoad::set_load`]).
    pub fn set_load(&mut self, load: (f32, f32)) {
        self.speaker.set_load(load.0, load.1);
    }

    pub fn new(sr: f32) -> Self {
        let sr8 = sr * 8.0;
        let mut s = Self {
            sr,
            // Small amp: a slightly tighter input HP than the big heads.
            front: FrontEnd::new(sr, 70.0),
            os: Oversampler8::new(sr),
            os_power: PowerOs::new(sr),
            pre_clip_hp: Biquad::highpass(sr8, 35.0, 0.707),
            // Inter-stage coupling trims the deep bass a small combo can't make.
            stage_hp: Biquad::highpass(sr8, 120.0, 0.707),
            bloom: Bloom::new(sr, 6.0, 45.0),
            // Gentle bright cap: a hint of top at low volume.
            bright: BrightCap::new(sr, 1800.0, 0.16),
            // Passive stack (Fender values); the single Tone knob drives treble.
            tone: ToneStack::new(sr, Components::FENDER),
            tone_cache: ToneCache::new(),
            // First-stage cathode bias: light and quick.
            cathode: CathodeBias::new(sr8, 1.5, 48.0, 0.04, 1.0),
            // Small amp: the grid block trips earlier than a high-headroom Twin's.
            grid: GridBlock::new(sr8, 0.25, 30.0, 0.22, 2.4),
            // Small output transformer that saturates early and a little harder.
            xfmr: OutputTransformer::new(sr, 150.0, 1.3, 0.045),
            // Warm, slightly mid-forward structural voicing.
            voice: VoiceBalance::new(sr, 180.0, 4.0, 900.0, -3.5),
            out_hp: Biquad::highpass(sr, 12.0, 0.707),
            envelope: 0.0,
            // Valve-rectified mains ripple rides the sagging rail.
            ripple: SupplyRipple::new(sr, 100.0, 0.04),
            // Small single speaker: higher resonance (~110 Hz), boxy dynamic bloom.
            speaker: SpeakerLoad::new(sr, 110.0, 1.0, 0.06, 0.3, 0.8, 0.35),
        };
        s.update_tone(0.55);
        s
    }

    /// Tone knob → the passive stack's treble (bass/mid held warm and fixed).
    fn update_tone(&mut self, tone: f32) {
        self.tone.update(0.5, 0.45, tone);
    }

    /// Valve-rectified sag: a shallow-but-slow give — a small combo's supply
    /// compresses under load and recovers over the note.
    #[inline]
    fn power_amp(&mut self, x: f32) -> f32 {
        let abs_x = x.abs();
        let coeff = if abs_x > self.envelope {
            1.0 - (-140.0 / self.sr).exp()
        } else {
            // ~180 ms recovery: the 5Y3 refills slowly, so the compression blooms.
            1.0 - (-5.5 / self.sr).exp()
        };
        self.envelope += coeff * (abs_x - self.envelope);
        let sag = 1.0 / (1.0 + self.envelope * 0.9);
        let supply = self.ripple.gain(sag, self.envelope);
        // Power clipper at 8× (supply held per sample): kills base-rate fizz.
        let (drive_up, rail) = sagged_rail(supply, 1.8);
        // `supply` is a rail voltage: as it falls the drive rises and the output
        // comes back down, so sag compresses *harder* under load. See `sagged_rail`.
        self.os_power
            .shape(x, |u| TubeClip::PUSH_PULL.shape(u * drive_up) * rail * 0.66)
    }
}

impl Amplifier for Supro {
    fn set_load(&mut self, load: (f32, f32)) {
        self.speaker.set_load(load.0, load.1);
    }

    #[inline]
    fn process(&mut self, sample: f32, knobs: &[f32; AMP_MAX]) -> f32 {
        let gain = knobs[0];
        let tone = knobs[1];

        if self.tone_cache.changed(0.5, 0.45, tone) {
            self.update_tone(tone);
        }

        let x = self.front.process(sample);
        // Bright cap across the Volume pot (strongest at low gain).
        let x = self.bright.process(x, gain);

        // Pushed preamp gain, split across the two stages so neither is fully
        // pinned — the early breakup comes from the low-headroom power section, but
        // the Volume knob has to reach it: at ~22× max the stages saturate into the
        // thick, vocal edge-of-breakup a cranked small combo lives in.
        let pregain = 1.0 + gain * 154.0;
        let bias = self.bloom.follow(x) * 0.11;
        let (k1, k2) = split_gain(PREAMP_GAIN_COEFF * pregain, 0.6, 1.2, 1.3);

        // ── 8× oversampled nonlinear section ──────────────────────────────────
        let up = self.os.upsample(x);
        let mut down = [0.0f32; 8];
        for (o, &u) in down.iter_mut().zip(up.iter()) {
            let u = self.pre_clip_hp.process(u);
            let d = self.grid.shift(self.cathode.shift(u + bias));
            let s = TubeClip::AX7.stage(d, k1);
            let s = self.stage_hp.process(s);
            *o = TubeClip::AX7.stage(s, k2);
        }
        let x = self.os.downsample(down);
        // ── end oversampled section ───────────────────────────────────────────

        let x = self.tone.process(x);
        let x = self.voice.process(x);
        let x = self.power_amp(x);
        let x = self.xfmr.process(x);
        let x = self.speaker.process(x, self.envelope);
        let x = self.out_hp.process(x);

        // Fixed output trim (no master) — level-matches the small combo to the
        // other models so switching amps doesn't jump the volume.
        x * 5.99
    }
}
