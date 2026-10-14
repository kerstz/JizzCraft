//! Proxy state shared by the static request helpers.

use super::FetchService;
use crate::config::FetchConfig;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

/// The SOCKS5 proxy for HTTP requests (indexers, direct downloads), shared by
/// the static request helpers. Set from the configuration.
static HTTP_PROXY: Mutex<Option<String>> = Mutex::new(None);

/// Mirrors `FetchConfig::require_proxy` for the static request helpers.
static REQUIRE_PROXY: AtomicBool = AtomicBool::new(false);

/// True when the kill switch is on and no proxy is set.
pub(crate) fn proxy_blocked() -> bool {
    REQUIRE_PROXY.load(Ordering::SeqCst) && current_proxy().is_none()
}

/// Redirects stay on the same host and scheme. A key sent to a source must
/// never follow a redirect to another site.
pub(crate) fn same_origin_policy() -> reqwest::redirect::Policy {
    reqwest::redirect::Policy::custom(|attempt| {
        let ok = attempt.previous().len() < 3
            && attempt.previous().last().is_some_and(|p| {
                p.host_str() == attempt.url().host_str() && p.scheme() == attempt.url().scheme()
            });
        if ok { attempt.follow() } else { attempt.stop() }
    })
}

/// The SOCKS5 proxy currently in force, for helpers started by the user.
pub(crate) fn current_proxy() -> Option<String> {
    HTTP_PROXY.lock().ok().and_then(|g| g.clone())
}

pub(super) fn publish_proxy(cfg: &FetchConfig) {
    REQUIRE_PROXY.store(cfg.require_proxy, Ordering::SeqCst);
    if let Ok(mut g) = HTTP_PROXY.lock() {
        *g = cfg
            .proxy
            .as_ref()
            .filter(|p| !p.is_empty())
            .map(|p| p.expose().to_owned());
    }
}

impl FetchService {
    pub(crate) fn builder() -> Result<reqwest::ClientBuilder, String> {
        let mut b = reqwest::Client::builder();
        let proxy = HTTP_PROXY.lock().ok().and_then(|g| g.clone());
        if proxy.is_none() && REQUIRE_PROXY.load(Ordering::SeqCst) {
            return Err("proxy required".into());
        }
        if let Some(p) = proxy {
            // socks5h: the proxy resolves names too.
            let url = p.replacen("socks5://", "socks5h://", 1);
            // Loopback (a local Jackett, say) is not a leak and the proxy
            // machine's own loopback is not what the user means.
            let proxy = reqwest::Proxy::all(url)
                .map_err(|e| e.without_url().to_string())?
                .no_proxy(reqwest::NoProxy::from_string("127.0.0.1,::1,localhost"));
            b = b.proxy(proxy);
        }
        Ok(b)
    }
}
