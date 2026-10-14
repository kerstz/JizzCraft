//! Proxy state shared by the static request helpers.

use super::FetchService;
use crate::config::FetchConfig;
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

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

/// Hosts of the indexers the user typed in. They may sit on the LAN under a
/// public-looking name (`jackett.example.org` with split DNS); every other
/// public name must resolve to public addresses.
static TRUSTED_HOSTS: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// The system resolver, for [`PublicOnly`].
fn system_lookup(host: &str) -> std::io::Result<Vec<IpAddr>> {
    use std::net::ToSocketAddrs;
    Ok((host, 0).to_socket_addrs()?.map(|a| a.ip()).collect())
}

/// What a name may resolve to. A name on the user's network (`nas`,
/// `jackett.lan`, an indexer they configured) may answer with private
/// addresses; any other name may not. Without this a public origin could
/// answer `127.0.0.1` for its own name after the vetting (DNS rebinding) and
/// aim the app at the LAN. Not needed under SOCKS5h: the proxy resolves.
pub(crate) fn allowed_answers(
    name: &str,
    answers: Vec<IpAddr>,
    trusted: &[String],
) -> Result<Vec<IpAddr>, &'static str> {
    let n = name.trim_end_matches('.').to_ascii_lowercase();
    if crate::sources::is_intranet_name(&n) || trusted.contains(&n) {
        return Ok(answers);
    }
    let public: Vec<IpAddr> = answers
        .into_iter()
        .filter(|ip| crate::sources::is_public_ip(*ip))
        .collect();
    if public.is_empty() {
        Err("refused: private address")
    } else {
        Ok(public)
    }
}

type Lookup = dyn Fn(&str) -> std::io::Result<Vec<IpAddr>> + Send + Sync;

/// A DNS resolver for `reqwest` that applies [`allowed_answers`].
pub(crate) struct PublicOnly {
    lookup: Arc<Lookup>,
}

impl PublicOnly {
    pub(crate) fn system() -> Self {
        Self::with_lookup(system_lookup)
    }

    pub(crate) fn with_lookup(
        f: impl Fn(&str) -> std::io::Result<Vec<IpAddr>> + Send + Sync + 'static,
    ) -> Self {
        Self {
            lookup: Arc::new(f),
        }
    }
}

impl reqwest::dns::Resolve for PublicOnly {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let host = name.as_str().to_owned();
        let lookup = self.lookup.clone();
        let trusted = TRUSTED_HOSTS.lock().map(|g| g.clone()).unwrap_or_default();
        Box::pin(async move {
            let found = tokio::task::spawn_blocking(move || {
                let ips = lookup(&host).map_err(|_| "name not found")?;
                allowed_answers(&host, ips, &trusted)
            })
            .await
            .map_err(|_| "name not found")?;
            let ips = found?;
            let addrs: reqwest::dns::Addrs =
                Box::new(ips.into_iter().map(|ip| SocketAddr::new(ip, 0)));
            Ok(addrs)
        })
    }
}

pub(super) fn publish_proxy(cfg: &FetchConfig) {
    if let Ok(mut g) = TRUSTED_HOSTS.lock() {
        *g = cfg
            .indexers
            .iter()
            .map(|i| {
                crate::sources::host_of(&i.url)
                    .trim_end_matches('.')
                    .to_ascii_lowercase()
            })
            .collect();
    }
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
        let mut b = reqwest::Client::builder().dns_resolver(Arc::new(PublicOnly::system()));
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::atomic::AtomicUsize;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn public_names_cannot_answer_inward() {
        let bad: Vec<IpAddr> = [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.9",
            "192.168.0.1",
            "169.254.169.254",
            "100.64.0.1",
            "0.0.0.0",
            "::1",
            "fd00::1",
            "fe80::1",
            "::ffff:127.0.0.1",
            "64:ff9b::7f00:1",
            "2002:c0a8:101::1",
        ]
        .into_iter()
        .map(ip)
        .collect();
        for a in &bad {
            assert!(
                allowed_answers("cdn.example.invalid", vec![*a], &[]).is_err(),
                "{a}"
            );
        }
        // Mixed answers keep only the public ones.
        let mixed = vec![ip("127.0.0.1"), ip("93.184.216.34"), ip("::1")];
        let ok = allowed_answers("cdn.example.invalid", mixed, &[]).unwrap();
        assert_eq!(ok, vec![ip("93.184.216.34")]);
        // LAN names and the user's own indexer hosts may be private.
        assert!(allowed_answers("nas", vec![ip("192.168.1.2")], &[]).is_ok());
        assert!(allowed_answers("jackett.lan", vec![ip("10.0.0.2")], &[]).is_ok());
        assert!(allowed_answers("LOCALHOST", vec![ip("127.0.0.1")], &[]).is_ok());
        let mine = vec!["jackett.example.invalid".to_owned()];
        assert!(allowed_answers("jackett.example.invalid", vec![ip("10.0.0.2")], &mine).is_ok());
    }

    /// Through a real client: the rebinding name is refused before any
    /// connection, the LAN name connects.
    #[test]
    fn rebinding_answer_never_connects() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let hits = Arc::new(AtomicUsize::new(0));
        let h2 = hits.clone();
        std::thread::spawn(move || {
            for s in l.incoming() {
                let Ok(mut s) = s else { continue };
                h2.fetch_add(1, Ordering::SeqCst);
                let mut b = [0u8; 1024];
                let _ = s.read(&mut b);
                let _ = s.write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok",
                );
            }
        });
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let client = reqwest::Client::builder()
            .dns_resolver(Arc::new(PublicOnly::with_lookup(|_| {
                Ok(vec!["127.0.0.1".parse().unwrap()])
            })))
            .build()
            .unwrap();
        let (evil, lan) = rt.block_on(async {
            let evil = client
                .get(format!("http://rebind.example.invalid:{port}/"))
                .send()
                .await;
            let after_evil = hits.load(Ordering::SeqCst);
            let lan = client.get(format!("http://nas:{port}/")).send().await;
            (evil.is_err().then_some(after_evil), lan.is_ok())
        });
        assert_eq!(evil, Some(0), "the rebinding name must not connect");
        assert!(lan, "a LAN name keeps working");
    }
}
