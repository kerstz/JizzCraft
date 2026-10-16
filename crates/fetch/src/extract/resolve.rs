//! Turning an address found in a page into one the network code may fetch.

use super::MAX_URL;
use crate::sources::inward_ok;

/// Resolve `raw` against `base` (the page, or its `<base href>`). Only
/// `http(s)` survives, and an address that points inward (loopback, private
/// ranges, however it is spelled) is dropped unless `origin` (the page it came
/// from) is inward itself: a public page never aims the app at the LAN.
pub fn resolve_candidate(base: &str, origin: &str, raw: &str) -> Option<String> {
    if raw.trim().is_empty() || raw.len() > MAX_URL || raw.chars().any(char::is_control) {
        return None;
    }
    let u = reqwest::Url::parse(base).ok()?.join(raw.trim()).ok()?;
    if !matches!(u.scheme(), "http" | "https") || u.as_str().len() > MAX_URL {
        return None;
    }
    if u.host_str().is_none_or(str::is_empty) || !inward_ok(origin, u.as_str()) {
        return None;
    }
    Some(u.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_addresses_join_and_inward_ones_are_dropped() {
        let page = "https://site.invalid/a/b.html";
        let r = |raw: &str| resolve_candidate(page, page, raw);
        assert_eq!(r("c.mp4").as_deref(), Some("https://site.invalid/a/c.mp4"));
        assert_eq!(r("/c.mp4").as_deref(), Some("https://site.invalid/c.mp4"));
        assert_eq!(
            r("//cdn.invalid/c.mp4").as_deref(),
            Some("https://cdn.invalid/c.mp4")
        );
        for bad in [
            "http://127.0.0.1/x.mp4",
            "http://localhost/x.mp4",
            "http://[::1]/x.mp4",
            "http://0x7f000001/x.mp4",
            "http://127.1/x.mp4",
            "http://192.168.1.1/x.mp4",
            "http://127.0.0.1\\@x.invalid/x.mp4",
            "javascript:alert(1)",
            "data:video/mp4;base64,AAAA",
            "file:///etc/passwd",
            "blob:https://site.invalid/uuid",
            "",
        ] {
            assert_eq!(r(bad), None, "{bad}");
        }
        let local = "http://127.0.0.1:8080/p";
        assert!(resolve_candidate(local, local, "/v.mp4").is_some());
        assert!(resolve_candidate(local, local, "http://192.168.1.2/v.mp4").is_some());
    }
}
