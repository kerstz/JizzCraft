//! Playlists, categories and tags in the desktop app: multi-select, the
//! sidebar sections, the picker, drag and drop, playlist order and undo.
//!
//! Everything here is user data in the library database. Nothing touches a
//! media file. The app ships no preset names: every playlist, category and tag
//! is created by the user. Toasts carry counts only, and in discreet or
//! disguised mode names are masked as "Playlist 01", "Category 01", "Tag 01"
//! and private entries disappear.
//!
//! The logic that decides things (ranges, undo, masking, ranking, drop
//! targets, move plans) is plain functions over ids and slices, so it is
//! tested without a window.

use super::{JizzApp, JobMsg, Tab, ToastKind, View};
use crate::i18n::{tr, trf};
use crate::theme::{self, APP, BG, FAINT, LINE, LINE_STRONG, MUTED, S, TEXT};
use crate::widgets;
use egui::{Align2, Color32, CornerRadius, FontFamily, FontId, Key, Modifiers, Rect, Ui, vec2};
use jizzcraft_core::organize::{LabelInfo, LabelKind, M3uOpts, MAX_FILTER_IDS, MAX_IDS, Playlist};
use jizzcraft_core::{
    Db, Error, Filter, HiddenMode, MediaPatch, OrgFilter, Registry, SortKey, UserState,
};
use serde_json::{Value, json};
use std::collections::{BTreeSet, HashSet};

/// Operations kept for undo.
pub const UNDO_DEPTH: usize = 30;
/// Largest list snapshotted for undo (equals the core's per-call id limit).
pub const SNAPSHOT_CAP: usize = MAX_IDS;
/// Largest range or select-all fetched in one query.
pub const RANGE_CAP: u64 = MAX_IDS as u64;
/// Entries shown at most in the picker.
const PICKER_ROWS: usize = 12;
/// Tags get a filter box above this many entries.
const TAG_FILTER_AFTER: usize = 30;
/// How far autoplay looks for a playable item before giving up.
const SKIP_LIMIT: u64 = 1000;

/// Eight swatches, muted enough for a dark UI.
pub const PALETTE: [u32; 8] = [
    0xE5484D, 0xF76B15, 0xFFC53D, 0x46A758, 0x12A594, 0x3E63DD, 0x8E4EC6, 0xE93D82,
];

// ---- identities -------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash, PartialOrd, Ord)]
pub enum OrgKind {
    Playlist,
    Category,
    Tag,
}

impl OrgKind {
    pub fn label_kind(self) -> Option<LabelKind> {
        match self {
            OrgKind::Playlist => None,
            OrgKind::Category => Some(LabelKind::Category),
            OrgKind::Tag => Some(LabelKind::Tag),
        }
    }

    fn noun(self) -> &'static str {
        match self {
            OrgKind::Playlist => "Playlist",
            OrgKind::Category => "Category",
            OrgKind::Tag => "Tag",
        }
    }

    fn mask_template(self) -> &'static str {
        match self {
            OrgKind::Playlist => "Playlist {}",
            OrgKind::Category => "Category {}",
            OrgKind::Tag => "Tag {}",
        }
    }

    fn create_template(self) -> &'static str {
        match self {
            OrgKind::Playlist => "Create “{}” as playlist",
            OrgKind::Category => "Create “{}” as category",
            OrgKind::Tag => "Create “{}” as tag",
        }
    }

    fn new_hint(self) -> &'static str {
        match self {
            OrgKind::Playlist => "New playlist",
            OrgKind::Category => "New category",
            OrgKind::Tag => "New tag",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct OrgRef {
    pub kind: OrgKind,
    pub id: i64,
}

/// Drag and drop payload: the media ids being carried.
pub struct DragIds(pub Vec<i64>);

// ---- pure: selection --------------------------------------------------------

/// Inclusive list indexes `lo..=hi` as an `(offset, limit)` query window,
/// capped at [`RANGE_CAP`] items.
pub fn range_window(a: u64, b: u64) -> (u64, u64) {
    let (lo, hi) = (a.min(b), a.max(b));
    (lo, (hi - lo + 1).min(RANGE_CAP))
}

/// Apply a click on tile `(id, idx)` to the marks.
///
/// * plain click: marks cleared, the tile becomes the anchor;
/// * Ctrl: toggles the tile (the previously selected tile is marked first if
///   nothing is);
/// * Shift: marks the range from the anchor, replacing the marks
///   (Ctrl+Shift adds to them).
pub fn click_marks(
    marks: &mut BTreeSet<i64>,
    anchor: &mut Option<u64>,
    prev: Option<(i64, u64)>,
    (id, idx): (i64, u64),
    (ctrl, shift): (bool, bool),
    range: impl FnOnce(u64, u64) -> Vec<i64>,
) {
    if shift {
        let from = anchor.or(prev.map(|p| p.1)).unwrap_or(idx);
        let ids = range(from, idx);
        if !ctrl {
            marks.clear();
        }
        marks.extend(ids);
        marks.insert(id);
        anchor.get_or_insert(from);
    } else if ctrl {
        if marks.is_empty()
            && let Some((pid, _)) = prev
            && pid != id
        {
            marks.insert(pid);
        }
        if !marks.remove(&id) {
            marks.insert(id);
        }
        *anchor = Some(idx);
    } else {
        marks.clear();
        *anchor = Some(idx);
    }
}

/// Media ids whose tile rectangle meets the rubber band.
pub fn band_hits(band: Rect, tiles: &[(i64, Rect)]) -> Vec<i64> {
    tiles
        .iter()
        .filter(|(_, r)| band.intersects(*r))
        .map(|(id, _)| *id)
        .collect()
}

/// Marks after a rubber band: `base` (kept by Ctrl, empty otherwise) plus the
/// tiles under the band.
pub fn band_marks(base: &BTreeSet<i64>, hits: &[i64]) -> BTreeSet<i64> {
    let mut m = base.clone();
    m.extend(hits.iter().copied());
    m
}

// ---- pure: privacy ----------------------------------------------------------

/// "Playlist 01": the English masked form. [`shown_name`] translates it.
#[cfg(test)]
pub fn mask_label_en(kind: OrgKind, ordinal: usize) -> String {
    format!("{} {:02}", kind.noun(), ordinal + 1)
}

fn masked_name(kind: OrgKind, ordinal: usize) -> String {
    trf(kind.mask_template(), &[&format!("{:02}", ordinal + 1)])
}

/// The name to put on screen.
pub fn shown_name(kind: OrgKind, ordinal: usize, name: &str, discreet: bool) -> String {
    if discreet {
        masked_name(kind, ordinal)
    } else {
        name.to_owned()
    }
}

/// For a list in tree order (`depth`, `private`), which entries may be shown.
/// In discreet mode a private entry hides itself and everything below it.
pub fn visible_flags(entries: &[(u32, bool)], discreet: bool) -> Vec<bool> {
    let mut out = Vec::with_capacity(entries.len());
    let mut hidden_at: Option<u32> = None;
    for &(depth, private) in entries {
        if let Some(d) = hidden_at
            && depth <= d
        {
            hidden_at = None;
        }
        let hide = discreet && (hidden_at.is_some() || private);
        if hide && hidden_at.is_none() {
            hidden_at = Some(depth);
        }
        out.push(!hide);
    }
    out
}

// ---- pure: picker ranking ---------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cand {
    pub kind: OrgKind,
    pub id: i64,
    pub name: String,
}

/// Indexes of `cands` matching `query`, best first: exact, prefix, word
/// prefix, substring, then in-order letters. Ties keep playlists before
/// categories before tags, then alphabetical.
pub fn rank(query: &str, cands: &[Cand]) -> Vec<usize> {
    let q = query.trim().to_lowercase();
    let mut hits: Vec<(u8, OrgKind, String, usize)> = Vec::new();
    for (i, c) in cands.iter().enumerate() {
        let n = c.name.to_lowercase();
        let score = if q.is_empty() || n == q {
            Some(0)
        } else if n.starts_with(&q) {
            Some(1)
        } else if n
            .split(|ch: char| !ch.is_alphanumeric())
            .any(|w| w.starts_with(&q))
        {
            Some(2)
        } else if n.contains(&q) {
            Some(3)
        } else if subsequence(&q, &n) {
            Some(4)
        } else {
            None
        };
        if let Some(s) = score {
            hits.push((s, c.kind, n, i));
        }
    }
    if q.is_empty() {
        // Empty query: keep the sidebar's own order.
        return hits.into_iter().map(|h| h.3).collect();
    }
    hits.sort();
    hits.into_iter().map(|h| h.3).collect()
}

fn subsequence(q: &str, s: &str) -> bool {
    let mut it = s.chars();
    q.chars().all(|c| it.any(|x| x == c))
}

/// Does some candidate have exactly this name (any kind, ignoring case)?
pub fn has_exact(query: &str, cands: &[Cand]) -> bool {
    let q = query.trim().to_lowercase();
    cands.iter().any(|c| c.name.to_lowercase() == q)
}

// ---- pure: drag and drop ----------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OrgAction {
    /// Add `ids` to a playlist, or assign a tag or category (never removes).
    Add { to: OrgRef, ids: Vec<i64> },
    /// Add when some lack it, remove when all have it.
    Toggle { to: OrgRef, ids: Vec<i64> },
}

/// What dropping `ids` on a sidebar row does.
pub fn drop_action(target: OrgRef, ids: &[i64]) -> Option<OrgAction> {
    if ids.is_empty() {
        return None;
    }
    Some(OrgAction::Add {
        to: target,
        ids: ids.to_vec(),
    })
}

// ---- pure: playlist moves ---------------------------------------------------

/// Moves that shift every id of `moving` one place up (or down) inside
/// `order`. Runs of adjacent moved items travel together; each entry is
/// `(run, before)` for `playlist.move`, to be applied in the given order.
pub fn plan_move(order: &[i64], moving: &HashSet<i64>, up: bool) -> Vec<(Vec<i64>, Option<i64>)> {
    let mut runs: Vec<(usize, usize)> = Vec::new(); // inclusive start..=end
    let mut i = 0;
    while i < order.len() {
        if moving.contains(&order[i]) {
            let s = i;
            while i + 1 < order.len() && moving.contains(&order[i + 1]) {
                i += 1;
            }
            runs.push((s, i));
        }
        i += 1;
    }
    let mut plan = Vec::new();
    if up {
        for (s, e) in runs {
            if s == 0 {
                continue;
            }
            plan.push((order[s..=e].to_vec(), Some(order[s - 1])));
        }
    } else {
        for (s, e) in runs.into_iter().rev() {
            if e + 1 >= order.len() {
                continue;
            }
            // After the follower: before the one behind it, or at the end.
            plan.push((order[s..=e].to_vec(), order.get(e + 2).copied()));
        }
    }
    plan
}

/// Next index to play from `cur` in direction `delta`, skipping missing
/// items and wrapping when `wrap`. `None` when nothing playable is left.
pub fn next_index(
    cur: u64,
    delta: i64,
    total: u64,
    wrap: bool,
    missing: impl Fn(u64) -> bool,
) -> Option<u64> {
    if total == 0 || delta == 0 {
        return None;
    }
    let mut i = cur as i64;
    for _ in 0..total.min(SKIP_LIMIT) {
        i += delta.signum();
        if i < 0 || i >= total as i64 {
            if !wrap {
                return None;
            }
            i = i.rem_euclid(total as i64);
        }
        if i as u64 == cur {
            return None;
        }
        if !missing(i as u64) {
            return Some(i as u64);
        }
    }
    None
}

/// Next state of a label filter after clicking `id`.
pub fn combine_labels(
    mut all: Vec<i64>,
    mut any: Vec<i64>,
    id: i64,
    ctrl: bool,
    shift: bool,
) -> (Vec<i64>, Vec<i64>) {
    let toggle = |v: &mut Vec<i64>| {
        if let Some(p) = v.iter().position(|x| *x == id) {
            v.remove(p);
        } else if v.len() < MAX_FILTER_IDS {
            v.push(id);
        }
    };
    if ctrl {
        toggle(&mut all);
    } else if shift {
        toggle(&mut any);
    } else {
        all = vec![id];
        any.clear();
    }
    (all, any)
}

// ---- undo -------------------------------------------------------------------

/// The inverse of one organise operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UndoOp {
    /// Take `ids` out of a playlist (inverse of an add).
    PlaylistRemove { id: i64, ids: Vec<i64> },
    /// Put a playlist back to exactly this order (inverse of remove / move).
    PlaylistOrder { id: i64, order: Vec<i64> },
    /// Set (`on`) or clear a label on `ids`.
    Assign {
        kind: LabelKind,
        label: i64,
        ids: Vec<i64>,
        on: bool,
    },
    /// Put favorite, rating and hidden flags back (inverse of a bulk change).
    UserData { rows: Vec<UserState> },
    /// Recreate a deleted playlist.
    PlaylistDeleted {
        old: i64,
        name: String,
        color: Option<u32>,
        private: bool,
        items: Vec<i64>,
    },
    /// Recreate a deleted tag or category. The items of a category that had
    /// sub-categories are not restored: the library keeps no per-level list.
    LabelDeleted {
        kind: LabelKind,
        old: i64,
        name: String,
        parent: Option<i64>,
        color: Option<u32>,
        private: bool,
        items: Vec<i64>,
    },
}

/// What applying an undo created, so later entries can follow the new id.
#[derive(Debug, PartialEq, Eq)]
pub enum Recreated {
    Nothing,
    Playlist { old: i64, new: i64 },
    Label { kind: LabelKind, old: i64, new: i64 },
}

impl UndoOp {
    pub fn apply(&self, db: &mut Db) -> Result<Recreated, Error> {
        match self {
            UndoOp::PlaylistRemove { id, ids } => {
                db.playlist_remove(*id, ids)?;
                Ok(Recreated::Nothing)
            }
            UndoOp::PlaylistOrder { id, order } => {
                let cur = db.playlist_item_ids(*id)?;
                for chunk in cur.chunks(MAX_IDS) {
                    db.playlist_remove(*id, chunk)?;
                }
                for chunk in order.chunks(MAX_IDS) {
                    db.playlist_add(*id, chunk, None)?;
                }
                Ok(Recreated::Nothing)
            }
            UndoOp::Assign {
                kind,
                label,
                ids,
                on,
            } => {
                db.label_assign(*kind, &[*label], ids, !on)?;
                Ok(Recreated::Nothing)
            }
            UndoOp::UserData { rows } => {
                db.bulk_restore(rows)?;
                Ok(Recreated::Nothing)
            }
            UndoOp::PlaylistDeleted {
                old,
                name,
                color,
                private,
                items,
            } => {
                let new = db.playlist_create(name, *color)?;
                if *private {
                    db.playlist_set(new, None, Some(true), None)?;
                }
                for chunk in items.chunks(MAX_IDS) {
                    db.playlist_add(new, chunk, None)?;
                }
                Ok(Recreated::Playlist { old: *old, new })
            }
            UndoOp::LabelDeleted {
                kind,
                old,
                name,
                parent,
                color,
                private,
                items,
            } => {
                // The parent may be gone by now: fall back to the top level.
                let parent = parent.filter(|p| {
                    db.labels(*kind)
                        .map(|l| l.iter().any(|x| x.id == *p))
                        .unwrap_or(false)
                });
                let new = db.label_create(*kind, name, parent, *color)?;
                if *private {
                    db.label_set(*kind, new, None, Some(true), None)?;
                }
                for chunk in items.chunks(MAX_IDS) {
                    db.label_assign(*kind, &[new], chunk, true)?;
                }
                Ok(Recreated::Label {
                    kind: *kind,
                    old: *old,
                    new,
                })
            }
        }
    }

    /// Point entries that named a deleted-then-recreated item at its new id.
    fn follow(&mut self, r: &Recreated) {
        match (self, r) {
            (
                UndoOp::PlaylistRemove { id, .. } | UndoOp::PlaylistOrder { id, .. },
                Recreated::Playlist { old, new },
            ) if *id == *old => *id = *new,
            (UndoOp::Assign { kind, label, .. }, Recreated::Label { kind: k, old, new })
                if *kind == *k && *label == *old =>
            {
                *label = *new
            }
            _ => {}
        }
    }
}

/// Bounded stack of inverse operations, in memory only.
#[derive(Default, Debug)]
pub struct UndoStack {
    ops: Vec<UndoOp>,
}

impl UndoStack {
    pub fn push(&mut self, op: UndoOp) {
        self.ops.push(op);
        if self.ops.len() > UNDO_DEPTH {
            self.ops.remove(0);
        }
    }

    pub fn pop(&mut self) -> Option<UndoOp> {
        self.ops.pop()
    }

    pub fn clear(&mut self) {
        self.ops.clear();
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.ops.len()
    }

    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.ops.is_empty()
    }

    fn follow(&mut self, r: &Recreated) {
        for op in &mut self.ops {
            op.follow(r);
        }
    }
}

// ---- operations on the database (each returns its inverse) -------------------

fn has_label(db: &Db, id: i64, label: i64) -> bool {
    db.labels_of(id)
        .map(|(t, c)| t.iter().chain(c.iter()).any(|l| l.id == label))
        .unwrap_or(false)
}

/// Append `ids` to a playlist. `(added, skipped, undo)`.
pub fn do_playlist_add(
    db: &mut Db,
    pl: i64,
    ids: &[i64],
) -> Result<(u64, u64, Option<UndoOp>), Error> {
    let before: HashSet<i64> = db.playlist_item_ids(pl)?.into_iter().collect();
    let fresh: Vec<i64> = ids
        .iter()
        .copied()
        .filter(|i| !before.contains(i))
        .collect();
    let (added, skipped) = db.playlist_add(pl, ids, None)?;
    let undo = (added > 0).then_some(UndoOp::PlaylistRemove { id: pl, ids: fresh });
    Ok((added, skipped, undo))
}

/// Remove `ids` from a playlist. `(removed, undo)`; no undo above the cap.
pub fn do_playlist_remove(
    db: &mut Db,
    pl: i64,
    ids: &[i64],
) -> Result<(u64, Option<UndoOp>), Error> {
    let order = db.playlist_item_ids(pl)?;
    let n = db.playlist_remove(pl, ids)?;
    let undo =
        (n > 0 && order.len() <= SNAPSHOT_CAP).then_some(UndoOp::PlaylistOrder { id: pl, order });
    Ok((n, undo))
}

/// Move `ids` one place up or down. `(moved, undo)`.
pub fn do_playlist_move(
    db: &mut Db,
    pl: i64,
    ids: &[i64],
    up: bool,
) -> Result<(u64, Option<UndoOp>), Error> {
    let order = db.playlist_item_ids(pl)?;
    let moving: HashSet<i64> = ids.iter().copied().collect();
    let plan = plan_move(&order, &moving, up);
    let mut moved = 0;
    for (run, before) in &plan {
        moved += db.playlist_move(pl, run, *before)?;
    }
    let undo = (moved > 0 && order.len() <= SNAPSHOT_CAP)
        .then_some(UndoOp::PlaylistOrder { id: pl, order });
    Ok((moved, undo))
}

/// Add to the playlist unless every id is already in it, then remove.
/// `(now_in, changed, skipped, undo)`.
pub fn do_playlist_toggle(
    db: &mut Db,
    pl: i64,
    ids: &[i64],
) -> Result<(bool, u64, u64, Option<UndoOp>), Error> {
    let members: HashSet<i64> = db.playlist_item_ids(pl)?.into_iter().collect();
    if !ids.is_empty() && ids.iter().all(|i| members.contains(i)) {
        let (n, undo) = do_playlist_remove(db, pl, ids)?;
        Ok((false, n, 0, undo))
    } else {
        let (a, s, undo) = do_playlist_add(db, pl, ids)?;
        Ok((true, a, s, undo))
    }
}

/// Set (`Some(true)`), clear (`Some(false)`) or toggle (`None`) a tag or
/// category on `ids`. `(now_on, changed, skipped, undo)`.
pub fn do_assign(
    db: &mut Db,
    kind: LabelKind,
    label: i64,
    ids: &[i64],
    set: Option<bool>,
) -> Result<(bool, u64, u64, Option<UndoOp>), Error> {
    let have: Vec<bool> = ids.iter().map(|i| has_label(db, *i, label)).collect();
    let on = set.unwrap_or_else(|| have.iter().any(|h| !h));
    let changed: Vec<i64> = ids
        .iter()
        .zip(&have)
        .filter(|(_, h)| **h != on)
        .map(|(i, _)| *i)
        .collect();
    if !changed.is_empty() {
        db.label_assign(kind, &[label], &changed, on)?;
    }
    let n = changed.len() as u64;
    let skipped = ids.len() as u64 - n;
    let undo = (n > 0).then_some(UndoOp::Assign {
        kind,
        label,
        ids: changed,
        on,
    });
    Ok((on, n, skipped, undo))
}

/// Favorite, rate or hide many items in one transaction.
/// `(items changed, undo)`.
pub fn do_bulk(
    db: &mut Db,
    ids: &[i64],
    patch: &MediaPatch,
) -> Result<(u64, Option<UndoOp>), Error> {
    if ids.len() > MAX_IDS {
        return Err(Error::Invalid(format!("at most {MAX_IDS} items per call")));
    }
    let before = db.bulk_patch(ids, patch)?;
    let n = before.len() as u64;
    let undo = (!before.is_empty()).then_some(UndoOp::UserData { rows: before });
    Ok((n, undo))
}

/// Delete a playlist, keeping what undo needs. `(items it held, undo)`.
pub fn do_delete_playlist(db: &mut Db, id: i64) -> Result<(u64, Option<UndoOp>), Error> {
    let pl: Option<Playlist> = db.playlists()?.into_iter().find(|p| p.id == id);
    let items = db.playlist_item_ids(id)?;
    let n = db.playlist_delete(id)?;
    let undo = pl
        .filter(|_| items.len() <= SNAPSHOT_CAP)
        .map(|p| UndoOp::PlaylistDeleted {
            old: id,
            name: p.name,
            color: p.color,
            private: p.private,
            items,
        });
    Ok((n, undo))
}

/// Delete a tag or category. `(memberships dropped, undo)`.
pub fn do_delete_label(
    db: &mut Db,
    kind: LabelKind,
    id: i64,
) -> Result<(u64, Option<UndoOp>), Error> {
    let info: Option<LabelInfo> = db.labels(kind)?.into_iter().find(|l| l.id == id);
    // Items of a leaf label are exactly its filter result.
    let leaf = info.as_ref().is_some_and(|l| l.total == l.count);
    let items = if leaf {
        let f = Filter {
            include_missing: true,
            hidden: HiddenMode::Include,
            org: OrgFilter {
                labels_all: vec![id],
                ..Default::default()
            },
            limit: Some(SNAPSHOT_CAP as u64 + 1),
            ..Default::default()
        };
        db.query(&f)?.into_iter().map(|m| m.id).collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    let n = db.label_delete(kind, id)?;
    let undo = info
        .filter(|_| items.len() <= SNAPSHOT_CAP)
        .map(|l| UndoOp::LabelDeleted {
            kind,
            old: id,
            name: l.name,
            parent: l.parent_id,
            color: l.color,
            private: l.private,
            items,
        });
    Ok((n, undo))
}

// ---- state ------------------------------------------------------------------

/// Playlists, categories and tags, cached per library generation.
pub(super) struct OrgCache {
    generation: u64,
    playlists: Vec<Playlist>,
    categories: Vec<LabelInfo>,
    tags: Vec<LabelInfo>,
}

struct Adding {
    kind: OrgKind,
    parent: Option<i64>,
    text: String,
    fresh: bool,
}

struct Picker {
    query: String,
    cursor: usize,
    ids: Vec<i64>,
    /// `(kind, id)` -> how many of `ids` are in it.
    member: std::collections::HashMap<(OrgKind, i64), usize>,
}

#[derive(Default)]
pub(super) struct OrgState {
    pub marked: BTreeSet<i64>,
    anchor: Option<u64>,
    /// Rubber band in progress: screen origin, scroll offset at its start, and
    /// the marks Ctrl+drag adds to.
    band: Option<(egui::Pos2, f32, BTreeSet<i64>)>,
    sig: String,
    undo: UndoStack,
    cache: Option<OrgCache>,
    picker: Option<Picker>,
    adding: Option<Adding>,
    renaming: Option<(OrgRef, String, bool)>,
    confirm: Option<(OrgRef, u64)>,
    pub loop_list: bool,
    last: Option<OrgRef>,
    tag_filter: String,
    collapsed: HashSet<i64>,
}

impl OrgState {
    pub(super) fn begin_band(&mut self, origin: egui::Pos2, offset: f32, base: BTreeSet<i64>) {
        self.band = Some((origin, offset, base));
        self.anchor = None;
    }

    pub(super) fn band_state(&self) -> Option<(egui::Pos2, f32, BTreeSet<i64>)> {
        self.band.clone()
    }

    pub(super) fn end_band(&mut self) {
        self.band = None;
    }
}

/// One drawable sidebar entry.
struct Entry {
    r: OrgRef,
    /// Real name.
    name: String,
    /// Shown name (masked in discreet mode).
    shown: String,
    /// Position among the visible entries of its kind (for masking).
    ordinal: usize,
    depth: usize,
    count: u64,
    color: Option<u32>,
    private: bool,
    has_kids: bool,
}

/// Pick-up the mode of the current hover so rows can react.
enum Click {
    None,
    Open { ctrl: bool, shift: bool },
    Chevron,
}

impl JizzApp {
    fn org_discreet(&self) -> bool {
        self.cfg.discreet || self.opts.disguised
    }

    fn org_cache(&mut self) -> &OrgCache {
        let stale = self
            .org
            .cache
            .as_ref()
            .is_none_or(|c| c.generation != self.generation);
        if stale {
            self.org.cache = Some(OrgCache {
                generation: self.generation,
                playlists: self.db.playlists().unwrap_or_default(),
                categories: self.db.labels(LabelKind::Category).unwrap_or_default(),
                tags: self.db.labels(LabelKind::Tag).unwrap_or_default(),
            });
        }
        self.org.cache.as_ref().expect("cache built above")
    }

    /// Items an organise action applies to: the open item in the viewer and
    /// player, else the marked tiles, else the selected one.
    pub(super) fn target_ids(&self) -> Vec<i64> {
        if matches!(self.view, View::Library) && !self.org.marked.is_empty() {
            return self.org.marked.iter().copied().collect();
        }
        self.target_id(&json!({})).into_iter().collect()
    }

    fn current_playlist(&self) -> Option<i64> {
        self.extra.as_ref().and_then(|f| f.org.playlist)
    }

    /// Esc: clears the marks first. True when it did.
    pub(super) fn org_escape(&mut self) -> bool {
        if self.org.marked.is_empty() {
            return false;
        }
        self.org.marked.clear();
        self.org.anchor = None;
        self.org.band = None;
        true
    }

    /// Boss key and panic switch: no organise state survives.
    pub(super) fn org_panic(&mut self) {
        self.org.marked.clear();
        self.org.anchor = None;
        self.org.band = None;
        self.org.undo.clear();
        self.org.picker = None;
        self.org.adding = None;
        self.org.renaming = None;
        self.org.confirm = None;
        self.org.last = None;
        self.org.tag_filter.clear();
        self.org.cache = None;
    }

    /// Per frame: marks do not outlive the filter that produced them.
    pub(super) fn org_tick(&mut self) {
        let sig = format!(
            "{:?}|{}|{:?}|{}|{}|{}",
            self.tab,
            self.search,
            self.extra.as_ref().map(|f| f.to_json().to_string()),
            self.sort.as_str(),
            self.sort_desc,
            self.seed,
        );
        if sig != self.org.sig {
            self.org.sig = sig;
            self.org.marked.clear();
            self.org.anchor = None;
        }
    }

    fn range_ids(&mut self, a: u64, b: u64) -> Vec<i64> {
        let (offset, limit) = range_window(a, b);
        let f = Filter {
            limit: Some(limit),
            offset,
            ..self.filter()
        };
        self.db
            .query(&f)
            .unwrap_or_default()
            .into_iter()
            .map(|m| m.id)
            .collect()
    }

    /// A click on a grid tile, with the modifiers held.
    pub(super) fn org_click(&mut self, id: i64, idx: u64, mods: Modifiers) {
        let prev = self.selected.zip(self.selected_index);
        let (ctrl, shift) = (mods.command, mods.shift);
        let mut marks = std::mem::take(&mut self.org.marked);
        let mut anchor = self.org.anchor;
        let mut fetched: Option<Vec<i64>> = None;
        if shift {
            let from = anchor.or(prev.map(|p| p.1)).unwrap_or(idx);
            fetched = Some(self.range_ids(from, idx));
        }
        click_marks(
            &mut marks,
            &mut anchor,
            prev,
            (id, idx),
            (ctrl, shift),
            |_, _| fetched.take().unwrap_or_default(),
        );
        self.org.marked = marks;
        self.org.anchor = anchor;
        self.selected = Some(id);
        self.selected_index = Some(idx);
    }

    fn select_all(&mut self) {
        let f = Filter {
            limit: Some(RANGE_CAP),
            ..self.filter()
        };
        let ids: Vec<i64> = self
            .db
            .query(&f)
            .unwrap_or_default()
            .into_iter()
            .map(|m| m.id)
            .collect();
        self.org.anchor = Some(0);
        self.org.marked = ids.into_iter().collect();
    }

    fn extend_selection(&mut self, dx: i64, dy: i64) {
        let cur = self.selected_index;
        let Some(next) = super::move_index(cur, dx, dy, self.last_cols, self.total) else {
            return;
        };
        let from = self.org.anchor.or(cur).unwrap_or(next);
        self.org.anchor = Some(from);
        self.select_index(next);
        let ids = self.range_ids(from, next);
        self.org.marked = ids.into_iter().collect();
    }

    // ---- reporting -----------------------------------------------------------

    fn org_error(&mut self, e: &Error) {
        let text = e.to_string();
        let t = if text.contains("already used") {
            tr("Name already used").to_owned()
        } else if text.contains("nest at most") {
            tr("Too deep").to_owned()
        } else {
            text
        };
        self.toast(ToastKind::Error, t);
    }

    fn toast_counts(&mut self, added: u64, skipped: u64) {
        let t = trf(
            "Added {} · skipped {}",
            &[&added.to_string(), &skipped.to_string()],
        );
        self.toast(ToastKind::Info, t);
    }

    fn toast_removed(&mut self, n: u64) {
        let t = trf("Removed {}", &[&n.to_string()]);
        self.toast(ToastKind::Info, t);
    }

    fn remember(&mut self, op: Option<UndoOp>) {
        if let Some(op) = op {
            self.org.undo.push(op);
        }
    }

    // ---- actions -------------------------------------------------------------

    /// Run one organise action and report it with counts only.
    pub(super) fn org_do(&mut self, a: OrgAction) {
        if self.read_only {
            return;
        }
        let (to, ids, toggle) = match a {
            OrgAction::Add { to, ids } => (to, ids, false),
            OrgAction::Toggle { to, ids } => (to, ids, true),
        };
        if ids.is_empty() {
            return;
        }
        let res = match to.kind.label_kind() {
            None if toggle => do_playlist_toggle(&mut self.db, to.id, &ids),
            None => do_playlist_add(&mut self.db, to.id, &ids).map(|(n, s, u)| (true, n, s, u)),
            Some(k) => do_assign(
                &mut self.db,
                k,
                to.id,
                &ids,
                if toggle { None } else { Some(true) },
            ),
        };
        match res {
            Ok((on, n, skipped, undo)) => {
                self.remember(undo);
                self.org.last = Some(to);
                if on {
                    self.toast_counts(n, skipped);
                } else {
                    self.toast_removed(n);
                }
                self.refresh();
            }
            Err(e) => self.org_error(&e),
        }
    }

    /// Favorite, rate or hide the marked (or open) items in one transaction,
    /// with one undo entry and one toast that carries a count only.
    pub(super) fn org_bulk(&mut self, ids: Vec<i64>, patch: MediaPatch) {
        if self.read_only || ids.is_empty() {
            return;
        }
        match do_bulk(&mut self.db, &ids, &patch) {
            Ok((n, undo)) => {
                self.remember(undo);
                let t = trf("Updated {}", &[&n.to_string()]);
                self.toast(ToastKind::Info, t);
                self.refresh();
            }
            Err(e) => self.org_error(&e),
        }
    }

    /// The bulk bar: visible while tiles are marked in the library.
    pub(super) fn bulk_bar(&mut self, ui: &mut Ui) {
        if !matches!(self.view, View::Library) || self.org.marked.is_empty() || self.read_only {
            return;
        }
        let ids: Vec<i64> = self.org.marked.iter().copied().collect();
        let n = ids.len().to_string();
        let mut patch: Option<MediaPatch> = None;
        let mut picker = false;
        let mut clear = false;
        let mut all = false;
        egui::Panel::bottom("bulk_bar")
            .frame(
                egui::Frame::new()
                    .fill(BG)
                    .stroke(egui::Stroke::new(1.0, LINE))
                    .inner_margin(egui::Margin::symmetric(16, 8)),
            )
            .show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    widgets::mono_label(ui, &trf("{} selected", &[&n]), APP);
                    ui.add_space(S);
                    widgets::mono_label(ui, tr("Rate"), FAINT);
                    for r in 1..=5u8 {
                        if widgets::button_outline(ui, &format!("{r}★")).clicked() {
                            patch = Some(MediaPatch {
                                rating: Some(r),
                                ..Default::default()
                            });
                        }
                    }
                    if widgets::button_outline(ui, tr("Clear rating")).clicked() {
                        patch = Some(MediaPatch {
                            rating: Some(0),
                            ..Default::default()
                        });
                    }
                    ui.add_space(S);
                    if widgets::button_outline(ui, tr("Favorite")).clicked() {
                        patch = Some(MediaPatch {
                            favorite: Some(true),
                            ..Default::default()
                        });
                    }
                    if widgets::button_outline(ui, tr("Unfavorite")).clicked() {
                        patch = Some(MediaPatch {
                            favorite: Some(false),
                            ..Default::default()
                        });
                    }
                    if widgets::button_outline(ui, tr("Hide")).clicked() {
                        patch = Some(MediaPatch {
                            hidden: Some(true),
                            ..Default::default()
                        });
                    }
                    if widgets::button_outline(ui, tr("Unhide")).clicked() {
                        patch = Some(MediaPatch {
                            hidden: Some(false),
                            ..Default::default()
                        });
                    }
                    ui.add_space(S);
                    if widgets::button_solid_bar(ui, tr("Add to…")).clicked() {
                        picker = true;
                    }
                    if widgets::button_outline(ui, tr("Select all")).clicked() {
                        all = true;
                    }
                    if widgets::button_outline(ui, tr("Clear selection")).clicked() {
                        clear = true;
                    }
                });
            });
        if let Some(p) = patch {
            self.org_bulk(ids.clone(), p);
        }
        if picker {
            self.open_picker(ids);
        }
        if all {
            self.select_all();
        }
        if clear {
            self.org_escape();
        }
    }

    fn org_repeat_last(&mut self) {
        let Some(to) = self.org.last else { return };
        let ids = self.target_ids();
        self.org_do(OrgAction::Add { to, ids });
    }

    fn org_create(&mut self, kind: OrgKind, name: &str, parent: Option<i64>) -> Option<i64> {
        if self.read_only {
            return None;
        }
        let r = match kind.label_kind() {
            None => self.db.playlist_create(name, None),
            Some(k) => self.db.label_create(k, name, parent, None),
        };
        match r {
            Ok(id) => {
                self.refresh();
                Some(id)
            }
            Err(e) => {
                self.org_error(&e);
                None
            }
        }
    }

    fn org_rename(&mut self, r: OrgRef, name: &str) {
        let res = match r.kind.label_kind() {
            None => self.db.playlist_rename(r.id, name),
            Some(k) => self.db.label_rename(k, r.id, name),
        };
        match res {
            Ok(()) => self.refresh(),
            Err(e) => self.org_error(&e),
        }
    }

    fn org_set(&mut self, r: OrgRef, color: Option<Option<u32>>, private: Option<bool>) {
        let res = match r.kind.label_kind() {
            None => self.db.playlist_set(r.id, color, private, None),
            Some(k) => self.db.label_set(k, r.id, color, private, None),
        };
        match res {
            Ok(()) => self.refresh(),
            Err(e) => self.org_error(&e),
        }
    }

    fn org_delete(&mut self, r: OrgRef) {
        let res = match r.kind.label_kind() {
            None => do_delete_playlist(&mut self.db, r.id),
            Some(k) => do_delete_label(&mut self.db, k, r.id),
        };
        match res {
            Ok((n, undo)) => {
                self.remember(undo);
                self.toast_removed(n);
                // Leave a view that no longer exists.
                if self.view_is(r) {
                    self.push("view.filter", json!({ "tab": "library" }));
                }
                self.refresh();
            }
            Err(e) => self.org_error(&e),
        }
    }

    fn view_is(&self, r: OrgRef) -> bool {
        let Some(f) = &self.extra else { return false };
        match r.kind {
            OrgKind::Playlist => f.org.playlist == Some(r.id),
            _ => f.org.labels_all.contains(&r.id) || f.org.labels_any.contains(&r.id),
        }
    }

    fn org_undo(&mut self) {
        if self.ctx.egui_wants_keyboard_input() {
            return;
        }
        let Some(op) = self.org.undo.pop() else {
            self.toast(ToastKind::Info, tr("Nothing to undo"));
            return;
        };
        match op.apply(&mut self.db) {
            Ok(r) => {
                self.org.undo.follow(&r);
                self.toast(ToastKind::Info, tr("Undone"));
                self.refresh();
            }
            Err(e) => self.org_error(&e),
        }
    }

    fn org_remove_current(&mut self) {
        let Some(pl) = self.current_playlist() else {
            return;
        };
        let ids = self.target_ids();
        if ids.is_empty() {
            return;
        }
        match do_playlist_remove(&mut self.db, pl, &ids) {
            Ok((n, undo)) => {
                self.remember(undo);
                self.toast_removed(n);
                self.org.marked.clear();
                self.selected_index = None;
                self.refresh();
            }
            Err(e) => self.org_error(&e),
        }
    }

    fn org_move(&mut self, delta: i64) {
        let Some(pl) = self.current_playlist() else {
            return;
        };
        let ids = self.target_ids();
        if ids.is_empty() {
            return;
        }
        let up = delta < 0;
        match do_playlist_move(&mut self.db, pl, &ids, up) {
            Ok((n, undo)) => {
                if n > 0 {
                    self.remember(undo);
                    if self.selected.is_some_and(|s| ids.contains(&s))
                        && let Some(i) = self.selected_index
                    {
                        let j = if up { i.saturating_sub(1) } else { i + 1 };
                        self.selected_index = Some(j.min(self.total.saturating_sub(1)));
                        self.scroll_to = self.selected_index;
                    }
                    self.refresh();
                }
            }
            Err(e) => self.org_error(&e),
        }
    }

    // ---- opening a list ------------------------------------------------------

    /// Show one playlist, tag or category as the current list.
    fn org_open(&mut self, r: OrgRef, ctrl: bool, shift: bool) {
        let mut spec;
        let generic;
        match r.kind {
            OrgKind::Playlist => {
                spec = self.db.playlist_filter(r.id, None, 0);
                generic = tr("Playlist");
            }
            _ => {
                let (all, any) = self
                    .extra
                    .as_ref()
                    .filter(|f| f.org.playlist.is_none())
                    .map(|f| (f.org.labels_all.clone(), f.org.labels_any.clone()))
                    .unwrap_or_default();
                let (all, any) = combine_labels(all, any, r.id, ctrl, shift);
                if all.is_empty() && any.is_empty() {
                    self.push("view.filter", json!({ "tab": "library" }));
                    return;
                }
                spec = Filter {
                    include_missing: true,
                    sort: if self.sort == SortKey::Position {
                        SortKey::default()
                    } else {
                        self.sort
                    },
                    desc: self.sort_desc,
                    org: OrgFilter {
                        labels_all: all,
                        labels_any: any,
                        ..Default::default()
                    },
                    ..Default::default()
                };
                generic = tr(r.kind.noun());
            }
        }
        spec.seed = self.seed;
        self.close_view();
        self.tab = Tab::Library;
        self.sort = spec.sort;
        self.sort_desc = spec.desc;
        self.extra = Some(spec);
        // Never the name: the breadcrumb is read over shoulders.
        self.view_label = generic.to_owned();
        self.search.clear();
        self.selected_index = None;
        self.org.marked.clear();
        self.refresh();
    }

    /// Open the first item that can play, from the top of the list.
    fn play_from_top(&mut self) {
        let mut i = 0;
        while i < self.total.min(SKIP_LIMIT) {
            self.ensure_page(i, i + 1);
            let missing = self
                .page
                .items
                .get((i - self.page.offset) as usize)
                .is_none_or(|m| m.missing);
            if !missing {
                self.open_index(i);
                return;
            }
            i += 1;
        }
    }

    fn is_missing_at(&mut self, i: u64) -> bool {
        self.ensure_page(i, i + 1);
        self.page
            .items
            .get((i.saturating_sub(self.page.offset)) as usize)
            .is_none_or(|m| m.missing)
    }

    /// Next playable index for autoplay and list stepping.
    pub(super) fn next_playable(&mut self, cur: u64, delta: i64) -> Option<u64> {
        let (total, wrap) = (self.total, self.org.loop_list);
        // `next_index` takes a shared closure; fetch pages through a cell.
        let this = std::cell::RefCell::new(self);
        next_index(cur, delta, total, wrap, |i| {
            this.borrow_mut().is_missing_at(i)
        })
    }

    // ---- export --------------------------------------------------------------

    pub(super) fn org_export(&mut self, id: i64) {
        // An `.m3u8` lists media paths in clear: an encrypted library asks
        // first, like the JSON export does.
        if self.opener.key.is_some() {
            self.vault_ui.m3u_warn = Some(id);
            return;
        }
        self.org_export_confirmed(id);
    }

    pub(super) fn org_export_confirmed(&mut self, id: i64) {
        let name = if self.org_discreet() {
            "playlist.m3u8".to_owned()
        } else {
            let n = self
                .org_cache()
                .playlists
                .iter()
                .find(|p| p.id == id)
                .map(|p| p.name.clone())
                .unwrap_or_else(|| "playlist".into());
            let clean: String = n
                .chars()
                .map(|c| {
                    if c.is_alphanumeric() || c == '-' || c == '_' {
                        c
                    } else {
                        '_'
                    }
                })
                .collect();
            format!("{clean}.m3u8")
        };
        let (tx, ctx) = (self.tx.clone(), self.ctx.clone());
        std::thread::spawn(move || {
            let dest = rfd::FileDialog::new()
                .set_title(tr("Export M3U8…"))
                .set_file_name(name)
                .add_filter("M3U8", &["m3u8"])
                .save_file();
            let _ = tx.send(JobMsg::OrgExport(id, dest));
            ctx.request_repaint();
        });
    }

    pub(super) fn org_export_to(&mut self, id: i64, dest: Option<std::path::PathBuf>) {
        let Some(dest) = dest else { return };
        match self.db.export_m3u(id, &dest, &M3uOpts::default()) {
            Ok(r) => {
                let t = trf(
                    "Exported {} · skipped {}",
                    &[
                        &r.written.to_string(),
                        &(r.skipped_missing + r.skipped_unsafe).to_string(),
                    ],
                );
                self.toast(ToastKind::Info, t);
            }
            Err(e) => self.org_error(&e),
        }
    }

    // ---- picker --------------------------------------------------------------

    fn open_picker(&mut self, ids: Vec<i64>) {
        if ids.is_empty() || self.read_only {
            return;
        }
        let mut member = std::collections::HashMap::new();
        // Counting is capped so a huge selection stays instant.
        for id in ids.iter().take(200) {
            for p in self.db.playlists_of(&[*id]).unwrap_or_default() {
                *member.entry((OrgKind::Playlist, p)).or_insert(0) += 1;
            }
            if let Ok((tags, cats)) = self.db.labels_of(*id) {
                for l in tags {
                    *member.entry((OrgKind::Tag, l.id)).or_insert(0) += 1;
                }
                for l in cats {
                    *member.entry((OrgKind::Category, l.id)).or_insert(0) += 1;
                }
            }
        }
        self.org.picker = Some(Picker {
            query: String::new(),
            cursor: 0,
            ids,
            member,
        });
    }

    fn picker_cands(&mut self) -> Vec<Cand> {
        let discreet = self.org_discreet();
        let c = self.org_cache();
        let mut out = Vec::new();
        let flags = |v: &[LabelInfo]| {
            visible_flags(
                &v.iter().map(|l| (l.depth, l.private)).collect::<Vec<_>>(),
                discreet,
            )
        };
        let pl_flags: Vec<bool> = c
            .playlists
            .iter()
            .map(|p| !(discreet && p.private))
            .collect();
        for (p, ok) in c.playlists.iter().zip(pl_flags) {
            if ok {
                out.push(Cand {
                    kind: OrgKind::Playlist,
                    id: p.id,
                    name: p.name.clone(),
                });
            }
        }
        for (kind, list) in [(OrgKind::Category, &c.categories), (OrgKind::Tag, &c.tags)] {
            for (l, ok) in list.iter().zip(flags(list)) {
                if ok {
                    out.push(Cand {
                        kind,
                        id: l.id,
                        name: l.name.clone(),
                    });
                }
            }
        }
        out
    }

    /// The picker window. Enter toggles, Shift+Enter toggles and (in triage)
    /// keeps, Esc closes.
    pub(super) fn picker_window(&mut self, ctx: &egui::Context) {
        let Some(mut p) = self.org.picker.take() else {
            return;
        };
        let discreet = self.org_discreet();
        let share_safe = self.cfg.screen_share_safe;
        let cands: Vec<Cand> = self.picker_cands();
        // Ordinals for masking follow the sidebar's own order per kind.
        let ordinal = |c: &Cand| -> usize {
            cands
                .iter()
                .filter(|x| x.kind == c.kind)
                .position(|x| x.id == c.id)
                .unwrap_or(0)
        };
        let shown: Vec<usize> = rank(&p.query, &cands);
        let creates: Vec<OrgKind> = if p.query.trim().is_empty() || has_exact(&p.query, &cands) {
            Vec::new()
        } else {
            vec![OrgKind::Playlist, OrgKind::Tag, OrgKind::Category]
        };
        let rows = shown.len().min(PICKER_ROWS) + creates.len();

        // Keys first, so the text field does not eat them.
        let (down, up, enter, shift_enter, esc) = ctx.input_mut(|i| {
            (
                i.consume_key(Modifiers::NONE, Key::ArrowDown),
                i.consume_key(Modifiers::NONE, Key::ArrowUp),
                i.consume_key(Modifiers::NONE, Key::Enter),
                i.consume_key(Modifiers::SHIFT, Key::Enter),
                i.consume_key(Modifiers::NONE, Key::Escape),
            )
        });
        if rows > 0 {
            if down {
                p.cursor = (p.cursor + 1) % rows;
            }
            if up {
                p.cursor = (p.cursor + rows - 1) % rows;
            }
        }
        p.cursor = p.cursor.min(rows.saturating_sub(1));

        let mut chosen: Option<usize> = None;
        let mut close = esc;
        egui::Window::new(tr("Add to…"))
            .id(egui::Id::new("org.picker"))
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_TOP, vec2(0.0, 96.0))
            .default_width(360.0)
            .show(ctx, |ui| {
                let r = ui.add(
                    egui::TextEdit::singleline(&mut p.query)
                        .hint_text(tr("Add to…"))
                        .desired_width(f32::INFINITY),
                );
                r.request_focus();
                if r.changed() {
                    p.cursor = 0;
                }
                ui.add_space(S);
                let count = p.ids.len() as u64;
                widgets::mono_label(ui, &trf("{} selected", &[&count.to_string()]), FAINT);
                ui.add_space(S);
                for (row, &ci) in shown.iter().take(PICKER_ROWS).enumerate() {
                    let c = &cands[ci];
                    let name = shown_name(c.kind, ordinal(c), &c.name, discreet);
                    let n = p.member.get(&(c.kind, c.id)).copied().unwrap_or(0);
                    let state = if n == 0 {
                        0
                    } else if n >= p.ids.len().min(200) {
                        2
                    } else {
                        1
                    };
                    let resp = picker_row(
                        ui,
                        row == p.cursor,
                        &name,
                        tr(c.kind.noun()),
                        state,
                        share_safe,
                    );
                    if resp.clicked() {
                        chosen = Some(row);
                    }
                }
                for (j, kind) in creates.iter().enumerate() {
                    let row = shown.len().min(PICKER_ROWS) + j;
                    let label = trf(kind.create_template(), &[p.query.trim()]);
                    let resp = picker_row(ui, row == p.cursor, &label, "+", 0, false);
                    if resp.clicked() {
                        chosen = Some(row);
                    }
                }
                if rows == 0 {
                    ui.label(theme::hud(tr("Create a playlist with + or P"), FAINT));
                }
            });
        if (enter || shift_enter) && rows > 0 {
            chosen = Some(p.cursor);
        }
        if let Some(row) = chosen {
            let visible = shown.len().min(PICKER_ROWS);
            let target = if row < visible {
                let c = &cands[shown[row]];
                Some(OrgRef {
                    kind: c.kind,
                    id: c.id,
                })
            } else {
                let kind = creates[row - visible];
                self.org_create(kind, &p.query.clone(), None)
                    .map(|id| OrgRef { kind, id })
            };
            if let Some(to) = target {
                self.org_do(OrgAction::Toggle {
                    to,
                    ids: p.ids.clone(),
                });
                // Membership may have flipped: recount for the checkmarks.
                let ids = p.ids.clone();
                self.open_picker(ids);
                if let Some(np) = self.org.picker.take() {
                    p.member = np.member;
                    p.query.clear();
                    p.cursor = 0;
                }
                if shift_enter && self.triage.is_some() {
                    self.push("triage.keep", json!({}));
                }
            }
            if shift_enter {
                close = true;
            }
        }
        if !close {
            self.org.picker = Some(p);
        }
    }

    // ---- sidebar -------------------------------------------------------------

    fn entries(&mut self, kind: OrgKind) -> Vec<Entry> {
        let discreet = self.org_discreet();
        let collapsed = self.org.collapsed.clone();
        let c = self.org_cache();
        match kind {
            OrgKind::Playlist => {
                let mut ord = 0;
                c.playlists
                    .iter()
                    .filter(|p| !(discreet && p.private))
                    .map(|p| {
                        let e = Entry {
                            r: OrgRef { kind, id: p.id },
                            name: p.name.clone(),
                            shown: shown_name(kind, ord, &p.name, discreet),
                            ordinal: ord,
                            depth: 0,
                            count: p.count,
                            color: p.color,
                            private: p.private,
                            has_kids: false,
                        };
                        ord += 1;
                        e
                    })
                    .collect()
            }
            _ => {
                let list = if kind == OrgKind::Category {
                    &c.categories
                } else {
                    &c.tags
                };
                let flags = visible_flags(
                    &list
                        .iter()
                        .map(|l| (l.depth, l.private))
                        .collect::<Vec<_>>(),
                    discreet,
                );
                let mut out = Vec::new();
                let mut hide_below: Option<u32> = None;
                let mut ord = 0;
                for (i, l) in list.iter().enumerate() {
                    if !flags[i] {
                        continue;
                    }
                    if let Some(d) = hide_below {
                        if l.depth > d {
                            ord += 1;
                            continue;
                        }
                        hide_below = None;
                    }
                    let has_kids = list
                        .get(i + 1)
                        .is_some_and(|n| n.depth > l.depth && flags[i + 1]);
                    if has_kids && collapsed.contains(&l.id) {
                        hide_below = Some(l.depth);
                    }
                    out.push(Entry {
                        r: OrgRef { kind, id: l.id },
                        name: l.name.clone(),
                        shown: shown_name(kind, ord, &l.name, discreet),
                        ordinal: ord,
                        depth: l.depth as usize,
                        count: if kind == OrgKind::Category {
                            l.total
                        } else {
                            l.count
                        },
                        color: l.color,
                        private: l.private,
                        has_kids,
                    });
                    ord += 1;
                }
                out
            }
        }
    }

    /// Playlists, categories and tags. Drawn between the views and folders.
    pub(super) fn org_sidebar(&mut self, ui: &mut Ui) {
        if self.stats.total == 0 {
            return;
        }
        let discreet = self.org_discreet();
        let share_safe = self.cfg.screen_share_safe;
        let active: Vec<OrgRef> = self.active_refs();
        let mut open: Option<(OrgRef, bool, bool)> = None;
        let mut actions: Vec<OrgAction> = Vec::new();
        let mut menu: Vec<MenuAct> = Vec::new();

        let sections = [
            ("03", OrgKind::Playlist, "Playlists"),
            ("04", OrgKind::Category, "Categories"),
            ("05", OrgKind::Tag, "Tags"),
        ];
        for (num, kind, title) in sections {
            ui.add_space(S * 2.0);
            if section_plus(ui, num, tr(title)) && !self.read_only {
                self.org.adding = Some(Adding {
                    kind,
                    parent: None,
                    text: String::new(),
                    fresh: true,
                });
            }
            self.inline_add(ui, kind);
            let mut entries = self.entries(kind);
            if kind == OrgKind::Tag && entries.len() > TAG_FILTER_AFTER {
                ui.horizontal(|ui| {
                    ui.add_space(16.0);
                    ui.add(
                        egui::TextEdit::singleline(&mut self.org.tag_filter)
                            .hint_text(tr("Tags"))
                            .desired_width(140.0),
                    );
                });
                let q = self.org.tag_filter.trim().to_lowercase();
                if !q.is_empty() {
                    entries.retain(|e| e.name.to_lowercase().contains(&q));
                }
            }
            if entries.is_empty() {
                ui.add_space(S);
                ui.horizontal(|ui| {
                    ui.add_space(16.0);
                    let hint = match kind {
                        OrgKind::Playlist => tr("Create a playlist with + or P"),
                        OrgKind::Category => tr("New category"),
                        OrgKind::Tag => tr("No tags yet"),
                    };
                    ui.label(theme::hud(hint, FAINT));
                });
                continue;
            }
            for e in &entries {
                self.entry_row(
                    ui,
                    e,
                    (discreet, share_safe),
                    active.contains(&e.r),
                    &mut open,
                    &mut actions,
                    &mut menu,
                );
            }
        }
        if let Some((r, ctrl, shift)) = open {
            self.org_open(r, ctrl, shift);
        }
        for a in actions {
            self.org_do(a);
        }
        for m in menu {
            self.menu_act(m);
        }
    }

    fn active_refs(&self) -> Vec<OrgRef> {
        let Some(f) = &self.extra else {
            return Vec::new();
        };
        let mut v = Vec::new();
        if let Some(p) = f.org.playlist {
            v.push(OrgRef {
                kind: OrgKind::Playlist,
                id: p,
            });
        }
        let cats: HashSet<i64> = self
            .org
            .cache
            .as_ref()
            .map(|c| c.categories.iter().map(|l| l.id).collect())
            .unwrap_or_default();
        for id in f.org.labels_all.iter().chain(&f.org.labels_any) {
            let kind = if cats.contains(id) {
                OrgKind::Category
            } else {
                OrgKind::Tag
            };
            v.push(OrgRef { kind, id: *id });
        }
        v
    }

    fn inline_add(&mut self, ui: &mut Ui, kind: OrgKind) {
        let Some(mut a) = self.org.adding.take() else {
            return;
        };
        if a.kind != kind {
            self.org.adding = Some(a);
            return;
        }
        let mut done = false;
        let mut create = false;
        ui.add_space(S);
        ui.horizontal(|ui| {
            ui.add_space(16.0);
            let r = ui.add(
                egui::TextEdit::singleline(&mut a.text)
                    .hint_text(tr(kind.new_hint()))
                    .desired_width(ui.available_width() - 16.0),
            );
            if a.fresh {
                r.request_focus();
                a.fresh = false;
            }
            if ui.input(|i| i.key_pressed(Key::Escape)) {
                done = true;
            } else if r.lost_focus() {
                if ui.input(|i| i.key_pressed(Key::Enter)) {
                    create = true;
                }
                done = true;
            }
        });
        if create && !a.text.trim().is_empty() {
            let name = a.text.trim().to_owned();
            if self.org_create(kind, &name, a.parent).is_none() {
                // Keep the box so the name can be fixed.
                a.fresh = true;
                done = false;
            }
        }
        if !done {
            self.org.adding = Some(a);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn entry_row(
        &mut self,
        ui: &mut Ui,
        e: &Entry,
        (discreet, share_safe): (bool, bool),
        active: bool,
        open: &mut Option<(OrgRef, bool, bool)>,
        actions: &mut Vec<OrgAction>,
        menu: &mut Vec<MenuAct>,
    ) {
        // Inline rename replaces the row.
        if let Some((r, text, fresh)) = &mut self.org.renaming
            && *r == e.r
        {
            let mut commit = false;
            let mut cancel = false;
            ui.horizontal(|ui| {
                ui.add_space(16.0 + e.depth as f32 * 12.0);
                let resp = ui.add(
                    egui::TextEdit::singleline(text).desired_width(ui.available_width() - 16.0),
                );
                if *fresh {
                    resp.request_focus();
                    *fresh = false;
                }
                if ui.input(|i| i.key_pressed(Key::Escape)) {
                    cancel = true;
                } else if resp.lost_focus() {
                    commit = ui.input(|i| i.key_pressed(Key::Enter));
                    cancel = !commit;
                }
            });
            if commit {
                let (r, text) = (*r, text.trim().to_owned());
                self.org.renaming = None;
                if !text.is_empty() {
                    self.org_rename(r, &text);
                }
            } else if cancel {
                self.org.renaming = None;
            }
            return;
        }
        let open_kids = !self.org.collapsed.contains(&e.r.id);
        let out = sidebar_entry(ui, e, active, open_kids, discreet, share_safe);
        match out.click {
            Click::Open { ctrl, shift } => *open = Some((e.r, ctrl, shift)),
            Click::Chevron => {
                if !self.org.collapsed.remove(&e.r.id) {
                    self.org.collapsed.insert(e.r.id);
                }
            }
            Click::None => {}
        }
        if let Some(ids) = out.dropped
            && let Some(a) = drop_action(e.r, &ids)
        {
            actions.push(a);
        }
        let read_only = self.read_only;
        out.resp.context_menu(|ui| {
            use MenuAct as M;
            if ui.button(tr("Play")).clicked() {
                menu.push(M::Play(e.r, false));
                ui.close();
            }
            if ui.button(tr("Shuffle play")).clicked() {
                menu.push(M::Play(e.r, true));
                ui.close();
            }
            if read_only {
                return;
            }
            ui.separator();
            if ui.button(tr("Rename")).clicked() {
                menu.push(M::Rename(e.r, e.name.clone()));
                ui.close();
            }
            if e.r.kind == OrgKind::Tag && !discreet && ui.button(tr("Aliases…")).clicked() {
                menu.push(M::Aliases(e.r.id));
                ui.close();
            }
            if !discreet {
                ui.menu_button(tr("Colour"), |ui| {
                    ui.horizontal(|ui| {
                        for c in PALETTE {
                            let col = rgb(c);
                            let b =
                                ui.add(egui::Button::new("").fill(col).min_size(vec2(18.0, 18.0)));
                            if b.clicked() {
                                menu.push(M::Colour(e.r, Some(c)));
                                ui.close();
                            }
                        }
                    });
                    if ui.button(tr("No colour")).clicked() {
                        menu.push(M::Colour(e.r, None));
                        ui.close();
                    }
                });
            }
            let mut private = e.private;
            if ui.checkbox(&mut private, tr("Private")).clicked() {
                menu.push(M::Private(e.r, private));
                ui.close();
            }
            if e.r.kind == OrgKind::Category && ui.button(tr("New category")).clicked() {
                menu.push(M::NewChild(e.r.id));
                ui.close();
            }
            if e.r.kind == OrgKind::Playlist && ui.button(tr("Export M3U8…")).clicked() {
                menu.push(M::Export(e.r.id));
                ui.close();
            }
            ui.separator();
            if ui.button(tr("Delete…")).clicked() {
                menu.push(M::Delete(e.r, e.count));
                ui.close();
            }
        });
    }

    fn menu_act(&mut self, m: MenuAct) {
        match m {
            MenuAct::Play(r, shuffle) => {
                self.org_open(r, false, false);
                if shuffle {
                    self.push("view.shuffle", json!({}));
                }
                self.push("org.play", json!({}));
            }
            MenuAct::Rename(r, name) => self.org.renaming = Some((r, name, true)),
            MenuAct::Aliases(tag) => {
                self.alias_edit = Some(super::meta_ui::AliasEdit {
                    tag,
                    input: String::new(),
                });
            }
            MenuAct::Colour(r, c) => self.org_set(r, Some(c), None),
            MenuAct::Private(r, p) => self.org_set(r, None, Some(p)),
            MenuAct::NewChild(parent) => {
                self.org.adding = Some(Adding {
                    kind: OrgKind::Category,
                    parent: Some(parent),
                    text: String::new(),
                    fresh: true,
                });
            }
            MenuAct::Export(id) => self.org_export(id),
            MenuAct::Delete(r, count) => {
                if count > 0 {
                    self.org.confirm = Some((r, count));
                } else {
                    self.org_delete(r);
                }
            }
        }
    }

    /// "Delete {} with {} items?" confirmation.
    pub(super) fn org_confirm_window(&mut self, ctx: &egui::Context) {
        let Some((r, count)) = self.org.confirm else {
            return;
        };
        let discreet = self.org_discreet();
        let entries = self.entries(r.kind);
        let name = entries
            .iter()
            .find(|e| e.r == r)
            .map(|e| {
                if discreet {
                    e.shown.clone()
                } else {
                    e.name.clone()
                }
            })
            .unwrap_or_default();
        let mut yes = false;
        let mut no = ctx.input(|i| i.key_pressed(Key::Escape));
        egui::Window::new(tr("Delete…"))
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, vec2(0.0, 0.0))
            .show(ctx, |ui| {
                ui.label(
                    egui::RichText::new(trf(
                        "Delete {} with {} items? Files are not touched.",
                        &[&name, &count.to_string()],
                    ))
                    .color(MUTED),
                );
                ui.add_space(S);
                ui.horizontal(|ui| {
                    if widgets::button_solid(ui, tr("Delete")).clicked() {
                        yes = true;
                    }
                    if widgets::button_outline(ui, tr("Cancel")).clicked() {
                        no = true;
                    }
                });
            });
        if yes {
            self.org.confirm = None;
            self.org_delete(r);
        } else if no {
            self.org.confirm = None;
        }
    }

    /// While dragging tiles: a small ghost with the count next to the pointer.
    pub(super) fn org_drag_ghost(&self, ctx: &egui::Context) {
        let Some(p) = egui::DragAndDrop::payload::<DragIds>(ctx) else {
            return;
        };
        let Some(pos) = ctx.pointer_latest_pos() else {
            return;
        };
        let painter = ctx.layer_painter(egui::LayerId::new(
            egui::Order::Tooltip,
            egui::Id::new("org-drag-ghost"),
        ));
        let text = trf("{} selected", &[&p.0.len().to_string()]);
        let galley = painter.layout_no_wrap(text, FontId::new(11.0, FontFamily::Monospace), TEXT);
        let rect = Rect::from_min_size(pos + vec2(14.0, 14.0), galley.size() + vec2(16.0, 8.0));
        painter.rect_filled(rect, CornerRadius::ZERO, BG);
        painter.rect_stroke(
            rect,
            CornerRadius::ZERO,
            egui::Stroke::new(1.0, LINE_STRONG),
            egui::StrokeKind::Inside,
        );
        painter.galley(rect.min + vec2(8.0, 4.0), galley, TEXT);
    }
}

enum MenuAct {
    Play(OrgRef, bool),
    Rename(OrgRef, String),
    Aliases(i64),
    Colour(OrgRef, Option<u32>),
    Private(OrgRef, bool),
    NewChild(i64),
    Export(i64),
    Delete(OrgRef, u64),
}

// ---- widgets ----------------------------------------------------------------

fn rgb(c: u32) -> Color32 {
    Color32::from_rgb((c >> 16) as u8, (c >> 8) as u8, c as u8)
}

/// Tick on a marked tile: a filled corner chip with a check, and a frame.
pub(super) fn paint_mark(p: &egui::Painter, rect: Rect) {
    let chip = Rect::from_min_size(rect.min + vec2(8.0, 28.0), vec2(16.0, 16.0));
    p.rect_stroke(
        rect,
        CornerRadius::ZERO,
        egui::Stroke::new(1.0, APP),
        egui::StrokeKind::Inside,
    );
    p.rect_filled(chip, CornerRadius::ZERO, APP);
    let st = egui::Stroke::new(1.6, BG);
    let c = chip.center();
    p.line_segment([c + vec2(-4.0, 0.0), c + vec2(-1.0, 3.0)], st);
    p.line_segment([c + vec2(-1.0, 3.0), c + vec2(4.5, -3.5)], st);
}

/// Section header with a `+` on the right. True when `+` was clicked.
fn section_plus(ui: &mut Ui, num: &str, name: &str) -> bool {
    let inner = ui.horizontal(|ui| {
        ui.add_space(16.0);
        widgets::eyebrow(ui, num, name, "");
    });
    // The plus is drawn with strokes: no font glyph needed.
    let c = egui::pos2(ui.max_rect().right() - 18.0, inner.response.rect.center().y);
    let hit = Rect::from_center_size(c, vec2(22.0, 22.0));
    let r = ui.interact(hit, ui.id().with(("org-plus", num)), egui::Sense::click());
    let col = if r.hovered() { TEXT } else { MUTED };
    let st = egui::Stroke::new(1.2, col);
    ui.painter()
        .line_segment([c - vec2(4.0, 0.0), c + vec2(4.0, 0.0)], st);
    ui.painter()
        .line_segment([c - vec2(0.0, 4.0), c + vec2(0.0, 4.0)], st);
    ui.add_space(6.0);
    r.on_hover_cursor(egui::CursorIcon::PointingHand).clicked()
}

struct EntryOut {
    resp: egui::Response,
    click: Click,
    dropped: Option<Vec<i64>>,
}

fn sidebar_entry(
    ui: &mut Ui,
    e: &Entry,
    active: bool,
    open_kids: bool,
    discreet: bool,
    share_safe: bool,
) -> EntryOut {
    let (rect, resp) =
        ui.allocate_exact_size(vec2(ui.available_width(), 26.0), egui::Sense::click());
    let hovering_drop = resp.dnd_hover_payload::<DragIds>().is_some();
    let dropped = resp.dnd_release_payload::<DragIds>().map(|p| p.0.clone());
    let p = ui.painter();
    if resp.hovered() || hovering_drop {
        p.rect_filled(rect, CornerRadius::ZERO, LINE);
    }
    if hovering_drop {
        p.rect_stroke(
            rect,
            CornerRadius::ZERO,
            egui::Stroke::new(1.0, APP),
            egui::StrokeKind::Inside,
        );
    }
    if active {
        p.rect_filled(
            Rect::from_min_size(rect.min, vec2(2.0, rect.height())),
            CornerRadius::ZERO,
            APP,
        );
    }
    // 2 px colour bar, never in discreet mode.
    if !discreet && let Some(c) = e.color {
        p.rect_filled(
            Rect::from_min_size(rect.min + vec2(4.0, 4.0), vec2(2.0, rect.height() - 8.0)),
            CornerRadius::ZERO,
            rgb(c),
        );
    }
    let indent = rect.left() + 16.0 + (e.depth + usize::from(e.has_kids)) as f32 * 12.0;
    let shown = if share_safe && !resp.hovered() && !discreet {
        masked_name(e.r.kind, e.ordinal)
    } else {
        e.shown.clone()
    };
    let color = if active { TEXT } else { MUTED };
    p.with_clip_rect(Rect::from_min_max(
        rect.min,
        egui::pos2(rect.right() - 44.0, rect.bottom()),
    ))
    .text(
        egui::pos2(indent, rect.center().y),
        Align2::LEFT_CENTER,
        &shown,
        FontId::new(12.0, FontFamily::Proportional),
        color,
    );
    if !discreet {
        p.text(
            egui::pos2(rect.right() - 12.0, rect.center().y),
            Align2::RIGHT_CENTER,
            e.count.to_string(),
            FontId::new(10.0, FontFamily::Monospace),
            FAINT,
        );
    }
    let mut chevron_hit = false;
    if e.has_kids {
        let c = egui::pos2(rect.left() + 12.0 + e.depth as f32 * 12.0, rect.center().y);
        let stroke = egui::Stroke::new(1.2, FAINT);
        let (a, b, d) = if open_kids {
            (
                c + vec2(-3.0, -1.5),
                c + vec2(0.0, 1.5),
                c + vec2(3.0, -1.5),
            )
        } else {
            (
                c + vec2(-1.5, -3.0),
                c + vec2(1.5, 0.0),
                c + vec2(-1.5, 3.0),
            )
        };
        ui.painter().line_segment([a, b], stroke);
        ui.painter().line_segment([b, d], stroke);
        let hit = Rect::from_center_size(c, vec2(18.0, 26.0));
        chevron_hit = resp.clicked()
            && ui
                .input(|i| i.pointer.interact_pos())
                .is_some_and(|pt| hit.contains(pt));
    }
    let click = if chevron_hit {
        Click::Chevron
    } else if resp.clicked() {
        let m = ui.input(|i| i.modifiers);
        Click::Open {
            ctrl: m.command,
            shift: m.shift,
        }
    } else {
        Click::None
    };
    EntryOut {
        resp: resp.on_hover_cursor(egui::CursorIcon::PointingHand),
        click,
        dropped,
    }
}

/// `state`: 0 not in it, 1 some, 2 all.
fn picker_row(
    ui: &mut Ui,
    cursor: bool,
    name: &str,
    kind: &str,
    state: u8,
    blur_until_hover: bool,
) -> egui::Response {
    let (rect, resp) =
        ui.allocate_exact_size(vec2(ui.available_width(), 26.0), egui::Sense::click());
    let p = ui.painter();
    if cursor || resp.hovered() {
        p.rect_filled(rect, CornerRadius::ZERO, LINE);
    }
    if cursor {
        p.rect_filled(
            Rect::from_min_size(rect.min, vec2(2.0, rect.height())),
            CornerRadius::ZERO,
            APP,
        );
    }
    let c = egui::pos2(rect.left() + 14.0, rect.center().y);
    let st = egui::Stroke::new(1.4, APP);
    match state {
        2 => {
            p.line_segment([c + vec2(-4.0, 0.0), c + vec2(-1.0, 3.0)], st);
            p.line_segment([c + vec2(-1.0, 3.0), c + vec2(4.5, -3.5)], st);
        }
        1 => {
            p.line_segment([c - vec2(4.0, 0.0), c + vec2(4.0, 0.0)], st);
        }
        _ => {}
    }
    let shown = if blur_until_hover && !resp.hovered() {
        "• • •"
    } else {
        name
    };
    p.text(
        egui::pos2(rect.left() + 30.0, rect.center().y),
        Align2::LEFT_CENTER,
        shown,
        FontId::new(12.0, FontFamily::Proportional),
        if cursor { TEXT } else { MUTED },
    );
    p.text(
        egui::pos2(rect.right() - 10.0, rect.center().y),
        Align2::RIGHT_CENTER,
        kind,
        FontId::new(9.5, FontFamily::Monospace),
        FAINT,
    );
    resp
}

// ---- commands ---------------------------------------------------------------

fn ids_arg(args: &Value) -> Option<Vec<i64>> {
    if let Some(a) = args.get("ids").and_then(Value::as_array) {
        return Some(a.iter().filter_map(Value::as_i64).collect());
    }
    args.get("id").and_then(Value::as_i64).map(|i| vec![i])
}

pub(super) fn register(reg: &mut Registry<JizzApp>) {
    reg.register(
        "org.picker",
        "Add the marked or open items to a playlist, tag or category: {ids?}",
        |app, args| {
            let ids = ids_arg(&args).unwrap_or_else(|| app.target_ids());
            app.open_picker(ids);
            Ok(json!({}))
        },
    );
    reg.register(
        "org.bulk",
        "Favorite, rate or hide the marked or open items in one transaction, undoable: {ids?, favorite?, rating?, hidden?}",
        |app, args| {
            let ids = ids_arg(&args).unwrap_or_else(|| app.target_ids());
            let patch = MediaPatch {
                favorite: args.get("favorite").and_then(Value::as_bool),
                rating: args
                    .get("rating")
                    .and_then(Value::as_u64)
                    .filter(|r| *r <= 5)
                    .map(|r| r as u8),
                hidden: args.get("hidden").and_then(Value::as_bool),
                inbox: None,
            };
            if patch.favorite.is_none() && patch.rating.is_none() && patch.hidden.is_none() {
                return Err(Error::Invalid(
                    "give `favorite`, `rating` (0..=5) or `hidden`".into(),
                ));
            }
            let n = ids.len();
            app.org_bulk(ids, patch);
            Ok(json!({ "items": n }))
        },
    );
    reg.register(
        "org.assign.repeat",
        "Repeat the last assignment on the marked or open items",
        |app, _| {
            app.org_repeat_last();
            Ok(json!({}))
        },
    );
    reg.register(
        "org.remove",
        "Remove the marked items from the current playlist",
        |app, _| {
            app.org_remove_current();
            Ok(json!({}))
        },
    );
    reg.register(
        "org.move",
        "Move the marked items in the current playlist: {delta: -1|1}",
        |app, args| {
            let d = args.get("delta").and_then(Value::as_i64).unwrap_or(0);
            if d != 0 {
                app.org_move(d);
            }
            Ok(json!({}))
        },
    );
    reg.register(
        "org.select.extend",
        "Extend the selection with the arrow: {dx, dy}",
        |app, args| {
            let dx = args.get("dx").and_then(Value::as_i64).unwrap_or(0);
            let dy = args.get("dy").and_then(Value::as_i64).unwrap_or(0);
            app.extend_selection(dx, dy);
            Ok(json!({}))
        },
    );
    reg.register(
        "org.select.all",
        "Select every item of the list",
        |app, _| {
            if !app.ctx.egui_wants_keyboard_input() {
                app.select_all();
            }
            Ok(json!({ "marked": app.org.marked.len() }))
        },
    );
    reg.register("org.select.clear", "Clear the marked items", |app, _| {
        app.org_escape();
        Ok(json!({}))
    });
    reg.register(
        "org.play",
        "Play the current list from the top",
        |app, _| {
            app.play_from_top();
            Ok(json!({}))
        },
    );
    reg.register(
        "org.undo",
        "Undo the last playlist, tag or category change",
        |app, _| {
            app.org_undo();
            Ok(json!({}))
        },
    );
    reg.register(
        "view.loop_list",
        "Wrap around at the end of the list (this session only)",
        |app, _| {
            app.org.loop_list = !app.org.loop_list;
            Ok(json!({ "loop": app.org.loop_list }))
        },
    );
}

// ---- tests ------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn r(kind: OrgKind, id: i64) -> OrgRef {
        OrgRef { kind, id }
    }

    /// A library of `n` tiny images, ids 1..=n in name order.
    fn library(dir: &Path, n: u32) -> Db {
        for i in 0..n {
            image::RgbImage::from_fn(8, 8, |x, y| image::Rgb([(x * 9) as u8, y as u8, i as u8]))
                .save(dir.join(format!("m{i:03}.png")))
                .unwrap();
        }
        let mut db = Db::open_in_memory().unwrap();
        jizzcraft_core::scan::import_folder(&mut db, dir).unwrap();
        db
    }

    fn all_ids(db: &Db) -> Vec<i64> {
        let f = Filter {
            sort: SortKey::Name,
            include_missing: true,
            hidden: HiddenMode::Include,
            ..Default::default()
        };
        db.query(&f).unwrap().into_iter().map(|m| m.id).collect()
    }

    // -- selection ---------------------------------------------------------

    #[test]
    fn range_window_is_ordered_inclusive_and_capped() {
        assert_eq!(range_window(3, 7), (3, 5));
        assert_eq!(range_window(7, 3), (3, 5));
        assert_eq!(range_window(4, 4), (4, 1));
        assert_eq!(range_window(0, 1_000_000).1, RANGE_CAP);
    }

    #[test]
    fn clicks_select_toggle_and_range() {
        let mut marks = BTreeSet::new();
        let mut anchor = None;
        // Plain click: only the anchor moves.
        click_marks(
            &mut marks,
            &mut anchor,
            None,
            (10, 0),
            (false, false),
            |_, _| vec![],
        );
        assert!(marks.is_empty());
        assert_eq!(anchor, Some(0));
        // Ctrl+click marks the previous selection too, then toggles.
        click_marks(
            &mut marks,
            &mut anchor,
            Some((10, 0)),
            (12, 2),
            (true, false),
            |_, _| vec![],
        );
        assert_eq!(marks.iter().copied().collect::<Vec<_>>(), vec![10, 12]);
        click_marks(
            &mut marks,
            &mut anchor,
            Some((12, 2)),
            (12, 2),
            (true, false),
            |_, _| vec![],
        );
        assert_eq!(marks.iter().copied().collect::<Vec<_>>(), vec![10]);
        // Shift+click replaces the marks with the range from the anchor.
        click_marks(
            &mut marks,
            &mut anchor,
            Some((12, 2)),
            (14, 4),
            (false, true),
            |a, b| {
                assert_eq!((a, b), (2, 4));
                vec![12, 13, 14]
            },
        );
        assert_eq!(marks.iter().copied().collect::<Vec<_>>(), vec![12, 13, 14]);
        // Plain click clears.
        click_marks(
            &mut marks,
            &mut anchor,
            None,
            (20, 9),
            (false, false),
            |_, _| vec![],
        );
        assert!(marks.is_empty());
        assert_eq!(anchor, Some(9));
    }

    #[test]
    fn ctrl_shift_adds_a_range_to_existing_marks() {
        let mut marks: BTreeSet<i64> = [1].into();
        let mut anchor = Some(5);
        click_marks(
            &mut marks,
            &mut anchor,
            None,
            (8, 8),
            (true, true),
            |_, _| vec![6, 7, 8],
        );
        assert_eq!(marks.iter().copied().collect::<Vec<_>>(), vec![1, 6, 7, 8]);
    }

    // -- privacy -----------------------------------------------------------

    #[test]
    fn discreet_masks_names_with_the_saved_view_scheme() {
        assert_eq!(mask_label_en(OrgKind::Playlist, 0), "Playlist 01");
        assert_eq!(mask_label_en(OrgKind::Category, 11), "Category 12");
        assert_eq!(mask_label_en(OrgKind::Tag, 2), "Tag 03");
        assert_eq!(shown_name(OrgKind::Tag, 2, "secret", true), "Tag 03");
        assert_eq!(shown_name(OrgKind::Tag, 2, "secret", false), "secret");
        assert!(!shown_name(OrgKind::Playlist, 0, "secret", true).contains("secret"));
    }

    #[test]
    fn discreet_hides_private_entries_and_their_children() {
        let list = [
            (0, false),
            (0, true),
            (1, false),
            (2, false),
            (0, false),
            (1, true),
        ];
        assert_eq!(
            visible_flags(&list, true),
            vec![true, false, false, false, true, false]
        );
        assert_eq!(visible_flags(&list, false), vec![true; 6]);
    }

    // -- picker ------------------------------------------------------------

    fn cand(kind: OrgKind, id: i64, name: &str) -> Cand {
        Cand {
            kind,
            id,
            name: name.into(),
        }
    }

    #[test]
    fn picker_ranks_exact_then_prefix_then_word_then_substring() {
        let c = [
            cand(OrgKind::Tag, 1, "night shift"),
            cand(OrgKind::Playlist, 2, "A night"),
            cand(OrgKind::Category, 3, "nightly"),
            cand(OrgKind::Playlist, 4, "night"),
            cand(OrgKind::Tag, 5, "midnight"),
            cand(OrgKind::Tag, 6, "unrelated"),
        ];
        let order: Vec<i64> = rank("night", &c).into_iter().map(|i| c[i].id).collect();
        // exact, prefix (category before tag), word prefix, substring.
        assert_eq!(order, vec![4, 3, 1, 2, 5]);
        assert!(has_exact("NIGHT", &c));
        assert!(!has_exact("nigh", &c));
    }

    #[test]
    fn picker_empty_query_keeps_order_and_letters_match_in_order() {
        let c = [
            cand(OrgKind::Tag, 1, "zeta"),
            cand(OrgKind::Playlist, 2, "alpha"),
        ];
        assert_eq!(rank("", &c), vec![0, 1]);
        assert_eq!(rank("zt", &c), vec![0]);
        assert!(rank("q", &c).is_empty());
    }

    // -- drop targets ------------------------------------------------------

    #[test]
    fn dropping_adds_to_playlists_and_assigns_labels() {
        let pl = r(OrgKind::Playlist, 3);
        let tag = r(OrgKind::Tag, 4);
        assert_eq!(
            drop_action(pl, &[1, 2]),
            Some(OrgAction::Add {
                to: pl,
                ids: vec![1, 2]
            })
        );
        assert_eq!(
            drop_action(tag, &[9]),
            Some(OrgAction::Add {
                to: tag,
                ids: vec![9]
            })
        );
        assert_eq!(drop_action(tag, &[]), None);
    }

    // -- playlist order and playback ---------------------------------------

    #[test]
    fn move_plans_shift_runs_by_one_and_stop_at_the_edges() {
        let order = [1, 2, 3, 4, 5, 6];
        let m: HashSet<i64> = [3, 4].into();
        assert_eq!(plan_move(&order, &m, true), vec![(vec![3, 4], Some(2))]);
        assert_eq!(plan_move(&order, &m, false), vec![(vec![3, 4], Some(6))]);
        let top: HashSet<i64> = [1].into();
        assert!(plan_move(&order, &top, true).is_empty());
        let end: HashSet<i64> = [6].into();
        assert!(plan_move(&order, &end, false).is_empty());
        assert_eq!(plan_move(&order, &[5].into(), false), vec![(vec![5], None)]);
        // Two runs: each one passes its neighbour.
        let two: HashSet<i64> = [2, 5].into();
        assert_eq!(
            plan_move(&order, &two, true),
            vec![(vec![2], Some(1)), (vec![5], Some(4))]
        );
    }

    #[test]
    fn autoplay_skips_missing_items_and_wraps_only_when_asked() {
        let missing = |i: u64| i == 2 || i == 3;
        assert_eq!(next_index(1, 1, 6, false, missing), Some(4));
        assert_eq!(next_index(5, 1, 6, false, missing), None);
        assert_eq!(next_index(5, 1, 6, true, missing), Some(0));
        assert_eq!(next_index(0, -1, 6, true, missing), Some(5));
        assert_eq!(next_index(0, 1, 1, true, |_| false), None);
        // Everything else is missing: nothing to play, no endless loop.
        assert_eq!(next_index(0, 1, 4, true, |i| i != 0), None);
    }

    #[test]
    fn label_filter_clicks_combine() {
        assert_eq!(
            combine_labels(vec![], vec![], 5, false, false),
            (vec![5], vec![])
        );
        assert_eq!(
            combine_labels(vec![5], vec![], 6, true, false),
            (vec![5, 6], vec![])
        );
        assert_eq!(
            combine_labels(vec![5, 6], vec![], 5, true, false),
            (vec![6], vec![])
        );
        assert_eq!(
            combine_labels(vec![5], vec![], 7, false, true),
            (vec![5], vec![7])
        );
        assert_eq!(
            combine_labels(vec![5], vec![7], 9, false, false),
            (vec![9], vec![])
        );
    }

    // -- undo: every operation puts the library back ------------------------

    #[test]
    fn undo_of_playlist_add_remove_and_move() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = library(dir.path(), 6);
        let ids = all_ids(&db);
        let pl = db.playlist_create("x", None).unwrap();
        db.playlist_add(pl, &ids[..4], None).unwrap();
        let start = db.playlist_item_ids(pl).unwrap();

        // add (one is already in, one is new)
        let (added, skipped, undo) = do_playlist_add(&mut db, pl, &[ids[3], ids[4]]).unwrap();
        assert_eq!((added, skipped), (1, 1));
        assert_eq!(db.playlist_len(pl).unwrap(), 5);
        undo.unwrap().apply(&mut db).unwrap();
        assert_eq!(db.playlist_item_ids(pl).unwrap(), start);

        // remove
        let (n, undo) = do_playlist_remove(&mut db, pl, &[ids[1], ids[2]]).unwrap();
        assert_eq!(n, 2);
        assert_eq!(db.playlist_len(pl).unwrap(), 2);
        undo.unwrap().apply(&mut db).unwrap();
        assert_eq!(db.playlist_item_ids(pl).unwrap(), start);

        // move down then back
        let (n, undo) = do_playlist_move(&mut db, pl, &[ids[0]], false).unwrap();
        assert_eq!(n, 1);
        assert_eq!(
            db.playlist_item_ids(pl).unwrap(),
            vec![ids[1], ids[0], ids[2], ids[3]]
        );
        undo.unwrap().apply(&mut db).unwrap();
        assert_eq!(db.playlist_item_ids(pl).unwrap(), start);
    }

    #[test]
    fn band_selects_intersecting_tiles_and_ctrl_keeps_base() {
        let t = |x: f32, y: f32| Rect::from_min_size(egui::pos2(x, y), vec2(10.0, 10.0));
        let tiles = [(1, t(0.0, 0.0)), (2, t(10.0, 0.0)), (3, t(0.0, 10.0))];
        let band = Rect::from_two_pos(egui::pos2(5.0, 5.0), egui::pos2(12.0, 8.0));
        assert_eq!(band_hits(band, &tiles), vec![1, 2]);
        // dragged the other way gives the same band
        let band = Rect::from_two_pos(egui::pos2(12.0, 8.0), egui::pos2(5.0, 5.0));
        assert_eq!(band_hits(band, &tiles), vec![1, 2]);
        assert!(
            band_hits(
                Rect::from_min_size(egui::pos2(50.0, 50.0), vec2(1.0, 1.0)),
                &tiles
            )
            .is_empty()
        );
        let base: BTreeSet<i64> = [9].into();
        assert_eq!(band_marks(&base, &[1, 2]), [1, 2, 9].into());
        assert_eq!(band_marks(&BTreeSet::new(), &[3]), [3].into());
    }

    #[test]
    fn bulk_change_is_one_undo_entry() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = library(dir.path(), 4);
        let ids = all_ids(&db);
        db.set_rating(ids[1], 3).unwrap();
        let patch = MediaPatch {
            rating: Some(5),
            favorite: Some(true),
            hidden: Some(true),
            ..Default::default()
        };
        let (n, undo) = do_bulk(&mut db, &ids[..3], &patch).unwrap();
        assert_eq!(n, 3);
        for id in &ids[..3] {
            let m = db.get(*id).unwrap().unwrap();
            assert!(m.favorite && m.hidden && m.rating == 5);
        }
        assert!(!db.get(ids[3]).unwrap().unwrap().favorite);
        undo.unwrap().apply(&mut db).unwrap();
        let m = db.get(ids[1]).unwrap().unwrap();
        assert!(!m.favorite && !m.hidden && m.rating == 3);
        // nothing matched: no undo entry
        let (n, undo) = do_bulk(&mut db, &[999_999], &patch).unwrap();
        assert_eq!(n, 0);
        assert!(undo.is_none());
    }

    #[test]
    fn undo_of_assign_and_toggle_for_tags_and_categories() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = library(dir.path(), 4);
        let ids = all_ids(&db);
        for kind in [LabelKind::Tag, LabelKind::Category] {
            let l = db.label_create(kind, "l", None, None).unwrap();
            db.label_assign(kind, &[l], &ids[..1], true).unwrap();
            // toggle on three where one already has it: two change
            let (on, n, skipped, undo) = do_assign(&mut db, kind, l, &ids[..3], None).unwrap();
            assert!(on);
            assert_eq!((n, skipped), (2, 1));
            undo.unwrap().apply(&mut db).unwrap();
            let have: Vec<i64> = ids
                .iter()
                .copied()
                .filter(|i| has_label(&db, *i, l))
                .collect();
            assert_eq!(have, vec![ids[0]], "{kind:?}");
            // all have it: toggle removes, undo restores
            db.label_assign(kind, &[l], &ids[..3], true).unwrap();
            let (on, n, _, undo) = do_assign(&mut db, kind, l, &ids[..3], None).unwrap();
            assert!(!on);
            assert_eq!(n, 3);
            undo.unwrap().apply(&mut db).unwrap();
            assert!(ids[..3].iter().all(|i| has_label(&db, *i, l)));
        }
    }

    #[test]
    fn undo_of_deleting_a_playlist_restores_name_colour_private_and_order() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = library(dir.path(), 5);
        let ids = all_ids(&db);
        let pl = db.playlist_create("Keep me", Some(0x3E63DD)).unwrap();
        db.playlist_set(pl, None, Some(true), None).unwrap();
        let order = vec![ids[3], ids[0], ids[4]];
        db.playlist_add(pl, &order, None).unwrap();

        let (n, undo) = do_delete_playlist(&mut db, pl).unwrap();
        assert_eq!(n, 3);
        assert!(db.playlists().unwrap().is_empty());
        // The library itself is untouched.
        assert_eq!(all_ids(&db).len(), 5);

        let re = undo.unwrap().apply(&mut db).unwrap();
        let Recreated::Playlist { old, new } = re else {
            panic!("expected a playlist");
        };
        assert_eq!(old, pl);
        let p = &db.playlists().unwrap()[0];
        assert_eq!(p.id, new);
        assert_eq!(
            (p.name.as_str(), p.color, p.private),
            ("Keep me", Some(0x3E63DD), true)
        );
        assert_eq!(db.playlist_item_ids(new).unwrap(), order);
    }

    #[test]
    fn undo_of_deleting_a_tag_and_a_leaf_category() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = library(dir.path(), 4);
        let ids = all_ids(&db);
        for kind in [LabelKind::Tag, LabelKind::Category] {
            let l = db.label_create(kind, "gone", None, Some(0xE5484D)).unwrap();
            db.label_assign(kind, &[l], &ids[..3], true).unwrap();
            let (n, undo) = do_delete_label(&mut db, kind, l).unwrap();
            assert_eq!(n, 3);
            assert!(db.labels(kind).unwrap().is_empty());
            let Recreated::Label { new, .. } = undo.unwrap().apply(&mut db).unwrap() else {
                panic!("expected a label");
            };
            let info = &db.labels(kind).unwrap()[0];
            assert_eq!(
                (info.name.as_str(), info.color, info.count),
                ("gone", Some(0xE5484D), 3)
            );
            assert!(ids[..3].iter().all(|i| has_label(&db, *i, new)));
            db.label_delete(kind, new).unwrap();
        }
    }

    #[test]
    fn later_undo_entries_follow_a_recreated_playlist() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = library(dir.path(), 3);
        let ids = all_ids(&db);
        let pl = db.playlist_create("p", None).unwrap();
        let mut stack = UndoStack::default();
        let (_, _, add) = do_playlist_add(&mut db, pl, &ids[..2]).unwrap();
        stack.push(add.unwrap());
        let (_, del) = do_delete_playlist(&mut db, pl).unwrap();
        stack.push(del.unwrap());
        // Undo the delete: the id changes, the older add must follow.
        let re = stack.pop().unwrap().apply(&mut db).unwrap();
        stack.follow(&re);
        stack.pop().unwrap().apply(&mut db).unwrap();
        let Recreated::Playlist { new, .. } = re else {
            panic!()
        };
        assert_eq!(db.playlist_len(new).unwrap(), 0);
    }

    #[test]
    fn the_undo_stack_keeps_only_the_last_thirty() {
        let mut s = UndoStack::default();
        for i in 0..(UNDO_DEPTH as i64 + 10) {
            s.push(UndoOp::PlaylistRemove { id: i, ids: vec![] });
        }
        assert_eq!(s.len(), UNDO_DEPTH);
        // Oldest dropped, newest first out.
        assert_eq!(
            s.pop(),
            Some(UndoOp::PlaylistRemove {
                id: UNDO_DEPTH as i64 + 9,
                ids: vec![]
            })
        );
        s.clear();
        assert!(s.is_empty());
    }

    #[test]
    fn palette_has_eight_distinct_swatches() {
        let set: HashSet<u32> = PALETTE.into_iter().collect();
        assert_eq!(set.len(), 8);
        assert!(PALETTE.iter().all(|c| *c <= 0xFF_FFFF));
    }
}
