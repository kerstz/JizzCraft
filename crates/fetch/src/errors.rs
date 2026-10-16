//! Plain-language failures. Raw errors from the HTTP client or the torrent
//! engine can contain URLs, torrent names or paths; nothing shown to the user
//! or written to a log comes from them directly. Everything goes through
//! [`classify`], whose output is a fixed set of sentences.

pub const NOT_REACHABLE: &str = "Indexer not reachable. Is Jackett or Prowlarr running?";
pub const BAD_KEY: &str = "The indexer refused the API key.";
pub const UNSAFE_PATHS: &str = "Torrent refused: unsafe file names.";
pub const DISK_FULL: &str = "Disk full. Downloads paused.";
pub const CANCELLED: &str = "Cancelled.";
pub const NO_VIDEO: &str = "No video file in this torrent.";
pub const NO_FOLDER: &str = "The download folder is not usable.";
pub const PROXY_REQUIRED: &str = "A proxy is required and none is set.";
pub const SOURCE_CHANGED: &str = "The source changed while playing.";
pub const RESOLVER_FAILED: &str = "The resolver failed or took too long.";
pub const NO_VIDEO_HERE: &str = "No video found at this address.";
pub const PROTECTED: &str = "This stream is protected (DRM) and cannot be played.";
pub const UNSUPPORTED_STREAM: &str = "This stream format is not supported.";
pub const STALLED: &str = "The stream stopped sending data.";
pub const LIVE_ENDED: &str = "The live stream ended.";
pub const TOO_LARGE: &str = "This stream is larger than the app allows.";
pub const NOT_FOUND: &str = "The address was not found.";
pub const GENERIC: &str = "Something went wrong.";
pub const UNREACHABLE: &str = "The server could not be reached.";
pub const FORBIDDEN: &str = "The server refused access.";
pub const EXPIRED: &str = "The link expired.";
pub const TIMEOUT: &str = "The server took too long to answer.";
pub const SERVER_BUSY: &str = "The server is busy. Try again later.";
pub const LIVE_UNSUPPORTED: &str = "Live DASH streams are not supported.";
pub const SEPARATE_TRACKS: &str =
    "This stream keeps video and audio apart in a format that cannot be combined.";
pub const CLOCK_SKEW: &str = "The secure connection failed. Check the computer's date and time.";
pub const NO_PEERS: &str = "No peers found for this torrent.";
pub const METADATA_TIMEOUT: &str = "Could not get the torrent details in time.";
pub const NEEDS_FULL_DOWNLOAD: &str = "This file must be fully downloaded before it can play.";

pub const RESOLVER_MISSING: &str =
    "The resolver program is missing. Look for a tool again in Settings.";
pub const RESOLVER_UNSUPPORTED: &str =
    "Your resolver could not read this page. The site may have changed; update the tool.";
pub const AGE_WALL: &str = "This site requires age confirmation or sign-in on its own website. JizzCraft cannot do this for you.";
pub const LOGIN_REQUIRED: &str = "This video requires an account on the site.";
pub const GEO_BLOCKED: &str = "This video is not available in your region.";
pub const RATE_LIMITED: &str = "The site is limiting requests. Wait a few minutes.";
pub const NEEDS_SAVING: &str =
    "This video cannot be streamed without saving it. Use Download to save it first.";
pub const NEEDS_JS: &str = "This page builds its video with scripts, which JizzCraft does not run. A resolver tool you installed may read it.";
pub const LIVE_NOT_SAVED: &str = "Live streams cannot be saved.";
pub const NOT_A_VIDEO: &str = "The address did not send a video file.";
pub const INCOMPLETE: &str = "The download ended before the whole file arrived.";
pub const FOLDER_CHANGED: &str = "The save folder changed during the download. Nothing was saved.";

/// Every sentence `classify` can return (for tests and for the CLI).
pub const ALL: &[&str] = &[
    NOT_REACHABLE,
    BAD_KEY,
    UNSAFE_PATHS,
    DISK_FULL,
    CANCELLED,
    NO_VIDEO,
    NO_FOLDER,
    PROXY_REQUIRED,
    SOURCE_CHANGED,
    RESOLVER_FAILED,
    NO_VIDEO_HERE,
    PROTECTED,
    UNSUPPORTED_STREAM,
    STALLED,
    LIVE_ENDED,
    TOO_LARGE,
    NOT_FOUND,
    GENERIC,
    UNREACHABLE,
    FORBIDDEN,
    EXPIRED,
    TIMEOUT,
    SERVER_BUSY,
    LIVE_UNSUPPORTED,
    SEPARATE_TRACKS,
    CLOCK_SKEW,
    NO_PEERS,
    METADATA_TIMEOUT,
    NEEDS_FULL_DOWNLOAD,
    RESOLVER_MISSING,
    RESOLVER_UNSUPPORTED,
    AGE_WALL,
    LOGIN_REQUIRED,
    GEO_BLOCKED,
    RATE_LIMITED,
    NEEDS_SAVING,
    NEEDS_JS,
    LIVE_NOT_SAVED,
    NOT_A_VIDEO,
    INCOMPLETE,
    FOLDER_CHANGED,
];

/// `word` appears in `r` as a whole word (so "alive" is not "live").
fn has_word(r: &str, word: &str) -> bool {
    r.split(|c: char| !c.is_ascii_alphanumeric())
        .any(|w| w == word)
}

/// Map any internal error text to one fixed sentence.
/// The stack trace stays in the kitchen; the customer gets the polite version.
pub fn classify(raw: &str) -> &'static str {
    let r = raw.to_ascii_lowercase();
    if r.contains("cancelled") {
        CANCELLED
    } else if r.contains("proxy required") {
        PROXY_REQUIRED
    } else if r.contains("clock")
        || r.contains("notvalidyet")
        || r.contains("not valid yet")
        || (r.contains("certificate") && r.contains("expired"))
    {
        CLOCK_SKEW
    } else if r.contains("cannot stream without saving") {
        NEEDS_SAVING
    } else if r.contains("page needs scripts") {
        NEEDS_JS
    } else if r.contains("resolver missing") {
        RESOLVER_MISSING
    } else if r.contains("resolver unsupported") {
        RESOLVER_UNSUPPORTED
    } else if r.contains("resolver age wall") {
        AGE_WALL
    } else if r.contains("resolver login") {
        LOGIN_REQUIRED
    } else if r.contains("resolver geo") {
        GEO_BLOCKED
    } else if r.contains("resolver drm") {
        PROTECTED
    } else if r.contains("resolver rate limit") {
        RATE_LIMITED
    } else if r.contains("resolver gone") {
        NOT_FOUND
    } else if r.contains("resolver") {
        RESOLVER_FAILED
    } else if r.contains("cannot save live") {
        LIVE_NOT_SAVED
    } else if r.contains("not a video") {
        NOT_A_VIDEO
    } else if r.contains("incomplete download") {
        INCOMPLETE
    } else if r.contains("folder changed") {
        FOLDER_CHANGED
    } else if r.contains("source changed") {
        SOURCE_CHANGED
    } else if r.contains("expired") {
        EXPIRED
    } else if r.contains("server busy") {
        SERVER_BUSY
    } else if r.contains("metadata timeout") {
        METADATA_TIMEOUT
    } else if r.contains("no peers") {
        NO_PEERS
    } else if r.contains("needs full download") {
        NEEDS_FULL_DOWNLOAD
    } else if r.contains("separate tracks") {
        SEPARATE_TRACKS
    } else if r.contains("unsafe file path") {
        UNSAFE_PATHS
    } else if r.contains("os error 28") || r.contains("no space left") || r.contains("disk full") {
        DISK_FULL
    } else if r.contains("http 401")
        || r.contains("http 403")
        || r.contains("incorrect user credentials")
    {
        BAD_KEY
    } else if r.contains("no video") {
        NO_VIDEO
    } else if r.contains("no media") {
        NO_VIDEO_HERE
    } else if r.contains("protected") {
        PROTECTED
    } else if r.contains("unsupported stream") {
        UNSUPPORTED_STREAM
    } else if r.contains("live ended") {
        LIVE_ENDED
    } else if has_word(&r, "live") {
        LIVE_UNSUPPORTED
    } else if r.contains("too large") {
        TOO_LARGE
    } else if r.contains("stalled") {
        STALLED
    } else if r.contains("http 404") || r.contains("http 410") {
        NOT_FOUND
    } else if r.contains("server timeout") {
        TIMEOUT
    } else if r.contains("forbidden") {
        FORBIDDEN
    } else if r.contains("unreachable") || r.contains("redirect") {
        UNREACHABLE
    } else if r.contains("timed out")
        || r.contains("timeout")
        || r.contains("error sending request")
        || r.contains("connect")
        || r.contains("dns")
        || r.contains("http 5")
        || r.contains("no enabled indexer")
    {
        NOT_REACHABLE
    } else if r.contains("download folder") || r.contains("permission denied") {
        NO_FOLDER
    } else {
        GENERIC
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_is_always_a_fixed_sentence_and_never_echoes_input() {
        let inputs = [
            "http://127.0.0.1:9117/api?apikey=SECRETKEY&q=private words: HTTP 401",
            "/home/me/Downloads/Some Torrent Name/file.mkv: No space left on device (os error 28)",
            "error sending request for url (http://host/x?apikey=k): connection refused",
            "torrent contains an unsafe file path; refused",
            "totally unexpected: Some Torrent Name",
            "",
        ];
        for i in inputs {
            let out = classify(i);
            assert!(ALL.contains(&out), "{out}");
            assert!(!out.contains("SECRETKEY") && !out.contains("Torrent Name"));
        }
        assert_eq!(classify(inputs[0]), BAD_KEY);
        assert_eq!(classify(inputs[1]), DISK_FULL);
        assert_eq!(classify(inputs[2]), NOT_REACHABLE);
        assert_eq!(classify(inputs[3]), UNSAFE_PATHS);
        assert_eq!(classify(inputs[4]), GENERIC);
    }

    #[test]
    fn stream_failures_map_to_their_sentence_without_echo() {
        let cases = [
            ("no media in https://x.invalid/secret-title", NO_VIDEO_HERE),
            ("protected", PROTECTED),
            ("unsupported stream", UNSUPPORTED_STREAM),
            ("stalled", STALLED),
            ("live ended", LIVE_ENDED),
            ("too large", TOO_LARGE),
            ("HTTP 404", NOT_FOUND),
            ("HTTP 410", NOT_FOUND),
        ];
        for (raw, want) in cases {
            let out = classify(raw);
            assert_eq!(out, want, "{raw}");
            assert!(ALL.contains(&out));
            assert!(!out.contains("secret-title") && !out.contains("x.invalid"));
        }
        // Existing priorities are kept.
        assert_eq!(classify("HTTP 503"), NOT_REACHABLE);
        assert_eq!(classify("HTTP 403"), BAD_KEY);
    }

    #[test]
    fn new_keywords_map_to_new_sentences() {
        let cases = [
            ("metadata timeout", METADATA_TIMEOUT),
            ("no peers", NO_PEERS),
            ("disk full", DISK_FULL),
            ("needs full download", NEEDS_FULL_DOWNLOAD),
            ("live", LIVE_UNSUPPORTED),
            ("separate tracks", SEPARATE_TRACKS),
            ("expired", EXPIRED),
            ("server busy", SERVER_BUSY),
            ("clock skew", CLOCK_SKEW),
            ("invalid peer certificate: NotValidYet", CLOCK_SKEW),
            ("invalid peer certificate: Expired", CLOCK_SKEW),
            ("live ended", LIVE_ENDED),
            ("an alive thing", GENERIC),
        ];
        for (raw, want) in cases {
            assert_eq!(classify(raw), want, "{raw}");
        }
        assert!(ALL.len() >= 36);
        let resolver = [
            ("resolver missing", RESOLVER_MISSING),
            ("resolver unsupported", RESOLVER_UNSUPPORTED),
            ("resolver age wall", AGE_WALL),
            ("resolver login", LOGIN_REQUIRED),
            ("resolver geo", GEO_BLOCKED),
            ("resolver drm", PROTECTED),
            ("resolver rate limit", RATE_LIMITED),
            ("resolver gone", NOT_FOUND),
            ("resolver failed", RESOLVER_FAILED),
            ("cannot stream without saving", NEEDS_SAVING),
            ("page needs scripts", NEEDS_JS),
            ("cannot save live", LIVE_NOT_SAVED),
            ("cannot save live stream http://x.invalid/t", LIVE_NOT_SAVED),
            ("not a video", NOT_A_VIDEO),
            ("incomplete download", INCOMPLETE),
            ("folder changed", FOLDER_CHANGED),
        ];
        for (raw, want) in resolver {
            assert_eq!(classify(raw), want, "{raw}");
            assert!(ALL.contains(&want));
            assert!(!want.contains("x.invalid"));
        }
    }
}
