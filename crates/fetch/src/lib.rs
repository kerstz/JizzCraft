//! Optional network companion.
//!
//! Everything that does not touch a socket (config, secrets, Torznab URL
//! building and XML parsing, redaction) is always compiled and unit-tested.
//! The BitTorrent engine and the HTTP client live behind the `network`
//! feature, which is off in the default build.
//! The crate that knows about the internet, kept on a short leash.

pub mod config;
pub mod detect;
pub mod errors;
pub mod inbox;
pub mod pick;
pub mod redact;
pub mod secret;
pub mod sources;
pub mod torznab;

#[cfg(feature = "network")]
pub mod commands;
#[cfg(feature = "network")]
pub mod resolver;
#[cfg(feature = "network")]
pub mod service;
#[cfg(feature = "network")]
pub mod stream;

pub use config::{FetchConfig, Indexer};
pub use secret::Secret;
pub use torznab::SearchResult;
