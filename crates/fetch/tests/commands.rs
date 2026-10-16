//! The `fetch.*` registry commands, through a real registry. Feature
//! `network`; only loopback traffic, and no torrent session is started.
#![cfg(feature = "network")]

#[path = "support/fault_server.rs"]
mod fault_server;

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

#[test]
fn rules_import_enable_remove_round_trip_and_persist() {
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("fetch.json");
    let mut h = FetchHost::new(path.clone());
    let one = br#"{"name":"a","hosts":["a.example.invalid"],"media":[{"regex":"(?P<url>x)"}]}"#;
    let two = br#"[{"name":"b","hosts":["b.example.invalid"],"media":[{"regex":"(?P<url>y)"}]},
                   {"name":"c","hosts":["c.example.invalid"],"media":[{"regex":"(?P<url>z)"}]}]"#;
    assert_eq!(h.import_rules(one).unwrap(), vec![1]);
    assert_eq!(h.import_rules(two).unwrap(), vec![2, 3]);
    // A bad file adds nothing and does not say what was wrong with the pattern.
    let e = h
        .import_rules(
            br#"{"name":"d","hosts":["d.example.invalid"],"media":[{"regex":"(?P<url>("}]}"#,
        )
        .unwrap_err();
    assert!(!e.to_string().contains("(?P<url>"));
    assert_eq!(h.config().extract_rules.len(), 3);
    h.set_rule_enabled(2, false).unwrap();
    h.remove_rule(1).unwrap();
    let again = FetchHost::new(path);
    let rules = &again.config().extract_rules;
    assert_eq!(
        rules.iter().map(|r| (r.id, r.enabled)).collect::<Vec<_>>(),
        [(2, false), (3, true)]
    );
    // Ids are not reused while a higher one exists.
    let mut h = again;
    assert_eq!(h.import_rules(one).unwrap(), vec![4]);
}

// ---- extract.page, download.* and the settings that go with them -------------

fn site() -> fault_server::Server {
    use fault_server::{Cfg, Route, mp4_moov_last};
    let mut cfg = Cfg::new(Vec::new());
    cfg.routes.push(Route::new(
        "/page",
        "text/html",
        br#"<meta property="og:title" content="Secret Title">
            <meta property="og:video" content="/media/clip.mp4">"#
            .to_vec(),
        false,
    ));
    cfg.routes.push(Route::new(
        "/media/clip.mp4",
        "video/mp4",
        mp4_moov_last(40_000),
        true,
    ));
    fault_server::start(cfg)
}

fn save_to(reg: &Registry<Ctx>, c: &mut Ctx, dir: &std::path::Path) {
    reg.run(c, "fetch.settings", json!({"save_dir": dir}))
        .unwrap();
}

#[test]
fn extract_page_hides_urls_unless_asked() {
    let d = tempfile::tempdir().unwrap();
    let srv = site();
    let (reg, mut c) = setup(d.path());
    let r = reg
        .run(
            &mut c,
            "extract.page",
            json!({"url": srv.url("/page"), "mode": "builtin"}),
        )
        .unwrap();
    let txt = r.to_string();
    assert!(
        !txt.contains("127.0.0.1") && !txt.contains("clip.mp4"),
        "{txt}"
    );
    assert_eq!(r["via"], "builtin");
    assert_eq!(r["entries"][0]["formats"][0]["index"], 0);
    assert!(r["token"].is_u64());
    assert_eq!(r["has_thumbnail"], false);
    let r = reg
        .run(
            &mut c,
            "extract.page",
            json!({"url": srv.url("/page"), "mode": "builtin", "show_urls": true}),
        )
        .unwrap();
    assert!(
        r["entries"][0]["formats"][0]["url"]
            .as_str()
            .unwrap()
            .ends_with("/media/clip.mp4")
    );
    assert!(
        reg.run(
            &mut c,
            "extract.page",
            json!({"url": srv.url("/page"), "mode": "x"})
        )
        .is_err()
    );
}

#[test]
fn download_start_by_token_then_wait_imports_once() {
    let d = tempfile::tempdir().unwrap();
    let save = d.path().join("saved");
    let srv = site();
    let (reg, mut c) = setup(d.path());
    save_to(&reg, &mut c, &save);
    let r = reg
        .run(&mut c, "extract.page", json!({"url": srv.url("/page")}))
        .unwrap();
    let ids = reg
        .run(
            &mut c,
            "download.start",
            json!({"token": r["token"], "quality": "720"}),
        )
        .unwrap();
    assert_eq!(ids["ids"].as_array().unwrap().len(), 1);
    let w = reg
        .run(&mut c, "download.wait", json!({"timeout_secs": 30}))
        .unwrap();
    assert_eq!(
        (w["done"].as_u64(), w["failed"].as_u64()),
        (Some(1), Some(0))
    );
    assert_eq!(w["imported"], 1);
    assert!(save.join("Secret Title.mp4").is_file());
    // The save folder became a library folder.
    assert_eq!(c.db.folders().unwrap().len(), 1);
    // Nothing new finished: nothing is imported twice.
    let w = reg.run(&mut c, "download.wait", json!({})).unwrap();
    assert_eq!(w["imported"], 0);
    // The list says nothing about the address, the title or the folder.
    let l = reg.run(&mut c, "download.list", json!({})).unwrap();
    let txt = l.to_string();
    assert!(!txt.contains("127.0.0.1") && !txt.contains("Secret") && !txt.contains("saved"));
    assert_eq!(l["jobs"][0]["state"], "done");
    let id = l["jobs"][0]["id"].as_u64().unwrap();
    reg.run(&mut c, "download.remove", json!({"id": id}))
        .unwrap();
    assert!(save.join("Secret Title.mp4").is_file());
    assert!(matches!(
        reg.run(&mut c, "download.remove", json!({"id": id})),
        Err(Error::NotFound(_))
    ));
    // An unknown token is a plain not-found.
    assert!(matches!(
        reg.run(&mut c, "download.start", json!({"token": 1})),
        Err(Error::NotFound(_))
    ));
}

#[test]
fn download_into_an_existing_library_folder_rescans_it() {
    let d = tempfile::tempdir().unwrap();
    let lib = d.path().join("lib");
    std::fs::create_dir_all(&lib).unwrap();
    let srv = site();
    let (reg, mut c) = setup(d.path());
    reg.run(&mut c, "library.import", json!({"path": lib}))
        .unwrap();
    save_to(&reg, &mut c, &lib.join("incoming"));
    reg.run(
        &mut c,
        "download.start",
        json!({"url": srv.url("/media/clip.mp4")}),
    )
    .unwrap();
    let w = reg
        .run(&mut c, "download.wait", json!({"timeout_secs": 30}))
        .unwrap();
    assert_eq!(w["imported"], 1);
    // Still one library folder: the save folder is inside it.
    assert_eq!(c.db.folders().unwrap().len(), 1);
}

#[test]
fn settings_validation_messages_are_fixed() {
    let d = tempfile::tempdir().unwrap();
    let (reg, mut c) = setup(d.path());
    let cases = [
        (
            json!({"save_dir": "relative/zzz"}),
            "the save folder must be an absolute path",
        ),
        (
            json!({"save_concurrency": 9}),
            "save_concurrency must be 1 to 4",
        ),
        (
            json!({"save_concurrency": 0}),
            "save_concurrency must be 1 to 4",
        ),
        (
            json!({"extractor": "zzz"}),
            "extractor must be first, fallback or off",
        ),
        (
            json!({"page_user_agent": "caf\u{e9}"}),
            "page_user_agent must be short printable text",
        ),
    ];
    for (args, want) in cases {
        let e = reg.run(&mut c, "fetch.settings", args.clone()).unwrap_err();
        assert_eq!(e.to_string(), format!("invalid input: {want}"));
        // The offending value never comes back.
        assert!(!e.to_string().contains("zzz"));
    }
    reg.run(
        &mut c,
        "fetch.settings",
        json!({"save_concurrency": 3, "import_after_save": false, "extractor": "first",
               "follow_embeds": false, "page_user_agent": "Test/1.0"}),
    )
    .unwrap();
    let s = &c.fetch.config().save;
    assert_eq!(
        (s.concurrency, s.import_after, s.follow_embeds),
        (3, false, false)
    );
    assert_eq!(s.page_user_agent.as_deref(), Some("Test/1.0"));
    // Empty values restore the defaults.
    reg.run(
        &mut c,
        "fetch.settings",
        json!({"page_user_agent": "", "save_dir": ""}),
    )
    .unwrap();
    assert!(c.fetch.config().save.page_user_agent.is_none());
    assert!(c.fetch.config().save.dir.is_none());
    // Bad download arguments say what is wrong, not what was given.
    let e = reg
        .run(
            &mut c,
            "download.start",
            json!({"url": "http://a.invalid/", "quality": "zzz"}),
        )
        .unwrap_err();
    assert!(!e.to_string().contains("zzz"));
    assert!(
        reg.run(
            &mut c,
            "download.start",
            json!({"url": "http://a.invalid/", "dir": "rel"})
        )
        .is_err()
    );
}

#[test]
fn rules_import_list_remove_enable_round_trip_never_echo_patterns() {
    let d = tempfile::tempdir().unwrap();
    let (reg, mut c) = setup(d.path());
    let file = d.path().join("rules.json");
    std::fs::write(
        &file,
        br#"{"name":"mine","hosts":["a.example.invalid","*.b.example.invalid"],
            "media":[{"regex":"SECRETPATTERN(?P<url>x)"}]}"#,
    )
    .unwrap();
    let r = reg
        .run(&mut c, "extract.rules.import", json!({"path": file}))
        .unwrap();
    assert_eq!(r["ids"], json!([1]));
    let l = reg.run(&mut c, "extract.rules.list", json!({})).unwrap();
    assert!(!l.to_string().contains("SECRETPATTERN"));
    assert!(!l.to_string().contains("example.invalid"));
    assert_eq!(l["rules"][0]["hosts"], 2);
    assert_eq!(l["rules"][0]["enabled"], true);
    reg.run(
        &mut c,
        "extract.rules.enable",
        json!({"id": 1, "on": false}),
    )
    .unwrap();
    let l = reg.run(&mut c, "extract.rules.list", json!({})).unwrap();
    assert_eq!(l["rules"][0]["enabled"], false);
    // A bad file adds nothing and never echoes the pattern.
    let bad = d.path().join("bad.json");
    std::fs::write(
        &bad,
        br#"{"name":"x","hosts":["a.example.invalid"],"media":[{"regex":"LEAKME(?P<url>("}]}"#,
    )
    .unwrap();
    let e = reg
        .run(&mut c, "extract.rules.import", json!({"path": bad}))
        .unwrap_err();
    assert!(!e.to_string().contains("LEAKME"));
    // A file over the limit is refused unread.
    let big = d.path().join("big.json");
    std::fs::write(&big, vec![b' '; 70_000]).unwrap();
    assert!(
        reg.run(&mut c, "extract.rules.import", json!({"path": big}))
            .is_err()
    );
    reg.run(&mut c, "extract.rules.remove", json!({"id": 1}))
        .unwrap();
    assert!(matches!(
        reg.run(&mut c, "extract.rules.remove", json!({"id": 1})),
        Err(Error::NotFound(_))
    ));
    let l = reg.run(&mut c, "extract.rules.list", json!({})).unwrap();
    assert_eq!(l["rules"].as_array().unwrap().len(), 0);
}

#[test]
fn the_new_commands_are_registered() {
    let d = tempfile::tempdir().unwrap();
    let (reg, _) = setup(d.path());
    let names: Vec<&str> = reg.list().map(|(n, _)| n).collect();
    for n in [
        "download.start",
        "download.pause",
        "download.resume",
        "download.cancel",
        "download.remove",
        "download.list",
        "download.wait",
        "extract.page",
        "extract.rules.import",
        "extract.rules.list",
        "extract.rules.remove",
        "extract.rules.enable",
    ] {
        assert!(names.contains(&n), "{n}");
    }
}
