//! Is a file head a list of locations instead of media? Pure, no I/O.

/// Does this file head look like a streaming manifest or a list of locations
/// (HLS, DASH, Smooth Streaming, SDP, plain URL list) rather than media?
/// Used on in-process streams before any pipeline exists, so a playlist
/// pushed through the stream source is refused up front.
/// A shopping list is not groceries, and an `.m3u8` is not a video.
pub fn looks_like_manifest(head: &[u8]) -> bool {
    let text = String::from_utf8_lossy(&head[..head.len().min(1024)]);
    let t = text.trim_start_matches('\u{feff}').trim_start();
    if t.starts_with("#EXTM3U") || t.starts_with("v=0") && t.contains("\no=") {
        return true;
    }
    if t.starts_with('<') && (t.contains("<MPD") || t.contains("<SmoothStreamingMedia")) {
        return true;
    }
    // text/uri-list: every non-comment line is `scheme://…`.
    let mut lines = t
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'));
    let mut n = 0;
    for l in lines.by_ref() {
        let ok = l.split_once("://").is_some_and(|(s, _)| {
            !s.is_empty()
                && s.chars()
                    .all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c))
        }) && !l.contains(char::is_whitespace);
        if !ok {
            return false;
        }
        n += 1;
    }
    n > 0 && t.is_ascii()
}
