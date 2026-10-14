//! Per-item user data that is not metadata about the file: rating, hidden/inbox
//! flags, resume point and play history, plus the small settings table.
//!
//! History is deliberately aggregate (last played, count, resume point), never
//! an event log. Clearing it is one UPDATE followed by a WAL truncate.
//! The app remembers *that* you watched, never *when exactly*. Like a good
//! friend.

use crate::db::{Db, now_secs};
use crate::error::{Error, Result};
use crate::model::Media;
use rusqlite::params;
use serde::Serialize;

/// Settings keys the app may write. Anything else is rejected.
pub const SETTINGS_KEYS: &[&str] = &["history.enabled", "resume.enabled"];

#[derive(Debug, Clone, Default)]
pub struct MediaPatch {
    pub favorite: Option<bool>,
    pub rating: Option<u8>,
    pub hidden: Option<bool>,
    pub inbox: Option<bool>,
}

/// Per-item user data as it was before a bulk change, so it can be put back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UserState {
    pub id: i64,
    pub favorite: bool,
    pub rating: u8,
    pub hidden: bool,
    pub inbox: bool,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
pub struct Progress {
    pub resume_ms: Option<u64>,
    pub counted: bool,
    pub recorded: bool,
}

/// Where playback should resume, or `None` when it is not worth offering.
/// Under 60 s total, before 10 s in, or within the last max(10 s, 5 %) = no.
pub fn resume_point(pos_ms: u64, duration_ms: Option<u64>) -> Option<u64> {
    let dur = duration_ms?;
    if dur < 60_000 || pos_ms < 10_000 {
        return None;
    }
    let tail = (dur / 20).max(10_000);
    if pos_ms > dur.saturating_sub(tail) {
        return None;
    }
    Some(pos_ms)
}

impl Db {
    pub fn setting(&self, key: &str) -> Result<Option<String>> {
        use rusqlite::OptionalExtension;
        Ok(self
            .conn
            .query_row("SELECT value FROM settings WHERE key = ?1", [key], |r| {
                r.get(0)
            })
            .optional()?)
    }

    pub fn set_setting(&self, key: &str, value: &str) -> Result<()> {
        if !SETTINGS_KEYS.contains(&key) {
            return Err(Error::Invalid(format!("unknown setting `{key}`")));
        }
        self.conn.execute(
            "INSERT INTO settings(key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    fn flag(&self, key: &str) -> bool {
        self.setting(key).ok().flatten().as_deref() != Some("0")
    }

    pub fn history_enabled(&self) -> bool {
        self.flag("history.enabled")
    }

    pub fn resume_enabled(&self) -> bool {
        self.flag("resume.enabled")
    }

    pub fn set_rating(&self, id: i64, rating: u8) -> Result<()> {
        if rating > 5 {
            return Err(Error::Invalid("rating must be 0..=5".into()));
        }
        self.exec_one(
            "UPDATE media SET rating = ?2 WHERE id = ?1",
            params![id, rating],
            id,
        )
    }

    /// Hiding also takes the item out of the inbox. The file is never touched.
    pub fn set_hidden(&self, id: i64, hidden: bool) -> Result<()> {
        self.exec_one(
            "UPDATE media SET hidden = ?2, inbox = CASE WHEN ?2 = 1 THEN 0 ELSE inbox END
             WHERE id = ?1",
            params![id, hidden as i64],
            id,
        )
    }

    pub fn toggle_hidden(&self, id: i64) -> Result<bool> {
        let m = self
            .get(id)?
            .ok_or_else(|| Error::NotFound(format!("media id {id}")))?;
        self.set_hidden(id, !m.hidden)?;
        Ok(!m.hidden)
    }

    /// Apply `favorite` / `rating` / `hidden` to many items in one
    /// transaction (all or nothing). Unknown ids are skipped. Returns the
    /// previous user state of every item that exists, in the order given,
    /// for undo. Hiding also takes items out of the inbox, like
    /// [`Db::set_hidden`].
    pub fn bulk_patch(&mut self, ids: &[i64], p: &MediaPatch) -> Result<Vec<UserState>> {
        if p.rating.is_some_and(|r| r > 5) {
            return Err(Error::Invalid("rating must be 0..=5".into()));
        }
        let tx = self.conn.transaction()?;
        let mut before = Vec::with_capacity(ids.len());
        {
            let mut get =
                tx.prepare("SELECT favorite, rating, hidden, inbox FROM media WHERE id = ?1")?;
            let mut set = tx.prepare(
                "UPDATE media SET
                   favorite = COALESCE(?2, favorite),
                   rating = COALESCE(?3, rating),
                   hidden = COALESCE(?4, hidden),
                   inbox = CASE WHEN ?4 = 1 THEN 0 ELSE inbox END
                 WHERE id = ?1",
            )?;
            let mut seen = std::collections::HashSet::new();
            for &id in ids {
                if !seen.insert(id) {
                    continue;
                }
                let cur = get.query_row([id], |r| {
                    Ok(UserState {
                        id,
                        favorite: r.get::<_, i64>(0)? != 0,
                        rating: r.get::<_, i64>(1)? as u8,
                        hidden: r.get::<_, i64>(2)? != 0,
                        inbox: r.get::<_, i64>(3)? != 0,
                    })
                });
                let cur = match cur {
                    Ok(c) => c,
                    Err(rusqlite::Error::QueryReturnedNoRows) => continue,
                    Err(e) => return Err(e.into()),
                };
                set.execute(params![
                    id,
                    p.favorite.map(i64::from),
                    p.rating.map(i64::from),
                    p.hidden.map(i64::from)
                ])?;
                before.push(cur);
            }
        }
        tx.commit()?;
        Ok(before)
    }

    /// Put back states returned by [`Db::bulk_patch`], in one transaction.
    pub fn bulk_restore(&mut self, rows: &[UserState]) -> Result<()> {
        let tx = self.conn.transaction()?;
        {
            let mut set = tx.prepare(
                "UPDATE media SET favorite = ?2, rating = ?3, hidden = ?4, inbox = ?5
                 WHERE id = ?1",
            )?;
            for r in rows {
                set.execute(params![
                    r.id,
                    i64::from(r.favorite),
                    i64::from(r.rating),
                    i64::from(r.hidden),
                    i64::from(r.inbox)
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn patch(&self, id: i64, p: &MediaPatch) -> Result<Media> {
        if let Some(r) = p.rating {
            self.set_rating(id, r)?;
        }
        if let Some(f) = p.favorite {
            self.set_favorite(id, f)?;
        }
        if let Some(h) = p.hidden {
            self.set_hidden(id, h)?;
        }
        if let Some(i) = p.inbox {
            self.exec_one(
                "UPDATE media SET inbox = ?2 WHERE id = ?1",
                params![id, i as i64],
                id,
            )?;
        }
        self.get(id)?
            .ok_or_else(|| Error::NotFound(format!("media id {id}")))
    }

    /// Leave the inbox for one item, or for all of them.
    pub fn clear_inbox(&self, id: Option<i64>) -> Result<u64> {
        let n = match id {
            Some(id) => self.conn.execute(
                "UPDATE media SET inbox = 0 WHERE id = ?1 AND inbox = 1",
                [id],
            )?,
            None => self
                .conn
                .execute("UPDATE media SET inbox = 0 WHERE inbox = 1", [])?,
        };
        Ok(n as u64)
    }

    /// Record playback progress. Honours the history and resume settings.
    pub fn record_progress(
        &self,
        id: i64,
        pos_ms: u64,
        duration_ms: Option<u64>,
        count_play: bool,
    ) -> Result<Progress> {
        let history = self.history_enabled();
        let resume = if self.resume_enabled() {
            resume_point(pos_ms, duration_ms)
        } else {
            None
        };
        if !history && !self.resume_enabled() {
            return Ok(Progress {
                resume_ms: None,
                counted: false,
                recorded: false,
            });
        }
        let counted = history && count_play;
        self.exec_one(
            "UPDATE media SET resume_ms = ?2,
                    play_count = play_count + ?3,
                    last_played_at = CASE WHEN ?4 = 1 THEN ?5 ELSE last_played_at END
             WHERE id = ?1",
            params![
                id,
                resume.map(|v| v as i64),
                counted as i64,
                history as i64,
                now_secs()
            ],
            id,
        )?;
        Ok(Progress {
            resume_ms: resume,
            counted,
            recorded: true,
        })
    }

    /// Forget play history (and resume points) for one item or the whole library.
    pub fn clear_history(&mut self, id: Option<i64>) -> Result<u64> {
        let n = match id {
            Some(id) => self.conn.execute(
                "UPDATE media SET resume_ms = NULL, play_count = 0, last_played_at = NULL
                 WHERE id = ?1",
                [id],
            )?,
            None => self.conn.execute(
                "UPDATE media SET resume_ms = NULL, play_count = 0, last_played_at = NULL
                 WHERE resume_ms IS NOT NULL OR play_count > 0 OR last_played_at IS NOT NULL",
                [],
            )?,
        };
        // Best effort: keep cleared history out of the WAL file too.
        let _ = self
            .conn
            .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()));
        Ok(n as u64)
    }

    /// Most recently played first.
    pub fn history(&self, limit: u64) -> Result<Vec<Media>> {
        self.query(&crate::db::Filter {
            played: Some(true),
            sort: crate::db::SortKey::LastPlayed,
            desc: true,
            limit: Some(limit.clamp(1, 500)),
            ..Default::default()
        })
    }

    fn exec_one(&self, sql: &str, p: impl rusqlite::Params, id: i64) -> Result<()> {
        let n = self.conn.execute(sql, p)?;
        if n == 0 {
            return Err(Error::NotFound(format!("media id {id}")));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::resume_point;

    #[test]
    fn resume_rules() {
        let min = 60_000;
        assert_eq!(resume_point(30_000, None), None);
        assert_eq!(resume_point(30_000, Some(min - 1)), None, "too short");
        assert_eq!(resume_point(9_999, Some(10 * min)), None, "too early");
        assert_eq!(resume_point(10_000, Some(10 * min)), Some(10_000));
        assert_eq!(resume_point(300_000, Some(10 * min)), Some(300_000));
        // Last max(10 s, 5 %) is "finished": 5 % of 10 min = 30 s.
        assert_eq!(
            resume_point(10 * min - 30_001, Some(10 * min)),
            Some(10 * min - 30_001)
        );
        assert_eq!(resume_point(10 * min - 29_999, Some(10 * min)), None);
        // Short file: the 10 s floor wins.
        assert_eq!(resume_point(min - 10_001, Some(min)), Some(min - 10_001));
        assert_eq!(resume_point(min - 9_999, Some(min)), None);
    }
}
