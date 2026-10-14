//! The CLI end to end, as a user runs it: real binary, private home, tiny
//! synthetic images. No GStreamer needed.

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
    // The default build has no network commands at all.
    assert!(!names.iter().any(|n| n.starts_with("fetch.")), "{names:?}");

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
    assert!(
        q["items"][0]["path"]
            .as_str()
            .unwrap()
            .ends_with("sub/c.png")
    );
}
