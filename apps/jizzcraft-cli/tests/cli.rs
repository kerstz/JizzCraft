//! The CLI end to end, as a user runs it: real binary, private home, tiny
//! synthetic images. No GStreamer needed. Pixels this small have no secrets.

use serde_json::Value;
use std::path::Path;
use std::process::{Command, Output};

fn cli(home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_jizzcraft-cli"))
        .arg("--home")
        .arg(home)
        .args(args)
        .env_remove("JIZZCRAFT_DB")
        .env_remove("JIZZCRAFT_HOME")
        .output()
        .expect("run jizzcraft-cli")
}

fn json(o: &Output) -> Value {
    assert!(
        o.status.success(),
        "exit {:?}: {}",
        o.status.code(),
        String::from_utf8_lossy(&o.stderr)
    );
    serde_json::from_slice(&o.stdout)
        .unwrap_or_else(|e| panic!("not JSON ({e}): {}", String::from_utf8_lossy(&o.stdout)))
}

fn make_library(dir: &Path) {
    std::fs::create_dir_all(dir.join("sub")).unwrap();
    for (i, name) in ["a.png", "b.png", "sub/c.png"].iter().enumerate() {
        let img = image::RgbImage::from_fn(8 + i as u32, 6, |x, y| {
            image::Rgb([(x * 30) as u8, (y * 40) as u8, 90])
        });
        img.save(dir.join(name)).unwrap();
    }
    std::fs::write(dir.join("notes.txt"), b"not media").unwrap();
}

#[test]
fn scan_query_favorite_and_stats() {
    let home = tempfile::tempdir().unwrap();
    let lib = tempfile::tempdir().unwrap();
    make_library(lib.path());

    let empty = json(&cli(home.path(), &["--json", "stats"]));
    assert_eq!(empty["total"], 0);

    let scan = json(&cli(
        home.path(),
        &["--json", "scan", lib.path().to_str().unwrap()],
    ));
    assert_eq!(scan["report"]["added"], 3, "{scan}");

    let q = json(&cli(home.path(), &["--json", "query", "--type", "image"]));
    assert_eq!(q["count"], 3);
    let id = q["items"][0]["id"].as_i64().unwrap();

    let by_name = json(&cli(home.path(), &["--json", "query", "--name", "sub"]));
    assert_eq!(
        by_name["count"], 0,
        "filename filter matches names, not folders"
    );
    let by_name = json(&cli(home.path(), &["--json", "query", "--name", "b."]));
    assert_eq!(by_name["count"], 1);

    json(&cli(home.path(), &["--json", "fav", &id.to_string()]));
    let hits = json(&cli(home.path(), &["--json", "search", "B", "PNG"]));
    assert_eq!(hits["count"], 1, "{hits}");
    let none = json(&cli(home.path(), &["--json", "search", "zzz"]));
    assert_eq!(none["count"], 0);
    let favs = json(&cli(home.path(), &["--json", "query", "--fav"]));
    assert_eq!(favs["count"], 1);

    let stats = json(&cli(home.path(), &["--json", "stats"]));
    assert_eq!(stats["total"], 3);

    // Rescanning an unchanged folder adds nothing and never touches media.
    let before: Vec<_> = std::fs::read_dir(lib.path())
        .unwrap()
        .count()
        .to_owned()
        .to_le_bytes()
        .to_vec();
    let rescan = json(&cli(home.path(), &["--json", "rescan"]));
    assert_eq!(rescan["report"]["added"], 0);
    let after: Vec<_> = std::fs::read_dir(lib.path())
        .unwrap()
        .count()
        .to_owned()
        .to_le_bytes()
        .to_vec();
    assert_eq!(before, after);
}

#[test]
fn registry_commands_are_listed_and_unknown_ones_fail_cleanly() {
    let home = tempfile::tempdir().unwrap();
    let list = json(&cli(home.path(), &["--json", "commands"]));
    let names: Vec<&str> = list
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"library.import"));
    assert!(names.contains(&"media.query"));
    if cfg!(feature = "network") {
        // The network build pins the commands it adds.
        assert!(names.contains(&"fetch.search"), "{names:?}");
    } else {
        // The default build has no network commands at all. Not hidden, absent.
        assert!(!names.iter().any(|n| n.starts_with("fetch.")), "{names:?}");
    }

    let bad = cli(home.path(), &["--json", "run", "no.such.command"]);
    assert!(!bad.status.success());
    let err: Value = serde_json::from_slice(&bad.stdout).unwrap();
    assert!(err["error"].as_str().unwrap().contains("not found"));

    let bad_args = cli(home.path(), &["--json", "run", "media.get", "{\"id\":"]);
    assert!(!bad_args.status.success());
}

#[test]
fn run_passes_json_arguments_through() {
    let home = tempfile::tempdir().unwrap();
    let lib = tempfile::tempdir().unwrap();
    make_library(lib.path());
    json(&cli(
        home.path(),
        &[
            "--json",
            "run",
            "library.import",
            &format!("{{\"path\":{:?}}}", lib.path()),
        ],
    ));
    let q = json(&cli(
        home.path(),
        &[
            "--json",
            "run",
            "media.query",
            "{\"sort\":\"name\",\"desc\":true,\"limit\":1}",
        ],
    ));
    assert_eq!(q["count"], 1);
    // Path separators differ per platform.
    assert!(
        std::path::Path::new(q["items"][0]["path"].as_str().unwrap())
            .ends_with(std::path::Path::new("sub").join("c.png"))
    );
}

/// The support report is built from an allow-list: seed distinctive names,
/// paths, keys and URLs everywhere they could live and make sure none leaks.
#[test]
fn doctor_report_never_contains_user_strings() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home-XYZZY-HOME");
    let lib = tmp.path().join("lib-PLUGH-FOLDER");
    make_library(&lib);
    std::fs::rename(lib.join("a.png"), lib.join("SECRETNAME-a.png")).unwrap();
    assert!(
        cli(&home, &["scan", lib.to_str().unwrap()])
            .status
            .success()
    );
    std::fs::create_dir_all(&home).unwrap();
    let cfg = serde_json::json!({
        "age_confirmed": true,
        "language": "fr",
        "panic_video": "/srv/PANICPATH/video.mp4",
        "indexer_key": "KEY-0123456789-APIKEY",
        "source_url": "https://tracker-SECRETHOST.invalid/announce",
        "last_search": "SEARCHTERM-private"
    });
    std::fs::write(home.join("config.json"), cfg.to_string()).unwrap();
    // The config dir under --home is the home itself or a subfolder: write both.
    for sub in ["config", "state"] {
        let d = home.join(sub);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("config.json"), cfg.to_string()).unwrap();
    }
    let o = cli(&home, &["doctor", "--report"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let text = String::from_utf8_lossy(&o.stdout).into_owned();
    for needle in [
        "XYZZY",
        "PLUGH",
        "SECRETNAME",
        "PANICPATH",
        "APIKEY",
        "SECRETHOST",
        "SEARCHTERM",
        "tracker",
        ".invalid",
        "/home",
        "lib-",
        "tmp",
    ] {
        assert!(!text.contains(needle), "leaked {needle}:\n{text}");
    }
    assert!(text.contains("items: 3"), "{text}");
    assert!(text.contains("folders: 1"), "{text}");
    assert!(text.contains("config.age_confirmed: true"), "{text}");
    assert!(text.contains("schema_version: "), "{text}");
    assert!(text.contains("edition: "), "{text}");
}

#[test]
fn version_states_the_edition() {
    let tmp = tempfile::tempdir().unwrap();
    let o = cli(tmp.path(), &["--version"]);
    let text = String::from_utf8_lossy(&o.stdout).into_owned();
    let want = if cfg!(feature = "online") || cfg!(feature = "network") {
        "online edition"
    } else {
        "offline edition"
    };
    assert!(text.starts_with("jizzcraft-cli "), "{text}");
    assert!(text.contains(want), "{text}");
}

#[test]
fn doctor_starts_with_the_edition() {
    let tmp = tempfile::tempdir().unwrap();
    let o = cli(tmp.path(), &["doctor"]);
    let text = String::from_utf8_lossy(&o.stdout).into_owned();
    let online = cfg!(feature = "online") || cfg!(feature = "network");
    let first = if online {
        "Edition: online"
    } else {
        "Edition: offline"
    };
    assert!(text.starts_with(first), "{text}");
    assert_eq!(text.contains("Browser: "), online, "{text}");
    let o = cli(tmp.path(), &["--json", "doctor"]);
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(v["edition"], if online { "online" } else { "offline" });
}

// ---- organise: playlists, tags, categories ----

fn tree_bytes(dir: &Path) -> Vec<(std::path::PathBuf, Vec<u8>)> {
    let mut out = vec![];
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                stack.push(p);
            } else {
                let b = std::fs::read(&p).unwrap();
                out.push((p, b));
            }
        }
    }
    out.sort();
    out
}

fn paths_of(v: &Value) -> Vec<String> {
    v["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| {
            Path::new(m["path"].as_str().unwrap())
                .file_name()
                .unwrap()
                .to_string_lossy()
                .into_owned()
        })
        .collect()
}

fn id_of(out: &Value) -> i64 {
    out["id"]
        .as_i64()
        .unwrap_or_else(|| panic!("no id in {out}"))
}

#[test]
fn organise_end_to_end_and_media_untouched() {
    let home = tempfile::tempdir().unwrap();
    let lib = tempfile::tempdir().unwrap();
    make_library(lib.path());
    let before = tree_bytes(lib.path());
    let c = |args: &[&str]| json(&cli(home.path(), args));
    c(&["--json", "scan", lib.path().to_str().unwrap()]);

    // Media ids by name.
    let q = c(&["--json", "query", "--type", "image"]);
    let mut by_name = std::collections::HashMap::new();
    for m in q["items"].as_array().unwrap() {
        let n = Path::new(m["path"].as_str().unwrap())
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        by_name.insert(n, m["id"].as_i64().unwrap().to_string());
    }
    let (a, b, cc) = (&by_name["a.png"], &by_name["b.png"], &by_name["c.png"]);

    // Playlist: create, add, reorder.
    let pl = id_of(&c(&["--json", "playlist", "create", "Evening"])).to_string();
    let added = c(&["--json", "playlist", "add", &pl, cc, a, b]);
    assert_eq!(added["added"], 3, "{added}");
    let q = c(&["--json", "query", "--playlist", &pl, "--sort", "position"]);
    assert_eq!(paths_of(&q), ["c.png", "a.png", "b.png"]);
    c(&["--json", "playlist", "move", &pl, b, "--before", cc]);
    let q = c(&["--json", "query", "--playlist", &pl, "--sort", "position"]);
    assert_eq!(paths_of(&q), ["b.png", "c.png", "a.png"]);
    let items = c(&["--json", "playlist", "items", &pl]);
    assert_eq!(items["total"], 3);
    c(&["--json", "playlist", "sort", &pl, "--by", "name", "--desc"]);
    let q = c(&["--json", "query", "--playlist", &pl, "--sort", "position"]);
    assert_eq!(paths_of(&q), ["c.png", "b.png", "a.png"]);
    let list = c(&["--json", "playlist", "list"]);
    assert_eq!(list["items"][0]["count"], 3);
    // Position sorting only makes sense inside a playlist.
    assert!(
        !cli(home.path(), &["query", "--sort", "position"])
            .status
            .success()
    );
    // Human table shows the id and the count.
    let human = cli(home.path(), &["playlist", "list"]);
    let text = String::from_utf8_lossy(&human.stdout);
    assert!(text.contains("items") && text.contains(&pl), "{text}");

    // Tags: AND / OR / NOT.
    let red = id_of(&c(&["--json", "tag", "create", "red"])).to_string();
    let blue = id_of(&c(&["--json", "tag", "create", "blue"])).to_string();
    c(&["--json", "tag", "assign", &red, a, b]);
    c(&["--json", "tag", "assign", &blue, b, cc]);
    let both = c(&["--json", "query", "--tag", &red, "--tag", &blue]);
    assert_eq!(paths_of(&both), ["b.png"]);
    let either = c(&["--json", "query", "--any-tag", &red, "--any-tag", &blue]);
    assert_eq!(either["count"], 3);
    let not = c(&["--json", "query", "--tag", &red, "--not-tag", &blue]);
    assert_eq!(paths_of(&not), ["a.png"]);
    c(&["--json", "tag", "assign", &red, b, "--off"]);
    let red_only = c(&["--json", "query", "--tag", &red]);
    assert_eq!(paths_of(&red_only), ["a.png"]);
    let tog = c(&["--json", "tag", "toggle", &red, a]);
    assert_eq!(tog["on"], false);
    let none = c(&["--json", "query", "--untagged"]);
    assert_eq!(paths_of(&none), ["a.png"]);
    c(&["--json", "tag", "toggle", &blue, cc]);
    c(&["--json", "tag", "assign", &blue, b, "--off"]);
    let untagged = c(&["--json", "query", "--untagged"]);
    assert_eq!(untagged["count"], 3);

    // Categories: a subtree filter.
    let top = id_of(&c(&["--json", "category", "create", "Top"])).to_string();
    let kid = id_of(&c(&[
        "--json", "category", "create", "Kid", "--parent", &top,
    ]))
    .to_string();
    c(&["--json", "category", "assign", &kid, cc]);
    c(&["--json", "category", "assign", &top, a]);
    let sub = c(&["--json", "query", "--category", &top]);
    assert_eq!(sub["count"], 2, "{sub}");
    let just_kid = c(&["--json", "query", "--category", &kid]);
    assert_eq!(paths_of(&just_kid), ["c.png"]);
    let unc = c(&["--json", "query", "--uncategorized"]);
    assert_eq!(paths_of(&unc), ["b.png"]);
    c(&["--json", "category", "move", &kid]);
    let cats = c(&["--json", "category", "list"]);
    assert_eq!(cats["items"].as_array().unwrap().len(), 2);
    assert_eq!(
        c(&[
            "--json",
            "run",
            "labels.of",
            &format!("{{\"media_id\":{cc}}}")
        ])["categories"][0]["name"],
        "Kid"
    );

    // M3U export refuses an existing destination, and works on a new one.
    let out = tempfile::tempdir().unwrap();
    let dest = out.path().join("list.m3u8");
    let dest_s = dest.to_str().unwrap();
    let ok = c(&["--json", "playlist", "export-m3u", &pl, dest_s]);
    assert_eq!(ok["written"], 3);
    let text = std::fs::read_to_string(&dest).unwrap();
    assert!(text.contains("a.png"));
    assert!(!text.contains("Evening"));
    let again = cli(
        home.path(),
        &["--json", "playlist", "export-m3u", &pl, dest_s],
    );
    assert!(!again.status.success(), "must not overwrite");
    assert_eq!(std::fs::read_to_string(&dest).unwrap(), text);

    // Remove, rename, delete: playlists and labels go, files stay.
    c(&["--json", "playlist", "rename", &pl, "Night"]);
    c(&["--json", "playlist", "remove", &pl, a]);
    c(&["--json", "tag", "merge", &blue, &red]);
    c(&["--json", "playlist", "delete", &pl]);
    c(&["--json", "category", "delete", &top]);
    c(&["--json", "tag", "delete", &red]);

    assert_eq!(before, tree_bytes(lib.path()), "media files changed");
}

#[test]
fn gen_man_renders_a_roff_page() {
    let home = tempfile::tempdir().unwrap();
    let o = cli(home.path(), &["gen", "man"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let page = String::from_utf8(o.stdout).unwrap();
    assert!(page.contains(".TH"), "not a roff page");
    assert!(page.contains("jizzcraft-cli"));
    assert!(!page.contains(&*home.path().to_string_lossy()));
    if let Ok(h) = std::env::var("HOME") {
        assert!(h.len() < 2 || !page.contains(&h), "page leaks the home dir");
    }
}

#[test]
fn metadata_end_to_end_through_the_cli() {
    let home = tempfile::tempdir().unwrap();
    let lib = tempfile::tempdir().unwrap();
    make_library(lib.path());
    let before = tree_bytes(lib.path());
    json(&cli(
        home.path(),
        &["--json", "scan", lib.path().to_str().unwrap()],
    ));
    let q = json(&cli(home.path(), &["--json", "query", "--type", "image"]));
    let id = q["items"][0]["id"].as_i64().unwrap().to_string();

    json(&cli(
        home.path(),
        &[
            "--json",
            "info",
            &id,
            "--title",
            "Quokka",
            "--released",
            "2020-02-29",
        ],
    ));
    let studio = id_of(&json(&cli(
        home.path(),
        &["--json", "studio", "create", "Acme"],
    )));
    json(&cli(
        home.path(),
        &[
            "--json",
            "studio",
            "assign",
            "--studio",
            &studio.to_string(),
            &id,
        ],
    ));
    let person = id_of(&json(&cli(
        home.path(),
        &["--json", "person", "create", "Jane"],
    )));
    let p = person.to_string();
    json(&cli(
        home.path(),
        &["--json", "person", "alias-add", &p, "Jay"],
    ));
    json(&cli(home.path(), &["--json", "person", "assign", &p, &id]));
    let info = json(&cli(home.path(), &["--json", "info", &id]));
    assert_eq!(info["title"], "Quokka");
    assert_eq!(info["studio"]["name"], "Acme");
    assert_eq!(info["people"][0]["aliases"][0], "Jay");

    let hit = json(&cli(home.path(), &["--json", "search", "jay"]));
    assert_eq!(hit["count"], 1, "{hit}");
    let by_person = json(&cli(home.path(), &["--json", "query", "--person", &p]));
    assert_eq!(by_person["count"], 1);
    let by_studio = json(&cli(
        home.path(),
        &["--json", "query", "--studio", &studio.to_string()],
    ));
    assert_eq!(by_studio["count"], 1);

    json(&cli(home.path(), &["--json", "info", &id, "--clear-title"]));
    let info = json(&cli(home.path(), &["--json", "info", &id]));
    assert!(info["title"].is_null());
    // Bad input fails cleanly and says nothing about the value.
    let bad = cli(
        home.path(),
        &["--json", "info", &id, "--released", "yesterday"],
    );
    assert!(!bad.status.success());
    assert_eq!(tree_bytes(lib.path()), before, "media untouched");
}

#[test]
fn import_nfo_is_a_dry_run_unless_applied() {
    let home = tempfile::tempdir().unwrap();
    let lib = tempfile::tempdir().unwrap();
    std::fs::write(lib.path().join("a.mp4"), b"not a real video, only a name").unwrap();
    std::fs::write(
        lib.path().join("a.nfo"),
        "<movie><title>From nfo</title></movie>",
    )
    .unwrap();
    let before = tree_bytes(lib.path());
    json(&cli(
        home.path(),
        &["--json", "scan", lib.path().to_str().unwrap()],
    ));
    let id = json(&cli(home.path(), &["--json", "query", "--type", "video"]))["items"][0]["id"]
        .as_i64()
        .unwrap()
        .to_string();

    let dry = json(&cli(home.path(), &["--json", "import-nfo"]));
    assert_eq!(
        (dry["dry_run"].clone(), dry["matched"].clone()),
        (true.into(), 1.into()),
        "{dry}"
    );
    assert!(json(&cli(home.path(), &["--json", "info", &id]))["title"].is_null());

    let done = json(&cli(home.path(), &["--json", "import-nfo", "--apply"]));
    assert_eq!(done["titles_set"], 1, "{done}");
    assert_eq!(
        json(&cli(home.path(), &["--json", "info", &id]))["title"],
        "From nfo"
    );
    assert_eq!(tree_bytes(lib.path()), before, "files untouched");
}

#[test]
fn import_stash_is_a_dry_run_unless_applied() {
    let home = tempfile::tempdir().unwrap();
    let lib = tempfile::tempdir().unwrap();
    let ex = tempfile::tempdir().unwrap();
    let bytes = vec![7u8; 3000];
    std::fs::write(lib.path().join("a.mp4"), &bytes).unwrap();
    json(&cli(
        home.path(),
        &["--json", "scan", lib.path().to_str().unwrap()],
    ));
    std::fs::create_dir_all(ex.path().join("scenes")).unwrap();
    std::fs::create_dir_all(ex.path().join("files")).unwrap();
    // Matched by oshash (every byte of the file is 7).
    // 3000 bytes: 375 words at each end, every word 0x0707070707070707.
    let oshash = 0x0707_0707_0707_0707u64
        .wrapping_mul(750)
        .wrapping_add(3000);
    std::fs::write(
        ex.path().join("files").join("a.json"),
        serde_json::json!({
            "path": "/stash/a.mp4", "size": 3000,
            "fingerprints": [{ "type": "oshash", "fingerprint": format!("{oshash:016x}") }]
        })
        .to_string(),
    )
    .unwrap();
    std::fs::write(
        ex.path().join("scenes").join("a.json"),
        serde_json::json!({ "title": "From stash", "files": ["/stash/a.mp4"] }).to_string(),
    )
    .unwrap();
    let id = json(&cli(home.path(), &["--json", "query", "--type", "video"]))["items"][0]["id"]
        .as_i64()
        .unwrap()
        .to_string();
    let dir = ex.path().to_str().unwrap();
    let dry = json(&cli(home.path(), &["--json", "import-stash", dir]));
    assert_eq!(dry["matched"], 1, "{dry}");
    assert!(json(&cli(home.path(), &["--json", "info", &id]))["title"].is_null());
    json(&cli(
        home.path(),
        &["--json", "import-stash", dir, "--apply"],
    ));
    assert_eq!(
        json(&cli(home.path(), &["--json", "info", &id]))["title"],
        "From stash"
    );
    let bad = cli(
        home.path(),
        &["--json", "import-stash", lib.path().to_str().unwrap()],
    );
    assert!(!bad.status.success());
}
