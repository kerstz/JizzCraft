#![cfg_attr(not(unix), allow(unused_imports, dead_code))]
use jizzcraft_fetch::config::{FetchConfig, Indexer};
use jizzcraft_fetch::redact::redact_url;
use jizzcraft_fetch::secret::Secret;
use jizzcraft_fetch::torznab::*;

fn ix() -> Indexer {
    Indexer {
        id: 3,
        name: "Local".into(),
        url: "http://localhost:9117/torznab/".into(),
        api_key: Secret::new("s3cr3t-key"),
        enabled: true,
    }
}

const RESULTS: &str = r#"<?xml version="1.0"?>
<rss version="2.0" xmlns:torznab="http://torznab.com/schemas/2015/feed">
 <channel>
  <item>
   <title>Sample Clip 01</title>
   <link>http://localhost:9117/dl/1.torrent</link>
   <pubDate>Mon, 01 Jan 2024 00:00:00 +0000</pubDate>
   <size>1048576</size>
   <enclosure url="http://localhost:9117/dl/1.torrent" length="1048576" type="application/x-bittorrent"/>
   <torznab:attr name="seeders" value="12"/>
   <torznab:attr name="peers" value="15"/>
   <torznab:attr name="magneturl" value="magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567"/>
  </item>
  <item><title>No link</title></item>
 </channel>
</rss>"#;

#[test]
fn parses_results() {
    let r = parse_results(RESULTS, 3).unwrap();
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].title, "Sample Clip 01");
    assert_eq!(r[0].seeders, Some(12));
    assert_eq!(r[0].peers, Some(15));
    assert_eq!(r[0].size, Some(1_048_576));
    assert!(r[0].magnet.as_deref().unwrap().starts_with("magnet:"));
    assert_eq!(r[0].indexer_id, 3);
}

#[test]
fn parses_caps_and_errors() {
    assert!(parse_caps("<caps><server/></caps>").is_ok());
    let e =
        parse_caps(r#"<error code="100" description="Incorrect user credentials"/>"#).unwrap_err();
    assert_eq!(
        e,
        TorznabError::Indexer {
            code: 100,
            description: "Incorrect user credentials".into()
        }
    );
    assert!(parse_results(r#"<error code="900" description="x"/>"#, 1).is_err());
    assert_eq!(
        parse_results("not xml at all", 1).unwrap_err(),
        TorznabError::Malformed
    );
}

#[test]
fn builds_urls_with_encoding() {
    let u = build_search_url(&ix(), "a b&c", &[2000, 5000], 50);
    assert!(u.starts_with("http://localhost:9117/torznab/api?apikey=s3cr3t-key&t=search"));
    assert!(u.contains("q=a%20b%26c"));
    assert!(u.ends_with("&limit=50&cat=2000,5000"));
}

#[test]
fn secrets_never_leak() {
    let i = ix();
    assert!(!format!("{i:?}").contains("s3cr3t"));
    let cfg = FetchConfig {
        indexers: vec![i.clone()],
        ..Default::default()
    };
    assert!(!format!("{cfg:?}").contains("s3cr3t"));
    assert!(!format!("{}", i.api_key).contains("s3cr3t"));
    let red = redact_url(&build_search_url(&i, "private query", &[], 10));
    assert_eq!(red, "http://localhost:9117/torznab/api");
    assert_eq!(redact_url("http://u:p@host/x?k=1"), "http://host/x");
}

#[test]
fn config_roundtrip_is_private() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("fetch.json");
    let cfg = FetchConfig {
        indexers: vec![ix()],
        ..Default::default()
    };
    cfg.save(&p).unwrap();
    let back = FetchConfig::load(&p);
    assert_eq!(back.indexers[0].api_key.expose(), "s3cr3t-key");
    assert!(!back.listen_incoming && !back.seed_after_complete);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&p).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}
