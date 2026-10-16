//! Playlists, categories and tags. Membership rows only: deleting a playlist,
//! category or tag never touches a `media` row or a file.
//!
//! * Playlist: ordered by hand, no duplicates. Order is a sparse integer `pos`
//!   (step 1024); an insert takes the midpoint and renumbers when the gap is gone.
//! * Category: hierarchical (depth <= 4); an item may sit in several; a filter
//!   on a category includes its whole subtree.
//! * Tag: flat. Categories and tags share the `labels` table (kind 1 / 2).
//! * "Smart playlists" are saved views that filter by playlist, tag or category.
//!
//! Filter values reach SQL only as bound parameters; only placeholder counts
//! (bounded by `MAX_FILTER_IDS`) are formatted into SQL text.

use crate::commands::{HasLibrary, Registry};
use crate::db::{Db, Filter, SortKey, now_secs, path_to_blob};
use crate::error::{Error, Result};
use crate::model::Media;
use rusqlite::types::Value as Sql;
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use serde_json::{Map, Value, json};
use std::collections::HashSet;
use std::io::Write;
use std::path::{Component, Path, PathBuf};

pub const STEP: i64 = 1024;
pub const MAX_NAME_CHARS: usize = 60;
pub const MAX_PLAYLISTS: i64 = 10_000;
pub const MAX_LABELS: i64 = 10_000;
pub const MAX_ITEMS: i64 = 100_000;
pub const MAX_IDS: usize = 10_000;
pub const MAX_DEPTH: usize = 4;
pub const MAX_FILTER_IDS: usize = 64;

fn invalid(m: impl Into<String>) -> Error {
    Error::Invalid(m.into())
}

// ---- filter ---------------------------------------------------------------

/// Playlist / tag / category conditions of a `Filter`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OrgFilter {
    /// Only members of this playlist.
    pub playlist: Option<i64>,
    /// Must be in every one of these labels (each includes its subtree).
    pub labels_all: Vec<i64>,
    /// Must be in at least one of these labels.
    pub labels_any: Vec<i64>,
    /// Must be in none of these labels.
    pub labels_none: Vec<i64>,
    /// Has no tag at all.
    pub untagged: bool,
    /// Is in no category at all.
    pub uncategorized: bool,
}

impl OrgFilter {
    pub fn is_empty(&self) -> bool {
        *self == OrgFilter::default()
    }

    pub(crate) fn from_json(o: &Map<String, Value>) -> Result<OrgFilter> {
        fn ids(o: &Map<String, Value>, k: &str) -> Result<Vec<i64>> {
            match o.get(k) {
                None | Some(Value::Null) => Ok(vec![]),
                Some(Value::Array(a)) => {
                    if a.len() > MAX_FILTER_IDS {
                        return Err(invalid(format!("`{k}` takes at most {MAX_FILTER_IDS} ids")));
                    }
                    a.iter()
                        .map(|v| {
                            v.as_i64()
                                .filter(|n| *n > 0)
                                .ok_or_else(|| invalid(format!("`{k}` must hold positive ids")))
                        })
                        .collect()
                }
                Some(_) => Err(invalid(format!("`{k}` must be an array of ids"))),
            }
        }
        fn flag(o: &Map<String, Value>, k: &str) -> Result<bool> {
            match o.get(k) {
                None | Some(Value::Null) => Ok(false),
                Some(v) => v
                    .as_bool()
                    .ok_or_else(|| invalid(format!("`{k}` must be a boolean"))),
            }
        }
        let playlist = match o.get("playlist") {
            None | Some(Value::Null) => None,
            Some(v) => Some(
                v.as_i64()
                    .filter(|n| *n > 0)
                    .ok_or_else(|| invalid("`playlist` must be a positive id"))?,
            ),
        };
        Ok(OrgFilter {
            playlist,
            labels_all: ids(o, "labels_all")?,
            labels_any: ids(o, "labels_any")?,
            labels_none: ids(o, "labels_none")?,
            untagged: flag(o, "untagged")?,
            uncategorized: flag(o, "uncategorized")?,
        })
    }

    pub(crate) fn to_json(&self, o: &mut Map<String, Value>) {
        if let Some(p) = self.playlist {
            o.insert("playlist".into(), json!(p));
        }
        for (k, v) in [
            ("labels_all", &self.labels_all),
            ("labels_any", &self.labels_any),
            ("labels_none", &self.labels_none),
        ] {
            if !v.is_empty() {
                o.insert(k.into(), json!(v));
            }
        }
        if self.untagged {
            o.insert("untagged".into(), json!(true));
        }
        if self.uncategorized {
            o.insert("uncategorized".into(), json!(true));
        }
    }
}

/// `media.id IN (<media in the subtrees of the labels matched by seed>)`.
/// Uncorrelated, so SQLite evaluates it once per query.
fn in_subtrees(seed: &str) -> String {
    format!(
        "(WITH RECURSIVE sub(id) AS (SELECT id FROM labels WHERE id {seed} \
         UNION SELECT l.id FROM labels l JOIN sub ON l.parent_id = sub.id) \
         SELECT ml.media_id FROM media_labels ml WHERE ml.label_id IN (SELECT id FROM sub))"
    )
}

fn placeholders(n: usize) -> String {
    let mut s = String::from("IN (");
    for i in 0..n {
        if i > 0 {
            s.push(',');
        }
        s.push('?');
    }
    s.push(')');
    s
}

/// Called from `Db::where_clause`. Only `?` placeholders are generated.
pub(crate) fn push_conds(f: &OrgFilter, conds: &mut Vec<String>, args: &mut Vec<Sql>) {
    if let Some(p) = f.playlist {
        conds
            .push("media.id IN (SELECT media_id FROM playlist_items WHERE playlist_id = ?)".into());
        args.push(Sql::Integer(p));
    }
    for id in f.labels_all.iter().take(MAX_FILTER_IDS) {
        conds.push(format!("media.id IN {}", in_subtrees("= ?")));
        args.push(Sql::Integer(*id));
    }
    let any: Vec<i64> = f.labels_any.iter().copied().take(MAX_FILTER_IDS).collect();
    if !any.is_empty() {
        conds.push(format!(
            "media.id IN {}",
            in_subtrees(&placeholders(any.len()))
        ));
        args.extend(any.into_iter().map(Sql::Integer));
    }
    let none: Vec<i64> = f.labels_none.iter().copied().take(MAX_FILTER_IDS).collect();
    if !none.is_empty() {
        conds.push(format!(
            "media.id NOT IN {}",
            in_subtrees(&placeholders(none.len()))
        ));
        args.extend(none.into_iter().map(Sql::Integer));
    }
    for (on, kind) in [(f.untagged, 2), (f.uncategorized, 1)] {
        if on {
            conds.push(format!(
                "media.id NOT IN (SELECT ml.media_id FROM media_labels ml \
                 JOIN labels l ON l.id = ml.label_id WHERE l.kind = {kind})"
            ));
        }
    }
}

// ---- shared helpers --------------------------------------------------------

/// Trim, check 1..=60 printable characters, return `(name, name_lc)`.
pub fn clean_name(raw: &str) -> Result<(String, String)> {
    let name = raw.trim();
    let n = name.chars().count();
    if n == 0 || n > MAX_NAME_CHARS || name.chars().any(char::is_control) {
        return Err(invalid("name must be 1-60 printable characters"));
    }
    Ok((name.to_owned(), name.to_lowercase()))
}

fn map_unique(e: rusqlite::Error, what: &str) -> Error {
    match &e {
        rusqlite::Error::SqliteFailure(f, _)
            if f.code == rusqlite::ErrorCode::ConstraintViolation =>
        {
            invalid(format!("{what} name is already used here"))
        }
        _ => Error::Db(e),
    }
}

fn check_ids(ids: &[i64]) -> Result<()> {
    if ids.len() > MAX_IDS {
        return Err(invalid(format!("at most {MAX_IDS} items per call")));
    }
    Ok(())
}

fn dedup(ids: &[i64]) -> Vec<i64> {
    let mut seen = HashSet::with_capacity(ids.len());
    ids.iter().copied().filter(|i| seen.insert(*i)).collect()
}

fn color_sql(c: Option<u32>) -> Result<Option<i64>> {
    match c {
        Some(v) if v > 0xFF_FFFF => Err(invalid("color must be 0..=0xFFFFFF")),
        v => Ok(v.map(i64::from)),
    }
}

// ---- playlists -------------------------------------------------------------

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Playlist {
    pub id: i64,
    pub name: String,
    pub color: Option<u32>,
    pub private: bool,
    pub position: i64,
    pub count: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaylistSort {
    Name,
    Added,
    Duration,
    Random,
}

impl PlaylistSort {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "name" => Some(Self::Name),
            "added" => Some(Self::Added),
            "duration" => Some(Self::Duration),
            "random" => Some(Self::Random),
            _ => None,
        }
    }
}

fn pl_exists(c: &Connection, id: i64) -> Result<()> {
    let n: i64 = c.query_row("SELECT COUNT(*) FROM playlists WHERE id = ?1", [id], |r| {
        r.get(0)
    })?;
    if n == 0 {
        return Err(Error::NotFound(format!("playlist {id}")));
    }
    Ok(())
}

fn pl_touch(c: &Connection, id: i64) -> Result<()> {
    c.execute(
        "UPDATE playlists SET updated_at = ?2 WHERE id = ?1",
        params![id, now_secs()],
    )?;
    Ok(())
}

fn pl_count(c: &Connection, id: i64) -> Result<i64> {
    Ok(c.query_row(
        "SELECT COUNT(*) FROM playlist_items WHERE playlist_id = ?1",
        [id],
        |r| r.get(0),
    )?)
}

/// Rewrite positions as 1024, 2048, ... in the current order.
fn renumber(c: &Connection, pl: i64) -> Result<()> {
    c.execute(
        "UPDATE playlist_items SET pos = r.rn * ?2
         FROM (SELECT media_id AS mid,
                      ROW_NUMBER() OVER (ORDER BY pos, added_at, media_id) AS rn
               FROM playlist_items WHERE playlist_id = ?1) AS r
         WHERE playlist_items.playlist_id = ?1 AND playlist_items.media_id = r.mid",
        params![pl, STEP],
    )?;
    Ok(())
}

/// Insert `(media_id, added_at)` rows, in order, before member `before`
/// (or at the end). The rows must not be members already.
fn insert_at(c: &Connection, pl: i64, items: &[(i64, i64)], before: Option<i64>) -> Result<()> {
    if items.is_empty() {
        return Ok(());
    }
    let k = items.len() as i64;
    let mut renumbered = false;
    let positions: Vec<i64> = loop {
        let (lo, hi): (i64, Option<i64>) = match before {
            None => (
                c.query_row(
                    "SELECT COALESCE(MAX(pos), 0) FROM playlist_items WHERE playlist_id = ?1",
                    [pl],
                    |r| r.get(0),
                )?,
                None,
            ),
            Some(b) => {
                let hi: i64 = c
                    .query_row(
                        "SELECT pos FROM playlist_items WHERE playlist_id = ?1 AND media_id = ?2",
                        params![pl, b],
                        |r| r.get(0),
                    )
                    .optional()?
                    .ok_or_else(|| invalid("`before` is not in the playlist"))?;
                let lo: i64 = c.query_row(
                    "SELECT COALESCE(MAX(pos), 0) FROM playlist_items
                     WHERE playlist_id = ?1 AND pos < ?2",
                    params![pl, hi],
                    |r| r.get(0),
                )?;
                (lo, Some(hi))
            }
        };
        match hi {
            None => break (1..=k).map(|i| lo + i * STEP).collect(),
            Some(hi) if hi - lo > k => {
                break (1..=k).map(|i| lo + (hi - lo) * i / (k + 1)).collect();
            }
            Some(hi) => {
                if !renumbered {
                    renumber(c, pl)?;
                    renumbered = true;
                } else {
                    // Even a fresh grid is too tight for this many items:
                    // shift the tail to make room.
                    c.execute(
                        "UPDATE playlist_items SET pos = pos + ?3
                         WHERE playlist_id = ?1 AND pos >= ?2",
                        params![pl, hi, (k + 1) * STEP],
                    )?;
                }
            }
        }
    };
    let mut ins = c.prepare_cached(
        "INSERT INTO playlist_items(playlist_id, media_id, pos, added_at) VALUES (?1, ?2, ?3, ?4)",
    )?;
    for ((mid, added), pos) in items.iter().zip(positions) {
        ins.execute(params![pl, mid, pos, added])?;
    }
    Ok(())
}

fn pl_add(c: &Connection, pl: i64, ids: &[i64], before: Option<i64>) -> Result<(u64, u64)> {
    check_ids(ids)?;
    pl_exists(c, pl)?;
    let now = now_secs();
    let mut fresh: Vec<(i64, i64)> = Vec::new();
    {
        let mut exists = c.prepare_cached("SELECT 1 FROM media WHERE id = ?1")?;
        let mut member = c.prepare_cached(
            "SELECT 1 FROM playlist_items WHERE playlist_id = ?1 AND media_id = ?2",
        )?;
        for id in dedup(ids) {
            if exists.exists([id])? && !member.exists(params![pl, id])? {
                fresh.push((id, now));
            }
        }
    }
    if pl_count(c, pl)? + fresh.len() as i64 > MAX_ITEMS {
        return Err(invalid(format!(
            "a playlist holds at most {MAX_ITEMS} items"
        )));
    }
    insert_at(c, pl, &fresh, before)?;
    if !fresh.is_empty() {
        pl_touch(c, pl)?;
    }
    let added = fresh.len() as u64;
    Ok((added, ids.len() as u64 - added))
}

fn pl_remove(c: &Connection, pl: i64, ids: &[i64]) -> Result<u64> {
    check_ids(ids)?;
    pl_exists(c, pl)?;
    let mut del =
        c.prepare_cached("DELETE FROM playlist_items WHERE playlist_id = ?1 AND media_id = ?2")?;
    let mut n = 0u64;
    for id in dedup(ids) {
        n += del.execute(params![pl, id])? as u64;
    }
    if n > 0 {
        pl_touch(c, pl)?;
    }
    Ok(n)
}

fn pl_move(c: &Connection, pl: i64, ids: &[i64], before: Option<i64>) -> Result<u64> {
    check_ids(ids)?;
    pl_exists(c, pl)?;
    let ids = dedup(ids);
    if before.is_some_and(|b| ids.contains(&b)) {
        return Err(invalid("`before` cannot be one of the moved items"));
    }
    let mut keep: Vec<(i64, i64)> = Vec::new();
    {
        let mut get = c.prepare_cached(
            "SELECT added_at FROM playlist_items WHERE playlist_id = ?1 AND media_id = ?2",
        )?;
        for id in &ids {
            if let Some(a) = get
                .query_row(params![pl, id], |r| r.get::<_, i64>(0))
                .optional()?
            {
                keep.push((*id, a));
            }
        }
    }
    if let Some(b) = before {
        let n: i64 = c.query_row(
            "SELECT COUNT(*) FROM playlist_items WHERE playlist_id = ?1 AND media_id = ?2",
            params![pl, b],
            |r| r.get(0),
        )?;
        if n == 0 {
            return Err(invalid("`before` is not in the playlist"));
        }
    }
    let mut del =
        c.prepare_cached("DELETE FROM playlist_items WHERE playlist_id = ?1 AND media_id = ?2")?;
    for (id, _) in &keep {
        del.execute(params![pl, id])?;
    }
    insert_at(c, pl, &keep, before)?;
    if !keep.is_empty() {
        pl_touch(c, pl)?;
    }
    Ok(keep.len() as u64)
}

fn pl_sort(c: &Connection, pl: i64, by: PlaylistSort, desc: bool, seed: u32) -> Result<()> {
    pl_exists(c, pl)?;
    struct Row {
        id: i64,
        added: i64,
        name: String,
        dur: Option<i64>,
    }
    let mut rows: Vec<Row> = {
        let mut st = c.prepare(
            "SELECT i.media_id, i.added_at, m.name_lc, m.duration_ms
             FROM playlist_items i JOIN media m ON m.id = i.media_id
             WHERE i.playlist_id = ?1",
        )?;
        let it = st.query_map([pl], |r| {
            Ok(Row {
                id: r.get(0)?,
                added: r.get(1)?,
                name: r.get(2)?,
                dur: r.get(3)?,
            })
        })?;
        it.collect::<std::result::Result<_, _>>()?
    };
    match by {
        PlaylistSort::Name => {
            rows.sort_by(|a, b| a.name.cmp(&b.name).then(a.id.cmp(&b.id)));
            if desc {
                rows.reverse();
            }
        }
        PlaylistSort::Added => {
            rows.sort_by_key(|r| (r.added, r.id));
            if desc {
                rows.reverse();
            }
        }
        PlaylistSort::Duration => {
            let (mut with, mut without): (Vec<Row>, Vec<Row>) =
                rows.into_iter().partition(|r| r.dur.is_some());
            with.sort_by_key(|r| (r.dur, r.id));
            if desc {
                with.reverse();
            }
            without.sort_by_key(|r| r.id);
            with.extend(without);
            rows = with;
        }
        PlaylistSort::Random => {
            let key = |id: i64| {
                (((id as u64).wrapping_add(u64::from(seed & 0x7fff_ffff)))
                    .wrapping_mul(2_654_435_761))
                    % 4_294_967_296
            };
            rows.sort_by_key(|r| (key(r.id), r.id));
        }
    }
    let mut up = c.prepare_cached(
        "UPDATE playlist_items SET pos = ?3 WHERE playlist_id = ?1 AND media_id = ?2",
    )?;
    for (i, r) in rows.iter().enumerate() {
        up.execute(params![pl, r.id, (i as i64 + 1) * STEP])?;
    }
    pl_touch(c, pl)
}

// ---- labels ----------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum LabelKind {
    Category,
    Tag,
}

impl LabelKind {
    fn as_i64(self) -> i64 {
        match self {
            LabelKind::Category => 1,
            LabelKind::Tag => 2,
        }
    }
    fn what(self) -> &'static str {
        match self {
            LabelKind::Category => "category",
            LabelKind::Tag => "tag",
        }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct LabelInfo {
    pub id: i64,
    pub kind: LabelKind,
    pub parent_id: Option<i64>,
    pub name: String,
    /// `A/B/C` for categories, the name for tags.
    pub path: String,
    /// 0 for tags and root categories.
    pub depth: u32,
    pub color: Option<u32>,
    pub private: bool,
    pub position: i64,
    /// Items directly in this label.
    pub count: u64,
    /// Distinct items in this label and below (equals `count` for tags).
    pub total: u64,
}

fn label_row(c: &Connection, kind: LabelKind, id: i64) -> Result<(Option<i64>, String)> {
    c.query_row(
        "SELECT parent_id, name FROM labels WHERE id = ?1 AND kind = ?2",
        params![id, kind.as_i64()],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .optional()?
    .ok_or_else(|| Error::NotFound(format!("{} {id}", kind.what())))
}

/// Number of ancestors above `id` plus one (a root is depth 1).
fn depth_of(c: &Connection, id: i64) -> Result<usize> {
    let mut depth = 1;
    let mut cur = id;
    for _ in 0..=MAX_DEPTH + 8 {
        let p: Option<i64> = c
            .query_row("SELECT parent_id FROM labels WHERE id = ?1", [cur], |r| {
                r.get(0)
            })
            .optional()?
            .flatten();
        match p {
            Some(p) => {
                depth += 1;
                cur = p;
            }
            None => return Ok(depth),
        }
    }
    Err(invalid("category tree is too deep"))
}

/// Levels of the subtree rooted at `id` (a leaf is 1).
fn height_of(c: &Connection, id: i64) -> Result<usize> {
    let h: i64 = c.query_row(
        "WITH RECURSIVE s(id, d) AS (
             SELECT ?1, 1
             UNION ALL SELECT l.id, s.d + 1 FROM labels l JOIN s ON l.parent_id = s.id
             WHERE s.d < 32)
         SELECT MAX(d) FROM s",
        [id],
        |r| r.get(0),
    )?;
    Ok(h as usize)
}

/// Is `node` equal to, or below, `anc`?
fn is_within(c: &Connection, anc: i64, node: i64) -> Result<bool> {
    let mut cur = node;
    for _ in 0..64 {
        if cur == anc {
            return Ok(true);
        }
        let p: Option<i64> = c
            .query_row("SELECT parent_id FROM labels WHERE id = ?1", [cur], |r| {
                r.get(0)
            })
            .optional()?
            .flatten();
        match p {
            Some(p) => cur = p,
            None => return Ok(false),
        }
    }
    Err(invalid("category tree is malformed"))
}

/// Is `lc` an alias of a tag other than `except`?
fn alias_taken(c: &Connection, lc: &str, except: Option<i64>) -> Result<bool> {
    Ok(c.query_row(
        "SELECT 1 FROM label_aliases WHERE alias_lc = ?1 AND label_id IS NOT ?2",
        params![lc, except],
        |_| Ok(()),
    )
    .optional()?
    .is_some())
}

fn label_create(
    c: &Connection,
    kind: LabelKind,
    name: &str,
    parent: Option<i64>,
    color: Option<u32>,
) -> Result<i64> {
    let (name, lc) = clean_name(name)?;
    if kind == LabelKind::Category && name.contains('/') {
        return Err(invalid("a category name cannot contain `/`"));
    }
    if kind == LabelKind::Tag && alias_taken(c, &lc, None)? {
        return Err(invalid("that name is already an alias of another tag"));
    }
    let n: i64 = c.query_row("SELECT COUNT(*) FROM labels", [], |r| r.get(0))?;
    if n >= MAX_LABELS {
        return Err(invalid(format!("at most {MAX_LABELS} categories and tags")));
    }
    if let Some(p) = parent {
        if kind != LabelKind::Category {
            return Err(invalid("tags are flat"));
        }
        label_row(c, LabelKind::Category, p)?;
        if depth_of(c, p)? + 1 > MAX_DEPTH {
            return Err(invalid(format!("categories nest at most {MAX_DEPTH} deep")));
        }
    }
    let color = color_sql(color)?;
    c.execute(
        "INSERT INTO labels(kind, parent_id, name, name_lc, color, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![kind.as_i64(), parent, name, lc, color, now_secs()],
    )
    .map_err(|e| map_unique(e, kind.what()))?;
    Ok(c.last_insert_rowid())
}

fn label_rename(c: &Connection, kind: LabelKind, id: i64, name: &str) -> Result<()> {
    let (name, lc) = clean_name(name)?;
    if kind == LabelKind::Category && name.contains('/') {
        return Err(invalid("a category name cannot contain `/`"));
    }
    if kind == LabelKind::Tag {
        // Renaming a tag to one of its own aliases promotes the alias.
        c.execute(
            "DELETE FROM label_aliases WHERE label_id = ?1 AND alias_lc = ?2",
            params![id, lc],
        )?;
        if alias_taken(c, &lc, Some(id))? {
            return Err(invalid("that name is already an alias of another tag"));
        }
    }
    let n = c
        .execute(
            "UPDATE labels SET name = ?3, name_lc = ?4 WHERE id = ?1 AND kind = ?2",
            params![id, kind.as_i64(), name, lc],
        )
        .map_err(|e| map_unique(e, kind.what()))?;
    if n == 0 {
        return Err(Error::NotFound(format!("{} {id}", kind.what())));
    }
    Ok(())
}

fn label_set(
    c: &Connection,
    kind: LabelKind,
    id: i64,
    color: Option<Option<u32>>,
    private: Option<bool>,
    position: Option<i64>,
) -> Result<()> {
    label_row(c, kind, id)?;
    if let Some(col) = color {
        c.execute(
            "UPDATE labels SET color = ?2 WHERE id = ?1",
            params![id, color_sql(col)?],
        )?;
    }
    if let Some(p) = private {
        c.execute(
            "UPDATE labels SET private = ?2 WHERE id = ?1",
            params![id, p],
        )?;
    }
    if let Some(p) = position {
        c.execute(
            "UPDATE labels SET position = ?2 WHERE id = ?1",
            params![id, p],
        )?;
    }
    Ok(())
}

fn label_delete(c: &Connection, kind: LabelKind, id: i64) -> Result<u64> {
    let (parent, _) = label_row(c, kind, id)?;
    let members: i64 = c.query_row(
        "SELECT COUNT(*) FROM media_labels WHERE label_id = ?1",
        [id],
        |r| r.get(0),
    )?;
    // Children move up to the deleted category's parent.
    c.execute(
        "UPDATE labels SET parent_id = ?2 WHERE parent_id = ?1",
        params![id, parent],
    )
    .map_err(|e| map_unique(e, "category"))?;
    c.execute("DELETE FROM labels WHERE id = ?1", [id])?;
    Ok(members as u64)
}

fn category_move(c: &Connection, id: i64, parent: Option<i64>) -> Result<()> {
    label_row(c, LabelKind::Category, id)?;
    if let Some(p) = parent {
        label_row(c, LabelKind::Category, p)?;
        if is_within(c, id, p)? {
            return Err(invalid(
                "a category cannot move into itself or its own subtree",
            ));
        }
        if depth_of(c, p)? + height_of(c, id)? > MAX_DEPTH {
            return Err(invalid(format!("categories nest at most {MAX_DEPTH} deep")));
        }
    }
    c.execute(
        "UPDATE labels SET parent_id = ?2 WHERE id = ?1",
        params![id, parent],
    )
    .map_err(|e| map_unique(e, "category"))?;
    Ok(())
}

fn label_merge(c: &Connection, kind: LabelKind, from: i64, into: i64) -> Result<u64> {
    if from == into {
        return Err(invalid("cannot merge into itself"));
    }
    label_row(c, kind, from)?;
    label_row(c, kind, into)?;
    if kind == LabelKind::Category {
        if is_within(c, from, into)? {
            return Err(invalid("cannot merge a category into its own subtree"));
        }
        let kids: Vec<i64> = {
            let mut st = c.prepare("SELECT id FROM labels WHERE parent_id = ?1")?;
            let it = st.query_map([from], |r| r.get(0))?;
            it.collect::<std::result::Result<_, _>>()?
        };
        for k in kids {
            category_move(c, k, Some(into))?;
        }
    }
    let moved = c.execute(
        "INSERT OR IGNORE INTO media_labels(label_id, media_id, added_at)
         SELECT ?2, media_id, added_at FROM media_labels WHERE label_id = ?1",
        params![from, into],
    )?;
    if kind == LabelKind::Tag {
        crate::meta::tx_tag_merge_aliases(c, from, into)?;
    }
    c.execute("DELETE FROM labels WHERE id = ?1", [from])?;
    Ok(moved as u64)
}

fn label_assign(
    c: &Connection,
    kind: LabelKind,
    labels: &[i64],
    media: &[i64],
    on: bool,
) -> Result<u64> {
    check_ids(media)?;
    if labels.len() > MAX_FILTER_IDS {
        return Err(invalid(format!("at most {MAX_FILTER_IDS} labels per call")));
    }
    let labels = dedup(labels);
    for l in &labels {
        label_row(c, kind, *l)?;
    }
    let media = dedup(media);
    let now = now_secs();
    let mut changed = 0u64;
    if on {
        let mut st = c.prepare_cached(
            "INSERT OR IGNORE INTO media_labels(label_id, media_id, added_at)
             SELECT ?1, id, ?3 FROM media WHERE id = ?2",
        )?;
        for l in &labels {
            for m in &media {
                changed += st.execute(params![l, m, now])? as u64;
            }
        }
    } else {
        let mut st =
            c.prepare_cached("DELETE FROM media_labels WHERE label_id = ?1 AND media_id = ?2")?;
        for l in &labels {
            for m in &media {
                changed += st.execute(params![l, m])? as u64;
            }
        }
    }
    Ok(changed)
}

fn label_toggle(c: &Connection, kind: LabelKind, label: i64, media: &[i64]) -> Result<(bool, u64)> {
    check_ids(media)?;
    label_row(c, kind, label)?;
    let media = dedup(media);
    let (mut existing, mut have) = (0u64, 0u64);
    {
        let mut ex = c.prepare_cached("SELECT 1 FROM media WHERE id = ?1")?;
        let mut hv =
            c.prepare_cached("SELECT 1 FROM media_labels WHERE label_id = ?1 AND media_id = ?2")?;
        for m in &media {
            if ex.exists([m])? {
                existing += 1;
                if hv.exists(params![label, m])? {
                    have += 1;
                }
            }
        }
    }
    let on = !(existing > 0 && have == existing);
    let changed = label_assign(c, kind, &[label], &media, on)?;
    Ok((on, changed))
}

fn labels_list(c: &Connection, kind: LabelKind) -> Result<Vec<LabelInfo>> {
    struct Raw {
        id: i64,
        parent: Option<i64>,
        name: String,
        name_lc: String,
        color: Option<i64>,
        private: bool,
        position: i64,
        count: u64,
    }
    let raws: Vec<Raw> = {
        let mut st = c.prepare(
            "SELECT l.id, l.parent_id, l.name, l.name_lc, l.color, l.private, l.position,
                    (SELECT COUNT(*) FROM media_labels m WHERE m.label_id = l.id)
             FROM labels l WHERE l.kind = ?1",
        )?;
        let it = st.query_map([kind.as_i64()], |r| {
            Ok(Raw {
                id: r.get(0)?,
                parent: r.get(1)?,
                name: r.get(2)?,
                name_lc: r.get(3)?,
                color: r.get(4)?,
                private: r.get::<_, i64>(5)? != 0,
                position: r.get(6)?,
                count: r.get::<_, i64>(7)? as u64,
            })
        })?;
        it.collect::<std::result::Result<_, _>>()?
    };
    let mut totals: std::collections::HashMap<i64, u64> = std::collections::HashMap::new();
    if kind == LabelKind::Category {
        let mut st = c.prepare(
            "WITH RECURSIVE t(root, id) AS (
                 SELECT id, id FROM labels WHERE kind = 1
                 UNION ALL SELECT t.root, l.id FROM labels l JOIN t ON l.parent_id = t.id)
             SELECT t.root, COUNT(DISTINCT ml.media_id)
             FROM t JOIN media_labels ml ON ml.label_id = t.id GROUP BY t.root",
        )?;
        let it = st.query_map([], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)? as u64))
        })?;
        for row in it {
            let (k, v) = row?;
            totals.insert(k, v);
        }
    }
    let by_id: std::collections::HashMap<i64, usize> =
        raws.iter().enumerate().map(|(i, r)| (r.id, i)).collect();
    let mut kids: std::collections::HashMap<Option<i64>, Vec<usize>> =
        std::collections::HashMap::new();
    for (i, r) in raws.iter().enumerate() {
        // A parent outside the set (corrupt data) is treated as a root.
        let p = r.parent.filter(|p| by_id.contains_key(p));
        kids.entry(p).or_default().push(i);
    }
    for v in kids.values_mut() {
        v.sort_by(|a, b| {
            (raws[*a].position, &raws[*a].name_lc, raws[*a].id).cmp(&(
                raws[*b].position,
                &raws[*b].name_lc,
                raws[*b].id,
            ))
        });
    }
    let mut out = Vec::with_capacity(raws.len());
    // Depth-first, children after their parent.
    let mut stack: Vec<(usize, u32, String)> = kids
        .get(&None)
        .into_iter()
        .flatten()
        .rev()
        .map(|i| (*i, 0, String::new()))
        .collect();
    while let Some((i, depth, prefix)) = stack.pop() {
        let r = &raws[i];
        let path = if prefix.is_empty() {
            r.name.clone()
        } else {
            format!("{prefix}/{}", r.name)
        };
        out.push(LabelInfo {
            id: r.id,
            kind,
            parent_id: r.parent,
            name: r.name.clone(),
            path: path.clone(),
            depth,
            color: r.color.map(|v| v as u32),
            private: r.private,
            position: r.position,
            count: r.count,
            total: if kind == LabelKind::Tag {
                r.count
            } else {
                totals.get(&r.id).copied().unwrap_or(0)
            },
        });
        if depth < 64 {
            for k in kids.get(&Some(r.id)).into_iter().flatten().rev() {
                stack.push((*k, depth + 1, path.clone()));
            }
        }
    }
    Ok(out)
}

// ---- public Db API ---------------------------------------------------------

impl Db {
    // playlists

    pub fn playlists(&self) -> Result<Vec<Playlist>> {
        let mut st = self.conn.prepare(
            "SELECT p.id, p.name, p.color, p.private, p.position,
                    (SELECT COUNT(*) FROM playlist_items i WHERE i.playlist_id = p.id)
             FROM playlists p ORDER BY p.position, p.name_lc, p.id",
        )?;
        let rows = st.query_map([], |r| {
            Ok(Playlist {
                id: r.get(0)?,
                name: r.get(1)?,
                color: r.get::<_, Option<i64>>(2)?.map(|v| v as u32),
                private: r.get::<_, i64>(3)? != 0,
                position: r.get(4)?,
                count: r.get::<_, i64>(5)? as u64,
            })
        })?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    pub fn playlist_create(&mut self, name: &str, color: Option<u32>) -> Result<i64> {
        let (name, lc) = clean_name(name)?;
        let color = color_sql(color)?;
        let tx = self.conn.transaction()?;
        let n: i64 = tx.query_row("SELECT COUNT(*) FROM playlists", [], |r| r.get(0))?;
        if n >= MAX_PLAYLISTS {
            return Err(invalid(format!("at most {MAX_PLAYLISTS} playlists")));
        }
        let now = now_secs();
        tx.execute(
            "INSERT INTO playlists(name, name_lc, color, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?4)",
            params![name, lc, color, now],
        )
        .map_err(|e| map_unique(e, "playlist"))?;
        let id = tx.last_insert_rowid();
        tx.commit()?;
        Ok(id)
    }

    pub fn playlist_rename(&mut self, id: i64, name: &str) -> Result<()> {
        let (name, lc) = clean_name(name)?;
        let n = self
            .conn
            .execute(
                "UPDATE playlists SET name = ?2, name_lc = ?3, updated_at = ?4 WHERE id = ?1",
                params![id, name, lc, now_secs()],
            )
            .map_err(|e| map_unique(e, "playlist"))?;
        if n == 0 {
            return Err(Error::NotFound(format!("playlist {id}")));
        }
        Ok(())
    }

    /// `color`: `None` leaves it, `Some(None)` clears it.
    pub fn playlist_set(
        &mut self,
        id: i64,
        color: Option<Option<u32>>,
        private: Option<bool>,
        position: Option<i64>,
    ) -> Result<()> {
        let tx = self.conn.transaction()?;
        pl_exists(&tx, id)?;
        if let Some(col) = color {
            tx.execute(
                "UPDATE playlists SET color = ?2 WHERE id = ?1",
                params![id, color_sql(col)?],
            )?;
        }
        if let Some(p) = private {
            tx.execute(
                "UPDATE playlists SET private = ?2 WHERE id = ?1",
                params![id, p],
            )?;
        }
        if let Some(p) = position {
            tx.execute(
                "UPDATE playlists SET position = ?2 WHERE id = ?1",
                params![id, p],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Returns how many membership rows went with it. Media is untouched.
    pub fn playlist_delete(&mut self, id: i64) -> Result<u64> {
        let tx = self.conn.transaction()?;
        pl_exists(&tx, id)?;
        let n = pl_count(&tx, id)?;
        tx.execute("DELETE FROM playlists WHERE id = ?1", [id])?;
        tx.commit()?;
        Ok(n as u64)
    }

    /// Returns `(added, skipped)`. Missing media and duplicates are skipped.
    pub fn playlist_add(
        &mut self,
        id: i64,
        media_ids: &[i64],
        before: Option<i64>,
    ) -> Result<(u64, u64)> {
        let tx = self.conn.transaction()?;
        let r = pl_add(&tx, id, media_ids, before)?;
        tx.commit()?;
        Ok(r)
    }

    pub fn playlist_remove(&mut self, id: i64, media_ids: &[i64]) -> Result<u64> {
        let tx = self.conn.transaction()?;
        let n = pl_remove(&tx, id, media_ids)?;
        tx.commit()?;
        Ok(n)
    }

    /// Move members (kept in the given order) before `before`, or to the end.
    pub fn playlist_move(
        &mut self,
        id: i64,
        media_ids: &[i64],
        before: Option<i64>,
    ) -> Result<u64> {
        let tx = self.conn.transaction()?;
        let n = pl_move(&tx, id, media_ids, before)?;
        tx.commit()?;
        Ok(n)
    }

    pub fn playlist_sort(
        &mut self,
        id: i64,
        by: PlaylistSort,
        desc: bool,
        seed: u32,
    ) -> Result<()> {
        let tx = self.conn.transaction()?;
        pl_sort(&tx, id, by, desc, seed)?;
        tx.commit()?;
        Ok(())
    }

    /// Force a renumbering (positions become 1024, 2048, ...).
    pub fn playlist_renumber(&mut self, id: i64) -> Result<()> {
        let tx = self.conn.transaction()?;
        pl_exists(&tx, id)?;
        renumber(&tx, id)?;
        tx.commit()?;
        Ok(())
    }

    /// Member ids in playlist order.
    pub fn playlist_item_ids(&self, id: i64) -> Result<Vec<i64>> {
        pl_exists(&self.conn, id)?;
        let mut st = self.conn.prepare(
            "SELECT media_id FROM playlist_items WHERE playlist_id = ?1
             ORDER BY pos, added_at, media_id",
        )?;
        let rows = st.query_map([id], |r| r.get(0))?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    pub fn playlist_len(&self, id: i64) -> Result<u64> {
        pl_exists(&self.conn, id)?;
        Ok(pl_count(&self.conn, id)? as u64)
    }

    /// One page of a playlist in order; missing and hidden members included.
    pub fn playlist_items(&self, id: i64, limit: Option<u64>, offset: u64) -> Result<Vec<Media>> {
        pl_exists(&self.conn, id)?;
        self.query(&self.playlist_filter(id, limit, offset))
    }

    /// The `Filter` that lists a playlist in order.
    pub fn playlist_filter(&self, id: i64, limit: Option<u64>, offset: u64) -> Filter {
        Filter {
            sort: SortKey::Position,
            include_missing: true,
            hidden: crate::db::HiddenMode::Include,
            org: OrgFilter {
                playlist: Some(id),
                ..Default::default()
            },
            limit,
            offset,
            ..Default::default()
        }
    }

    /// Ids of playlists that contain at least one of `media_ids`.
    pub fn playlists_of(&self, media_ids: &[i64]) -> Result<Vec<i64>> {
        check_ids(media_ids)?;
        let mut set: std::collections::BTreeSet<i64> = Default::default();
        let mut st = self
            .conn
            .prepare_cached("SELECT playlist_id FROM playlist_items WHERE media_id = ?1")?;
        for m in media_ids {
            for r in st.query_map([m], |r| r.get::<_, i64>(0))? {
                set.insert(r?);
            }
        }
        Ok(set.into_iter().collect())
    }

    // categories and tags

    pub fn labels(&self, kind: LabelKind) -> Result<Vec<LabelInfo>> {
        labels_list(&self.conn, kind)
    }

    pub fn label_create(
        &mut self,
        kind: LabelKind,
        name: &str,
        parent: Option<i64>,
        color: Option<u32>,
    ) -> Result<i64> {
        let tx = self.conn.transaction()?;
        let id = label_create(&tx, kind, name, parent, color)?;
        tx.commit()?;
        Ok(id)
    }

    pub fn label_rename(&mut self, kind: LabelKind, id: i64, name: &str) -> Result<()> {
        let tx = self.conn.transaction()?;
        label_rename(&tx, kind, id, name)?;
        tx.commit()?;
        Ok(())
    }

    pub fn label_set(
        &mut self,
        kind: LabelKind,
        id: i64,
        color: Option<Option<u32>>,
        private: Option<bool>,
        position: Option<i64>,
    ) -> Result<()> {
        let tx = self.conn.transaction()?;
        label_set(&tx, kind, id, color, private, position)?;
        tx.commit()?;
        Ok(())
    }

    /// Returns the number of memberships dropped. Media is untouched; the
    /// children of a category move up to its parent.
    pub fn label_delete(&mut self, kind: LabelKind, id: i64) -> Result<u64> {
        let tx = self.conn.transaction()?;
        let n = label_delete(&tx, kind, id)?;
        tx.commit()?;
        Ok(n)
    }

    /// Fold `from` into `into`; returns items that newly gained `into`.
    pub fn label_merge(&mut self, kind: LabelKind, from: i64, into: i64) -> Result<u64> {
        let tx = self.conn.transaction()?;
        let n = label_merge(&tx, kind, from, into)?;
        tx.commit()?;
        Ok(n)
    }

    pub fn label_assign(
        &mut self,
        kind: LabelKind,
        labels: &[i64],
        media_ids: &[i64],
        on: bool,
    ) -> Result<u64> {
        let tx = self.conn.transaction()?;
        let n = label_assign(&tx, kind, labels, media_ids, on)?;
        tx.commit()?;
        Ok(n)
    }

    /// Adds the label to all when any lacks it, otherwise removes it from all.
    /// Returns `(now_on, changed)`.
    pub fn label_toggle(
        &mut self,
        kind: LabelKind,
        label: i64,
        media_ids: &[i64],
    ) -> Result<(bool, u64)> {
        let tx = self.conn.transaction()?;
        let r = label_toggle(&tx, kind, label, media_ids)?;
        tx.commit()?;
        Ok(r)
    }

    /// Re-parent a category (`None` = make it a root). Refuses cycles and
    /// anything that would nest deeper than 4.
    pub fn category_move(&mut self, id: i64, parent: Option<i64>) -> Result<()> {
        let tx = self.conn.transaction()?;
        category_move(&tx, id, parent)?;
        tx.commit()?;
        Ok(())
    }

    /// `(tags, categories)` of one item.
    pub fn labels_of(&self, media_id: i64) -> Result<(Vec<LabelInfo>, Vec<LabelInfo>)> {
        let ids: HashSet<i64> = {
            let mut st = self
                .conn
                .prepare("SELECT label_id FROM media_labels WHERE media_id = ?1")?;
            let it = st.query_map([media_id], |r| r.get(0))?;
            it.collect::<std::result::Result<_, _>>()?
        };
        let pick = |kind| -> Result<Vec<LabelInfo>> {
            Ok(labels_list(&self.conn, kind)?
                .into_iter()
                .filter(|l| ids.contains(&l.id))
                .collect())
        };
        Ok((pick(LabelKind::Tag)?, pick(LabelKind::Category)?))
    }

    // ---- m3u8 export ----

    /// Write a UTF-8 `.m3u8` playlist. Never overwrites, never writes with a
    /// media extension; missing files are skipped.
    pub fn export_m3u(&self, id: i64, dest: &Path, o: &M3uOpts) -> Result<M3uReport> {
        pl_exists(&self.conn, id)?;
        crate::portability::check_dest(dest)?;
        let base: Option<PathBuf> = dest
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .map(|p| crate::pathbytes::canonical(p).unwrap_or_else(|_| p.to_path_buf()));
        let mut st = self.conn.prepare(
            "SELECT m.path, m.name, m.duration_ms, m.missing
             FROM playlist_items i JOIN media m ON m.id = i.media_id
             WHERE i.playlist_id = ?1 ORDER BY i.pos, i.added_at, i.media_id",
        )?;
        let rows = st.query_map([id], |r| {
            Ok((
                crate::db::blob_to_path(r.get::<_, Vec<u8>>(0)?),
                r.get::<_, String>(1)?,
                r.get::<_, Option<i64>>(2)?,
                r.get::<_, i64>(3)? != 0,
            ))
        })?;
        let mut rep = M3uReport::default();
        let mut out: Vec<u8> = b"#EXTM3U\n".to_vec();
        for row in rows {
            let (path, name, dur, missing) = row?;
            if missing || !path.exists() {
                rep.skipped_missing += 1;
                continue;
            }
            let shown = match (&base, o.relative) {
                (Some(b), true) => relative_to(&path, b),
                _ => path.clone(),
            };
            let bytes = path_to_blob(&shown);
            if bytes.iter().any(|b| *b == b'\n' || *b == b'\r') {
                rep.skipped_unsafe += 1;
                continue;
            }
            if o.include_names {
                let title: String = name.chars().filter(|c| !c.is_control()).collect();
                let secs = dur.map(|d| d / 1000).unwrap_or(-1);
                out.extend_from_slice(format!("#EXTINF:{secs},{title}\n").as_bytes());
            }
            out.extend_from_slice(&bytes);
            out.push(b'\n');
            rep.written += 1;
        }
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(dest)?;
        f.write_all(&out)?;
        Ok(rep)
    }
}

#[derive(Debug, Clone, Default)]
pub struct M3uOpts {
    /// Paths relative to the playlist file's folder when possible.
    pub relative: bool,
    /// `#EXTINF` lines with file names as titles (names can be sensitive).
    pub include_names: bool,
}

#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct M3uReport {
    pub written: u64,
    pub skipped_missing: u64,
    /// Paths holding a line break cannot be written to M3U.
    pub skipped_unsafe: u64,
}

fn relative_to(path: &Path, base: &Path) -> PathBuf {
    let p: Vec<Component> = path.components().collect();
    let b: Vec<Component> = base.components().collect();
    let common = p.iter().zip(&b).take_while(|(x, y)| x == y).count();
    // No shared root (another drive): keep the absolute path.
    if common == 0 || !path.is_absolute() {
        return path.to_path_buf();
    }
    let mut out = PathBuf::new();
    for _ in common..b.len() {
        out.push("..");
    }
    for c in &p[common..] {
        out.push(c.as_os_str());
    }
    out
}

// ---- commands ----------------------------------------------------------------

fn int_arg(a: &Value, k: &str) -> Result<i64> {
    a.get(k)
        .and_then(Value::as_i64)
        .ok_or_else(|| invalid(format!("missing integer argument `{k}`")))
}

fn str_arg<'a>(a: &'a Value, k: &str) -> Result<&'a str> {
    a.get(k)
        .and_then(Value::as_str)
        .ok_or_else(|| invalid(format!("missing string argument `{k}`")))
}

/// Absent or null = `None`.
fn opt_int(a: &Value, k: &str) -> Result<Option<i64>> {
    match a.get(k) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v
            .as_i64()
            .map(Some)
            .ok_or_else(|| invalid(format!("`{k}` must be an integer"))),
    }
}

fn opt_bool(a: &Value, k: &str) -> Result<Option<bool>> {
    match a.get(k) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v
            .as_bool()
            .map(Some)
            .ok_or_else(|| invalid(format!("`{k}` must be a boolean"))),
    }
}

fn id_list(a: &Value, k: &str) -> Result<Vec<i64>> {
    let arr = a
        .get(k)
        .and_then(Value::as_array)
        .ok_or_else(|| invalid(format!("missing array argument `{k}`")))?;
    if arr.len() > MAX_IDS {
        return Err(invalid(format!("`{k}` takes at most {MAX_IDS} ids")));
    }
    arr.iter()
        .map(|v| {
            v.as_i64()
                .ok_or_else(|| invalid(format!("`{k}` must hold integer ids")))
        })
        .collect()
}

/// `color`: absent = unchanged, null = cleared, number = set.
fn color_patch(a: &Value) -> Result<Option<Option<u32>>> {
    match a.get("color") {
        None => Ok(None),
        Some(Value::Null) => Ok(Some(None)),
        Some(v) => {
            let n = v
                .as_u64()
                .filter(|n| *n <= 0xFF_FFFF)
                .ok_or_else(|| invalid("`color` must be 0..=16777215"))?;
            Ok(Some(Some(n as u32)))
        }
    }
}

fn color_create(a: &Value) -> Result<Option<u32>> {
    Ok(color_patch(a)?.flatten())
}

fn entropy() -> u32 {
    use std::hash::{BuildHasher, Hasher};
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u128(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    );
    h.finish() as u32
}

fn label_cmds<C: HasLibrary>(
    reg: &mut Registry<C>,
    names: [&'static str; 9],
    helps: [&'static str; 9],
    kind: LabelKind,
) {
    // Command handlers are plain fn pointers, so the kind is selected by a
    // per-kind wrapper rather than captured.
    macro_rules! reg {
        ($i:expr, $f:expr) => {
            reg.register(names[$i], helps[$i], $f);
        };
    }
    match kind {
        LabelKind::Tag => {
            reg!(0, |c, a| lc_list(c, a, LabelKind::Tag));
            reg!(1, |c, a| lc_create(c, a, LabelKind::Tag));
            reg!(2, |c, a| lc_rename(c, a, LabelKind::Tag));
            reg!(3, |c, a| lc_set(c, a, LabelKind::Tag));
            reg!(4, |c, a| lc_delete(c, a, LabelKind::Tag));
            reg!(5, |c, a| lc_merge(c, a, LabelKind::Tag));
            reg!(6, |c, a| lc_assign(c, a, LabelKind::Tag));
            reg!(7, |c, a| lc_toggle(c, a, LabelKind::Tag));
        }
        LabelKind::Category => {
            reg!(0, |c, a| lc_list(c, a, LabelKind::Category));
            reg!(1, |c, a| lc_create(c, a, LabelKind::Category));
            reg!(2, |c, a| lc_rename(c, a, LabelKind::Category));
            reg!(3, |c, a| lc_set(c, a, LabelKind::Category));
            reg!(4, |c, a| lc_delete(c, a, LabelKind::Category));
            reg!(5, |c, a| lc_merge(c, a, LabelKind::Category));
            reg!(6, |c, a| lc_assign(c, a, LabelKind::Category));
            reg!(7, |c, a| lc_toggle(c, a, LabelKind::Category));
            reg!(8, |c, a| {
                let id = int_arg(&a, "id")?;
                let parent = opt_int(&a, "parent")?;
                c.db().category_move(id, parent)?;
                Ok(json!({ "id": id, "parent_id": parent }))
            });
        }
    }
}

fn lc_list<C: HasLibrary>(c: &mut C, _a: Value, kind: LabelKind) -> Result<Value> {
    Ok(json!({ "items": c.db().labels(kind)? }))
}

fn lc_create<C: HasLibrary>(c: &mut C, a: Value, kind: LabelKind) -> Result<Value> {
    let name = str_arg(&a, "name")?.to_owned();
    let parent = opt_int(&a, "parent")?;
    let color = color_create(&a)?;
    Ok(json!({ "id": c.db().label_create(kind, &name, parent, color)? }))
}

fn lc_rename<C: HasLibrary>(c: &mut C, a: Value, kind: LabelKind) -> Result<Value> {
    let id = int_arg(&a, "id")?;
    c.db().label_rename(kind, id, str_arg(&a, "name")?)?;
    Ok(json!({ "id": id }))
}

fn lc_set<C: HasLibrary>(c: &mut C, a: Value, kind: LabelKind) -> Result<Value> {
    let id = int_arg(&a, "id")?;
    c.db().label_set(
        kind,
        id,
        color_patch(&a)?,
        opt_bool(&a, "private")?,
        opt_int(&a, "position")?,
    )?;
    Ok(json!({ "id": id }))
}

fn lc_delete<C: HasLibrary>(c: &mut C, a: Value, kind: LabelKind) -> Result<Value> {
    let id = int_arg(&a, "id")?;
    Ok(json!({ "deleted": true, "items": c.db().label_delete(kind, id)? }))
}

fn lc_merge<C: HasLibrary>(c: &mut C, a: Value, kind: LabelKind) -> Result<Value> {
    let (from, into) = (int_arg(&a, "from")?, int_arg(&a, "into")?);
    Ok(json!({ "moved": c.db().label_merge(kind, from, into)? }))
}

fn lc_assign<C: HasLibrary>(c: &mut C, a: Value, kind: LabelKind) -> Result<Value> {
    let ids = id_list(&a, "ids")?;
    let media = id_list(&a, "media_ids")?;
    let on = opt_bool(&a, "on")?.unwrap_or(true);
    Ok(json!({ "changed": c.db().label_assign(kind, &ids, &media, on)? }))
}

fn lc_toggle<C: HasLibrary>(c: &mut C, a: Value, kind: LabelKind) -> Result<Value> {
    let id = int_arg(&a, "id")?;
    let media = id_list(&a, "media_ids")?;
    let (on, changed) = c.db().label_toggle(kind, id, &media)?;
    Ok(json!({ "on": on, "changed": changed }))
}

/// Registers `playlist.*`, `tag.*`, `category.*` and `labels.of`.
pub fn register<C: HasLibrary>(reg: &mut Registry<C>) {
    reg.register("playlist.list", "Playlists with counts", |c, _| {
        Ok(json!({ "items": c.db().playlists()? }))
    });
    reg.register(
        "playlist.create",
        "Create a playlist: {name, color?}",
        |c, a| {
            let name = str_arg(&a, "name")?.to_owned();
            Ok(json!({ "id": c.db().playlist_create(&name, color_create(&a)?)? }))
        },
    );
    reg.register(
        "playlist.rename",
        "Rename a playlist: {id, name}",
        |c, a| {
            let id = int_arg(&a, "id")?;
            c.db().playlist_rename(id, str_arg(&a, "name")?)?;
            Ok(json!({ "id": id }))
        },
    );
    reg.register(
        "playlist.set",
        "Playlist options: {id, color?, private?, position?}",
        |c, a| {
            let id = int_arg(&a, "id")?;
            c.db().playlist_set(
                id,
                color_patch(&a)?,
                opt_bool(&a, "private")?,
                opt_int(&a, "position")?,
            )?;
            Ok(json!({ "id": id }))
        },
    );
    reg.register(
        "playlist.delete",
        "Delete a playlist (media untouched): {id}",
        |c, a| {
            let id = int_arg(&a, "id")?;
            Ok(json!({ "deleted": true, "items": c.db().playlist_delete(id)? }))
        },
    );
    reg.register(
        "playlist.add",
        "Add items: {id, media_ids, before?: media_id}",
        |c, a| {
            let id = int_arg(&a, "id")?;
            let ids = id_list(&a, "media_ids")?;
            let (added, skipped) = c.db().playlist_add(id, &ids, opt_int(&a, "before")?)?;
            Ok(json!({ "added": added, "skipped": skipped }))
        },
    );
    reg.register(
        "playlist.remove",
        "Remove items: {id, media_ids}",
        |c, a| {
            let id = int_arg(&a, "id")?;
            let ids = id_list(&a, "media_ids")?;
            Ok(json!({ "removed": c.db().playlist_remove(id, &ids)? }))
        },
    );
    reg.register(
        "playlist.move",
        "Reorder: {id, media_ids, before?: media_id|null (end)}",
        |c, a| {
            let id = int_arg(&a, "id")?;
            let ids = id_list(&a, "media_ids")?;
            Ok(json!({ "moved": c.db().playlist_move(id, &ids, opt_int(&a, "before")?)? }))
        },
    );
    reg.register(
        "playlist.items",
        "Items in order: {id, limit?, offset?}; count = page length, total = all",
        |c, a| {
            let id = int_arg(&a, "id")?;
            let limit = a.get("limit").and_then(Value::as_u64);
            let offset = a.get("offset").and_then(Value::as_u64).unwrap_or(0);
            let items = c.db().playlist_items(id, limit, offset)?;
            let total = c.db().playlist_len(id)?;
            Ok(json!({ "count": items.len(), "total": total, "items": items }))
        },
    );
    reg.register(
        "playlist.of",
        "Playlists containing any of: {media_ids}",
        |c, a| {
            let ids = id_list(&a, "media_ids")?;
            Ok(json!({ "ids": c.db().playlists_of(&ids)? }))
        },
    );
    reg.register(
        "playlist.sort",
        "Reorder by a key: {id, by: name|added|duration|random, desc?}",
        |c, a| {
            let id = int_arg(&a, "id")?;
            let by = PlaylistSort::parse(str_arg(&a, "by")?)
                .ok_or_else(|| invalid("`by` must be name, added, duration or random"))?;
            let desc = opt_bool(&a, "desc")?.unwrap_or(false);
            c.db().playlist_sort(id, by, desc, entropy())?;
            Ok(json!({ "id": id }))
        },
    );
    reg.register(
        "playlist.export_m3u",
        "Write an .m3u8 file: {id, dest, relative?, include_names?}",
        |c, a| {
            let id = int_arg(&a, "id")?;
            let dest = str_arg(&a, "dest")?.to_owned();
            let o = M3uOpts {
                relative: opt_bool(&a, "relative")?.unwrap_or(false),
                include_names: opt_bool(&a, "include_names")?.unwrap_or(false),
            };
            let rep = c.db().export_m3u(id, Path::new(&dest), &o)?;
            Ok(json!({
                "written": rep.written,
                "skipped_missing": rep.skipped_missing,
                "skipped_unsafe": rep.skipped_unsafe,
            }))
        },
    );
    label_cmds(
        reg,
        [
            "tag.list",
            "tag.create",
            "tag.rename",
            "tag.set",
            "tag.delete",
            "tag.merge",
            "tag.assign",
            "tag.toggle",
            "",
        ],
        [
            "Tags with counts",
            "Create a tag: {name, color?}",
            "Rename a tag: {id, name}",
            "Tag options: {id, color?, private?, position?}",
            "Delete a tag (media untouched): {id}",
            "Merge a tag into another: {from, into}",
            "Add or remove tags: {ids, media_ids, on?}",
            "Toggle a tag on items: {id, media_ids}",
            "",
        ],
        LabelKind::Tag,
    );
    label_cmds(
        reg,
        [
            "category.list",
            "category.create",
            "category.rename",
            "category.set",
            "category.delete",
            "category.merge",
            "category.assign",
            "category.toggle",
            "category.move",
        ],
        [
            "Categories, flat in tree order, with depth, count, total",
            "Create a category: {name, parent?, color?}",
            "Rename a category: {id, name}",
            "Category options: {id, color?, private?, position?}",
            "Delete a category, children move up: {id}",
            "Merge a category into another: {from, into}",
            "Add or remove categories: {ids, media_ids, on?}",
            "Toggle a category on items: {id, media_ids}",
            "Re-parent a category: {id, parent|null}",
        ],
        LabelKind::Category,
    );
    reg.register(
        "labels.of",
        "Tags and categories of one item: {media_id}",
        |c, a| {
            let id = int_arg(&a, "media_id")?;
            let (tags, categories) = c.db().labels_of(id)?;
            Ok(json!({ "tags": tags, "categories": categories }))
        },
    );
}

// Crate-internal entry points for portability.
pub(crate) fn tx_label_create(
    c: &Connection,
    kind: LabelKind,
    name: &str,
    parent: Option<i64>,
    color: Option<u32>,
) -> Result<i64> {
    label_create(c, kind, name, parent, color)
}

pub(crate) fn tx_label_assign(
    c: &Connection,
    kind: LabelKind,
    label: i64,
    media: i64,
) -> Result<()> {
    label_assign(c, kind, &[label], &[media], true).map(|_| ())
}

pub(crate) fn tx_playlist_add(c: &Connection, pl: i64, media: &[i64]) -> Result<u64> {
    pl_add(c, pl, media, None).map(|(a, _)| a)
}

pub(crate) fn tx_label_set_private(c: &Connection, id: i64, private: bool) -> Result<()> {
    c.execute(
        "UPDATE labels SET private = ?2 WHERE id = ?1",
        params![id, private],
    )?;
    Ok(())
}
