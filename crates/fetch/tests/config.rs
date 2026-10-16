use jizzcraft_fetch::config::{FetchConfig, Indexer};
use jizzcraft_fetch::secret::Secret;

fn ix(id: u32) -> Indexer {
    Indexer {
        id,
        name: format!("n{id}"),
        url: "http://127.0.0.1:1/".into(),
        api_key: Secret::new("k"),
        enabled: true,
    }
}

#[test]
fn download_folder_defaults_need_no_setup() {
    let c = FetchConfig::default();
    let d = c.effective_download_dir();
    assert!(
        d.ends_with("JizzCraft") || d.ends_with("downloads"),
        "{}",
        d.display()
    );

    let disguised = FetchConfig {
        disguised: true,
        ..Default::default()
    };
    let n = disguised.effective_download_dir();
    assert!(
        !n.to_string_lossy().to_lowercase().contains("jizz") || n.ends_with("downloads"),
        "disguised folder name leaks the brand: {}",
        n.display()
    );

    let chosen = FetchConfig {
        download_dir: Some("/somewhere/else".into()),
        ..Default::default()
    };
    assert_eq!(
        chosen.effective_download_dir(),
        std::path::PathBuf::from("/somewhere/else")
    );
}

#[test]
fn missing_or_corrupt_config_falls_back_to_defaults() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("fetch.json");
    let c = FetchConfig::load(&p);
    assert!(c.indexers.is_empty() && c.dht && !c.listen_incoming && !c.seed_after_complete);
    std::fs::write(&p, b"{ not json").unwrap();
    assert!(FetchConfig::load(&p).indexers.is_empty());
    // Unknown and absent fields are tolerated (older or newer files).
    std::fs::write(&p, br#"{"indexers":[],"future_option":1}"#).unwrap();
    assert!(FetchConfig::load(&p).dht);
}

#[test]
fn indexer_ids_never_repeat_and_limits_roundtrip() {
    let mut c = FetchConfig::default();
    assert_eq!(c.next_indexer_id(), 1);
    c.indexers = vec![ix(1), ix(7)];
    assert_eq!(c.next_indexer_id(), 8);
    c.down_limit_mibps = Some(5);
    c.proxy = Some(Secret::new("socks5://127.0.0.1:9050"));
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("fetch.json");
    c.save(&p).unwrap();
    let back = FetchConfig::load(&p);
    assert_eq!(back.down_limit_mibps, Some(5));
    assert_eq!(back.proxy.unwrap().expose(), "socks5://127.0.0.1:9050");
    // `disguised` is runtime only: never written.
    let txt = std::fs::read_to_string(&p).unwrap();
    assert!(!txt.contains("disguised"));
}

#[test]
fn a_proxy_switches_off_everything_it_cannot_carry() {
    let open = FetchConfig {
        listen_incoming: true,
        ..Default::default()
    };
    let p = open.net_policy();
    assert!(p.dht && p.trackers && p.listen);

    let proxied = FetchConfig {
        listen_incoming: true,
        proxy: Some(Secret::new("socks5://127.0.0.1:9050")),
        ..Default::default()
    };
    let p = proxied.net_policy();
    assert!(!p.dht && !p.trackers && !p.listen, "{p:?}");

    // An empty proxy string means "no proxy", not "a proxy named nothing".
    let empty = FetchConfig {
        proxy: Some(Secret::new("")),
        ..Default::default()
    };
    assert!(empty.net_policy().dht);
}

/// The proxy is optional: the defaults have none and no kill switch, and a
/// blank or `null` proxy in a hand-edited file is "no proxy", never an
/// invalid one that would make the network fail.
#[test]
fn no_proxy_is_the_default_and_blank_means_none() {
    let c = FetchConfig::default();
    assert!(c.proxy.is_none() && !c.require_proxy);
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("fetch.json");
    for raw in [
        r#"{}"#,
        r#"{"proxy":null}"#,
        r#"{"proxy":""}"#,
        r#"{"proxy":"   "}"#,
    ] {
        std::fs::write(&p, raw).unwrap();
        let c = FetchConfig::load(&p);
        assert!(!c.load_error, "{raw}");
        assert!(c.proxy.is_none() && !c.require_proxy, "{raw}");
        let pol = c.net_policy();
        assert!(pol.dht && pol.trackers, "{raw}");
    }
    std::fs::write(&p, r#"{"proxy":" socks5://127.0.0.1:9050 "}"#).unwrap();
    let c = FetchConfig::load(&p);
    assert_eq!(c.proxy.unwrap().expose(), "socks5://127.0.0.1:9050");
}

#[test]
fn unreadable_config_turns_the_kill_switch_on() {
    let d = tempfile::tempdir().unwrap();
    let cases: [(&str, &[u8]); 2] = [
        ("junk", &[0xff, 0xfe, 0x00, 0x01, b'{', 0x80]),
        ("truncated", br#"{"require_proxy":false,"indexers":[{"#),
    ];
    for (name, bytes) in cases {
        let p = d.path().join(name).join("fetch.json");
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, bytes).unwrap();
        let c = FetchConfig::load(&p);
        assert!(c.load_error && c.require_proxy, "{name}");
    }
    // A directory where the file should be: read fails.
    let p = d.path().join("dir").join("fetch.json");
    std::fs::create_dir_all(&p).unwrap();
    let c = FetchConfig::load(&p);
    assert!(c.load_error && c.require_proxy);
    // A missing file is a fresh install, not an error.
    let c = FetchConfig::load(&d.path().join("none.json"));
    assert!(!c.load_error && !c.require_proxy);
    assert!(!FetchConfig::default().require_proxy);
}

#[test]
fn save_settings_default_and_old_files_still_load() {
    use jizzcraft_fetch::config::{DEFAULT_PAGE_UA, ExtractorMode, SaveSettings};
    let c: FetchConfig = serde_json::from_str("{}").unwrap();
    assert_eq!(c.save, SaveSettings::default());
    assert_eq!(c.save.extractor, ExtractorMode::Fallback);
    assert_eq!(c.save.concurrency(), 2);
    assert!(c.save.import_after && c.save.follow_embeds);
    assert_eq!(c.save.user_agent(), DEFAULT_PAGE_UA);
    let c: FetchConfig = serde_json::from_str(
        r#"{"save":{"concurrency":99,"extractor":"first","page_user_agent":"bad\u0007ua"}}"#,
    )
    .unwrap();
    assert_eq!(c.save.concurrency(), 4);
    assert_eq!(c.save.extractor, ExtractorMode::First);
    assert_eq!(c.save.user_agent(), DEFAULT_PAGE_UA);
    let s = SaveSettings {
        concurrency: 0,
        page_user_agent: Some("Plain UA/1.0".into()),
        ..Default::default()
    };
    assert_eq!(s.concurrency(), 1);
    assert_eq!(s.user_agent(), "Plain UA/1.0");
    assert_eq!(ExtractorMode::parse("off"), Some(ExtractorMode::Off));
    assert_eq!(ExtractorMode::parse("x"), None);
    let mut c = FetchConfig::default();
    assert_eq!(c.effective_save_dir(), c.effective_download_dir());
    c.save.dir = Some("/somewhere/saved".into());
    assert_eq!(
        c.effective_save_dir(),
        std::path::PathBuf::from("/somewhere/saved")
    );
}
