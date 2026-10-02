use super::{
    AMP_MAX, AmpKnob, Amplifier, Bloom, BrightCap, Cached, CathodeBias, DynamicPresence, FrontEnd,
    GridBlock, OutputTransformer, PowerOs, SpeakerLoad, SupplyRipple, ToneCache, VoiceBalance,
    sagged_rail, tube_clip_asym,
};
use crate::dsp::biquad::Biquad;
use crate::dsp::oversample::Oversampler8;
use crate::dsp::tonestack::{Components, ToneStack};

/// Super Lead "Plexi" front-panel controls (DSP order): Volume II (Bright),
/// Volume I (Normal), Bass, Mid, Treble, Presence. The 1959 has no master
/// volume — the channel volumes *are* the gain, and the power section does much
/// of the distortion — so output level is a fixed internal trim.
pub const KNOBS: &[AmpKnob] = &[
    AmpKnob {
        label: "VOL II",
        slug: "gain",
        default: 0.75,
    },
    AmpKnob {
        label: "VOL I",
        slug: "normal",
        default: 0.0,
    },
    AmpKnob {
        label: "BASS",
        slug: "bass",
        default: 1.0,
    },
    AmpKnob {
        label: "MID",
        slug: "mid",
        default: 0.0,
    },
    AmpKnob {
        label: "TREBLE",
        slug: "treble",
        default: 0.65,
    },
    AmpKnob {
        label: "PRESENCE",
        slug: "presence",
        default: 0.40,
    },
];

/// Marshall Super Lead (model 1959) "Plexi" amplifier simulation.
///
/// Signal path:
///   DC block → input HP → [jumpered channels: Bright + darker Normal] → 8× OS:
///   stage-1 tube + HP + stage-2 tube → tone stack → power-amp sag + ripple →
///   output transformer → speaker load → presence → output trim.
///
/// Character:
///   • **Non-master**: where the JCM800 cascades huge preamp gain into a master
///     volume, the Plexi's preamp stays much lower-gain and the *power section*
///     supplies the compression and grind. That is why a Plexi is clean with the
///     guitar down and blooms into thick, woolly overdrive when it is cranked —
///     the touch dynamic Led Zeppelin, AC/DC and early Van Halen are built on.
///   • **Jumpered channels**: the Bright channel (Vol II, with a bright cap) drives
///     the first stage; the darker Normal channel (Vol I) adds a parallel feed, as
///     when the two input channels are linked with a patch cable.
///   • **Fuller, rounder voicing** than the JCM800: a lower inter-stage coupling
///     corner keeps more low-mid body, and the output transformer saturates earlier
///     for the complex cranked-PA low end.
///   • **Solid-state rectified supply**: the 1959 Super Lead moved from a GZ34
///     valve rectifier to a silicon bridge around 1966, so the late-60s/70s amps
///     the presets target have a *stiff*, fast-recovering rail — the power-amp
///     compression and bloom come from the output stage and transformer, not
///     rectifier sag.
pub struct Plexi {
    sr: f32,
    front: FrontEnd,
    os: Oversampler8,
    os_power: PowerOs,
    pre_clip_hp: Biquad,
    stage_hp: Biquad,
    // Normal-channel input low-pass: the darker of the two jumpered channels.
    normal_lp: Biquad,
    bloom: Bloom,
    // Treble-bleed cap across the Bright volume pot.
    bright: BrightCap,
    tone_cache: ToneCache,
    cathode: CathodeBias,
    // Hard grid-blocking on a slammed first stage (crackle-then-recover).
    grid: GridBlock,
    xfmr: OutputTransformer,
    tone: ToneStack,
    presence: DynamicPresence,
    presence_cache: Cached,
    voice: VoiceBalance,
    out_hp: Biquad,
    envelope: f32,
    // Solid-state rectifier: the rail is stiff, so mains ripple is shallow.
    ripple: SupplyRipple,
    speaker: SpeakerLoad,
}

impl Plexi {
    pub fn new(sr: f32) -> Self {
        let sr8 = sr * 8.0;
        let mut p = Self {
            sr,
            front: FrontEnd::new(sr, 60.0),
            os: Oversampler8::new(sr),
            os_power: PowerOs::new(sr),
            // Input coupling → sub-rumble cut below the 82 Hz low-E.
            pre_clip_hp: Biquad::highpass(sr8, 35.0, 0.707),
            // Inter-stage coupling is fuller than the JCM800's (~200 Hz vs 300 Hz):
            // the Plexi keeps low-mid body feeding the second stage, part of its
            // thicker, rounder voice.
            stage_hp: Biquad::highpass(sr8, 200.0, 0.707),
            normal_lp: Biquad::lowpass(sr, 3000.0, 0.707),
            bloom: Bloom::new(sr, 8.0, 55.0),
            // Plexi bright cap: the Bright channel's 500 pF cap is a touch stronger
            // and higher than the JCM800's, the source of its cutting top.
            bright: BrightCap::new(sr, 2500.0, 0.20),
            tone_cache: ToneCache::new(),
            // First-stage cathode bias: light and fast, so the dynamic give lives
            // within a note without smearing note-to-note attack.
            cathode: CathodeBias::new(sr8, 1.5, 45.0, 0.045, 1.0),
            // Grid blocking: threshold above ordinary playing, so only a genuinely
            // slammed input chokes the first stage (as a cranked Plexi does).
            grid: GridBlock::new(sr8, 0.25, 30.0, 0.22, 1.8),
            // Output transformer: saturates earlier and a little deeper than the
            // JCM800's — the cranked-Plexi low-end compression.
            xfmr: OutputTransformer::new(sr, 140.0, 1.6, 0.05),
            tone: ToneStack::new(sr, Components::MARSHALL),
            // Presence at 3.8 kHz, ±6 dB around a +2 dB static lift.
            presence: DynamicPresence::new(sr, 3800.0, 4.0, 1.3),
            presence_cache: Cached::new(),
            // Restore low-mid body and tame the upper-mid tilt.
            voice: VoiceBalance::new(sr, 170.0, 6.0, 800.0, -5.0),
            out_hp: Biquad::highpass(sr, 12.0, 0.707),
            envelope: 0.0,
            // UK mains → 100 Hz full-wave ripple; a silicon-bridge rail with big
            // filter caps stays stiff, so the ripple depth is small.
            ripple: SupplyRipple::new(sr, 100.0, 0.035),
            // Greenback 4×12 resonance ~95 Hz; dynamic bloom comes from the output
            // transformer and speaker interaction, not rectifier sag (the 1959 is
            // solid-state rectified).
            speaker: SpeakerLoad::new(sr, 95.0, 1.1, 0.07, 0.35, 0.9, 0.35),
        };
        p.update_tone_stack(0.5, 0.45, 0.65);
        p.update_presence(0.5);
        p
    }

    fn update_tone_stack(&mut self, bass: f32, mid: f32, treble: f32) {
        self.tone.update(bass, mid, treble);
    }

    fn update_presence(&mut self, presence: f32) {
        self.presence.set_knob((presence - 0.5) * 12.0 + 2.0);
    }

    /// Solid-state rectified supply: a stiff, fast-recovering rail (shallow sag,
    /// quick attack/release), so the Plexi's compression comes from the output
    /// stage and transformer rather than a sagging valve rectifier.
    #[inline]
    fn power_amp(&mut self, x: f32) -> f32 {
        let abs_x = x.abs();
        let coeff = if abs_x > self.envelope {
            // ~4.5 ms attack: the silicon rail tracks the signal quickly.
            1.0 - (-220.0 / self.sr).exp()
        } else {
            // ~150 ms recovery, matching the solid-state JCM800 supply.
            1.0 - (-6.7 / self.sr).exp()
        };
        self.envelope += coeff * (abs_x - self.envelope);
        let sag = 1.0 / (1.0 + self.envelope * 1.3);
        let supply = self.ripple.gain(sag, self.envelope);
        // Power clipper at 8× (envelope held per sample).
        let (drive_up, rail) = sagged_rail(supply, 2.6);
        // `supply` is a rail voltage: as it falls the drive rises and the output
        // comes back down, so sag compresses *harder* under load. See `sagged_rail`.
        self.os_power
            .shape(x, |u| tube_clip_asym(u * drive_up) * rail * 0.6)
    }
}

impl Amplifier for Plexi {
    #[inline]
    fn process(&mut self, sample: f32, knobs: &[f32; AMP_MAX]) -> f32 {
        let gain = knobs[0];
        let normal = knobs[1];
        let bass = knobs[2];
        let mid = knobs[3];
        let treble = knobs[4];
        let presence = knobs[5];

        if self.tone_cache.changed(bass, mid, treble) {
            self.update_tone_stack(bass, mid, treble);
        }
        if self.presence_cache.changed(presence) {
            self.update_presence(presence);
        }

        let x = self.front.process(sample);
        // Jumpered channels: Bright drives; Normal adds a darker parallel feed.
        let x = x + self.normal_lp.process(x * normal * 0.6);
        // Bright cap across the Bright volume pot (strongest at low gain).
        let x = self.bright.process(x, gain);

        // Lower max gain than the JCM800 (~25× vs 40×): the Plexi keeps the preamp
        // on the round part of the curve and lets the power section distort.
        let pregain = 1.0 + gain * 24.0;
        let bias = self.bloom.follow(x) * 0.05;

        // ── 8× oversampled nonlinear section ──────────────────────────────────
        let g1 = pregain.powf(0.62) * 1.3;
        let g2 = (pregain / pregain.powf(0.62)) * 1.5;
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
        let x = self.power_amp(x);
        let x = self.xfmr.process(x);
        let x = self.speaker.process(x, self.envelope);
        let x = self.presence.process(x, self.envelope);
        let x = self.out_hp.process(x);

        // Fixed output trim (no master volume) — level-matches the Plexi to the
        // other models so switching amps doesn't jump the volume.
        x * 3.39
    }
}
