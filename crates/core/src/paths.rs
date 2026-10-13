use directories::ProjectDirs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

fn dirs() -> Option<ProjectDirs> {
    ProjectDirs::from("org", "jizzcraft", "JizzCraft")
}

/// Explicit home (portable mode). Set once at startup, before any path lookup.
static HOME: OnceLock<PathBuf> = OnceLock::new();

/// Use `dir` as the single home for database, config and cache. Returns
/// `false` if a home was already chosen.
pub fn set_home(dir: PathBuf) -> bool {
    HOME.set(dir).is_ok()
}

/// `JIZZCRAFT_HOME`, when set and non-empty.
pub fn home_from_env() -> Option<PathBuf> {
    std::env::var_os("JIZZCRAFT_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

/// Portable home: `jizzcraft-data` next to the executable.
pub fn portable_home() -> Option<PathBuf> {
    std::env::current_exe()
        .ok()?
        .parent()
        .map(|p| p.join("jizzcraft-data"))
}

fn home() -> Option<&'static Path> {
    HOME.get().map(PathBuf::as_path)
}

pub fn is_portable() -> bool {
    home().is_some()
}

/// Library database. Precedence: `JIZZCRAFT_DB`, explicit home, XDG.
pub fn db_path() -> PathBuf {
    if let Some(p) = std::env::var_os("JIZZCRAFT_DB") {
        return PathBuf::from(p);
    }
    if let Some(h) = home() {
        return h.join("library.db");
    }
    dirs()
        .map(|d| d.data_dir().join("library.db"))
        .unwrap_or_else(|| PathBuf::from("library.db"))
}

pub fn config_dir() -> PathBuf {
    if let Some(h) = home() {
        return h.join("config");
    }
    dirs()
        .map(|d| d.config_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Thumbnails live here and nowhere else: never next to the originals.
pub fn cache_dir() -> PathBuf {
    if let Some(h) = home() {
        return h.join("cache");
    }
    dirs()
        .map(|d| d.cache_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from(".cache"))
}
