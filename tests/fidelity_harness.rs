//! Integration test for the fidelity harness: one bundled preset through the
//! real `render_preset` path must be deterministic, finite, and land in a sane
//! loudness range. (Does not spawn the example binary.)

use rusty_riff::analysis::metrics::lufs_integrated;
use rusty_riff::analysis::render::{RenderOpts, render_preset};
use rusty_riff::analysis::synth::{Phrase, phrase};
use rusty_riff::preset::{Preset, PresetSource};

#[test]
fn harness_render_is_deterministic_finite_and_loud() {
    let path = std::path::PathBuf::from("presets/pink_floyd_time_solo.toml");
    let preset = Preset::load(&path, PresetSource::System).expect("load bundled preset");
    let di = phrase(Phrase::Chugs, 48_000.0);
    let opts = RenderOpts {
        max_tail_s: 0.0,
        ..RenderOpts::default()
    };

    let a = render_preset(&preset, &di, &opts);
    let b = render_preset(&preset, &di, &opts);
    assert_eq!(a.l, b.l, "render is not deterministic (L)");
    assert_eq!(a.r, b.r, "render is not deterministic (R)");
    assert!(
        a.l.iter().chain(a.r.iter()).all(|x| x.is_finite()),
        "render produced a non-finite sample"
    );

    let lufs = lufs_integrated(&a.l, &a.r, a.sr);
    assert!(
        lufs.is_finite() && (-40.0..=0.0).contains(&lufs),
        "integrated loudness out of range: {lufs} LUFS"
    );
}
