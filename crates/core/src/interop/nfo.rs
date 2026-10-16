//! Kodi-style `.nfo` files (also written by Jellyfin, Emby and Plex agents):
//! a small XML document per video with a `movie`, `musicvideo` or
//! `episodedetails` root.
//!
//! The parser is deliberately narrow. It reads a handful of fields, refuses
//! anything with a DOCTYPE (so no entity can ever expand), resolves only the
//! five predefined entities and numeric references, and is bounded in size,
//! depth and element counts. A file holding only an address is counted and
//! never fetched. Text after the root element is ignored.

use super::{ImportOpts, InteropReport, MAX_DEPTH, MAX_FILES, clean_text, read_capped, walk_files};
use crate::db::{Db, parent_blob};
use crate::error::Result;
use crate::fingerprint::{parse_fp, parse_oshash};
use crate::meta::{self, Resolved};
use quick_xml::Reader;
use quick_xml::XmlVersion;
use quick_xml::events::{BytesStart, Event};
use rusqlite::{Connection, params};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

/// Largest file read.
pub const MAX_BYTES: u64 = 1 << 20;
const MAX_ELEMENT_DEPTH: usize = 16;
const MAX_EVENTS: usize = 20_000;
const MAX_LABELS_PER_FILE: usize = 256;
const MAX_ACTORS: usize = 128;
const MAX_UNIQUEIDS: usize = 16;
const MAX_STUDIOS: usize = 8;
/// Longest text kept for any field.
const MAX_FIELD_BYTES: usize = 4096;
/// Performers applied to one item (the model's own cap is 64).
const MAX_ACTORS_APPLIED: usize = 64;

/// Why a file was not used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NfoReject {
    TooBig,
    /// Any DOCTYPE: entity tricks and external entities are not read.
    DocType,
    Malformed,
    TooDeep,
    TooMany,
    /// Valid XML (or text) that is not a movie, music video or episode.
    NotNfo,
    /// Only an address: counted, never fetched.
    UrlOnly,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct NfoDoc {
    pub title: Option<String>,
    /// As written (`YYYY-MM-DD` when valid).
    pub premiered: Option<String>,
    pub year: Option<i32>,
    pub studios: Vec<String>,
    pub genres: Vec<String>,
    pub tags: Vec<String>,
    /// Name and `order`.
    pub actors: Vec<(String, Option<i64>)>,
    /// 0 to 10.
    pub userrating: Option<f32>,
    pub playcount: Option<u32>,
    /// Seconds since the epoch, midnight UTC of the written day.
    pub lastplayed: Option<i64>,
    pub resume_s: Option<f64>,
    /// `(type, value)`, type lowercased.
    pub uniqueids: Vec<(String, String)>,
    /// Elements read but not used.
    pub ignored: u32,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Field {
    Title,
    Premiered,
    Year,
    Studio,
    Genre,
    Tag,
    UserRating,
    PlayCount,
    LastPlayed,
    UniqueId,
    ActorName,
    ActorOrder,
    ResumePos,
}

fn attr(e: &BytesStart<'_>, name: &str) -> Option<String> {
    e.attributes()
        .flatten()
        .find(|a| a.key.as_ref() == name.as_bytes())
        .and_then(|a| {
            a.normalized_value(XmlVersion::Implicit1_0)
                .ok()
                .map(|v| v.into_owned())
        })
}

/// Days since 1970-01-01 of a proleptic Gregorian date.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Midnight UTC of the `YYYY-MM-DD` at the start of `s`.
fn day_start(s: &str) -> Option<i64> {
    let day = s.trim().get(..10)?;
    let day = meta::clean_date(day).ok()?;
    let (y, m, d) = (
        day[..4].parse::<i64>().ok()?,
        day[5..7].parse::<i64>().ok()?,
        day[8..].parse::<i64>().ok()?,
    );
    Some(days_from_civil(y, m, d) * 86_400)
}

/// Parse one `.nfo`. Pure: no file, no network.
pub fn parse_nfo(bytes: &[u8]) -> std::result::Result<NfoDoc, NfoReject> {
    use NfoReject::*;
    if bytes.len() as u64 > MAX_BYTES {
        return Err(TooBig);
    }
    let text = std::str::from_utf8(bytes).map_err(|_| Malformed)?;
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    if !text.contains('<') {
        let t = text.trim();
        let one_line = !t.contains(['\n', '\r']);
        return Err(
            if one_line && (t.starts_with("http://") || t.starts_with("https://")) {
                UrlOnly
            } else {
                NotNfo
            },
        );
    }

    let mut r = Reader::from_str(text);
    let mut doc = NfoDoc::default();
    let mut stack: Vec<Vec<u8>> = Vec::new();
    let mut events = 0usize;
    let (mut labels, mut actors, mut uids) = (0usize, 0usize, 0usize);
    let mut field: Option<(Field, usize)> = None;
    let mut buf = String::new();
    let mut uid_type = String::new();
    let mut actor: Option<(String, Option<i64>)> = None;
    let mut saw_root = false;

    loop {
        events += 1;
        if events > MAX_EVENTS {
            return Err(TooMany);
        }
        let ev = r.read_event().map_err(|_| Malformed)?;
        // Empty elements are a start followed by an end.
        let (start, end) = match &ev {
            Event::Start(_) => (true, false),
            Event::Empty(_) => (true, true),
            Event::End(_) => (false, true),
            _ => (false, false),
        };
        if start {
            let e = match &ev {
                Event::Start(e) | Event::Empty(e) => e,
                _ => unreachable!(),
            };
            let name = e.name().as_ref().to_vec();
            if stack.len() + 1 > MAX_ELEMENT_DEPTH {
                return Err(TooDeep);
            }
            let depth = stack.len() + 1;
            if depth == 1 {
                if !matches!(
                    name.as_slice(),
                    b"movie" | b"musicvideo" | b"episodedetails"
                ) {
                    return Err(NotNfo);
                }
                saw_root = true;
            }
            let parent = stack.last().cloned();
            stack.push(name.clone());
            if depth == 2 {
                let f = match name.as_slice() {
                    b"title" => Some(Field::Title),
                    b"premiered" => Some(Field::Premiered),
                    b"year" => Some(Field::Year),
                    b"studio" => Some(Field::Studio),
                    b"genre" => Some(Field::Genre),
                    b"tag" => Some(Field::Tag),
                    b"userrating" => Some(Field::UserRating),
                    b"playcount" => Some(Field::PlayCount),
                    b"lastplayed" => Some(Field::LastPlayed),
                    b"uniqueid" => Some(Field::UniqueId),
                    b"actor" => {
                        actors += 1;
                        if actors > MAX_ACTORS {
                            return Err(TooMany);
                        }
                        actor = Some((String::new(), None));
                        None
                    }
                    b"resume" => None,
                    _ => {
                        doc.ignored = doc.ignored.saturating_add(1);
                        None
                    }
                };
                if matches!(f, Some(Field::Genre | Field::Tag)) {
                    labels += 1;
                    if labels > MAX_LABELS_PER_FILE {
                        return Err(TooMany);
                    }
                }
                if f == Some(Field::UniqueId) {
                    uids += 1;
                    if uids > MAX_UNIQUEIDS {
                        return Err(TooMany);
                    }
                    uid_type = attr(e, "type").unwrap_or_default().to_lowercase();
                }
                if let Some(f) = f {
                    field = Some((f, depth));
                    buf.clear();
                }
            } else if depth == 3 {
                let f = match (parent.as_deref(), name.as_slice()) {
                    (Some(b"actor"), b"name") => Some(Field::ActorName),
                    (Some(b"actor"), b"order") => Some(Field::ActorOrder),
                    (Some(b"resume"), b"position") => Some(Field::ResumePos),
                    _ => None,
                };
                if let Some(f) = f {
                    field = Some((f, depth));
                    buf.clear();
                }
            }
        }
        match ev {
            Event::DocType(_) => return Err(DocType),
            Event::Decl(d) => {
                if let Some(enc) = d.encoding() {
                    let enc = enc.map_err(|_| Malformed)?;
                    if !enc.eq_ignore_ascii_case(b"utf-8") {
                        return Err(Malformed);
                    }
                }
            }
            Event::Text(t) => {
                if field.is_some_and(|(_, d)| d == stack.len()) {
                    let v = t.decode().map_err(|_| Malformed)?;
                    push_capped(&mut buf, &v);
                }
            }
            Event::CData(t) => {
                if field.is_some_and(|(_, d)| d == stack.len()) {
                    let v = t.decode().map_err(|_| Malformed)?;
                    push_capped(&mut buf, &v);
                }
            }
            Event::GeneralRef(g) => {
                let c = match g.resolve_char_ref() {
                    Ok(Some(c)) => Some(c),
                    Ok(None) => match g.decode().map_err(|_| Malformed)?.as_ref() {
                        "amp" => Some('&'),
                        "lt" => Some('<'),
                        "gt" => Some('>'),
                        "quot" => Some('"'),
                        "apos" => Some('\''),
                        _ => return Err(Malformed),
                    },
                    Err(_) => return Err(Malformed),
                };
                if let Some(c) = c
                    && field.is_some_and(|(_, d)| d == stack.len())
                {
                    let mut tmp = [0u8; 4];
                    push_capped(&mut buf, c.encode_utf8(&mut tmp));
                }
            }
            Event::Eof => return Err(Malformed),
            _ => {}
        }
        if end {
            let depth = stack.len();
            if let Some((f, d)) = field
                && d == depth
            {
                finish_field(f, &buf, &uid_type, &mut doc, &mut actor);
                field = None;
                buf.clear();
            }
            if depth == 2
                && stack.last().map(Vec::as_slice) == Some(b"actor")
                && let Some((name, order)) = actor.take()
            {
                let name = clean_text(&name, 60);
                if !name.is_empty() {
                    doc.actors.push((name, order));
                }
            }
            stack.pop();
            if stack.is_empty() {
                // Anything after the root (Kodi appends a scraper URL) is ignored.
                break;
            }
        }
    }
    if !saw_root {
        return Err(NotNfo);
    }
    Ok(doc)
}

fn push_capped(buf: &mut String, s: &str) {
    if buf.len() < MAX_FIELD_BYTES {
        buf.push_str(s);
    }
}

fn finish_field(
    f: Field,
    raw: &str,
    uid_type: &str,
    doc: &mut NfoDoc,
    actor: &mut Option<(String, Option<i64>)>,
) {
    match f {
        Field::Title => {
            let t = clean_text(raw, 200);
            if doc.title.is_none() && !t.is_empty() {
                doc.title = Some(t);
            }
        }
        Field::Premiered => {
            let t = clean_text(raw, 32);
            if doc.premiered.is_none() && !t.is_empty() {
                doc.premiered = Some(t);
            }
        }
        Field::Year => {
            if doc.year.is_none() {
                doc.year = raw
                    .trim()
                    .parse::<i32>()
                    .ok()
                    .filter(|y| (1..=9999).contains(y));
            }
        }
        Field::Studio => {
            let t = clean_text(raw, 60);
            if !t.is_empty() && doc.studios.len() < MAX_STUDIOS {
                doc.studios.push(t);
            }
        }
        Field::Genre => {
            let t = clean_text(raw, 60);
            if !t.is_empty() {
                doc.genres.push(t);
            }
        }
        Field::Tag => {
            let t = clean_text(raw, 60);
            if !t.is_empty() {
                doc.tags.push(t);
            }
        }
        Field::UserRating => {
            doc.userrating = raw
                .trim()
                .parse::<f32>()
                .ok()
                .filter(|v| v.is_finite() && (0.0..=10.0).contains(v));
        }
        Field::PlayCount => {
            doc.playcount = raw
                .trim()
                .parse::<i64>()
                .ok()
                .map(|n| n.clamp(0, u32::MAX as i64) as u32);
        }
        Field::LastPlayed => doc.lastplayed = day_start(raw),
        Field::UniqueId => {
            let v = clean_text(raw, 128);
            if !v.is_empty() {
                doc.uniqueids.push((uid_type.to_owned(), v));
            }
        }
        Field::ActorName => {
            if let Some(a) = actor {
                a.0 = raw.to_owned();
            }
        }
        Field::ActorOrder => {
            if let Some(a) = actor {
                a.1 = raw.trim().parse::<i64>().ok();
            }
        }
        Field::ResumePos => {
            doc.resume_s = raw
                .trim()
                .parse::<f64>()
                .ok()
                .filter(|v| v.is_finite() && *v >= 0.0 && *v < 1.0e7);
        }
    }
}

// ---- matching and applying ---------------------------------------------------

/// A present video in a folder, for sidecar matching.
struct DirRow {
    id: i64,
    stem_lc: String,
}

enum Found {
    One(i64),
    Many,
    None,
}

fn pick(ids: Vec<i64>) -> Found {
    match ids.as_slice() {
        [] => Found::None,
        [one] => Found::One(*one),
        _ => Found::Many,
    }
}

fn ids_where(c: &Connection, sql: &str, arg: &dyn rusqlite::ToSql) -> Result<Vec<i64>> {
    let mut st = c.prepare_cached(sql)?;
    let rows = st.query_map([arg], |r| r.get(0))?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

fn stem_lc(name: &str) -> String {
    Path::new(name)
        .file_stem()
        .map(|s| s.to_string_lossy().to_lowercase())
        .unwrap_or_default()
}

/// First hit wins: a `jizzcraft-fp1` id, an `oshash` id, the same-named video
/// next to the file, then `movie.nfo` beside exactly one video.
fn find_item(
    c: &Connection,
    doc: &NfoDoc,
    path: &Path,
    dirs: &mut HashMap<Vec<u8>, Vec<DirRow>>,
) -> Result<Found> {
    for (kind, value) in &doc.uniqueids {
        match kind.as_str() {
            "jizzcraft-fp1" => {
                let v = if value.starts_with("fp1:") {
                    value.clone()
                } else {
                    format!("fp1:{value}")
                };
                if let Some(fp) = parse_fp(&v) {
                    match pick(ids_where(
                        c,
                        "SELECT id FROM media WHERE fp = ?1 ORDER BY id LIMIT 3",
                        &fp.as_slice(),
                    )?) {
                        Found::None => {}
                        other => return Ok(other),
                    }
                }
            }
            "oshash" => {
                if let Some(h) = parse_oshash(value) {
                    match pick(ids_where(
                        c,
                        "SELECT id FROM media WHERE oshash = ?1 ORDER BY id LIMIT 3",
                        &(h as i64),
                    )?) {
                        Found::None => {}
                        other => return Ok(other),
                    }
                }
            }
            _ => {}
        }
    }
    let dir = parent_blob(path);
    if dir.is_empty() {
        return Ok(Found::None);
    }
    if !dirs.contains_key(&dir) {
        let mut st = c.prepare_cached(
            "SELECT id, name FROM media WHERE dir = ?1 AND missing = 0 AND kind = 1 ORDER BY id",
        )?;
        let rows = st
            .query_map([&dir], |r| {
                Ok(DirRow {
                    id: r.get(0)?,
                    stem_lc: stem_lc(&r.get::<_, String>(1)?),
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        dirs.insert(dir.clone(), rows);
    }
    let rows = &dirs[&dir];
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    match pick(
        rows.iter()
            .filter(|r| r.stem_lc == stem)
            .map(|r| r.id)
            .collect(),
    ) {
        Found::None => {}
        other => return Ok(other),
    }
    if stem == "movie" {
        return Ok(pick(rows.iter().map(|r| r.id).collect()));
    }
    Ok(Found::None)
}

fn apply(
    c: &Connection,
    id: i64,
    doc: &NfoDoc,
    o: &ImportOpts,
    rep: &mut InteropReport,
) -> Result<bool> {
    let mut changed = false;
    // Title.
    if let Some(t) = doc.title.as_deref().and_then(|t| meta::clean_title(t).ok()) {
        let cur: Option<String> =
            c.query_row("SELECT title FROM media WHERE id = ?1", [id], |r| r.get(0))?;
        if cur.as_deref() != Some(t.as_str()) && (cur.is_none() || o.overwrite) {
            c.execute("UPDATE media SET title = ?2 WHERE id = ?1", params![id, t])?;
            rep.titles_set += 1;
            changed = true;
        }
    }
    // Release date: premiered, else the year.
    let date = doc
        .premiered
        .as_deref()
        .and_then(|d| meta::clean_date(d).ok())
        .or_else(|| doc.year.map(|y| format!("{y:04}-01-01")));
    if let Some(d) = date.and_then(|d| meta::clean_date(&d).ok()) {
        let n = if o.overwrite {
            c.execute(
                "UPDATE media SET released = ?2 WHERE id = ?1 AND released IS NOT ?2",
                params![id, d],
            )?
        } else {
            c.execute(
                "UPDATE media SET released = ?2 WHERE id = ?1 AND released IS NULL",
                params![id, d],
            )?
        };
        changed |= n > 0;
    }
    // Studio: the first one; the rest are ignored.
    if let Some(name) = doc.studios.first() {
        let cur: Option<i64> =
            c.query_row("SELECT studio_id FROM media WHERE id = ?1", [id], |r| {
                r.get(0)
            })?;
        if (cur.is_none() || o.overwrite)
            && let Ok((sid, created)) = meta::tx_resolve_studio(c, name)
        {
            rep.studios_created += created as u64;
            if cur != Some(sid) {
                c.execute(
                    "UPDATE media SET studio_id = ?2 WHERE id = ?1",
                    params![id, sid],
                )?;
                changed = true;
            }
        }
    }
    rep.ignored_fields += doc.studios.len().saturating_sub(1) as u64;
    // Performers.
    for (name, order) in doc.actors.iter().take(MAX_ACTORS_APPLIED) {
        let pid = match meta::tx_resolve_person(c, name, "") {
            Ok(Resolved::Found(pid)) => pid,
            Ok(Resolved::Created(pid)) => {
                rep.people_created += 1;
                pid
            }
            Ok(Resolved::Ambiguous) => {
                rep.ignored_fields += 1;
                continue;
            }
            Err(_) => continue,
        };
        if meta::tx_person_link(c, id, pid, *order).unwrap_or(false) {
            rep.links_added += 1;
            changed = true;
        }
    }
    // Genres and tags become tags, through aliases.
    for name in doc.genres.iter().chain(&doc.tags) {
        let Ok((lid, created)) = meta::tx_resolve_tag(c, name) else {
            continue;
        };
        rep.tags_created += created as u64;
        let n = c.execute(
            "INSERT OR IGNORE INTO media_labels(label_id, media_id, added_at)
             SELECT ?1, id, ?3 FROM media WHERE id = ?2",
            params![lid, id, crate::db::now_secs()],
        )?;
        if n > 0 {
            rep.links_added += 1;
            changed = true;
        }
    }
    // Foreign ids, minus the ones that are ours.
    for (kind, value) in &doc.uniqueids {
        if kind != "jizzcraft-fp1"
            && !kind.is_empty()
            && meta::tx_external_id_add(c, id, kind, value)?
        {
            changed = true;
        }
    }
    // Rating: 0 to 10 onto 0 to 5 stars.
    if o.ratings
        && let Some(v) = doc.userrating
    {
        let stars = super::stars_from_ten(v);
        if stars > 0 {
            let n = if o.overwrite {
                c.execute(
                    "UPDATE media SET rating = ?2 WHERE id = ?1 AND rating <> ?2",
                    params![id, stars],
                )?
            } else {
                c.execute(
                    "UPDATE media SET rating = ?2 WHERE id = ?1 AND rating = 0",
                    params![id, stars],
                )?
            };
            if n > 0 {
                rep.ratings_set += 1;
                changed = true;
            }
        }
    }
    // History: the larger count and the later date win; a resume point only
    // fills an empty one.
    if o.history {
        if let Some(pc) = doc.playcount {
            changed |= c.execute(
                "UPDATE media SET play_count = ?2 WHERE id = ?1 AND play_count < ?2",
                params![id, pc],
            )? > 0;
        }
        if let Some(lp) = doc.lastplayed {
            changed |= c.execute(
                "UPDATE media SET last_played_at = ?2
                 WHERE id = ?1 AND COALESCE(last_played_at, 0) < ?2",
                params![id, lp],
            )? > 0;
        }
        if let Some(s) = doc.resume_s {
            changed |= c.execute(
                "UPDATE media SET resume_ms = ?2 WHERE id = ?1 AND resume_ms IS NULL",
                params![id, (s * 1000.0).round() as i64],
            )? > 0;
        }
    }
    rep.ignored_fields += u64::from(doc.ignored);
    Ok(changed)
}

/// Read every `.nfo` below `dir` (or below every library folder when `dir`
/// is `None`) and apply it to the item it belongs to. Nothing is written
/// outside the database; with `dry_run` nothing is written at all.
pub fn import_nfo(
    db: &mut Db,
    dir: Option<&Path>,
    o: &ImportOpts,
    cancel: &AtomicBool,
) -> Result<InteropReport> {
    let roots: Vec<PathBuf> = match dir {
        Some(d) => {
            if !d.is_dir() {
                return Err(crate::error::Error::Invalid("not a directory".into()));
            }
            vec![d.to_path_buf()]
        }
        None => db.folders()?.into_iter().map(|(_, p)| p).collect(),
    };
    let mut files: Vec<PathBuf> = Vec::new();
    for root in &roots {
        let room = MAX_FILES.saturating_sub(files.len());
        if room == 0 {
            break;
        }
        files.extend(walk_files(root, "nfo", room, MAX_DEPTH));
    }
    let mut rep = InteropReport {
        dry_run: o.dry_run,
        files_seen: files.len() as u64,
        ..Default::default()
    };
    let tx = db.conn.transaction()?;
    let mut dirs: HashMap<Vec<u8>, Vec<DirRow>> = HashMap::new();
    for path in &files {
        if cancel.load(Ordering::Relaxed) {
            rep.cancelled = true;
            break;
        }
        let Ok(bytes) = read_capped(path, MAX_BYTES) else {
            rep.rejected += 1;
            continue;
        };
        let doc = match parse_nfo(&bytes) {
            Ok(d) => d,
            Err(_) => {
                rep.rejected += 1;
                continue;
            }
        };
        rep.parsed += 1;
        match find_item(&tx, &doc, path, &mut dirs)? {
            Found::None => rep.unmatched += 1,
            Found::Many => rep.ambiguous += 1,
            Found::One(id) => {
                rep.matched += 1;
                if apply(&tx, id, &doc, o, &mut rep)? {
                    rep.applied += 1;
                }
            }
        }
    }
    if o.dry_run || rep.cancelled {
        tx.rollback()?;
    } else {
        tx.commit()?;
    }
    if rep.cancelled {
        rep = InteropReport {
            files_seen: rep.files_seen,
            dry_run: o.dry_run,
            cancelled: true,
            ..Default::default()
        };
    }
    Ok(rep)
}
