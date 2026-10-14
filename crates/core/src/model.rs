use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MediaKind {
    Image,
    Video,
}

impl MediaKind {
    pub fn as_i64(self) -> i64 {
        match self {
            MediaKind::Image => 0,
            MediaKind::Video => 1,
        }
    }

    pub fn from_i64(v: i64) -> Self {
        if v == 1 {
            MediaKind::Video
        } else {
            MediaKind::Image
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "image" | "images" => Some(MediaKind::Image),
            "video" | "videos" => Some(MediaKind::Video),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            MediaKind::Image => "IMAGE",
            MediaKind::Video => "VIDEO",
        }
    }
}

const IMAGE_EXT: &[&str] = &["png", "jpg", "jpeg", "webp", "gif", "bmp", "avif"];
const VIDEO_EXT: &[&str] = &[
    "mp4", "mkv", "webm", "mov", "avi", "m4v", "wmv", "flv", "mpg", "mpeg", "ts", "ogv",
];

/// Classify a file by extension. `None` = unsupported.
pub fn classify(path: &std::path::Path) -> Option<MediaKind> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    if IMAGE_EXT.contains(&ext.as_str()) {
        Some(MediaKind::Image)
    } else if VIDEO_EXT.contains(&ext.as_str()) {
        Some(MediaKind::Video)
    } else {
        None
    }
}

/// A library entry. Mirrors one row of the `media` table.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Media {
    pub id: i64,
    pub path: PathBuf,
    pub name: String,
    pub kind: MediaKind,
    pub size: u64,
    /// Seconds since Unix epoch.
    pub mtime: i64,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub duration_ms: Option<u64>,
    pub codec: Option<String>,
    pub added_at: i64,
    pub missing: bool,
    pub favorite: bool,
    /// 0 = unrated, 1..=5 stars.
    pub rating: u8,
    pub resume_ms: Option<u64>,
    pub play_count: u32,
    pub last_played_at: Option<i64>,
    /// Hidden from the library (database flag only; the file is untouched).
    pub hidden: bool,
    /// Newly imported and not yet triaged.
    pub inbox: bool,
    /// 0 = fine, 1 = decode failure, 2 = checksum changed.
    pub damaged: u8,
    pub check_note: Option<String>,
    pub checked_at: Option<i64>,
    /// Quick content fingerprint, `fp1:<hex>`.
    pub fp: Option<String>,
}

/// Metadata discovered after indexing (by core for images, by `media` for videos).
#[derive(Debug, Clone, Default)]
pub struct MediaMeta {
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub duration_ms: Option<u64>,
    pub codec: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Stats {
    pub total: u64,
    pub images: u64,
    pub videos: u64,
    pub favorites: u64,
    pub missing: u64,
    pub folders: u64,
    pub total_bytes: u64,
    pub rated: u64,
    pub hidden: u64,
    pub inbox: u64,
    pub damaged: u64,
    pub played: u64,
}
