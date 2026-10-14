//! Log hygiene: full paths and filenames only appear when debug logging is
//! explicitly enabled with `JIZZCRAFT_DEBUG=1`.

use std::path::Path;

pub fn debug_enabled() -> bool {
    std::env::var_os("JIZZCRAFT_DEBUG").is_some_and(|v| v == "1")
}

/// Printable form of a path for logs/errors: the real path in debug mode,
/// otherwise a stable opaque tag.
pub fn redact(path: &Path) -> String {
    if debug_enabled() {
        return path.display().to_string();
    }
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    path.hash(&mut h);
    format!("<path:{:08x}>", h.finish() as u32)
}

/// Debug-only log line; compiled in but silent unless `JIZZCRAFT_DEBUG=1`.
#[macro_export]
macro_rules! debug_log {
    ($($arg:tt)*) => {
        if $crate::privacy::debug_enabled() {
            eprintln!("[jizzcraft:debug] {}", format_args!($($arg)*));
        }
    };
}
