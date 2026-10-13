//! One registry of named commands. Menus, shortcuts and the CLI all go through
//! it. Arguments and results are JSON so every front end speaks the same
//! language. The registry is generic over its context so the UI can register
//! its own commands (`player.toggle`, `app.boss_key`…) next to the core ones.

use crate::db::{Db, Filter, SortKey};
use crate::error::{Error, Result};
use crate::model::MediaKind;
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
    pub const LIBRARY_STATS: &str = "library.stats";
    pub const MEDIA_QUERY: &str = "media.query";
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
    let kind = match a.get("kind").and_then(Value::as_str) {
        None => None,
        Some(s) => {
            Some(MediaKind::parse(s).ok_or_else(|| Error::Invalid(format!("unknown kind `{s}`")))?)
        }
    };
    let sort = match a.get("sort").and_then(Value::as_str) {
        None => SortKey::default(),
        Some(s) => {
            SortKey::parse(s).ok_or_else(|| Error::Invalid(format!("unknown sort `{s}`")))?
        }
    };
    Ok(Filter {
        sort,
        desc: a.get("desc").and_then(Value::as_bool).unwrap_or(false),
        kind,
        favorites: a.get("fav").and_then(Value::as_bool).unwrap_or(false),
        name: a.get("name").and_then(Value::as_str).map(str::to_owned),
        include_missing: a
            .get("include_missing")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        limit: a.get("limit").and_then(Value::as_u64),
        offset: a.get("offset").and_then(Value::as_u64).unwrap_or(0),
    })
}
