use super::{
    AMP_MAX, AmpKnob, Amplifier, Bloom, BrightCap, Cached, CathodeBias, DynamicPresence, FrontEnd,
    OutputTransformer, SpeakerLoad, ToneCache, VoiceBalance,
};
use crate::dsp::biquad::Biquad;
use crate::dsp::oversample::Oversampler8;
use crate::dsp::tonestack::{Components, ToneStack};

/// DR103 front-panel controls (DSP order): Brilliant Volume, Normal Volume,
/// Bass, Treble, Middle, Presence, Master.
pub const KNOBS: &[AmpKnob] = &[
    AmpKnob {
        label: "BRILL",
        slug: "gain",
        default: 0.75,
    },
    AmpKnob {
        label: "NORMAL",
        slug: "normal",
        default: 0.0,
    },
    AmpKnob {
        label: "BASS",
        slug: "bass",
        default: 1.0,
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

/// Hiwatt DR103 "Custom 100" amplifier simulation.
///
/// Signal path:
///   DC block → input HP → [8× OS: stage-1 tube + inter-stage HP + stage-2 tube] →
///   tone stack → voice balance → power amp sag → output transformer → speaker
///   load → presence → output trim
///
/// Character:
///   • The DR103 is a **high-headroom, hi-fi** amp, not a cranked Marshall. Its
///     Partridge transformers and stiff solid-state-rectified supply mean it stays
///     clean and tight at stage volume and is driven by the pedals in front of it —
///     which is exactly how Gilmour used it (Fuzz Face / Power Boost → Hiwatt).
///   • A **stiff, minimally-sagging power section** with a strong output transformer
///     keeps the low end tight and the amp clean far up the master, breaking up
///     only when genuinely slammed.
///   • A **wide-bandwidth, low-cut inter-stage HP** (~150 Hz) keeps the low end
///     solid and tight rather than the Marshall's thicker, mid-forward coupling.
///   • A **passive TMB tone stack** — the DR103's "unique TMB" network, wired
///     FMV-style with Hiwatt-derived values — gives a **flatter mid** than the
///     Marshall (larger mid-pot load) with a bright top, matching the DR103's
///     clear, un-scooped voice. It is **passive**, not an active Baxandall.
///   • Light power-amp sag and a strong output transformer keep the bottom tight;
///     the bloom is present but gentle, true to the amp's clean headroom.
pub struct Hiwatt {
    sr: f32,
    // Pre-gain front end: DC block + input HP (base rate)
    front: FrontEnd,
    // 8× oversampling for the nonlinear section
    os: Oversampler8,
    // Bass cut before the first gain stage at 8× rate — prevents sub-bass from
    // entering the clipper and generating low-frequency IM products ("fart").
    pre_clip_hp: Biquad,
    // Inter-stage coupling HP between tube stages (at 8× rate)
    stage_hp: Biquad,
    // Dynamic preamp bloom
    bloom: Bloom,
    // Treble-bleed cap across the gain control (mild — the DR103 is bright but
    // not a chime machine like the AC30).
    bright: BrightCap,
    // Normal-channel input low-pass: when the two channels are jumpered the Normal
    // channel contributes a darker, lower-gain parallel feed into the first stage.
    normal_lp: Biquad,
    // Tone-stack change detector — recompute coefficients only when a knob moves.
    tone_cache: ToneCache,
    // Dynamic cathode-bias shift on the first triode stage (touch sensitivity).
    cathode: CathodeBias,
    // Output-transformer core saturation + push-pull crossover (base rate).
    xfmr: OutputTransformer,
    // Passive FMV tone stack (base rate) — Hiwatt values give a flatter, less
    // scooped mid than the Marshall's.
    tone: ToneStack,
    // Presence — power-amp NFB characteristic, drive-dependent (base rate).
    presence: DynamicPresence,
    presence_cache: Cached,
    // Structural voicing balance (base rate) — a light body lift and a gentle tilt
    // trim, flatter than the Marshall's, keeping the DR103's even response.
    voice: VoiceBalance,
    // Output DC blocker (the asymmetric power clip leaves a small offset).
    out_hp: Biquad,
    // Power amp envelope follower (sag simulation)
    envelope: f32,
    // Power-amp ↔ speaker impedance interaction (dynamic low-end bloom).
    speaker: SpeakerLoad,
}

impl Hiwatt {
    pub fn new(sr: f32) -> Self {
        let sr8 = sr * 8.0;
        let mut h = Self {
            sr,
            front: FrontEnd::new(sr, 50.0),
            os: Oversampler8::new(sr),
            // DR103 input coupling → sub-rumble cut at ~30 Hz, below the 82 Hz
            // low-E fundamental so the distorted bass string stays intact.
            pre_clip_hp: Biquad::highpass(sr8, 30.0, 0.707),
            // Wider coupling than the JCM800's (~150 Hz vs ~300 Hz): the DR103's
            // hi-fi bandwidth keeps low-mid body feeding the second stage, part of
            // why it stays full and solid rather than thin when pushed.
            stage_hp: Biquad::highpass(sr8, 150.0, 0.707),
            bloom: Bloom::new(sr, 6.0, 45.0),
            // Bright cap: the DR103 is bright but not chimey — gentler and lower
            // than the AC30's Top Boost, closer to the JCM800's but softer.
            bright: BrightCap::new(sr, 1800.0, 0.10),
            // Normal channel is darker than the Brilliant channel (no bright cap,
            // rolled-off top) — ~3 kHz low-pass.
            normal_lp: Biquad::lowpass(sr, 3000.0, 0.707),
            tone_cache: ToneCache::new(),
            // First-stage cathode bias: light. The DR103 is a clean, high-headroom
            // amp — the give is there but subtle, and recovers quickly so
            // note-to-note attack stays consistent.
            cathode: CathodeBias::new(sr8, 1.5, 48.0, 0.032, 1.0),
            // Output transformer: a big Partridge — strong, with very little
            // low-frequency core saturation and a trace of crossover. The tight,
            // authoritative bottom is the DR103 signature.
            xfmr: OutputTransformer::new(sr, 140.0, 1.0, 0.02),
            tone: ToneStack::new(sr, Components::HIWATT),
            // Presence in the NFB loop: 4 kHz corner, collapsing toward a +3 dB
            // open-loop lift as the sag envelope loads the loop (less than the
            // Marshall's, the DR103's loop holds its authority longer).
            presence: DynamicPresence::new(sr, 4000.0, 3.0, 0.8),
            presence_cache: Cached::new(),
            // Light body lift and a gentle tilt trim — flatter than the Marshall's,
            // keeping the amp even across the neck rather than thick low-mids.
            voice: VoiceBalance::new(sr, 160.0, 4.0, 900.0, -3.5),
            out_hp: Biquad::highpass(sr, 12.0, 0.707),
            envelope: 0.0,
            // 4×12 resonance ~90 Hz, fairly well damped (a stiff supply and a big
            // transformer keep the bottom tight); a little dynamic bloom under load.
            speaker: SpeakerLoad::new(sr, 90.0, 0.9, 0.05, 0.22, 0.7),
        };
        h.update_tone_stack(0.5, 0.45, 0.65);
        h.update_presence(0.5);
        h
    }

    fn update_tone_stack(&mut self, bass: f32, mid: f32, treble: f32) {
        self.tone.update(bass, mid, treble);
    }

    fn update_presence(&mut self, presence: f32) {
        // Presence models the DR103's output-transformer NFB loop: shelf at 4 kHz,
        // ±6 dB around a +2 dB static lift (less static lift than the JCM800's —
        // the loop is stiffer and holds the response flatter).
        self.presence.set_knob((presence - 0.5) * 12.0 + 2.0);
    }

    #[inline]
    fn power_amp(&mut self, x: f32) -> f32 {
        let abs_x = x.abs();
        let coeff = if abs_x > self.envelope {
            1.0 - (-200.0 / self.sr).exp()
        } else {
            // Solid-state-rectified supply: stiffer than the Marshall models, so it
            // sags less and recovers faster. ~90 ms release keeps the bottom tight
            // and percussive.
            1.0 - (-11.0 / self.sr).exp()
        };
        self.envelope += coeff * (abs_x - self.envelope);
        // Gentler sag than the Marshall (0.45 vs 1.5): the DR103's supply barely
        // compresses, so the amp stays clean and loud.
        let sag = 1.0 / (1.0 + self.envelope * 0.45);
        // Modest drive (1.8) and a healthy output scale keep the amp on the round,
        // clean part of the curve until it is genuinely pushed.
        tube_clip_asym(x * sag * 1.8) * 0.7
    }
}

impl Amplifier for Hiwatt {
    #[inline]
    fn process(&mut self, sample: f32, knobs: &[f32; AMP_MAX]) -> f32 {
        let gain = knobs[0];
        let normal = knobs[1];
        let bass = knobs[2];
        let treble = knobs[3];
        let mid = knobs[4];
        let presence = knobs[5];
        let master = knobs[6];

        if self.tone_cache.changed(bass, mid, treble) {
            self.update_tone_stack(bass, mid, treble);
        }
        if self.presence_cache.changed(presence) {
            self.update_presence(presence);
        }

        let x = self.front.process(sample);
        // Jumpered channels: the Brilliant channel drives the preamp; the Normal
        // channel adds its own darker, lower-gain parallel feed into the first stage.
        let x = x + self.normal_lp.process(x * normal * 0.6);
        // Bright cap across the gain pot: injects highs that then feed the clipper,
        // strongest at low gain (see BrightCap). Driven by the Brilliant volume.
        let x = self.bright.process(x, gain);

        // Max gain 34× — enough preamp range to break up and stay touch-sensitive
        // when pushed, while the stiff power section keeps the amp clean and tight
        // at stage volume.
        let pregain = 1.0 + gain * 34.0;
        // Dynamic grid-bias offset (removed downstream by the inter-stage HP).
        // A touch deeper than the Marshall's: the DR103's clean preamp needs the
        // operating-point drift to stay touch-responsive without hard clipping.
        let bias = self.bloom.follow(x) * 0.14;

        // ── 8× oversampled nonlinear section ──────────────────────────────────
        let up = self.os.upsample(x);
        let mut down = [0.0f32; 8];
        for (o, &u) in down.iter_mut().zip(up.iter()) {
            let u = self.pre_clip_hp.process(u); // cut sub-bass before clipping
            // Dynamic cathode bias shifts the operating point under hard drive
            // before the stage-1 waveshaper; the inter-stage HP strips its DC.
            let d = self.cathode.shift((u + bias) * pregain);
            let s = tube_clip_asym(d) / pregain.sqrt();
            let s = self.stage_hp.process(s);
            *o = tube_clip_asym(s * 2.6) / 2.6_f32.sqrt();
        }
        let x = self.os.downsample(down);
        // ── end oversampled section ───────────────────────────────────────────

        // Passive FMV tone stack (base rate — no aliasing risk)
        let x = self.tone.process(x);
        // Structural voicing balance: light low-mid body, gentle upper-mid trim.
        let x = self.voice.process(x);

        // Power amp: transformer sag + light saturation
        let x = self.power_amp(x);
        // Output transformer: low-frequency core saturation + push-pull crossover.
        let x = self.xfmr.process(x);

        // Speaker impedance interaction — dynamic low-end bloom driven by sag.
        let x = self.speaker.process(x, self.envelope);

        // Presence: output-transformer NFB shelf, losing authority as the sag
        // envelope loads the loop.
        let x = self.presence.process(x, self.envelope);

        // Output DC block: the asymmetric power-stage clip injects a small DC offset;
        // a real output transformer passes no DC, so strip it here before the trim.
        let x = self.out_hp.process(x);

        // Output trim: level-matched to the other models so switching amps doesn't
        // jump in volume. Lands the DR103 mid-band alongside the Vox/Mesa/Randall.
        x * master * 14.33
    }
}

/// Asymmetric 12AX7 triode waveshaper (see marshall.rs for rationale).
#[inline]
fn tube_clip_asym(x: f32) -> f32 {
    use std::f32::consts::FRAC_2_PI;
    if x >= 0.0 {
        FRAC_2_PI * x.atan()
    } else {
        // Negative half saturates faster; still asymptotically approaches -1
        FRAC_2_PI * (x * 1.1).atan()
    }
}
