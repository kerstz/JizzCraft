//! Own test binary: `set_home` is a process-wide, set-once value.

use jizzcraft_core::paths;

#[test]
fn explicit_home_redirects_db_config_and_cache() {
    // Skip if the environment already forces a database path.
    if std::env::var_os("JIZZCRAFT_DB").is_some() {
        return;
    }
    let dir = std::env::temp_dir().join("jizzcraft-home-test");
    assert!(paths::set_home(dir.clone()));
    assert!(!paths::set_home(dir.join("other")), "set-once");
    assert!(paths::is_portable());
    assert_eq!(paths::db_path(), dir.join("library.db"));
    assert_eq!(paths::config_dir(), dir.join("config"));
    assert_eq!(paths::cache_dir(), dir.join("cache"));
}
