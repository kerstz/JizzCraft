//! Kodi-style `.nfo` import: a narrow parser that refuses hostile input, and
//! an importer that is read-only, bounded, dry-run by default and exact.
use jizzcraft_core::fingerprint::{fingerprint_pending, quick_hashes};
use jizzcraft_core::interop::ImportOpts;
use jizzcraft_core::interop::nfo::{NfoReject, import_nfo, parse_nfo, parse_tvshow};
use jizzcraft_core::organize::LabelKind;
use jizzcraft_core::{Db, Filter, Media, scan};
use rusqlite::types::ValueRef;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

fn apply() -> ImportOpts {
    ImportOpts {
        dry_run: false,
        ..Default::default()
    }
}

fn run(db: &mut Db, dir: Option<&Path>, o: &ImportOpts) -> jizzcraft_core::interop::InteropReport {
    import_nfo(db, dir, o, &AtomicBool::new(false)).unwrap()
}

// ---- parser --------------------------------------------------------------------

const MINIMAL: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<movie>
  <title>Harbour at dawn</title>
  <premiered>2020-02-29</premiered>
  <studio>Acme</studio>
  <studio>Other</studio>
  <genre>Drama</genre>
  <tag>blue hour</tag>
  <userrating>8</userrating>
  <playcount>3</playcount>
  <lastplayed>2021-03-04 21:14:09</lastplayed>
  <resume><position>90.5</position><total>1000</total></resume>
  <uniqueid type="imdb" default="true">tt1234567</uniqueid>
  <actor><name>Jane Roe</name><role>x</role><order>0</order></actor>
  <actor><name>Bob</name><order>1</order></actor>
  <plot>ignored</plot>
  <runtime>90</runtime>
</movie>
https://example.invalid/some/page
"#;

#[test]
fn minimal_example() {
    let d = parse_nfo(MINIMAL.as_bytes()).unwrap();
    assert_eq!(d.title.as_deref(), Some("Harbour at dawn"));
    assert_eq!(d.premiered.as_deref(), Some("2020-02-29"));
    assert_eq!(d.studios, vec!["Acme", "Other"]);
    assert_eq!(d.genres, vec!["Drama"]);
    assert_eq!(d.tags, vec!["blue hour"]);
    assert_eq!(d.userrating, Some(8.0));
    assert_eq!(d.playcount, Some(3));
    assert_eq!(
        d.lastplayed,
        Some(1_614_816_000),
        "midnight UTC of 2021-03-04"
    );
    assert_eq!(d.resume_s, Some(90.5));
    assert_eq!(
        d.uniqueids,
        vec![("imdb".to_string(), "tt1234567".to_string())]
    );
    assert_eq!(
        d.actors,
        vec![
            ("Jane Roe".to_string(), Some(0)),
            ("Bob".to_string(), Some(1))
        ]
    );
    assert_eq!(d.ignored, 2, "plot and runtime");
}

#[test]
fn other_roots_year_bom_cdata_and_entities() {
    let d = parse_nfo(b"<musicvideo><title>A</title><year>1999</year></musicvideo>").unwrap();
    assert_eq!((d.title.as_deref(), d.year), (Some("A"), Some(1999)));
    assert!(parse_nfo(b"<episodedetails><title>E</title></episodedetails>").is_ok());
    let mut bom = vec![0xEF, 0xBB, 0xBF];
    bom.extend_from_slice(b"<movie><title>B</title></movie>");
    assert_eq!(parse_nfo(&bom).unwrap().title.as_deref(), Some("B"));
    let d = parse_nfo(b"<movie><title><![CDATA[x < y & z]]></title></movie>").unwrap();
    assert_eq!(d.title.as_deref(), Some("x < y & z"));
    let d = parse_nfo(
        "<movie><title>Tom &amp; Jerry caf&#233; &lt;3 &#x41;</title></movie>".as_bytes(),
    )
    .unwrap();
    assert_eq!(d.title.as_deref(), Some("Tom & Jerry café <3 A"));
    // Control characters never reach a title.
    let d = parse_nfo(b"<movie><title>  a&#7;b  </title></movie>").unwrap();
    assert_eq!(d.title.as_deref(), Some("ab"));
    // Empty elements and unknown children are harmless.
    let d =
        parse_nfo(b"<movie><title/><thumb aspect='x'/><fileinfo><a><b/></a></fileinfo></movie>")
            .unwrap();
    assert!(d.title.is_none());
    assert_eq!(d.ignored, 2);
}

#[test]
fn hostile_input_is_refused() {
    let lol = r#"<?xml version="1.0"?>
<!DOCTYPE lolz [<!ENTITY lol "lol"><!ENTITY lol2 "&lol;&lol;&lol;&lol;&lol;&lol;&lol;&lol;&lol;&lol;">]>
<movie><title>&lol2;</title></movie>"#;
    assert_eq!(parse_nfo(lol.as_bytes()), Err(NfoReject::DocType));
    let xxe = r#"<!DOCTYPE m [<!ENTITY x SYSTEM "file:///etc/passwd">]><movie><title>&x;</title></movie>"#;
    assert_eq!(parse_nfo(xxe.as_bytes()), Err(NfoReject::DocType));
    let big = format!("<movie><title>{}</title></movie>", "a".repeat(2 << 20));
    assert_eq!(parse_nfo(big.as_bytes()), Err(NfoReject::TooBig));
    let deep = format!(
        "<movie>{}{}</movie>",
        "<a>".repeat(10_000),
        "</a>".repeat(10_000)
    );
    assert_eq!(parse_nfo(deep.as_bytes()), Err(NfoReject::TooDeep));
    let many = format!("<movie>{}</movie>", "<tag>x</tag>".repeat(100_000));
    assert!(matches!(
        parse_nfo(many.as_bytes()),
        Err(NfoReject::TooMany | NfoReject::TooBig)
    ));
    let tags = format!("<movie>{}</movie>", "<tag>x</tag>".repeat(300));
    assert_eq!(parse_nfo(tags.as_bytes()), Err(NfoReject::TooMany));
    let actors = format!(
        "<movie>{}</movie>",
        "<actor><name>a</name></actor>".repeat(129)
    );
    assert_eq!(parse_nfo(actors.as_bytes()), Err(NfoReject::TooMany));
    let ids = format!(
        "<movie>{}</movie>",
        "<uniqueid type='a'>1</uniqueid>".repeat(17)
    );
    assert_eq!(parse_nfo(ids.as_bytes()), Err(NfoReject::TooMany));
    assert_eq!(
        parse_nfo(b"<movie><title>&undefined;</title></movie>"),
        Err(NfoReject::Malformed)
    );
    assert_eq!(
        parse_nfo(b"<movie><title>x</movie>"),
        Err(NfoReject::Malformed)
    );
    assert_eq!(parse_nfo(b"<movie><title>x"), Err(NfoReject::Malformed));
    assert_eq!(parse_nfo(&[0xff, 0xfe, b'<']), Err(NfoReject::Malformed));
    assert_eq!(
        parse_nfo(b"<?xml version=\"1.0\" encoding=\"ISO-8859-1\"?><movie/>"),
        Err(NfoReject::Malformed)
    );
    assert_eq!(parse_nfo(b"<html><body/></html>"), Err(NfoReject::NotNfo));
    assert_eq!(parse_nfo(b"just words"), Err(NfoReject::NotNfo));
    assert_eq!(
        parse_nfo(b"https://example.invalid/x\n"),
        Err(NfoReject::UrlOnly)
    );
    assert_eq!(
        parse_nfo(b"\xEF\xBB\xBFhttp://example.invalid/x"),
        Err(NfoReject::UrlOnly)
    );
    // Garbage after the root is ignored, even a second root or a DOCTYPE.
    assert!(parse_nfo(b"<movie><title>t</title></movie><!DOCTYPE x><movie>").is_ok());
    // Out-of-range numbers are dropped, not stored.
    let d = parse_nfo(
        b"<movie><userrating>99</userrating><playcount>-4</playcount><lastplayed>nonsense</lastplayed><year>0</year></movie>",
    )
    .unwrap();
    assert_eq!(
        (d.userrating, d.playcount, d.lastplayed, d.year),
        (None, Some(0), None, None)
    );
}

// ---- importer --------------------------------------------------------------------

struct Lib {
    _t: tempfile::TempDir,
    root: PathBuf,
    db: Db,
}

/// A library of tiny video files with distinct content, hashed.
fn lib(names: &[&str]) -> Lib {
    let t = tempfile::tempdir().unwrap();
    let root = jizzcraft_core::pathbytes::canonical(t.path()).unwrap();
    for (i, n) in names.iter().enumerate() {
        let p = root.join(n);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(
            &p,
            format!("not really a video number {i} {}", "x".repeat(40 + i)),
        )
        .unwrap();
    }
    let mut db = Db::open_in_memory().unwrap();
    scan::import_folder(&mut db, &root).unwrap();
    fingerprint_pending(
        &mut db,
        false,
        None,
        &AtomicBool::new(false),
        &mut |_, _| {},
    )
    .unwrap();
    Lib { _t: t, root, db }
}

fn find(db: &Db, name: &str) -> Media {
    db.query(&Filter {
        include_missing: true,
        ..Default::default()
    })
    .unwrap()
    .into_iter()
    .find(|m| m.name == name)
    .unwrap_or_else(|| panic!("no item {name}"))
}

fn nfo(dir: &Path, name: &str, body: &str) {
    fs::write(dir.join(name), body).unwrap();
}

#[test]
fn sidecar_match_maps_every_field() {
    let mut l = lib(&["a.mp4", "b.mp4"]);
    nfo(&l.root, "a.nfo", MINIMAL);
    let rep = run(
        &mut l.db,
        None,
        &ImportOpts {
            ratings: true,
            history: true,
            ..apply()
        },
    );
    assert_eq!(
        (rep.files_seen, rep.parsed, rep.matched, rep.applied),
        (1, 1, 1, 1),
        "{rep:?}"
    );
    assert_eq!(
        (rep.titles_set, rep.studios_created, rep.people_created),
        (1, 1, 2),
        "{rep:?}"
    );
    assert_eq!(rep.tags_created, 2);
    assert_eq!(rep.ratings_set, 1);
    let m = find(&l.db, "a.mp4");
    assert_eq!(
        (m.rating, m.play_count, m.last_played_at),
        (4, 3, Some(1_614_816_000))
    );
    assert_eq!(m.resume_ms, Some(90_500));
    assert_eq!(m.title.as_deref(), Some("Harbour at dawn"));
    let i = l.db.media_info(m.id).unwrap();
    assert_eq!(i.released.as_deref(), Some("2020-02-29"));
    assert_eq!(i.studio.unwrap().name, "Acme", "extra studios are ignored");
    assert_eq!(
        i.people.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(),
        ["Jane Roe", "Bob"]
    );
    assert_eq!(i.external_ids.len(), 1);
    assert_eq!(l.db.labels(LabelKind::Tag).unwrap().len(), 2);
    // The other item is untouched, favorite and hidden never change.
    let b = find(&l.db, "b.mp4");
    assert!(b.title.is_none() && !b.favorite && !b.hidden && b.rating == 0);
    assert!(!m.favorite && !m.hidden);
    // Applying twice changes nothing more.
    let again = run(
        &mut l.db,
        None,
        &ImportOpts {
            ratings: true,
            history: true,
            ..apply()
        },
    );
    assert_eq!((again.matched, again.applied), (1, 0), "{again:?}");
}

#[test]
fn ratings_and_history_need_their_switches() {
    let mut l = lib(&["a.mp4"]);
    nfo(&l.root, "a.nfo", MINIMAL);
    run(&mut l.db, None, &apply());
    let m = find(&l.db, "a.mp4");
    assert_eq!(
        (m.rating, m.play_count, m.last_played_at, m.resume_ms),
        (0, 0, None, None)
    );
    // Existing history wins; larger count and later date win.
    l.db.set_rating(m.id, 2).unwrap();
    l.db.conn_for_tests()
        .execute(
            "UPDATE media SET play_count = 9, last_played_at = 5000000000, resume_ms = 7 WHERE id = ?1",
            [m.id],
        )
        .unwrap();
    run(
        &mut l.db,
        None,
        &ImportOpts {
            ratings: true,
            history: true,
            ..apply()
        },
    );
    let m = find(&l.db, "a.mp4");
    assert_eq!(
        (m.rating, m.play_count, m.last_played_at, m.resume_ms),
        (2, 9, Some(5_000_000_000), Some(7))
    );
    run(
        &mut l.db,
        None,
        &ImportOpts {
            ratings: true,
            overwrite: true,
            ..apply()
        },
    );
    assert_eq!(
        find(&l.db, "a.mp4").rating,
        4,
        "overwrite replaces the rating"
    );
}

#[test]
fn watched_counts_as_one_play_only_with_history() {
    let body = "<movie><title>T</title><watched>true</watched><lockdata>true</lockdata>\
                <dateadded>2021-03-04 01:01:01</dateadded></movie>";
    let d = parse_nfo(body.as_bytes()).unwrap();
    assert_eq!((d.watched, d.playcount, d.ignored), (Some(true), None, 2));

    let mut l = lib(&["a.mp4", "b.mp4"]);
    nfo(&l.root, "a.nfo", body);
    nfo(
        &l.root,
        "b.nfo",
        &body.replace("<title>", "<playcount>4</playcount><title>"),
    );
    run(&mut l.db, None, &apply());
    assert_eq!(
        find(&l.db, "a.mp4").play_count,
        0,
        "needs the history switch"
    );
    let h = ImportOpts {
        history: true,
        ..apply()
    };
    let dry = run(&mut l.db, None, &ImportOpts { dry_run: true, ..h });
    assert_eq!(find(&l.db, "a.mp4").play_count, 0);
    let real = run(&mut l.db, None, &h);
    assert_eq!(
        (dry.applied, dry.ignored_fields),
        (real.applied, real.ignored_fields)
    );
    assert_eq!(find(&l.db, "a.mp4").play_count, 1);
    assert_eq!(find(&l.db, "b.mp4").play_count, 4, "an explicit count wins");
    // Already watched more often: unchanged.
    l.db.conn_for_tests()
        .execute("UPDATE media SET play_count = 6", [])
        .unwrap();
    run(&mut l.db, None, &h);
    assert_eq!(find(&l.db, "a.mp4").play_count, 6);
}

#[test]
fn fill_versus_overwrite() {
    let mut l = lib(&["a.mp4"]);
    let id = find(&l.db, "a.mp4").id;
    l.db.set_info(
        id,
        jizzcraft_core::meta::Patch::Set("Mine".into()),
        jizzcraft_core::meta::Patch::Set("1999-01-01".into()),
    )
    .unwrap();
    let s = l.db.studio_create("Mine Studio").unwrap();
    l.db.studio_assign(&[id], Some(s)).unwrap();
    nfo(&l.root, "a.nfo", MINIMAL);
    run(&mut l.db, None, &apply());
    let i = l.db.media_info(id).unwrap();
    assert_eq!(i.title.as_deref(), Some("Mine"));
    assert_eq!(i.released.as_deref(), Some("1999-01-01"));
    assert_eq!(i.studio.unwrap().name, "Mine Studio");
    assert_eq!(i.people.len(), 2, "people are added");
    let rep = run(
        &mut l.db,
        None,
        &ImportOpts {
            overwrite: true,
            ..apply()
        },
    );
    let i = l.db.media_info(id).unwrap();
    assert_eq!(i.title.as_deref(), Some("Harbour at dawn"));
    assert_eq!(i.released.as_deref(), Some("2020-02-29"));
    assert_eq!(i.studio.unwrap().name, "Acme");
    assert_eq!(rep.titles_set, 1);
}

#[test]
fn year_is_the_fallback_date() {
    let mut l = lib(&["a.mp4"]);
    nfo(&l.root, "a.nfo", "<movie><year>2011</year></movie>");
    run(&mut l.db, None, &apply());
    let id = find(&l.db, "a.mp4").id;
    assert_eq!(
        l.db.media_info(id).unwrap().released.as_deref(),
        Some("2011-01-01")
    );
}

#[test]
fn matching_rules() {
    let mut l = lib(&[
        "A.MP4",
        "dup.mp4",
        "dup.mkv",
        "solo/only.mkv",
        "pair/x.mp4",
        "pair/y.mp4",
    ]);
    // Stem equal ignoring case.
    nfo(&l.root, "a.nfo", "<movie><title>Upper</title></movie>");
    // Two videos with that stem.
    nfo(&l.root, "dup.nfo", "<movie><title>Twin</title></movie>");
    // movie.nfo beside exactly one video, and beside two.
    nfo(
        &l.root.join("solo"),
        "movie.nfo",
        "<movie><title>Solo</title></movie>",
    );
    nfo(
        &l.root.join("pair"),
        "movie.nfo",
        "<movie><title>Pair</title></movie>",
    );
    // No video of that name.
    nfo(&l.root, "ghost.nfo", "<movie><title>Ghost</title></movie>");
    // Not an nfo at all, and a hidden folder is skipped.
    nfo(&l.root, "junk.nfo", "<html/>");
    fs::create_dir_all(l.root.join(".hidden")).unwrap();
    nfo(
        &l.root.join(".hidden"),
        "a.nfo",
        "<movie><title>Hidden</title></movie>",
    );
    let rep = run(&mut l.db, None, &apply());
    assert_eq!(
        (
            rep.files_seen,
            rep.parsed,
            rep.rejected,
            rep.matched,
            rep.unmatched,
            rep.ambiguous
        ),
        (6, 5, 1, 2, 1, 2),
        "{rep:?}"
    );
    assert_eq!(find(&l.db, "A.MP4").title.as_deref(), Some("Upper"));
    assert_eq!(find(&l.db, "only.mkv").title.as_deref(), Some("Solo"));
    assert!(find(&l.db, "dup.mp4").title.is_none());
    assert!(find(&l.db, "x.mp4").title.is_none());
}

#[test]
fn unique_ids_match_from_a_folder_outside_the_library() {
    let mut l = lib(&["a.mp4", "b.mp4"]);
    let outside = tempfile::tempdir().unwrap();
    let ha = quick_hashes(&l.root.join("a.mp4")).unwrap();
    let hb = quick_hashes(&l.root.join("b.mp4")).unwrap();
    let fp = jizzcraft_core::fingerprint::fp_hex(&ha.fp);
    nfo(
        outside.path(),
        "one.nfo",
        &format!(
            "<movie><title>By fp</title><uniqueid type=\"jizzcraft-fp1\">{fp}</uniqueid></movie>"
        ),
    );
    nfo(
        outside.path(),
        "two.nfo",
        &format!(
            "<movie><title>By hash</title><uniqueid type=\"OSHASH\">{}</uniqueid></movie>",
            jizzcraft_core::fingerprint::oshash_hex(hb.oshash.unwrap())
        ),
    );
    let rep = run(&mut l.db, Some(outside.path()), &apply());
    assert_eq!((rep.matched, rep.applied), (2, 2), "{rep:?}");
    assert_eq!(find(&l.db, "a.mp4").title.as_deref(), Some("By fp"));
    assert_eq!(find(&l.db, "b.mp4").title.as_deref(), Some("By hash"));
    // Our own id is not stored as a foreign one.
    let a = find(&l.db, "a.mp4").id;
    assert!(l.db.media_info(a).unwrap().external_ids.is_empty());
    // Two rows with one hash: ambiguous, not guessed.
    l.db.conn_for_tests()
        .execute(
            "UPDATE media SET oshash = (SELECT oshash FROM media WHERE name = 'b.mp4')",
            [],
        )
        .unwrap();
    l.db.set_info(
        a,
        jizzcraft_core::meta::Patch::Clear,
        jizzcraft_core::meta::Patch::Keep,
    )
    .unwrap();
    fs::remove_file(outside.path().join("one.nfo")).unwrap();
    let rep = run(&mut l.db, Some(outside.path()), &apply());
    assert_eq!((rep.matched, rep.ambiguous), (0, 1), "{rep:?}");
}

#[test]
fn missing_folder_is_an_error() {
    let mut l = lib(&["a.mp4"]);
    let r = import_nfo(
        &mut l.db,
        Some(&l.root.join("nope")),
        &apply(),
        &AtomicBool::new(false),
    );
    assert!(r.is_err());
}

fn dump(db: &Db) -> Vec<(String, Vec<Vec<String>>)> {
    let c = db.conn_for_tests();
    let names: Vec<String> = c
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    names
        .into_iter()
        .map(|n| {
            let mut st = c.prepare(&format!("SELECT * FROM \"{n}\"")).unwrap();
            let cols = st.column_count();
            let rows: Vec<Vec<String>> = st
                .query_map([], |r| {
                    (0..cols)
                        .map(|i| {
                            Ok(match r.get_ref(i)? {
                                ValueRef::Null => "NULL".into(),
                                ValueRef::Integer(v) => v.to_string(),
                                ValueRef::Real(v) => v.to_string(),
                                ValueRef::Text(t) => String::from_utf8_lossy(t).into_owned(),
                                ValueRef::Blob(b) => format!("{b:?}"),
                            })
                        })
                        .collect()
                })
                .unwrap()
                .map(Result::unwrap)
                .collect();
            (n, rows)
        })
        .collect()
}

#[test]
fn dry_run_changes_nothing_and_reports_exactly_what_apply_does() {
    let mut l = lib(&["a.mp4", "b.mp4"]);
    nfo(&l.root, "a.nfo", MINIMAL);
    nfo(
        &l.root,
        "b.nfo",
        "<movie><title>B</title><genre>Drama</genre><set>Box</set><actor><name>Bob</name></actor></movie>",
    );
    let before = dump(&l.db);
    let all = ImportOpts {
        ratings: true,
        history: true,
        playlists: true,
        ..Default::default()
    };
    assert!(all.dry_run, "dry run is the default");
    let dry = run(&mut l.db, None, &all);
    assert!(dry.dry_run && dry.applied == 2, "{dry:?}");
    assert_eq!(dry.playlists_created, 1, "{dry:?}");
    assert_eq!(
        dump(&l.db),
        before,
        "a dry run writes nothing, not even an index row"
    );
    let done = run(
        &mut l.db,
        None,
        &ImportOpts {
            dry_run: false,
            ..all
        },
    );
    assert!(!done.dry_run);
    assert_eq!(
        jizzcraft_core::interop::InteropReport {
            dry_run: false,
            ..dry
        },
        done,
        "same counts"
    );
    assert_ne!(dump(&l.db), before);
}

#[test]
fn names_resolve_through_tag_aliases() {
    let mut l = lib(&["a.mp4"]);
    let t =
        l.db.label_create(LabelKind::Tag, "blowjob", None, None)
            .unwrap();
    l.db.tag_alias_add(t, "blow job").unwrap();
    nfo(
        &l.root,
        "a.nfo",
        "<movie><genre>Blow Job</genre><tag>BLOWJOB</tag><tag>fresh</tag></movie>",
    );
    let rep = run(&mut l.db, None, &apply());
    assert_eq!(rep.tags_created, 1, "{rep:?}");
    let tags: Vec<String> =
        l.db.labels(LabelKind::Tag)
            .unwrap()
            .into_iter()
            .map(|t| t.name)
            .collect();
    assert_eq!(tags.len(), 2);
    assert!(tags.contains(&"blowjob".into()) && tags.contains(&"fresh".into()));
    let a = find(&l.db, "a.mp4").id;
    assert_eq!(l.db.labels_of(a).unwrap().0.len(), 2);
}

#[test]
fn cancel_changes_nothing() {
    let mut l = lib(&["a.mp4"]);
    nfo(&l.root, "a.nfo", MINIMAL);
    let before = dump(&l.db);
    let rep = import_nfo(&mut l.db, None, &apply(), &AtomicBool::new(true)).unwrap();
    assert!(rep.cancelled && rep.applied == 0);
    assert_eq!(dump(&l.db), before);
}

#[test]
fn symlinks_and_oversized_files_are_not_followed_or_read() {
    let mut l = lib(&["a.mp4"]);
    let outside = tempfile::tempdir().unwrap();
    nfo(
        outside.path(),
        "secret.nfo",
        "<movie><title>Leak</title></movie>",
    );
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(outside.path(), l.root.join("link")).unwrap();
        std::os::unix::fs::symlink(outside.path().join("secret.nfo"), l.root.join("a.nfo"))
            .unwrap();
    }
    fs::write(l.root.join("big.nfo"), vec![b'a'; (2 << 20) as usize]).unwrap();
    let rep = run(&mut l.db, None, &apply());
    assert_eq!((rep.files_seen, rep.matched), (1, 0), "{rep:?}");
    assert_eq!(rep.rejected, 1);
    assert!(find(&l.db, "a.mp4").title.is_none());
}

#[test]
fn interop_sources_never_print() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src")
        .join("interop");
    let mut n = 0;
    for e in fs::read_dir(&dir).unwrap().flatten() {
        let text = fs::read_to_string(e.path()).unwrap();
        for bad in ["println!", "eprintln!", "print!(", "dbg!", "debug_log!"] {
            assert!(!text.contains(bad), "{bad} in {:?}", e.file_name());
        }
        n += 1;
    }
    assert!(n >= 2);
}

#[test]
fn set_parses_in_both_forms_and_first_wins() {
    let d = parse_nfo(b"<movie><set>Old style</set><set>Later</set></movie>").unwrap();
    assert_eq!((d.set.as_deref(), d.ignored), (Some("Old style"), 0));
    let d = parse_nfo(b"<movie><set><name>New style</name><overview>x</overview></set></movie>")
        .unwrap();
    assert_eq!((d.set.as_deref(), d.ignored), (Some("New style"), 0));
    assert_eq!(
        parse_tvshow(b"<tvshow><title>Show</title></tvshow>").unwrap(),
        Some("Show".to_string())
    );
    assert_eq!(
        parse_nfo(b"<tvshow><title>Show</title></tvshow>"),
        Err(NfoReject::NotNfo)
    );
    assert_eq!(
        parse_tvshow(b"<movie><title>M</title></movie>"),
        Err(NfoReject::NotNfo)
    );
}

fn playlist_sizes(db: &Db) -> Vec<(String, u64)> {
    db.playlists()
        .unwrap()
        .into_iter()
        .map(|p| (p.name, p.count))
        .collect()
}

#[test]
fn set_needs_switch() {
    let mut l = lib(&["a.mp4"]);
    nfo(
        &l.root,
        "a.nfo",
        "<movie><title>A</title><set>Box</set></movie>",
    );
    let off = run(&mut l.db, None, &apply());
    assert_eq!(
        (off.playlists_created, off.ignored_fields),
        (0, 1),
        "{off:?}"
    );
    assert!(playlist_sizes(&l.db).is_empty());
    let on_opts = ImportOpts {
        playlists: true,
        ..apply()
    };
    let on = run(&mut l.db, None, &on_opts);
    assert_eq!((on.playlists_created, on.ignored_fields), (1, 0), "{on:?}");
    assert_eq!(playlist_sizes(&l.db), vec![("Box".to_string(), 1)]);
    let again = run(&mut l.db, None, &on_opts);
    assert_eq!(
        (again.playlists_created, again.applied),
        (0, 0),
        "{again:?}"
    );
    assert_eq!(playlist_sizes(&l.db), vec![("Box".to_string(), 1)]);
}

fn studio_of(db: &Db, name: &str) -> Option<String> {
    db.media_info(find(db, name).id)
        .unwrap()
        .studio
        .map(|s| s.name)
}

#[test]
fn tvshow_title_fills_studio() {
    let mut l = lib(&["Show/S1/a.mp4", "Show/S1/b.mp4", "Show/S2/c.mp4"]);
    nfo(
        &l.root.join("Show"),
        "tvshow.nfo",
        "<tvshow><title>The Show</title></tvshow>",
    );
    nfo(
        &l.root.join("Show/S1"),
        "a.nfo",
        "<episodedetails><title>A</title></episodedetails>",
    );
    nfo(
        &l.root.join("Show/S1"),
        "b.nfo",
        "<episodedetails><title>B</title><studio>Own</studio></episodedetails>",
    );
    // Outside the walk root: the show file is not looked at.
    // Joined by component: a mixed-separator path is not the stored one on Windows.
    let s2 = l.root.join("Show").join("S2");
    nfo(
        &s2,
        "c.nfo",
        "<episodedetails><title>C</title></episodedetails>",
    );
    let rep = run(&mut l.db, Some(&s2), &apply());
    assert_eq!((rep.matched, rep.studios_from_show), (1, 0), "{rep:?}");
    assert_eq!(studio_of(&l.db, "c.mp4"), None);

    let rep = run(&mut l.db, None, &apply());
    assert_eq!(rep.studios_from_show, 2, "a and c: {rep:?}");
    assert_eq!(rep.rejected, 1, "tvshow.nfo is not an item file: {rep:?}");
    assert_eq!(studio_of(&l.db, "a.mp4").as_deref(), Some("The Show"));
    assert_eq!(studio_of(&l.db, "b.mp4").as_deref(), Some("Own"));
    assert_eq!(studio_of(&l.db, "c.mp4").as_deref(), Some("The Show"));
}

#[cfg(unix)]
#[test]
fn tvshow_symlink_is_ignored() {
    let mut l = lib(&["Show/a.mp4"]);
    let outside = tempfile::tempdir().unwrap();
    nfo(
        outside.path(),
        "tvshow.nfo",
        "<tvshow><title>Leak</title></tvshow>",
    );
    std::os::unix::fs::symlink(
        outside.path().join("tvshow.nfo"),
        l.root.join("Show/tvshow.nfo"),
    )
    .unwrap();
    nfo(
        &l.root.join("Show"),
        "a.nfo",
        "<movie><title>A</title></movie>",
    );
    let rep = run(&mut l.db, None, &apply());
    assert_eq!((rep.matched, rep.studios_from_show), (1, 0), "{rep:?}");
    assert_eq!(studio_of(&l.db, "a.mp4"), None);
}
