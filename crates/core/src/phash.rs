//! Perceptual hash of a video and duplicate groups. Pure: frames come in as
//! images (the media crate decodes them, in memory only), hashes go out as
//! `u64`. Nothing here reads or writes a media file.
//!
//! The layout follows what Stash does: 25 frames of 160 px width pasted in a
//! 5 x 5 sprite, then a DCT perceptual hash of the sprite (64 x 64 bilinear
//! resize, grayscale, DCT-II, the top-left 8 x 8 block against its median).
//! The bits are not guaranteed to equal another program's (different decoders
//! give slightly different pixels), so hashes are only ever compared by
//! Hamming distance.

use crate::commands::{HasLibrary, Registry};
use crate::db::{Db, blob_to_path};
use crate::error::{Error, Result};
use image::{RgbImage, imageops};
use rusqlite::params;
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::PathBuf;

/// Version of the hashing recipe. Rows hashed with an older one are redone.
pub const PHASH_ALGO: u8 = 1;
/// Frames per video.
pub const FRAMES: usize = 25;
/// Width of one frame in the sprite.
pub const FRAME_W: u32 = 160;
/// Largest group a review shows.
pub const MAX_GROUP: usize = 50;
/// Largest Hamming distance a caller may ask for.
pub const MAX_DISTANCE: u32 = 16;

/// Sprite of exactly [`FRAMES`] frames (5 x 5, row-major). Each frame is put
/// at [`FRAME_W`] pixels wide and the height of the first one.
pub fn sprite(frames: &[RgbImage]) -> Option<RgbImage> {
    if frames.len() != FRAMES || frames.iter().any(|f| f.width() == 0 || f.height() == 0) {
        return None;
    }
    let cell_h = (u64::from(frames[0].height()) * u64::from(FRAME_W) / u64::from(frames[0].width()))
        .max(1) as u32;
    let mut out = RgbImage::new(FRAME_W * 5, cell_h * 5);
    for (i, f) in frames.iter().enumerate() {
        let (cx, cy) = ((i % 5) as u32 * FRAME_W, (i / 5) as u32 * cell_h);
        if f.width() == FRAME_W && f.height() == cell_h {
            imageops::replace(&mut out, f, i64::from(cx), i64::from(cy));
        } else {
            let r = imageops::resize(f, FRAME_W, cell_h, imageops::FilterType::Triangle);
            imageops::replace(&mut out, &r, i64::from(cx), i64::from(cy));
        }
    }
    Some(out)
}

/// DCT perceptual hash: bit 63 is the first of the 8 x 8 low frequencies.
pub fn perception_hash(img: &RgbImage) -> u64 {
    const N: usize = 64;
    const K: usize = 8;
    let small = imageops::resize(img, N as u32, N as u32, imageops::FilterType::Triangle);
    let gray: Vec<f64> = small
        .pixels()
        .map(|p| 0.299 * f64::from(p[0]) + 0.587 * f64::from(p[1]) + 0.114 * f64::from(p[2]))
        .collect();
    // Unnormalised DCT-II: X[k] = sum x[n] cos(pi / N * (n + 1/2) * k).
    let mut cos = [[0f64; N]; K];
    for (k, row) in cos.iter_mut().enumerate() {
        for (n, c) in row.iter_mut().enumerate() {
            *c = (std::f64::consts::PI / N as f64 * (n as f64 + 0.5) * k as f64).cos();
        }
    }
    // Rows first (keep K coefficients), then columns.
    let mut rows = [[0f64; K]; N];
    for (y, out) in rows.iter_mut().enumerate() {
        for (k, o) in out.iter_mut().enumerate() {
            *o = (0..N).map(|x| gray[y * N + x] * cos[k][x]).sum();
        }
    }
    let mut coef = [0f64; K * K];
    for u in 0..K {
        for v in 0..K {
            coef[u * K + v] = (0..N).map(|y| rows[y][v] * cos[u][y]).sum();
        }
    }
    let mut sorted = coef;
    sorted.sort_by(f64::total_cmp);
    let median = (sorted[K * K / 2 - 1] + sorted[K * K / 2]) / 2.0;
    coef.iter()
        .enumerate()
        .filter(|(_, c)| **c > median)
        .fold(0u64, |h, (i, _)| h | (1u64 << (63 - i)))
}

pub fn hamming(a: u64, b: u64) -> u32 {
    (a ^ b).count_ones()
}

/// Metric tree over Hamming distance: finds every hash within `d` of a query
/// without comparing against all of them.
#[derive(Default)]
pub struct BkTree {
    nodes: Vec<Node>,
}

struct Node {
    hash: u64,
    id: i64,
    children: Vec<(u32, usize)>,
}

impl BkTree {
    pub fn new() -> BkTree {
        BkTree::default()
    }

    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    pub fn insert(&mut self, hash: u64, id: i64) {
        let new = self.nodes.len();
        let mut at = 0usize;
        if self.nodes.is_empty() {
            self.nodes.push(Node {
                hash,
                id,
                children: Vec::new(),
            });
            return;
        }
        loop {
            let d = hamming(self.nodes[at].hash, hash);
            match self.nodes[at].children.iter().find(|(cd, _)| *cd == d) {
                Some((_, child)) => at = *child,
                None => {
                    self.nodes[at].children.push((d, new));
                    self.nodes.push(Node {
                        hash,
                        id,
                        children: Vec::new(),
                    });
                    return;
                }
            }
        }
    }

    /// `(id, distance)` of every entry within `max` of `hash`, unordered.
    pub fn within(&self, hash: u64, max: u32) -> Vec<(i64, u32)> {
        let mut out = Vec::new();
        if self.nodes.is_empty() {
            return out;
        }
        let mut stack = vec![0usize];
        while let Some(i) = stack.pop() {
            let n = &self.nodes[i];
            let d = hamming(n.hash, hash);
            if d <= max {
                out.push((n.id, d));
            }
            let (lo, hi) = (d.saturating_sub(max), d + max);
            stack.extend(
                n.children
                    .iter()
                    .filter(|(cd, _)| (lo..=hi).contains(cd))
                    .map(|(_, c)| *c),
            );
        }
        out
    }
}

/// One item of a duplicate group.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct DupItem {
    pub id: i64,
    pub name: String,
    pub duration_ms: Option<u64>,
    pub size: u64,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct DupGroup {
    /// Largest distance between linked members.
    pub distance: u32,
    pub items: Vec<DupItem>,
}

#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct DupReport {
    pub groups: Vec<DupGroup>,
    /// Items that carry at least one hash (own or imported).
    pub hashed: u64,
    /// Hashes looked up in the tree.
    pub compared: u64,
}

fn find(parent: &mut [usize], mut x: usize) -> usize {
    while parent[x] != x {
        parent[x] = parent[parent[x]];
        x = parent[x];
    }
    x
}

/// Parse an imported `phash.stash` value (16 hex digits).
fn imported(v: &str) -> Option<u64> {
    crate::fingerprint::parse_oshash(v)
}

impl Db {
    /// Store a hash (`Some`) or note that hashing failed (`None`; not retried
    /// until the recipe version changes or the caller asks for everything).
    pub fn set_phash(&self, id: i64, hash: Option<u64>, algo: u8) -> Result<()> {
        self.conn.execute(
            "UPDATE media SET phash = ?2, phash_algo = ?3 WHERE id = ?1",
            params![id, hash.map(|h| h as i64), i64::from(algo)],
        )?;
        Ok(())
    }

    /// The hash stored for an item, if any.
    pub fn phash_of(&self, id: i64) -> Result<Option<u64>> {
        let v: Option<i64> = self
            .conn
            .query_row("SELECT phash FROM media WHERE id = ?1", [id], |r| r.get(0))
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => Error::NotFound(format!("media id {id}")),
                e => e.into(),
            })?;
        Ok(v.map(|h| h as u64))
    }

    fn phash_pending_where(all: bool) -> String {
        let retry = if all { " OR phash IS NULL" } else { "" };
        format!(
            "kind = 1 AND missing = 0 AND (phash_algo IS NULL OR phash_algo < {PHASH_ALGO}{retry})"
        )
    }

    /// Present videos still to hash: `(id, path)`.
    pub fn phash_pending(&self, all: bool, limit: u64) -> Result<Vec<(i64, PathBuf)>> {
        let mut st = self.conn.prepare(&format!(
            "SELECT id, path FROM media WHERE {} ORDER BY id LIMIT ?1",
            Self::phash_pending_where(all)
        ))?;
        let rows = st.query_map([limit.min(i64::MAX as u64) as i64], |r| {
            Ok((r.get::<_, i64>(0)?, blob_to_path(r.get(1)?)))
        })?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    pub fn phash_remaining(&self, all: bool) -> Result<u64> {
        Ok(self.conn.query_row(
            &format!(
                "SELECT COUNT(*) FROM media WHERE {}",
                Self::phash_pending_where(all)
            ),
            [],
            |r| r.get::<_, i64>(0),
        )? as u64)
    }

    /// Every hash an item carries: its own, then the imported ones.
    fn hashes_of(&self, id: i64) -> Result<Vec<u64>> {
        let mut out = Vec::new();
        if let Some(h) = self.phash_of(id)? {
            out.push(h);
        }
        let mut st = self.conn.prepare(
            "SELECT value FROM external_ids WHERE media_id = ?1 AND scheme = 'phash.stash'",
        )?;
        for v in st.query_map([id], |r| r.get::<_, String>(0))? {
            out.extend(imported(&v?));
        }
        Ok(out)
    }

    /// Smallest Hamming distance between any hash of `a` and any of `b`.
    pub fn phash_distance(&self, a: i64, b: i64) -> Result<Option<u32>> {
        let (ha, hb) = (self.hashes_of(a)?, self.hashes_of(b)?);
        Ok(ha
            .iter()
            .flat_map(|x| hb.iter().map(move |y| hamming(*x, *y)))
            .min())
    }

    /// Groups of present, visible videos whose hashes are within
    /// `max_distance` of each other (linked transitively). With
    /// `duration_diff_ms`, two items whose known durations differ by more are
    /// never linked. Groups are capped at [`MAX_GROUP`] items (lowest ids),
    /// sorted by size then first id, and at `max_groups`.
    pub fn duplicates(
        &self,
        max_distance: u32,
        duration_diff_ms: Option<u64>,
        max_groups: usize,
    ) -> Result<DupReport> {
        if max_distance > MAX_DISTANCE {
            return Err(Error::Invalid(format!(
                "distance must be 0 to {MAX_DISTANCE}"
            )));
        }
        let mut st = self.conn.prepare(
            "SELECT id, name, duration_ms, size, phash FROM media
             WHERE kind = 1 AND missing = 0 AND hidden = 0 ORDER BY id",
        )?;
        let mut items: Vec<(DupItem, Vec<u64>)> = Vec::new();
        for r in st.query_map([], |r| {
            Ok((
                DupItem {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    duration_ms: r.get::<_, Option<i64>>(2)?.map(|v| v.max(0) as u64),
                    size: r.get::<_, i64>(3)?.max(0) as u64,
                },
                r.get::<_, Option<i64>>(4)?,
            ))
        })? {
            let (item, own) = r?;
            items.push((item, own.map(|h| h as u64).into_iter().collect()));
        }
        let index: HashMap<i64, usize> = items
            .iter()
            .enumerate()
            .map(|(i, (it, _))| (it.id, i))
            .collect();
        let mut imp = self
            .conn
            .prepare("SELECT media_id, value FROM external_ids WHERE scheme = 'phash.stash'")?;
        for r in imp.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))? {
            let (id, v) = r?;
            if let (Some(&i), Some(h)) = (index.get(&id), imported(&v)) {
                items[i].1.push(h);
            }
        }

        let mut tree = BkTree::new();
        let mut report = DupReport::default();
        for (i, (_, hs)) in items.iter().enumerate() {
            if !hs.is_empty() {
                report.hashed += 1;
            }
            for h in hs {
                tree.insert(*h, i as i64);
            }
        }
        let mut parent: Vec<usize> = (0..items.len()).collect();
        let mut edge_max: Vec<u32> = vec![0; items.len()];
        let mut edges: Vec<(usize, usize, u32)> = Vec::new();
        for (i, (it, hs)) in items.iter().enumerate() {
            for h in hs {
                report.compared += 1;
                for (j, d) in tree.within(*h, max_distance) {
                    let j = j as usize;
                    if j <= i {
                        continue;
                    }
                    if let (Some(diff), Some(a), Some(b)) =
                        (duration_diff_ms, it.duration_ms, items[j].0.duration_ms)
                        && a.abs_diff(b) > diff
                    {
                        continue;
                    }
                    edges.push((i, j, d));
                }
            }
        }
        for &(i, j, _) in &edges {
            let (a, b) = (find(&mut parent, i), find(&mut parent, j));
            if a != b {
                parent[a.max(b)] = a.min(b);
            }
        }
        for &(i, _, d) in &edges {
            let r = find(&mut parent, i);
            edge_max[r] = edge_max[r].max(d);
        }
        let mut members: HashMap<usize, Vec<usize>> = HashMap::new();
        for i in 0..items.len() {
            let r = find(&mut parent, i);
            members.entry(r).or_default().push(i);
        }
        let mut groups: Vec<DupGroup> = members
            .into_iter()
            .filter(|(_, m)| m.len() > 1)
            .map(|(root, mut m)| {
                m.sort_unstable();
                m.truncate(MAX_GROUP);
                DupGroup {
                    distance: edge_max[root],
                    items: m.into_iter().map(|i| items[i].0.clone()).collect(),
                }
            })
            .collect();
        groups.sort_by(|a, b| {
            b.items
                .len()
                .cmp(&a.items.len())
                .then(a.items[0].id.cmp(&b.items[0].id))
        });
        groups.truncate(max_groups);
        report.groups = groups;
        Ok(report)
    }
}

pub fn register<C: HasLibrary>(reg: &mut Registry<C>) {
    reg.register(
        "media.duplicates",
        "Groups of videos that look alike (needs library.phash first): {distance?=8 (0..=16), duration_diff_ms?}",
        |c, a| {
            let distance = match a.get("distance") {
                None | Some(Value::Null) => 8,
                Some(v) => v
                    .as_u64()
                    .filter(|d| *d <= u64::from(MAX_DISTANCE))
                    .ok_or_else(|| Error::Invalid("`distance` must be 0 to 16".into()))?
                    as u32,
            };
            let diff = match a.get("duration_diff_ms") {
                None | Some(Value::Null) => None,
                Some(v) => Some(v.as_u64().ok_or_else(|| {
                    Error::Invalid("`duration_diff_ms` must be a non-negative integer".into())
                })?),
            };
            let rep = c.db().duplicates(distance, diff, 1000)?;
            Ok(json!(rep))
        },
    );
    reg.register(
        "media.phash.distance",
        "Smallest Hamming distance between the visual fingerprints of two items: {a, b}",
        |c, a| {
            let id = |k: &str| {
                a.get(k)
                    .and_then(Value::as_i64)
                    .ok_or_else(|| Error::Invalid(format!("missing integer argument `{k}`")))
            };
            let d = c.db().phash_distance(id("a")?, id("b")?)?;
            Ok(json!({ "distance": d }))
        },
    );
}
