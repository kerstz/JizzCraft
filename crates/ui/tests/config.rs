//! Saved settings. Own process: the home is set once.

use jizzcraft_ui::config::Config;

#[test]
fn settings_roundtrip_and_survive_garbage() {
    let d = tempfile::tempdir().unwrap();
    assert!(jizzcraft_core::paths::set_home(d.path().to_path_buf()));
    // First start: inspector open, nothing confirmed.
    let first = Config::load();
    assert!(first.inspector_open && !first.age_confirmed && first.language.is_none());

    let c = Config {
        age_confirmed: true,
        left_hand: true,
        language: Some("fr".into()),
        volume: Some(0.4),
        ..Config::default()
    };
    c.save();
    let back = Config::load();
    assert_eq!(back, c);

    // A corrupt file falls back to defaults instead of crashing the app.
    std::fs::write(jizzcraft_ui::config::path(), b"\x00\x01 not json").unwrap();
    let broken = Config::load();
    assert!(!broken.age_confirmed);

    // Settings from an older version (missing fields) keep what they have.
    std::fs::write(jizzcraft_ui::config::path(), br#"{"age_confirmed":true}"#).unwrap();
    let old = Config::load();
    assert!(old.age_confirmed && !old.left_hand);
}
