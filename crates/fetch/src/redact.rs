/// Drop the query string and fragment of a URL: that is where `apikey` and
/// the search terms live. Every error string that mentions a URL goes through
/// this.
pub fn redact_url(url: &str) -> String {
    let end = url.find(['?', '#']).unwrap_or(url.len());
    let base = &url[..end];
    // Userinfo (`user:pass@host`) is a secret too. Putting a password in a URL
    // is a bold choice; we respect it by deleting it.
    match base.split_once("://") {
        Some((scheme, rest)) => {
            let cut = rest.find('/').unwrap_or(rest.len());
            let (authority, path) = rest.split_at(cut);
            let host = authority.rsplit('@').next().unwrap_or(authority);
            format!("{scheme}://{host}{path}")
        }
        None => {
            // `user:pass@host/path` without a scheme. Only `/` ends the
            // authority here: a backslash is more likely part of a password,
            // and over-redacting is the safe direction.
            let cut = base.find('/').unwrap_or(base.len());
            match base[..cut].rsplit_once('@') {
                Some((_, host)) => format!("{host}{}", &base[cut..]),
                None => base.to_owned(),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::redact_url;

    #[test]
    fn query_fragment_and_userinfo_are_dropped() {
        assert_eq!(
            redact_url("https://u:p@h.invalid:81/a/b?apikey=K#f"),
            "https://h.invalid:81/a/b"
        );
        assert_eq!(redact_url("https://h.invalid"), "https://h.invalid");
        assert_eq!(redact_url("u:p@h.invalid/x?k=1"), "h.invalid/x");
        assert_eq!(redact_url("magnet:?xt=urn:btih:abc"), "magnet:");
        assert_eq!(
            redact_url("http://user:pw\\@h.invalid/x"),
            "http://h.invalid/x"
        );
    }
}
