//! Filter <-> JSON, built-in views, saved views, folder tree, random pick.
//! JSON keys are whitelisted; values only ever reach SQL as bound parameters.

use crate::db::{Db, Filter, HiddenMode, Orientation, SortKey, now_secs};
use crate::error::{Error, Result};
use crate::model::MediaKind;
use rusqlite::params;
use serde::Serialize;
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const ALLOWED_KEYS: &[&str] = &[
    "fav",
    "kind",
    "name",
    "sort",
    "desc",
    "include_missing",
    "played",
    "hidden",
    "min_rating",
    "unrated",
    "in_progress",
    "min_duration_ms",
    "max_duration_ms",
    "min_size",
    "max_size",
    "orientation",
    "added_within_days",
    "inbox",
    "damaged",
    "missing_only",
    "has_markers",
    "dir",
    "recursive",
    "seed",
    "limit",
    "offset",
];

fn invalid(m: impl Into<String>) -> Error {
    Error::Invalid(m.into())
}

fn get_bool(o: &Map<String, Value>, k: &str) -> Result<bool> {
    match o.get(k) {
        None | Some(Value::Null) => Ok(false),
        Some(v) => v
            .as_bool()
            .ok_or_else(|| invalid(format!("`{k}` must be a boolean"))),
    }
}

fn get_u64(o: &Map<String, Value>, k: &str) -> Result<Option<u64>> {
    match o.get(k) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v
            .as_u64()
            .map(Some)
            .ok_or_else(|| invalid(format!("`{k}` must be a non-negative integer"))),
    }
}

fn get_str<'a>(o: &'a Map<String, Value>, k: &str) -> Result<Option<&'a str>> {
    match o.get(k) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v
            .as_str()
            .map(Some)
            .ok_or_else(|| invalid(format!("`{k}` must be a string"))),
    }
}

impl Filter {
    /// Parse a filter from JSON. Unknown keys are an error.
    pub fn from_json(v: &Value) -> Result<Filter> {
        let empty = Map::new();
        let o = match v {
            Value::Null => &empty,
            Value::Object(o) => o,
            _ => return Err(invalid("filter must be a JSON object")),
        };
        if let Some(k) = o.keys().find(|k| !ALLOWED_KEYS.contains(&k.as_str())) {
            return Err(invalid(format!("unknown filter key `{k}`")));
        }
        let kind = match get_str(o, "kind")? {
            None => None,
            Some(s) => {
                Some(MediaKind::parse(s).ok_or_else(|| invalid(format!("unknown kind `{s}`")))?)
            }
        };
        let sort = match get_str(o, "sort")? {
            None => SortKey::default(),
            Some(s) => SortKey::parse(s).ok_or_else(|| invalid(format!("unknown sort `{s}`")))?,
        };
        let hidden = match get_str(o, "hidden")? {
            None => HiddenMode::default(),
            Some(s) => {
                HiddenMode::parse(s).ok_or_else(|| invalid(format!("unknown hidden mode `{s}`")))?
            }
        };
        let orientation = match get_str(o, "orientation")? {
            None => None,
            Some(s) => Some(
                Orientation::parse(s)
                    .ok_or_else(|| invalid(format!("unknown orientation `{s}`")))?,
            ),
        };
        let min_rating = match get_u64(o, "min_rating")? {
            Some(r) if r > 5 => return Err(invalid("`min_rating` must be 0..=5")),
            r => r.map(|r| r as u8),
        };
        Ok(Filter {
            kind,
            favorites: get_bool(o, "fav")?,
            name: get_str(o, "name")?.map(str::to_owned),
            include_missing: get_bool(o, "include_missing")?,
            played: match o.get("played") {
                None | Some(Value::Null) => None,
                Some(v) => Some(
                    v.as_bool()
                        .ok_or_else(|| invalid("`played` must be a boolean"))?,
                ),
            },
            hidden,
            min_rating,
            unrated: get_bool(o, "unrated")?,
            in_progress: get_bool(o, "in_progress")?,
            min_duration_ms: get_u64(o, "min_duration_ms")?,
            max_duration_ms: get_u64(o, "max_duration_ms")?,
            min_size: get_u64(o, "min_size")?,
            max_size: get_u64(o, "max_size")?,
            orientation,
            added_within_days: get_u64(o, "added_within_days")?.map(|d| d.min(36_500) as u32),
            inbox: get_bool(o, "inbox")?,
            damaged: get_bool(o, "damaged")?,
            missing_only: get_bool(o, "missing_only")?,
            has_markers: get_bool(o, "has_markers")?,
            dir: get_str(o, "dir")?.map(PathBuf::from),
            recursive: get_bool(o, "recursive")?,
            seed: get_u64(o, "seed")?.unwrap_or(0) as u32,
            sort,
            desc: get_bool(o, "desc")?,
            limit: get_u64(o, "limit")?,
            offset: get_u64(o, "offset")?.unwrap_or(0),
        })
    }

    /// Serialise only the non-default parts. Paging and the shuffle seed are
    /// left out: a saved view describes *what*, not *where you were*.
    pub fn to_json(&self) -> Value {
        let mut o = Map::new();
        let d = Filter::default();
        if let Some(k) = self.kind {
            o.insert("kind".into(), json!(k.label().to_lowercase()));
        }
        let flags = [
            ("fav", self.favorites),
            ("include_missing", self.include_missing),
            ("unrated", self.unrated),
            ("in_progress", self.in_progress),
            ("inbox", self.inbox),
            ("damaged", self.damaged),
            ("missing_only", self.missing_only),
            ("has_markers", self.has_markers),
            ("recursive", self.recursive),
            ("desc", self.desc),
        ];
        for (k, on) in flags {
            if on {
                o.insert(k.into(), json!(true));
            }
        }
        if let Some(n) = self.name.as_deref().filter(|n| !n.trim().is_empty()) {
            o.insert("name".into(), json!(n));
        }
        if let Some(p) = self.played {
            o.insert("played".into(), json!(p));
        }
        if self.hidden != d.hidden {
            o.insert("hidden".into(), json!(self.hidden.as_str()));
        }
        if self.sort != d.sort {
            o.insert("sort".into(), json!(self.sort.as_str()));
        }
        if let Some(o2) = self.orientation {
            o.insert("orientation".into(), json!(o2.as_str()));
        }
        for (k, v) in [
            ("min_rating", self.min_rating.map(u64::from)),
            ("min_duration_ms", self.min_duration_ms),
            ("max_duration_ms", self.max_duration_ms),
            ("min_size", self.min_size),
            ("max_size", self.max_size),
            ("added_within_days", self.added_within_days.map(u64::from)),
        ] {
            if let Some(v) = v {
                o.insert(k.into(), json!(v));
            }
        }
        if let Some(dir) = &self.dir {
            o.insert("dir".into(), json!(dir.to_string_lossy()));
        }
        Value::Object(o)
    }
}

// ---- built-in and saved views --------------------------------------------

pub struct Builtin {
    pub key: &'static str,
    pub label: &'static str,
    /// Derived from play history: hidden in discreet mode.
    pub sensitive: bool,
    pub spec: fn() -> Filter,
}

pub const BUILTINS: &[Builtin] = &[
    Builtin {
        key: "continue",
        label: "Continue",
        sensitive: true,
        spec: || Filter {
            in_progress: true,
            sort: SortKey::LastPlayed,
            desc: true,
            ..Default::default()
        },
    },
    Builtin {
        key: "recent",
        label: "Recently played",
        sensitive: true,
        spec: || Filter {
            played: Some(true),
            sort: SortKey::LastPlayed,
            desc: true,
            ..Default::default()
        },
    },
    Builtin {
        key: "top-rated",
        label: "Top rated",
        sensitive: false,
        spec: || Filter {
            min_rating: Some(4),
            sort: SortKey::Rating,
            desc: true,
            ..Default::default()
        },
    },
    Builtin {
        key: "unrated",
        label: "Unrated",
        sensitive: false,
        spec: || Filter {
            unrated: true,
            ..Default::default()
        },
    },
    Builtin {
        key: "inbox",
        label: "Inbox",
        sensitive: false,
        spec: || Filter {
            inbox: true,
            sort: SortKey::Added,
            desc: true,
            ..Default::default()
        },
    },
    Builtin {
        key: "hidden",
        label: "Hidden",
        sensitive: false,
        spec: || Filter {
            hidden: HiddenMode::Only,
            ..Default::default()
        },
    },
    Builtin {
        key: "damaged",
        label: "Damaged",
        sensitive: false,
        spec: || Filter {
            damaged: true,
            ..Default::default()
        },
    },
    Builtin {
        key: "missing",
        label: "Missing",
        sensitive: false,
        spec: || Filter {
            missing_only: true,
            ..Default::default()
        },
    },
    Builtin {
        key: "markers",
        label: "With markers",
        sensitive: false,
        spec: || Filter {
            has_markers: true,
            ..Default::default()
        },
    },
];

pub fn builtin(key: &str) -> Option<&'static Builtin> {
    BUILTINS.iter().find(|b| b.key == key)
}

#[derive(Debug, Clone, Serialize)]
pub struct SavedView {
    pub id: i64,
    pub name: String,
    pub spec: Value,
}

impl Db {
    pub fn views(&self) -> Result<Vec<SavedView>> {
        let mut st = self
            .conn
            .prepare("SELECT id, name, spec FROM saved_views ORDER BY position, name")?;
        let rows = st.query_map([], |r| {
            let spec: String = r.get(2)?;
            Ok(SavedView {
                id: r.get(0)?,
                name: r.get(1)?,
                spec: serde_json::from_str(&spec).unwrap_or(Value::Null),
            })
        })?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    pub fn view(&self, name: &str) -> Result<Option<SavedView>> {
        Ok(self.views()?.into_iter().find(|v| v.name == name))
    }

    /// Upsert by name (1..=60 characters, no control characters).
    pub fn save_view(&self, name: &str, filter: &Filter) -> Result<i64> {
        let name = name.trim();
        let n = name.chars().count();
        if n == 0 || n > 60 || name.chars().any(char::is_control) {
            return Err(invalid("view name must be 1-60 printable characters"));
        }
        let mut f = filter.clone();
        f.limit = None;
        f.offset = 0;
        f.seed = 0;
        let spec = f.to_json().to_string();
        self.conn.execute(
            "INSERT INTO saved_views(name, spec, created_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(name) DO UPDATE SET spec = excluded.spec",
            params![name, spec, now_secs()],
        )?;
        Ok(self
            .conn
            .query_row("SELECT id FROM saved_views WHERE name = ?1", [name], |r| {
                r.get(0)
            })?)
    }

    pub fn delete_view(&self, name: &str) -> Result<bool> {
        Ok(self
            .conn
            .execute("DELETE FROM saved_views WHERE name = ?1", [name])?
            > 0)
    }

    /// Item counts per directory, hidden and missing items excluded.
    pub fn dir_counts(&self) -> Result<Vec<(PathBuf, u64)>> {
        let mut st = self.conn.prepare(
            "SELECT dir, COUNT(*) FROM media
             WHERE missing = 0 AND hidden = 0 AND dir IS NOT NULL GROUP BY dir",
        )?;
        let rows = st.query_map([], |r| {
            Ok((
                crate::db::blob_to_path(r.get::<_, Vec<u8>>(0)?),
                r.get::<_, i64>(1)? as u64,
            ))
        })?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    /// Pick a random item matching `filter`. `entropy` decides which one.
    /// Indecision, outsourced.
    pub fn random_index(&self, filter: &Filter, entropy: u64) -> Result<Option<(u64, i64)>> {
        let total = self.count(filter)?;
        if total == 0 {
            return Ok(None);
        }
        let index = entropy % total;
        let page = self.query(&Filter {
            limit: Some(1),
            offset: index,
            ..filter.clone()
        })?;
        Ok(page.first().map(|m| (index, m.id)))
    }
}

// ---- folder tree -----------------------------------------------------------

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct DirNode {
    pub path: PathBuf,
    pub name: String,
    /// Items directly in this directory.
    pub own: u64,
    /// Items in this directory and below.
    pub total: u64,
    pub children: Vec<DirNode>,
}

/// Build one tree per imported root from per-directory counts. Intermediate
/// directories without files of their own are synthesised.
pub fn build_tree(roots: &[PathBuf], counts: &[(PathBuf, u64)]) -> Vec<DirNode> {
    roots
        .iter()
        .map(|root| {
            let mut own: BTreeMap<PathBuf, u64> = BTreeMap::new();
            let mut total: BTreeMap<PathBuf, u64> = BTreeMap::new();
            own.insert(root.clone(), 0);
            total.insert(root.clone(), 0);
            for (dir, n) in counts.iter().filter(|(d, _)| d.starts_with(root)) {
                *own.entry(dir.clone()).or_default() += n;
                let mut cur: Option<&Path> = Some(dir);
                while let Some(p) = cur {
                    if !p.starts_with(root) {
                        break;
                    }
                    *total.entry(p.to_path_buf()).or_default() += n;
                    own.entry(p.to_path_buf()).or_default();
                    cur = p.parent();
                }
            }
            fn node(
                path: &Path,
                own: &BTreeMap<PathBuf, u64>,
                total: &BTreeMap<PathBuf, u64>,
                root: &Path,
            ) -> DirNode {
                let mut children: Vec<DirNode> = own
                    .keys()
                    .filter(|k| k.parent() == Some(path) && k.as_path() != path)
                    .map(|k| node(k, own, total, root))
                    .collect();
                children.sort_by_key(|c| c.name.to_lowercase());
                DirNode {
                    path: path.to_path_buf(),
                    name: if path == root {
                        root.file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_else(|| root.to_string_lossy().into_owned())
                    } else {
                        path.file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_default()
                    },
                    own: own.get(path).copied().unwrap_or(0),
                    total: total.get(path).copied().unwrap_or(0),
                    children,
                }
            }
            node(root, &own, &total, root)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tree_synthesises_intermediate_dirs() {
        let roots = vec![PathBuf::from("/lib")];
        let counts = vec![
            (PathBuf::from("/lib"), 1),
            (PathBuf::from("/lib/a/b/c"), 2),
            (PathBuf::from("/lib/Z"), 3),
            (PathBuf::from("/other"), 9),
        ];
        let t = build_tree(&roots, &counts);
        assert_eq!(t.len(), 1);
        let root = &t[0];
        assert_eq!((root.name.as_str(), root.own, root.total), ("lib", 1, 6));
        let names: Vec<_> = root.children.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["a", "Z"], "sorted case-insensitively");
        let a = &root.children[0];
        assert_eq!((a.own, a.total), (0, 2));
        assert_eq!(a.children[0].children[0].name, "c");
        assert_eq!(a.children[0].children[0].own, 2);
    }

    #[test]
    fn json_roundtrip_and_rejection() {
        let f = Filter::from_json(&json!({
            "kind": "video", "min_rating": 4, "sort": "rating", "desc": true,
            "hidden": "include", "dir": "/x/y", "recursive": true, "played": false
        }))
        .unwrap();
        let j = f.to_json();
        let g = Filter::from_json(&j).unwrap();
        assert_eq!(g.to_json(), j);
        assert!(Filter::from_json(&json!({ "evil": 1 })).is_err());
        assert!(Filter::from_json(&json!({ "sort": "'; DROP TABLE media" })).is_err());
        assert!(Filter::from_json(&json!({ "min_rating": 6 })).is_err());
        assert!(Filter::from_json(&json!({ "limit": -1 })).is_err());
    }
}
