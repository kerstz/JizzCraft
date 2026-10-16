//! Kodi-style `.nfo` files (also written by Jellyfin, Emby and Plex agents):
//! a small XML document per video with a `movie`, `musicvideo` or
//! `episodedetails` root.
//!
//! The parser is deliberately narrow. It reads a handful of fields, refuses
//! anything with a DOCTYPE (so no entity can ever expand), resolves only the
//! five predefined entities and numeric references, and is bounded in size,
//! depth and element counts. A file holding only an address is counted and
//! never fetched. Text after the root element is ignored.

pub use super::nfo_export::{NfoExportOpts, NfoExportReport, check_export_dir, export_nfo};
use super::{
    ImportOpts, InteropReport, MAX_DEPTH, MAX_FILES, PersonRec, Record, TagRec, apply_record,
    clean_text, day_start, read_capped, walk_files,
};
use crate::db::{Db, parent_blob};
use crate::error::Result;
use crate::fingerprint::{parse_fp, parse_oshash};
use crate::meta;
use quick_xml::Reader;
use quick_xml::XmlVersion;
use quick_xml::events::{BytesStart, Event};
use rusqlite::Connection;
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
    /// `<set>` (collection name), either as text or as `<set><name>`.
    pub set: Option<String>,
    pub genres: Vec<String>,
    pub tags: Vec<String>,
    /// Name and `order`.
    pub actors: Vec<(String, Option<i64>)>,
    /// 0 to 10.
    pub userrating: Option<f32>,
    pub playcount: Option<u32>,
    /// `<watched>`: Jellyfin and Plex style flag, without a count.
    pub watched: Option<bool>,
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
    Set,
    SetName,
    Genre,
    Tag,
    UserRating,
    PlayCount,
    Watched,
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

/// Parse one `.nfo`. Pure: no file, no network.
pub fn parse_nfo(bytes: &[u8]) -> std::result::Result<NfoDoc, NfoReject> {
    parse_root(bytes, &[b"movie", b"musicvideo", b"episodedetails"])
}

/// The title of a `tvshow.nfo` (root `tvshow`). Pure, same limits as
/// [`parse_nfo`].
pub fn parse_tvshow(bytes: &[u8]) -> std::result::Result<Option<String>, NfoReject> {
    parse_root(bytes, &[b"tvshow"]).map(|d| d.title)
}

fn parse_root(bytes: &[u8], roots: &[&[u8]]) -> std::result::Result<NfoDoc, NfoReject> {
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
                if !roots.contains(&name.as_slice()) {
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
                    b"set" => Some(Field::Set),
                    b"genre" => Some(Field::Genre),
                    b"tag" => Some(Field::Tag),
                    b"userrating" => Some(Field::UserRating),
                    b"playcount" => Some(Field::PlayCount),
                    b"watched" => Some(Field::Watched),
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
                    (Some(b"set"), b"name") => Some(Field::SetName),
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
        Field::Set | Field::SetName => {
            let t = clean_text(raw, 60);
            if doc.set.is_none() && !t.is_empty() {
                doc.set = Some(t);
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
        Field::Watched => {
            doc.watched = match raw.trim().to_ascii_lowercase().as_str() {
                "true" | "1" => Some(true),
                "false" | "0" => Some(false),
                _ => None,
            };
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
pub(super) struct DirRow {
    id: i64,
    stem_lc: String,
}

pub(super) enum Found {
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
    match sidecar_owner(c, path, dirs)? {
        Found::None => {}
        other => return Ok(other),
    }
    if path
        .file_stem()
        .is_some_and(|s| s.to_string_lossy().to_lowercase() == "movie")
    {
        let dir = parent_blob(path);
        if !dir.is_empty()
            && let Some(rows) = dirs.get(&dir)
        {
            return Ok(pick(rows.iter().map(|r| r.id).collect()));
        }
    }
    Ok(Found::None)
}

/// The video of the same folder with the same file name (extension aside).
pub(super) fn sidecar_owner(
    c: &Connection,
    path: &Path,
    dirs: &mut HashMap<Vec<u8>, Vec<DirRow>>,
) -> Result<Found> {
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
    Ok(pick(
        rows.iter()
            .filter(|r| r.stem_lc == stem)
            .map(|r| r.id)
            .collect(),
    ))
}

/// Title of the nearest `tvshow.nfo` above `path`, walking up to `ceiling`
/// (included) and at most `MAX_DEPTH` levels. Only a regular file (not a
/// link) within the size cap is read. Results are cached per folder.
fn show_studio(
    path: &Path,
    ceiling: &Path,
    cache: &mut HashMap<PathBuf, Option<String>>,
) -> Option<String> {
    let mut dir = path.parent()?;
    let mut visited: Vec<PathBuf> = Vec::new();
    let mut found = None;
    for _ in 0..MAX_DEPTH {
        if !dir.starts_with(ceiling) {
            break;
        }
        if let Some(hit) = cache.get(dir) {
            found = hit.clone();
            break;
        }
        visited.push(dir.to_path_buf());
        let f = dir.join("tvshow.nfo");
        if let Ok(md) = std::fs::symlink_metadata(&f)
            && md.is_file()
            && !md.file_type().is_symlink()
            && md.len() <= MAX_BYTES
            && let Ok(bytes) = read_capped(&f, MAX_BYTES)
            && let Ok(Some(t)) = parse_tvshow(&bytes)
        {
            let t = clean_text(&t, 60);
            if !t.is_empty() {
                found = Some(t);
                break;
            }
        }
        if dir == ceiling {
            break;
        }
        match dir.parent() {
            Some(p) => dir = p,
            None => break,
        }
    }
    for v in visited {
        cache.insert(v, found.clone());
    }
    found
}

/// What an NFO says, in the importer's common shape.
fn record_of(doc: &NfoDoc) -> Record {
    let released = doc
        .premiered
        .as_deref()
        .and_then(|d| meta::clean_date(d).ok())
        .or_else(|| doc.year.map(|y| format!("{y:04}-01-01")))
        .and_then(|d| meta::clean_date(&d).ok());
    Record {
        title: doc.title.clone(),
        released,
        studios: doc.studios.clone(),
        people: doc
            .actors
            .iter()
            .map(|(name, order)| PersonRec {
                name: name.clone(),
                position: *order,
                ..Default::default()
            })
            .collect(),
        tags: doc
            .genres
            .iter()
            .chain(&doc.tags)
            .map(|n| TagRec {
                name: n.clone(),
                aliases: Vec::new(),
            })
            .collect(),
        // Our own id is how the file was found, not something to store.
        external_ids: doc
            .uniqueids
            .iter()
            .filter(|(k, _)| k != "jizzcraft-fp1" && k != "oshash" && !k.is_empty())
            .cloned()
            .collect(),
        stars: doc.userrating.map(super::stars_from_ten).filter(|s| *s > 0),
        // A bare "watched" means one play; the importer only ever raises a count.
        play_count: doc
            .playcount
            .or_else(|| (doc.watched == Some(true)).then_some(1)),
        last_played: doc.lastplayed,
        resume_ms: doc.resume_s.map(|s| (s * 1000.0).round() as i64),
        markers: Vec::new(),
        playlists: doc.set.iter().cloned().collect(),
        ignored: u64::from(doc.ignored),
    }
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
    // Each file with the walk root that produced it (the ceiling for tvshow.nfo).
    let mut files: Vec<(PathBuf, usize)> = Vec::new();
    for (ri, root) in roots.iter().enumerate() {
        let room = MAX_FILES.saturating_sub(files.len());
        if room == 0 {
            break;
        }
        files.extend(
            walk_files(root, "nfo", room, MAX_DEPTH)
                .into_iter()
                .map(|p| (p, ri)),
        );
    }
    let mut rep = InteropReport {
        dry_run: o.dry_run,
        files_seen: files.len() as u64,
        ..Default::default()
    };
    let tx = db.conn.transaction()?;
    let mut dirs: HashMap<Vec<u8>, Vec<DirRow>> = HashMap::new();
    let mut show_cache: HashMap<PathBuf, Option<String>> = HashMap::new();
    let mut cache_root = usize::MAX;
    for (path, ri) in &files {
        if *ri != cache_root {
            show_cache.clear();
            cache_root = *ri;
        }
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
                let mut rec = record_of(&doc);
                if doc.studios.is_empty()
                    && let Some(t) = show_studio(path, &roots[*ri], &mut show_cache)
                {
                    rec.studios = vec![t];
                    rep.studios_from_show += 1;
                }
                if apply_record(&tx, id, &rec, o, &mut rep)? {
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
