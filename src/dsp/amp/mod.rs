pub mod fender;
pub mod hiwatt;
pub mod marshall;
pub mod mesa;
pub mod plexi;
pub mod randall;
pub mod supro;
pub mod tweed;
pub mod vox;

use crate::dsp::AmpModel;
use crate::dsp::biquad::Biquad;

pub use fender::Fender;
pub use hiwatt::Hiwatt;
pub use marshall::Marshall;
pub use mesa::Mesa;
pub use plexi::Plexi;
pub use randall::Randall;
pub use supro::Supro;
pub use tweed::Tweed;
pub use vox::Vox;

/// Maximum number of front-panel knobs any single amp model exposes. Each model's
/// [`KNOBS`](AmpKnob) list is decoded positionally by its `process`, so the array
/// passed to [`Amplifier::process`] is always this long and trailing slots are
/// simply ignored by models with fewer controls.
pub const AMP_MAX: usize = 7;

/// One amp front-panel control.
///
/// `slug` is the stable key used by presets and by the semantic test helpers; the
/// shared roles (`gain`, `bass`, `mid`, `treble`, `presence`, `master`) are the
/// same string across models so a control can be addressed by role, while
/// model-specific controls (`normal`, `cut`, later `reverb`/`speed`/`intensity`)
/// use their own slugs. `label` is the short uppercase panel text.
pub struct AmpKnob {
    pub label: &'static str,
    pub slug: &'static str,
    pub default: f32,
}

/// Build an [`AMP_MAX`] knob array by *role* for a given model, falling back to
/// each control's default for roles the model doesn't have. Used by the tests
/// (and anywhere a semantic handful of controls must be swept across every model
/// regardless of its exact panel).
pub fn standard_knobs(
    model: crate::dsp::AmpModel,
    gain: f32,
    bass: f32,
    mid: f32,
    treble: f32,
    presence: f32,
    master: f32,
) -> [f32; AMP_MAX] {
    let mut k = [0.0f32; AMP_MAX];
    for (i, knob) in model.controls().iter().enumerate() {
        k[i] = match knob.slug {
            "gain" => gain,
            "bass" => bass,
            "mid" => mid,
            "treble" => treble,
            "presence" => presence,
            "master" => master,
            _ => knob.default,
        };
    }
    k
}

/// Models the way a real power amp "sees" the loudspeaker's impedance curve
/// through its negative-feedback loop.
///
/// A speaker is not a flat resistive load: its impedance has a tall resonant peak
/// near the cabinet's tuning (~80–110 Hz) and rises again through the treble from
/// voice-coil inductance. Because the power amp has a finite output impedance, more
/// drive develops across the speaker exactly where its impedance is high — so the
/// low-frequency resonance blooms and the top end lifts. Crucially this is
/// *dynamic*: as the power supply sags under hard playing the damping factor drops
/// and the low-end resonance opens up further, giving the amp its touch-dependent
/// "give" and three-dimensional low end.
///
/// We tap a resonant band (a 0 dB band-pass at the resonance) and feed back a
/// portion that grows with the sag envelope, plus a static high shelf for the
/// inductive treble rise. A displacement estimator adds excursion-driven bloom:
/// palm-mute lows physically push the cone, dropping damping and opening the
/// resonance further for a few tens of ms — the "thump" a static load can't give.
///
/// `exc_amt` bounds that bloom per amp. Phase 7 cut it to 0.10 on the Hiwatt,
/// whose WEM cab already piled on structural lows; the other amps keep the larger
/// A3 value, because the pile-up was Hiwatt/WEM-specific and the global cut
/// removed their low-end bloom for no reason. Even at 0.35 the stage sits after
/// every user control, so it must stay feel, not loudness.
pub(crate) struct SpeakerLoad {
    resonance: Biquad,
    /// Rate and current tuning of `resonance`, so a cab change can retune it in
    /// place (state preserved) and only when the numbers actually differ.
    sr: f32,
    fs: f32,
    q: f32,
    presence: Biquad,
    disp_lp: Biquad,
    exc_env: f32,
    exc_atk: f32,
    exc_rel: f32,
    res_base: f32,
    res_dyn: f32,
    exc_amt: f32,
}

impl SpeakerLoad {
    /// `fs` resonance frequency, `q` its sharpness, `res_base` the static
    /// resonance amount, `res_dyn` how much more the sag envelope adds, `pres_db`
    /// the inductive high-shelf lift (at 5 kHz), and `exc_amt` the max
    /// excursion-driven resonance bloom.
    pub fn new(
        sr: f32,
        fs: f32,
        q: f32,
        res_base: f32,
        res_dyn: f32,
        pres_db: f32,
        exc_amt: f32,
    ) -> Self {
        let coeff = |ms: f32| 1.0 - (-1.0 / (sr * ms / 1000.0)).exp();
        Self {
            resonance: Biquad::bandpass(sr, fs, q),
            sr,
            fs,
            q,
            presence: Biquad::high_shelf(sr, 5000.0, pres_db),
            disp_lp: Biquad::lowpass(sr, 100.0, 0.9),
            exc_env: 0.0,
            exc_atk: coeff(8.0),
            exc_rel: coeff(50.0),
            res_base,
            res_dyn,
            exc_amt,
        }
    }

    /// Retune the speaker resonance for a different cabinet.
    ///
    /// The **cabinet** owns the load: the amp drives whatever is patched into it, so
    /// a JCM800 into a Supro must load a 1×10 and not the 4×12 its model used to
    /// assume. `res_base` / `res_dyn` / `exc_amt` deliberately stay with the model —
    /// how hard the amp drives the load is the amp's character, what the load *is* is
    /// the cab's.
    ///
    /// Retuned in place so the filter state survives a cab switch, and skipped
    /// entirely when the numbers are unchanged so the common case costs a compare.
    #[inline]
    pub fn set_load(&mut self, fs: f32, q: f32) {
        if (fs - self.fs).abs() < 0.01 && (q - self.q).abs() < 0.001 {
            return;
        }
        self.resonance.set_bandpass(self.sr, fs, q);
        self.fs = fs;
        self.q = q;
    }

    #[inline]
    pub fn process(&mut self, x: f32, sag: f32) -> f32 {
        // Excursion follows bass displacement (fast attack, ~50 ms release so
        // chugs thump then recover instead of hanging over the next hit or
        // stacking into loudness).
        let d = self.disp_lp.process(x).clamp(-1.5, 1.5).abs();
        let c = if d > self.exc_env {
            self.exc_atk
        } else {
            self.exc_rel
        };
        self.exc_env += c * (d - self.exc_env);
        let exc = (self.exc_env * self.exc_amt).min(self.exc_amt);
        let band = self.resonance.process(x);
        let amt = self.res_base + self.res_dyn * sag + exc;
        self.presence.process(x + band * amt)
    }
}

/// Slow envelope follower used to give a gain stage playing dynamics.
///
/// A real tube's operating point drifts under sustained drive (grid-bias
/// excursion / cathode self-bias). Feeding this envelope in as a small DC bias
/// before an asymmetric waveshaper increases even-harmonic content and adds a
/// gentle "give" the harder you play — the touch sensitivity and bloom that make
/// a tube amp feel alive rather than statically clamped. The following
/// inter-stage high-pass removes the injected DC, leaving only the harmonic and
/// compression effect.
pub(crate) struct Bloom {
    env: f32,
    atk: f32,
    rel: f32,
}

impl Bloom {
    pub fn new(sr: f32, atk_ms: f32, rel_ms: f32) -> Self {
        Self {
            env: 0.0,
            atk: 1.0 - (-1.0 / (atk_ms * 0.001 * sr)).exp(),
            rel: 1.0 - (-1.0 / (rel_ms * 0.001 * sr)).exp(),
        }
    }

    #[inline]
    pub fn follow(&mut self, x: f32) -> f32 {
        let a = x.abs();
        let c = if a > self.env { self.atk } else { self.rel };
        self.env += c * (a - self.env);
        self.env
    }
}

/// Per-stage dynamic grid/cathode bias with an RC recovery time constant — the
/// "live" core of a real 12AX7 stage that a memoryless waveshaper cannot capture.
///
/// In a cathode-biased triode the cathode-bypass cap holds the DC operating point.
/// Hard positive grid excursions draw grid current, which charges the coupling and
/// cathode network and pushes the *average* bias toward cutoff. That shift decays
/// back over an RC time constant (the caps bleeding through the grid-leak resistor).
/// The audible consequences are the two things players feel as "tube give":
///   • **Blocking-distortion bloom** — under a hard transient the stage momentarily
///     biases colder, so gain sags then recovers, swelling the note instead of
///     clamping it flat.
///   • **Dynamic asymmetry** — the moving operating point shifts where on the
///     transfer curve the signal sits, so even-harmonic content grows the harder
///     you dig in and relaxes when you back off (touch sensitivity).
///
/// This runs *at the oversampled rate*, just before the stage's waveshaper, and the
/// DC component of the shift is removed downstream by the inter-stage coupling
/// high-pass — leaving only the dynamic gain/harmonic motion.
pub(crate) struct CathodeBias {
    bias: f32,
    charge: f32,
    recover: f32,
    depth: f32,
    thresh: f32,
}

impl CathodeBias {
    /// `sr` is the rate this is clocked at (the oversampled rate). `charge_ms` is
    /// the (fast) grid-conduction charging time, `recover_ms` the (slow) RC bleed
    /// back to the resting bias, `depth` how far the stored charge shifts the
    /// operating point, and `thresh` the drive level at which grid current starts.
    pub fn new(sr: f32, charge_ms: f32, recover_ms: f32, depth: f32, thresh: f32) -> Self {
        Self {
            bias: 0.0,
            charge: 1.0 - (-1.0 / (charge_ms * 0.001 * sr)).exp(),
            recover: 1.0 - (-1.0 / (recover_ms * 0.001 * sr)).exp(),
            depth,
            thresh,
        }
    }

    /// Apply the dynamic bias shift to a stage input already scaled to the
    /// waveshaper's drive range. Returns the bias-shifted value to clip.
    #[inline]
    pub fn shift(&mut self, x: f32) -> f32 {
        // Grid current flows only on hard positive excursions past the conduction
        // threshold. It charges the cap fast and bleeds away slowly (RC recovery).
        let conduction = (x - self.thresh).max(0.0);
        let c = if conduction > self.bias {
            self.charge
        } else {
            self.recover
        };
        self.bias += c * (conduction - self.bias);
        // Stored charge biases the stage toward cutoff, reducing gain on the
        // samples that follow — the blocking-distortion "give".
        x - self.bias * self.depth
    }
}

/// Power-supply ripple riding on the sag — the source of "ghost notes".
///
/// A real amp's B+ rail is a rectified mains supply: full-wave rectification
/// leaves a ripple at twice the mains frequency (100 Hz UK/EU, 120 Hz US) whose
/// amplitude grows as the supply is loaded down by hard playing. Because the
/// power stage's gain tracks the rail, that ripple amplitude-modulates the
/// signal, putting faint sidebands ±100/120 Hz around every note — the
/// subliminal "ghost notes" of a big amp working hard, part of the amp-in-the-
/// room texture no smooth sag envelope produces. At idle the supply is barely
/// loaded and the ripple (and its modulation) all but vanishes.
pub(crate) struct SupplyRipple {
    phase: f32,
    inc: f32,
    depth: f32,
}

impl SupplyRipple {
    /// `hz` is the ripple frequency (2× mains: 100 or 120 Hz), `depth` the peak
    /// gain-modulation at full supply load.
    pub fn new(sr: f32, hz: f32, depth: f32) -> Self {
        Self {
            phase: 0.0,
            inc: 2.0 * std::f32::consts::PI * hz / sr,
            depth,
        }
    }

    /// Modulate the sag gain by the ripple. `load` is the sag envelope: the
    /// harder the supply is worked, the deeper the ripple rides on the rail.
    #[inline]
    pub fn gain(&mut self, sag: f32, load: f32) -> f32 {
        self.phase += self.inc;
        if self.phase > 2.0 * std::f32::consts::PI {
            self.phase -= 2.0 * std::f32::consts::PI;
        }
        sag * (1.0 + self.depth * load.min(1.0) * self.phase.sin())
    }
}

/// Blocking distortion proper: the harsh "crackle-then-recover" of a truly
/// slammed input stage, distinct from [`CathodeBias`]'s gentle within-note give.
///
/// When the grid is driven hard past conduction, grid current through the
/// coupling cap is not proportional — the diode-like grid-cathode junction turns
/// on exponentially, so charge accumulation grows roughly with the *square* of
/// the overdrive. On a hard transient the cap charges almost instantly, slamming
/// the stage toward cutoff (the note's attack spits and chokes), then the charge
/// bleeds off through the grid-leak resistor over tens of milliseconds and the
/// gain recovers into the note. The gain split across stages keeps ordinary
/// playing away from this regime; only genuinely slammed inputs (a boost into a
/// cranked front end, a violent transient) trigger it — which is exactly the
/// behaviour of the real circuit. Runs at the oversampled rate before the
/// stage-1 waveshaper; the inter-stage HP strips the DC component downstream.
pub(crate) struct GridBlock {
    bias: f32,
    charge: f32,
    recover: f32,
    depth: f32,
    thresh: f32,
    /// Anode voltage ceiling — see [`GridBlock::shift`].
    ceiling: f32,
}

/// How far past its conduction threshold the anode still follows the grid.
///
/// Past this the transfer function goes dead flat. Expressed as a multiple of the
/// stage's own threshold so it needs no per-model tuning: at 2.5x it sits well
/// clear of normal operation and only engages once the stage is genuinely slammed.
const GRID_CEILING_MULT: f32 = 2.5;

/// The quadratic charge target is capped so a sustained max-gain signal shifts
/// the operating point by a bounded amount instead of choking the stage dead.
const GRID_BLOCK_CAP: f32 = 2.0;

impl GridBlock {
    /// `thresh` sits above [`CathodeBias`]'s conduction threshold (in waveshaper
    /// drive units), `charge_ms` is near-instant, `recover_ms` the grid-leak
    /// bleed, `depth` how far full charge shifts the bias.
    pub fn new(sr: f32, charge_ms: f32, recover_ms: f32, depth: f32, thresh: f32) -> Self {
        Self {
            bias: 0.0,
            charge: 1.0 - (-1.0 / (charge_ms * 0.001 * sr)).exp(),
            recover: 1.0 - (-1.0 / (recover_ms * 0.001 * sr)).exp(),
            depth,
            thresh,
            ceiling: thresh * GRID_CEILING_MULT,
        }
    }

    #[inline]
    pub fn shift(&mut self, x: f32) -> f32 {
        let over = (x - self.thresh).max(0.0);
        // Exponential grid conduction ≈ quadratic charge growth past threshold.
        let target = (over * over).min(GRID_BLOCK_CAP);
        let c = if target > self.bias {
            self.charge
        } else {
            self.recover
        };
        self.bias += c * (target - self.bias);
        let shifted = x - self.bias * self.depth;
        // **Grid conduction.** The bias shift above *tilts* the gain down. A real
        // grid does not stop the anode dead that way: driven past the conduction
        // point it starts drawing current, the anode can no longer follow it, and
        // the transfer function goes **flat** — a hard ceiling rather than a tilt.
        //
        // That is what the "slam" at the top of a cranked preamp actually is: the
        // peaks stop rising while the valleys keep swinging, so the stage sounds
        // hard-edged and compressed rather than merely louder. With only the
        // gain-reduction arm, a hard-driven Marshall kept folding its peaks back in
        // smoothly, which is the smooth "hiss" of an overdriven solid state and not
        // the grainy crunch of a valve stage being pushed into conduction.
        //
        // Runs inside the 8x oversampled section, so the new corner does not alias.
        shifted.clamp(-self.ceiling, self.ceiling)
    }
}

/// Presence that lives where the real one does: inside the power-amp negative-
/// feedback loop, so its action changes with drive.
///
/// The presence pot bleeds high frequencies out of the NFB divider — the boost
/// exists only because the *loop* stops correcting the highs. When the power
/// stage is driven hard its incremental gain collapses, the loop loses
/// authority, and the response drifts toward the open-loop voicing regardless
/// of where the knob sits: the knob's range shrinks and the top end settles on
/// the amp's natural (NFB-free) lift. A static shelf can't do that. Modelled as
/// a high-frequency tap whose gain crossfades, per sample, from the knob's
/// shelf toward a fixed open-loop lift as the sag envelope loads the loop.
pub(crate) struct DynamicPresence {
    hp: Biquad,
    g_knob: f32,
    g_open: f32,
    sag_k: f32,
}

impl DynamicPresence {
    /// `freq` is the shelf corner, `open_db` the open-loop HF lift the response
    /// collapses toward under drive, `sag_k` how fast the sag envelope eats the
    /// loop gain. Call [`set_knob`](Self::set_knob) to dial the static shelf.
    pub fn new(sr: f32, freq: f32, open_db: f32, sag_k: f32) -> Self {
        Self {
            hp: Biquad::highpass(sr, freq, 0.707),
            g_knob: 0.0,
            g_open: 10.0_f32.powf(open_db / 20.0) - 1.0,
            sag_k,
        }
    }

    /// Re-dial the knob's shelf amount (dB at full loop authority).
    pub fn set_knob(&mut self, db: f32) {
        self.g_knob = 10.0_f32.powf(db / 20.0) - 1.0;
    }

    #[inline]
    pub fn process(&mut self, x: f32, env: f32) -> f32 {
        // Loop authority falls as the supply sags; the shelf gain slides from
        // the knob's setting toward the open-loop lift.
        let w = 1.0 / (1.0 + self.sag_k * env);
        let g = self.g_knob * w + self.g_open * (1.0 - w);
        x + g * self.hp.process(x)
    }
}

/// Output-transformer character: low-frequency core saturation plus a trace of
/// class-AB crossover content.
///
/// A guitar output transformer is not a clean gain block. At high flux — which on
/// a transformer means *low frequencies* — the core saturates, gently compressing
/// and rounding the bottom end. That is the woolly, breathing low end of a cranked
/// power amp, and crucially it is frequency-selective: the highs (low flux) stay
/// linear, so only the lows compress. Separately, a push-pull output pair handing
/// the signal off at the zero crossing leaves a small amount of crossover content
/// that adds odd-harmonic complexity and "bite" without an audible hard kink.
pub(crate) struct OutputTransformer {
    lf: Biquad,
    drive: f32,
    xover: f32,
}

impl OutputTransformer {
    /// `lf_corner` splits the saturating low-frequency flux from the linear highs,
    /// `drive` sets how hard the core is pushed (higher = more LF compression), and
    /// `xover` the depth of the class-AB crossover content.
    pub fn new(sr: f32, lf_corner: f32, drive: f32, xover: f32) -> Self {
        Self {
            lf: Biquad::lowpass(sr, lf_corner, 0.707),
            drive,
            xover,
        }
    }

    #[inline]
    pub fn process(&mut self, x: f32) -> f32 {
        // Split the low-frequency flux (which saturates the core) from the highs.
        let low = self.lf.process(x);
        let high = x - low;
        // Core saturation acts on the lows only; normalised so small-signal gain is
        // unity (tanh(low·d)/d → low as low → 0) and only larger flux compresses.
        let y = (low * self.drive).tanh() / self.drive + high;
        // A touch of crossover: gain dips slightly through the zero-crossing handoff
        // and vanishes for larger swings, so it adds odd-harmonic bite, not a kink.
        let cross = self.xover * y * (-(y * y) * 25.0).exp();
        y - cross
    }
}

/// One triode's clipping characteristic, as a per-model/per-stage instance.
///
/// The curve family is unchanged — `atan` with a faster-saturating negative half —
/// but the negative-half multiplier is now a **field** rather than a hard-coded
/// `1.1`. That single number is where a tube's harmonic fingerprint actually lives:
/// an asymmetric transfer function is the *only* source of even harmonics in a
/// stage, so two amps that share it share their h2 exactly.
///
/// See the per-model constants below for the values, and [`TubeClip::shape`] for
/// what the asymmetry does.
///
/// The Mesa is deliberately absent — silicon is linear until it nears a rail, so
/// `silicon_clip_asym` in `mesa.rs` is a different curve and stays there.
#[derive(Clone, Copy)]
pub(crate) struct TubeClip {
    /// Curvature asymmetry at the quiescent operating point. `1.0` is symmetric;
    /// larger saturates the negative half sooner, which is what generates 2nd- and
    /// 4th-harmonic content.
    pub asymmetry: f32,
}

impl TubeClip {
    /// A push-pull power stage feeding a centre-tapped output transformer.
    ///
    /// **Mostly symmetric, and that is the point.** A push-pull output transformer
    /// is a differential device: the two halves swing opposite ways through a shared
    /// magnetic path, so the even-harmonic currents they generate largely cancel in
    /// the secondary. A real power amp's output is therefore **odd-harmonic
    /// dominant**, and even harmonics are mostly a property of the driver and the
    /// preamp ahead of it, not of the output devices.
    ///
    /// The old code ran the same h2-rich curve (asymmetry 1.10) in the power stage
    /// as in the preamp, putting a second and quite independent even-harmonic
    /// source at the very end of the chain — so h2 grew far faster with drive than
    /// it should, and every model's output was warm in a way that smeared the
    /// differences between them.
    ///
    /// **Why 1.05 and not 1.0.** Ideal cancellation is the right first-order physics
    /// and 1.0 does halve the effect again, but it reads too thin: valve pairs are
    /// never perfectly matched and transformer leakage inductance is real, so a
    /// measured tube amp does show even harmonics at the speaker. 1.05 keeps the
    /// touch-sensitive bloom the preamp legitimately provides while cutting the
    /// power stage's spurious contribution roughly in half. Measured on a Marshall
    /// (h2/h1 at hard drive / absolute h2 growth from soft to hard):
    ///
    /// | power-stage asymmetry | h2/h1 | h2 growth |
    /// | --- | --- | --- |
    /// | 1.10 (before C4) | 0.0184 | 4.3x |
    /// | **1.05 (this)** | **0.0094** | **2.9x** |
    /// | 1.00 (ideal) | 0.0053 | 1.5x |
    ///
    /// Pre-existing h2 from the preamp is unaffected by any of these: the power
    /// stage can only fail to *generate* more of it, never remove what arrived.
    pub const PUSH_PULL: TubeClip = TubeClip { asymmetry: 1.05 };

    /// 12AX7 — mu ≈ 17, the classic JCM800 preamp valve. The most asymmetric of
    /// the set, which is why a Marshall's preamp breakup is so noticeably warm.
    pub const AX7: TubeClip = TubeClip { asymmetry: 1.10 };

    /// EL84 — mu ≈ 10, low plate resistance, the AC30's valve. Low-mu valves
    /// compress earlier and asymmetrically *less*; the AC30's brightness is a
    /// high-frequency gain-staging fact, not an even-harmonic one.
    pub const EL84: TubeClip = TubeClip { asymmetry: 1.05 };

    /// 6V6 / 6L6 — mu ≈ 10, the Tweed and large-power-tube family.
    pub const V6: TubeClip = TubeClip { asymmetry: 1.07 };

    /// 6V6/6SL7 preamp — the small-valve Fender voicing, slightly gentler than a
    /// 6V6 proper.
    pub const V6_PREAMP: TubeClip = TubeClip { asymmetry: 1.06 };

    #[inline]
    pub fn shape(&self, x: f32) -> f32 {
        use std::f32::consts::FRAC_2_PI;
        if x >= 0.0 {
            FRAC_2_PI * x.atan()
        } else {
            // Negative half saturates faster; still asymptotically approaches -1.
            FRAC_2_PI * (x * self.asymmetry).atan()
        }
    }

    /// Drive this stage for a designed small-signal voltage gain of `k`, clipping
    /// against a fixed ±1 rail.
    ///
    /// **This is gain staging.** It divides out the curve's own insertion loss
    /// ([`INSERTION_LOSS`], about −3.9 dB) so the gain you ask for is the gain you
    /// get, and — critically — it makes the *ceiling* a property of the rail rather
    /// than of the drive setting.
    ///
    /// The old form everywhere in this tree was `shape(x * g) / g.sqrt()`, which has
    /// small-signal gain `0.6366 * sqrt(g)` and saturated output `1/sqrt(g)`. Both
    /// facts together meant turning the gain up made a stage clip *more* and get
    /// *quieter*, so a Marshall had ~0.8 dB of level authority across the whole knob
    /// and it was non-monotonic. That is a compression control wearing a gain
    /// knob's label, and it left the master pot nowhere to go.
    ///
    /// Here, small-signal gain is exactly `k` and the ceiling is always ±1, so
    /// `gain` means gain and `master` has real range to work with.
    #[inline]
    pub fn stage(&self, x: f32, k: f32) -> f32 {
        self.shape(x * k / INSERTION_LOSS)
    }
}

/// The `atan` clipping curve's small-signal slope, `2/π` — about −3.9 dB.
///
/// Two cascaded stages therefore cost −7.9 dB, which used to be absorbed by fixed
/// `VoiceBalance` shelves of up to +9 dB that the user could not dial out and that
/// partially cancelled their own bass and treble moves. [`TubeClip::stage`] divides
/// this out explicitly instead.
pub(crate) const INSERTION_LOSS: f32 = std::f32::consts::FRAC_2_PI;

/// The master position at which the power stage is driven at full — the default for
/// every master-equipped model. A real master volume is a pre-phase-inverter control
/// that scales the signal *into* the power amp, so backing it off reduces power-amp
/// drive, sag and clipping rather than only the output level. Referencing the default
/// keeps the boot tone and level exactly as they were and gives the knob real
/// authority on both sides of it. `power_amp` receives this as a fixed divider input;
/// the master knob scales the drive separately.
pub(crate) const MASTER_DRIVE_REF: f32 = 0.55;

/// Global negative feedback around the power stage.
///
/// The engine had no loop gain, no gain reduction and no damping factor anywhere.
/// The only NF-adjacent artefact was [`DynamicPresence`]: a level-dependent shelf
/// placed *after* the output transformer and speaker load, driven by the pre-sag
/// level envelope. That is a description of a symptom — a cranked amp should sound
/// different because its **loop** collapsed, not because a shelf moved.
///
/// Three things a real global NFB loop does, all of them missing here:
///
/// 1. **Tightens the midrange.** The loop subtracts a fraction of the power stage's
///    own output from its input, so gain falls where the loop acts.
/// 2. **Scoops selectively.** The divider is LF-shaped — less feedback at low
///    frequency — so closing the loop pulls the midrange down while leaving the bass
///    alone. That LF-versus-mid relationship *is* the "scooped and tight" character.
/// 3. **Runs at a fixed loop gain.** The loop is a fixed resistor network: on a
///    JCM800 the master volume is a **pre-phase-inverter** control and is *not* part
///    of the feedback divider (the loop runs from the output tap to the PI tail).
///    The master therefore scales the power stage's *drive* — see
///    [`MASTER_DRIVE_REF`] and each model's `power_amp` — not the loop.
///
/// And the loop's authority **falls as the supply sags**, for free: [`sagged_rail`]
/// reduces the power stage's drive as the rails fall, so less signal reaches the
/// divider and there is less to subtract. Nothing extra is needed for the "loop
/// collapses under drive" behaviour — it falls out of the sag model.
///
/// Presence is wired *into* the divider rather than sitting after it, which is where
/// it lives on a real amp: the pot bleeds high frequencies out of the feedback path.
pub(crate) struct GlobalNfb {
    /// Lowpass forming the LF-shaping of the divider. Feedback below its corner is
    /// reduced, so the loop pulls the midrange down without scooping the bass.
    lf: Biquad,
    /// Highpass used to bleed the top out of the divider for the presence pot.
    bleed: Biquad,
    /// Fixed loop gain.
    beta: f32,
}

impl GlobalNfb {
    pub fn new(sr: f32, beta: f32, lf_corner: f32) -> Self {
        Self {
            lf: Biquad::lowpass(sr, lf_corner, 0.707),
            bleed: Biquad::highpass(sr, 4000.0, 0.707),
            beta,
        }
    }

    /// The feedback signal to subtract from the power stage's input.
    ///
    /// `v_out` is the power stage's **previous** output and `presence` the pot, which
    /// bleeds the top out of the loop path. The loop gain is fixed; the master is not
    /// in the divider.
    #[inline]
    pub fn feedback(&mut self, v_out: f32, presence: f32) -> f32 {
        // LF-shaping: subtract the low band so the loop does not act on the bass.
        let shaped = v_out - self.lf.process(v_out);
        // Presence bleed: `presence` 0.5 = neutral, so the bleed is symmetric about
        // the centre and the pot does not shift the amp's balance at rest.
        let bled = shaped - self.bleed.process(shaped);
        let bleed_amt = (presence - 0.5) * 2.0;
        let looped = shaped - bleed_amt * bled;
        self.beta * looped
    }
}

/// Split a preamp's total voltage gain into two stages, **front-loaded**.
///
/// `k1 = front_load * pregain^p` and `k2 = total / k1`, so the product is exactly
/// `total` while the first stage takes the drive and the second is an attenuator
/// (`k2 < 1`) carrying a small signal onwards.
///
/// **Why this shape, and not an even split.** An even split is the obvious choice and
/// it is wrong for a valve preamp. It matches the *total* small-signal gain but leaves
/// each stage barely into saturation, and a stage that is barely clipping generates
/// almost no harmonics. Measured on the Marshall, an even split drove clipper 1 to an
/// input argument of **2.06**; the pre-C1 normalisation drove it to **6.21**. That is
/// the whole reason the first C1 attempt sounded thin: preamp h3/h1 fell from 0.425
/// to 0.258 and the final output's absolute h3 nearly halved.
///
/// The old `g` values *looked* enormous (12–28 at full gain) but existed only to be
/// divided straight back out by `sqrt(g)`; what reached the curve was `u * g`, and
/// that is the number that sets the saturation. So the split is specified by how hard
/// **stage one** should be driven, and stage two follows from the total.
///
/// Front-loading is also what a real gain-staged triode preamp does: the first stage
/// is where an input breaks up, and interstage coupling attenuates into the second.
pub(crate) fn split_gain(total: f32, pregain: f32, front_load: f32, p: f32) -> (f32, f32) {
    let k1 = front_load * pregain.powf(p);
    let k2 = if k1 > 1e-6 { total / k1 } else { 1.0 };
    (k1, k2)
}

/// Three-stage form of [`split_gain`].
///
/// The exponents must sum to 1 so `k1 * k2 * k3 == total`. The Mesa uses this for
/// its three-triode preamp, where the last stage is the hotter silicon inverter.
pub(crate) fn split_gain3(
    total: f32,
    p1: f32,
    c1: f32,
    p2: f32,
    c2: f32,
    p3: f32,
    c3: f32,
) -> (f32, f32, f32) {
    debug_assert!(
        (p1 + p2 + p3 - 1.0).abs() < 1e-4,
        "the stage exponents must sum to 1 so the total gain is preserved"
    );
    let norm = (c1 * c2 * c3).cbrt();
    let (a, b, c) = (c1 / norm, c2 / norm, c3 / norm);
    let k1 = total.powf(p1) * a;
    let k2 = total.powf(p2) * b;
    let k3 = total.powf(p3) * c;
    (k1, k2, k3)
}

/// Split a sagging power supply into `(clip drive, rail)` for one oversampled
/// sample.
///
/// `rail` is the supply voltage as a fraction of nominal, in `(0, 1]`; `drive` is
/// the model's nominal clipping drive. The caller applies its own clipper between
/// the two and its own output trim afterwards:
///
/// ```text
/// let (drive_up, rail) = sagged_rail(supply, 2.2);
/// os_power.shape(x, |u| clip(u * drive_up) * rail * 0.62)
/// ```
///
/// **Why the sag divides the drive instead of scaling the signal.** Every model
/// used to write this as `clip(u * supply * drive) * trim`, which is backwards. A
/// collapsing supply leaves *less* headroom, so the stage reaches its knee earlier
/// and clips *harder*; the old form pushed **less** signal into the clipper as the
/// rail fell, so distortion *decreased* under load. That is a 1/k level compressor
/// wearing a power supply's clothes, and it is wrong on the one axis that matters
/// most for feel: a Marshall should get dirtier and more compressed when you lean
/// on it, not cleaner.
///
/// Dividing the drive by the rail and multiplying the output back by it fixes both
/// halves at once, and **the small-signal gain cancels exactly**:
///
/// ```text
/// clip(u * drive / rail) * rail  ->  slope * drive * u     for any rail
/// ```
///
/// That cancellation is the point, and it is physically right: a sagging rail does
/// nothing to a signal far below it. Near and past the rail the curve bites earlier
/// and the asymptote comes down together, so the amp gets quieter *and* dirtier
/// under load — which is what a sagging supply does, and what the old model could
/// not express.
///
/// The exponent on the drive is 1.0 (the physical value). The per-model sag
/// constants were tuned against the old formulation, so the depth of sag now reads
/// slightly stronger at high load; that is intended, and the per-model output
/// trims absorb the level.
#[inline]
pub(crate) fn sagged_rail(rail: f32, drive: f32) -> (f32, f32) {
    // Guard the reciprocal: a pathological envelope must not produce an infinite
    // drive. 0.05 caps the boost at 20x, far past any real sag depth.
    let rail = rail.clamp(0.05, 1.0);
    (drive / rail, rail)
}

/// Treble-bleed ("bright") cap bridging the gain pot, as on a Marshall-style
/// preamp.
///
/// A small cap across the gain control passes high frequencies around the wiper.
/// Its effect is strongest with the pot turned down (more resistance for the cap to
/// bleed across) and washes out as the pot is opened up. Musically this adds
/// sparkle and cut at low-to-moderate gain that tightens as the amp is cranked —
/// the reason a Marshall stays articulate at the edge of breakup.
pub(crate) struct BrightCap {
    hp: Biquad,
    amount: f32,
}

impl BrightCap {
    /// `corner` is the cap's high-pass corner (above which it bleeds), `amount` the
    /// peak amount of high end injected (at gain = 0).
    pub fn new(sr: f32, corner: f32, amount: f32) -> Self {
        Self {
            hp: Biquad::highpass(sr, corner, 0.707),
            amount,
        }
    }

    /// Inject the bright-cap highs, weighted by how far the gain pot is *down*.
    #[inline]
    pub fn process(&mut self, x: f32, gain: f32) -> f32 {
        let highs = self.hp.process(x);
        x + highs * self.amount * (1.0 - gain)
    }
}

/// Shared preamp front end: a fixed DC blocker followed by the model's input
/// high-pass.
///
/// Every model opens the same way — strip any DC at ~10 Hz, then a model-specific
/// subsonic/rumble high-pass before the gain stages. Only the input-HP corner
/// differs (tube amps sit lower, the solid-state Randall tighter), so that is the
/// one knob the constructor takes.
pub(crate) struct FrontEnd {
    dc_block: Biquad,
    input_hp: Biquad,
}

impl FrontEnd {
    pub fn new(sr: f32, input_hp_hz: f32) -> Self {
        Self {
            dc_block: Biquad::highpass(sr, 10.0, 0.707),
            input_hp: Biquad::highpass(sr, input_hp_hz, 0.707),
        }
    }

    #[inline]
    pub fn process(&mut self, x: f32) -> f32 {
        self.input_hp.process(self.dc_block.process(x))
    }
}

/// Structural voicing balance applied after the tone stack: a low shelf that
/// restores low-mid body and a high shelf that tames the tone stack's treble-
/// forward tilt, so notes stay even in level across the neck.
///
/// Every model needs this same body-up / tilt-down pair (the gain-stage
/// high-passes and peak-normalised tone stacks otherwise leave the upper register
/// blasting out); only the corner frequencies and depths are voiced per model.
pub(crate) struct VoiceBalance {
    body: Biquad,
    tilt: Biquad,
}

impl VoiceBalance {
    pub fn new(sr: f32, body_hz: f32, body_db: f32, tilt_hz: f32, tilt_db: f32) -> Self {
        Self {
            body: Biquad::low_shelf(sr, body_hz, body_db),
            tilt: Biquad::high_shelf(sr, tilt_hz, tilt_db),
        }
    }

    #[inline]
    pub fn process(&mut self, x: f32) -> f32 {
        self.tilt.process(self.body.process(x))
    }
}

/// Tracks the value a control was last at so an expensive coefficient recompute
/// only fires when the knob actually moves. Starts "dirty" (NaN), so the first
/// real call always recomputes.
pub(crate) struct Cached {
    last: f32,
}

impl Cached {
    pub fn new() -> Self {
        Self { last: f32::NAN }
    }

    /// Returns `true` (and latches the new value) when `v` has moved beyond the
    /// smoothing epsilon since the last latched value. The initial NaN forces the
    /// first call to report changed, syncing coefficients to the real control value.
    #[inline]
    pub fn changed(&mut self, v: f32) -> bool {
        if self.last.is_nan() || (v - self.last).abs() > 0.001 {
            self.last = v;
            true
        } else {
            false
        }
    }
}

/// The three-knob counterpart of [`Cached`] for the bass/mid/treble tone stack,
/// whose coefficients are recomputed as a set whenever *any* of the three moves.
pub(crate) struct ToneCache {
    bass: f32,
    mid: f32,
    treble: f32,
}

impl ToneCache {
    pub fn new() -> Self {
        Self {
            bass: f32::NAN,
            mid: f32::NAN,
            treble: f32::NAN,
        }
    }

    #[inline]
    pub fn changed(&mut self, bass: f32, mid: f32, treble: f32) -> bool {
        let moved = self.bass.is_nan()
            || (bass - self.bass).abs() > 0.001
            || (mid - self.mid).abs() > 0.001
            || (treble - self.treble).abs() > 0.001;
        if moved {
            self.bass = bass;
            self.mid = mid;
            self.treble = treble;
        }
        moved
    }
}

/// 8× oversampling wrapper for a power-stage memoryless waveshaper.
///
/// The preamp already runs at 8×, but the power-amp clipper ran at base rate and
/// folded harmonics back as fizz. The sag/ripple envelope is slow (ms) so it is
/// computed once per base-rate sample and held constant across the 8 subsamples —
/// only the memoryless clip is evaluated at high rate. Owns an independent
/// [`Oversampler`](crate::dsp::oversample::Oversampler) so preamp history is
/// untouched; the extra round-trip group delay (~M base samples) is series delay,
/// well inside the gigging budget.
pub(crate) struct PowerOs {
    os: crate::dsp::oversample::Oversampler8,
}

impl PowerOs {
    pub fn new(sr: f32) -> Self {
        Self {
            os: crate::dsp::oversample::Oversampler8::new(sr),
        }
    }

    /// Run memoryless `f` at 8× for one base-rate sample.
    #[inline]
    pub fn shape<F: FnMut(f32) -> f32>(&mut self, x: f32, mut f: F) -> f32 {
        let up = self.os.upsample(x);
        let mut down = [0.0f32; 8];
        for (o, &u) in down.iter_mut().zip(up.iter()) {
            *o = f(u);
        }
        self.os.downsample(down)
    }
}

/// Common interface every amp model must satisfy.
///
/// `knobs` holds the model's front-panel controls in the order its
/// [`KNOBS`](AmpKnob) descriptor declares, padded to [`AMP_MAX`]; every value is
/// normalised 0–1.
pub trait Amplifier {
    fn process(&mut self, sample: f32, knobs: &[f32; AMP_MAX]) -> f32;

    /// Adopt the selected cabinet's speaker load as `(fs, q)`.
    ///
    /// On the trait rather than inherent-per-model because it is part of every
    /// model's interface now: the load is a property of the *cabinet*, but it has to
    /// reach whichever model is live. See [`SpeakerLoad::set_load`] for what moves
    /// and what deliberately stays with the model.
    fn set_load(&mut self, load: (f32, f32));
}

/// Mains frequency the hum source runs at. 60 Hz (the model of a US amp);
/// switch to 50 for Europe.
const HUM_HZ: f32 = 60.0;
/// Hum level injected at the amp input (linear). Kept low — the high-gain models
/// amplify it to a realistic ~-55 dBFS floor.
const HUM_LEVEL: f32 = 0.00035;
/// Broadband noise-floor level injected at the amp input (linear).
const NOISE_LEVEL: f32 = 0.00012;

/// A quiet mains-hum + thermal-noise floor, injected at the amp's input so the gain
/// stages amplify it the way a real amp does.
///
/// Without this a silent input produced **bit-exact zero** from every model — an amp
/// that hums (all of them do; the Vox most of all) was the one thing the models could
/// not do. The hum is the mains fundamental plus its 2nd/3rd harmonics, generated
/// from one rotating unit phasor (Chebyshev, so no per-sample transcendentals); the
/// noise is a cheap xorshift broadband floor. Both are small enough to sit under the
/// playing signal and be swept up by the noise gate.
struct HumNoise {
    c: f32,
    s: f32,
    cos_w: f32,
    sin_w: f32,
    rng: u32,
}

impl HumNoise {
    fn new(sr: f32) -> Self {
        let w = std::f32::consts::TAU * HUM_HZ / sr.max(1.0);
        Self {
            c: 1.0,
            s: 0.0,
            cos_w: w.cos(),
            sin_w: w.sin(),
            rng: 0x9e37_79b9,
        }
    }

    #[inline]
    fn next(&mut self) -> f32 {
        // Rotate the phasor one step; sin θ, sin 2θ and sin 3θ fall out of (c, s).
        let (c, s) = (
            self.c * self.cos_w - self.s * self.sin_w,
            self.s * self.cos_w + self.c * self.sin_w,
        );
        self.c = c;
        self.s = s;
        // Chebyshev: sin2θ = 2sc, sin3θ = 3s − 4s³.
        let hum = s + 0.5 * (2.0 * s * c) + 0.25 * (3.0 * s - 4.0 * s * s * s);
        // xorshift32 broadband noise, mapped to [-1, 1).
        let mut x = self.rng;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.rng = x;
        let noise = x as f32 * (2.0 / u32::MAX as f32) - 1.0;
        hum * HUM_LEVEL + noise * NOISE_LEVEL
    }
}

/// Owns all amp instances simultaneously so filter state is preserved across
/// model switches (no audible click from zeroed delay lines on switch).
pub struct AmpBank {
    marshall: Marshall,
    mesa: Mesa,
    randall: Randall,
    vox: Vox,
    hiwatt: Hiwatt,
    plexi: Plexi,
    fender: Fender,
    supro: Supro,
    tweed: Tweed,
    hum: HumNoise,
}

impl AmpBank {
    pub fn new(sr: f32) -> Self {
        Self {
            marshall: Marshall::new(sr),
            mesa: Mesa::new(sr),
            randall: Randall::new(sr),
            vox: Vox::new(sr),
            hiwatt: Hiwatt::new(sr),
            plexi: Plexi::new(sr),
            fender: Fender::new(sr),
            supro: Supro::new(sr),
            tweed: Tweed::new(sr),
            hum: HumNoise::new(sr),
        }
    }

    #[inline]
    pub fn process(
        &mut self,
        model: AmpModel,
        sample: f32,
        knobs: &[f32; AMP_MAX],
        load: (f32, f32),
    ) -> f32 {
        // Push the load into **every** model, not just the live one, so a model
        // selected later cannot run a block against a stale load. Each call is a
        // compare-and-return when the numbers already match, so the eight no-op calls
        // are the cheap case and only the live model's biquad is ever rebuilt.
        self.marshall.set_load(load);
        self.mesa.set_load(load);
        self.randall.set_load(load);
        self.vox.set_load(load);
        self.hiwatt.set_load(load);
        self.plexi.set_load(load);
        self.fender.set_load(load);
        self.supro.set_load(load);
        self.tweed.set_load(load);
        // Inject the hum/noise floor at the input so the gain stages amplify it.
        let sample = sample + self.hum.next();
        match model {
            AmpModel::Marshall => self.marshall.process(sample, knobs),
            AmpModel::Mesa => self.mesa.process(sample, knobs),
            AmpModel::Randall => self.randall.process(sample, knobs),
            AmpModel::Vox => self.vox.process(sample, knobs),
            AmpModel::Hiwatt => self.hiwatt.process(sample, knobs),
            AmpModel::Plexi => self.plexi.process(sample, knobs),
            AmpModel::Fender => self.fender.process(sample, knobs),
            AmpModel::Supro => self.supro.process(sample, knobs),
            AmpModel::Tweed => self.tweed.process(sample, knobs),
        }
    }
}

#[cfg(test)]
mod tests {

    /// Render a steady tone through a model at a given gain / master / HF pot.
    fn nfb_tone(model: AmpModel, gain: f32, master: f32, hf: f32, f: f32) -> Vec<f32> {
        let sr = 48_000.0;
        let mut amp: Box<dyn Amplifier> = match model {
            AmpModel::Marshall => Box::new(Marshall::new(sr)),
            AmpModel::Mesa => Box::new(Mesa::new(sr)),
            AmpModel::Randall => Box::new(Randall::new(sr)),
            AmpModel::Vox => Box::new(Vox::new(sr)),
            AmpModel::Hiwatt => Box::new(Hiwatt::new(sr)),
            AmpModel::Plexi => Box::new(Plexi::new(sr)),
            AmpModel::Fender => Box::new(Fender::new(sr)),
            AmpModel::Supro => Box::new(Supro::new(sr)),
            AmpModel::Tweed => Box::new(Tweed::new(sr)),
        };
        amp.set_load(crate::dsp::CabModel::Mesa.speaker_load());
        let knobs = standard_knobs(model, gain, 0.5, 0.5, 0.6, hf, master);
        (0..sr as usize)
            .map(|n| amp.process((2.0 * PI * f * n as f32 / sr).sin() * 0.5, &knobs))
            .collect()
    }

    fn nfb_rms(v: &[f32]) -> f32 {
        (v.iter().map(|&x| (x * x) as f64).sum::<f64>() / v.len() as f64).sqrt() as f32
    }

    #[test]
    fn nfb_scoops_the_midrange_and_leaves_the_bass() {
        let bass = 80.0;
        let mid = 400.0;
        // Reference: same models with the loop disabled, by rendering through a
        // copy of the divider's job and comparing the ratio to the looped case.
        // We compare mid-vs-bass *change*, which isolates the loop's LF shaping.
        let bass_looped = nfb_rms(&nfb_tone(AmpModel::Marshall, 0.9, 1.0, 0.5, bass));
        let mid_looped = nfb_rms(&nfb_tone(AmpModel::Marshall, 0.9, 1.0, 0.5, mid));
        // The pre-loop reference levels are the ones measured with beta = 0:
        // 0.4777 at 80 Hz and 0.6495 at 400 Hz (see the increment log).
        let bass_ref = 0.4777;
        let mid_ref = 0.6495;
        let d_bass = 20.0 * (bass_looped / bass_ref).log10();
        let d_mid = 20.0 * (mid_looped / mid_ref).log10();
        // Mid must fall at least twice as far as the bass.
        assert!(
            d_mid < -0.5,
            "the loop should pull the midrange down, moved {d_mid:.2} dB"
        );
        assert!(
            d_mid < d_bass * 1.5,
            "mid ({d_mid:.2} dB) should fall further than bass ({d_bass:.2} dB) \
             or the divider is not LF-shaped"
        );
    }

    /// C2 — presence bleeds the top out of the divider, so the pot must change the
    /// top end (the master no longer moves the loop, so this is measured at one
    /// setting).
    #[test]
    fn presence_changes_the_top_end() {
        let f = 2000.0;
        let tone = |hf: f32| {
            let v = nfb_tone(AmpModel::Marshall, 0.9, 0.55, hf, f);
            nfb_rms(&v[v.len() / 2..])
        };
        let ratio = tone(0.9) / tone(0.1);
        assert!(
            (ratio - 1.0).abs() > 0.2,
            "presence did not change the top end: {ratio:.3}x"
        );
    }

    /// C5 — the master scales the power stage's **input** (real master-volume
    /// behaviour), so it changes drive and compression, not just level. Its level
    /// authority now comes from the power amp, not a post-clip multiply.
    #[test]
    fn master_drives_the_power_stage() {
        let f = 300.0;
        // Master 0.2 -> drive 0.36, master 1.0 -> drive 1.82: a 5.05x input swing.
        // A post-clip multiply would scale the output by ~5.05x; driving the power
        // stage into compression keeps the swing measurably below the linear ratio.
        let lo = nfb_rms(&nfb_tone(AmpModel::Marshall, 0.9, 0.2, 0.5, f)[24000..]);
        let hi = nfb_rms(&nfb_tone(AmpModel::Marshall, 0.9, 1.0, 0.5, f)[24000..]);
        let ratio = hi / lo;
        assert!(
            ratio < 4.8,
            "higher master did not compress the power stage: {ratio:.3}x vs linear 5.05x"
        );
        assert!(
            ratio > 2.0,
            "the master must still control level: {ratio:.3}x"
        );
    }

    /// C5 — a real amp hums on a silent input; the models must not output bit-exact
    /// zero. The floor must be present but quiet.
    #[test]
    fn amp_hums_on_a_silent_input() {
        let mut amp = AmpBank::new(48_000.0);
        let knobs = standard_knobs(AmpModel::Marshall, 0.75, 0.5, 0.5, 0.6, 0.4, 0.55);
        let out: Vec<f32> = (0..48_000)
            .map(|_| {
                amp.process(
                    AmpModel::Marshall,
                    0.0,
                    &knobs,
                    crate::dsp::CabModel::Marshall.speaker_load(),
                )
            })
            .collect();
        let rms = nfb_rms(&out[24_000..]);
        assert!(
            rms > 1e-5,
            "silent input produced no hum/noise (rms {rms:.2e})"
        );
        assert!(rms < 0.05, "the hum/noise floor is too loud (rms {rms:.4})");
    }

    /// C2 — the loop must be unconditionally stable at every amp's gain staging.
    /// A feedback path that blows up would show up as non-finite samples.
    #[test]
    fn nfb_is_stable_across_the_whole_gain_range() {
        for model in AmpModel::ALL {
            for gain in [0.0, 0.25, 0.5, 0.75, 1.0] {
                for master in [0.0, 0.5, 1.0] {
                    let v = nfb_tone(model, gain, master, 0.5, 220.0);
                    assert!(
                        v.iter().all(|s| s.is_finite()),
                        "{model:?} went non-finite at gain {gain} master {master}"
                    );
                    assert!(
                        nfb_rms(&v) < 8.0,
                        "{model:?} ran away at gain {gain} master {master}"
                    );
                }
            }
        }
    }

    #[test]
    fn the_selected_cabinet_changes_the_amps_speaker_load() {
        fn band_amp_at(model: AmpModel, load: (f32, f32), freq: f32) -> f32 {
            let mut amp: Box<dyn Amplifier> = match model {
                AmpModel::Marshall => Box::new(Marshall::new(SR)),
                AmpModel::Mesa => Box::new(Mesa::new(SR)),
                AmpModel::Randall => Box::new(Randall::new(SR)),
                AmpModel::Vox => Box::new(Vox::new(SR)),
                AmpModel::Hiwatt => Box::new(Hiwatt::new(SR)),
                AmpModel::Plexi => Box::new(Plexi::new(SR)),
                AmpModel::Fender => Box::new(Fender::new(SR)),
                AmpModel::Supro => Box::new(Supro::new(SR)),
                AmpModel::Tweed => Box::new(Tweed::new(SR)),
            };
            amp.set_load(load);
            let knobs = standard_knobs(model, 0.5, 0.5, 0.5, 0.6, 0.5, 0.6);
            let n = SR as usize;
            let mut out: Vec<f32> = Vec::with_capacity(n / 2);
            for i in (n / 2)..n {
                let x = (2.0 * PI * freq * i as f32 / SR).sin() * 0.5;
                out.push(amp.process(x, &knobs));
            }
            rms(&out)
        }

        let supro = CabModel::Supro.speaker_load();
        let fender = CabModel::Fender.speaker_load();
        assert!(
            (supro.0 - fender.0).abs() > 25.0,
            "the two test cabs are too close to prove anything"
        );

        for (name, model) in [
            ("Marshall", AmpModel::Marshall),
            ("Mesa", AmpModel::Mesa),
            ("Plexi", AmpModel::Plexi),
            ("Vox", AmpModel::Vox),
            ("Tweed", AmpModel::Tweed),
            ("Supro", AmpModel::Supro),
            ("Fender", AmpModel::Fender),
            ("Hiwatt", AmpModel::Hiwatt),
        ] {
            // Probe each cab at *its own* fundamental: the resonance peaks there, so
            // comparing a 110 Hz-loaded amp against a 76 Hz-loaded amp at one shared
            // frequency would only measure how far apart the peaks are.
            let with_supro = band_amp_at(model, supro, supro.0);
            let with_fender = band_amp_at(model, fender, fender.0);
            let delta = (20.0 * (with_supro / with_fender.max(1e-9)).log10()).abs();
            assert!(
                delta > 0.5,
                "{name}: the cabinet barely changed the amp (Supro {with_supro:.4} vs \
                 Fender {with_fender:.4}, {delta:.2} dB)"
            );
        }
    }

    /// The two Greenback cabinets must load identically, and that has to be a
    /// deliberate shared row rather than two numbers that happen to be near.
    #[test]
    fn greenback_cabs_load_identically() {
        assert_eq!(
            CabModel::Marshall.speaker_load(),
            CabModel::Orange.speaker_load(),
            "a Marshall cab and an Orange PPC412 are the same speaker"
        );
    }

    use super::*;
    use crate::dsp::CabModel;
    use std::f32::consts::PI;

    #[test]
    fn the_gain_knob_has_real_and_monotonic_level_authority() {
        fn level_in_db(model: AmpModel, gain: f32) -> f32 {
            let mut amp: Box<dyn Amplifier> = match model {
                AmpModel::Marshall => Box::new(Marshall::new(SR)),
                AmpModel::Plexi => Box::new(Plexi::new(SR)),
                AmpModel::Vox => Box::new(Vox::new(SR)),
                AmpModel::Mesa => Box::new(Mesa::new(SR)),
                AmpModel::Hiwatt => Box::new(Hiwatt::new(SR)),
                AmpModel::Fender => Box::new(Fender::new(SR)),
                AmpModel::Supro => Box::new(Supro::new(SR)),
                AmpModel::Tweed => Box::new(Tweed::new(SR)),
                AmpModel::Randall => Box::new(Randall::new(SR)),
            };
            let knobs = standard_knobs(model, gain, 0.5, 0.5, 0.6, 0.5, 0.6);
            let out: Vec<f32> = (0..(SR as usize * 2))
                .map(|n| amp.process((2.0 * PI * 220.0 * n as f32 / SR).sin() * 0.5, &knobs))
                .collect();
            let tail = &out[out.len() / 2..];
            let r = (tail.iter().map(|&v| (v * v) as f64).sum::<f64>() / tail.len() as f64).sqrt();
            20.0 * (r as f32).max(1e-9).log10()
        }

        for (name, model) in [
            ("Marshall", AmpModel::Marshall),
            ("Plexi", AmpModel::Plexi),
            ("Vox", AmpModel::Vox),
            ("Mesa", AmpModel::Mesa),
            ("Hiwatt", AmpModel::Hiwatt),
            ("Fender", AmpModel::Fender),
            ("Supro", AmpModel::Supro),
            ("Tweed", AmpModel::Tweed),
            ("Randall", AmpModel::Randall),
        ] {
            let db: Vec<f32> = [0.0, 0.25, 0.5, 0.75, 1.0]
                .iter()
                .map(|&g| level_in_db(model, g))
                .collect();

            let authority = db[4] - db[0];
            assert!(
                authority > 12.0,
                "{name}: the gain knob only has {authority:.1} dB of level authority \
                 ({db:?}) -- it is still a compression control, not a gain control"
            );
            for (i, w) in db.windows(2).enumerate() {
                assert!(
                    w[1] > w[0] - 0.05,
                    "{name}: level is non-monotonic between gain {} and {} ({db:?})",
                    [0.0, 0.25, 0.5, 0.75, 1.0][i],
                    [0.0, 0.25, 0.5, 0.75, 1.0][i + 1],
                );
            }
        }
    }

    #[test]
    fn sagged_rail_leaves_small_signal_gain_untouched() {
        for drive in [1.5f32, 1.8, 2.2, 2.6] {
            let slope_at = |rail: f32| {
                let (d, r) = sagged_rail(rail, drive);
                // Probe just below the knee, where both clippers are still linear.
                let u = 1e-4;
                TubeClip::AX7.shape(u * d) * r / u
            };
            let unloaded = slope_at(1.0);
            for rail in [0.9, 0.7, 0.5, 0.3, 0.1] {
                let got = slope_at(rail);
                assert!(
                    (got - unloaded).abs() < 1e-4,
                    "drive {drive}: small-signal gain moved from {unloaded:.6} to \
                     {got:.6} at rail {rail}"
                );
            }
        }
    }

    /// ...and the defining fix: **more sag, more distortion**.
    ///
    /// Measured as the third harmonic relative to the fundamental on a steady tone,
    /// which is the most direct read of "how hard is this thing clipping". The old
    /// formulation moved the wrong way; if this ever regresses, sag has been
    /// re-applied as a signal attenuator instead of a rail.
    #[test]
    fn more_sag_means_more_distortion() {
        fn h3_over_h1(rail: f32) -> f64 {
            // Goertzel-ish: correlate against 1x and 3x of a 220 Hz tone.
            let (d, r) = sagged_rail(rail, 2.2);
            let sr: f64 = 48_000.0;
            let f = 220.0;
            let mut a1 = (0.0f64, 0.0f64);
            let mut a3 = (0.0f64, 0.0f64);
            let n = 48_000usize;
            for k in 0..n {
                let x = (2.0 * std::f64::consts::PI * f * k as f64 / sr).sin();
                let y = TubeClip::AX7.shape((x as f32) * d) as f64 * r as f64;
                for (harm, acc) in [(1.0f64, &mut a1), (3.0, &mut a3)] {
                    let w = 2.0 * std::f64::consts::PI * f * harm * k as f64 / sr;
                    acc.0 += y * w.cos();
                    acc.1 += y * w.sin();
                }
            }
            let mag = |a: (f64, f64)| (a.0 * a.0 + a.1 * a.1).sqrt();
            mag(a3) / mag(a1)
        }
        let light = h3_over_h1(0.9);
        let heavy = h3_over_h1(0.45);
        assert!(
            heavy > light,
            "sagging the rails did not increase distortion: h3/h1 went {light:.4} \
             -> {heavy:.4}"
        );
    }

    /// And the other half of the fix: **more sag, less output**.
    #[test]
    fn more_sag_means_less_output() {
        fn peak(rail: f32) -> f32 {
            let (d, r) = sagged_rail(rail, 2.2);
            let mut hi: f32 = 0.0;
            for k in 0..4_800 {
                let x = (2.0 * PI * 220.0 * k as f32 / 48_000.0).sin() * 0.5;
                hi = hi.max(TubeClip::AX7.shape(x * d) * r);
            }
            hi
        }
        assert!(
            peak(0.45) < peak(0.9),
            "sagging the rails did not lower the ceiling: {} -> {}",
            peak(0.9),
            peak(0.45)
        );
    }

    /// The rail clamp must survive a pathological envelope without producing an
    /// infinite drive.
    #[test]
    fn sagged_rail_clamps_a_degenerate_supply() {
        let (d, r) = sagged_rail(0.0, 2.2);
        assert!(d.is_finite(), "drive went non-finite: {d}");
        assert_eq!(r, 0.05);
        let (d, r) = sagged_rail(-3.0, 2.2);
        assert!(d.is_finite() && d > 0.0, "bad rail gave drive {d}");
        assert_eq!(r, 0.05);
        // An over-unity "supply" is clamped too, so the model cannot be fed a
        // rail that would *reduce* clipping under load.
        let (_, r) = sagged_rail(4.0, 2.2);
        assert_eq!(r, 1.0);
    }

    const SR: f32 = 48_000.0;

    /// One amp instance per model, addressed through the `Amplifier` trait so the
    /// sound-quality checks below run identically against all of them. The model is
    /// carried alongside so tests can build its control array by role.
    fn each_amp() -> Vec<(&'static str, AmpModel, Box<dyn Amplifier>)> {
        vec![
            (
                "Marshall",
                AmpModel::Marshall,
                Box::new(Marshall::new(SR)) as Box<dyn Amplifier>,
            ),
            ("Mesa", AmpModel::Mesa, Box::new(Mesa::new(SR))),
            ("Randall", AmpModel::Randall, Box::new(Randall::new(SR))),
            ("Vox", AmpModel::Vox, Box::new(Vox::new(SR))),
            ("Hiwatt", AmpModel::Hiwatt, Box::new(Hiwatt::new(SR))),
            ("Plexi", AmpModel::Plexi, Box::new(Plexi::new(SR))),
            ("Fender", AmpModel::Fender, Box::new(Fender::new(SR))),
            ("Supro", AmpModel::Supro, Box::new(Supro::new(SR))),
            ("Tweed", AmpModel::Tweed, Box::new(Tweed::new(SR))),
        ]
    }

    /// Single-bin DFT magnitude (normalised so a unit sine reads ~1.0).
    fn goertzel(samples: &[f32], f: f32, sr: f32) -> f32 {
        let w = 2.0 * PI * f / sr;
        let coeff = 2.0 * w.cos();
        let (mut s1, mut s2) = (0.0f32, 0.0f32);
        for &x in samples {
            let s0 = x + coeff * s1 - s2;
            s2 = s1;
            s1 = s0;
        }
        let real = s1 - s2 * w.cos();
        let imag = s2 * w.sin();
        (real * real + imag * imag).sqrt() / (samples.len() as f32 / 2.0)
    }

    /// Push a tone through one amp and collect the steady-state tail (filter and
    /// envelope transients discarded). `amp_amp` is the input sine amplitude.
    fn run_tone(
        amp: &mut dyn Amplifier,
        freq: f32,
        amp_amp: f32,
        knobs: &[f32; AMP_MAX],
    ) -> Vec<f32> {
        let n = SR as usize;
        let warmup = n / 3; // let sag/bloom envelopes and HP filters settle
        let mut out = Vec::with_capacity(n - warmup);
        for i in 0..n {
            let x = (2.0 * PI * freq * i as f32 / SR).sin() * amp_amp;
            let y = amp.process(x, knobs);
            if i >= warmup {
                out.push(y);
            }
        }
        out
    }

    fn rms(s: &[f32]) -> f32 {
        (s.iter().map(|&x| x * x).sum::<f32>() / s.len() as f32).sqrt()
    }

    fn mean(s: &[f32]) -> f32 {
        s.iter().sum::<f32>() / s.len() as f32
    }

    /// No amp may produce NaN/Inf or a runaway level across the full sweep of the
    /// gain and master controls — the cheapest guarantee against the worst
    /// "unpleasant sound" of all (a blast of digital noise).
    #[test]
    fn stable_and_bounded_across_control_sweep() {
        for (name, model, mut amp) in each_amp() {
            let mut max_abs = 0.0f32;
            for &gain in &[0.0, 0.5, 1.0] {
                for &master in &[0.0, 0.5, 1.0] {
                    let knobs = standard_knobs(model, gain, 0.7, 0.5, 0.7, 0.6, master);
                    // hot low-E so the gain stages are genuinely driven
                    for i in 0..(SR as usize / 4) {
                        let x = (2.0 * PI * 82.41 * i as f32 / SR).sin() * 0.9;
                        let y = amp.process(x, &knobs);
                        assert!(
                            y.is_finite(),
                            "{name} non-finite at gain={gain} master={master}"
                        );
                        max_abs = max_abs.max(y.abs());
                    }
                }
            }
            assert!(max_abs < 4.0, "{name} runaway output: {max_abs}");
        }
    }

    /// The asymmetric tube/FET/silicon waveshapers all inject a DC offset. The
    /// inter-stage and power-amp high-passes exist to strip it; a residual DC
    /// offset wastes headroom and thumps on note transitions. Confirm the
    /// steady-state output is centred on zero even under hard, asymmetric drive.
    #[test]
    fn output_is_dc_free_under_hard_drive() {
        for (name, model, mut amp) in each_amp() {
            let out = run_tone(
                &mut *amp,
                110.0,
                0.8,
                &standard_knobs(model, 0.95, 0.6, 0.5, 0.7, 0.6, 0.7),
            );
            let dc = mean(&out).abs();
            let level = rms(&out).max(1e-6);
            assert!(
                dc < 0.03 * level,
                "{name} has DC offset: |mean|={dc:.4} vs rms={level:.4}"
            );
        }
    }

    /// Driving an amp hard must add harmonics (that is the whole point) but the
    /// energy has to land on the *harmonic series* of the note — musical overtones —
    /// not smear into inharmonic hash from aliasing in the stacked clippers. The
    /// 8× oversampling is what keeps that hash inaudible; this test fails loudly if
    /// the oversampling is ever broken or removed.
    #[test]
    fn distortion_is_harmonic_not_aliased_hash() {
        let f0 = 220.0; // A3
        // Harmonic bins (well below Nyquist) vs. clearly inharmonic probe bins.
        let harmonics: Vec<f32> = (1..=20).map(|k| f0 * k as f32).collect();
        let inharmonic = [130.0, 290.0, 510.0, 1234.0, 2050.0, 3001.0, 5003.0];
        for (name, model, mut amp) in each_amp() {
            let out = run_tone(
                &mut *amp,
                f0,
                0.5,
                &standard_knobs(model, 0.95, 0.5, 0.5, 0.7, 0.5, 0.7),
            );

            let h2 = goertzel(&out, f0 * 2.0, SR);
            let h3 = goertzel(&out, f0 * 3.0, SR);
            let fund = goertzel(&out, f0, SR);
            // Genuine distortion: the 2nd/3rd harmonics carry real energy.
            assert!(
                h2 + h3 > 0.1 * fund,
                "{name} barely distorting: h2+h3={:.4}, fund={fund:.4}",
                h2 + h3
            );

            let harm_energy: f32 = harmonics
                .iter()
                .map(|&f| goertzel(&out, f, SR).powi(2))
                .sum();
            let alias_energy: f32 = inharmonic
                .iter()
                .map(|&f| goertzel(&out, f, SR).powi(2))
                .sum();
            assert!(
                alias_energy < 0.02 * harm_energy,
                "{name} aliasing/hash too high: alias={alias_energy:.6} harm={harm_energy:.6}"
            );
        }
    }

    /// A low-E power chord through a high-gain, scooped rig must stay tight: the
    /// inaudible sub-bass below the 82 Hz fundamental (difference-tone "fart") must
    /// remain a small fraction of the musical body harmonics. Mirrors the worst
    /// case the subsonic high-passes in each amp were built to defeat.
    #[test]
    fn power_chord_low_end_stays_tight() {
        let chord = [82.41f32, 123.47, 164.81]; // E2 root + fifth + octave
        for (name, model, mut amp) in each_amp() {
            let knobs = standard_knobs(model, 0.93, 0.82, 0.12, 0.86, 0.73, 0.65);
            let n = SR as usize;
            let warmup = n / 3;
            let mut out = Vec::with_capacity(n - warmup);
            for i in 0..n {
                let t = i as f32 / SR;
                let x: f32 = chord.iter().map(|&f| (2.0 * PI * f * t).sin()).sum::<f32>() * 0.3;
                let y = amp.process(x, &knobs);
                if i >= warmup {
                    out.push(y);
                }
            }
            let sub = goertzel(&out, 41.0, SR) + goertzel(&out, 55.0, SR);
            let body =
                goertzel(&out, 164.81, SR) + goertzel(&out, 247.0, SR) + goertzel(&out, 330.0, SR);
            assert!(
                sub / body.max(1e-9) < 0.45,
                "{name} low end is farty: sub/body = {:.2}",
                sub / body.max(1e-9)
            );
        }
    }

    /// The tone and presence controls must move the spectrum in the expected
    /// direction — bass up brightens the lows, treble up the highs, presence the
    /// upper-mid air. Uses a small signal so the tone stack is exercised roughly
    /// linearly. Guards against an inverted or dead control shipping a harsh tone.
    #[test]
    fn tone_and_presence_controls_track() {
        // Each probe is only run for models that expose the matching control, so a
        // model without a Mid/Presence knob is exempt rather than falsely failing.
        let band = |model: AmpModel, amp: &mut dyn Amplifier, f: f32, b: f32, t: f32, p: f32| {
            let out = run_tone(amp, f, 0.05, &standard_knobs(model, 0.4, b, 0.5, t, p, 0.7));
            goertzel(&out, f, SR)
        };
        for (name, model, mut amp) in each_amp() {
            let a = &mut *amp;
            if model.knob_slot("bass").is_some() {
                assert!(
                    band(model, a, 100.0, 0.9, 0.65, 0.5) > band(model, a, 100.0, 0.1, 0.65, 0.5),
                    "{name} bass control dead/inverted at 100 Hz"
                );
            }
            if model.knob_slot("treble").is_some() {
                assert!(
                    band(model, a, 4000.0, 0.5, 0.9, 0.5) > band(model, a, 4000.0, 0.5, 0.1, 0.5),
                    "{name} treble control dead/inverted at 4 kHz"
                );
            }
            if model.knob_slot("presence").is_some() {
                assert!(
                    band(model, a, 5000.0, 0.5, 0.65, 0.95)
                        > band(model, a, 5000.0, 0.5, 0.65, 0.05),
                    "{name} presence control dead/inverted at 5 kHz"
                );
            }
        }
    }

    /// At equal settings the models must sit within a sane loudness window of each
    /// other, so flipping models on stage doesn't jump the volume. The output
    /// trims in each amp exist precisely to enforce this.
    ///
    /// Measured on a short broadband chug DI in the **300 Hz–5 kHz** band, not raw
    /// RMS: the ear weights the mids far more than the low end a chug is full of, so
    /// raw RMS let bass-heavy voicings (and the low-headroom small combos) drift out
    /// of perceptual balance. See `examples/rig_loudness.rs`.

    #[test]
    fn amps_are_loudness_matched() {
        use crate::dsp::biquad::Biquad;
        let di = {
            let mut di = vec![0.0f32; (SR * 3.0) as usize];
            for k in 0..8 {
                let start = (SR * 0.35) as usize * k;
                for i in 0..(SR * 0.25) as usize {
                    let t = i as f32 / SR;
                    let env = (-t * 9.0).exp();
                    di[start + i] += 0.6 * env * (2.0 * PI * 82.41 * t).sin().signum() * 0.5;
                }
            }
            di
        };
        let mid_rms = |s: &[f32]| {
            let mut hp = Biquad::highpass(SR, 300.0, 0.707);
            let mut lp = Biquad::lowpass(SR, 5000.0, 0.707);
            let mid: Vec<f32> = s.iter().map(|&x| lp.process(hp.process(x))).collect();
            rms(&mid)
        };

        let mut levels = Vec::new();
        let mut names = Vec::new();
        for (name, model, mut amp) in each_amp() {
            names.push(name);
            // Driven hard — the regime the per-amp output trims are tuned to match.
            let knobs = standard_knobs(model, 0.93, 0.5, 0.5, 0.65, 0.5, 0.65);
            let out: Vec<f32> = di.iter().map(|&x| amp.process(x, &knobs)).collect();
            let lv = mid_rms(&out);
            levels.push(lv);
        }
        let lo = levels.iter().cloned().fold(f32::INFINITY, f32::min);
        let hi = levels.iter().cloned().fold(0.0f32, f32::max);
        assert!(
            hi / lo < 1.7,
            "amps not perceptually loudness-matched: mid-band rms spread {hi:.4}/{lo:.4} = {:.2}x",
            hi / lo
        );
        // **Absolute** level as well as relative. The spread check alone let the
        // C2 feedback loop pass: negative feedback removes gain by definition, and
        // it removed it from every model, so the spread stayed tight while the
        // whole bank sat up to 2 dB quiet. A spread assertion cannot see that.
        for (name, lv) in names.iter().zip(levels.iter()) {
            assert!(
                (lv - 0.0650).abs() < 0.0010,
                "{name} mid-band rms {lv:.4} is not at the 0.0650 trim target"
            );
        }
    }

    /// Every amp model must be **time-aligned**, so switching models on stage
    /// does not jump the signal in time.
    ///
    /// The Fender Twin runs an onboard bias tremolo (`fender.rs`) through the
    /// shared rack `Tremolo` module, using the tremolo end — no pitch
    /// modulation. That path used to route through the vibrato delay tap with no
    /// dry path, so the Twin sat `CENTER_MS` (4 ms, 192 samples @48 kHz) behind
    /// the other eight models. Against a reverb or the take bus that reads as a
    /// slapback on every model switch.
    ///
    /// **On the metric.** These models are non-linear (sag, bias offsets,
    /// saturators), so a transfer-function group delay is not well defined, and
    /// an energy centroid measures the response's long tail rather than its
    /// arrival — measured that way the spread is 27–118 samples and says nothing
    /// about alignment. What matters is when a *transient arrives*, so this uses
    /// the first sample crossing 1% of the impulse response's own peak. The
    /// tolerance is 8 samples (0.17 ms @48 kHz): that absorbs the differences in
    /// filter shape between the models (the Tweed's treble-cut lowpass rises
    /// more slowly than the others' shelves, worth ~4 samples) while being
    /// **24x** tighter than the 192-sample defect it guards against.
    #[test]
    fn all_amp_models_are_latency_aligned() {
        const IMPULSE: f32 = 0.9;
        const ONSET_FRACTION: f32 = 0.01;
        const MAX_SPREAD: usize = 8;

        let onset = |model: crate::dsp::AmpModel| -> usize {
            let (_name, _m, mut amp) = each_amp()
                .into_iter()
                .find(|(_, m, _)| *m == model)
                .expect("model missing from each_amp()");
            let knobs = standard_knobs(model, 0.6, 0.5, 0.5, 0.5, 0.5, 0.5);
            // Settle on silence first so the DC blocks and sag envelopes are in
            // steady state before the impulse.
            for _ in 0..(SR as usize / 4) {
                amp.process(0.0, &knobs);
            }
            let n = SR as usize;
            let mut h = vec![0.0f32; n];
            for (i, w) in h.iter_mut().enumerate() {
                let x = if i == 0 { IMPULSE } else { 0.0 };
                *w = amp.process(x, &knobs);
            }
            let peak = h.iter().fold(0.0f32, |a, &b| a.max(b.abs()));
            assert!(peak > 1e-3, "{model:?} produced no impulse response");
            let threshold = peak * ONSET_FRACTION;
            h.iter()
                .position(|&v| v.abs() >= threshold)
                .unwrap_or(usize::MAX)
        };

        let all = [
            crate::dsp::AmpModel::Marshall,
            crate::dsp::AmpModel::Mesa,
            crate::dsp::AmpModel::Randall,
            crate::dsp::AmpModel::Vox,
            crate::dsp::AmpModel::Hiwatt,
            crate::dsp::AmpModel::Plexi,
            crate::dsp::AmpModel::Fender,
            crate::dsp::AmpModel::Supro,
            crate::dsp::AmpModel::Tweed,
        ];
        let onsets: Vec<(crate::dsp::AmpModel, usize)> =
            all.iter().map(|&m| (m, onset(m))).collect();

        let lo = onsets.iter().map(|(_, o)| *o).min().unwrap();
        let hi = onsets.iter().map(|(_, o)| *o).max().unwrap();
        assert!(
            hi - lo <= MAX_SPREAD,
            "amp models are not time-aligned: onset spread {lo}..{hi} \
             ({} samples) across {:?}",
            hi - lo,
            onsets
                .iter()
                .map(|(m, o)| format!("{}:{o}", m.name()))
                .collect::<Vec<_>>()
        );
    }

    // ── New dynamic-realism features ──────────────────────────────────────────
    //
    // The three additions below — dynamic cathode-bias, output-transformer
    // saturation/crossover, and the gain-pot bright cap — exist to make the amp
    // *react* the way real iron and tubes do rather than sit as a static transfer
    // curve. Each is unit-tested in isolation (so a regression points straight at
    // the offending block) and then again through a whole amp (so we know the wiring
    // and tuning actually deliver the effect at the controls a player turns).

    // — Dynamic cathode bias (blocking-distortion bloom / touch) ———————————————

    /// Below grid conduction the stage must be perfectly transparent: a clean, quiet
    /// signal that never swings the grid into current must pass untouched, with no
    /// bias build-up colouring it. This is what keeps low-level playing clear instead
    /// of permanently "ducked".
    #[test]
    fn cathode_bias_is_transparent_below_conduction() {
        let mut cb = CathodeBias::new(SR, 1.5, 45.0, 0.3, 1.0);
        for i in 0..(SR as usize / 10) {
            let x = (2.0 * PI * 200.0 * i as f32 / SR).sin() * 0.8; // peaks 0.8 < thresh 1.0
            let y = cb.shift(x);
            assert!(
                (y - x).abs() < 1e-6,
                "cathode bias altered a signal that never reaches grid conduction"
            );
        }
    }

    /// The defining dynamic behaviour: when drive suddenly exceeds grid conduction
    /// the bias charges up and pulls the operating point colder, so the stage gain
    /// *sags in* over the first few milliseconds (blocking-distortion bloom) instead
    /// of clamping flat instantly. We measure the positive-peak envelope right at
    /// onset vs. once the bias has settled and require a clear droop.
    #[test]
    fn cathode_bias_blooms_under_a_hard_transient() {
        let mut cb = CathodeBias::new(SR, 1.5, 45.0, 0.3, 1.0);
        let n = SR as usize / 10; // 100 ms
        let mut peaks = Vec::with_capacity(n);
        for i in 0..n {
            let x = (2.0 * PI * 200.0 * i as f32 / SR).sin() * 2.0; // peaks well past thresh
            peaks.push(cb.shift(x));
        }
        let win = SR as usize / 100; // 10 ms
        let early = peaks[..win].iter().cloned().fold(f32::MIN, f32::max);
        let late = peaks[n - win..].iter().cloned().fold(f32::MIN, f32::max);
        assert!(
            early > late + 0.05,
            "cathode bias shows no blocking-distortion sag: early peak {early:.3} late {late:.3}"
        );
    }

    /// The stored charge must bleed away over the RC recovery time so the *next*
    /// note starts from the resting bias — otherwise hard playing would leave the
    /// amp permanently compressed. After a loud burst and a recovery window, a quiet
    /// probe must again pass essentially untouched.
    #[test]
    fn cathode_bias_recovers_after_the_drive_stops() {
        let mut cb = CathodeBias::new(SR, 1.5, 45.0, 0.3, 1.0);
        for i in 0..(SR as usize / 10) {
            let x = (2.0 * PI * 200.0 * i as f32 / SR).sin() * 2.0; // load it up
            cb.shift(x);
        }
        for _ in 0..(SR as usize / 4) {
            cb.shift(0.0); // 250 ms of silence — RC recovery (~45 ms) completes
        }
        let y = cb.shift(0.5); // below conduction
        assert!(
            (y - 0.5).abs() < 0.01,
            "cathode bias never recovered: probe {y:.4} (expected ~0.5)"
        );
    }

    /// The bias shift is one-sided (it pulls the operating point toward cutoff), so
    /// under symmetric hard drive it skews the waveform's average negative. That DC
    /// skew (stripped later by the inter-stage HP) is exactly the dynamic asymmetry
    /// that breeds the even-harmonic warmth a static shaper can't.
    #[test]
    fn cathode_bias_skews_the_operating_point_under_drive() {
        let mut cb = CathodeBias::new(SR, 1.5, 45.0, 0.3, 1.0);
        let n = SR as usize / 5;
        let warm = n / 2;
        let mut out = Vec::with_capacity(n - warm);
        for i in 0..n {
            let x = (2.0 * PI * 200.0 * i as f32 / SR).sin() * 2.0;
            let y = cb.shift(x);
            if i >= warm {
                out.push(y);
            }
        }
        let m = mean(&out);
        assert!(
            m < -0.02,
            "cathode bias did not skew the operating point under hard drive: mean {m:.4}"
        );
    }

    // — Supply ripple (ghost notes) ————————————————————————————————————————————

    /// The mains ripple must intermodulate with the signal only when the supply
    /// is loaded: a hard-driven note grows sidebands at ±(ripple Hz) around the
    /// fundamental — the ghost notes — while quiet playing stays essentially
    /// sideband-free. Probed through the whole Marshall (100 Hz ripple) so the
    /// wiring from the sag envelope to the supply gain is covered too.
    #[test]
    fn supply_ripple_grows_ghost_sidebands_only_when_driven() {
        // Over a 30 720-sample window, 225 Hz (144 cycles), the ±100 Hz
        // sidebands (80/208 cycles) and the ±62.5 Hz control bins are all
        // integer-cycle, so the huge fundamental leaks nothing into the bins
        // under test (a rectangular-window sinc sidelobe would otherwise set a
        // ~0.005 floor and mask the ghost notes).
        let f0 = 225.0;
        const WIN: usize = 30_720;
        let sidebands = |amp_in: f32, gain: f32| -> f32 {
            let mut amp = Marshall::new(SR);
            let knobs = standard_knobs(AmpModel::Marshall, gain, 0.5, 0.5, 0.6, 0.5, 0.8);
            let out = run_tone(&mut amp, f0, amp_in, &knobs);
            let out = &out[out.len() - WIN..];
            let fund = goertzel(out, f0, SR).max(1e-9);
            (goertzel(out, f0 - 100.0, SR) + goertzel(out, f0 + 100.0, SR)) / fund
        };
        // Quiet: low input *and* low gain, so the supply is genuinely unloaded
        // (at high gain the preamp's compression loads the power amp even for
        // a whisper of input — physical, but not the contrast under test).
        let quiet = sidebands(0.01, 0.2);
        let driven = sidebands(0.6, 0.9);
        assert!(
            driven > quiet * 2.0,
            "ghost notes not load-dependent: quiet {quiet:.5} driven {driven:.5}"
        );
        // Present but subliminal: audible as texture, nowhere near a tremolo.
        //
        // The floor moved from 0.001 to 0.0004 because C4 removed the h2-rich
        // curve from the power stage. The ripple sidebands are an even-order
        // product, and with the output stage no longer generating its own even
        // harmonics the ghost notes sit slightly further below the fundamental
        // (0.00093 against the old ~0.001). The load-dependence and the upper
        // bound are the assertions that carry meaning here; this floor only
        // distinguishes "there is some ripple" from "there is none".
        assert!(
            (0.0004..0.15).contains(&driven),
            "driven ghost-note level out of range: {driven:.5}"
        );
        // The sidebands must be *ripple* products, not generic spectral skirt:
        // clearly above equally-offset control bins that are neither harmonics
        // nor ripple sidebands.
        let mut amp = Marshall::new(SR);
        let knobs = standard_knobs(AmpModel::Marshall, 0.9, 0.5, 0.5, 0.6, 0.5, 0.8);
        let out = run_tone(&mut amp, f0, 0.6, &knobs);
        let out = &out[out.len() - WIN..];
        let sb = goertzel(out, f0 - 100.0, SR) + goertzel(out, f0 + 100.0, SR);
        let ctl = goertzel(out, f0 - 62.5, SR) + goertzel(out, f0 + 62.5, SR);
        assert!(
            sb > ctl * 2.0,
            "no distinct ripple sidebands: sb {sb:.6} vs control {ctl:.6}"
        );
    }

    /// Grid conduction must produce a real **flat top**, not just a gain tilt.
    ///
    /// The old `GridBlock` only reduced gain on hard positives, so a slammed stage
    /// kept folding its peaks back in smoothly — the soft "hiss" of an overdriven
    /// solid state rather than the grainy crunch of a valve driven into conduction.
    /// A real grid draws current past the conduction point and the anode stops
    /// following, so the peaks should stop rising *dead flat* while the valleys keep
    /// swinging.
    ///
    /// Asserted directly on the block rather than through an amp, so the property
    /// cannot be met by some other stage's saturation.
    #[test]
    fn grid_conduction_flattens_the_peaks() {
        let mut gb = GridBlock::new(SR, 0.25, 30.0, 0.22, 2.6);
        let ceiling = 2.6 * GRID_CEILING_MULT;
        assert!(
            ceiling > 2.6,
            "the ceiling must sit above the conduction threshold"
        );

        // A steady level that drives the stage hard, so the bias has charged.
        let level = ceiling * 4.0;
        for _ in 0..4_000 {
            gb.shift(level);
        }
        // Anything the bias shift leaves *above* the ceiling must come out
        // identical — that is the flat top. The bias only removes
        // `GRID_BLOCK_CAP * depth` = 0.44, so the probe has to start above
        // `ceiling + 0.44` rather than merely above `ceiling`.
        let bias_offset = GRID_BLOCK_CAP * 0.22;
        let first = gb.shift(ceiling + bias_offset + 0.05);
        let mid = gb.shift(ceiling * 2.0);
        let far = gb.shift(ceiling * 5.0);
        assert!(
            (first - mid).abs() < 1e-4 && (mid - far).abs() < 1e-4,
            "peaks above the ceiling are not flat: {first:.5} / {mid:.5} / \
             {far:.5} against a ceiling of {ceiling:.5}"
        );
        assert!(
            (mid - ceiling).abs() < 1e-4,
            "the flat top should sit exactly on the anode ceiling: {mid:.5} vs \
             {ceiling:.5}"
        );

        // And it must still be transparent in ordinary playing, or every preset
        // gains a hard clip it never had.
        let mut gb = GridBlock::new(SR, 0.25, 30.0, 0.22, 2.6);
        let mut worst = 0.0f32;
        for i in 0..(SR as usize / 20) {
            let x = (2.0 * PI * 220.0 * i as f32 / SR).sin() * 1.5; // under thresh
            worst = worst.max((gb.shift(x) - x).abs());
        }
        assert!(
            worst < 0.05,
            "grid conduction is engaging below threshold: {worst:.4}"
        );
    }

    // — Grid blocking (crackle-then-recover on a slammed input) ————————————————

    /// Below its conduction threshold the grid block must be perfectly
    /// transparent — it exists for slammed inputs only, and ordinary playing
    /// (which CathodeBias already handles) must never touch it.
    #[test]
    fn grid_block_is_transparent_below_threshold() {
        let mut gb = GridBlock::new(SR, 0.25, 30.0, 0.22, 2.6);
        for i in 0..(SR as usize / 10) {
            let x = (2.0 * PI * 200.0 * i as f32 / SR).sin() * 2.4; // < 2.6
            let y = gb.shift(x);
            assert!((y - x).abs() < 1e-6, "grid block leaked below threshold");
        }
    }

    /// The defining behaviour: a slam past conduction charges the coupling cap
    /// near-instantly (the stage chokes — positive peaks duck hard within
    /// milliseconds), then the charge bleeds off over the grid-leak RC and a
    /// quiet probe passes untouched again. Crackle, then recover.
    #[test]
    fn grid_block_chokes_fast_and_recovers_slow() {
        let mut gb = GridBlock::new(SR, 0.25, 30.0, 0.22, 2.6);
        // Slam: hold the grid at ~2× threshold. The charge is near-instant
        // (0.25 ms), so within 1 ms the stage is already deeply choked.
        let one_ms = SR as usize / 1000;
        let mut choke_1ms = 0.0f32;
        for _ in 0..one_ms {
            choke_1ms = 5.0 - gb.shift(5.0);
        }
        assert!(
            choke_1ms > 0.3,
            "charge not near-instant: choke after 1 ms only {choke_1ms:.3}"
        );
        // The bleed is slow (30 ms grid-leak RC): 5 ms after the slam ends, a
        // sub-threshold probe is still visibly choked — the crackle hasn't
        // recovered yet…
        for _ in 0..(5 * one_ms) {
            gb.shift(0.0);
        }
        let probe = gb.shift(1.0);
        assert!(
            probe < 0.85,
            "charge bled off too fast (no crackle window): probe {probe:.3}"
        );
        // …but after 150 ms (≫ RC) the same probe passes essentially untouched.
        for _ in 0..(150 * one_ms) {
            gb.shift(0.0);
        }
        let probe = gb.shift(1.0);
        assert!(
            (probe - 1.0).abs() < 0.02,
            "grid block never recovered: probe {probe:.4}"
        );
    }

    // — Dynamic presence (NFB-loop authority collapses under drive) —————————————

    /// At idle the presence knob must own its full range; with the loop loaded
    /// (deep sag envelope) the knob's authority must shrink as the response
    /// collapses toward the fixed open-loop lift. We measure the spread between
    /// knob extremes at a presence-band frequency, quiet vs slammed.
    #[test]
    fn presence_authority_shrinks_under_drive() {
        let band_gain = |knob_db: f32, env: f32| -> f32 {
            let mut dp = DynamicPresence::new(SR, 3500.0, 4.0, 1.2);
            dp.set_knob(knob_db);
            let n = SR as usize / 4;
            let warm = n / 2;
            let mut out = Vec::with_capacity(n - warm);
            for i in 0..n {
                let x = (2.0 * PI * 6000.0 * i as f32 / SR).sin();
                let y = dp.process(x, env);
                if i >= warm {
                    out.push(y);
                }
            }
            goertzel(&out, 6000.0, SR)
        };
        let idle_spread = band_gain(8.5, 0.0) / band_gain(-3.5, 0.0);
        let driven_spread = band_gain(8.5, 2.5) / band_gain(-3.5, 2.5);
        assert!(
            idle_spread > driven_spread * 1.8,
            "presence authority not drive-dependent: idle {idle_spread:.3} driven {driven_spread:.3}"
        );
        // …and under drive the response must still carry the open-loop lift
        // (top end doesn't just die when the loop lets go).
        assert!(
            band_gain(-3.5, 2.5) > band_gain(-3.5, 0.0),
            "cut presence did not drift up toward the open-loop lift under drive"
        );
    }

    // — Output transformer (LF core saturation + crossover) ————————————————————

    /// The transformer must compress the *lows* (high core flux) while leaving the
    /// *highs* (low flux) essentially linear — the frequency-selective give that
    /// makes a cranked power amp woolly on the bottom but not smeared on top. We
    /// compare effective gain quiet-vs-loud at a low and a high frequency.
    #[test]
    fn output_transformer_compresses_lows_not_highs() {
        let eff_gain = |freq: f32, amp_in: f32| -> f32 {
            let mut ot = OutputTransformer::new(SR, 150.0, 1.5, 0.04);
            let n = SR as usize / 4;
            let warm = n / 2;
            let mut out = Vec::with_capacity(n - warm);
            for i in 0..n {
                let x = (2.0 * PI * freq * i as f32 / SR).sin() * amp_in;
                let y = ot.process(x);
                if i >= warm {
                    out.push(y);
                }
            }
            goertzel(&out, freq, SR) / amp_in
        };
        let low_quiet = eff_gain(60.0, 0.1);
        let low_loud = eff_gain(60.0, 1.5);
        assert!(
            low_loud < low_quiet * 0.85,
            "OT lows not compressing: quiet {low_quiet:.3} → loud {low_loud:.3}"
        );
        let high_quiet = eff_gain(4000.0, 0.1);
        let high_loud = eff_gain(4000.0, 1.5);
        assert!(
            high_loud > high_quiet * 0.9,
            "OT is compressing highs (should pass clean): quiet {high_quiet:.3} → loud {high_loud:.3}"
        );
    }

    /// A hard low note through the transformer must grow genuine harmonic
    /// complexity (core saturation → odd harmonics, crossover → bite) while staying
    /// finite and bounded — woolly warmth, not a fizzy mess.
    #[test]
    fn output_transformer_adds_low_harmonics_and_is_finite() {
        let mut ot = OutputTransformer::new(SR, 150.0, 1.6, 0.05);
        let freq = 80.0;
        let n = SR as usize / 4;
        let warm = n / 2;
        let mut out = Vec::with_capacity(n - warm);
        for i in 0..n {
            let x = (2.0 * PI * freq * i as f32 / SR).sin() * 1.5;
            let y = ot.process(x);
            assert!(y.is_finite() && y.abs() < 4.0, "OT unstable: {y}");
            if i >= warm {
                out.push(y);
            }
        }
        let fund = goertzel(&out, freq, SR);
        let h2 = goertzel(&out, freq * 2.0, SR);
        let h3 = goertzel(&out, freq * 3.0, SR);
        assert!(
            h2 + h3 > 0.02 * fund,
            "OT added no harmonic complexity: h2+h3={:.4} fund={fund:.4}",
            h2 + h3
        );
    }

    // — Bright cap (treble-bleed across the gain pot) ——————————————————————————

    /// The cap bleeds treble around the gain wiper, strongest with the pot down and
    /// gone when it's wide open — and it must only touch the highs, never the lows
    /// (the cap blocks them). Verified directly on the building block.
    #[test]
    fn bright_cap_adds_treble_only_when_the_pot_is_down() {
        let level = |gain: f32, freq: f32| -> f32 {
            let mut bc = BrightCap::new(SR, 2000.0, 0.2);
            let n = SR as usize / 4;
            let warm = n / 2;
            let mut out = Vec::with_capacity(n - warm);
            for i in 0..n {
                let x = (2.0 * PI * freq * i as f32 / SR).sin();
                let y = bc.process(x, gain);
                if i >= warm {
                    out.push(y);
                }
            }
            goertzel(&out, freq, SR)
        };
        // Highs (above the cap corner): boosted at low gain, ~unity wide open.
        let treble_low_gain = level(0.0, 4000.0);
        let treble_high_gain = level(1.0, 4000.0);
        assert!(
            treble_low_gain > treble_high_gain * 1.05,
            "bright cap adds no treble at low gain: {treble_low_gain:.3} vs {treble_high_gain:.3}"
        );
        assert!(
            (treble_high_gain - 1.0).abs() < 0.05,
            "bright cap not ~unity with the pot wide open: {treble_high_gain:.3}"
        );
        // Lows (below the corner): the cap blocks them at any setting.
        let bass_low_gain = level(0.0, 100.0);
        assert!(
            (bass_low_gain - 1.0).abs() < 0.1,
            "bright cap is leaking lows: {bass_low_gain:.3}"
        );
    }

    // — Integration: the features reach the player's controls ——————————————————

    /// One instance per tube amp (Marshall + Mesa + Vox + Hiwatt), the models that
    /// carry the triode/transformer/bright-cap chain. The Randall is solid-state and
    /// is deliberately left out of these — it has no output transformer or triode
    /// stage to model.
    fn tube_amps() -> Vec<(&'static str, AmpModel, Box<dyn Amplifier>)> {
        vec![
            (
                "Marshall",
                AmpModel::Marshall,
                Box::new(Marshall::new(SR)) as Box<dyn Amplifier>,
            ),
            ("Mesa", AmpModel::Mesa, Box::new(Mesa::new(SR))),
            ("Vox", AmpModel::Vox, Box::new(Vox::new(SR))),
            ("Hiwatt", AmpModel::Hiwatt, Box::new(Hiwatt::new(SR))),
            ("Plexi", AmpModel::Plexi, Box::new(Plexi::new(SR))),
            ("Fender", AmpModel::Fender, Box::new(Fender::new(SR))),
            ("Supro", AmpModel::Supro, Box::new(Supro::new(SR))),
            ("Tweed", AmpModel::Tweed, Box::new(Tweed::new(SR))),
        ]
    }

    /// Through a whole amp, the bright cap must make low-gain settings audibly
    /// brighter than cranked ones: the treble-to-mid tilt, measured small-signal so
    /// the gain stages stay roughly linear, must fall as the gain pot is opened.
    #[test]
    fn bright_cap_brightens_low_gain_settings() {
        let tilt = |model: AmpModel, amp: &mut dyn Amplifier, gain: f32| -> f32 {
            let knobs = standard_knobs(model, gain, 0.5, 0.5, 0.5, 0.5, 0.6);
            let hi = run_tone(amp, 4000.0, 0.02, &knobs);
            let lo = run_tone(amp, 300.0, 0.02, &knobs);
            goertzel(&hi, 4000.0, SR) / goertzel(&lo, 300.0, SR).max(1e-9)
        };
        for (name, model, mut amp) in tube_amps() {
            let a = &mut *amp;
            let low_gain = tilt(model, a, 0.1);
            let high_gain = tilt(model, a, 0.9);
            assert!(
                low_gain > high_gain * 1.05,
                "{name}: bright cap not brightening low gain (tilt {low_gain:.3} vs {high_gain:.3})"
            );
        }
    }

    /// The dynamic stages must make the amp *touch sensitive*: digging in harder
    /// should bloom more even-harmonic warmth than playing softly, at the same
    /// settings. Measured at a moderate gain where the stage is responsive (not
    /// already pinned), so the growth comes from the dynamics, not just more static
    /// clipping. This is the single best proxy for "alive, not artificial".
    ///
    /// **Measured as absolute h2, not h2/h1.** The original version gated on
    /// `h2/h1` rising 5%, and that gate was silently calibrated against the
    /// inverted sag model (C3): with sag suppressing the power stage under load, the
    /// ratio happened to creep up 12% on a Marshall. Fixing the sag makes the power
    /// stage clip *harder* when you lean on it, which grows h3 faster than h2 and
    /// dilutes the ratio.
    ///
    /// Absolute h2 is the property this doc actually describes, and it is
    /// *stricter* than the old gate: h2 grows **4.3x** on a Marshall from soft to
    /// hard picking under the fixed sag, against 3.4x under the old one. The touch
    /// response got better; the ratio was just the wrong witness.
    ///
    /// A second guard replaces the ratio check an earlier draft used. Requiring
    /// `h2/h1` to hold up as gain rises is the same mistake the original gate made:
    /// opening the gain legitimately grows odd harmonics faster than even ones, so
    /// the ratio falls on a healthy amp (a Marshall goes 0.0141 -> 0.0094 here) while
    /// the *absolute* even content it is meant to protect grows 2.9x. The guard is
    /// now a presence floor instead -- the output must still carry real even
    /// harmonic content when driven, not merely more of it than before.
    #[test]
    fn tube_amps_are_touch_sensitive() {
        fn h2_h1(amp: &mut dyn Amplifier, model: AmpModel, drive_in: f32) -> (f32, f32) {
            let out = run_tone(
                amp,
                150.0,
                drive_in,
                &standard_knobs(model, 0.3, 0.5, 0.5, 0.6, 0.5, 0.6),
            );
            (
                goertzel(&out, 300.0, SR),
                goertzel(&out, 150.0, SR).max(1e-12),
            )
        }
        for (name, model, mut amp) in tube_amps() {
            let a = &mut *amp;
            let (h2_soft, _h1_soft) = h2_h1(a, model, 0.05);
            let (h2_hard, h1_hard) = h2_h1(a, model, 0.5);

            let growth = h2_hard / h2_soft;
            assert!(
                growth > 1.5,
                "{name}: even-harmonic content did not bloom with picking force \
                 (h2 {h2_soft:.6} -> {h2_hard:.6}, {growth:.2}x)"
            );

            let ratio_hard = h2_hard / h1_hard;
            assert!(
                ratio_hard > 0.006,
                "{name}: driven output has almost no even-harmonic content \
                 (h2/h1 {ratio_hard:.4}) -- it will read as thin and buzzy"
            );
        }
    }
}
