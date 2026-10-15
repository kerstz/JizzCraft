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
        subs: Some("fr".into()),
        ..Config::default()
    };
    c.save();
    let back = Config::load();
    assert_eq!(back, c);

    // Private by default on Unix: directory 0700, file 0600.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let file = jizzcraft_ui::config::path();
        let mode = |p: &std::path::Path| std::fs::metadata(p).unwrap().permissions().mode();
        assert_eq!(mode(&file) & 0o077, 0);
        assert_eq!(mode(file.parent().unwrap()) & 0o077, 0);
    }

    // A corrupt file falls back to defaults instead of crashing the app.
    // Forgive, forget, re-default.
    std::fs::write(jizzcraft_ui::config::path(), b"\x00\x01 not json").unwrap();
    let broken = Config::load();
    assert!(!broken.age_confirmed);

    // Settings from an older version (missing fields) keep what they have.
    std::fs::write(jizzcraft_ui::config::path(), br#"{"age_confirmed":true}"#).unwrap();
    let old = Config::load();
    assert!(old.age_confirmed && !old.left_hand);
    // ... and get the defaults of the automatic lock: 15 minutes, boss key off.
    assert_eq!(old.autolock_minutes(), 15);
    assert!(!old.boss_locks);

    // The automatic lock settings persist, 0 (never) included, and are clamped.
    let c = Config {
        autolock_minutes: Some(0),
        boss_locks: true,
        ..Config::default()
    };
    c.save();
    let back = Config::load();
    assert_eq!(back.autolock_minutes(), 0);
    assert!(back.boss_locks);
    let c = Config {
        autolock_minutes: Some(7),
        ..Config::default()
    };
    c.save();
    assert_eq!(Config::load().autolock_minutes(), 7);
    let huge = Config {
        autolock_minutes: Some(u32::MAX),
        ..Config::default()
    };
    assert_eq!(huge.autolock_minutes(), 24 * 60);
}
