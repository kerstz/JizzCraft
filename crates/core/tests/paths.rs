//! Where things live. Its own process: the home is set once per process.

use jizzcraft_core::paths;
use std::path::Path;

#[test]
fn an_explicit_home_holds_everything() {
    let d = tempfile::tempdir().unwrap();
    assert!(!paths::is_portable());
    assert!(paths::set_home(d.path().to_path_buf()));
    assert!(!paths::set_home(d.path().join("other")), "home is set once");
    assert!(paths::is_portable());
    assert_eq!(paths::home(), Some(d.path()));
    for p in [
        paths::db_path(),
        paths::config_dir(),
        paths::cache_dir(),
        paths::state_dir(),
    ] {
        assert!(p.starts_with(d.path()), "{} escapes the home", p.display());
    }
    assert_ne!(paths::config_dir(), paths::cache_dir());
    assert_ne!(paths::state_dir(), paths::cache_dir());
    assert!(paths::portable_home().is_some_and(|p| p.ends_with("jizzcraft-data")));
    let _ = Path::new("");
}
