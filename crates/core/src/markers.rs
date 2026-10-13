//! Labelled positions inside a video. Database only.

use crate::db::{Db, now_secs};
use crate::error::{Error, Result};
use rusqlite::{OptionalExtension, Row, params};
use serde::Serialize;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Marker {
    pub id: i64,
    pub media_id: i64,
    pub pos_ms: u64,
    pub label: String,
    pub created_at: i64,
}

fn clean_label(raw: &str) -> Result<String> {
    let s = raw.trim();
    if s.chars().any(char::is_control) {
        return Err(Error::Invalid("marker label has control characters".into()));
    }
    Ok(s.chars().take(200).collect())
}

fn row(r: &Row<'_>) -> rusqlite::Result<Marker> {
    Ok(Marker {
        id: r.get(0)?,
        media_id: r.get(1)?,
        pos_ms: r.get::<_, i64>(2)? as u64,
        label: r.get(3)?,
        created_at: r.get(4)?,
    })
}

const COLS: &str = "id, media_id, pos_ms, label, created_at";

impl Db {
    pub fn markers(&self, media_id: i64) -> Result<Vec<Marker>> {
        let mut st = self.conn.prepare_cached(&format!(
            "SELECT {COLS} FROM markers WHERE media_id = ?1 ORDER BY pos_ms"
        ))?;
        let rows = st.query_map([media_id], row)?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    /// Upsert on `(media_id, pos_ms)`: marking the same instant twice relabels it.
    pub fn add_marker(&self, media_id: i64, pos_ms: u64, label: &str) -> Result<Marker> {
        let label = clean_label(label)?;
        if self.get(media_id)?.is_none() {
            return Err(Error::NotFound(format!("media id {media_id}")));
        }
        self.conn.execute(
            "INSERT INTO markers(media_id, pos_ms, label, created_at) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(media_id, pos_ms) DO UPDATE SET label = excluded.label",
            params![media_id, pos_ms as i64, label, now_secs()],
        )?;
        Ok(self.conn.query_row(
            &format!("SELECT {COLS} FROM markers WHERE media_id = ?1 AND pos_ms = ?2"),
            params![media_id, pos_ms as i64],
            row,
        )?)
    }

    pub fn update_marker(
        &self,
        marker_id: i64,
        pos_ms: Option<u64>,
        label: Option<&str>,
    ) -> Result<Marker> {
        if let Some(l) = label {
            let l = clean_label(l)?;
            self.conn.execute(
                "UPDATE markers SET label = ?2 WHERE id = ?1",
                params![marker_id, l],
            )?;
        }
        if let Some(p) = pos_ms {
            self.conn.execute(
                "UPDATE markers SET pos_ms = ?2 WHERE id = ?1",
                params![marker_id, p as i64],
            )?;
        }
        self.conn
            .query_row(
                &format!("SELECT {COLS} FROM markers WHERE id = ?1"),
                [marker_id],
                row,
            )
            .optional()?
            .ok_or_else(|| Error::NotFound(format!("marker {marker_id}")))
    }

    pub fn remove_marker(&self, marker_id: i64) -> Result<bool> {
        Ok(self
            .conn
            .execute("DELETE FROM markers WHERE id = ?1", [marker_id])?
            > 0)
    }
}
