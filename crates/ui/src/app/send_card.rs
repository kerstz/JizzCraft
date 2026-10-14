//! "Send to JizzCraft": addresses handed over by another launch
//! (`jizzcraft --send URL`) wait on a confirmation card. The running app never
//! plays one by itself. Cards live in memory only, at most five, and the boss
//! key / panic clears them. Offline edition: this module does not exist.
#![cfg(feature = "network")]

use super::JizzApp;
use crate::theme::MUTED;
use crate::widgets;
use egui::RichText;
use serde_json::json;
use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// Cards waiting for an answer. More are dropped, oldest kept.
pub(super) const MAX_CARDS: usize = 5;

/// Pending addresses (normalized, already vetted). Never persisted.
#[derive(Default)]
pub(super) struct SendCards {
    pending: VecDeque<String>,
    checked: Option<Instant>,
}

impl SendCards {
    /// Vet and queue an address. `false` = refused or queue full.
    pub(super) fn offer(&mut self, url: &str) -> bool {
        let Ok(u) = jizzcraft_fetch::inbox::vet_link(url) else {
            return false;
        };
        if self.pending.len() >= MAX_CARDS || self.pending.contains(&u) {
            return false;
        }
        self.pending.push_back(u);
        true
    }

    pub(super) fn len(&self) -> usize {
        self.pending.len()
    }

    /// The address on the front card, if any.
    pub(super) fn front(&self) -> Option<&str> {
        self.pending.front().map(String::as_str)
    }

    /// Answer the front card. `Some(url)` only for a click on Play.
    pub(super) fn answer(&mut self, play: bool) -> Option<String> {
        let u = self.pending.pop_front()?;
        play.then_some(u)
    }

    /// Boss key, panic: forget every card and stop polling for a while.
    pub(super) fn cancel_pending(&mut self) {
        self.pending.clear();
        self.checked = Some(Instant::now() + Duration::from_secs(3600));
    }
}

/// Host only, no port, path or query: all a card ever shows.
pub(super) fn host_of(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let auth = rest.split(['/', '?', '#']).next().unwrap_or("");
    match auth.strip_prefix('[') {
        Some(v6) => format!("[{}]", v6.split(']').next().unwrap_or("")),
        None => auth.split(':').next().unwrap_or("").to_owned(),
    }
}

/// Sentence on the card title and body, host only outside discreet modes.
pub(super) fn card_text(url: &str, discreet: bool) -> (&'static str, Option<String>) {
    if discreet {
        ("Play a received address?", None)
    } else {
        ("Play this address?", Some(host_of(url)))
    }
}

/// Once per frame: pick up handed-over addresses (about once a second), show
/// the front card, run `stream.open` only on a click on Play.
pub(super) fn poll_and_show(app: &mut JizzApp, ctx: &egui::Context) {
    let due = app
        .send_cards
        .checked
        .is_none_or(|t| t.elapsed() >= Duration::from_secs(1));
    if due {
        app.send_cards.checked = Some(Instant::now());
        for u in jizzcraft_fetch::inbox::take_links() {
            app.send_cards.offer(&u);
        }
    }
    let Some(url) = app.send_cards.front().map(str::to_owned) else {
        return;
    };
    let discreet = app.discreet() || app.opts.disguised;
    let (title, host) = card_text(&url, discreet);
    let more = app.send_cards.len() - 1;
    let mut answer = None;
    egui::Window::new(crate::i18n::tr(title))
        .id(egui::Id::new("send-to-jizzcraft-card"))
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .collapsible(false)
        .resizable(false)
        .show(ctx, |ui| {
            if let Some(h) = host {
                ui.label(RichText::new(h).strong());
            }
            if more > 0 {
                ui.label(
                    RichText::new(crate::i18n::trf("{} more waiting", &[&more.to_string()]))
                        .color(MUTED),
                );
            }
            ui.horizontal(|ui| {
                if widgets::button_solid(ui, crate::i18n::tr("Play")).clicked() {
                    answer = Some(true);
                }
                if widgets::button_outline(ui, crate::i18n::tr("Dismiss")).clicked() {
                    answer = Some(false);
                }
            });
        });
    if let Some(play) = answer
        && let Some(u) = app.send_cards.answer(play)
    {
        app.push("stream.open", json!({ "url": u }));
    }
    ctx.request_repaint_after(Duration::from_millis(500));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_plays_without_a_click() {
        let mut c = SendCards::default();
        assert!(c.offer("https://example.com/v.m3u8"));
        assert_eq!(c.len(), 1);
        assert_eq!(c.answer(false), None);
        assert_eq!(c.len(), 0);
        assert!(c.offer("https://example.com/v.m3u8"));
        assert_eq!(
            c.answer(true).as_deref(),
            Some("https://example.com/v.m3u8")
        );
    }

    #[test]
    fn at_most_five_and_junk_refused() {
        let mut c = SendCards::default();
        for i in 0..8 {
            c.offer(&format!("https://example.com/{i}"));
        }
        assert_eq!(c.len(), MAX_CARDS);
        let mut d = SendCards::default();
        for bad in [
            "javascript:alert(1)",
            "file:///etc/passwd",
            "https://u:p@example.com/",
            "http://localhost/",
        ] {
            assert!(!d.offer(bad));
        }
        assert_eq!(d.len(), 0);
    }

    #[test]
    fn cancel_pending_clears_cards() {
        let mut c = SendCards::default();
        c.offer("https://example.com/a");
        c.offer("https://example.com/b");
        c.cancel_pending();
        assert_eq!(c.len(), 0);
        assert!(c.front().is_none());
    }

    #[test]
    fn card_shows_host_only_and_nothing_when_discreet() {
        let u = "https://cdn.example.com:8443/secret/path?q=1";
        assert_eq!(host_of(u), "cdn.example.com");
        assert_eq!(host_of("https://[2606:4700::1]:443/v"), "[2606:4700::1]");
        let (t, h) = card_text(u, false);
        assert_eq!(t, "Play this address?");
        assert_eq!(h.as_deref(), Some("cdn.example.com"));
        let (t, h) = card_text(u, true);
        assert_eq!(t, "Play a received address?");
        assert!(h.is_none());
    }
}
