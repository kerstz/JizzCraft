//! One registry of named commands. Menus, shortcuts and the CLI all go through
//! it. Arguments and results are JSON so every front end speaks the same
//! language. The registry is generic over its context so the UI can register
//! its own commands (`player.toggle`, `app.boss_key`…) next to the core ones.
//! Every menu, key and script queues at the same front desk. No VIP entrance.

use crate::db::{Db, Filter};
use crate::error::{Error, Result};
use crate::scan;
use serde_json::{Value, json};
use std::collections::BTreeMap;

pub type Handler<C> = fn(&mut C, Value) -> Result<Value>;

pub struct Command<C> {
    pub name: &'static str,
    pub help: &'static str,
    pub handler: Handler<C>,
}

pub struct Registry<C> {
    cmds: BTreeMap<&'static str, Command<C>>,
}

impl<C> Default for Registry<C> {
    fn default() -> Self {
        Self {
            cmds: BTreeMap::new(),
        }
    }
}

impl<C> Registry<C> {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, name: &'static str, help: &'static str, handler: Handler<C>) {
        self.cmds.insert(
            name,
            Command {
                name,
                help,
                handler,
            },
        );
    }

    pub fn run(&self, ctx: &mut C, name: &str, args: Value) -> Result<Value> {
        let cmd = self
            .cmds
            .get(name)
            .ok_or_else(|| Error::NotFound(format!("command `{name}`")))?;
        (cmd.handler)(ctx, args)
    }

    pub fn has(&self, name: &str) -> bool {
        self.cmds.contains_key(name)
    }

    pub fn list(&self) -> impl Iterator<Item = (&'static str, &'static str)> + '_ {
        self.cmds.values().map(|c| (c.name, c.help))
    }
}

/// Anything that owns a library can host the core commands.
pub trait HasLibrary {
    fn db(&mut self) -> &mut Db;
}

impl HasLibrary for Db {
    fn db(&mut self) -> &mut Db {
        self
    }
}

pub mod names {
    pub const LIBRARY_IMPORT: &str = "library.import";
    pub const LIBRARY_RESCAN: &str = "library.rescan";
    pub const LIBRARY_REMOVE: &str = "library.remove";
    pub const LIBRARY_STATS: &str = "library.stats";
    pub const MEDIA_QUERY: &str = "media.query";
    pub const MEDIA_SEARCH: &str = "media.search";
    pub const MEDIA_GET: &str = "media.get";
    pub const MEDIA_FAVORITE_TOGGLE: &str = "media.favorite.toggle";
}

pub fn register_core<C: HasLibrary>(reg: &mut Registry<C>) {
    use names::*;
    reg.register(
        LIBRARY_IMPORT,
        "Import and scan a folder: {path}",
        |c, a| {
            let path = str_arg(&a, "path")?;
            let (folder_id, report) = scan::import_folder(c.db(), std::path::Path::new(&path))?;
            Ok(json!({ "folder_id": folder_id, "report": report }))
        },
    );
    reg.register(LIBRARY_RESCAN, "Rescan every imported folder", |c, _| {
        Ok(json!({ "report": scan::rescan_all(c.db())? }))
    });
    reg.register(
        LIBRARY_REMOVE,
        "Forget an imported folder and its media rows, never the files: {path}",
        |c, a| {
            let path = str_arg(&a, "path")?;
            let removed = c
                .db()
                .remove_folder(std::path::Path::new(&path))?
                .ok_or_else(|| Error::NotFound("imported folder".into()))?;
            Ok(json!({ "removed_media": removed }))
        },
    );
    reg.register(
        MEDIA_SEARCH,
        "Full-text search over names, tags, categories and marker notes: {query, limit?, offset?}",
        |c, a| {
            let q = str_arg(&a, "query")?;
            let limit = a
                .get("limit")
                .and_then(Value::as_u64)
                .unwrap_or(100)
                .min(10_000);
            let offset = a.get("offset").and_then(Value::as_u64).unwrap_or(0);
            let items = c.db().search(&q, limit, offset)?;
            Ok(json!({ "count": items.len(), "items": items }))
        },
    );
    reg.register(LIBRARY_STATS, "Library counters", |c, _| {
        Ok(serde_json::to_value(c.db().stats()?).expect("stats serialize"))
    });
    reg.register(
        MEDIA_QUERY,
        "Query media: {fav?, kind?, name?, sort?: name|added|size|duration, desc?, include_missing?, limit?, offset?}",
        |c, a| {
            let f = filter_from_args(&a)?;
            let items = c.db().query(&f)?;
            Ok(json!({ "count": items.len(), "items": items }))
        },
    );
    reg.register(MEDIA_GET, "Fetch one media entry: {id}", |c, a| {
        let id = id_arg(&a)?;
        let m = c
            .db()
            .get(id)?
            .ok_or_else(|| Error::NotFound(format!("media id {id}")))?;
        Ok(serde_json::to_value(m).expect("media serialize"))
    });
    reg.register(MEDIA_FAVORITE_TOGGLE, "Toggle favorite: {id}", |c, a| {
        let id = id_arg(&a)?;
        let favorite = c.db().toggle_favorite(id)?;
        Ok(json!({ "id": id, "favorite": favorite }))
    });
    register_userdata(reg);
    register_views(reg);
    crate::organize::register(reg);
    reg.register(
        "library.fingerprint",
        "Fingerprint files for move detection: {all?, limit?}",
        |c, a| {
            let all = a.get("all").and_then(Value::as_bool).unwrap_or(false);
            let limit = a.get("limit").and_then(Value::as_u64);
            let cancel = std::sync::atomic::AtomicBool::new(false);
            let rep = crate::fingerprint::fingerprint_pending(
                c.db(),
                !all,
                limit,
                &cancel,
                &mut |_, _| {},
            )?;
            Ok(serde_json::to_value(rep).expect("report"))
        },
    );
    reg.register("library.backup", "Back up the library: {dest}", |c, a| {
        let dest = str_arg(&a, "dest")?;
        let bytes = crate::portability::backup(c.db(), std::path::Path::new(&dest))?;
        Ok(json!({ "bytes": bytes }))
    });
    reg.register(
        "meta.export",
        "Export favorites, ratings, markers, tags, categories, playlists: {dest, include_history?, include_names?, include_private?}",
        |c, a| {
            let dest = str_arg(&a, "dest")?;
            let o = crate::portability::ExportOpts {
                include_history: a
                    .get("include_history")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                include_names: a
                    .get("include_names")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            };
            let private = a
                .get("include_private")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let rep = crate::portability::export_meta_ext(
                c.db(),
                std::path::Path::new(&dest),
                &o,
                private,
            )?;
            Ok(serde_json::to_value(rep).expect("report"))
        },
    );
    reg.register(
        "meta.import",
        "Import an export onto matching files: {src, dry_run?}",
        |c, a| {
            let src = str_arg(&a, "src")?;
            let dry = a.get("dry_run").and_then(Value::as_bool).unwrap_or(false);
            let rep = crate::portability::import_meta(c.db(), std::path::Path::new(&src), dry)?;
            Ok(serde_json::to_value(rep).expect("report"))
        },
    );
    reg.register(
        "library.checksum",
        "Checksum files and flag silent changes: {scope?: unchecked|all, limit?}",
        |c, a| {
            let scope = match a.get("scope").and_then(Value::as_str) {
                None | Some("unchecked") => crate::integrity::Scope::Unchecked,
                Some("all") => crate::integrity::Scope::All,
                Some(s) => return Err(Error::Invalid(format!("unknown scope `{s}`"))),
            };
            let limit = a.get("limit").and_then(Value::as_u64);
            let cancel = std::sync::atomic::AtomicBool::new(false);
            let rep =
                crate::integrity::checksum_pending(c.db(), scope, limit, &cancel, &mut |_, _| {})?;
            Ok(serde_json::to_value(rep).expect("report"))
        },
    );
    reg.register(
        "media.relink",
        "Merge a vanished entry into its new location: {from, to}",
        |c, a| {
            let from = a
                .get("from")
                .and_then(Value::as_i64)
                .ok_or_else(|| Error::Invalid("missing integer argument `from`".into()))?;
            let to = a
                .get("to")
                .and_then(Value::as_i64)
                .ok_or_else(|| Error::Invalid("missing integer argument `to`".into()))?;
            c.db().merge_into(from, to)?;
            Ok(json!({ "id": to }))
        },
    );
}

fn register_views<C: HasLibrary>(reg: &mut Registry<C>) {
    reg.register("media.count", "Count media matching a filter", |c, a| {
        let f = filter_from_args(&a)?;
        Ok(json!({ "count": c.db().count(&f)? }))
    });
    reg.register(
        "media.random",
        "Pick a random item matching a filter",
        |c, a| {
            let f = filter_from_args(&a)?;
            let entropy = {
                use std::hash::{BuildHasher, Hasher};
                let mut h = std::collections::hash_map::RandomState::new().build_hasher();
                h.write_u128(
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_nanos())
                        .unwrap_or(0),
                );
                h.finish()
            };
            Ok(match c.db().random_index(&f, entropy)? {
                Some((index, id)) => json!({ "index": index, "id": id }),
                None => Value::Null,
            })
        },
    );
    reg.register("views.list", "Built-in and saved views", |c, _| {
        let builtin: Vec<Value> = crate::view::BUILTINS
            .iter()
            .map(|b| json!({ "key": b.key, "label": b.label, "sensitive": b.sensitive }))
            .collect();
        Ok(json!({ "builtin": builtin, "saved": c.db().views()? }))
    });
    reg.register(
        "views.get",
        "Resolve a view to its filter: {name}",
        |c, a| {
            let name = str_arg(&a, "name")?;
            if let Some(b) = crate::view::builtin(&name) {
                return Ok(json!({ "spec": (b.spec)().to_json() }));
            }
            let v = c
                .db()
                .view(&name)?
                .ok_or_else(|| Error::NotFound(format!("view `{name}`")))?;
            Ok(json!({ "spec": v.spec }))
        },
    );
    reg.register("views.save", "Save a view: {name, spec}", |c, a| {
        let name = str_arg(&a, "name")?;
        let f = Filter::from_json(a.get("spec").unwrap_or(&Value::Null))?;
        Ok(json!({ "id": c.db().save_view(&name, &f)? }))
    });
    reg.register("views.delete", "Delete a saved view: {name}", |c, a| {
        let name = str_arg(&a, "name")?;
        Ok(json!({ "deleted": c.db().delete_view(&name)? }))
    });
    reg.register("library.tree", "Folder tree with item counts", |c, _| {
        let roots: Vec<_> = c.db().folders()?.into_iter().map(|(_, p)| p).collect();
        let counts = c.db().dir_counts()?;
        Ok(json!({ "roots": crate::view::build_tree(&roots, &counts) }))
    });
}

fn register_userdata<C: HasLibrary>(reg: &mut Registry<C>) {
    reg.register(
        "media.rate",
        "Set star rating: {id, rating: 0..5}",
        |c, a| {
            let id = id_arg(&a)?;
            let r = a
                .get("rating")
                .and_then(Value::as_u64)
                .filter(|r| *r <= 5)
                .ok_or_else(|| Error::Invalid("`rating` must be 0..=5".into()))?
                as u8;
            c.db().set_rating(id, r)?;
            Ok(json!({ "id": id, "rating": r }))
        },
    );
    reg.register(
        "media.hide",
        "Hide or unhide an item in the library (database only): {id, hidden?}",
        |c, a| {
            let id = id_arg(&a)?;
            let hidden = match a.get("hidden").and_then(Value::as_bool) {
                Some(h) => {
                    c.db().set_hidden(id, h)?;
                    h
                }
                None => c.db().toggle_hidden(id)?,
            };
            Ok(json!({ "id": id, "hidden": hidden }))
        },
    );
    reg.register(
        "media.set",
        "Patch user data: {id, favorite?, rating?, hidden?, inbox?}",
        |c, a| {
            let id = id_arg(&a)?;
            let patch = crate::history::MediaPatch {
                favorite: a.get("favorite").and_then(Value::as_bool),
                rating: a.get("rating").and_then(Value::as_u64).map(|r| r as u8),
                hidden: a.get("hidden").and_then(Value::as_bool),
                inbox: a.get("inbox").and_then(Value::as_bool),
            };
            Ok(serde_json::to_value(c.db().patch(id, &patch)?).expect("media serialize"))
        },
    );
    reg.register(
        "media.inbox.clear",
        "Mark items as triaged: {id?} (all when omitted)",
        |c, a| {
            let id = a.get("id").and_then(Value::as_i64);
            Ok(json!({ "cleared": c.db().clear_inbox(id)? }))
        },
    );
    reg.register(
        "history.progress",
        "Record playback progress: {id, pos_ms, duration_ms?, count_play?}",
        |c, a| {
            let id = id_arg(&a)?;
            let pos = a
                .get("pos_ms")
                .and_then(Value::as_u64)
                .ok_or_else(|| Error::Invalid("missing integer argument `pos_ms`".into()))?;
            let dur = a.get("duration_ms").and_then(Value::as_u64);
            let count = a
                .get("count_play")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let p = c.db().record_progress(id, pos, dur, count)?;
            Ok(serde_json::to_value(p).expect("progress serialize"))
        },
    );
    reg.register("history.list", "Recently played: {limit?}", |c, a| {
        let limit = a.get("limit").and_then(Value::as_u64).unwrap_or(50);
        let items = c.db().history(limit)?;
        Ok(json!({ "count": items.len(), "items": items }))
    });
    reg.register(
        "history.clear",
        "Forget play history and resume points: {id?}",
        |c, a| {
            let id = a.get("id").and_then(Value::as_i64);
            Ok(json!({ "cleared": c.db().clear_history(id)? }))
        },
    );
    reg.register(
        "history.pause",
        "Stop (true) or resume (false) recording history: {paused}",
        |c, a| {
            let paused = a
                .get("paused")
                .and_then(Value::as_bool)
                .ok_or_else(|| Error::Invalid("missing boolean argument `paused`".into()))?;
            c.db()
                .set_setting("history.enabled", if paused { "0" } else { "1" })?;
            Ok(json!({ "paused": paused }))
        },
    );
    reg.register("settings.get", "Read settings: {key?}", |c, a| {
        let db = c.db();
        let mut out = serde_json::Map::new();
        let keys: Vec<&str> = match a.get("key").and_then(Value::as_str) {
            Some(k) => vec![k],
            None => crate::history::SETTINGS_KEYS.to_vec(),
        };
        for k in keys {
            out.insert(k.into(), json!(db.setting(k)?));
        }
        Ok(Value::Object(out))
    });
    reg.register("settings.set", "Write a setting: {key, value}", |c, a| {
        let key = str_arg(&a, "key")?;
        let value = str_arg(&a, "value")?;
        c.db().set_setting(&key, &value)?;
        Ok(json!({}))
    });
    reg.register("markers.list", "Markers of a video: {id}", |c, a| {
        let id = id_arg(&a)?;
        let items = c.db().markers(id)?;
        Ok(json!({ "count": items.len(), "items": items }))
    });
    reg.register(
        "markers.add",
        "Add a marker: {id, pos_ms, label?}",
        |c, a| {
            let id = id_arg(&a)?;
            let pos = a
                .get("pos_ms")
                .and_then(Value::as_u64)
                .ok_or_else(|| Error::Invalid("missing integer argument `pos_ms`".into()))?;
            let label = a.get("label").and_then(Value::as_str).unwrap_or("");
            Ok(serde_json::to_value(c.db().add_marker(id, pos, label)?).expect("marker"))
        },
    );
    reg.register(
        "markers.update",
        "Edit a marker: {marker_id, pos_ms?, label?}",
        |c, a| {
            let mid = a
                .get("marker_id")
                .and_then(Value::as_i64)
                .ok_or_else(|| Error::Invalid("missing integer argument `marker_id`".into()))?;
            let pos = a.get("pos_ms").and_then(Value::as_u64);
            let label = a.get("label").and_then(Value::as_str);
            Ok(serde_json::to_value(c.db().update_marker(mid, pos, label)?).expect("marker"))
        },
    );
    reg.register("markers.remove", "Delete a marker: {marker_id}", |c, a| {
        let mid = a
            .get("marker_id")
            .and_then(Value::as_i64)
            .ok_or_else(|| Error::Invalid("missing integer argument `marker_id`".into()))?;
        Ok(json!({ "removed": c.db().remove_marker(mid)? }))
    });
}

fn str_arg(a: &Value, key: &str) -> Result<String> {
    a.get(key)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| Error::Invalid(format!("missing string argument `{key}`")))
}

fn id_arg(a: &Value) -> Result<i64> {
    a.get("id")
        .and_then(Value::as_i64)
        .ok_or_else(|| Error::Invalid("missing integer argument `id`".into()))
}

fn filter_from_args(a: &Value) -> Result<Filter> {
    Filter::from_json(a)
}
