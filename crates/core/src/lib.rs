//! JizzCraft engine. No UI, no GStreamer: everything here is testable headless.

pub mod commands;
pub mod db;
pub mod error;
pub mod fingerprint;
pub mod fsutil;
pub mod history;
pub mod integrity;
pub mod manifest;
pub mod markers;
pub mod model;
pub mod pathbytes;
pub mod paths;
pub mod portability;
pub mod privacy;
pub mod scan;
pub mod view;

pub use commands::{HasLibrary, Registry, register_core};
pub use db::{Db, Filter, HiddenMode, Orientation, SortKey};
pub use error::{Error, Result};
pub use history::MediaPatch;
pub use markers::Marker;
pub use model::{Media, MediaKind, MediaMeta, Stats};
