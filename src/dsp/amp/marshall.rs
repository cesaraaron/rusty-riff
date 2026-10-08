use super::{
    AMP_MAX, AmpKnob, Amplifier, Bloom, BrightCap, Cached, CathodeBias, DynamicPresence, FrontEnd,
    GlobalNfb, GridBlock, OutputTransformer, PowerOs, SpeakerLoad, SupplyRipple, ToneCache,
    TubeClip, VoiceBalance, sagged_rail, split_gain,
};
use crate::dsp::biquad::Biquad;
use crate::dsp::oversample::Oversampler8;
use crate::dsp::tonestack::{Components, ToneStack};

/// Small-signal voltage gain of the two-triode preamp at `pregain = 1`.
///
/// 0.6067 = the old formulation's effective coefficient (`INSERTION_LOSS *
/// sqrt(2.24)`), so **gain 0 sounds the same as before** and the change is
/// entirely in what the knob does above it. Because the total now scales *linearly*
/// with `pregain` rather than with its square root, the preamp reaches +27.7 dB at
/// gain 1 against +11.7 dB before.
const PREAMP_GAIN_COEFF: f32 = 6.0 / 280.0;

/// Global negative feedback around the power stage.
///
/// `NFB_BETA` is the loop gain at full master, measured: 0.50 is the strongest
/// loop that still produces a musical result here, giving about -3 dB at 400 Hz
/// against -1.3 dB at 80 Hz. Above roughly 1.5 the LF filter's phase delay turns
/// the loop net-positive at low frequency and the amp runs away, so there is
/// little headroom and `GlobalNfb::feedback` is the only place to raise it.
///
/// `NFB_FLOOR` is how much of that survives at master = 0, and it is what puts the
/// master pot *in* the divider: turning down opens the loop rather than merely
/// getting quieter, which is why a Marshall at low master sounds like a different
/// amplifier from the same Marshall at full master.
///
/// `NFB_LF_CORNER` is where the divider stops acting on the bass. Feedback below it
/// is reduced, so the loop tightens the midrange and leaves the low end alone --
/// that LF-versus-mid relationship is the "scooped and tight" JCM800 character.
const NFB_BETA: f32 = 0.5;
const NFB_FLOOR: f32 = 0.35;
const NFB_LF_CORNER: f32 = 220.0;

/// JCM800 front-panel controls, in the order `process` decodes them.
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

/// Marshall JCM800 amplifier simulation.
///
/// Front-panel controls (DSP order): Gain, Bass, Mid, Treble, Presence, Master.
///
/// Signal path:
///   DC block → input HP → [8× OS: stage-1 tube + inter-stage HP + stage-2 tube] → tone stack → power amp sag → presence
///
/// Character:
///   • 8× oversampling through the nonlinear gain stages keeps aliasing well above
///     the audible band, removing the harsh "digital" edge of stacked clippers
///   • Asymmetric 12AX7 waveshaper generates even harmonics (2nd, 4th) for warmth
///   • Dynamic grid-bias bloom adds touch sensitivity under hard playing
///   • Inter-stage coupling HP at ~300 Hz (JCM800 22 nF cap into the following
///     grid resistor, which corners far lower than the textbook 720 Hz figure)
///     tightens low-end while keeping mid-neck fundamentals feeding stage 2
///   • Presence shelf in the power-amp NFB loop adds air and cut at 3.5 kHz
pub struct Marshall {
    sr: f32,
    /// Global NFB around the power stage, with the master pot in the divider.
    nfb: GlobalNfb,
    /// The power stage's previous output — the feedback divider reads this.
    pf_out: f32,
    // Pre-gain front end: DC block + input HP (base rate)
    front: FrontEnd,
    // 8× oversampling for the nonlinear section
    os: Oversampler8,
    // 8× oversampling for the power-stage clipper (sag envelope held per sample)
    os_power: PowerOs,
    // Bass cut before the first gain stage at 8× rate — prevents sub-bass from
    // entering the clipper and generating low-frequency IM products ("fart").
    pre_clip_hp: Biquad,
    // Inter-stage coupling HP between tube stages (at 8× rate)
    stage_hp: Biquad,
    // Dynamic preamp bloom
    bloom: Bloom,
    // Treble-bleed cap across the gain control (sparkle at low gain).
    bright: BrightCap,
    // Tone-stack change detector — recompute coefficients only when a knob moves.
    tone_cache: ToneCache,
    // Dynamic cathode-bias shift on the first triode stage (blocking-distortion
    // bloom / touch sensitivity), runs at 8× rate before the stage-1 waveshaper.
    cathode: CathodeBias,
    // Hard blocking distortion on the same stage: grid-conduction charge that
    // only a truly slammed input triggers (crackle-then-recover), at 8× rate.
    grid: GridBlock,
    // Output-transformer core saturation + push-pull crossover (base rate).
    xfmr: OutputTransformer,
    // Passive FMV tone stack (base rate) — bass/mid/treble interact like the real
    // JCM800 network, with the characteristic mid scoop.
    tone: ToneStack,
    // Presence — power-amp NFB characteristic, drive-dependent (base rate).
    presence: DynamicPresence,
    presence_cache: Cached,
    // Structural voicing balance (base rate): low shelf restores low-mid body, high
    // shelf tames the tone stack's treble-forward tilt, so notes stay even across
    // the neck rather than the upper register blasting out.
    voice: VoiceBalance,
    // Output DC blocker (the asymmetric power clip leaves a small offset).
    out_hp: Biquad,
    // Power amp envelope follower (sag simulation)
    envelope: f32,
    // Mains ripple riding on the sagging supply (ghost notes).
    ripple: SupplyRipple,
    // Power-amp ↔ speaker impedance interaction (dynamic low-end bloom).
    speaker: SpeakerLoad,
}

impl Marshall {
    /// Retune the speaker load for the selected cabinet (see
    /// [`SpeakerLoad::set_load`]).
    pub fn set_load(&mut self, load: (f32, f32)) {
        self.speaker.set_load(load.0, load.1);
    }

    pub fn new(sr: f32) -> Self {
        let sr8 = sr * 8.0;
        let mut m = Self {
            sr,
            nfb: GlobalNfb::new(sr, NFB_BETA, NFB_FLOOR, NFB_LF_CORNER),
            pf_out: 0.0,
            front: FrontEnd::new(sr, 60.0),
            os: Oversampler8::new(sr),
            os_power: PowerOs::new(sr),
            // JCM800 input coupling cap → sub-rumble cut at ~35 Hz, kept below the
            // 82 Hz low-E fundamental so the distorted bass string stays intact.
            pre_clip_hp: Biquad::highpass(sr8, 35.0, 0.707),
            // JCM800 inter-stage coupling → HP at ~300 Hz. The 22 nF cap with the
            // following grid resistor actually corners well below the old 720 Hz;
            // dropping it keeps the fundamental of mid-neck notes feeding the second
            // stage so the note leads its overtones, while still tightening the lows.
            stage_hp: Biquad::highpass(sr8, 300.0, 0.707),
            bloom: Bloom::new(sr, 8.0, 55.0),
            // Bright cap: JCM800 treble-bleed, corner ~2 kHz, gentle so it adds
            // sparkle at low gain without turning the clean edge brittle.
            bright: BrightCap::new(sr, 2000.0, 0.16),
            tone_cache: ToneCache::new(),
            // First-stage cathode bias: grid current charges fast (~1.5 ms), bleeds
            // back over ~45 ms. Threshold/depth kept light so the dynamic give lives
            // within a note and recovers between notes (no cross-note timbre drift).
            cathode: CathodeBias::new(sr8, 1.5, 45.0, 0.030, 1.0),
            // Blocking distortion: conduction threshold well above the cathode
            // stage's (2.6 vs 1.0 drive units) so ordinary playing never touches
            // it; near-instant charge (0.25 ms), grid-leak recovery ~30 ms.
            grid: GridBlock::new(sr8, 0.25, 30.0, 0.22, 2.6),
            // Output transformer: lows (core flux) below ~160 Hz compress; modest
            // drive and a trace of crossover for the woolly, complex cranked-PA low end.
            xfmr: OutputTransformer::new(sr, 160.0, 1.4, 0.045),
            tone: ToneStack::new(sr, Components::MARSHALL),
            // Presence in the NFB loop: 3.5 kHz corner, collapsing toward a +4 dB
            // open-loop lift as the sag envelope loads the loop.
            presence: DynamicPresence::new(sr, 3500.0, 4.0, 1.2),
            presence_cache: Cached::new(),
            // Body shelf deepened (+3.5 → +8) and measured against a commercial
            // JCM-family rig: a real driven Marshall carries its 110–350 Hz
            // low-mid body ~12 dB above the 1–1.4 kHz pocket; ours ran nearly
            // flat, which read as thin and quiet next to it.
            // Tilt eased −7 → −6 dB: with the cab, A3's overtone band
            // (0.9–2.4 kHz) measured a harmonic centroid of 1.42 vs a
            // professional rig's 2.18 — the note spoke as fundamental thump
            // ("palm muted"); see the pluck probes in examples/amp_analysis.
            voice: VoiceBalance::new(sr, 180.0, 8.0, 750.0, -6.0),
            out_hp: Biquad::highpass(sr, 12.0, 0.707),
            envelope: 0.0,
            // UK mains → full-wave ripple at 100 Hz; depth sized so ghost-note
            // sidebands sit ~30 dB under the notes only when the supply is loaded.
            ripple: SupplyRipple::new(sr, 100.0, 0.05),
            // 8×12 resonance ~95 Hz; tube amp has moderate damping. The bloom is
            // kept light so the low resonance supports the note without hanging
            // over the next palm-muted chug, preserving a percussive, muted feel.
            speaker: SpeakerLoad::new(sr, 95.0, 1.0, 0.06, 0.30, 0.8, 0.35),
        };
        m.update_tone_stack(0.5, 0.45, 0.65);
        m.update_presence(0.5);
        m
    }

    fn update_tone_stack(&mut self, bass: f32, mid: f32, treble: f32) {
        self.tone.update(bass, mid, treble);
    }

    fn update_presence(&mut self, presence: f32) {
        // Presence models the JCM800 output-transformer NFB loop: shelf at
        // 3.5 kHz, ±6 dB around a +2.5 dB static lift (the reference rig holds
        // its 1.8–5.6 kHz shelf well above the mid pocket even at noon). This is
        // the knob's *full-authority* setting; under drive the NFB loop collapses
        // and the shelf drifts toward the open-loop lift (see DynamicPresence).
        self.presence.set_knob((presence - 0.5) * 12.0 + 2.5);
    }

    #[inline]
    fn power_supply(&mut self, x: f32) -> f32 {
        let abs_x = x.abs();
        let coeff = if abs_x > self.envelope {
            1.0 - (-220.0 / self.sr).exp()
        } else {
            // Sag recovery ~150 ms. The original 200 ms release swelled palm-muted
            // chugs 20–40 ms in (fixed by dropping to 60 ms), but 60 ms also erased
            // the *singing* half of sag: on a held note the supply recovering over
            // the decay is what lifts the tail — the professional reference rig
            // holds a plucked note ~8 dB above its natural decay at 300 ms (see
            // examples/amp_analysis.rs), and with a 60 ms release all recovery
            // happened inside the attack, adding nothing. 150 ms keeps the chug
            // attack percussive (the 5 ms attack still ducks it instantly) while
            // the recovery spreads across the note's decay as sustain.
            1.0 - (-6.7 / self.sr).exp()
        };
        self.envelope += coeff * (abs_x - self.envelope);
        // The sag term stays strong so the supply compression reduces level
        // without bending the waveform, which keeps the attack percussive
        // while adding a little sustain under load.
        let sag = 1.0 / (1.0 + self.envelope * 1.5);
        // Mains ripple rides on the loaded supply: the 100 Hz gain modulation
        // intermodulates with the signal (ghost-note sidebands), fading out as
        // the supply unloads at idle.
        self.ripple.gain(sag, self.envelope)
    }

    #[inline]
    fn power_amp(&mut self, x: f32, master: f32, presence: f32) -> f32 {
        let supply = self.power_supply(x);
        // Power clipper at 8×: the envelope is slow (ms) so `supply` is held
        // across subsamples; only the memoryless curve runs hot, killing
        // base-rate fold-back fizz without touching feel.
        // The static drive stays around 2.2 so the decay remains on the tube
        // curve's knee, which adds tail compression without losing the note's
        // touch-sensitive even-harmonic growth. Pushing it much higher would
        // flatten the asymmetry and make the amp feel less responsive.
        //
        // `supply` is a rail voltage, so it goes through `sagged_rail`: as the
        // rails fall the drive *rises* and the output comes back down. See
        // `sagged_rail` — the old `clip(u * supply * 2.2)` reduced clipping under
        // load, which is backwards.
        let (drive_up, rail) = sagged_rail(supply, 2.2);
        // Subtract the divider's feedback from the stage's own input. Read from
        // `pf_out` (the previous sample) so the loop is causal.
        let fb = self.nfb.feedback(self.pf_out, master, presence);
        let out = self.os_power.shape(x - fb, |u| {
            TubeClip::PUSH_PULL.shape(u * drive_up) * rail * 0.62
        });
        self.pf_out = out;
        out
    }
}

impl Amplifier for Marshall {
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
        // Bright cap across the gain pot: injects highs that then feed the clipper,
        // strongest at low gain (see BrightCap).
        let x = self.bright.process(x, gain);

        let pregain = 1.0 + gain * 273.0;
        // Dynamic grid-bias offset (removed downstream by the inter-stage HP).
        // Bias depth halved and the bloom release shortened (above): the slow,
        // deep grid-bias follower stayed elevated between notes, so a note played
        // right after others got a louder, more even-harmonic attack than the same
        // note played alone — an audible note-to-note inconsistency. A lighter,
        // faster bloom keeps the touch-sensitive give within a note but recovers
        // between them so every note attacks the same.
        let bias = self.bloom.follow(x) * 0.06;

        // ── 8× oversampled nonlinear section ──────────────────────────────────
        // Explicit gain staging. The preamp's small-signal voltage gain is a
        // designed `PREAMP_GAIN_AT_1 * pregain`, split across the two triodes by
        // `split_gain`, and each stage clips against a fixed ±1 rail.
        //
        // This replaces `shape(x * g) / g.sqrt()`, whose small-signal gain was
        // `0.6366 * sqrt(g)` and whose saturated output *fell* as `1/sqrt(g)` — so
        // turning gain up clipped more and got quieter, leaving ~0.8 dB of level
        // authority across the whole knob and none for the master pot. Here the
        // preamp runs +4.7 dB at gain 0 to +27.7 dB at gain 1, so `gain` means gain.
        //
        // The split is still front-loaded: one stage driven 26× runs deep on its
        // plateau and squares the wave, and a square's slowly-decaying h5/h7 series
        // is the "cheap fizz" fingerprint. Two stages at ~7× and ~4× stay on the
        // round part of the curve and produce the fast-falling harmonic series a
        // real cascade measures.
        let (k1, k2) = split_gain(PREAMP_GAIN_COEFF * pregain, pregain, 0.2773, 0.6);
        let up = self.os.upsample(x);
        let mut down = [0.0f32; 8];
        for (o, &u) in down.iter_mut().zip(up.iter()) {
            let u = self.pre_clip_hp.process(u); // cut sub-bass before clipping
            // Dynamic cathode bias shifts the operating point under hard drive
            // before the stage-1 waveshaper; a truly slammed input additionally
            // triggers hard grid-blocking (crackle-then-recover). The inter-stage
            // HP strips the DC both inject.
            let d = self.grid.shift(self.cathode.shift(u + bias));
            let s = TubeClip::AX7.stage(d, k1);
            let s = self.stage_hp.process(s);
            *o = TubeClip::AX7.stage(s, k2);
        }
        let x = self.os.downsample(down);
        // ── end oversampled section ───────────────────────────────────────────

        // Passive FMV tone stack (base rate — no aliasing risk)
        let x = self.tone.process(x);
        // Structural voicing balance: restore low-mid body, tame the upper-mid tilt.
        let x = self.voice.process(x);

        // Power amp: transformer sag + light saturation
        let x = self.power_amp(x, master, presence);
        // Output transformer: low-frequency core saturation + push-pull crossover.
        let x = self.xfmr.process(x);

        // Speaker impedance interaction — dynamic low-end bloom driven by sag.
        let x = self.speaker.process(x, self.envelope);

        // Presence: output-transformer NFB shelf, losing authority as the sag
        // envelope loads the loop.
        let x = self.presence.process(x, self.envelope);

        // Output DC block: the asymmetric power-stage clip injects a small DC offset
        // and (unlike the Mesa/Randall) there is no power-section high-pass after it;
        // a real output transformer passes no DC, so strip it here before the trim.
        let x = self.out_hp.process(x);

        // Output trim: level-matches the JCM800 to the other models so switching
        // doesn't jump in volume (re-measured after the power-drive increase).
        x * master * 5.271
    }
}
