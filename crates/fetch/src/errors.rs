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
pub const GENERIC: &str = "Something went wrong.";

/// Every sentence `classify` can return (for tests and for the CLI).
pub const ALL: &[&str] = &[
    NOT_REACHABLE,
    BAD_KEY,
    UNSAFE_PATHS,
    DISK_FULL,
    CANCELLED,
    NO_VIDEO,
    NO_FOLDER,
    GENERIC,
];

/// Map any internal error text to one fixed sentence.
pub fn classify(raw: &str) -> &'static str {
    let r = raw.to_ascii_lowercase();
    if r.contains("cancelled") {
        CANCELLED
    } else if r.contains("unsafe file path") {
        UNSAFE_PATHS
    } else if r.contains("os error 28") || r.contains("no space left") {
        DISK_FULL
    } else if r.contains("http 401")
        || r.contains("http 403")
        || r.contains("incorrect user credentials")
    {
        BAD_KEY
    } else if r.contains("no video") {
        NO_VIDEO
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
}
