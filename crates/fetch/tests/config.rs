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
