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
pub mod subs;
pub mod theme;
pub mod widgets;

pub use app::{JizzApp, Options};
