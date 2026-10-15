//! egui front end: Craft theme, grid, viewer, player, inspector, dialogs.
//! Immediate mode: the whole UI is redrawn from scratch, like a goldfish with
//! excellent frame times.

pub mod app;
pub mod bench;
pub mod config;
pub mod edition;
pub mod fun;
pub mod i18n;
pub mod identity;
pub mod keys;
pub mod lang;
pub mod player_view;
pub mod root;
pub mod subs;
pub mod theme;
pub mod widgets;

pub use app::{JizzApp, Library, Options};
pub use root::Root;

/// Unit tests must never touch the real cache, config or library of the user
/// who runs them: `paths::set_home` is process-wide and set once.
#[cfg(test)]
pub(crate) mod test_support {
    use std::sync::OnceLock;

    static HOME: OnceLock<std::path::PathBuf> = OnceLock::new();

    /// Point every default path (cache, config, state) at a throwaway
    /// directory, once for the whole test process.
    pub(crate) fn isolate_home() -> &'static std::path::Path {
        HOME.get_or_init(|| {
            let dir = tempfile::tempdir().expect("temp dir").keep();
            assert!(
                jizzcraft_core::paths::set_home(dir.clone())
                    || jizzcraft_core::paths::home().is_some(),
                "home chosen"
            );
            dir
        })
    }
}
