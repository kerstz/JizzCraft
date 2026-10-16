//! Writing Kodi-style `.nfo` files, always outside every library folder.
//!
//! Nothing here ever writes inside or above a library root: the destination
//! is checked before anything is created and again after each directory is
//! made (a symlink swapped in between is caught), files are created with
//! `create_new` (never overwritten) and mode 0600 on Unix. The files hold
//! only what the options ask for; copying them beside the media is the
//! user's step.

use crate::db::{Db, blob_to_path};
use crate::error::{Error, Result};
use crate::fingerprint::oshash_hex;
use crate::fsutil;
use crate::organize::LabelKind;
use crate::pathbytes::canonical;
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::path::{Component, Path, PathBuf};

/// What to put in the files.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NfoExportOpts {
    /// Include private performers, studios and tags.
    pub include_private: bool,
    /// Write `userrating` (stars x 2).
    pub ratings: bool,
    /// Write play count, last played and resume point.
    pub history: bool,
}

impl Default for NfoExportOpts {
    fn default() -> Self {
        NfoExportOpts {
            include_private: false,
            ratings: true,
            history: false,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct NfoExportReport {
    pub written: u64,
    /// Videos with nothing to say (after the options and privacy rules).
    pub skipped_empty: u64,
    /// Paths that could not be placed safely, or would collide.
    pub skipped_unsafe: u64,
}

fn refuse(msg: &str) -> Error {
    Error::Invalid(msg.into())
}

fn canon_roots(roots: &[PathBuf]) -> Vec<PathBuf> {
    roots
        .iter()
        .map(|r| canonical(r).unwrap_or_else(|_| r.clone()))
        .collect()
}

/// `dest` equals, is inside, or contains one of the (canonical) roots.
fn overlaps(dest: &Path, roots: &[PathBuf]) -> bool {
    roots
        .iter()
        .any(|r| dest.starts_with(r) || r.starts_with(dest))
}

/// Validate an export destination: an existing empty folder, or a new folder
/// in an existing one. Returns its canonical path. Refuses a destination that
/// is, is inside, or contains a library root (symlinks resolved), one that
/// looks like a media file, and any non-empty folder.
pub fn check_export_dir(dest: &Path, roots: &[PathBuf]) -> Result<PathBuf> {
    if dest.as_os_str().is_empty() {
        return Err(refuse("empty destination"));
    }
    if crate::model::classify(dest).is_some() {
        return Err(refuse("destination looks like a media file"));
    }
    let canon = if dest.exists() {
        if !dest.is_dir() {
            return Err(refuse("destination is not a folder"));
        }
        let mut rd = std::fs::read_dir(dest)?;
        if rd.next().is_some() {
            return Err(refuse("destination folder is not empty"));
        }
        canonical(dest)?
    } else {
        let name = dest
            .file_name()
            .filter(|n| *n != "..")
            .ok_or_else(|| refuse("bad destination name"))?;
        let parent = dest
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        if !parent.is_dir() {
            return Err(refuse("destination folder does not exist"));
        }
        canonical(parent)?.join(name)
    };
    if overlaps(&canon, &canon_roots(roots)) {
        return Err(refuse("destination must be outside every library folder"));
    }
    Ok(canon)
}

fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c if c.is_control() || c == '\u{fffe}' || c == '\u{ffff}' => {}
            c => out.push(c),
        }
    }
    out
}

/// Days since 1970-01-01 to a proleptic Gregorian date.
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn kodi_datetime(epoch: i64) -> String {
    let (y, m, d) = civil_from_days(epoch.div_euclid(86_400));
    let s = epoch.rem_euclid(86_400);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02}",
        s / 3600,
        (s / 60) % 60,
        s % 60
    )
}

/// Directory-safe label for a library root.
fn root_label(root: &Path, taken: &mut HashSet<String>, n: usize) -> String {
    let raw = root
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut s: String = raw
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || matches!(c, ' ' | '-' | '_' | '.') {
                c
            } else {
                '_'
            }
        })
        .collect();
    s = s.trim_matches(|c: char| c == '.' || c == ' ').to_owned();
    let upper = s.to_ascii_uppercase();
    if matches!(upper.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (upper.len() == 4
            && (upper.starts_with("COM") || upper.starts_with("LPT"))
            && upper.as_bytes()[3].is_ascii_digit())
    {
        s.push('_');
    }
    let s: String = s.chars().take(60).collect();
    let base = if s.is_empty() { format!("root-{n}") } else { s };
    let mut label = base.clone();
    let mut k = 2;
    while !taken.insert(label.to_lowercase()) {
        label = format!("{base}-{k}");
        k += 1;
    }
    label
}

struct Item {
    id: i64,
    path: PathBuf,
    root: usize,
    rating: i64,
    plays: i64,
    last: Option<i64>,
    resume: Option<i64>,
    fp: Option<Vec<u8>>,
    oshash: Option<i64>,
}

fn line(body: &mut String, tag: &str, v: &str) {
    body.push_str(&format!("  <{tag}>{}</{tag}>\n", escape(v)));
}

fn nfo_text(db: &Db, it: &Item, o: &NfoExportOpts) -> Result<Option<String>> {
    let info = db.media_info(it.id)?;
    let mut body = String::new();
    let mut fields = 0;
    if let Some(t) = &info.title {
        line(&mut body, "title", t);
        fields += 1;
    }
    if let Some(d) = &info.released {
        line(&mut body, "premiered", d);
        fields += 1;
    }
    if let Some(s) = info
        .studio
        .as_ref()
        .filter(|s| o.include_private || !s.private)
    {
        line(&mut body, "studio", &s.name);
        fields += 1;
    }
    let (tags, _) = db.labels_of(it.id)?;
    for t in tags
        .iter()
        .filter(|t| t.kind == LabelKind::Tag && (o.include_private || !t.private))
    {
        line(&mut body, "tag", &t.name);
        fields += 1;
    }
    for (order, p) in info
        .people
        .iter()
        .filter(|p| o.include_private || !p.private)
        .enumerate()
    {
        body.push_str(&format!(
            "  <actor>\n    <name>{}</name>\n    <order>{order}</order>\n  </actor>\n",
            escape(&p.name)
        ));
        fields += 1;
    }
    if o.ratings && it.rating > 0 {
        line(&mut body, "userrating", &(it.rating * 2).to_string());
        fields += 1;
    }
    if o.history {
        if it.plays > 0 {
            line(&mut body, "playcount", &it.plays.to_string());
            fields += 1;
        }
        if let Some(t) = it.last {
            line(&mut body, "lastplayed", &kodi_datetime(t));
            fields += 1;
        }
        if let Some(ms) = it.resume.filter(|ms| *ms > 0) {
            let secs = format!("{}.{:03}", ms / 1000, ms % 1000);
            body.push_str(&format!(
                "  <resume>\n    <position>{secs}</position>\n  </resume>\n"
            ));
            fields += 1;
        }
    }
    for e in &info.external_ids {
        if matches!(
            e.scheme.as_str(),
            "phash.stash" | "oshash" | "jizzcraft-fp1"
        ) {
            continue;
        }
        body.push_str(&format!(
            "  <uniqueid type=\"{}\">{}</uniqueid>\n",
            escape(&e.scheme),
            escape(&e.value)
        ));
        fields += 1;
    }
    if fields == 0 {
        return Ok(None);
    }
    // Identity, so the importer finds the item again wherever the files go.
    if let Some(fp) = &it.fp {
        body.push_str(&format!(
            "  <uniqueid type=\"jizzcraft-fp1\">{}</uniqueid>\n",
            crate::db::fp_string(fp)
        ));
    }
    if let Some(h) = it.oshash {
        body.push_str(&format!(
            "  <uniqueid type=\"oshash\">{}</uniqueid>\n",
            oshash_hex(h as u64)
        ));
    }
    Ok(Some(format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<movie>\n{body}</movie>\n"
    )))
}

/// A planned file: where it goes (relative to the destination) and its text.
struct Planned {
    dirs: Vec<std::ffi::OsString>,
    file: std::ffi::OsString,
    text: String,
}

/// Write one `.nfo` per video that has something to say, under
/// `dest/<root label>/<relative folder>/<name>.nfo`. `dest` must pass
/// [`check_export_dir`]. Never overwrites, never writes inside a library.
pub fn export_nfo(db: &Db, dest: &Path, o: &NfoExportOpts) -> Result<NfoExportReport> {
    let roots: Vec<PathBuf> = db.folders()?.into_iter().map(|(_, p)| p).collect();
    let croots = canon_roots(&roots);
    let cdest = check_export_dir(dest, &roots)?;
    let mut rep = NfoExportReport::default();

    let mut taken = HashSet::new();
    let labels: Vec<String> = roots
        .iter()
        .enumerate()
        .map(|(i, r)| root_label(r, &mut taken, i + 1))
        .collect();

    // Plan everything first, so a refusal later leaves nothing behind.
    let mut items: Vec<Item> = Vec::new();
    {
        let mut st = db.conn.prepare(
            "SELECT m.id, m.path, f.path, m.rating, m.play_count, m.last_played_at,
                    m.resume_ms, m.fp, m.oshash
             FROM media m JOIN folders f ON f.id = m.folder_id
             WHERE m.kind = 1 AND m.missing = 0 ORDER BY f.id, m.id",
        )?;
        let rows = st.query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                blob_to_path(r.get(1)?),
                blob_to_path(r.get(2)?),
                r.get::<_, i64>(3)?,
                r.get::<_, i64>(4)?,
                r.get::<_, Option<i64>>(5)?,
                r.get::<_, Option<i64>>(6)?,
                r.get::<_, Option<Vec<u8>>>(7)?,
                r.get::<_, Option<i64>>(8)?,
            ))
        })?;
        let index: HashMap<&Path, usize> = roots
            .iter()
            .enumerate()
            .map(|(i, r)| (r.as_path(), i))
            .collect();
        for r in rows {
            let (id, path, root, rating, plays, last, resume, fp, oshash) = r?;
            let Some(&ri) = index.get(root.as_path()) else {
                rep.skipped_unsafe += 1;
                continue;
            };
            items.push(Item {
                id,
                path,
                root: ri,
                rating,
                plays,
                last,
                resume,
                fp,
                oshash,
            });
        }
    }
    let mut plan: Vec<Planned> = Vec::new();
    let mut seen: HashSet<(Vec<std::ffi::OsString>, String)> = HashSet::new();
    for it in &items {
        let Ok(rel) = it.path.strip_prefix(&roots[it.root]) else {
            rep.skipped_unsafe += 1;
            continue;
        };
        let comps: Vec<_> = rel.components().collect();
        let safe = !comps.is_empty() && comps.iter().all(|c| matches!(c, Component::Normal(_)));
        let (Some(stem), true) = (rel.file_stem(), safe) else {
            rep.skipped_unsafe += 1;
            continue;
        };
        let Some(text) = nfo_text(db, it, o)? else {
            rep.skipped_empty += 1;
            continue;
        };
        let mut dirs: Vec<std::ffi::OsString> = vec![labels[it.root].clone().into()];
        dirs.extend(
            comps[..comps.len() - 1]
                .iter()
                .map(|c| c.as_os_str().to_owned()),
        );
        let mut file = stem.to_owned();
        file.push(".nfo");
        // a.mp4 and a.mkv would both want a.nfo: the first one wins.
        let key = (dirs.clone(), file.to_string_lossy().to_lowercase());
        if !seen.insert(key) {
            rep.skipped_unsafe += 1;
            continue;
        }
        plan.push(Planned { dirs, file, text });
    }

    if !cdest.exists() {
        fsutil::create_private_dir_all(&cdest)?;
    }
    // After creating it, resolve again: a symlink raced in must not lead
    // into a library.
    let base = canonical(&cdest)?;
    if overlaps(&base, &croots) {
        return Err(refuse("destination must be outside every library folder"));
    }
    let mut good: HashSet<PathBuf> = HashSet::new();
    good.insert(base.clone());
    for p in plan {
        let mut dir = base.clone();
        let mut ok = true;
        for c in &p.dirs {
            dir.push(c);
            if good.contains(&dir) {
                continue;
            }
            match std::fs::symlink_metadata(&dir) {
                Ok(m) if m.is_dir() => {}
                Ok(_) => {
                    ok = false;
                    break;
                }
                Err(_) => {
                    if std::fs::create_dir(&dir).is_err() {
                        ok = false;
                        break;
                    }
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt;
                        let _ =
                            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
                    }
                }
            }
            // Re-check after every directory made or entered.
            match canonical(&dir) {
                Ok(c) if c.starts_with(&base) && !overlaps(&c, &croots) => {
                    good.insert(dir.clone());
                }
                _ => {
                    ok = false;
                    break;
                }
            }
        }
        if !ok {
            rep.skipped_unsafe += 1;
            continue;
        }
        let target = dir.join(&p.file);
        let created = fsutil::private_file_options()
            .write(true)
            .create_new(true)
            .open(&target);
        match created.and_then(|mut f| std::io::Write::write_all(&mut f, p.text.as_bytes())) {
            Ok(()) => rep.written += 1,
            Err(_) => rep.skipped_unsafe += 1,
        }
    }
    Ok(rep)
}

pub fn register<C: crate::commands::HasLibrary>(reg: &mut crate::commands::Registry<C>) {
    use serde_json::{Value, json};
    reg.register(
        "meta.export_nfo",
        "Write Kodi-style .nfo files outside every library folder: {dest, include_private?, ratings?=true, history?}",
        |c, a| {
            let dest = a
                .get("dest")
                .and_then(Value::as_str)
                .map(PathBuf::from)
                .ok_or_else(|| Error::Invalid("missing string argument `dest`".into()))?;
            let flag = |k: &str, d: bool| a.get(k).and_then(Value::as_bool).unwrap_or(d);
            let o = NfoExportOpts {
                include_private: flag("include_private", false),
                ratings: flag("ratings", true),
                history: flag("history", false),
            };
            Ok(json!(export_nfo(c.db(), &dest, &o)?))
        },
    );
}
