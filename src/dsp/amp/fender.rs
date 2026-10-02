use super::{
    AMP_MAX, AmpKnob, Amplifier, Bloom, BrightCap, CathodeBias, FrontEnd, GridBlock,
    OutputTransformer, PowerOs, SpeakerLoad, ToneCache, VoiceBalance, sagged_rail, tube_clip_asym,
};
use crate::dsp::biquad::Biquad;
use crate::dsp::effects::{SpringReverb, Tremolo};
use crate::dsp::oversample::Oversampler8;
use crate::dsp::tonestack::{Components, ToneStack};

/// Fender Twin Reverb (blackface AB763) front-panel controls (DSP order): Volume,
/// Treble, Middle, Bass, Reverb, Speed, Intensity.
pub const KNOBS: &[AmpKnob] = &[
    AmpKnob {
        label: "VOLUME",
        slug: "gain",
        default: 0.75,
    },
    AmpKnob {
        label: "TREBLE",
        slug: "treble",
        default: 0.65,
    },
    AmpKnob {
        label: "MIDDLE",
        slug: "mid",
        default: 0.0,
    },
    AmpKnob {
        label: "BASS",
        slug: "bass",
        default: 1.0,
    },
    AmpKnob {
        label: "REVERB",
        slug: "reverb",
        default: 0.0,
    },
    AmpKnob {
        label: "SPEED",
        slug: "speed",
        default: 0.0,
    },
    AmpKnob {
        label: "INTENSITY",
        slug: "intensity",
        default: 0.0,
    },
];

/// Fender Twin Reverb amplifier simulation — the blackface clean reference.
///
/// Signal path:
///   DC block → input HP → [8× OS: stage-1 tube + HP + stage-2 tube] → tone stack →
///   voice → onboard spring reverb → bias tremolo → power amp → output transformer
///   → speaker load → output trim.
///
/// Character:
///   • **High-headroom, glassy clean**: the preamp stays on the linear part of the
///     curve far up the Volume knob, with a bright cap for the blackface sparkle and
///     a scooped Fender tone stack. It breaks up late and evenly, then compresses
///     softly rather than clamping — the clean bed under Eagles and Gilmour.
///   • **Onboard spring reverb** (SpringReverb) and **bias/optical tremolo** (Speed /
///     Intensity) are modelled in-amp, unlike the rack spring-reverb and tremolo
///     pedals — the real combo has them built in.
///   • **6L6 power section**: gentle sag and a big, clean output transformer, so the
///     lows stay tight and the top stays open. Twin Reverbs are **solid-state
///     rectified** in every revision (blackface AB763 through silverface AA769), so
///     the supply is stiff and the compression is soft, not valve-rectifier sag.
pub struct Fender {
    sr: f32,
    front: FrontEnd,
    os: Oversampler8,
    os_power: PowerOs,
    pre_clip_hp: Biquad,
    stage_hp: Biquad,
    bloom: Bloom,
    // Treble-bleed cap across the volume pot (blackface bright switch, subtle).
    bright: BrightCap,
    tone_cache: ToneCache,
    cathode: CathodeBias,
    // Hard grid-blocking only when the input is genuinely slammed.
    grid: GridBlock,
    xfmr: OutputTransformer,
    tone: ToneStack,
    voice: VoiceBalance,
    out_hp: Biquad,
    envelope: f32,
    speaker: SpeakerLoad,
    // Onboard spring reverb (mono tank; the amp stage is mono → mono).
    spring: SpringReverb,
    // Output-stage bias tremolo.
    trem: Tremolo,
}

impl Fender {
    pub fn new(sr: f32) -> Self {
        let sr8 = sr * 8.0;
        let mut f = Self {
            sr,
            front: FrontEnd::new(sr, 50.0),
            os: Oversampler8::new(sr),
            os_power: PowerOs::new(sr),
            pre_clip_hp: Biquad::highpass(sr8, 30.0, 0.707),
            // Wide-bandwidth coupling: the blackface clean keeps its low-mid body.
            stage_hp: Biquad::highpass(sr8, 120.0, 0.707),
            bloom: Bloom::new(sr, 6.0, 50.0),
            // Blackface bright cap: subtle, ~1.5 kHz.
            bright: BrightCap::new(sr, 1500.0, 0.16),
            tone_cache: ToneCache::new(),
            // Light first-stage bias shift — the Twin is high-headroom, so the
            // touch-sensitive give is present but gentle.
            cathode: CathodeBias::new(sr8, 1.5, 50.0, 0.03, 1.0),
            // Grid blocking: the Twin is high-headroom, so the threshold sits high;
            // only a hard slam chokes the first stage.
            grid: GridBlock::new(sr8, 0.25, 30.0, 0.22, 2.6),
            // Big, clean 6L6 output transformer: little core saturation.
            xfmr: OutputTransformer::new(sr, 110.0, 1.0, 0.02),
            tone: ToneStack::new(sr, Components::FENDER),
            voice: VoiceBalance::new(sr, 180.0, 3.5, 900.0, -4.0),
            out_hp: Biquad::highpass(sr, 12.0, 0.707),
            envelope: 0.0,
            // Open-back 2×12 (Jensen-style) resonance ~80 Hz, lightly damped.
            speaker: SpeakerLoad::new(sr, 80.0, 0.9, 0.04, 0.18, 0.9, 0.35),
            spring: SpringReverb::new(sr),
            trem: Tremolo::new(sr),
        };
        f.update_tone_stack(0.5, 0.45, 0.65);
        f
    }

    fn update_tone_stack(&mut self, bass: f32, mid: f32, treble: f32) {
        self.tone.update(bass, mid, treble);
    }

    /// 6L6 power section: gentle sag (a stiff-ish supply that compresses softly).
    #[inline]
    fn power_amp(&mut self, x: f32) -> f32 {
        let abs_x = x.abs();
        let coeff = if abs_x > self.envelope {
            1.0 - (-180.0 / self.sr).exp()
        } else {
            1.0 - (-8.0 / self.sr).exp()
        };
        self.envelope += coeff * (abs_x - self.envelope);
        let sag = 1.0 / (1.0 + self.envelope * 0.25);
        // Power clipper at 8× (sag held per sample).
        let (drive_up, rail) = sagged_rail(sag, 1.5);
        // `sag` is a rail voltage: as it falls the drive rises and the output
        // comes back down, so sag compresses *harder* under load. See `sagged_rail`.
        self.os_power
            .shape(x, |u| tube_clip_asym(u * drive_up) * rail * 0.72)
    }
}

impl Amplifier for Fender {
    #[inline]
    fn process(&mut self, sample: f32, knobs: &[f32; AMP_MAX]) -> f32 {
        let volume = knobs[0];
        let treble = knobs[1];
        let mid = knobs[2];
        let bass = knobs[3];
        let reverb = knobs[4];
        let speed = knobs[5];
        let intensity = knobs[6];

        if self.tone_cache.changed(bass, mid, treble) {
            self.update_tone_stack(bass, mid, treble);
        }

        let x = self.front.process(sample);
        let x = self.bright.process(x, volume);

        // Moderate max gain (~13×): the Twin stays clean well up the volume but its
        // preamp still reaches its responsive region when pushed.
        let pregain = 1.0 + volume * 12.0;
        // Dynamic grid-bias bloom (the main touch mechanism for a high-headroom amp):
        // it is the level-dependent even-harmonic growth that lets a clean Twin still
        // open up as you dig in, without static clipping at normal levels.
        let bias = self.bloom.follow(x) * 0.10;

        // ── 8× oversampled nonlinear section ──────────────────────────────────
        let g1 = pregain.powf(0.6) * 1.2;
        let g2 = (pregain / pregain.powf(0.6)) * 1.3;
        let up = self.os.upsample(x);
        let mut down = [0.0f32; 8];
        for (o, &u) in down.iter_mut().zip(up.iter()) {
            let u = self.pre_clip_hp.process(u);
            let d = self.grid.shift(self.cathode.shift((u + bias) * g1));
            let s = tube_clip_asym(d) / g1.sqrt();
            let s = self.stage_hp.process(s);
            *o = tube_clip_asym(s * g2) / g2.sqrt();
        }
        let x = self.os.downsample(down);
        // ── end oversampled section ───────────────────────────────────────────

        let x = self.tone.process(x);
        let x = self.voice.process(x);

        // Onboard spring tank: mono, before the bias tremolo and power amp as in
        // the real Twin. The knob is the reverb recovery level; the tank's own
        // decay/damping are fixed.
        let wet = self.spring.process(x, 0.72, 0.42);
        let x = x + wet * reverb * 2.5;
        // Output-stage bias tremolo (amplitude only).
        let (tl, tr) = self.trem.process(x, x, speed, intensity, 0.0, 0.0);
        let x = 0.5 * (tl + tr);

        let x = self.power_amp(x);
        let x = self.xfmr.process(x);
        let x = self.speaker.process(x, self.envelope);
        let x = self.out_hp.process(x);

        // Fixed output trim (level-matched to the other models).
        x * 10.12
    }
}
