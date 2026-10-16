//! Descriptive metadata kept in the database: a title, a release date, a
//! studio, performers (with aliases) and external ids per item, plus aliases
//! for tags. Nothing here touches a file.
//!
//! * Performers are a name, a disambiguation, aliases, `favorite` and
//!   `private`. Nothing else is stored (no birth date, gender, body
//!   attributes or images; see AGENTS.md).
//! * Private performers and studios are never indexed for search.
//! * Names go through [`crate::organize::clean_name`]; titles are 1 to 200
//!   printable characters.
//! * Aliases resolve names on import: `blow job` finds the tag `blowjob` when
//!   it is an alias of it. An alias never equals a tag name, and a tag name
//!   never equals an alias.

use crate::commands::{HasLibrary, Registry};
use crate::db::{Db, now_secs};
use crate::error::{Error, Result};
use crate::organize::{LabelKind, clean_name};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::HashSet;

pub const MAX_PEOPLE: i64 = 100_000;
pub const MAX_STUDIOS: i64 = 10_000;
pub const MAX_ALIASES: i64 = 32;
pub const MAX_PEOPLE_PER_ITEM: i64 = 64;
pub const MAX_TITLE_CHARS: usize = 200;
pub const MAX_IDS: usize = 10_000;

fn invalid(m: impl Into<String>) -> Error {
    Error::Invalid(m.into())
}

/// Change to an optional text field.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Patch<T> {
    #[default]
    Keep,
    Clear,
    Set(T),
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Person {
    pub id: i64,
    pub name: String,
    pub disambiguation: String,
    pub aliases: Vec<String>,
    pub favorite: bool,
    pub private: bool,
    /// Items this performer appears in.
    pub count: u64,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Studio {
    pub id: i64,
    pub name: String,
    pub private: bool,
    pub count: u64,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ExternalId {
    pub scheme: String,
    pub value: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct MediaInfo {
    pub id: i64,
    pub title: Option<String>,
    pub released: Option<String>,
    pub studio: Option<Studio>,
    pub people: Vec<Person>,
    pub external_ids: Vec<ExternalId>,
}

// ---- validation -------------------------------------------------------------

/// Trim, drop control characters, check 1..=200 characters.
pub fn clean_title(raw: &str) -> Result<String> {
    let t: String = raw.chars().filter(|c| !c.is_control()).collect();
    let t = t.trim();
    let n = t.chars().count();
    if n == 0 || n > MAX_TITLE_CHARS {
        return Err(invalid("title must be 1-200 characters"));
    }
    Ok(t.to_owned())
}

/// `YYYY-MM-DD`, a real calendar date.
pub fn clean_date(raw: &str) -> Result<String> {
    let s = raw.trim();
    let b = s.as_bytes();
    let ok_shape = b.len() == 10
        && b[4] == b'-'
        && b[7] == b'-'
        && b.iter()
            .enumerate()
            .all(|(i, c)| i == 4 || i == 7 || c.is_ascii_digit());
    if !ok_shape {
        return Err(invalid("date must look like YYYY-MM-DD"));
    }
    let (y, m, d): (u32, u32, u32) = (
        s[..4].parse().unwrap_or(0),
        s[5..7].parse().unwrap_or(0),
        s[8..].parse().unwrap_or(0),
    );
    let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
    let dim = match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => 0,
    };
    if y == 0 || d == 0 || d > dim {
        return Err(invalid("not a calendar date"));
    }
    Ok(s.to_owned())
}

/// Scheme of an external id: `[a-z0-9._-]{1,32}` after lowercasing; anything
/// else is replaced by `-`. `None` when nothing is left.
pub fn clean_scheme(raw: &str) -> Option<String> {
    let s: String = raw
        .trim()
        .to_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '-'
            }
        })
        .take(32)
        .collect();
    if s.is_empty() || s.chars().all(|c| c == '-') {
        None
    } else {
        Some(s)
    }
}

fn clean_value(raw: &str) -> Option<String> {
    let v: String = raw.chars().filter(|c| !c.is_control()).collect();
    let v = v.trim();
    let n = v.chars().count();
    (1..=128).contains(&n).then(|| v.to_owned())
}

fn map_unique(e: rusqlite::Error, what: &str) -> Error {
    match &e {
        rusqlite::Error::SqliteFailure(f, _)
            if f.code == rusqlite::ErrorCode::ConstraintViolation =>
        {
            invalid(format!("{what} is already used"))
        }
        _ => Error::Db(e),
    }
}

fn dedup(ids: &[i64]) -> Vec<i64> {
    let mut seen = HashSet::with_capacity(ids.len());
    ids.iter().copied().filter(|i| seen.insert(*i)).collect()
}

fn check_ids(ids: &[i64]) -> Result<()> {
    if ids.len() > MAX_IDS {
        return Err(invalid(format!("at most {MAX_IDS} items per call")));
    }
    Ok(())
}

fn count(c: &Connection, sql: &str, id: i64) -> Result<i64> {
    Ok(c.query_row(sql, [id], |r| r.get(0))?)
}

// ---- studios ----------------------------------------------------------------

fn studio_exists(c: &Connection, id: i64) -> Result<()> {
    c.query_row("SELECT 1 FROM studios WHERE id = ?1", [id], |_| Ok(()))
        .optional()?
        .ok_or_else(|| Error::NotFound(format!("studio {id}")))
}

fn studio_create(c: &Connection, name: &str) -> Result<i64> {
    let (name, lc) = clean_name(name)?;
    let n: i64 = c.query_row("SELECT COUNT(*) FROM studios", [], |r| r.get(0))?;
    if n >= MAX_STUDIOS {
        return Err(invalid(format!("at most {MAX_STUDIOS} studios")));
    }
    c.execute(
        "INSERT INTO studios(name, name_lc, created_at) VALUES (?1, ?2, ?3)",
        params![name, lc, now_secs()],
    )
    .map_err(|e| map_unique(e, "studio name"))?;
    Ok(c.last_insert_rowid())
}

/// Existing studio by name, or a new one. `(id, created)`.
pub(crate) fn tx_resolve_studio(c: &Connection, name: &str) -> Result<(i64, bool)> {
    let (_, lc) = clean_name(name)?;
    if let Some(id) = c
        .query_row("SELECT id FROM studios WHERE name_lc = ?1", [&lc], |r| {
            r.get(0)
        })
        .optional()?
    {
        return Ok((id, false));
    }
    Ok((studio_create(c, name)?, true))
}

fn studios_list(c: &Connection) -> Result<Vec<Studio>> {
    let mut st = c.prepare(
        "SELECT s.id, s.name, s.private,
                (SELECT COUNT(*) FROM media m WHERE m.studio_id = s.id)
         FROM studios s ORDER BY s.name_lc, s.id",
    )?;
    let rows = st.query_map([], |r| {
        Ok(Studio {
            id: r.get(0)?,
            name: r.get(1)?,
            private: r.get::<_, i64>(2)? != 0,
            count: r.get::<_, i64>(3)?.max(0) as u64,
        })
    })?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

fn studio_get(c: &Connection, id: i64) -> Result<Studio> {
    c.query_row(
        "SELECT s.id, s.name, s.private,
                (SELECT COUNT(*) FROM media m WHERE m.studio_id = s.id)
         FROM studios s WHERE s.id = ?1",
        [id],
        |r| {
            Ok(Studio {
                id: r.get(0)?,
                name: r.get(1)?,
                private: r.get::<_, i64>(2)? != 0,
                count: r.get::<_, i64>(3)?.max(0) as u64,
            })
        },
    )
    .optional()?
    .ok_or_else(|| Error::NotFound(format!("studio {id}")))
}

// ---- people -----------------------------------------------------------------

pub(crate) fn person_exists(c: &Connection, id: i64) -> Result<()> {
    c.query_row("SELECT 1 FROM people WHERE id = ?1", [id], |_| Ok(()))
        .optional()?
        .ok_or_else(|| Error::NotFound(format!("performer {id}")))
}

fn clean_disambiguation(raw: &str) -> Result<String> {
    let t = raw.trim();
    if t.chars().count() > 60 || t.chars().any(char::is_control) {
        return Err(invalid(
            "disambiguation must be at most 60 printable characters",
        ));
    }
    Ok(t.to_owned())
}

fn person_create(c: &Connection, name: &str, disambiguation: &str) -> Result<i64> {
    let (name, lc) = clean_name(name)?;
    let dis = clean_disambiguation(disambiguation)?;
    let n: i64 = c.query_row("SELECT COUNT(*) FROM people", [], |r| r.get(0))?;
    if n >= MAX_PEOPLE {
        return Err(invalid(format!("at most {MAX_PEOPLE} performers")));
    }
    c.execute(
        "INSERT INTO people(name, name_lc, disambiguation, created_at) VALUES (?1, ?2, ?3, ?4)",
        params![name, lc, dis, now_secs()],
    )
    .map_err(|e| map_unique(e, "performer name"))?;
    Ok(c.last_insert_rowid())
}

/// Outcome of looking a performer name up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Resolved {
    Found(i64),
    Created(i64),
    /// Several performers answer to this name: the caller skips it.
    Ambiguous,
}

/// With a disambiguation: exactly that performer, or a new one. Without: the
/// only performer of that name (or the only one with an empty
/// disambiguation), else the only performer with that alias, else a new one.
pub(crate) fn tx_resolve_person(
    c: &Connection,
    name: &str,
    disambiguation: &str,
) -> Result<Resolved> {
    let (_, lc) = clean_name(name)?;
    let dis = clean_disambiguation(disambiguation)?;
    let ids = |sql: &str, args: &[&dyn rusqlite::ToSql]| -> Result<Vec<i64>> {
        let mut st = c.prepare_cached(sql)?;
        let rows = st.query_map(args, |r| r.get(0))?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    };
    if !dis.is_empty() {
        let found = ids(
            "SELECT id FROM people WHERE name_lc = ?1 AND disambiguation = ?2",
            &[&lc, &dis],
        )?;
        return Ok(match found.first() {
            Some(id) => Resolved::Found(*id),
            None => Resolved::Created(person_create(c, name, &dis)?),
        });
    }
    let named = ids(
        "SELECT id FROM people WHERE name_lc = ?1 ORDER BY id",
        &[&lc],
    )?;
    match named.as_slice() {
        [one] => return Ok(Resolved::Found(*one)),
        [] => {}
        _ => {
            let plain = ids(
                "SELECT id FROM people WHERE name_lc = ?1 AND disambiguation = ''",
                &[&lc],
            )?;
            return Ok(match plain.as_slice() {
                [one] => Resolved::Found(*one),
                _ => Resolved::Ambiguous,
            });
        }
    }
    let by_alias = ids(
        "SELECT DISTINCT person_id FROM person_aliases WHERE alias_lc = ?1 ORDER BY person_id",
        &[&lc],
    )?;
    match by_alias.as_slice() {
        [] => Ok(Resolved::Created(person_create(c, name, "")?)),
        [one] => Ok(Resolved::Found(*one)),
        _ => Ok(Resolved::Ambiguous),
    }
}

fn aliases_of(c: &Connection, id: i64) -> Result<Vec<String>> {
    let mut st = c.prepare_cached(
        "SELECT alias FROM person_aliases WHERE person_id = ?1 ORDER BY alias_lc",
    )?;
    let rows = st.query_map([id], |r| r.get(0))?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

const PERSON_SELECT: &str = "SELECT p.id, p.name, p.disambiguation, p.favorite, p.private,
        (SELECT COUNT(*) FROM media_people mp WHERE mp.person_id = p.id) FROM people p";

fn person_row(c: &Connection, r: &rusqlite::Row<'_>) -> Result<Person> {
    let id: i64 = r.get(0)?;
    Ok(Person {
        id,
        name: r.get(1)?,
        disambiguation: r.get(2)?,
        aliases: aliases_of(c, id)?,
        favorite: r.get::<_, i64>(3)? != 0,
        private: r.get::<_, i64>(4)? != 0,
        count: r.get::<_, i64>(5)?.max(0) as u64,
    })
}

fn person_get(c: &Connection, id: i64) -> Result<Person> {
    let mut st = c.prepare(&format!("{PERSON_SELECT} WHERE p.id = ?1"))?;
    let mut rows = st.query([id])?;
    match rows.next()? {
        Some(r) => person_row(c, r),
        None => Err(Error::NotFound(format!("performer {id}"))),
    }
}

fn people_list(c: &Connection, query: Option<&str>, limit: i64) -> Result<Vec<Person>> {
    let limit = limit.clamp(1, 1000);
    let mut st;
    let mut rows = match query.map(str::trim).filter(|q| !q.is_empty()) {
        Some(q) => {
            let esc = q
                .to_lowercase()
                .replace('\\', "\\\\")
                .replace('%', "\\%")
                .replace('_', "\\_");
            st = c.prepare(&format!(
                "{PERSON_SELECT} WHERE p.name_lc LIKE ?1 ESCAPE '\\'
                 OR EXISTS (SELECT 1 FROM person_aliases a WHERE a.person_id = p.id
                            AND a.alias_lc LIKE ?1 ESCAPE '\\')
                 ORDER BY p.name_lc, p.id LIMIT ?2"
            ))?;
            st.query(params![format!("%{esc}%"), limit])?
        }
        None => {
            st = c.prepare(&format!(
                "{PERSON_SELECT} ORDER BY p.name_lc, p.id LIMIT ?1"
            ))?;
            st.query(params![limit])?
        }
    };
    let mut out = Vec::new();
    while let Some(r) = rows.next()? {
        out.push(person_row(c, r)?);
    }
    Ok(out)
}

pub(crate) fn person_alias_add_tx(c: &Connection, id: i64, alias: &str) -> Result<bool> {
    alias_add_person(c, id, alias)
}

fn alias_add_person(c: &Connection, id: i64, alias: &str) -> Result<bool> {
    let (alias, lc) = clean_name(alias)?;
    let own: String = c
        .query_row("SELECT name_lc FROM people WHERE id = ?1", [id], |r| {
            r.get(0)
        })
        .optional()?
        .ok_or_else(|| Error::NotFound(format!("performer {id}")))?;
    if own == lc {
        return Ok(false);
    }
    let have = count(
        c,
        "SELECT COUNT(*) FROM person_aliases WHERE person_id = ?1",
        id,
    )?;
    if have >= MAX_ALIASES {
        return Err(invalid(format!(
            "at most {MAX_ALIASES} aliases per performer"
        )));
    }
    let n = c.execute(
        "INSERT OR IGNORE INTO person_aliases(person_id, alias, alias_lc) VALUES (?1, ?2, ?3)",
        params![id, alias, lc],
    )?;
    Ok(n > 0)
}

/// Link a performer to an item at `position` (default: after the last one).
/// `Ok(false)` when already linked.
pub(crate) fn tx_person_link(
    c: &Connection,
    media: i64,
    person: i64,
    position: Option<i64>,
) -> Result<bool> {
    let have = count(
        c,
        "SELECT COUNT(*) FROM media_people WHERE media_id = ?1",
        media,
    )?;
    let linked: bool = c
        .query_row(
            "SELECT 1 FROM media_people WHERE media_id = ?1 AND person_id = ?2",
            params![media, person],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if linked {
        return Ok(false);
    }
    if have >= MAX_PEOPLE_PER_ITEM {
        return Err(invalid(format!(
            "at most {MAX_PEOPLE_PER_ITEM} performers per item"
        )));
    }
    let pos = match position {
        Some(p) => p.clamp(0, 1_000_000),
        None => c.query_row(
            "SELECT COALESCE(MAX(position) + 1, 0) FROM media_people WHERE media_id = ?1",
            [media],
            |r| r.get(0),
        )?,
    };
    let n = c.execute(
        "INSERT OR IGNORE INTO media_people(media_id, person_id, position)
         SELECT id, ?2, ?3 FROM media WHERE id = ?1",
        params![media, person, pos],
    )?;
    Ok(n > 0)
}

fn person_assign(c: &Connection, ids: &[i64], media: &[i64], on: bool) -> Result<u64> {
    check_ids(media)?;
    if ids.len() > 64 {
        return Err(invalid("at most 64 performers per call"));
    }
    let ids = dedup(ids);
    for p in &ids {
        person_exists(c, *p)?;
    }
    let media = dedup(media);
    let mut changed = 0u64;
    for m in &media {
        for p in &ids {
            if on {
                changed += tx_person_link(c, *m, *p, None)? as u64;
            } else {
                changed += c.execute(
                    "DELETE FROM media_people WHERE media_id = ?1 AND person_id = ?2",
                    params![m, p],
                )? as u64;
            }
        }
    }
    Ok(changed)
}

fn person_merge(c: &Connection, from: i64, into: i64) -> Result<u64> {
    if from == into {
        return Err(invalid("cannot merge into itself"));
    }
    person_exists(c, from)?;
    person_exists(c, into)?;
    let moved = c.execute(
        "INSERT OR IGNORE INTO media_people(media_id, person_id, position)
         SELECT media_id, ?2, position FROM media_people WHERE person_id = ?1",
        params![from, into],
    )? as u64;
    // Names the old record answered to keep working.
    let (from_name, from_aliases) = {
        let n: String = c.query_row("SELECT name FROM people WHERE id = ?1", [from], |r| {
            r.get(0)
        })?;
        (n, aliases_of(c, from)?)
    };
    for a in std::iter::once(from_name).chain(from_aliases) {
        // Over the cap or a duplicate: skipped, the merge itself goes on.
        let _ = alias_add_person(c, into, &a);
    }
    c.execute(
        "UPDATE people SET
            favorite = MAX(favorite, (SELECT favorite FROM people WHERE id = ?1)),
            private = MAX(private, (SELECT private FROM people WHERE id = ?1))
         WHERE id = ?2",
        params![from, into],
    )?;
    c.execute("DELETE FROM people WHERE id = ?1", [from])?;
    Ok(moved)
}

// ---- tag aliases ------------------------------------------------------------

fn tag_lc(c: &Connection, id: i64) -> Result<String> {
    c.query_row(
        "SELECT name_lc FROM labels WHERE id = ?1 AND kind = 2",
        [id],
        |r| r.get(0),
    )
    .optional()?
    .ok_or_else(|| Error::NotFound(format!("tag {id}")))
}

pub(crate) fn tag_alias_add(c: &Connection, id: i64, alias: &str) -> Result<()> {
    let (alias, lc) = clean_name(alias)?;
    let own = tag_lc(c, id)?;
    if own == lc {
        return Err(invalid("an alias cannot equal the tag name"));
    }
    let taken: bool = c
        .query_row(
            "SELECT 1 FROM labels WHERE kind = 2 AND name_lc = ?1",
            [&lc],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if taken {
        return Err(invalid("an alias cannot equal another tag name"));
    }
    let have = count(
        c,
        "SELECT COUNT(*) FROM label_aliases WHERE label_id = ?1",
        id,
    )?;
    if have >= MAX_ALIASES {
        return Err(invalid(format!("at most {MAX_ALIASES} aliases per tag")));
    }
    c.execute(
        "INSERT INTO label_aliases(label_id, alias, alias_lc) VALUES (?1, ?2, ?3)",
        params![id, alias, lc],
    )
    .map_err(|e| map_unique(e, "alias"))?;
    Ok(())
}

pub(crate) fn tag_aliases_of(c: &Connection, id: i64) -> Result<Vec<String>> {
    let mut st =
        c.prepare_cached("SELECT alias FROM label_aliases WHERE label_id = ?1 ORDER BY alias_lc")?;
    let rows = st.query_map([id], |r| r.get(0))?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

/// Existing tag by name or alias, or a new one. `(id, created)`.
pub(crate) fn tx_resolve_tag(c: &Connection, name: &str) -> Result<(i64, bool)> {
    let (name, lc) = clean_name(name)?;
    if let Some(id) = c
        .query_row(
            "SELECT id FROM labels WHERE kind = 2 AND name_lc = ?1",
            [&lc],
            |r| r.get(0),
        )
        .optional()?
    {
        return Ok((id, false));
    }
    if let Some(id) = c
        .query_row(
            "SELECT label_id FROM label_aliases WHERE alias_lc = ?1",
            [&lc],
            |r| r.get(0),
        )
        .optional()?
    {
        return Ok((id, false));
    }
    let id = crate::organize::tx_label_create(c, LabelKind::Tag, &name, None, None)?;
    Ok((id, true))
}

/// Used by `organize::label_merge`: the merged-away tag's name and aliases
/// keep resolving to `into`. Best effort (alias cap, duplicates).
pub(crate) fn tx_tag_merge_aliases(c: &Connection, from: i64, into: i64) -> Result<()> {
    let from_name: String = c.query_row("SELECT name FROM labels WHERE id = ?1", [from], |r| {
        r.get(0)
    })?;
    let mut names = vec![from_name];
    names.extend(tag_aliases_of(c, from)?);
    c.execute("DELETE FROM label_aliases WHERE label_id = ?1", [from])?;
    for n in names {
        let Ok((alias, lc)) = clean_name(&n) else {
            continue;
        };
        if tag_lc(c, into)? == lc {
            continue;
        }
        let have = count(
            c,
            "SELECT COUNT(*) FROM label_aliases WHERE label_id = ?1",
            into,
        )?;
        if have >= MAX_ALIASES {
            break;
        }
        c.execute(
            "INSERT OR IGNORE INTO label_aliases(label_id, alias, alias_lc) VALUES (?1, ?2, ?3)",
            params![into, alias, lc],
        )?;
    }
    Ok(())
}

// ---- external ids -----------------------------------------------------------

/// Sets an external id unless the item already has one for the scheme.
/// `Ok(true)` when stored.
pub(crate) fn tx_external_id_add(
    c: &Connection,
    media: i64,
    scheme: &str,
    value: &str,
) -> Result<bool> {
    let (Some(scheme), Some(value)) = (clean_scheme(scheme), clean_value(value)) else {
        return Ok(false);
    };
    let n = c.execute(
        "INSERT OR IGNORE INTO external_ids(media_id, scheme, value)
         SELECT id, ?2, ?3 FROM media WHERE id = ?1",
        params![media, scheme, value],
    )?;
    Ok(n > 0)
}

// ---- media info -------------------------------------------------------------

fn media_info(c: &Connection, id: i64) -> Result<MediaInfo> {
    let (title, released, studio_id): (Option<String>, Option<String>, Option<i64>) = c
        .query_row(
            "SELECT title, released, studio_id FROM media WHERE id = ?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?
        .ok_or_else(|| Error::NotFound(format!("media id {id}")))?;
    let studio = match studio_id {
        Some(s) => Some(studio_get(c, s)?),
        None => None,
    };
    let mut people = Vec::new();
    {
        let mut st = c.prepare(&format!(
            "{PERSON_SELECT} JOIN media_people l ON l.person_id = p.id
             WHERE l.media_id = ?1 ORDER BY l.position, p.id"
        ))?;
        let mut rows = st.query([id])?;
        while let Some(r) = rows.next()? {
            people.push(person_row(c, r)?);
        }
    }
    let mut st =
        c.prepare("SELECT scheme, value FROM external_ids WHERE media_id = ?1 ORDER BY scheme")?;
    let rows = st.query_map([id], |r| {
        Ok(ExternalId {
            scheme: r.get(0)?,
            value: r.get(1)?,
        })
    })?;
    Ok(MediaInfo {
        id,
        title,
        released,
        studio,
        people,
        external_ids: rows.collect::<std::result::Result<_, _>>()?,
    })
}

fn set_info(
    c: &Connection,
    id: i64,
    title: &Patch<String>,
    released: &Patch<String>,
) -> Result<()> {
    let title = match title {
        Patch::Keep => None,
        Patch::Clear => Some(None),
        Patch::Set(t) => Some(Some(clean_title(t)?)),
    };
    let released = match released {
        Patch::Keep => None,
        Patch::Clear => Some(None),
        Patch::Set(d) => Some(Some(clean_date(d)?)),
    };
    let exists: bool = c
        .query_row("SELECT 1 FROM media WHERE id = ?1", [id], |_| Ok(()))
        .optional()?
        .is_some();
    if !exists {
        return Err(Error::NotFound(format!("media id {id}")));
    }
    if let Some(t) = title {
        c.execute("UPDATE media SET title = ?2 WHERE id = ?1", params![id, t])?;
    }
    if let Some(d) = released {
        c.execute(
            "UPDATE media SET released = ?2 WHERE id = ?1",
            params![id, d],
        )?;
    }
    Ok(())
}

fn studio_assign(c: &Connection, media: &[i64], studio: Option<i64>) -> Result<u64> {
    check_ids(media)?;
    if let Some(s) = studio {
        studio_exists(c, s)?;
    }
    let mut changed = 0u64;
    for m in dedup(media) {
        changed += c.execute(
            "UPDATE media SET studio_id = ?2 WHERE id = ?1 AND studio_id IS NOT ?2",
            params![m, studio],
        )? as u64;
    }
    Ok(changed)
}

fn studio_merge(c: &Connection, from: i64, into: i64) -> Result<u64> {
    if from == into {
        return Err(invalid("cannot merge into itself"));
    }
    studio_exists(c, from)?;
    studio_exists(c, into)?;
    let moved = c.execute(
        "UPDATE media SET studio_id = ?2 WHERE studio_id = ?1",
        params![from, into],
    )? as u64;
    c.execute(
        "UPDATE studios SET private = MAX(private, (SELECT private FROM studios WHERE id = ?1))
         WHERE id = ?2",
        params![from, into],
    )?;
    c.execute("DELETE FROM studios WHERE id = ?1", [from])?;
    Ok(moved)
}

// ---- Db API -------------------------------------------------------------------

macro_rules! in_tx {
    ($self:ident, |$tx:ident| $body:expr) => {{
        let $tx = $self.conn.transaction()?;
        let out: Result<_> = $body;
        let out = out?;
        $tx.commit()?;
        Ok(out)
    }};
}

impl Db {
    pub fn media_info(&self, id: i64) -> Result<MediaInfo> {
        media_info(&self.conn, id)
    }

    pub fn set_info(
        &mut self,
        id: i64,
        title: Patch<String>,
        released: Patch<String>,
    ) -> Result<()> {
        in_tx!(self, |tx| set_info(&tx, id, &title, &released))
    }

    pub fn studios(&self) -> Result<Vec<Studio>> {
        studios_list(&self.conn)
    }

    pub fn studio_create(&mut self, name: &str) -> Result<i64> {
        in_tx!(self, |tx| studio_create(&tx, name))
    }

    pub fn studio_rename(&mut self, id: i64, name: &str) -> Result<()> {
        in_tx!(self, |tx| {
            let (name, lc) = clean_name(name)?;
            let n = tx
                .execute(
                    "UPDATE studios SET name = ?2, name_lc = ?3 WHERE id = ?1",
                    params![id, name, lc],
                )
                .map_err(|e| map_unique(e, "studio name"))?;
            if n == 0 {
                Err(Error::NotFound(format!("studio {id}")))
            } else {
                Ok(())
            }
        })
    }

    pub fn studio_set(&mut self, id: i64, private: Option<bool>) -> Result<()> {
        in_tx!(self, |tx| {
            studio_exists(&tx, id)?;
            if let Some(p) = private {
                tx.execute(
                    "UPDATE studios SET private = ?2 WHERE id = ?1",
                    params![id, p],
                )?;
            }
            Ok(())
        })
    }

    /// Items keep their other data and lose only the studio.
    pub fn studio_delete(&mut self, id: i64) -> Result<u64> {
        in_tx!(self, |tx| {
            let n = studio_get(&tx, id)?.count;
            tx.execute("DELETE FROM studios WHERE id = ?1", [id])?;
            Ok(n)
        })
    }

    pub fn studio_merge(&mut self, from: i64, into: i64) -> Result<u64> {
        in_tx!(self, |tx| studio_merge(&tx, from, into))
    }

    /// Set or clear (`None`) the studio of several items; returns items changed.
    pub fn studio_assign(&mut self, media_ids: &[i64], studio: Option<i64>) -> Result<u64> {
        in_tx!(self, |tx| studio_assign(&tx, media_ids, studio))
    }

    pub fn people(&self, query: Option<&str>, limit: u64) -> Result<Vec<Person>> {
        people_list(&self.conn, query, limit.min(1000) as i64)
    }

    pub fn person(&self, id: i64) -> Result<Person> {
        person_get(&self.conn, id)
    }

    pub fn person_create(&mut self, name: &str, disambiguation: &str) -> Result<i64> {
        in_tx!(self, |tx| person_create(&tx, name, disambiguation))
    }

    pub fn person_rename(&mut self, id: i64, name: &str) -> Result<()> {
        in_tx!(self, |tx| {
            let (name, lc) = clean_name(name)?;
            let n = tx
                .execute(
                    "UPDATE people SET name = ?2, name_lc = ?3 WHERE id = ?1",
                    params![id, name, lc],
                )
                .map_err(|e| map_unique(e, "performer name"))?;
            if n == 0 {
                Err(Error::NotFound(format!("performer {id}")))
            } else {
                Ok(())
            }
        })
    }

    pub fn person_set(
        &mut self,
        id: i64,
        favorite: Option<bool>,
        private: Option<bool>,
        disambiguation: Option<&str>,
    ) -> Result<()> {
        in_tx!(self, |tx| {
            person_exists(&tx, id)?;
            if let Some(f) = favorite {
                tx.execute(
                    "UPDATE people SET favorite = ?2 WHERE id = ?1",
                    params![id, f],
                )?;
            }
            if let Some(p) = private {
                tx.execute(
                    "UPDATE people SET private = ?2 WHERE id = ?1",
                    params![id, p],
                )?;
            }
            if let Some(d) = disambiguation {
                tx.execute(
                    "UPDATE people SET disambiguation = ?2 WHERE id = ?1",
                    params![id, clean_disambiguation(d)?],
                )
                .map_err(|e| map_unique(e, "performer name"))?;
            }
            Ok(())
        })
    }

    /// Returns the item links dropped. Items are untouched.
    pub fn person_delete(&mut self, id: i64) -> Result<u64> {
        in_tx!(self, |tx| {
            let n = person_get(&tx, id)?.count;
            tx.execute("DELETE FROM people WHERE id = ?1", [id])?;
            Ok(n)
        })
    }

    /// Fold `from` into `into`: links move, `from`'s name and aliases become
    /// aliases of `into`. Returns links newly gained by `into`.
    pub fn person_merge(&mut self, from: i64, into: i64) -> Result<u64> {
        in_tx!(self, |tx| person_merge(&tx, from, into))
    }

    pub fn person_alias_add(&mut self, id: i64, alias: &str) -> Result<()> {
        in_tx!(self, |tx| {
            if alias_add_person(&tx, id, alias)? {
                Ok(())
            } else {
                Err(invalid("alias equals the name or is already used here"))
            }
        })
    }

    pub fn person_alias_remove(&mut self, id: i64, alias: &str) -> Result<bool> {
        in_tx!(self, |tx| {
            person_exists(&tx, id)?;
            let (_, lc) = clean_name(alias)?;
            Ok(tx.execute(
                "DELETE FROM person_aliases WHERE person_id = ?1 AND alias_lc = ?2",
                params![id, lc],
            )? > 0)
        })
    }

    /// Add (`on`) or remove performers on items; returns links changed.
    pub fn person_assign(&mut self, ids: &[i64], media_ids: &[i64], on: bool) -> Result<u64> {
        in_tx!(self, |tx| person_assign(&tx, ids, media_ids, on))
    }

    pub fn tag_alias_add(&mut self, tag: i64, alias: &str) -> Result<()> {
        in_tx!(self, |tx| tag_alias_add(&tx, tag, alias))
    }

    pub fn tag_alias_remove(&mut self, tag: i64, alias: &str) -> Result<bool> {
        in_tx!(self, |tx| {
            tag_lc(&tx, tag)?;
            let (_, lc) = clean_name(alias)?;
            Ok(tx.execute(
                "DELETE FROM label_aliases WHERE label_id = ?1 AND alias_lc = ?2",
                params![tag, lc],
            )? > 0)
        })
    }

    pub fn tag_aliases(&self, tag: i64) -> Result<Vec<String>> {
        tag_lc(&self.conn, tag)?;
        tag_aliases_of(&self.conn, tag)
    }
}

// ---- commands -----------------------------------------------------------------

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

/// Absent = keep, null or empty text = clear, text = set.
fn text_patch(a: &Value, k: &str) -> Result<Patch<String>> {
    match a.get(k) {
        None => Ok(Patch::Keep),
        Some(Value::Null) => Ok(Patch::Clear),
        Some(Value::String(s)) if s.trim().is_empty() => Ok(Patch::Clear),
        Some(Value::String(s)) => Ok(Patch::Set(s.clone())),
        Some(_) => Err(invalid(format!("`{k}` must be text or null"))),
    }
}

pub fn register<C: HasLibrary>(reg: &mut Registry<C>) {
    reg.register(
        "media.info",
        "Title, release date, studio, performers and external ids of one item: {id}",
        |c, a| {
            let id = int_arg(&a, "id")?;
            Ok(serde_json::to_value(c.db().media_info(id)?).expect("info"))
        },
    );
    reg.register(
        "media.info.set",
        "Set or clear title and release date (null clears, absent keeps): {id, title?, released?}",
        |c, a| {
            let id = int_arg(&a, "id")?;
            c.db()
                .set_info(id, text_patch(&a, "title")?, text_patch(&a, "released")?)?;
            Ok(json!({ "id": id }))
        },
    );
    reg.register("studio.list", "List studios with item counts", |c, _| {
        Ok(json!({ "items": c.db().studios()? }))
    });
    reg.register("studio.create", "Create a studio: {name}", |c, a| {
        Ok(json!({ "id": c.db().studio_create(str_arg(&a, "name")?)? }))
    });
    reg.register("studio.rename", "Rename a studio: {id, name}", |c, a| {
        let id = int_arg(&a, "id")?;
        c.db().studio_rename(id, str_arg(&a, "name")?)?;
        Ok(json!({ "id": id }))
    });
    reg.register("studio.set", "Studio flags: {id, private?}", |c, a| {
        let id = int_arg(&a, "id")?;
        c.db().studio_set(id, opt_bool(&a, "private")?)?;
        Ok(json!({ "id": id }))
    });
    reg.register(
        "studio.delete",
        "Delete a studio (items lose only the studio): {id}",
        |c, a| Ok(json!({ "items": c.db().studio_delete(int_arg(&a, "id")?)? })),
    );
    reg.register(
        "studio.merge",
        "Fold a studio into another: {from, into}",
        |c, a| {
            let (from, into) = (int_arg(&a, "from")?, int_arg(&a, "into")?);
            Ok(json!({ "moved": c.db().studio_merge(from, into)? }))
        },
    );
    reg.register(
        "studio.assign",
        "Set (or clear with null) the studio of items: {media_ids, studio}",
        |c, a| {
            let media = id_list(&a, "media_ids")?;
            let studio = match a.get("studio") {
                None | Some(Value::Null) => None,
                Some(v) => Some(
                    v.as_i64()
                        .ok_or_else(|| invalid("`studio` must be an id or null"))?,
                ),
            };
            Ok(json!({ "changed": c.db().studio_assign(&media, studio)? }))
        },
    );
    reg.register(
        "person.list",
        "List performers, best name first: {query?, limit?}",
        |c, a| {
            let q = a.get("query").and_then(Value::as_str);
            let limit = a.get("limit").and_then(Value::as_u64).unwrap_or(100);
            Ok(json!({ "items": c.db().people(q, limit)? }))
        },
    );
    reg.register(
        "person.create",
        "Create a performer: {name, disambiguation?}",
        |c, a| {
            let dis = a
                .get("disambiguation")
                .and_then(Value::as_str)
                .unwrap_or("");
            Ok(json!({ "id": c.db().person_create(str_arg(&a, "name")?, dis)? }))
        },
    );
    reg.register("person.rename", "Rename a performer: {id, name}", |c, a| {
        let id = int_arg(&a, "id")?;
        c.db().person_rename(id, str_arg(&a, "name")?)?;
        Ok(json!({ "id": id }))
    });
    reg.register(
        "person.set",
        "Performer flags: {id, favorite?, private?, disambiguation?}",
        |c, a| {
            let id = int_arg(&a, "id")?;
            c.db().person_set(
                id,
                opt_bool(&a, "favorite")?,
                opt_bool(&a, "private")?,
                a.get("disambiguation").and_then(Value::as_str),
            )?;
            Ok(json!({ "id": id }))
        },
    );
    reg.register(
        "person.delete",
        "Delete a performer (items are untouched): {id}",
        |c, a| Ok(json!({ "items": c.db().person_delete(int_arg(&a, "id")?)? })),
    );
    reg.register(
        "person.merge",
        "Fold a performer into another, keeping its names as aliases: {from, into}",
        |c, a| {
            let (from, into) = (int_arg(&a, "from")?, int_arg(&a, "into")?);
            Ok(json!({ "moved": c.db().person_merge(from, into)? }))
        },
    );
    reg.register(
        "person.alias.add",
        "Add a performer alias: {id, alias}",
        |c, a| {
            let id = int_arg(&a, "id")?;
            c.db().person_alias_add(id, str_arg(&a, "alias")?)?;
            Ok(json!({ "id": id }))
        },
    );
    reg.register(
        "person.alias.remove",
        "Remove a performer alias: {id, alias}",
        |c, a| {
            let id = int_arg(&a, "id")?;
            Ok(json!({ "removed": c.db().person_alias_remove(id, str_arg(&a, "alias")?)? }))
        },
    );
    reg.register(
        "person.assign",
        "Add or remove performers on items: {ids, media_ids, on?}",
        |c, a| {
            let ids = id_list(&a, "ids")?;
            let media = id_list(&a, "media_ids")?;
            let on = opt_bool(&a, "on")?.unwrap_or(true);
            Ok(json!({ "changed": c.db().person_assign(&ids, &media, on)? }))
        },
    );
    reg.register("tag.alias.add", "Add a tag alias: {id, alias}", |c, a| {
        let id = int_arg(&a, "id")?;
        c.db().tag_alias_add(id, str_arg(&a, "alias")?)?;
        Ok(json!({ "id": id }))
    });
    reg.register(
        "tag.alias.remove",
        "Remove a tag alias: {id, alias}",
        |c, a| {
            let id = int_arg(&a, "id")?;
            Ok(json!({ "removed": c.db().tag_alias_remove(id, str_arg(&a, "alias")?)? }))
        },
    );
    reg.register("tag.aliases", "Aliases of a tag: {id}", |c, a| {
        Ok(json!({ "items": c.db().tag_aliases(int_arg(&a, "id")?)? }))
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Db {
        Db::open_in_memory().unwrap()
    }

    #[test]
    fn performer_names_resolve_through_names_then_aliases() {
        let mut db = db();
        let a = db.person_create("Alice", "").unwrap();
        db.person_alias_add(a, "Al").unwrap();
        let r = |db: &Db, n: &str, d: &str| tx_resolve_person(&db.conn, n, d).unwrap();
        assert_eq!(r(&db, "ALICE", ""), Resolved::Found(a));
        assert_eq!(r(&db, "al", ""), Resolved::Found(a), "alias");
        let Resolved::Created(b) = r(&db, "Bob", "") else {
            panic!("new name creates")
        };
        assert_eq!(r(&db, "Bob", ""), Resolved::Found(b));
        // Two people of one name: the one without disambiguation wins.
        let c = db.person_create("Bob", "singer").unwrap();
        assert_eq!(r(&db, "Bob", ""), Resolved::Found(b));
        assert_eq!(r(&db, "bob", "singer"), Resolved::Found(c));
        // Two with a disambiguation and none plain: ambiguous.
        let d = db.person_create("Dee", "one").unwrap();
        let e = db.person_create("Dee", "two").unwrap();
        assert_ne!(d, e);
        assert_eq!(r(&db, "Dee", ""), Resolved::Ambiguous);
        // An alias shared by two performers is ambiguous too.
        db.person_alias_add(d, "Shared").unwrap();
        db.person_alias_add(e, "Shared").unwrap();
        assert_eq!(r(&db, "shared", ""), Resolved::Ambiguous);
        // A single disambiguated performer answers to the bare name.
        assert!(matches!(r(&db, "dee", "three"), Resolved::Created(_)));
    }

    #[test]
    fn tags_and_studios_resolve_by_name_or_alias() {
        let mut db = db();
        let (t, created) = tx_resolve_tag(&db.conn, "blowjob").unwrap();
        assert!(created);
        db.tag_alias_add(t, "blow job").unwrap();
        assert_eq!(tx_resolve_tag(&db.conn, "Blow Job").unwrap(), (t, false));
        assert_eq!(tx_resolve_tag(&db.conn, "BLOWJOB").unwrap(), (t, false));
        assert!(tx_resolve_tag(&db.conn, "   ").is_err());
        let (s, created) = tx_resolve_studio(&db.conn, "Acme").unwrap();
        assert!(created);
        assert_eq!(tx_resolve_studio(&db.conn, "acme").unwrap(), (s, false));
    }

    #[test]
    fn schemes_and_values_are_sanitised() {
        assert_eq!(clean_scheme("Stash Box!").as_deref(), Some("stash-box-"));
        assert_eq!(clean_scheme("---"), None);
        assert_eq!(clean_scheme(&"a".repeat(50)).unwrap().len(), 32);
        assert!(clean_value("").is_none());
        assert!(clean_value(&"v".repeat(129)).is_none());
        assert_eq!(clean_date("2024-02-29").unwrap(), "2024-02-29");
        assert!(clean_date("2023-02-29").is_err());
    }
}
