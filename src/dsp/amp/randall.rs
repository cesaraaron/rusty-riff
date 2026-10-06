use super::{
    AMP_MAX, AmpKnob, Amplifier, Bloom, Cached, FrontEnd, PowerOs, SpeakerLoad, ToneCache,
    VoiceBalance, split_gain,
};
use crate::dsp::biquad::Biquad;
use crate::dsp::oversample::Oversampler8;

/// Warhead front-panel controls, in the order `process` decodes them.
/// `6.0 / pregain_max` puts the top of the gain knob well past the stage's clipping
/// rail, so the knob spans clean to slammed on every model.
const PREAMP_GAIN_COEFF: f32 = 6.0 / 238.0;

/// The final op-amp limiter's share of the preamp gain. Fixed, as before: this
/// stage is a safety rail, not a gain stage, and its job is to catch the peaks
/// the two gain stages ahead of it produced.
const RAIL_LIMITER_GAIN: f32 = 2.2;

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

/// Randall Warhead solid-state amp simulation.
///
/// Signal path:
///   DC block → input HP → [8× OS: FET + HP + BJT + HP + rail clip] → active tone stack → presence → stiff power section
///
/// Character:
///   • 8× oversampling through all three gain stages keeps aliasing inaudible
///   • Asymmetric FET waveshaper adds subtle even harmonics
///   • A touch of dynamic bloom keeps the otherwise stiff solid-state feel responsive
///   • Two inter-stage HPs (500 Hz and 800 Hz) tighten the solid-state response
///   • Presence knob (user-adjustable shelf at 5 kHz)
pub struct Randall {
    sr: f32,
    front: FrontEnd,
    os: Oversampler8,
    os_power: PowerOs,
    // Pre-clip HP at 8× rate — the Warhead's tight solid-state input coupling
    pre_clip_hp: Biquad,
    // Inter-stage HPs at 8× rate
    stage_hp_1: Biquad,
    stage_hp_2: Biquad,
    // Power section subsonic cut (base rate, 4th-order = two cascaded biquads).
    // Prevents the output tanh distorting sub-bass and strips the inaudible
    // difference-tone "fart" a low power chord generates, while the 70 Hz corner
    // leaves the 82 Hz low-E fundamental essentially intact.
    power_hp: Biquad,
    power_hp2: Biquad,
    bloom: Bloom,
    // Active tone stack (base rate)
    bass_shelf: Biquad,
    mid_peak: Biquad,
    treble_shelf: Biquad,
    tone_cache: ToneCache,
    // Presence (base rate)
    presence_shelf: Biquad,
    presence_cache: Cached,
    // Structural voicing balance (base rate): restore low-mid body (the tight
    // solid-state input + power high-passes gut the low E) and tame the upper-mid
    // tilt, so notes stay even in level across the neck.
    voice: VoiceBalance,
    // Speaker impedance interaction (static — stiff rails, high damping factor).
    speaker: SpeakerLoad,
}

impl Randall {
    /// Retune the speaker load for the selected cabinet (see
    /// [`SpeakerLoad::set_load`]).
    pub fn set_load(&mut self, load: (f32, f32)) {
        self.speaker.set_load(load.0, load.1);
    }

    pub fn new(sr: f32) -> Self {
        let sr8 = sr * 8.0;
        let mut r = Self {
            sr,
            // 75 Hz input HP: tighter than tube amps (60 Hz) but doesn't cut 82 Hz low-E
            front: FrontEnd::new(sr, 75.0),
            os: Oversampler8::new(sr),
            os_power: PowerOs::new(sr),
            // Warhead pre-clip HP: 55 Hz — tighter than Marshall/Mesa but below 82 Hz
            pre_clip_hp: Biquad::highpass(sr8, 55.0, 0.707),
            // After FET stage: 195 Hz. The old 500 Hz corner sat above the
            // fundamental of most fretted notes and buried the note under high
            // harmonics. This keeps the note's body while cutting enough low-mid in
            // the cascade for tight, percussive palm mutes (the post-stack `body`
            // shelf restores the steady low-mid level for sustained notes).
            stage_hp_1: Biquad::highpass(sr8, 195.0, 0.707),
            // After BJT stage: 285 Hz (driver-stage coupling) — the chug-tightening
            // cut, matched by the body shelf corner so sustained notes stay even.
            stage_hp_2: Biquad::highpass(sr8, 285.0, 0.707),
            // Output stage HP at 70 Hz, cascaded → 24 dB/oct. Lets the 82 Hz
            // fundamental through while hard-killing the sub-bass fart below it.
            power_hp: Biquad::highpass(sr, 55.0, 0.707),
            power_hp2: Biquad::highpass(sr, 55.0, 0.707),
            bloom: Bloom::new(sr, 8.0, 50.0),
            bass_shelf: Biquad::low_shelf(sr, 80.0, 0.0),
            mid_peak: Biquad::peak_eq(sr, 500.0, 0.4, 0.0),
            treble_shelf: Biquad::high_shelf(sr, 4500.0, 0.0),
            tone_cache: ToneCache::new(),
            presence_shelf: Biquad::high_shelf(sr, 5000.0, 3.0),
            presence_cache: Cached::new(),
            voice: VoiceBalance::new(sr, 260.0, 8.5, 800.0, -4.0),
            // Tight 8×12 resonance ~90 Hz, modest and static (no rectifier sag).
            speaker: SpeakerLoad::new(sr, 90.0, 1.0, 0.05, 0.0, 0.8, 0.35),
        };
        r.update_tone_stack(0.5, 0.3, 0.75);
        r.update_presence(0.5);
        r
    }

    fn update_tone_stack(&mut self, bass: f32, mid: f32, treble: f32) {
        self.bass_shelf
            .set_low_shelf(self.sr, 80.0, (bass - 0.5) * 30.0);
        self.mid_peak
            .set_peak_eq(self.sr, 500.0, 0.4, (mid - 0.5) * 24.0);
        self.treble_shelf
            .set_high_shelf(self.sr, 4500.0, (treble - 0.5) * 30.0);
    }

    fn update_presence(&mut self, presence: f32) {
        // Randall presence at 5 kHz (glassy solid-state top end), +3 dB at noon → ±6 dB range
        let gain_db = 3.0 + (presence - 0.5) * 12.0;
        self.presence_shelf.set_high_shelf(self.sr, 5000.0, gain_db);
    }
}

impl Amplifier for Randall {
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

        let pregain = 1.0 + gain * 238.0;
        // The bloom is kept light and fast so the Randall does not carry the
        // previous note's attack into the next hit, keeping each note's attack
        // consistent.
        let bias = self.bloom.follow(x) * 0.022;

        // ── 8× oversampled nonlinear section ──────────────────────────────────
        // The BJT and rail stages are kept a little milder so the 3rd–7th
        // harmonics do not overpower the fundamental. The gain split is still
        // more aggressive than the tube amps, which preserves the Randall's
        // buzzy, solid-state edge without over-squaring the waveform.
        let up = self.os.upsample(x);
        let mut down = [0.0f32; 8];
        for (o, &u) in down.iter_mut().zip(up.iter()) {
            let u = self.pre_clip_hp.process(u); // cut sub-bass before FET stage
            // Explicit gain staging, same discipline as `TubeClip::stage`: a
            // designed small-signal voltage gain per stage and a ceiling set by
            // each clipper rather than by the drive. All three Warhead curves have
            // unit small-signal slope, so unlike the tube stages there is no
            // insertion loss to divide out here.
            let (k1, k2) = split_gain(PREAMP_GAIN_COEFF * pregain, pregain, 0.2568, 0.7);
            let s = fet_clip_asym((u + bias) * k1);
            let s = self.stage_hp_1.process(s);
            let s = bjt_clip(s * k2);
            let s = self.stage_hp_2.process(s);
            // The rail stage stays at a moderate drive so it adds grit rather than
            // turning the third clipper into a second brickwall over the BJT.
            *o = rail_clip(s * RAIL_LIMITER_GAIN);
        }
        let x = self.os.downsample(down);
        // ── end oversampled section ───────────────────────────────────────────

        let x = self.bass_shelf.process(x);
        let x = self.mid_peak.process(x);
        let x = self.treble_shelf.process(x);
        let x = self.presence_shelf.process(x);
        // Structural voicing balance: restore low-mid body, tame the upper-mid tilt.
        let x = self.voice.process(x);

        // The solid-state power section uses stiff rails and no sag. The drive is
        // kept slightly below the raw rail-clip level so the stage stays punchy
        // without flattening every pick transient. The HP before the clipper also
        // keeps the output stage from distorting sub-bass. The rail tanh runs at
        // 8× like the tube power stages — base-rate clipping here folded
        // harmonics back as harshness.
        let x = self.power_hp.process(x);
        let x = self.os_power.shape(x, |u| (u * 1.85).tanh() * 0.54);
        let x = self.speaker.process(x, 0.0);
        // Second subsonic stage after the tanh: the clipper regenerates a low
        // difference-tone "fart" from the chord's intervals; strip it here.
        let x = self.power_hp2.process(x);

        // Output trim: 1.10 holds the Randall ~+4 dB above the tube family on
        // the amp-only probe — the known exception. Do NOT "fix" it by lowering:
        // the trim also sets the cab drive operating point (SpeakerDrive
        // breakup + mic saturation), and starving it buries E2's fundamental
        // under overtones (`fundamental_is_not_buried_under_overtones`) and
        // breaks the DS-chain level match. Re-tuning it means re-tuning the cab.
        x * master * 0.762
    }
}

/// Asymmetric FET saturation.
///
/// f(x) = x / sqrt(1 + x²) — smooth approach to ±1, softer than tanh.
/// Negative half uses 1.08× input scale to simulate FET pinch-off asymmetry.
#[inline]
fn fet_clip_asym(x: f32) -> f32 {
    if x >= 0.0 {
        x / (1.0 + x * x).sqrt()
    } else {
        let x2 = x * 1.08;
        x2 / (1.0 + x2 * x2).sqrt()
    }
}

/// BJT transistor clip — standard tanh, harder knee than FET.
#[inline]
fn bjt_clip(x: f32) -> f32 {
    x.tanh()
}

/// Op-amp rail limiter — hard clip with a soft knee above 0.85.
///
/// The knee is deliberately not instant so the limiter adds a rounded
/// transition instead of snapping every pick transient straight to the rail.
/// That keeps the attack crest from collapsing into a choked, palm-muted
/// sound and preserves the solid-state bite.
#[inline]
fn rail_clip(x: f32) -> f32 {
    let lim = 0.85_f32;
    let abs_x = x.abs();
    if abs_x <= lim {
        x
    } else {
        let excess = abs_x - lim;
        let knee = excess / (1.0 + excess * 5.0);
        x.signum() * (lim + knee * (1.0 - lim))
    }
}
