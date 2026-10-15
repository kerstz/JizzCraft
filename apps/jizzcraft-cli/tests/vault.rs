//! The vault from the command line, end to end, plus the canary test: a
//! synthetic file, tags, categories and notes with unique strings go through
//! scan, thumbnails and `vault enable`; then every byte under the home
//! directory and every captured stdout/stderr is searched for them, for the
//! passphrase and for the database key.
//!
//! `config/config.json` is scanned too: only the keys of the GUI `Config`
//! are allowed in it (`panic_video` may hold a path the owner chose, never a
//! library item name; see docs/vault.md). The media file itself lives outside
//! the home and the vault never touches media. Output of commands that print
//! library data on purpose (`tag create`, `search`, ...) is not searched for
//! names, only for the passphrase and the key.
//!
//! Runs on every platform. Only `--passphrase-fd` (a Unix descriptor) is
//! exercised under `cfg(unix)`; everything else goes through stdin.

use std::io::Write;
#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const PASS: &str = "CANARY-pass-1234";
const FILE: &str = "zz-canary-7f3a.png";
const TAG: &str = "tag-canary-9c1e";
const TAG_LATE: &str = "tag-canary-late-1c2d";
const CATEGORY: &str = "cat-canary-4b7d";
const NOTE: &str = "note-canary-2e8a";
const PLAYLIST: &str = "plist-canary-5d3f";

struct Run {
    out: Output,
}

impl Run {
    fn code(&self) -> i32 {
        self.out.status.code().unwrap_or(-1)
    }
    fn text(&self) -> String {
        format!(
            "{}{}",
            String::from_utf8_lossy(&self.out.stdout),
            String::from_utf8_lossy(&self.out.stderr)
        )
    }
}

/// Give the child a pipe holding `data` on descriptor 3. Returns the read end
/// to close in the parent after the spawn.
#[cfg(unix)]
fn pipe_on_fd3(c: &mut Command, fd3: Option<&str>) -> Option<i32> {
    let mut fd3_read = None;
    if let Some(data) = fd3 {
        let mut fds = [0i32; 2];
        // SAFETY: plain pipe(2) with a valid array.
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
        let (r, w) = (fds[0], fds[1]);
        // SAFETY: write end owned here; small payload fits the pipe buffer.
        unsafe {
            let n = libc::write(w, data.as_ptr().cast(), data.len());
            assert_eq!(n as usize, data.len());
            libc::close(w);
        }
        fd3_read = Some(r);
        // SAFETY: only dup2 in the forked child, async-signal-safe.
        unsafe {
            c.pre_exec(move || {
                if libc::dup2(r, 3) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
    fd3_read
}

/// Run the CLI with `stdin` bytes and optionally a pipe on fd 3.
fn cli(home: &Path, args: &[&str], stdin: Option<&str>, fd3: Option<&str>) -> Run {
    let mut c = Command::new(env!("CARGO_BIN_EXE_jizzcraft-cli"));
    c.arg("--home")
        .arg(home)
        .args(args)
        .env_remove("JIZZCRAFT_DB")
        .env_remove("JIZZCRAFT_HOME")
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    let fd3_read = pipe_on_fd3(&mut c, fd3);
    #[cfg(not(unix))]
    assert!(fd3.is_none(), "fd 3 is a Unix feature");
    let mut child = c.spawn().expect("spawn jizzcraft-cli");
    #[cfg(unix)]
    if let Some(r) = fd3_read {
        // SAFETY: the parent's copy of the read end is no longer needed.
        unsafe { libc::close(r) };
    }
    if let Some(s) = stdin {
        child
            .stdin
            .take()
            .unwrap()
            .write_all(s.as_bytes())
            .expect("write stdin");
    }
    Run {
        out: child.wait_with_output().unwrap(),
    }
}

fn plain(home: &Path, args: &[&str]) -> Run {
    cli(home, args, None, None)
}

fn make_media(dir: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    let img = image::RgbImage::from_fn(16, 12, |x, y| {
        image::Rgb([(x * 10) as u8, (y * 15) as u8, 80])
    });
    img.save(dir.join(FILE)).unwrap();
    let img = image::RgbImage::from_fn(10, 10, |x, y| {
        image::Rgb([200, (x * 20) as u8, (y * 20) as u8])
    });
    img.save(dir.join("plain-other.png")).unwrap();
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let p = e.path();
        let md = std::fs::symlink_metadata(&p).unwrap();
        if md.is_dir() {
            walk(&p, out);
        } else {
            out.push(p);
        }
    }
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty() && hay.windows(needle.len()).any(|w| w == needle)
}

/// Keys `config.json` may hold (the GUI `Config` struct). `panic_video` is a
/// path chosen by the owner: it can name a file, so it is never a library
/// item, and the canary names must not appear anywhere in the file.
const CONFIG_ALLOWED: &[&str] = &[
    "age_confirmed",
    "onboarding_done",
    "discreet",
    "screen_share_safe",
    "inspector_open",
    "sidebar_closed",
    "volume",
    "left_hand",
    "sort",
    "sort_desc",
    "hide_history",
    "disguise",
    "autoplay_next",
    "no_previews",
    "language",
    "no_jokes",
    "skip_age_gate",
    "panic_video",
    "cache_pruned_at",
    "ui_zoom",
    "reduce_motion",
    "subs",
    "audio_lang",
    "autolock_minutes",
    "boss_locks",
];

/// Every key of every `config.json` under `home` is on the allow list.
fn config_keys_not_allowed(home: &Path) -> Vec<String> {
    let mut files = Vec::new();
    walk(home, &mut files);
    let mut bad = Vec::new();
    for f in files
        .iter()
        .filter(|f| f.file_name().is_some_and(|n| n == "config.json"))
    {
        let v: serde_json::Value =
            serde_json::from_slice(&std::fs::read(f).unwrap()).expect("config.json is valid JSON");
        for k in v.as_object().expect("config.json is an object").keys() {
            if !CONFIG_ALLOWED.contains(&k.as_str()) {
                bad.push(k.clone());
            }
        }
    }
    bad
}

/// Files under `home` that contain any needle, `config.json` included.
fn leaks(home: &Path, needles: &[(&str, Vec<u8>)]) -> Vec<String> {
    let mut files = Vec::new();
    walk(home, &mut files);
    let mut found = Vec::new();
    for f in files {
        let bytes = std::fs::read(&f).unwrap();
        for (label, n) in needles {
            if contains(&bytes, n) {
                found.push(format!(
                    "{} in {}",
                    label,
                    f.strip_prefix(home).unwrap().display()
                ));
            }
        }
    }
    found
}

fn json_of(r: &Run) -> serde_json::Value {
    assert_eq!(r.code(), 0, "{}", r.text());
    serde_json::from_slice(&r.out.stdout).unwrap_or_else(|e| panic!("not JSON ({e}): {}", r.text()))
}

/// Every secret key of the library, as raw bytes and lowercase hex: the
/// database key, the data key (DEK) and the two thumbnail sub-keys.
fn all_keys(home: &Path) -> Vec<(&'static str, Vec<u8>)> {
    let db = home.join("data").join("library.db");
    let db = if db.exists() {
        db
    } else {
        let mut files = Vec::new();
        walk(home, &mut files);
        files
            .into_iter()
            .find(|p| p.file_name().is_some_and(|n| n == "library.db"))
            .expect("library.db under home")
    };
    let key = jizzcraft_core::vault_convert::unlock_library(&db, PASS).expect("unlock");
    let thumbs = key.thumb_keys().bytes_for_tests();
    let raw: [(&'static str, [u8; 32]); 4] = [
        ("db key", *key.db_key().bytes_for_tests()),
        ("data key", *key.bytes_for_tests()),
        ("thumb aead key", *thumbs[0]),
        ("thumb name key", *thumbs[1]),
    ];
    let mut out = Vec::new();
    for (label, b) in raw {
        let hexs: String = b.iter().map(|x| format!("{x:02x}")).collect();
        out.push((label, b.to_vec()));
        out.push((label, hexs.to_uppercase().into_bytes()));
        out.push((label, hexs.into_bytes()));
    }
    // The keys are distinct, or the test would prove less than it says.
    let mut seen: Vec<&Vec<u8>> = out.iter().step_by(3).map(|(_, b)| b).collect();
    seen.sort();
    seen.dedup();
    assert_eq!(seen.len(), 4);
    out
}

#[test]
fn canary_nothing_readable_after_enable() {
    let home = tempfile::tempdir().unwrap();
    let lib = tempfile::tempdir().unwrap();
    make_media(lib.path());
    let h = home.path();
    // A config.json as the GUI writes it: allowed keys only, and it is scanned.
    std::fs::create_dir_all(h.join("config")).unwrap();
    std::fs::write(
        h.join("config").join("config.json"),
        r#"{"age_confirmed":true,"left_hand":true,"language":"fr","panic_video":"/neutral/videos/maths.mp4"}"#,
    )
    .unwrap();
    let mut checked_outputs: Vec<(String, String)> = Vec::new();
    let mut all_outputs: Vec<(String, String)> = Vec::new();

    // Build a library with unique strings, in clear.
    let scan = plain(h, &["--json", "scan", lib.path().to_str().unwrap()]);
    assert_eq!(json_of(&scan)["report"]["added"], 2, "{}", scan.text());
    let q = json_of(&plain(h, &["--json", "query", "--name", "canary"]));
    assert_eq!(q["count"], 1);
    let id = q["items"][0]["id"].as_i64().unwrap().to_string();
    let tag = json_of(&plain(h, &["--json", "tag", "create", TAG]));
    let tag_id = tag["id"].as_i64().unwrap().to_string();
    assert_eq!(plain(h, &["tag", "assign", &tag_id, &id]).code(), 0);
    let cat = json_of(&plain(h, &["--json", "category", "create", CATEGORY]));
    let cat_id = cat["id"].as_i64().unwrap().to_string();
    assert_eq!(plain(h, &["category", "assign", &cat_id, &id]).code(), 0);
    let pl = json_of(&plain(h, &["--json", "playlist", "create", PLAYLIST]));
    let pl_id = pl["id"].as_i64().unwrap().to_string();
    assert_eq!(plain(h, &["playlist", "add", &pl_id, &id]).code(), 0);
    let mk = plain(h, &["markers", &id, "--add", "1000", "--label", NOTE]);
    assert_eq!(mk.code(), 0, "{}", mk.text());

    let th = plain(h, &["--json", "thumbs"]);
    let v = json_of(&th);
    assert_eq!((v["images"].as_u64(), v["ok"].as_u64()), (Some(2), Some(2)));

    // Positive control: before enabling, the scanner finds the canary.
    let name_n: Vec<(&str, Vec<u8>)> = vec![("name", FILE.as_bytes().to_vec())];
    assert!(
        !leaks(h, &name_n).is_empty(),
        "positive control failed: the plain library should contain the name"
    );
    assert!(h.join("cache/thumbs").exists());

    // Enable. Refused without the acknowledgement.
    let refused = cli(h, &["vault", "enable"], Some(&format!("{PASS}\n")), None);
    assert_ne!(refused.code(), 0);
    // (the prompt stdin is not a terminal: --passphrase-stdin supplies it)
    let refused = cli(
        h,
        &["--passphrase-stdin", "vault", "enable"],
        Some(&format!("{PASS}\n")),
        None,
    );
    assert_eq!(refused.code(), 1, "{}", refused.text());
    assert!(refused.text().contains("--i-understand-no-recovery"));
    let en = cli(
        h,
        &[
            "--passphrase-stdin",
            "vault",
            "enable",
            "--i-understand-no-recovery",
        ],
        Some(&format!("{PASS}\n")),
        None,
    );
    assert_eq!(en.code(), 0, "{}", en.text());
    assert!(en.text().contains("no recovery"), "warning shown");
    checked_outputs.push(("vault enable".into(), en.text()));

    // Everything below runs against the encrypted library.
    let status = plain(h, &["vault", "status"]);
    assert_eq!(status.code(), 0);
    assert!(status.text().contains("vault: locked"));
    checked_outputs.push(("vault status".into(), status.text()));

    // Locked and no passphrase, not a terminal: exit 4 with a fixed sentence.
    let locked = plain(h, &["stats"]);
    assert_eq!(locked.code(), 4, "{}", locked.text());
    checked_outputs.push(("locked stats".into(), locked.text()));

    let v = cli(
        h,
        &["--passphrase-stdin", "vault", "verify"],
        Some(&format!("{PASS}\n")),
        None,
    );
    assert_eq!(v.code(), 0, "{}", v.text());
    checked_outputs.push(("vault verify".into(), v.text()));

    let th = cli(
        h,
        &["--json", "--passphrase-stdin", "thumbs"],
        Some(&format!("{PASS}\n")),
        None,
    );
    let tv = json_of(&th);
    assert_eq!(
        (tv["images"].as_u64(), tv["ok"].as_u64()),
        (Some(2), Some(2))
    );
    checked_outputs.push(("thumbs".into(), th.text()));

    // A tag created while encrypted, and a rescan.
    let late = cli(
        h,
        &["--json", "--passphrase-stdin", "tag", "create", TAG_LATE],
        Some(&format!("{PASS}\n")),
        None,
    );
    assert_eq!(late.code(), 0, "{}", late.text());
    all_outputs.push(("tag create".into(), late.text()));
    let rs = cli(
        h,
        &["--passphrase-stdin", "rescan"],
        Some(&format!("{PASS}\n")),
        None,
    );
    assert_eq!(rs.code(), 0, "{}", rs.text());
    checked_outputs.push(("rescan".into(), rs.text()));

    for args in [
        vec!["--passphrase-stdin", "stats"],
        vec!["--passphrase-stdin", "run", "vault.status"],
        vec!["--passphrase-stdin", "run", "cache.prune"],
        vec!["--passphrase-stdin", "doctor", "--report"],
    ] {
        let r = cli(h, &args, Some(&format!("{PASS}\n")), None);
        assert_eq!(r.code(), 0, "{args:?}: {}", r.text());
        if args.contains(&"doctor") {
            assert!(r.text().contains("vault: on"), "{}", r.text());
        }
        checked_outputs.push((args.join(" "), r.text()));
    }
    let backup = h.join("copy.db");
    let b = cli(
        h,
        &["--passphrase-stdin", "backup", backup.to_str().unwrap()],
        Some(&format!("{PASS}\n")),
        None,
    );
    assert_eq!(b.code(), 0, "{}", b.text());
    checked_outputs.push(("backup".into(), b.text()));
    assert!(backup.exists());

    // Data is readable with the passphrase (and prints the name on purpose).
    let s = cli(
        h,
        &["--json", "--passphrase-stdin", "search", "canary"],
        Some(&format!("{PASS}\n")),
        None,
    );
    assert_eq!(json_of(&s)["count"], 1, "{}", s.text());
    all_outputs.push(("search".into(), s.text()));
    let lock_cmd = cli(
        h,
        &["--passphrase-stdin", "run", "vault.lock"],
        Some(&format!("{PASS}\n")),
        None,
    );
    assert_eq!(lock_cmd.code(), 0);
    all_outputs.push(("vault.lock".into(), lock_cmd.text()));

    // Vault thumbnails exist, nothing clear remains.
    let mut files = Vec::new();
    walk(&h.join("cache"), &mut files);
    assert!(
        files
            .iter()
            .any(|f| f.extension().is_some_and(|e| e == "jzt"))
    );
    assert!(
        !files
            .iter()
            .any(|f| f.extension().is_some_and(|e| e == "jpg"))
    );

    // The scan of every byte.
    let mut needles: Vec<(&str, Vec<u8>)> = vec![
        ("file name", FILE.as_bytes().to_vec()),
        ("file stem", b"zz-canary".to_vec()),
        ("tag", TAG.as_bytes().to_vec()),
        ("late tag", TAG_LATE.as_bytes().to_vec()),
        ("category", CATEGORY.as_bytes().to_vec()),
        ("note", NOTE.as_bytes().to_vec()),
        ("playlist", PLAYLIST.as_bytes().to_vec()),
        ("passphrase", PASS.as_bytes().to_vec()),
    ];
    needles.extend(all_keys(h));
    let found = leaks(h, &needles);
    assert!(found.is_empty(), "plaintext found on disk: {found:?}");
    assert_eq!(config_keys_not_allowed(h), Vec::<String>::new());
    // The header of a production enable carries the default cost (65536/3/1).
    let hdr = std::fs::read(jizzcraft_core::vault::header_path(
        &h.join("data").join("library.db"),
    ))
    .or_else(|_| {
        let mut files = Vec::new();
        walk(h, &mut files);
        let f = files
            .into_iter()
            .find(|p| p.extension().is_some_and(|e| e == "vault"))
            .expect("a .vault header under home");
        std::fs::read(f)
    })
    .unwrap();
    assert_eq!(
        jizzcraft_core::vault::validate_header(&hdr).unwrap(),
        jizzcraft_core::vault::KdfParams {
            m_cost: 65_536,
            t_cost: 3,
            p_cost: 1
        }
    );

    for (label, text) in &checked_outputs {
        for (what, n) in &needles {
            assert!(
                !contains(text.as_bytes(), n),
                "{what} leaked in the output of `{label}`"
            );
        }
    }
    // Commands that print data may show names, never the passphrase or key.
    for (label, text) in checked_outputs.iter().chain(all_outputs.iter()) {
        for (what, n) in needles
            .iter()
            .filter(|(w, _)| *w == "passphrase" || w.contains("key"))
        {
            assert!(!contains(text.as_bytes(), n), "{what} leaked in `{label}`");
        }
    }
}

/// Portable: stdin only (no descriptors), so it runs on Windows too.
#[test]
fn stdin_roundtrip_and_exit_codes() {
    let home = tempfile::tempdir().unwrap();
    let lib = tempfile::tempdir().unwrap();
    make_media(lib.path());
    let h = home.path();
    json_of(&plain(h, &["--json", "scan", lib.path().to_str().unwrap()]));
    let before = json_of(&plain(h, &["--json", "stats"]));
    let pw = format!("{PASS}\n");
    let flags = [
        "--passphrase-stdin",
        "vault",
        "enable",
        "--i-understand-no-recovery",
    ];
    let en = cli(h, &flags, Some(&pw), None);
    assert_eq!(en.code(), 0, "{}", en.text());
    assert_eq!(plain(h, &["stats"]).code(), 4);
    let w = cli(
        h,
        &["--passphrase-stdin", "stats"],
        Some("not the passphrase\n"),
        None,
    );
    assert_eq!(w.code(), 3, "{}", w.text());
    let v = cli(
        h,
        &["--passphrase-stdin", "vault", "verify"],
        Some(&pw),
        None,
    );
    assert_eq!(v.code(), 0, "{}", v.text());
    let rep = cli(
        h,
        &["--passphrase-stdin", "doctor", "--report"],
        Some(&pw),
        None,
    );
    assert!(rep.text().contains("vault: on"), "{}", rep.text());
    let dis = cli(
        h,
        &["--passphrase-stdin", "vault", "disable"],
        Some(&pw),
        None,
    );
    assert_eq!(dis.code(), 0, "{}", dis.text());
    let after = json_of(&plain(h, &["--json", "stats"]));
    assert_eq!(after["total"], before["total"]);
    assert!(plain(h, &["vault", "status"]).text().contains("vault: off"));
}

#[cfg(unix)]
#[test]
fn enable_disable_roundtrip_and_exit_codes() {
    let home = tempfile::tempdir().unwrap();
    let lib = tempfile::tempdir().unwrap();
    make_media(lib.path());
    let h = home.path();
    json_of(&plain(h, &["--json", "scan", lib.path().to_str().unwrap()]));
    let before = json_of(&plain(h, &["--json", "stats"]));
    let st = plain(h, &["vault", "status"]);
    assert_eq!(st.code(), 0);
    assert!(st.text().contains("vault: off"));

    // Too short.
    let short = cli(
        h,
        &[
            "--passphrase-stdin",
            "vault",
            "enable",
            "--i-understand-no-recovery",
        ],
        Some("short\n"),
        None,
    );
    assert_eq!(short.code(), 1);
    assert!(short.text().contains("at least 12"));
    assert!(plain(h, &["--json", "stats"]).code() == 0, "still plain");

    // The new passphrase comes from fd 3 (CRLF stripped), not argv.
    let en = cli(
        h,
        &[
            "--new-passphrase-fd",
            "3",
            "vault",
            "enable",
            "--i-understand-no-recovery",
        ],
        None,
        Some(&format!("{PASS}\r\n")),
    );
    assert_eq!(en.code(), 0, "{}", en.text());
    assert_eq!(plain(h, &["stats"]).code(), 4);

    // Wrong passphrase: 3, on the library and on the vault commands.
    let w = cli(
        h,
        &["--passphrase-stdin", "stats"],
        Some("not the passphrase\n"),
        None,
    );
    assert_eq!(w.code(), 3, "{}", w.text());
    let w = cli(
        h,
        &["--passphrase-stdin", "vault", "verify"],
        Some("not the passphrase\n"),
        None,
    );
    assert_eq!(w.code(), 3);
    // A secret field in command JSON is refused outright.
    let r = cli(
        h,
        &[
            "--passphrase-stdin",
            "run",
            "vault.status",
            &format!("{{\"passphrase\":\"{PASS}\"}}"),
        ],
        Some(&format!("{PASS}\n")),
        None,
    );
    assert_ne!(r.code(), 0);
    assert!(!r.text().contains(PASS));

    // Change the passphrase: old on stdin, new on fd 3.
    let newp = "another-long-passphrase";
    let ch = cli(
        h,
        &[
            "--passphrase-stdin",
            "--new-passphrase-fd",
            "3",
            "vault",
            "change-passphrase",
        ],
        Some(&format!("{PASS}\n")),
        Some(newp),
    );
    assert_eq!(ch.code(), 0, "{}", ch.text());
    let old = cli(
        h,
        &["--passphrase-stdin", "stats"],
        Some(&format!("{PASS}\n")),
        None,
    );
    assert_eq!(old.code(), 3);
    let ok = cli(
        h,
        &["--json", "--passphrase-stdin", "stats"],
        Some(&format!("{newp}\n")),
        None,
    );
    assert_eq!(json_of(&ok)["total"], before["total"]);

    // Disable: back to a plain library with the same rows.
    let dis = cli(
        h,
        &["--passphrase-stdin", "vault", "disable"],
        Some(&format!("{newp}\n")),
        None,
    );
    assert_eq!(dis.code(), 0, "{}", dis.text());
    let after = json_of(&plain(h, &["--json", "stats"]));
    assert_eq!(after["total"], before["total"]);
    assert!(plain(h, &["vault", "status"]).text().contains("vault: off"));
    assert!(!h.join("data").join("library.vault").exists());
}

#[test]
fn damaged_or_missing_header_is_exit_5() {
    let home = tempfile::tempdir().unwrap();
    let lib = tempfile::tempdir().unwrap();
    make_media(lib.path());
    let h = home.path();
    json_of(&plain(h, &["--json", "scan", lib.path().to_str().unwrap()]));
    let en = cli(
        h,
        &[
            "--passphrase-stdin",
            "vault",
            "enable",
            "--i-understand-no-recovery",
        ],
        Some(&format!("{PASS}\n")),
        None,
    );
    assert_eq!(en.code(), 0, "{}", en.text());
    let mut files = Vec::new();
    walk(h, &mut files);
    let header = files
        .into_iter()
        .find(|p| p.extension().is_some_and(|e| e == "vault"))
        .expect("header file");
    let saved = std::fs::read(&header).unwrap();

    // Corrupt: wrong length.
    std::fs::write(&header, &saved[..50]).unwrap();
    let r = cli(
        h,
        &["--passphrase-stdin", "stats"],
        Some(&format!("{PASS}\n")),
        None,
    );
    assert_eq!(r.code(), 5, "{}", r.text());
    assert_eq!(plain(h, &["vault", "status"]).code(), 5);

    // Missing.
    std::fs::remove_file(&header).unwrap();
    let r = cli(
        h,
        &["--passphrase-stdin", "stats"],
        Some(&format!("{PASS}\n")),
        None,
    );
    assert_eq!(r.code(), 5, "{}", r.text());

    // Restored: opens again, nothing was lost.
    std::fs::write(&header, &saved).unwrap();
    let r = cli(
        h,
        &["--json", "--passphrase-stdin", "stats"],
        Some(&format!("{PASS}\n")),
        None,
    );
    assert_eq!(json_of(&r)["total"], 2);
}

#[test]
fn doctor_report_says_locked_without_statistics() {
    let home = tempfile::tempdir().unwrap();
    let lib = tempfile::tempdir().unwrap();
    make_media(lib.path());
    let h = home.path();
    json_of(&plain(h, &["--json", "scan", lib.path().to_str().unwrap()]));
    let rep = plain(h, &["doctor", "--report"]);
    assert!(rep.text().contains("vault: off"), "{}", rep.text());
    assert!(rep.text().contains("items: 2"), "{}", rep.text());
    let en = cli(
        h,
        &[
            "--passphrase-stdin",
            "vault",
            "enable",
            "--i-understand-no-recovery",
        ],
        Some(&format!("{PASS}\n")),
        None,
    );
    assert_eq!(en.code(), 0, "{}", en.text());
    let rep = plain(h, &["doctor", "--report"]);
    let t = rep.text();
    assert!(t.contains("vault: locked"), "{t}");
    assert!(
        t.contains("items: unknown") && t.contains("schema_version: unknown"),
        "{t}"
    );
}

/// Descriptors 0, 1 and 2 are refused before anything is read.
#[test]
fn standard_descriptors_are_refused_as_passphrase_sources() {
    let home = tempfile::tempdir().unwrap();
    for flag in ["--passphrase-fd", "--new-passphrase-fd"] {
        for fd in ["0", "1", "2"] {
            let r = cli(
                home.path(),
                &[flag, fd, "vault", "status"],
                Some("whatever\n"),
                None,
            );
            assert_eq!(r.code(), 1, "{flag} {fd}: {}", r.text());
            assert!(r.text().contains("descriptors 0, 1 and 2"), "{}", r.text());
        }
    }
}

/// A passphrase longer than the shared limit is refused, one at the limit is
/// accepted (stdin, so it runs everywhere).
#[test]
fn passphrase_limit_is_1024_characters() {
    let home = tempfile::tempdir().unwrap();
    let lib = tempfile::tempdir().unwrap();
    make_media(lib.path());
    let h = home.path();
    json_of(&plain(h, &["--json", "scan", lib.path().to_str().unwrap()]));
    let flags = [
        "--passphrase-stdin",
        "vault",
        "enable",
        "--i-understand-no-recovery",
    ];
    let long = format!("{}\n", "a".repeat(1025));
    let r = cli(h, &flags, Some(&long), None);
    assert_eq!(r.code(), 1, "{}", r.text());
    assert!(r.text().contains("too long"));
    assert!(plain(h, &["vault", "status"]).text().contains("vault: off"));
    let ok = format!("{}\n", "a".repeat(1024));
    let r = cli(h, &flags, Some(&ok), None);
    assert_eq!(r.code(), 0, "{}", r.text());
}

/// Under a pseudo-terminal `--passphrase-stdin` cannot set a new passphrase:
/// one silent entry without confirmation is refused with a fixed message and
/// nothing is changed. Needs `script(1)` (util-linux); skipped without it.
#[cfg(target_os = "linux")]
#[test]
fn passphrase_stdin_is_refused_on_a_terminal_for_enable() {
    if Command::new("script").arg("--version").output().is_err() {
        eprintln!("script(1) not available, skipped");
        return;
    }
    let home = tempfile::tempdir().unwrap();
    let lib = tempfile::tempdir().unwrap();
    make_media(lib.path());
    let h = home.path();
    json_of(&plain(h, &["--json", "scan", lib.path().to_str().unwrap()]));
    let exe = env!("CARGO_BIN_EXE_jizzcraft-cli");
    for sub in [
        "vault enable --i-understand-no-recovery",
        "vault change-passphrase",
    ] {
        let line = format!("'{exe}' --home '{}' --passphrase-stdin {sub}", h.display());
        let out = Command::new("script")
            .args(["-qec", &line, "/dev/null"])
            .stdin(Stdio::null())
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout).into_owned();
        assert_eq!(out.status.code(), Some(1), "{sub}: {text}");
        assert!(
            text.contains("use the interactive prompt instead"),
            "{sub}: {text}"
        );
    }
    assert!(plain(h, &["vault", "status"]).text().contains("vault: off"));
}
