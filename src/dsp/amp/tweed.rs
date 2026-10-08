use super::{
    AMP_MAX, AmpKnob, Amplifier, Bloom, BrightCap, Cached, CathodeBias, FrontEnd, GlobalNfb,
    GridBlock, OutputTransformer, PowerOs, SpeakerLoad, SupplyRipple, TubeClip, VoiceBalance,
    sagged_rail, split_gain,
};
use crate::dsp::biquad::Biquad;
use crate::dsp::oversample::Oversampler8;

/// Tweed Deluxe (5E3-style) front-panel controls (DSP order): Volume, Tone.
///
/// The real 5E3 has two channels, each with a Volume, plus one shared Tone. We
/// expose a single Volume (the channels jumpered/paralleled) and the Tone.
/// Small-signal voltage gain of the preamp per unit of `pregain`.
///
/// `2.0 / pregain_max` puts the top of the gain knob exactly at the stage's
/// clipping rail, so the knob spans clean at the bottom to slammed at the top
/// on every model regardless of how much range its `pregain` law has.
/// Global negative feedback around the power stage -- classic 'pin the tail' feedback: strong, LF rolled off.
const NFB_BETA: f32 = 0.6;
const NFB_FLOOR: f32 = 0.4;
const NFB_LF_CORNER: f32 = 170.0;

const PREAMP_GAIN_COEFF: f32 = 6.0 / 154.0;

pub const KNOBS: &[AmpKnob] = &[
    AmpKnob {
        label: "VOLUME",
        slug: "gain",
        default: 0.72,
    },
    AmpKnob {
        label: "TONE",
        slug: "treble",
        default: 0.60,
    },
];

/// Tweed Deluxe small combo — the "Hotel California" lead voice.
///
/// Signal path:
///   DC block → input HP → [8× OS: stage-1 tube + inter-stage HP + stage-2 tube] →
///   tone (treble cut) → voice balance → 5Y3-rectified power amp sag → small
///   output transformer → speaker load → output trim.
///
/// Character (an approximation of a ~15 W cathode-biased 6V6 tweed combo, not a
/// schematic-exact model):
///   • **Early, loose breakup.** Cathode-biased 6V6s with no negative feedback
///     clip and compress very early — warm, thick and mid-forward with a soft,
///     slightly flubby low end. The 1959 Les Paul → cranked Deluxe is the cited
///     Felder lead voice.
///   • **5Y3 valve rectifier.** The supply sags and recovers slowly, so notes
///     bloom and the whole amp feels elastic under a held bend.
///   • **Single Tone control.** The 5E3 tone is a simple treble cut, not an FMV
///     stack: one knob, warm and dark at the bottom, bright at the top.
///   • **Small output transformer and a 1×12 speaker** — boxy low-end
///     compression and a tight, vocal midrange.
pub struct Tweed {
    sr: f32,
    /// Global NFB around the power stage, with the master pot in the divider.
    nfb: GlobalNfb,
    /// The power stage's previous output — the divider reads this.
    pf_out: f32,
    front: FrontEnd,
    os: Oversampler8,
    os_power: PowerOs,
    pre_clip_hp: Biquad,
    stage_hp: Biquad,
    bloom: Bloom,
    // Subtle treble-bleed across the volume pot (a little sparkle when backed off).
    bright: BrightCap,
    // Tone = treble cut: a low-pass whose corner the knob sweeps.
    tone_cache: Cached,
    tone: Biquad,
    cathode: CathodeBias,
    grid: GridBlock,
    xfmr: OutputTransformer,
    voice: VoiceBalance,
    out_hp: Biquad,
    envelope: f32,
    ripple: SupplyRipple,
    speaker: SpeakerLoad,
}

impl Tweed {
    /// Retune the speaker load for the selected cabinet (see
    /// [`SpeakerLoad::set_load`]).
    pub fn set_load(&mut self, load: (f32, f32)) {
        self.speaker.set_load(load.0, load.1);
    }

    pub fn new(sr: f32) -> Self {
        let sr8 = sr * 8.0;
        let mut t = Self {
            sr,
            nfb: GlobalNfb::new(sr, NFB_BETA, NFB_FLOOR, NFB_LF_CORNER),
            pf_out: 0.0,
            front: FrontEnd::new(sr, 60.0),
            os: Oversampler8::new(sr),
            os_power: PowerOs::new(sr),
            pre_clip_hp: Biquad::highpass(sr8, 40.0, 0.707),
            stage_hp: Biquad::highpass(sr8, 110.0, 0.707),
            bloom: Bloom::new(sr, 6.0, 45.0),
            bright: BrightCap::new(sr, 1800.0, 0.14),
            tone_cache: Cached::new(),
            tone: Biquad::lowpass(sr, 5000.0, 0.707),
            // Cathode-biased 6V6: a soft, quick-ish first-stage give.
            cathode: CathodeBias::new(sr8, 1.5, 46.0, 0.04, 1.0),
            grid: GridBlock::new(sr8, 0.25, 30.0, 0.22, 2.4),
            // Small output transformer: saturates early and adds low-end compression.
            xfmr: OutputTransformer::new(sr, 130.0, 1.4, 0.05),
            voice: VoiceBalance::new(sr, 160.0, 5.5, 800.0, -4.0),
            out_hp: Biquad::highpass(sr, 12.0, 0.707),
            envelope: 0.0,
            ripple: SupplyRipple::new(sr, 100.0, 0.045),
            speaker: SpeakerLoad::new(sr, 95.0, 1.0, 0.06, 0.30, 0.85, 0.35),
        };
        t.update_tone(0.6);
        t
    }

    /// Tone knob → the treble-cut corner (dark at 0, open at 1).
    fn update_tone(&mut self, tone: f32) {
        let tone = tone.clamp(0.0, 1.0);
        // ~1.4 kHz fully cut to ~8 kHz wide open.
        let corner = 1400.0 * (8000.0f32 / 1400.0).powf(tone);
        self.tone.set_lowpass(self.sr, corner, 0.707);
    }

    /// 5Y3-rectified sag: a deep-but-slow give — the supply compresses under load
    /// and recovers over the note.
    #[inline]
    fn power_amp(&mut self, x: f32, master: f32, presence: f32) -> f32 {
        let abs_x = x.abs();
        let coeff = if abs_x > self.envelope {
            1.0 - (-120.0 / self.sr).exp()
        } else {
            // ~200 ms recovery: a small valve rectifier refills slowly.
            1.0 - (-5.0 / self.sr).exp()
        };
        self.envelope += coeff * (abs_x - self.envelope);
        let sag = 1.0 / (1.0 + self.envelope * 1.1);
        let supply = self.ripple.gain(sag, self.envelope);
        // Power clipper at 8× (supply held per sample).
        let (drive_up, rail) = sagged_rail(supply, 1.8);
        // `supply` is a rail voltage: as it falls the drive rises and the output
        // comes back down, so sag compresses *harder* under load. See `sagged_rail`.
        // Subtract the divider's feedback from the stage's own input, read from
        // `pf_out` so the loop stays causal.
        let fb = self.nfb.feedback(self.pf_out, master, presence);
        let out = self.os_power.shape(x - fb, |u| {
            TubeClip::PUSH_PULL.shape(u * drive_up) * rail * 0.66
        });
        self.pf_out = out;
        out
    }
}

impl Amplifier for Tweed {
    fn set_load(&mut self, load: (f32, f32)) {
        self.speaker.set_load(load.0, load.1);
    }

    #[inline]
    fn process(&mut self, sample: f32, knobs: &[f32; AMP_MAX]) -> f32 {
        let gain = knobs[0];
        let tone = knobs[1];

        if self.tone_cache.changed(tone) {
            self.update_tone(tone);
        }

        let x = self.front.process(sample);
        let x = self.bright.process(x, gain);

        // Pushed preamp gain: a tweed Deluxe is into breakup well before the top
        // of the Volume knob, so the small power section is slammed early.
        let pregain = 1.0 + gain * 154.0;
        let bias = self.bloom.follow(x) * 0.18;
        let (k1, k2) = split_gain(PREAMP_GAIN_COEFF * pregain, pregain, 0.241, 0.6);

        // ── 8× oversampled nonlinear section ──────────────────────────────────
        let up = self.os.upsample(x);
        let mut down = [0.0f32; 8];
        for (o, &u) in down.iter_mut().zip(up.iter()) {
            let u = self.pre_clip_hp.process(u);
            let d = self.grid.shift(self.cathode.shift(u + bias));
            let s = TubeClip::V6.stage(d, k1);
            let s = self.stage_hp.process(s);
            *o = TubeClip::V6.stage(s, k2);
        }
        let x = self.os.downsample(down);
        // ── end oversampled section ───────────────────────────────────────────

        let x = self.tone.process(x);
        let x = self.voice.process(x);
        // No master pot on the Tweed, so the divider runs at full loop gain; the
        // tone pot feeds the loop's high-frequency bleed (neutral at centre).
        let x = self.power_amp(x, 1.0, tone);
        let x = self.xfmr.process(x);
        let x = self.speaker.process(x, self.envelope);
        let x = self.out_hp.process(x);

        x * 1.405
    }
}
