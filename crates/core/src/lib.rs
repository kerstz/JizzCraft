//! JizzCraft engine. No UI, no GStreamer: everything here is testable headless.

pub mod commands;
pub mod db;
pub mod error;
pub mod model;
pub mod paths;
pub mod privacy;
pub mod scan;

pub use commands::{HasLibrary, Registry, register_core};
pub use db::{Db, Filter, SortKey};
pub use error::{Error, Result};
pub use model::{Media, MediaKind, MediaMeta, Stats};
