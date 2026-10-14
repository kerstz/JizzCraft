//! Policy that needs no socket: the network truth table and the config file
//! rules (corrupt file set aside, mode 0600, resume default).

use jizzcraft_fetch::config::{FetchConfig, ResumeOnStart};
use jizzcraft_fetch::secret::Secret;

#[test]
fn net_policy_truth_table() {
    // (dht, listen_incoming, proxy) -> (dht, trackers, listen); UPnP never.
    let cases = [
        (false, false, false, (false, true, false)),
        (true, false, false, (true, true, false)),
        (true, true, false, (true, true, true)),
        (false, true, false, (false, true, true)),
        (true, false, true, (false, false, false)),
        (true, true, true, (false, false, false)),
        (false, true, true, (false, false, false)),
    ];
    for (dht, listen, proxy, want) in cases {
        let c = FetchConfig {
            dht,
            listen_incoming: listen,
            proxy: proxy.then(|| Secret::new("socks5://127.0.0.1:9050")),
            ..Default::default()
        };
        let p = c.net_policy();
        assert_eq!(
            (p.dht, p.trackers, p.listen),
            want,
            "{dht} {listen} {proxy}"
        );
        assert!(!p.upnp);
    }
    // Out of the box: listener off, UPnP off, DHT on.
    let d = FetchConfig::default().net_policy();
    assert!(d.dht && d.trackers && !d.listen && !d.upnp);
}

#[test]
fn resume_defaults_to_ask_and_round_trips() {
    assert_eq!(FetchConfig::default().resume_on_start, ResumeOnStart::Ask);
    let old: FetchConfig = serde_json::from_str(r#"{"indexers":[]}"#).unwrap();
    assert_eq!(old.resume_on_start, ResumeOnStart::Ask);
    let c: FetchConfig = serde_json::from_str(r#"{"resume_on_start":"always"}"#).unwrap();
    assert_eq!(c.resume_on_start, ResumeOnStart::Always);
    assert_eq!(ResumeOnStart::parse("never"), Some(ResumeOnStart::Never));
    assert_eq!(ResumeOnStart::parse("sometimes"), None);
}

#[test]
fn a_corrupt_file_is_set_aside_not_overwritten() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("fetch.json");
    let junk = br#"{"indexers":[{"id":1,"name":"x","url":"u","api_key":"SECRET" ,"#;
    std::fs::write(&p, junk).unwrap();
    let c = FetchConfig::load(&p);
    assert!(c.load_error && c.indexers.is_empty());
    assert!(!p.exists());
    let kept: Vec<_> = std::fs::read_dir(d.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("fetch.json.corrupt-"))
        .collect();
    assert_eq!(kept.len(), 1, "{kept:?}");
    assert_eq!(std::fs::read(d.path().join(&kept[0])).unwrap(), junk);
    // `load_error` is runtime only; a healthy file does not set it.
    c.save(&p).unwrap();
    assert!(!std::fs::read_to_string(&p).unwrap().contains("load_error"));
    assert!(!FetchConfig::load(&p).load_error);
    // A missing file is not an error.
    assert!(!FetchConfig::load(&d.path().join("none.json")).load_error);
}

#[cfg(unix)]
#[test]
fn load_tightens_a_loose_mode_to_0600() {
    use std::os::unix::fs::PermissionsExt;
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("fetch.json");
    FetchConfig::default().save(&p).unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o644)).unwrap();
    let _ = FetchConfig::load(&p);
    let mode = std::fs::metadata(&p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
}
