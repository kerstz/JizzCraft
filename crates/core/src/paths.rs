use directories::ProjectDirs;
use std::path::PathBuf;

fn dirs() -> Option<ProjectDirs> {
    ProjectDirs::from("org", "jizzcraft", "JizzCraft")
}

/// Library database. `JIZZCRAFT_DB` overrides it (tests, CLI `--db`).
pub fn db_path() -> PathBuf {
    if let Some(p) = std::env::var_os("JIZZCRAFT_DB") {
        return PathBuf::from(p);
    }
    dirs()
        .map(|d| d.data_dir().join("library.db"))
        .unwrap_or_else(|| PathBuf::from("library.db"))
}

pub fn config_dir() -> PathBuf {
    dirs()
        .map(|d| d.config_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Thumbnails live here and nowhere else: never next to the originals.
pub fn cache_dir() -> PathBuf {
    dirs()
        .map(|d| d.cache_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from(".cache"))
}
