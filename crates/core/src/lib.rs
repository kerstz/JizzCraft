//! JizzCraft engine. No UI, no GStreamer: everything here is testable headless.
//! It has never seen a single pixel, and it is fine with that.

pub mod commands;
pub mod db;
pub mod distro;
pub mod error;
pub mod fingerprint;
pub mod fsutil;
pub mod history;
pub mod integrity;
pub mod manifest;
pub mod markers;
pub mod model;
pub mod organize;
pub mod pathbytes;
pub mod paths;
pub mod portability;
pub mod privacy;
pub mod report;
pub mod scan;
pub mod search;
pub mod subtitles;
pub mod timed;
pub mod vault;
pub mod vault_convert;
pub mod view;

pub use commands::{HasLibrary, Registry, register_core};
pub use db::{Db, Filter, HiddenMode, Orientation, RemoveReport, SortKey};
pub use error::{Error, Result};
pub use history::{MediaPatch, UserState};
pub use markers::Marker;
pub use model::{Media, MediaKind, MediaMeta, Stats};
pub use organize::OrgFilter;
pub use timed::{Chunk, TimedSource, VariantInfo};
