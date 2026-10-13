//! Thumbnails, codec probing and (soon) the GStreamer player.
//! Everything here is local-file only: no network element is ever created.

pub mod integrity;
pub mod player;
pub mod preview;
pub mod probe;
pub mod service;
pub mod thumbs;

use std::sync::OnceLock;

/// Initialise GStreamer once. Safe to call from any thread, any number of times.
pub fn init() -> Result<(), String> {
    static INIT: OnceLock<Result<(), String>> = OnceLock::new();
    INIT.get_or_init(|| {
        gstreamer::init().map_err(|e| {
            format!("GStreamer is not usable: {e}. Install gstreamer + gst-plugins-base.")
        })
    })
    .clone()
}

/// A `file://` URI for a local path. Never accepts anything else.
pub fn file_uri(path: &std::path::Path) -> Result<String, String> {
    let abs = std::fs::canonicalize(path).map_err(|e| format!("cannot open file: {e}"))?;
    gstreamer::glib::filename_to_uri(&abs, None)
        .map(|u| u.to_string())
        .map_err(|e| format!("bad path: {e}"))
}

/// Turn a GStreamer error/missing-plugin message into a human sentence.
pub fn describe_missing_plugin(msg: &gstreamer::Message) -> Option<String> {
    use gstreamer_pbutils::MissingPluginMessage;
    if !MissingPluginMessage::is(msg) {
        return None;
    }
    let desc = MissingPluginMessage::parse(msg).ok()?.description();
    Some(format!(
        "missing GStreamer plugin: {desc} — install gst-plugins-good/bad/ugly and gst-libav"
    ))
}

/// Append a GStreamer debug string to an error only when debug logging is on
/// (it can contain file paths).
pub(crate) fn debug_detail(text: &mut String, detail: &str) {
    if jizzcraft_core::privacy::debug_enabled() {
        text.push_str(" (");
        text.push_str(detail);
        text.push(')');
    }
}
