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
            let (authority, path) = rest.split_once('/').map_or((rest, ""), |(a, p)| (a, p));
            let host = authority.rsplit('@').next().unwrap_or(authority);
            if path.is_empty() {
                format!("{scheme}://{host}")
            } else {
                format!("{scheme}://{host}/{path}")
            }
        }
        None => base.to_owned(),
    }
}
