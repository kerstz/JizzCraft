//! Suggested resolver definitions, age advice and health hints.
#![cfg(all(feature = "network", unix))]

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use jizzcraft_fetch::FetchConfig;
use jizzcraft_fetch::detect_tools::{
    Found, age_sentence, already_added, find_resolver_tools, suggest, vetted_argv,
};
use jizzcraft_fetch::resolver::{self, ResolverDef};
use jizzcraft_fetch::resolver_setup;

fn found(path: &str) -> Found {
    Found {
        path: PathBuf::from(path),
        version: "2026.01.01".into(),
        age_days: Some(10),
    }
}

#[test]
fn argv_validates_and_suggestion_is_proxy_aware() {
    let d = suggest(&found("/usr/bin/yt-dlp"));
    assert_eq!(d.argv, vetted_argv(&PathBuf::from("/usr/bin/yt-dlp")));
    assert!(resolver::validate(&d).is_ok());
    // Counts as proxy-aware through the pair alone, as under the kill switch.
    assert!(!d.honours_proxy);
    assert!(d.proxy_aware());
    assert!(d.enabled);
    assert_eq!(d.timeout_secs, 60);
    assert_eq!(d.name, "Resolver");
}

#[test]
fn already_added_matches_program() {
    let mut cfg = FetchConfig::default();
    let p = PathBuf::from("/usr/bin/yt-dlp");
    assert!(!already_added(&cfg, &p));
    let d: ResolverDef = suggest(&found("/usr/bin/yt-dlp"));
    cfg.resolvers.push(d);
    assert!(already_added(&cfg, &p));
    assert!(!already_added(&cfg, &PathBuf::from("/usr/bin/other")));
}

#[test]
fn sentences_leak_nothing() {
    assert_eq!(age_sentence(None), None);
    assert_eq!(age_sentence(Some(60)), None);
    let s = age_sentence(Some(61)).unwrap();
    for i in 0..3 {
        resolver_setup::record(900, false);
        assert_eq!(resolver_setup::hint(900).is_some(), i == 2);
    }
    let h = resolver_setup::hint(900).unwrap();
    for t in [s, h] {
        assert!(!t.contains('/') && !t.contains("://") && !t.contains("yt-dlp"));
    }
    resolver_setup::record(900, true);
    assert_eq!(resolver_setup::hint(900), None);
}

#[test]
fn stub_in_path_dir_is_detected_with_new_argv() {
    let dir = std::env::temp_dir().join(format!("jz-setup-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    let tool = dir.join("yt-dlp");
    std::fs::write(&tool, "#!/bin/sh\necho 2026.01.01\n").unwrap();
    std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o700)).unwrap();
    let all = find_resolver_tools(std::slice::from_ref(&dir));
    let f = all.iter().find(|f| f.path == tool).expect("detected");
    let d = suggest(f);
    assert!(resolver::validate(&d).is_ok());
    assert!(d.argv.iter().any(|a| a == "{proxy}"));
    let _ = std::fs::remove_dir_all(&dir);
}
