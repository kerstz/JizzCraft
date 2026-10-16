//! The `fetch.*` registry commands, through a real registry. Feature
//! `network`; only loopback traffic, and no torrent session is started.
#![cfg(feature = "network")]

use jizzcraft_core::commands::{HasLibrary, Registry, register_core};
use jizzcraft_core::{Db, Error};
use jizzcraft_fetch::commands::{FetchHost, HasFetch, register_fetch};
use serde_json::{Value, json};
use std::io::{Read, Write};
use std::net::TcpListener;

struct Ctx {
    db: Db,
    fetch: FetchHost,
}
impl HasLibrary for Ctx {
    fn db(&mut self) -> &mut Db {
        &mut self.db
    }
}
impl HasFetch for Ctx {
    fn fetch(&mut self) -> &mut FetchHost {
        &mut self.fetch
    }
}

fn setup(dir: &std::path::Path) -> (Registry<Ctx>, Ctx) {
    let mut reg = Registry::new();
    register_core(&mut reg);
    register_fetch(&mut reg);
    let ctx = Ctx {
        db: Db::open_in_memory().unwrap(),
        fetch: FetchHost::new(dir.join("fetch.json")),
    };
    (reg, ctx)
}

fn indexer_server(xml: &'static str) -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for s in l.incoming() {
            let mut s = s.unwrap();
            let mut b = [0u8; 2048];
            let _ = s.read(&mut b);
            let _ = write!(
                s,
                "HTTP/1.1 200 OK\r\nContent-Type: application/xml\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{xml}",
                xml.len()
            );
        }
    });
    port
}

const XML: &str = r#"<rss xmlns:torznab="x"><channel>
<item><title>Sample Clip 01</title>
<link>http://127.0.0.1:1/dl/secret-token.torrent</link>
<enclosure url="http://127.0.0.1:1/dl/secret-token.torrent" length="10"/>
<torznab:attr name="seeders" value="9"/></item>
<item><title>Sample Clip 02</title><link>magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567</link></item>
</channel></rss>"#;

#[test]
fn indexers_are_managed_without_ever_revealing_keys() {
    let d = tempfile::tempdir().unwrap();
    let (reg, mut c) = setup(d.path());
    let r = reg
        .run(
            &mut c,
            "fetch.indexers.add",
            json!({"name":"","url":"http://localhost:9117/torznab/","api_key":"TOPSECRET"}),
        )
        .unwrap();
    assert_eq!(r["id"], 1);
    let l = reg.run(&mut c, "fetch.indexers.list", json!({})).unwrap();
    let txt = l.to_string();
    assert!(!txt.contains("TOPSECRET"));
    assert_eq!(l["indexers"][0]["has_key"], true);
    // The name defaulted to the host.
    assert_eq!(l["indexers"][0]["name"], "localhost");

    // Persisted for the next launch, with the key only in the 0600 file.
    let again = FetchHost::new(d.path().join("fetch.json"));
    assert_eq!(again.config().indexers.len(), 1);

    assert!(matches!(
        reg.run(&mut c, "fetch.indexers.remove", json!({"id": 99})),
        Err(Error::NotFound(_))
    ));
    reg.run(&mut c, "fetch.indexers.remove", json!({"id": 1}))
        .unwrap();
    let l = reg.run(&mut c, "fetch.indexers.list", json!({})).unwrap();
    assert_eq!(l["indexers"].as_array().unwrap().len(), 0);
}

#[test]
fn search_returns_magnets_but_never_download_links() {
    let d = tempfile::tempdir().unwrap();
    let port = indexer_server(XML);
    let (reg, mut c) = setup(d.path());
    reg.run(
        &mut c,
        "fetch.indexers.add",
        json!({"name":"mock","url":format!("http://127.0.0.1:{port}/t"),"api_key":"KEY123"}),
    )
    .unwrap();
    let r = reg
        .run(
            &mut c,
            "fetch.search",
            json!({"query":"sample clip","limit":10}),
        )
        .unwrap();
    assert_eq!(r["count"], 2);
    let txt = r.to_string();
    // A link can carry the indexer key: only its presence is reported.
    // "Yes, there is a key." "Which one?" "Yes."
    assert!(!txt.contains("secret-token") && !txt.contains("KEY123"));
    let items = r["items"].as_array().unwrap();
    let with_link = items.iter().find(|i| i["has_link"] == true).unwrap();
    assert!(with_link["magnet"].is_null());
    let with_magnet = items.iter().find(|i| i["magnet"].is_string()).unwrap();
    assert!(
        with_magnet["magnet"]
            .as_str()
            .unwrap()
            .starts_with("magnet:")
    );
}

#[test]
fn search_without_an_indexer_is_a_plain_error() {
    let d = tempfile::tempdir().unwrap();
    let (reg, mut c) = setup(d.path());
    let e = reg
        .run(&mut c, "fetch.search", json!({"query":"x"}))
        .unwrap_err();
    assert!(e.to_string().contains("no enabled indexer"));
    assert!(reg.run(&mut c, "fetch.search", json!({})).is_err());
    assert!(reg.run(&mut c, "fetch.add", json!({})).is_err());
}

#[test]
fn download_folder_is_created_and_imported_as_a_library_root() {
    let d = tempfile::tempdir().unwrap();
    let dl = d.path().join("incoming");
    let (reg, mut c) = setup(d.path());
    let v: Value = reg
        .run(&mut c, "fetch.dir.set", json!({"path": dl}))
        .unwrap();
    assert!(v["folder_id"].is_number());
    assert!(dl.is_dir());
    assert_eq!(c.fetch.config().download_dir.as_deref(), Some(dl.as_path()));
}

#[test]
fn proxy_setting_is_optional_and_trimmed() {
    let d = tempfile::tempdir().unwrap();
    let (reg, mut c) = setup(d.path());
    assert!(c.fetch.config().proxy.is_none() && !c.fetch.config().require_proxy);
    reg.run(
        &mut c,
        "fetch.settings",
        json!({"proxy": "  socks5://127.0.0.1:9050  "}),
    )
    .unwrap();
    let p = c.fetch.config().proxy.clone().unwrap();
    assert_eq!(p.expose(), "socks5://127.0.0.1:9050");
    // Blanks clear it: back to direct connections, not an error.
    reg.run(&mut c, "fetch.settings", json!({"proxy": "   "}))
        .unwrap();
    assert!(c.fetch.config().proxy.is_none());
    assert!(
        reg.run(&mut c, "fetch.settings", json!({"proxy": "http://x:1"}))
            .is_err()
    );
}

#[test]
fn a_corrupt_config_makes_the_host_start_with_the_kill_switch_on() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("fetch.json");
    std::fs::write(&p, b"{\"require_proxy\":false,").unwrap();
    let host = FetchHost::new(p);
    assert!(host.config().require_proxy && host.config().load_error);
}
