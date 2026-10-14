//! What did the user paste into the one box? Pure: no I/O, the filesystem is
//! never consulted (a path is judged by how it is written).

use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Route {
    Magnet(String),
    TorrentFile(PathBuf),
    TorrentUrl(String),
    /// A streaming source definition (JSON) on disk.
    SourceFile(PathBuf),
    /// A local `.m3u8` or `.mpd`.
    LocalPlaylist(PathBuf),
    /// Any other `http(s)` address: a file, a manifest or a page.
    Url(String),
    /// Everything else is a search query.
    Search(String),
}

fn strip_quotes(s: &str) -> &str {
    let mut s = s.trim();
    loop {
        let b = s.as_bytes();
        if b.len() >= 2 && matches!(b[0], b'"' | b'\'') && b[b.len() - 1] == b[0] {
            s = s[1..s.len() - 1].trim();
        } else {
            return s;
        }
    }
}

fn starts_ci(s: &str, p: &str) -> bool {
    s.len() >= p.len() && s.is_char_boundary(p.len()) && s[..p.len()].eq_ignore_ascii_case(p)
}

fn ext_of(path: &str) -> String {
    let name = path.rsplit(['/', '\\']).next().unwrap_or("");
    name.rsplit_once('.')
        .map(|(_, e)| e.to_ascii_lowercase())
        .unwrap_or_default()
}

fn looks_like_path(s: &str) -> bool {
    let b = s.as_bytes();
    s.starts_with('/')
        || s.starts_with("~/")
        || s.starts_with("./")
        || s.starts_with("../")
        || s.starts_with("\\\\")
        || starts_ci(s, "file://")
        || (b.len() >= 3
            && b[0].is_ascii_alphabetic()
            && b[1] == b':'
            && (b[2] == b'\\' || b[2] == b'/'))
}

fn file_url_path(s: &str) -> String {
    let rest = &s[7..];
    // `file:///C:/x` -> `C:/x`
    let b = rest.as_bytes();
    if b.len() >= 3 && b[0] == b'/' && b[1].is_ascii_alphabetic() && b[2] == b':' {
        rest[1..].to_owned()
    } else {
        rest.to_owned()
    }
}

pub fn route(input: &str) -> Option<Route> {
    let s = strip_quotes(input);
    if s.is_empty() {
        return None;
    }
    if starts_ci(s, "magnet:") {
        return Some(Route::Magnet(s.to_owned()));
    }
    if (starts_ci(s, "http://") || starts_ci(s, "https://")) && !s.contains(char::is_whitespace) {
        let path = s.split(['?', '#']).next().unwrap_or(s);
        return Some(if ext_of(path) == "torrent" {
            Route::TorrentUrl(s.to_owned())
        } else {
            Route::Url(s.to_owned())
        });
    }
    if looks_like_path(s) {
        let p = if starts_ci(s, "file://") {
            file_url_path(s)
        } else {
            s.to_owned()
        };
        match ext_of(&p).as_str() {
            "torrent" => return Some(Route::TorrentFile(PathBuf::from(p))),
            "json" => return Some(Route::SourceFile(PathBuf::from(p))),
            "m3u8" | "mpd" => return Some(Route::LocalPlaylist(PathBuf::from(p))),
            _ => {}
        }
    }
    Some(Route::Search(s.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::Route::*;
    use super::*;

    #[test]
    fn routing_table() {
        let p = |s: &str| PathBuf::from(s);
        let cases: Vec<(&str, Option<Route>)> = vec![
            ("", None),
            ("   ", None),
            ("\"\"", None),
            (
                "magnet:?xt=urn:btih:abc",
                Some(Magnet("magnet:?xt=urn:btih:abc".into())),
            ),
            ("  MAGNET:?xt=1 ", Some(Magnet("MAGNET:?xt=1".into()))),
            ("\"magnet:?xt=2\"", Some(Magnet("magnet:?xt=2".into()))),
            (
                "https://h.invalid/a.torrent",
                Some(TorrentUrl("https://h.invalid/a.torrent".into())),
            ),
            (
                "http://h.invalid/a.TORRENT?k=v#x",
                Some(TorrentUrl("http://h.invalid/a.TORRENT?k=v#x".into())),
            ),
            (
                "https://h.invalid/a.mp4?f=x.torrent",
                Some(Url("https://h.invalid/a.mp4?f=x.torrent".into())),
            ),
            (
                "https://h.invalid/a.m3u8",
                Some(Url("https://h.invalid/a.m3u8".into())),
            ),
            (
                "https://h.invalid/page",
                Some(Url("https://h.invalid/page".into())),
            ),
            (
                "'https://h.invalid/p'",
                Some(Url("https://h.invalid/p".into())),
            ),
            (
                "https://h.invalid/a b",
                Some(Search("https://h.invalid/a b".into())),
            ),
            ("/tmp/a.torrent", Some(TorrentFile(p("/tmp/a.torrent")))),
            (
                "~/Downloads/x.torrent",
                Some(TorrentFile(p("~/Downloads/x.torrent"))),
            ),
            (
                "\"/tmp/my file.torrent\"",
                Some(TorrentFile(p("/tmp/my file.torrent"))),
            ),
            ("./src.json", Some(SourceFile(p("./src.json")))),
            ("/etc/s.JSON", Some(SourceFile(p("/etc/s.JSON")))),
            ("/x/list.m3u8", Some(LocalPlaylist(p("/x/list.m3u8")))),
            ("C:\\v\\a.mpd", Some(LocalPlaylist(p("C:\\v\\a.mpd")))),
            ("file:///tmp/a.m3u8", Some(LocalPlaylist(p("/tmp/a.m3u8")))),
            (
                "file:///C:/v/a.torrent",
                Some(TorrentFile(p("C:/v/a.torrent"))),
            ),
            ("a.torrent", Some(Search("a.torrent".into()))),
            ("/tmp/clip.mp4", Some(Search("/tmp/clip.mp4".into()))),
            ("some words", Some(Search("some words".into()))),
            (
                "ftp://h.invalid/a.torrent",
                Some(Search("ftp://h.invalid/a.torrent".into())),
            ),
        ];
        for (i, (inp, want)) in cases.into_iter().enumerate() {
            assert_eq!(route(inp), want, "case {i}: {inp:?}");
        }
    }
}
