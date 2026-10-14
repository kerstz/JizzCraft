//! Full-text search over file names, tag and category names and marker notes.
//!
//! Backed by an SQLite FTS5 table (`media_fts`, schema v4) that triggers keep
//! current, so scans, labelling, marker edits and folder removal need no
//! extra call. The bundled SQLite always ships FTS5; there is no LIKE
//! fallback for the index itself (the plain name filter keeps a substring
//! match next to it, see `Db::where_clause`).
//!
//! Matching is case and accent insensitive (`unicode61 remove_diacritics 2`),
//! every word is a prefix, and words are ANDed. File names split on
//! punctuation, so `holiday_clip.mp4` is found by `clip` or `mp4`.
//! Private tags and categories are not indexed.

use crate::db::{Db, SortKey, row_to_media_cols};
use crate::error::Result;
use crate::model::Media;
use rusqlite::types::Value;

/// Words kept from a query; more would only slow the match down.
const MAX_WORDS: usize = 16;

/// Turn free text into a safe FTS5 expression (`"a"* "b"*`), or `None` when
/// it holds no word. User text never reaches FTS5 syntax: every word is
/// quoted, so `AND`, `NEAR`, `-` and `:` are plain text.
pub fn fts_query(text: &str) -> Option<String> {
    let words: Vec<String> = text
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .take(MAX_WORDS)
        .map(|w| format!("\"{w}\"*"))
        .collect();
    if words.is_empty() {
        None
    } else {
        Some(words.join(" "))
    }
}

impl Db {
    /// Best matches first (names weigh most, then tags, categories, notes).
    /// Hidden and missing items are left out, like the normal library view.
    pub fn search(&self, text: &str, limit: u64, offset: u64) -> Result<Vec<Media>> {
        let Some(q) = fts_query(text) else {
            return Ok(Vec::new());
        };
        let cols = row_to_media_cols("media.");
        let sql = format!(
            "SELECT {cols} FROM media_fts JOIN media ON media.id = media_fts.rowid \
             WHERE media_fts MATCH ?1 AND media.hidden = 0 AND media.missing = 0 \
             ORDER BY bm25(media_fts, 10.0, 5.0, 4.0, 2.0), media.{col}, media.id \
             LIMIT ?2 OFFSET ?3",
            col = SortKey::Name.column_name()
        );
        let mut st = self.conn.prepare_cached(&sql)?;
        let rows = st.query_map(
            rusqlite::params_from_iter([
                Value::Text(q),
                Value::Integer(limit.min(i64::MAX as u64) as i64),
                Value::Integer(offset.min(i64::MAX as u64) as i64),
            ]),
            crate::db::row_to_media,
        )?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }
}

#[cfg(test)]
mod tests {
    use super::fts_query;

    #[test]
    fn query_is_quoted_and_prefixed() {
        assert_eq!(fts_query("Café  sun").unwrap(), "\"Café\"* \"sun\"*");
        assert_eq!(fts_query("a_b.c").unwrap(), "\"a\"* \"b\"* \"c\"*");
    }

    #[test]
    fn syntax_characters_are_inert() {
        assert_eq!(
            fts_query("\"x\" OR -y:z*").unwrap(),
            "\"x\"* \"OR\"* \"y\"* \"z\"*"
        );
        assert!(fts_query("  \"*- ").is_none());
        assert!(fts_query("").is_none());
    }
}
