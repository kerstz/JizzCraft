//! Playlists, categories and tags: rules, ordering, filters, portability, scale.
use jizzcraft_core::fingerprint::fingerprint_pending;
use jizzcraft_core::organize::{LabelKind, M3uOpts, PlaylistSort};
use jizzcraft_core::portability::{ExportOpts, export_meta, export_meta_ext, import_meta};
use jizzcraft_core::{Db, Error, Filter, OrgFilter, Registry, SortKey, register_core, scan};
use serde_json::{Value, json};
use std::fs;
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

// ---- helpers ---------------------------------------------------------------

/// In-memory library with `n` synthetic media rows (ids 1..=n), inserted raw.
fn synthetic(n: i64) -> Db {
    let db = Db::open_in_memory().unwrap();
    let c = db.conn_for_tests();
    c.execute_batch("BEGIN; INSERT INTO folders(id, path, added_at) VALUES (1, x'2f6c', 0);")
        .unwrap();
    {
        let mut st = c
            .prepare(
                "INSERT INTO media(id, folder_id, path, name, name_lc, kind, size, mtime, added_at, dir, duration_ms)
                 VALUES (?1, 1, ?2, ?3, ?3, 0, 1, 0, 0, x'2f6c', ?1)",
            )
            .unwrap();
        for i in 1..=n {
            st.execute(rusqlite::params![
                i,
                format!("/l/{i}.png").as_bytes(),
                format!("m{i:06}.png")
            ])
            .unwrap();
        }
    }
    c.execute_batch("COMMIT").unwrap();
    db
}

fn media_count(db: &Db) -> i64 {
    db.conn_for_tests()
        .query_row("SELECT COUNT(*) FROM media", [], |r| r.get(0))
        .unwrap()
}

fn ids(db: &Db, f: &Filter) -> Vec<i64> {
    db.query(f).unwrap().into_iter().map(|m| m.id).collect()
}

fn invalid<T: std::fmt::Debug>(r: Result<T, Error>) {
    match r {
        Err(Error::Invalid(_)) => {}
        other => panic!("expected Invalid, got {other:?}"),
    }
}

fn png(path: &Path, w: u32, salt: u8) {
    image::RgbImage::from_fn(w, w, |x, y| image::Rgb([x as u8 ^ salt, y as u8, salt]))
        .save(path)
        .unwrap();
}

fn file_lib(root: &Path) -> Db {
    png(&root.join("a.png"), 33, 1);
    png(&root.join("b.png"), 34, 2);
    png(&root.join("c.png"), 35, 3);
    let mut db = Db::open_in_memory().unwrap();
    scan::import_folder(&mut db, root).unwrap();
    let c = AtomicBool::new(false);
    fingerprint_pending(&mut db, false, None, &c, &mut |_, _| {}).unwrap();
    db
}

fn id_of(db: &Db, name: &str) -> i64 {
    db.query(&Filter::default())
        .unwrap()
        .into_iter()
        .find(|m| m.name == name)
        .unwrap()
        .id
}

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

// ---- names and rules ---------------------------------------------------------

#[test]
fn names_follow_the_rules_and_are_unique() {
    let mut db = synthetic(3);
    let a = db.playlist_create("  Faves  ", Some(0xFF8800)).unwrap();
    invalid(db.playlist_create("faves", None));
    invalid(db.playlist_create("FAVES", None));
    invalid(db.playlist_create("", None));
    invalid(db.playlist_create("   ", None));
    invalid(db.playlist_create("bad\nname", None));
    invalid(db.playlist_create(&"x".repeat(61), None));
    db.playlist_create(&"é".repeat(60), None).unwrap();
    invalid(db.playlist_create("ok", Some(0x1_000000)));
    let b = db.playlist_create("Other", None).unwrap();
    invalid(db.playlist_rename(b, "FAVES"));
    db.playlist_rename(a, "FAVES").unwrap(); // case-only rename of itself
    let list = db.playlists().unwrap();
    assert_eq!(list.iter().find(|p| p.id == a).unwrap().name, "FAVES");
    assert_eq!(
        list.iter().find(|p| p.id == a).unwrap().color,
        Some(0xFF8800)
    );

    let t = db.label_create(LabelKind::Tag, "Ünï", None, None).unwrap();
    invalid(db.label_create(LabelKind::Tag, "ünï", None, None));
    // The same name is fine as a category, and under different parents.
    let c1 = db
        .label_create(LabelKind::Category, "ünï", None, None)
        .unwrap();
    let c2 = db
        .label_create(LabelKind::Category, "x", None, None)
        .unwrap();
    db.label_create(LabelKind::Category, "ünï", Some(c2), None)
        .unwrap();
    invalid(db.label_create(LabelKind::Category, "ÜNÏ", Some(c2), None));
    invalid(db.label_create(LabelKind::Category, "a/b", None, None));
    invalid(db.label_create(LabelKind::Tag, "child", Some(c1), None));
    assert!(matches!(
        db.label_rename(LabelKind::Tag, c1, "nope"),
        Err(Error::NotFound(_))
    )); // wrong kind
    db.label_rename(LabelKind::Tag, t, "renamed").unwrap();
}

#[test]
fn counts_are_capped() {
    let mut db = synthetic(3);
    let c = db.conn_for_tests();
    c.execute_batch(
        "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 10000)
         INSERT INTO playlists(name, name_lc, created_at, updated_at)
         SELECT 'p' || i, 'p' || i, 0, 0 FROM n;",
    )
    .unwrap();
    invalid(db.playlist_create("one more", None));
    let too_many: Vec<i64> = (1..=10_001).collect();
    let id = db.playlists().unwrap()[0].id;
    invalid(db.playlist_add(id, &too_many, None));
}

// ---- cascade and relinking ---------------------------------------------------

#[test]
fn deleting_playlists_and_labels_leaves_media_intact() {
    let mut db = synthetic(5);
    let p = db.playlist_create("p", None).unwrap();
    db.playlist_add(p, &[1, 2, 3], None).unwrap();
    let cat = db
        .label_create(LabelKind::Category, "A", None, None)
        .unwrap();
    let sub = db
        .label_create(LabelKind::Category, "B", Some(cat), None)
        .unwrap();
    let tag = db.label_create(LabelKind::Tag, "t", None, None).unwrap();
    db.label_assign(LabelKind::Category, &[cat, sub], &[1, 2], true)
        .unwrap();
    db.label_assign(LabelKind::Tag, &[tag], &[1, 4], true)
        .unwrap();

    assert_eq!(db.playlist_delete(p).unwrap(), 3);
    assert_eq!(media_count(&db), 5);
    assert_eq!(db.label_delete(LabelKind::Tag, tag).unwrap(), 2);
    assert_eq!(db.label_delete(LabelKind::Category, cat).unwrap(), 2);
    assert_eq!(media_count(&db), 5);
    // Child moved up to the root.
    let cats = db.labels(LabelKind::Category).unwrap();
    assert_eq!(cats.len(), 1);
    assert_eq!(
        (cats[0].id, cats[0].parent_id, cats[0].depth),
        (sub, None, 0)
    );

    // Deleting media cascades its memberships away.
    let p2 = db.playlist_create("p2", None).unwrap();
    db.playlist_add(p2, &[1, 2], None).unwrap();
    db.conn_for_tests()
        .execute("DELETE FROM media WHERE id = 1", [])
        .unwrap();
    assert_eq!(db.playlist_item_ids(p2).unwrap(), vec![2]);
    let n: i64 = db
        .conn_for_tests()
        .query_row(
            "SELECT COUNT(*) FROM media_labels WHERE media_id = 1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 0);
}

#[test]
fn merge_into_carries_memberships() {
    let mut db = synthetic(4);
    let p = db.playlist_create("p", None).unwrap();
    let q = db.playlist_create("q", None).unwrap();
    db.playlist_add(p, &[1, 2, 3], None).unwrap();
    db.playlist_add(q, &[1, 4], None).unwrap();
    let t = db.label_create(LabelKind::Tag, "t", None, None).unwrap();
    let u = db.label_create(LabelKind::Tag, "u", None, None).unwrap();
    db.label_assign(LabelKind::Tag, &[t], &[1, 2], true)
        .unwrap();
    db.label_assign(LabelKind::Tag, &[u], &[1], true).unwrap();
    // `from` = 1 (in p at index 0, q at 0), `to` = 2 (already in p and tag t).
    db.merge_into(1, 2).unwrap();
    assert_eq!(
        db.playlist_item_ids(p).unwrap(),
        vec![2, 3],
        "already a member: kept as is"
    );
    assert_eq!(
        db.playlist_item_ids(q).unwrap(),
        vec![2, 4],
        "takes the position of `from`"
    );
    let (tags, _) = db.labels_of(2).unwrap();
    let mut names: Vec<_> = tags.iter().map(|t| t.name.as_str()).collect();
    names.sort();
    assert_eq!(names, ["t", "u"]);
}

#[test]
fn relink_keeps_memberships() {
    let t = tempfile::tempdir().unwrap();
    let mut db = file_lib(t.path());
    let a = id_of(&db, "a.png");
    let p = db.playlist_create("p", None).unwrap();
    db.playlist_add(p, &[a], None).unwrap();
    let tag = db.label_create(LabelKind::Tag, "t", None, None).unwrap();
    db.label_assign(LabelKind::Tag, &[tag], &[a], true).unwrap();

    fs::create_dir(t.path().join("moved")).unwrap();
    fs::rename(t.path().join("a.png"), t.path().join("moved/a2.png")).unwrap();
    let rep = scan::rescan_all(&mut db).unwrap();
    assert_eq!(rep.relinked, 1, "{rep:?}");
    assert_eq!(db.playlist_item_ids(p).unwrap(), vec![a]);
    assert_eq!(db.labels_of(a).unwrap().0.len(), 1);
}

// ---- categories ----------------------------------------------------------------

#[test]
fn category_cycles_depth_and_reparenting() {
    let mut db = synthetic(1);
    let k = LabelKind::Category;
    let a = db.label_create(k, "a", None, None).unwrap();
    let b = db.label_create(k, "b", Some(a), None).unwrap();
    let c = db.label_create(k, "c", Some(b), None).unwrap();
    let d = db.label_create(k, "d", Some(c), None).unwrap();
    invalid(db.label_create(k, "e", Some(d), None)); // depth 5
    invalid(db.category_move(a, Some(a)));
    invalid(db.category_move(a, Some(d))); // cycle
    invalid(db.category_move(b, Some(c))); // cycle
    // Moving the 3-level subtree b>c>d under d's depth-4 slot is too deep.
    let x = db.label_create(k, "x", None, None).unwrap();
    let y = db.label_create(k, "y", Some(x), None).unwrap();
    invalid(db.category_move(b, Some(y))); // y is depth 2, b has height 3 -> 5
    db.category_move(c, Some(x)).unwrap(); // c>d under x: depth 3
    db.category_move(d, None).unwrap();
    let list = db.labels(k).unwrap();
    let by = |n: &str| list.iter().find(|l| l.name == n).unwrap().clone();
    assert_eq!((by("c").depth, by("c").parent_id), (1, Some(x)));
    assert_eq!(by("d").depth, 0);
    assert_eq!(by("y").path, "x/y");
    // Name collision on re-parent is refused.
    let a2 = db.label_create(k, "c", None, None).unwrap();
    invalid(db.category_move(a2, Some(x)));
    // Merging a category into its own subtree is refused.
    invalid(db.label_merge(k, x, y));
    // Tree order: parents before children, depth-first.
    let order: Vec<&str> = list.iter().map(|l| l.name.as_str()).collect();
    let xi = order.iter().position(|n| *n == "x").unwrap();
    assert!(order[xi + 1..].contains(&"c"));
}

#[test]
fn tag_and_category_merge() {
    let mut db = synthetic(5);
    let t1 = db.label_create(LabelKind::Tag, "one", None, None).unwrap();
    let t2 = db.label_create(LabelKind::Tag, "two", None, None).unwrap();
    db.label_assign(LabelKind::Tag, &[t1], &[1, 2, 3], true)
        .unwrap();
    db.label_assign(LabelKind::Tag, &[t2], &[3, 4], true)
        .unwrap();
    assert_eq!(
        db.label_merge(LabelKind::Tag, t1, t2).unwrap(),
        2,
        "3 was already there"
    );
    let tags = db.labels(LabelKind::Tag).unwrap();
    assert_eq!(tags.len(), 1);
    assert_eq!((tags[0].id, tags[0].count), (t2, 4));
    invalid(db.label_merge(LabelKind::Tag, t2, t2));

    let k = LabelKind::Category;
    let a = db.label_create(k, "a", None, None).unwrap();
    let ab = db.label_create(k, "ab", Some(a), None).unwrap();
    let z = db.label_create(k, "z", None, None).unwrap();
    db.label_assign(k, &[ab], &[1], true).unwrap();
    db.label_assign(k, &[a], &[2], true).unwrap();
    db.label_merge(k, a, z).unwrap();
    let list = db.labels(k).unwrap();
    assert_eq!(list.len(), 2);
    let ab = list.iter().find(|l| l.name == "ab").unwrap();
    assert_eq!(ab.parent_id, Some(z));
    let z = list.iter().find(|l| l.id == z).unwrap();
    assert_eq!((z.count, z.total), (1, 2));
}

#[test]
fn toggle_and_assign() {
    let mut db = synthetic(3);
    let t = db.label_create(LabelKind::Tag, "t", None, None).unwrap();
    assert_eq!(
        db.label_toggle(LabelKind::Tag, t, &[1, 2]).unwrap(),
        (true, 2)
    );
    // Partially tagged: toggling tags the rest.
    assert_eq!(
        db.label_toggle(LabelKind::Tag, t, &[2, 3]).unwrap(),
        (true, 1)
    );
    assert_eq!(
        db.label_toggle(LabelKind::Tag, t, &[1, 2, 3]).unwrap(),
        (false, 3)
    );
    // Unknown media are ignored; unknown labels are errors.
    assert_eq!(
        db.label_assign(LabelKind::Tag, &[t], &[1, 999], true)
            .unwrap(),
        1
    );
    assert!(
        db.label_assign(LabelKind::Tag, &[12345], &[1], true)
            .is_err()
    );
}

// ---- filters -----------------------------------------------------------------------

fn tree_lib() -> (Db, [i64; 5], [i64; 2]) {
    let mut db = synthetic(9);
    let k = LabelKind::Category;
    let a = db.label_create(k, "A", None, None).unwrap();
    let b = db.label_create(k, "B", Some(a), None).unwrap();
    let c = db.label_create(k, "C", Some(b), None).unwrap();
    let d = db.label_create(k, "D", None, None).unwrap();
    let e = db.label_create(k, "E", Some(d), None).unwrap();
    let x = db.label_create(LabelKind::Tag, "x", None, None).unwrap();
    let y = db.label_create(LabelKind::Tag, "y", None, None).unwrap();
    db.label_assign(k, &[a], &[1], true).unwrap();
    db.label_assign(k, &[b], &[2, 5], true).unwrap();
    db.label_assign(k, &[c], &[3], true).unwrap();
    db.label_assign(k, &[d], &[4, 5], true).unwrap();
    db.label_assign(k, &[e], &[6], true).unwrap();
    db.label_assign(LabelKind::Tag, &[x], &[6, 7], true)
        .unwrap();
    db.label_assign(LabelKind::Tag, &[y], &[7], true).unwrap();
    (db, [a, b, c, d, e], [x, y])
}

fn org(f: impl FnOnce(&mut OrgFilter)) -> Filter {
    let mut o = OrgFilter::default();
    f(&mut o);
    Filter {
        org: o,
        ..Default::default()
    }
}

#[test]
fn filter_all_any_none_and_subtree() {
    let (db, [a, b, c, d, _e], [x, y]) = tree_lib();
    // A includes its whole subtree (A, B, C).
    assert_eq!(ids(&db, &org(|o| o.labels_all = vec![a])), [1, 2, 3, 5]);
    assert_eq!(ids(&db, &org(|o| o.labels_all = vec![b])), [2, 3, 5]);
    assert_eq!(ids(&db, &org(|o| o.labels_all = vec![c])), [3]);
    // All: groups ANDed.
    assert_eq!(ids(&db, &org(|o| o.labels_all = vec![a, d])), [5]);
    // Any.
    assert_eq!(ids(&db, &org(|o| o.labels_any = vec![c, d])), [3, 4, 5, 6]);
    // None.
    assert_eq!(ids(&db, &org(|o| o.labels_none = vec![a])), [4, 6, 7, 8, 9]);
    // Combined with ordinary conditions.
    let mut f = org(|o| o.labels_any = vec![a, d]);
    f.name = Some("m000005".into());
    assert_eq!(ids(&db, &f), [5]);
    // Tags.
    assert_eq!(ids(&db, &org(|o| o.labels_all = vec![x, y])), [7]);
    assert_eq!(ids(&db, &org(|o| o.untagged = true)), [1, 2, 3, 4, 5, 8, 9]);
    assert_eq!(ids(&db, &org(|o| o.uncategorized = true)), [7, 8, 9]);
    // Count agrees with query.
    assert_eq!(db.count(&org(|o| o.labels_all = vec![a])).unwrap(), 4);
    // Deleting a label stops it matching without erroring.
    assert!(ids(&db, &org(|o| o.labels_all = vec![9999])).is_empty());
}

#[test]
fn filter_json_is_whitelisted_and_round_trips() {
    let ok = json!({
        "playlist": 3, "labels_all": [1, 2], "labels_any": [4], "labels_none": [5],
        "untagged": true, "uncategorized": true, "sort": "position", "desc": true
    });
    let f = Filter::from_json(&ok).unwrap();
    assert_eq!(f.sort, SortKey::Position);
    assert_eq!(f.org.playlist, Some(3));
    let j = f.to_json();
    assert_eq!(Filter::from_json(&j).unwrap().to_json(), j);
    for k in [
        "playlist",
        "labels_all",
        "labels_any",
        "labels_none",
        "untagged",
        "uncategorized",
    ] {
        assert!(j.get(k).is_some(), "{k}");
    }

    // Hostile values never parse, so they never reach SQL.
    for bad in [
        json!({ "labels_all": ["1) OR 1=1 --"] }),
        json!({ "labels_any": "1" }),
        json!({ "labels_none": [-1] }),
        json!({ "labels_none": [1.5] }),
        json!({ "playlist": "1; DROP TABLE media" }),
        json!({ "playlist": 0 }),
        json!({ "untagged": "yes" }),
        json!({ "labels_all": (1..=65).collect::<Vec<i64>>() }),
        json!({ "sort": "position" }),
        json!({ "sort": "pos; DROP TABLE media" }),
        json!({ "tags": [1] }),
    ] {
        assert!(Filter::from_json(&bad).is_err(), "{bad}");
    }
    // 64 is the limit.
    assert!(Filter::from_json(&json!({ "labels_all": (1..=64).collect::<Vec<i64>>() })).is_ok());

    // Saved views keep the new keys.
    let db = synthetic(1);
    db.save_view("smart", &f).unwrap();
    let back = db.view("smart").unwrap().unwrap();
    assert_eq!(back.spec, j);
}

#[test]
fn position_sort_orders_by_playlist() {
    let mut db = synthetic(6);
    let p = db.playlist_create("p", None).unwrap();
    db.playlist_add(p, &[5, 2, 6, 1], None).unwrap();
    db.playlist_add(p, &[4], Some(2)).unwrap();
    let f = db.playlist_filter(p, None, 0);
    assert_eq!(ids(&db, &f), [5, 4, 2, 6, 1]);
    let desc = Filter {
        desc: true,
        ..f.clone()
    };
    assert_eq!(ids(&db, &desc), [1, 6, 2, 4, 5]);
    let page = Filter {
        limit: Some(2),
        offset: 1,
        ..f.clone()
    };
    assert_eq!(ids(&db, &page), [4, 2]);
    // Other conditions still apply.
    let named = Filter {
        name: Some("m000006".into()),
        ..f
    };
    assert_eq!(ids(&db, &named), [6]);
    assert_eq!(db.count(&db.playlist_filter(p, None, 0)).unwrap(), 5);
}

// ---- ordering ---------------------------------------------------------------------

#[test]
fn playlist_rules() {
    let mut db = synthetic(5);
    let p = db.playlist_create("p", None).unwrap();
    assert_eq!(db.playlist_add(p, &[3, 1, 3, 99], None).unwrap(), (2, 2));
    assert_eq!(db.playlist_add(p, &[1, 2], None).unwrap(), (1, 1));
    assert_eq!(db.playlist_item_ids(p).unwrap(), [3, 1, 2]);
    invalid(db.playlist_add(p, &[4], Some(5))); // before is not a member
    invalid(db.playlist_move(p, &[1], Some(1)));
    assert_eq!(db.playlist_move(p, &[2, 99, 3], None).unwrap(), 2);
    assert_eq!(db.playlist_item_ids(p).unwrap(), [1, 2, 3]);
    assert_eq!(db.playlist_move(p, &[3], Some(1)).unwrap(), 1);
    assert_eq!(db.playlist_item_ids(p).unwrap(), [3, 1, 2]);
    assert_eq!(db.playlist_remove(p, &[1, 1, 77]).unwrap(), 1);
    assert_eq!(db.playlists_of(&[3, 5]).unwrap(), vec![p]);
    assert!(db.playlists_of(&[5]).unwrap().is_empty());

    // A failed operation changes nothing.
    let before = db.playlist_item_ids(p).unwrap();
    invalid(db.playlist_move(p, &[2], Some(4)));
    assert_eq!(db.playlist_item_ids(p).unwrap(), before);

    // Sorting.
    db.playlist_add(p, &[4, 5], None).unwrap();
    db.playlist_sort(p, PlaylistSort::Name, true, 0).unwrap();
    assert_eq!(db.playlist_item_ids(p).unwrap(), [5, 4, 3, 2]);
    db.playlist_sort(p, PlaylistSort::Duration, false, 0)
        .unwrap();
    assert_eq!(db.playlist_item_ids(p).unwrap(), [2, 3, 4, 5]);
    db.playlist_sort(p, PlaylistSort::Random, false, 7).unwrap();
    let a = db.playlist_item_ids(p).unwrap();
    db.playlist_sort(p, PlaylistSort::Random, false, 7).unwrap();
    assert_eq!(a, db.playlist_item_ids(p).unwrap(), "seeded");
    let mut s = a.clone();
    s.sort();
    assert_eq!(s, [2, 3, 4, 5]);
}

#[test]
fn ordering_property_against_a_vec_model() {
    const MEDIA: i64 = 300;
    let mut db = synthetic(MEDIA);
    let p = db.playlist_create("p", None).unwrap();
    let mut model: Vec<i64> = Vec::new();
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);

    for step in 0..10_000 {
        let k = 1 + rng.below(4);
        let pick: Vec<i64> = (0..k)
            .map(|_| 1 + rng.below(MEDIA as usize) as i64)
            .collect();
        // Half of the inserts target the head: that is what exhausts gaps.
        let before_pick = |rng: &mut Rng, model: &[i64], exclude: &[i64]| -> Option<i64> {
            let cands: Vec<i64> = model
                .iter()
                .copied()
                .filter(|m| !exclude.contains(m))
                .collect();
            if cands.is_empty() || rng.below(4) == 0 {
                return None;
            }
            if rng.below(2) == 0 {
                Some(cands[0])
            } else {
                Some(cands[rng.below(cands.len())])
            }
        };
        match rng.below(10) {
            0..=4 => {
                let before = before_pick(&mut rng, &model, &[]);
                let mut fresh: Vec<i64> = Vec::new();
                for m in &pick {
                    if !model.contains(m) && !fresh.contains(m) {
                        fresh.push(*m);
                    }
                }
                let (added, skipped) = db.playlist_add(p, &pick, before).unwrap();
                assert_eq!(
                    (added as usize, skipped as usize),
                    (fresh.len(), pick.len() - fresh.len())
                );
                let at =
                    before.map_or(model.len(), |b| model.iter().position(|m| *m == b).unwrap());
                model.splice(at..at, fresh);
            }
            5..=7 => {
                let mut moved: Vec<i64> = Vec::new();
                for m in &pick {
                    if model.contains(m) && !moved.contains(m) {
                        moved.push(*m);
                    }
                }
                let before = before_pick(&mut rng, &model, &moved);
                let n = db.playlist_move(p, &pick, before).unwrap();
                assert_eq!(n as usize, moved.len());
                model.retain(|m| !moved.contains(m));
                let at =
                    before.map_or(model.len(), |b| model.iter().position(|m| *m == b).unwrap());
                model.splice(at..at, moved);
            }
            _ => {
                let mut gone = 0;
                for m in &pick {
                    if let Some(i) = model.iter().position(|x| x == m) {
                        model.remove(i);
                        gone += 1;
                    }
                }
                let mut uniq = pick.clone();
                uniq.sort();
                uniq.dedup();
                assert_eq!(db.playlist_remove(p, &uniq).unwrap(), gone);
            }
        }
        assert_eq!(db.playlist_item_ids(p).unwrap(), model, "step {step}");
        if step % 100 == 0 {
            // Total order: positions strictly increase, so no tie-break is needed.
            let pos: Vec<i64> = {
                let mut st = db
                    .conn_for_tests()
                    .prepare("SELECT pos FROM playlist_items WHERE playlist_id = ?1 ORDER BY pos")
                    .unwrap();
                st.query_map([p], |r| r.get(0))
                    .unwrap()
                    .map(|r| r.unwrap())
                    .collect()
            };
            assert!(
                pos.windows(2).all(|w| w[0] < w[1]),
                "step {step}: positions tie"
            );
            assert!(pos.iter().all(|v| *v > 0));
        }
    }
    assert!(model.len() > 10, "the run exercised a non-trivial list");

    // Explicit renumber gives the canonical grid and keeps the order.
    db.playlist_renumber(p).unwrap();
    let pos: Vec<i64> = {
        let mut st = db
            .conn_for_tests()
            .prepare("SELECT pos FROM playlist_items WHERE playlist_id = ?1 ORDER BY pos")
            .unwrap();
        st.query_map([p], |r| r.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect()
    };
    let want: Vec<i64> = (1..=model.len() as i64).map(|i| i * 1024).collect();
    assert_eq!(pos, want);
    assert_eq!(db.playlist_item_ids(p).unwrap(), model);
}

#[test]
fn head_inserts_force_renumbering_and_stay_ordered() {
    let mut db = synthetic(400);
    let p = db.playlist_create("p", None).unwrap();
    db.playlist_add(p, &[400], None).unwrap();
    // Always insert right before the current head: halves the gap each time.
    for i in (1..400).rev() {
        let head = db.playlist_item_ids(p).unwrap()[0];
        db.playlist_add(p, &[i], Some(head)).unwrap();
    }
    let got = db.playlist_item_ids(p).unwrap();
    assert_eq!(got, (1..=400).collect::<Vec<i64>>());
    // A block bigger than the fresh grid's gap still fits.
    let q = db.playlist_create("q", None).unwrap();
    db.playlist_add(q, &[1, 2], None).unwrap();
    let block: Vec<i64> = (3..=400).collect();
    db.playlist_add(q, &block, Some(1)).unwrap();
    let mut want = block.clone();
    want.extend([1, 2]);
    assert_eq!(db.playlist_item_ids(q).unwrap(), want);
}

// ---- portability ---------------------------------------------------------------------

#[test]
fn export_v2_round_trip_and_v1_import() {
    let t = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let src = file_lib(t.path());
    let mut src = src;
    let (a, b, c) = (
        id_of(&src, "a.png"),
        id_of(&src, "b.png"),
        id_of(&src, "c.png"),
    );
    let cat = LabelKind::Category;
    let work_cat = src.label_create(cat, "Work", None, Some(0x123456)).unwrap();
    let sub = src.label_create(cat, "Sub", Some(work_cat), None).unwrap();
    let secret = src
        .label_create(LabelKind::Tag, "secret", None, None)
        .unwrap();
    let tag = src
        .label_create(LabelKind::Tag, "Fun", None, Some(0xABCDEF))
        .unwrap();
    src.label_set(LabelKind::Tag, secret, None, Some(true), None)
        .unwrap();
    src.label_assign(cat, &[sub], &[a], true).unwrap();
    src.label_assign(LabelKind::Tag, &[tag], &[a, b], true)
        .unwrap();
    src.label_assign(LabelKind::Tag, &[secret], &[c], true)
        .unwrap();
    let p = src.playlist_create("Evening", Some(0x00FF00)).unwrap();
    src.playlist_add(p, &[c, a, b], None).unwrap();
    let hidden = src.playlist_create("Hidden one", None).unwrap();
    src.playlist_set(hidden, None, Some(true), None).unwrap();
    src.playlist_add(hidden, &[a], None).unwrap();

    let out = work.path().join("e.json");
    let rep = export_meta(&src, &out, &ExportOpts::default()).unwrap();
    assert_eq!(rep.playlists, 1, "private playlist excluded");
    let text = fs::read_to_string(&out).unwrap();
    let doc: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(doc["version"], 3);
    assert!(!text.contains("secret"), "private tag leaked");
    assert!(!text.contains("Hidden one"));
    let item_a = doc["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["categories"] == json!(["Work/Sub"]))
        .unwrap();
    assert_eq!(item_a["tags"], json!(["Fun"]));
    assert_eq!(doc["playlists"][0]["name"], "Evening");
    assert_eq!(doc["playlists"][0]["items"].as_array().unwrap().len(), 3);

    // Import into a fresh library over the same files.
    let mut dst = file_lib(t.path());
    let rep = import_meta(&mut dst, &out, false).unwrap();
    assert!(rep.playlists == 1 && rep.labels >= 3, "{rep:?}");
    let pl = dst.playlists().unwrap();
    assert_eq!(pl.len(), 1);
    assert_eq!(
        (pl[0].name.as_str(), pl[0].color),
        ("Evening", Some(0x00FF00))
    );
    let names: Vec<String> = dst
        .playlist_items(pl[0].id, None, 0)
        .unwrap()
        .into_iter()
        .map(|m| m.name)
        .collect();
    assert_eq!(names, ["c.png", "a.png", "b.png"], "order kept");
    let da = id_of(&dst, "a.png");
    let (tags, cats) = dst.labels_of(da).unwrap();
    assert_eq!(tags[0].name, "Fun");
    assert_eq!(tags[0].color, Some(0xABCDEF));
    assert_eq!(cats[0].path, "Work/Sub");
    // Merging again changes nothing and adds nothing.
    import_meta(&mut dst, &out, false).unwrap();
    assert_eq!(dst.playlist_len(pl[0].id).unwrap(), 3);
    assert_eq!(dst.playlists().unwrap().len(), 1);

    // Existing playlist of the same (case-insensitive) name: missing items are
    // appended in file order, existing order is kept.
    let mut dst2 = file_lib(t.path());
    let ex = dst2.playlist_create("EVENING", None).unwrap();
    let (db_, db_c) = (id_of(&dst2, "b.png"), id_of(&dst2, "c.png"));
    dst2.playlist_add(ex, &[db_], None).unwrap();
    import_meta(&mut dst2, &out, false).unwrap();
    assert_eq!(dst2.playlists().unwrap().len(), 1);
    let order = dst2.playlist_item_ids(ex).unwrap();
    assert_eq!(order[0], db_);
    assert_eq!(order[1..], [db_c, id_of(&dst2, "a.png")]);

    // include_private exports and re-imports the private flag.
    let out2 = work.path().join("e2.json");
    export_meta_ext(&src, &out2, &ExportOpts::default(), true).unwrap();
    assert!(fs::read_to_string(&out2).unwrap().contains("secret"));
    let mut dst3 = file_lib(t.path());
    import_meta(&mut dst3, &out2, false).unwrap();
    let st = dst3
        .labels(LabelKind::Tag)
        .unwrap()
        .into_iter()
        .find(|l| l.name == "secret")
        .unwrap();
    assert!(st.private, "private flag survives the round trip");
    assert!(dst3.playlists().unwrap().iter().any(|p| p.private));

    // Version 1 files still import.
    let v1 = work.path().join("v1.json");
    let fp = doc["items"][0]["fp"].clone();
    let size = doc["items"][0]["size"].clone();
    fs::write(
        &v1,
        json!({"format":"jizzcraft-meta","version":1,"items":[
            {"fp": fp, "size": size, "favorite": true, "rating": 4, "hidden": false, "markers": []}
        ],"views":[]})
        .to_string(),
    )
    .unwrap();
    let mut dst4 = file_lib(t.path());
    let rep = import_meta(&mut dst4, &v1, false).unwrap();
    assert_eq!((rep.matched, rep.applied), (1, 1));
    assert_eq!(rep.playlists, 0);
    let v9 = work.path().join("v9.json");
    fs::write(
        &v9,
        json!({"format":"jizzcraft-meta","version":9,"items":[]}).to_string(),
    )
    .unwrap();
    assert!(import_meta(&mut dst4, &v9, false).is_err());
}

// ---- m3u8 ------------------------------------------------------------------------------

#[test]
fn m3u8_export_rules() {
    let t = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let mut db = file_lib(t.path());
    let (a, b, c) = (
        id_of(&db, "a.png"),
        id_of(&db, "b.png"),
        id_of(&db, "c.png"),
    );
    let p = db.playlist_create("p", None).unwrap();
    db.playlist_add(p, &[b, a, c], None).unwrap();
    fs::remove_file(t.path().join("c.png")).unwrap();

    let dest = out.path().join("list.m3u8");
    let rep = db
        .export_m3u(
            p,
            &dest,
            &M3uOpts {
                relative: false,
                include_names: false,
            },
        )
        .unwrap();
    assert_eq!((rep.written, rep.skipped_missing), (2, 1));
    let text = fs::read_to_string(&dest).unwrap();
    assert!(text.starts_with("#EXTM3U\n"));
    assert!(!text.contains("EXTINF"), "no titles unless asked");
    assert!(!text.contains("a.png\n#"), "{text}");
    let lines: Vec<&str> = text.lines().skip(1).collect();
    assert!(
        lines[0].ends_with("b.png") && lines[1].ends_with("a.png"),
        "{lines:?}"
    );
    assert!(Path::new(lines[0]).is_absolute());

    // Refuses to overwrite or to write a media extension.
    assert!(db.export_m3u(p, &dest, &M3uOpts::default()).is_err());
    for bad in ["x.png", "x.mp4", "x.MKV"] {
        assert!(
            db.export_m3u(p, &out.path().join(bad), &M3uOpts::default())
                .is_err(),
            "{bad}"
        );
        assert!(!out.path().join(bad).exists());
    }
    assert!(
        db.export_m3u(p, &out.path().join("nodir/x.m3u8"), &M3uOpts::default())
            .is_err()
    );

    // Names and relative paths on request.
    let d2 = out.path().join("rel.m3u8");
    db.export_m3u(
        p,
        &d2,
        &M3uOpts {
            relative: true,
            include_names: true,
        },
    )
    .unwrap();
    let t2 = fs::read_to_string(&d2).unwrap();
    assert!(t2.contains("#EXTINF:-1,b.png\n"), "{t2}");
    assert!(
        t2.lines()
            .any(|l| !l.starts_with('#') && !Path::new(l).is_absolute() && l.ends_with("a.png"))
    );
    // Media were never touched.
    assert!(t.path().join("a.png").exists());
}

// ---- command surface ----------------------------------------------------------------------

#[test]
fn commands_cover_the_surface() {
    let mut db = synthetic(6);
    let mut reg: Registry<Db> = Registry::new();
    register_core(&mut reg);
    let mut run = |name: &str, args: Value| reg.run(&mut db, name, args).unwrap();

    let p = run("playlist.create", json!({ "name": "p", "color": 255 }))["id"]
        .as_i64()
        .unwrap();
    let r = run(
        "playlist.add",
        json!({ "id": p, "media_ids": [3, 1, 2, 3] }),
    );
    assert_eq!(
        (r["added"].as_u64(), r["skipped"].as_u64()),
        (Some(3), Some(1))
    );
    assert_eq!(
        run(
            "playlist.move",
            json!({ "id": p, "media_ids": [2], "before": 3 })
        )["moved"],
        1
    );
    run(
        "playlist.move",
        json!({ "id": p, "media_ids": [3], "before": null }),
    );
    let r = run("playlist.items", json!({ "id": p }));
    let order: Vec<i64> = r["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["id"].as_i64().unwrap())
        .collect();
    assert_eq!(order, [2, 1, 3]);
    assert_eq!(
        (r["count"].as_u64(), r["total"].as_u64()),
        (Some(3), Some(3))
    );
    assert_eq!(
        run("playlist.of", json!({ "media_ids": [1, 6] }))["ids"],
        json!([p])
    );
    let l = run("playlist.list", json!({}));
    assert_eq!(l["items"][0]["count"], 3);
    assert_eq!(l["items"][0]["color"], 255);
    run(
        "playlist.set",
        json!({ "id": p, "private": true, "color": null }),
    );
    assert_eq!(run("playlist.list", json!({}))["items"][0]["private"], true);
    run("playlist.rename", json!({ "id": p, "name": "renamed" }));
    run("playlist.sort", json!({ "id": p, "by": "name" }));
    assert_eq!(
        run("playlist.remove", json!({ "id": p, "media_ids": [1, 9] }))["removed"],
        1
    );

    let tag = run("tag.create", json!({ "name": "t" }))["id"]
        .as_i64()
        .unwrap();
    let tag2 = run("tag.create", json!({ "name": "t2" }))["id"]
        .as_i64()
        .unwrap();
    assert_eq!(
        run("tag.assign", json!({ "ids": [tag], "media_ids": [1, 2] }))["changed"],
        2
    );
    let r = run("tag.toggle", json!({ "id": tag, "media_ids": [1, 2] }));
    assert_eq!(
        (r["on"].as_bool(), r["changed"].as_u64()),
        (Some(false), Some(2))
    );
    run(
        "tag.assign",
        json!({ "ids": [tag], "media_ids": [1], "on": true }),
    );
    run("tag.rename", json!({ "id": tag2, "name": "t3" }));
    run(
        "tag.set",
        json!({ "id": tag2, "color": 1, "private": false, "position": 4 }),
    );
    assert_eq!(
        run("tag.merge", json!({ "from": tag, "into": tag2 }))["moved"],
        1
    );
    assert_eq!(
        run("tag.list", json!({}))["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );

    let a = run("category.create", json!({ "name": "A" }))["id"]
        .as_i64()
        .unwrap();
    let b = run("category.create", json!({ "name": "B", "parent": a }))["id"]
        .as_i64()
        .unwrap();
    run("category.assign", json!({ "ids": [b], "media_ids": [4] }));
    let l = run("category.list", json!({}));
    let items = l["items"].as_array().unwrap();
    assert_eq!(
        (
            items[0]["id"].as_i64(),
            items[0]["depth"].as_u64(),
            items[0]["total"].as_u64()
        ),
        (Some(a), Some(0), Some(1))
    );
    assert_eq!(
        (items[1]["parent_id"].as_i64(), items[1]["count"].as_u64()),
        (Some(a), Some(1))
    );
    assert!(
        reg.run(&mut db, "category.move", json!({ "id": a, "parent": b }))
            .is_err()
    );
    reg.run(&mut db, "category.move", json!({ "id": b, "parent": null }))
        .unwrap();
    reg.run(
        &mut db,
        "category.toggle",
        json!({ "id": a, "media_ids": [5] }),
    )
    .unwrap();
    reg.run(&mut db, "category.set", json!({ "id": a, "color": 7 }))
        .unwrap();
    reg.run(&mut db, "category.rename", json!({ "id": a, "name": "AA" }))
        .unwrap();
    let r = reg
        .run(&mut db, "labels.of", json!({ "media_id": 5 }))
        .unwrap();
    assert_eq!(r["categories"][0]["name"], "AA");
    assert_eq!(r["tags"].as_array().unwrap().len(), 0);
    let r = reg
        .run(&mut db, "category.merge", json!({ "from": b, "into": a }))
        .unwrap();
    assert_eq!(r["moved"], 1);
    reg.run(&mut db, "category.delete", json!({ "id": a }))
        .unwrap();
    reg.run(&mut db, "tag.delete", json!({ "id": tag2 }))
        .unwrap();
    assert_eq!(
        reg.run(&mut db, "playlist.delete", json!({ "id": p }))
            .unwrap()["deleted"],
        true
    );

    // Bad arguments are errors, not panics.
    assert!(
        reg.run(
            &mut db,
            "playlist.add",
            json!({ "id": 1, "media_ids": "x" })
        )
        .is_err()
    );
    assert!(
        reg.run(
            &mut db,
            "playlist.create",
            json!({ "name": "c", "color": -1 })
        )
        .is_err()
    );
    assert!(
        reg.run(&mut db, "playlist.sort", json!({ "id": 1, "by": "rm -rf" }))
            .is_err()
    );
    let big: Vec<i64> = (0..10_001).collect();
    assert!(
        reg.run(
            &mut db,
            "tag.assign",
            json!({ "ids": [1], "media_ids": big })
        )
        .is_err()
    );
}

// ---- scale ----------------------------------------------------------------------------------

fn scale(media: i64, playlists: i64, per: i64, strict: bool) {
    let mut db = synthetic(media);
    // Raw bulk load: 1k playlists x 100 items, plus one 100k-item playlist.
    {
        let c = db.conn_for_tests();
        c.execute_batch(&format!(
            "BEGIN;
             WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < {playlists})
             INSERT INTO playlists(id, name, name_lc, created_at, updated_at)
             SELECT i, 'p' || i, 'p' || i, 0, 0 FROM n;
             WITH RECURSIVE n(i) AS (SELECT 0 UNION ALL SELECT i + 1 FROM n WHERE i < {total})
             INSERT INTO playlist_items(playlist_id, media_id, pos, added_at)
             SELECT 1 + i / {per}, 1 + (i * 7919) % {media}, (i % {per} + 1) * 1024, 0 FROM n
             WHERE 1;
             COMMIT;",
            total = playlists * per - 1
        ))
        .unwrap();
    }
    let big = db.playlist_create("big", None).unwrap();
    let n_big = media.min(100_000);
    db.conn_for_tests()
        .execute(
            "INSERT INTO playlist_items(playlist_id, media_id, pos, added_at)
             SELECT ?1, id, id * 1024, 0 FROM media WHERE id <= ?2",
            rusqlite::params![big, n_big],
        )
        .unwrap();

    let budget = |ms: u128| if strict { ms } else { ms * 20 };

    let t = Instant::now();
    let pl = db.playlists().unwrap();
    let e = t.elapsed().as_millis();
    assert!(pl.len() as i64 > playlists);
    assert!(e < budget(150), "sidebar counts took {e} ms");

    let t = Instant::now();
    let page = db.playlist_items(big, Some(50), n_big as u64 / 2).unwrap();
    let e = t.elapsed().as_millis();
    assert_eq!(page.len(), 50);
    assert_eq!(page[0].id, n_big / 2 + 1);
    assert!(e < budget(30), "position page took {e} ms");

    // Mid-insert into the big playlist: remove a member then re-insert it
    // between two neighbours that leave a gap.
    let mid = n_big / 2;
    db.playlist_remove(big, &[mid]).unwrap();
    let t = Instant::now();
    db.playlist_add(big, &[mid], Some(mid + 1)).unwrap();
    let e = t.elapsed().as_millis();
    assert!(e < budget(10), "mid-insert took {e} ms");
    let ids_page = ids(&db, &db.playlist_filter(big, Some(3), mid as u64 - 2));
    assert_eq!(ids_page, [mid - 1, mid, mid + 1]);

    let t = Instant::now();
    db.playlist_renumber(big).unwrap();
    let e = t.elapsed().as_millis();
    assert!(e < budget(300), "renumber took {e} ms");
    db.quick_check().unwrap();
}

#[test]
fn scale_10k_always() {
    // Same code path as the 100k run; budgets are relaxed 20x for debug builds.
    scale(10_000, 100, 100, false);
}

#[test]
#[ignore = "set JIZZCRAFT_SCALE=1 and pass --ignored (use --release)"]
fn scale_100k() {
    if std::env::var("JIZZCRAFT_SCALE").as_deref() != Ok("1") {
        eprintln!("JIZZCRAFT_SCALE=1 not set: skipped");
        return;
    }
    scale(100_000, 1_000, 100, true);
}
