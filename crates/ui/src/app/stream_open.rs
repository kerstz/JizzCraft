//! Opening streams in the player (feature `network`): the ticket handed back
//! by the fetch service becomes a file player, a stream reader or a timed source.

use super::downloads::open_external;
use super::{JizzApp, ToastKind};
use jizzcraft_fetch::service::StreamTicket;
use std::time::Duration;

impl JizzApp {
    /// Open a stream in the player. Name is hidden when discreet or disguised.
    pub(super) fn open_stream_player(
        &mut self,
        name: String,
        reader: jizzcraft_fetch::service::StreamReader,
        len: u64,
    ) {
        let shown = if self.discreet() || self.opts.disguised {
            "Video".to_owned()
        } else {
            name
        };
        self.close_view();
        let ctx = self.ctx.clone();
        let vol = f64::from(self.cfg.volume.unwrap_or(0.8));
        let wake = std::sync::Arc::new(move || ctx.request_repaint_after(Duration::from_millis(1)));
        let player = jizzcraft_media::player::Player::open_stream(Box::new(reader), len, vol, wake);
        let mut pv = crate::player_view::PlayerView::new(None, shown, player);
        pv.autoplay_on = false;
        // Scrub previews read the file from disk: not available while streaming.
        pv.previews_enabled = false;
        self.view = super::View::Player(Box::new(pv));
        self.autoplay_at = None;
    }

    /// Open a segmented stream (HLS or DASH) in the player.
    pub(super) fn open_timed_player(
        &mut self,
        name: String,
        source: Box<dyn jizzcraft_core::timed::TimedSource>,
    ) {
        let shown = if self.discreet() || self.opts.disguised {
            "Video".to_owned()
        } else {
            name
        };
        self.close_view();
        let ctx = self.ctx.clone();
        let vol = f64::from(self.cfg.volume.unwrap_or(0.8));
        let wake = std::sync::Arc::new(move || ctx.request_repaint_after(Duration::from_millis(1)));
        let player = jizzcraft_media::player::Player::open_timed(source, vol, wake);
        let mut pv = crate::player_view::PlayerView::new(None, shown, player);
        pv.autoplay_on = false;
        pv.previews_enabled = false;
        self.view = super::View::Player(Box::new(pv));
        self.autoplay_at = None;
    }

    /// Act on a ticket whose flow finished: file player, stream or external.
    pub(super) fn open_ticket(&mut self, t: StreamTicket, external: bool) {
        match t {
            StreamTicket::Done(path) if external => match open_external(&path) {
                Ok(()) => self.toast(ToastKind::Info, "Opened in an external player."),
                Err(m) => self.toast(ToastKind::Error, m),
            },
            StreamTicket::Live { .. } | StreamTicket::Timed { .. } if external => {
                self.toast(ToastKind::Error, "Wait until the download is complete.");
            }
            StreamTicket::Done(path) => {
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                self.open_player(None, name, &path, None);
            }
            StreamTicket::Live { reader, len, name } => self.open_stream_player(name, reader, len),
            StreamTicket::Timed { source, name } => self.open_timed_player(name, source),
        }
    }
}
