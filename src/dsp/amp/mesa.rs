use super::{
    AMP_MAX, AmpKnob, Amplifier, Bloom, BrightCap, Cached, CathodeBias, DynamicPresence, FrontEnd,
    GlobalNfb, GridBlock, INSERTION_LOSS, MASTER_DRIVE_REF, OutputTransformer, PowerOs,
    SpeakerLoad, SupplyRipple, ToneCache, TubeClip, VoiceBalance, sagged_rail, split_gain3,
};
use crate::dsp::biquad::Biquad;
use crate::dsp::oversample::Oversampler8;
use crate::dsp::tonestack::{Components, ToneStack};

/// Dual Rectifier front-panel controls, in the order `process` decodes them.
/// `6.0 / pregain_max` puts the top of the gain knob well past the stage's clipping
/// rail, so the knob spans clean to slammed on every model.
/// Global negative feedback around the power stage -- solid-state global loop; the Recto's is generous.
const NFB_BETA: f32 = 0.45;
const NFB_LF_CORNER: f32 = 240.0;

const PREAMP_GAIN_COEFF: f32 = 6.0 / 210.0;

pub const KNOBS: &[AmpKnob] = &[
    AmpKnob {
        label: "GAIN",
        slug: "gain",
        default: 0.75,
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
    AmpKnob {
        label: "MASTER",
        slug: "master",
        default: 0.55,
    },
];

/// Mesa/Boogie Dual Rectifier — Modern channel simulation.
///
/// Signal path:
///   DC block → input HP → [8× OS: stage-1 + HP + stage-2 + HP + stage-3 silicon] → tone stack → power amp → presence
///
/// Character:
///   • 8× oversampling through all three nonlinear stages keeps aliasing inaudible
///   • Asymmetric waveshapers on tube stages add even-harmonic warmth
///   • Dynamic grid-bias bloom adds touch sensitivity under hard playing
///   • Two inter-stage HPs (680 Hz and 1 kHz) prevent bass accumulation across stages
///   • Presence shelf at 4 kHz — Recto's presence is brighter/tighter than the JCM800
pub struct Mesa {
    sr: f32,
    /// Global NFB around the power stage, with a fixed divider (the master is a
    /// pre-phase-inverter volume, not part of the loop).
    nfb: GlobalNfb,
    /// The power stage's previous output — the divider reads this.
    pf_out: f32,
    front: FrontEnd,
    os: Oversampler8,
    os_power: PowerOs,
    // Pre-clip HP at 8× rate — cuts sub-bass before the first gain stage
    pre_clip_hp: Biquad,
    // Two inter-stage coupling HPs at 8× rate
    stage_hp_1: Biquad,
    stage_hp_2: Biquad,
    // Power-section subsonic cut (base rate, 4th-order = two cascaded biquads).
    // The asymmetric silicon stage generates strong difference tones on a low
    // power chord; this strips that inaudible sub-bass "fart" while leaving the
    // 82 Hz low-E fundamental intact.
    power_hp: Biquad,
    power_hp2: Biquad,
    bloom: Bloom,
    // Treble-bleed cap across the gain control (Recto bright cap).
    bright: BrightCap,
    // Dynamic cathode-bias shift on the first triode stage (8× rate).
    cathode: CathodeBias,
    // Hard blocking distortion on the same stage: grid-conduction charge that
    // only a truly slammed input triggers (crackle-then-recover), at 8× rate.
    grid: GridBlock,
    // Output-transformer core saturation + push-pull crossover (base rate).
    xfmr: OutputTransformer,
    // Passive FMV tone stack (base rate) — Fender-type values for the Recto's
    // thicker low end and gentler scoop.
    tone: ToneStack,
    tone_cache: ToneCache,
    // Presence — power-amp NFB characteristic, drive-dependent (base rate).
    presence: DynamicPresence,
    presence_cache: Cached,
    // Structural voicing balance (base rate): the FENDER tone stack is treble-heavy
    // and peak-normalised, so the upper mids/treble sit well above the low mids;
    // left alone, notes higher up the neck blast out. The low shelf restores body
    // and the high shelf tames the tilt, flattening the across-the-neck response.
    // The body shelf's corner is matched to the upper inter-stage HP so the low-mid
    // level it restores has no gap.
    voice: VoiceBalance,
    // Silicon-rectifier sag envelope
    envelope: f32,
    // Mains ripple riding on the sagging supply (ghost notes).
    ripple: SupplyRipple,
    // Power-amp ↔ speaker impedance interaction.
    speaker: SpeakerLoad,
}

impl Mesa {
    /// Retune the speaker load for the selected cabinet (see
    /// [`SpeakerLoad::set_load`]).
    pub fn set_load(&mut self, load: (f32, f32)) {
        self.speaker.set_load(load.0, load.1);
    }

    pub fn new(sr: f32) -> Self {
        let sr8 = sr * 8.0;
        let mut m = Self {
            sr,
            nfb: GlobalNfb::new(sr, NFB_BETA, NFB_LF_CORNER),
            pf_out: 0.0,
            front: FrontEnd::new(sr, 60.0),
            os: Oversampler8::new(sr),
            os_power: PowerOs::new(sr),
            // Recto input coupling HP at ~70 Hz — keeps sub-bass out of the gain
            // stages so they don't generate difference-tone mud, while preserving
            // the 82 Hz low-E fundamental.
            pre_clip_hp: Biquad::highpass(sr8, 70.0, 0.707),
            // Between stage 1 and 2: ~225 Hz. The original 680 Hz stripped the
            // fundamental of every note under ~1 kHz before the next clipper, so the
            // note's body vanished under a 10th-harmonic fizz. This corner keeps the
            // fundamental intact while still cutting enough low-mid in the cascade
            // that a palm-muted chug decays fast and stays percussive (the post-stack
            // `body` shelf restores the steady low-mid level for sustained notes).
            stage_hp_1: Biquad::highpass(sr8, 225.0, 0.707),
            // Between stage 2 and 3: ~320 Hz (silicon stage compresses harder, so a
            // tighter corner) — the chug-tightening cut, still below the fundamentals.
            stage_hp_2: Biquad::highpass(sr8, 320.0, 0.707),
            // Subsonic cut at 55 Hz, cascaded → 24 dB/oct. This keeps the 82 Hz
            // low-E fundamental full and weighty while still removing the
            // inaudible difference-tone fart well below it.
            power_hp: Biquad::highpass(sr, 55.0, 0.707),
            power_hp2: Biquad::highpass(sr, 55.0, 0.707),
            bloom: Bloom::new(sr, 8.0, 55.0),
            // Recto bright cap: corner a touch higher (~2.4 kHz) and lighter than the
            // JCM800 — the Recto is already a bright amp.
            bright: BrightCap::new(sr, 2400.0, 0.12),
            // First-stage cathode bias, same fast-charge / RC-recovery shape as the
            // JCM800; the Recto's tighter feel keeps the depth modest.
            cathode: CathodeBias::new(sr8, 1.5, 45.0, 0.055, 0.8),
            // Blocking distortion: threshold above ordinary playing levels; the
            // Recto's tighter front end recovers a touch faster than the JCM800.
            grid: GridBlock::new(sr8, 0.25, 24.0, 0.20, 2.6),
            // Output transformer: the big Recto iron compresses the lows; corner
            // ~140 Hz, gentle drive and a trace of crossover.
            xfmr: OutputTransformer::new(sr, 140.0, 1.5, 0.04),
            tone: ToneStack::new(sr, Components::FENDER),
            tone_cache: ToneCache::new(),
            // Presence in the NFB loop: 4 kHz corner; the Recto's stiffer silicon
            // supply loads the loop less, so its collapse is gentler than the JCM800.
            presence: DynamicPresence::new(sr, 4000.0, 3.5, 0.9),
            presence_cache: Cached::new(),
            voice: VoiceBalance::new(sr, 320.0, 9.0, 600.0, -9.5),
            envelope: 0.0,
            // US mains → full-wave ripple at 120 Hz; the silicon supply is stiffer
            // than the JCM800's, so a lighter ghost-note depth.
            ripple: SupplyRipple::new(sr, 120.0, 0.035),
            // Recto 4×12 resonance ~100 Hz; the speaker load is kept fairly tight
            // so palm-muted chugs stay percussive instead of blooming after the attack.
            speaker: SpeakerLoad::new(sr, 100.0, 1.0, 0.06, 0.22, 0.8, 0.35),
        };
        m.update_tone_stack(0.5, 0.45, 0.65);
        m.update_presence(0.5);
        m
    }

    fn update_tone_stack(&mut self, bass: f32, mid: f32, treble: f32) {
        self.tone.update(bass, mid, treble);
    }

    fn update_presence(&mut self, presence: f32) {
        // Recto presence: 4 kHz (brighter/tighter than JCM800 3.5 kHz), ±6 dB.
        // Full-authority setting; drive collapses it toward the open-loop lift.
        self.presence.set_knob((presence - 0.5) * 12.0 + 2.0);
    }

    /// Silicon rectifier sag: tight attack (0.5 ms), moderate release (80 ms).
    #[inline]
    fn power_amp(&mut self, x: f32, presence: f32) -> f32 {
        let abs_x = x.abs();
        let coeff = if abs_x > self.envelope {
            1.0 - (-1.0 / (0.0005 * self.sr)).exp()
        } else {
            1.0 - (-1.0 / (0.080 * self.sr)).exp()
        };
        self.envelope += coeff * (abs_x - self.envelope);
        // Silicon supply: stiff, true to the Recto (deep sag also scales the
        // asymmetric clip's drive down under sustained level, which would kill
        // the h2 growth that makes the amp touch-sensitive).
        let sag = 1.0 / (1.0 + self.envelope * 0.45);
        // 120 Hz mains ripple rides on the loaded supply (ghost-note sidebands),
        // fading out as the supply unloads at idle.
        let supply = self.ripple.gain(sag, self.envelope);
        // The static drive stays around 2.4 so the power stage adds a little
        // sustain without flattening the attack; the gentler curve preserves
        // the Recto's tight, percussive feel while still compressing the note.
        // Clipper at 8× (supply held per sample).
        let (drive_up, rail) = sagged_rail(supply, 2.4);
        // `supply` is a rail voltage: as it falls the drive rises and the output
        // comes back down, so sag compresses *harder* under load. See `sagged_rail`.
        // Subtract the divider's feedback from the stage's own input, read from
        // `pf_out` so the loop stays causal.
        let fb = self.nfb.feedback(self.pf_out, presence);
        let out = self.os_power.shape(x - fb, |u| {
            TubeClip::PUSH_PULL.shape(u * drive_up) * rail * 0.9964
        });
        self.pf_out = out;
        out
    }
}

impl Amplifier for Mesa {
    fn set_load(&mut self, load: (f32, f32)) {
        self.speaker.set_load(load.0, load.1);
    }

    #[inline]
    fn process(&mut self, sample: f32, knobs: &[f32; AMP_MAX]) -> f32 {
        let gain = knobs[0];
        let bass = knobs[1];
        let mid = knobs[2];
        let treble = knobs[3];
        let presence = knobs[4];
        let master = knobs[5];

        if self.tone_cache.changed(bass, mid, treble) {
            self.update_tone_stack(bass, mid, treble);
        }
        if self.presence_cache.changed(presence) {
            self.update_presence(presence);
        }

        let x = self.front.process(sample);
        // Bright cap across the gain pot (see BrightCap).
        let x = self.bright.process(x, gain);

        let pregain = 1.0 + gain * 210.0;
        // The bloom amount is kept light so the first stage adds touch-sensitive
        // compression without smearing the note attack or making successive hits
        // feel inconsistent.
        let bias = self.bloom.follow(x) * 0.09;

        // ── 8× oversampled nonlinear section ──────────────────────────────────
        // The per-stage gains stay moderate so the cascade saturates hard without
        // burying the fundamental under its own overtones. The final silicon stage
        // is still hotter than the tube amp stages, which gives the Recto its
        // modern aggression without letting any one stage run too deep.
        // Three-stage cascade. `split_gain3` keeps the total exact while the
        // exponents keep the front stages ahead of the last one, which is how a real
        // gain-staged preamp is laid out and what keeps any single stage off its own
        // clipper's deep-saturation plateau.
        let (k1, k2, k3) =
            split_gain3(PREAMP_GAIN_COEFF * pregain, 0.62, 1.4, 0.22, 1.8, 0.16, 1.3);
        let up = self.os.upsample(x);
        let mut down = [0.0f32; 8];
        for (o, &u) in down.iter_mut().zip(up.iter()) {
            let u = self.pre_clip_hp.process(u); // cut sub-bass before clipping
            // Dynamic cathode bias on stage 1, plus hard grid-blocking on truly
            // slammed inputs (DC removed by the inter-stage HP).
            let d = self.grid.shift(self.cathode.shift(u + bias));
            let s = TubeClip::AX7.stage(d, k1);
            let s = self.stage_hp_1.process(s);
            let s = TubeClip::AX7.stage(s, k2);
            let s = self.stage_hp_2.process(s);
            *o = silicon_stage(s, k3);
        }
        let x = self.os.downsample(down);
        // ── end oversampled section ───────────────────────────────────────────

        let x = self.tone.process(x);
        // Structural voicing balance: restore low-mid body, tame the upper-mid tilt.
        let x = self.voice.process(x);

        // Subsonic cut before the power stage so the silicon clipper can't fold
        // sub-bass into difference-tone mud.
        let x = self.power_hp.process(x);
        // The master drives the power stage's input (real master-volume behaviour).
        let drive = (master / MASTER_DRIVE_REF).clamp(0.0, 2.0);
        let x = self.power_amp(x * drive, presence);
        // Output transformer: low-frequency core saturation + push-pull crossover.
        let x = self.xfmr.process(x);
        let x = self.speaker.process(x, self.envelope);
        // Second subsonic stage after the asymmetric clipper, which regenerates a
        // low difference-tone "fart" from the chord's intervals.
        let x = self.power_hp2.process(x);
        // Presence: NFB shelf losing authority as the sag envelope loads the loop.
        let x = self.presence.process(x, self.envelope);

        // Output trim: level-match the Recto to the other models so switching
        // doesn't jump in volume. The master's level is in the power-amp input scale.
        x * MASTER_DRIVE_REF * 3.453
    }
}

/// Drive the silicon inverter stage for a designed small-signal voltage gain `k`.
///
/// The same explicit gain staging as [`TubeClip::stage`], for the Mesa's third
/// preamp stage — a silicon inverter rather than a triode, so it keeps its own
/// curve but borrows the staging discipline.
#[inline]
fn silicon_stage(x: f32, k: f32) -> f32 {
    silicon_clip_asym(x * k / INSERTION_LOSS)
}

/// Asymmetric silicon diode clipper.
///
/// Positive half: 1 - e^{-x} (forward-biased exponential).
/// Negative half: atan-based with 1.1× input scale — models the different
/// reverse-bias characteristic of real junction diodes (harder knee on neg swing).
#[inline]
fn silicon_clip_asym(x: f32) -> f32 {
    // Both halves have unity slope at zero — silicon is *linear* until it nears
    // a rail, so quiet playing passes clean (this is what lets the amp's h2 grow
    // with picking strength instead of idling at a static floor). The asymmetry
    // lives in the rails: the positive half saturates exponentially toward +1,
    // the negative half clamps harder toward −0.79 — even harmonics appear only
    // once the stage is actually driven.
    if x >= 0.0 {
        1.0 - (-x).exp()
    } else {
        (2.0 * x).atan() / 2.0
    }
}
