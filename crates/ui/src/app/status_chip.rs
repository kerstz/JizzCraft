//! The connection chip in the top bar (online edition only).
//!
//! It reads the saved configuration and makes no network call. The only call
//! is the user's click on "Test the proxy": it connects to the proxy itself
//! and checks that it answers a SOCKS5 greeting. Nothing is relayed anywhere.
//! In discreet mode the chip never shows an interface name.
#![cfg(feature = "network")]

use super::JizzApp;
use crate::i18n::{tr, trf};
use crate::widgets;
use jizzcraft_fetch::service::{Connection, ProxyProbe, connection_state, probe_proxy};
use std::sync::mpsc::{Receiver, TryRecvError, channel};

/// Text of the chip. Interface names only outside discreet modes.
pub(super) fn label(c: &Connection, discreet: bool) -> String {
    match c {
        Connection::Protected(_) if discreet => tr("Protected").to_owned(),
        Connection::Protected(i) => trf("Protected: {}", &[i]),
        Connection::ProxySet => tr("Proxy set").to_owned(),
        Connection::NotProtected => tr("Not protected").to_owned(),
        Connection::Blocked => tr("Blocked").to_owned(),
    }
}

/// One line saying what the state means.
pub(super) fn explain(c: &Connection) -> &'static str {
    match c {
        Connection::Protected(_) => "Connections use the interface you chose.",
        Connection::ProxySet => "Connections go through your proxy.",
        Connection::NotProtected => "No proxy or interface is set: connections go out directly.",
        Connection::Blocked => "The kill switch is on and no proxy is set: nothing connects.",
    }
}

pub(super) fn probe_sentence(p: ProxyProbe) -> &'static str {
    match p {
        ProxyProbe::Answers => "The proxy answered.",
        ProxyProbe::Unreachable => "The proxy did not answer.",
        ProxyProbe::NotSocks5 => "That does not look like a SOCKS5 proxy.",
        ProxyProbe::BadAddress => "The proxy address is not valid.",
    }
}

#[derive(Default)]
pub(super) struct ProxyTest {
    job: Option<Receiver<ProxyProbe>>,
    pub(super) last: Option<ProxyProbe>,
}

impl ProxyTest {
    pub(super) fn busy(&self) -> bool {
        self.job.is_some()
    }

    pub(super) fn start(&mut self, proxy: String, wake: Box<dyn Fn() + Send>) {
        if self.job.is_some() {
            return;
        }
        let (tx, rx) = channel();
        self.last = None;
        self.job = Some(rx);
        std::thread::spawn(move || {
            let _ = tx.send(probe_proxy(&proxy));
            wake();
        });
    }

    pub(super) fn poll(&mut self) {
        if let Some(rx) = &self.job {
            match rx.try_recv() {
                Ok(p) => {
                    self.last = Some(p);
                    self.job = None;
                }
                Err(TryRecvError::Disconnected) => {
                    self.last = Some(ProxyProbe::Unreachable);
                    self.job = None;
                }
                Err(TryRecvError::Empty) => {}
            }
        }
    }
}

/// The chip itself. A click opens Settings on the connection page.
pub(super) fn show(app: &mut JizzApp, ui: &mut egui::Ui) {
    let state = connection_state(app.downloads.host.config());
    let discreet = app.discreet() || app.opts.disguised;
    let on = matches!(state, Connection::Protected(_) | Connection::ProxySet);
    if widgets::toggle_chip(ui, &label(&state, discreet), on)
        .on_hover_text(tr(explain(&state)))
        .clicked()
    {
        app.push("app.settings", serde_json::json!({ "tab": "connection" }));
    }
}

/// `net.proxy.test`: only on the user's click (Settings, Connection).
pub(super) fn register(reg: &mut jizzcraft_core::Registry<JizzApp>) {
    reg.register(
        "net.proxy.test",
        "Check that the configured proxy answers a SOCKS5 greeting (nothing is sent through it)",
        |app, _| {
            let proxy = app
                .downloads
                .host
                .config()
                .proxy
                .as_ref()
                .filter(|p| !p.is_empty())
                .map(|p| p.expose().to_owned())
                .ok_or_else(|| jizzcraft_core::Error::Invalid("no proxy is set".into()))?;
            let ctx = app.ctx.clone();
            app.proxy_test
                .start(proxy, Box::new(move || ctx.request_repaint()));
            Ok(serde_json::json!({ "started": true }))
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chip_state_table() {
        // The language is process-global and other tests change it: compare with
        // the translated text of the same keys, never with a fixed English string.
        let tun = Connection::Protected("tun0".into());
        assert_eq!(label(&tun, false), trf("Protected: {}", &["tun0"]));
        assert_eq!(label(&tun, true), tr("Protected"));
        assert_eq!(label(&Connection::ProxySet, false), tr("Proxy set"));
        assert_eq!(label(&Connection::NotProtected, true), tr("Not protected"));
        assert_eq!(label(&Connection::Blocked, false), tr("Blocked"));
        // The discreet label never shows the interface name.
        assert!(!label(&tun, true).contains("tun0"));
        for c in [
            Connection::ProxySet,
            Connection::NotProtected,
            Connection::Blocked,
        ] {
            assert!(!label(&c, true).contains("tun0"));
        }
    }

    #[test]
    fn probe_sentences_are_fixed() {
        for p in [
            ProxyProbe::Answers,
            ProxyProbe::Unreachable,
            ProxyProbe::NotSocks5,
            ProxyProbe::BadAddress,
        ] {
            assert!(probe_sentence(p).ends_with('.'));
        }
    }

    #[test]
    fn a_bad_address_is_reported_without_a_connection() {
        let mut t = ProxyTest::default();
        t.start("not-a-proxy".into(), Box::new(|| {}));
        for _ in 0..200 {
            t.poll();
            if !t.busy() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert_eq!(t.last, Some(ProxyProbe::BadAddress));
    }
}
