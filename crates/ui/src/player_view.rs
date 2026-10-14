//! Video player view: persistent wgpu texture updated in place, timeline,
//! volume, overlays. The pipeline itself lives in `jizzcraft-media`.

use crate::theme::{self, ACCENT, BG, DANGER, MUTED, S, TEXT};
use crate::widgets;
use egui::{Align2, Color32, CornerRadius, FontFamily, FontId, Rect, Sense, Ui, pos2, vec2};
use egui_wgpu::{RenderState, wgpu};
use jizzcraft_media::player::{Frame, PlayState, Player, PlayerError};
use jizzcraft_media::preview::{PREVIEW_W, PreviewService, bucket_for};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

const BAR_H: f32 = 56.0;

/// Live numbers of a torrent-backed stream (copied from the fetch status).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StreamFacts {
    pub paused: bool,
    pub peers: u32,
    pub down_bps: u64,
    /// `"disk full"` or `"error"` once the torrent failed.
    pub error: Option<String>,
}

/// What the overlay says while a stream waits for bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StreamNote {
    DiskFull,
    Failed,
    Paused,
    /// Nothing is arriving; the number is the peers connected right now.
    WaitingForPeers(u32),
}

impl StreamNote {
    /// The (English) text; goes through `i18n::tr_dyn`.
    pub fn text(&self) -> String {
        match self {
            StreamNote::DiskFull => "Disk full. Downloads paused.".into(),
            StreamNote::Failed => "Something went wrong.".into(),
            StreamNote::Paused => "Download paused".into(),
            StreamNote::WaitingForPeers(n) => format!("Waiting for peers ({n})"),
        }
    }
}

/// Pick the note for a stalled stream (`stalled`: loading or buffering).
/// Pure so the priority is tested: failure, then pause, then no traffic.
pub fn stream_note(f: &StreamFacts, stalled: bool) -> Option<StreamNote> {
    if !stalled {
        return None;
    }
    if let Some(e) = &f.error {
        return Some(if e == "disk full" {
            StreamNote::DiskFull
        } else {
            StreamNote::Failed
        });
    }
    if f.paused {
        return Some(StreamNote::Paused);
    }
    (f.down_bps == 0).then_some(StreamNote::WaitingForPeers(f.peers))
}

struct VideoTexture {
    texture: wgpu::Texture,
    id: egui::TextureId,
    size: [u32; 2],
}

pub struct PlayerView {
    /// Position in the filtered list (None for `--bench`).
    pub index: Option<u64>,
    pub name: String,
    pub player: Player,
    tex: Option<VideoTexture>,
    display_aspect: f32,
    pub fps: f32,
    /// Frames uploaded to the GPU (for `--bench`).
    pub uploads: u64,
    scrubbing: bool,
    pub fullscreen: bool,
    last_mouse: Instant,
    /// Library row this player shows (None for `--bench`).
    pub media_id: Option<i64>,
    /// Duration known from the library, until the pipeline reports its own.
    pub duration_hint: Option<u64>,
    /// A play has been counted in the history.
    pub counted: bool,
    pub ended_flushed: bool,
    pub last_write: Instant,
    /// Where playback was resumed from, and when (for the HUD chip).
    pub resumed: Option<(Duration, Instant)>,
    /// Marker positions (ms) drawn as ticks on the timeline.
    pub marker_ticks: Vec<u64>,
    /// Stream quality picked by the user (None = automatic).
    pub manual_variant: Option<usize>,
    /// `(path, size, mtime)` for the preview service.
    pub source: Option<(PathBuf, u64, i64)>,
    pub previews_enabled: bool,
    /// Discreet / share-safe: previews use the blurred variant.
    pub blur_previews: bool,
    preview: Option<PreviewService>,
    preview_tex: HashMap<u32, (egui::TextureHandle, egui::TextureHandle, Instant)>,
    /// Force a preview popup at this position (screenshots, `player.preview`).
    pub force_preview: Option<Duration>,
    /// Seconds until the next item starts by itself (autoplay).
    pub autoplay_in: Option<u32>,
    pub autoplay_on: bool,
    /// Commands queued by the bar's chips, drained by the app each frame.
    pub actions: Vec<(String, serde_json::Value)>,
    /// Reads the live torrent numbers of a stream (None for files).
    pub stream_probe: Option<Box<dyn Fn() -> StreamFacts + Send>>,
}

impl PlayerView {
    pub fn new(index: Option<u64>, name: String, player: Player) -> Self {
        PlayerView {
            index,
            name,
            player,
            tex: None,
            display_aspect: 16.0 / 9.0,
            fps: 0.0,
            uploads: 0,
            scrubbing: false,
            fullscreen: false,
            last_mouse: Instant::now(),
            media_id: None,
            duration_hint: None,
            counted: false,
            ended_flushed: false,
            last_write: Instant::now(),
            resumed: None,
            marker_ticks: Vec::new(),
            manual_variant: None,
            source: None,
            previews_enabled: true,
            blur_previews: false,
            preview: None,
            preview_tex: HashMap::new(),
            force_preview: None,
            autoplay_in: None,
            autoplay_on: false,
            actions: Vec::new(),
            stream_probe: None,
        }
    }

    pub fn video_size(&self) -> (u32, u32) {
        self.tex.as_ref().map_or((0, 0), |t| (t.size[0], t.size[1]))
    }

    pub fn has_frame(&self) -> bool {
        self.tex.is_some()
    }

    /// Upload the newest frame (if any) into the persistent texture.
    pub fn pump(&mut self, rs: &RenderState) {
        let Some(frame) = self.player.take_frame() else {
            return;
        };
        self.upload(rs, &frame);
    }

    fn upload(&mut self, rs: &RenderState, f: &Frame) {
        let size = [f.width(), f.height()];
        self.display_aspect = f.display_aspect();
        self.fps = f.fps();
        // Reallocate only when the resolution changes.
        if self.tex.as_ref().is_none_or(|t| t.size != size) {
            let texture = rs.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("jizzcraft-video"),
                size: wgpu::Extent3d {
                    width: size[0],
                    height: size[1],
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
            let mut renderer = rs.renderer.write();
            let id = match self.tex.take() {
                Some(old) => {
                    renderer.update_egui_texture_from_wgpu_texture(
                        &rs.device,
                        &view,
                        wgpu::FilterMode::Linear,
                        old.id,
                    );
                    old.id
                }
                None => {
                    renderer.register_native_texture(&rs.device, &view, wgpu::FilterMode::Linear)
                }
            };
            self.tex = Some(VideoTexture { texture, id, size });
        }
        let Some(t) = &self.tex else { return };
        let stride = f.stride() as u32;
        f.with_pixels(|data| {
            // Real stride, in place: no ColorImage, no per-frame allocation.
            rs.queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &t.texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                data,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(stride),
                    rows_per_image: Some(size[1]),
                },
                wgpu::Extent3d {
                    width: size[0],
                    height: size[1],
                    depth_or_array_layers: 1,
                },
            );
        });
        self.uploads += 1;
    }

    /// Free the GPU texture. Call before dropping the view.
    pub fn release(&mut self, rs: Option<&RenderState>) {
        if let (Some(rs), Some(t)) = (rs, self.tex.take()) {
            rs.renderer.write().free_texture(&t.id);
        }
    }

    /// Draw the player. Returns true when the user asked to replay.
    pub fn show(&mut self, ui: &mut Ui, discreet: bool) {
        let full = ui.max_rect();
        let moved = ui.input(|i| i.pointer.delta() != egui::Vec2::ZERO || i.pointer.any_down());
        if moved {
            self.last_mouse = Instant::now();
        }
        let state = self.player.state();
        let show_bar = !self.fullscreen
            || state != PlayState::Playing
            || self.scrubbing
            || self.last_mouse.elapsed() < Duration::from_millis(2500);
        if self.fullscreen && !show_bar {
            ui.ctx().set_cursor_icon(egui::CursorIcon::None);
        }
        let painter = ui.painter_at(full);
        painter.rect_filled(full, CornerRadius::ZERO, Color32::BLACK);

        let video_area = if show_bar {
            Rect::from_min_max(full.min, pos2(full.max.x, full.max.y - BAR_H))
        } else {
            full
        };
        // The picture itself is a control: click = play/pause, double click =
        // fullscreen. Interacted before the bar so the bar's widgets win.
        let surface = ui.interact(video_area, ui.id().with("surface"), Sense::click());
        if surface.double_clicked() {
            self.actions
                .push(("player.fullscreen".into(), serde_json::json!({})));
        } else if surface.clicked() {
            self.actions
                .push(("player.toggle".into(), serde_json::json!({})));
        }

        // Letterboxed video inside the area, honouring pixel aspect ratio.
        match &self.tex {
            Some(t) => {
                let a = self.display_aspect.max(0.01);
                let mut w = video_area.width();
                let mut h = w / a;
                if h > video_area.height() {
                    h = video_area.height();
                    w = h * a;
                }
                let r = Rect::from_center_size(video_area.center(), vec2(w, h));
                painter.image(
                    t.id,
                    r,
                    Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
                    Color32::WHITE,
                );
            }
            None => {
                if state != PlayState::Error {
                    painter.text(
                        video_area.center(),
                        Align2::CENTER_CENTER,
                        crate::i18n::tr("LOADING…"),
                        FontId::new(12.0, FontFamily::Monospace),
                        MUTED,
                    );
                }
            }
        }

        match state {
            PlayState::Error if self.player.error_kind() == Some(PlayerError::NoData) => {
                let job = theme::spaced(crate::i18n::tr("No data is arriving."), 12.0, 1.5, DANGER);
                let size = painter.layout_job(job.clone()).size();
                painter.galley(
                    video_area.center() - vec2(0.0, 36.0) - vec2(size.x / 2.0, 0.0),
                    painter.layout_job(job),
                    DANGER,
                );
                let r = Rect::from_center_size(
                    video_area.center() + vec2(0.0, 12.0),
                    vec2(140.0, 36.0),
                );
                let mut child = ui.new_child(egui::UiBuilder::new().max_rect(r));
                if widgets::button_solid(&mut child, crate::i18n::tr("Retry")).clicked() {
                    self.actions
                        .push(("fetch.play.retry".into(), serde_json::json!({})));
                }
            }
            PlayState::Error => {
                let msg = self
                    .player
                    .error()
                    .unwrap_or_else(|| "playback failed".into());
                let job =
                    theme::spaced(crate::i18n::tr("Cannot play this file"), 12.0, 1.5, DANGER);
                painter.galley(
                    video_area.center()
                        - vec2(0.0, 36.0)
                        - vec2(painter.layout_job(job.clone()).size().x / 2.0, 0.0),
                    painter.layout_job(job),
                    DANGER,
                );
                let g = painter.layout(
                    msg,
                    FontId::new(11.0, FontFamily::Monospace),
                    TEXT,
                    (video_area.width() - 96.0).clamp(120.0, 640.0),
                );
                painter.galley(video_area.center() - vec2(g.size().x / 2.0, 0.0), g, TEXT);
            }
            PlayState::Playing | PlayState::Paused | PlayState::Loading => {
                let pct = self.player.buffering_percent();
                let stalled = pct < 100 || state == PlayState::Loading;
                if let Some(f) = &self.stream_probe
                    && let Some(note) = stream_note(&f(), stalled)
                {
                    painter.text(
                        video_area.center() - vec2(0.0, 28.0),
                        Align2::CENTER_CENTER,
                        crate::i18n::tr_dyn(&note.text()).into_owned(),
                        FontId::new(12.0, FontFamily::Proportional),
                        MUTED,
                    );
                    ui.ctx().request_repaint_after(Duration::from_millis(500));
                }
                if pct < 100 {
                    painter.text(
                        video_area.center(),
                        Align2::CENTER_CENTER,
                        crate::i18n::tr_dyn(&format!("BUFFERING {pct}%")).into_owned(),
                        FontId::new(12.0, FontFamily::Monospace),
                        TEXT,
                    );
                    if let Some(q) = crate::fun::steady(crate::fun::Quip::Buffering) {
                        painter.text(
                            video_area.center() + vec2(0.0, 24.0),
                            Align2::CENTER_CENTER,
                            q,
                            FontId::new(12.0, FontFamily::Proportional),
                            MUTED,
                        );
                    }
                }
            }
            PlayState::Ended => {}
        }
        if let Some((at, when)) = self.resumed {
            if when.elapsed() < Duration::from_secs(5) && !discreet {
                let secs = at.as_secs();
                let text = format!(
                    "RESUMED {:02}:{:02}:{:02} · HOME RESTARTS",
                    secs / 3600,
                    (secs / 60) % 60,
                    secs % 60
                );
                let galley = painter.layout_job(theme::hud(&text, TEXT));
                let pos = video_area.left_top() + vec2(16.0, 16.0);
                let chip = Rect::from_min_size(pos, galley.size() + vec2(16.0, 10.0));
                painter.rect_filled(chip, CornerRadius::ZERO, BG.gamma_multiply(0.85));
                painter.galley(pos + vec2(8.0, 5.0), galley, TEXT);
                ui.ctx().request_repaint_after(Duration::from_millis(500));
            } else if when.elapsed() >= Duration::from_secs(5) {
                self.resumed = None;
            }
        }
        if state == PlayState::Ended {
            let r = Rect::from_center_size(video_area.center(), vec2(140.0, 36.0));
            let mut child = ui.new_child(egui::UiBuilder::new().max_rect(r));
            if widgets::button_solid(&mut child, crate::i18n::tr("Replay")).clicked() {
                self.player.replay();
            }
            if let Some(q) = crate::fun::steady(crate::fun::Quip::Ended) {
                painter.text(
                    video_area.center() + vec2(0.0, 72.0),
                    Align2::CENTER_CENTER,
                    q,
                    FontId::new(12.0, FontFamily::Proportional),
                    MUTED,
                );
            }
            if let Some(n) = self.autoplay_in {
                painter.text(
                    video_area.center() + vec2(0.0, 40.0),
                    Align2::CENTER_CENTER,
                    crate::i18n::tr_dyn(&format!("NEXT IN {n} · ESC CANCELS")).into_owned(),
                    FontId::new(11.0, FontFamily::Monospace),
                    MUTED,
                );
            }
        }

        if show_bar {
            self.bar(
                ui,
                Rect::from_min_max(pos2(full.min.x, full.max.y - BAR_H), full.max),
                discreet,
            );
        }
    }

    fn bar(&mut self, ui: &mut Ui, rect: Rect, discreet: bool) {
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, CornerRadius::ZERO, BG);
        painter.hline(rect.x_range(), rect.top(), theme::hairline());

        let dur = self.player.duration();
        let pos = self.player.position().min(dur.unwrap_or(Duration::MAX));
        let margin = S * 2.0;
        let track = Rect::from_min_max(
            pos2(rect.left() + margin, rect.top() + 14.0),
            pos2(rect.right() - margin, rect.top() + 17.0),
        );
        // Generous hit area around the thin 3 px track.
        let hit = track.expand2(vec2(0.0, 10.0));
        let resp = ui.interact(hit, ui.id().with("timeline"), Sense::click_and_drag());

        let frac_at = |x: f32| ((x - track.left()) / track.width()).clamp(0.0, 1.0);
        if let (Some(d), Some(p)) = (dur, resp.interact_pointer_pos()) {
            let target = d.mul_f32(frac_at(p.x));
            if resp.drag_started() {
                self.scrubbing = true;
            }
            if resp.dragged() {
                // Key-unit seeks while dragging; coalesced in the control thread.
                self.player.seek(target, false);
            }
            if resp.drag_stopped() {
                self.scrubbing = false;
                self.player.seek(target, true);
            } else if resp.clicked() {
                self.player.seek(target, true);
            }
        }

        // Track, buffered-ahead band, elapsed portion, playhead.
        painter.rect_filled(track, CornerRadius::ZERO, theme::LINE);
        if let (Some(d), Some(b)) = (dur.filter(|d| !d.is_zero()), self.player.buffered_until()) {
            let bx =
                track.left() + (b.as_secs_f32() / d.as_secs_f32()).clamp(0.0, 1.0) * track.width();
            painter.rect_filled(
                Rect::from_min_max(track.min, pos2(bx, track.max.y)),
                CornerRadius::ZERO,
                MUTED.gamma_multiply(0.35),
            );
        }
        let frac = match dur {
            Some(d) if !d.is_zero() => (pos.as_secs_f32() / d.as_secs_f32()).clamp(0.0, 1.0),
            _ => 0.0,
        };
        let x = track.left() + frac * track.width();
        painter.rect_filled(
            Rect::from_min_max(track.min, pos2(x, track.max.y)),
            CornerRadius::ZERO,
            MUTED,
        );
        // A-B loop window.
        let (la, lb) = self.player.loop_points();
        if let Some(d) = dur.filter(|d| !d.is_zero()) {
            let fx = |t: Duration| {
                track.left() + (t.as_secs_f32() / d.as_secs_f32()).clamp(0.0, 1.0) * track.width()
            };
            if la.is_some() || lb.is_some() {
                let (xa, xb) = (la.map_or(track.left(), fx), lb.map_or(track.right(), fx));
                painter.rect_filled(
                    Rect::from_min_max(
                        pos2(xa, track.top() - 3.0),
                        pos2(xb.max(xa + 2.0), track.bottom() + 3.0),
                    ),
                    CornerRadius::ZERO,
                    ACCENT.gamma_multiply(0.35),
                );
                for p in [la, lb].into_iter().flatten() {
                    painter.vline(
                        fx(p),
                        (track.top() - 5.0)..=(track.bottom() + 5.0),
                        egui::Stroke::new(1.5, theme::ACCENT_INK),
                    );
                }
            }
            // Marker ticks.
            for ms in &self.marker_ticks {
                let t = Duration::from_millis(*ms);
                painter.rect_filled(
                    Rect::from_center_size(pos2(fx(t), track.top() - 5.0), vec2(2.0, 8.0)),
                    CornerRadius::ZERO,
                    theme::APP,
                );
            }
        }
        painter.rect_filled(
            Rect::from_center_size(pos2(x, track.center().y), vec2(3.0, 14.0)),
            CornerRadius::ZERO,
            ACCENT,
        );
        // Scrub preview popup.
        let hover_at = match (dur, ui.input(|i| i.pointer.hover_pos())) {
            (Some(d), Some(h)) if resp.hovered() => Some((d, d.mul_f32(frac_at(h.x)), h.x)),
            _ => None,
        };
        let forced = dur.zip(self.force_preview).map(|(d, p)| {
            let f = (p.as_secs_f32() / d.as_secs_f32().max(0.001)).clamp(0.0, 1.0);
            (d, p, track.left() + f * track.width())
        });
        if let Some((d, t, x)) = hover_at.or(forced) {
            self.preview_popup(ui, d, t, x, track.top(), discreet);
        }
        if resp.hovered()
            && let (Some(d), Some(h)) = (dur, ui.input(|i| i.pointer.hover_pos()))
        {
            let t = fmt_clock(d.mul_f32(frac_at(h.x)));
            painter.text(
                pos2(
                    h.x.clamp(track.left() + 24.0, track.right() - 24.0),
                    track.top() - 8.0,
                ),
                Align2::CENTER_BOTTOM,
                t,
                FontId::new(10.0, FontFamily::Monospace),
                TEXT,
            );
        }

        // Time, mono: 00:12:43 / 00:41:07
        let total = dur.map_or("--:--:--".to_owned(), fmt_clock);
        painter.text(
            pos2(rect.left() + margin, rect.bottom() - 18.0),
            Align2::LEFT_CENTER,
            format!("{} / {}", fmt_clock(pos), total),
            FontId::new(12.0, FontFamily::Monospace),
            TEXT,
        );

        let state = self.player.state();
        let status = match state {
            PlayState::Playing => "PLAYING",
            PlayState::Paused => "PAUSED",
            PlayState::Ended => "ENDED",
            PlayState::Loading => "LOADING",
            PlayState::Error => "ERROR",
        };
        painter.text(
            pos2(rect.center().x, rect.bottom() - 18.0),
            Align2::CENTER_CENTER,
            status,
            FontId::new(10.0, FontFamily::Monospace),
            if state == PlayState::Error {
                DANGER
            } else {
                MUTED
            },
        );
        // Transport, right of the clock: back, play/pause, mute, fullscreen.
        let transport = Rect::from_min_max(
            pos2(rect.left() + margin + 156.0, rect.bottom() - 36.0),
            pos2(rect.left() + margin + 156.0 + 330.0, rect.bottom() - 6.0),
        );
        {
            let mut child = ui.new_child(
                egui::UiBuilder::new()
                    .max_rect(transport)
                    .layout(egui::Layout::left_to_right(egui::Align::Center)),
            );
            child.spacing_mut().item_spacing.x = 6.0;
            let playing = state == PlayState::Playing;
            if widgets::toggle_chip(&mut child, &format!("← {}", crate::i18n::tr("Back")), false)
                .clicked()
            {
                self.actions
                    .push(("viewer.close".into(), serde_json::json!({})));
            }
            let label = if playing {
                crate::i18n::tr("Pause")
            } else {
                crate::i18n::tr("Play")
            };
            if widgets::toggle_chip(&mut child, label, false).clicked() {
                self.actions
                    .push(("player.toggle".into(), serde_json::json!({})));
            }
            if widgets::toggle_chip(&mut child, crate::i18n::tr("Mute"), self.player.muted())
                .clicked()
            {
                self.actions
                    .push(("player.mute".into(), serde_json::json!({})));
            }
            if widgets::toggle_chip(&mut child, crate::i18n::tr("Fullscreen"), self.fullscreen)
                .clicked()
            {
                self.actions
                    .push(("player.fullscreen".into(), serde_json::json!({})));
            }
        }
        // Right-hand chips: speed, A-B loop, autoplay.
        let chips = Rect::from_min_max(
            pos2(transport.right() + 8.0, rect.bottom() - 36.0),
            pos2(rect.right() - margin - 130.0, rect.bottom() - 6.0),
        );
        if chips.width() > 150.0 {
            let mut child = ui.new_child(
                egui::UiBuilder::new()
                    .max_rect(chips)
                    .layout(egui::Layout::right_to_left(egui::Align::Center)),
            );
            child.spacing_mut().item_spacing.x = 6.0;
            let rate = self.player.rate();
            if widgets::toggle_chip(&mut child, crate::i18n::tr("Auto"), self.autoplay_on)
                .on_hover_text(crate::i18n::tr("Play the next item when this one ends"))
                .clicked()
            {
                self.actions
                    .push(("player.autoplay.toggle".into(), serde_json::json!({})));
            }
            // Streams: quality and live state.
            let variants = self.player.variants();
            if variants.len() > 1 {
                let label = match self.manual_variant.and_then(|i| variants.get(i)) {
                    Some(v) => v.height.map_or_else(|| "?".to_owned(), |h| format!("{h}p")),
                    None => match self.player.current_variant().and_then(|i| variants.get(i)) {
                        Some(v) => v
                            .height
                            .map_or("AUTO".to_owned(), |h| format!("AUTO · {h}p")),
                        None => "AUTO".to_owned(),
                    },
                };
                if widgets::toggle_chip(&mut child, &label, self.manual_variant.is_some())
                    .on_hover_text(crate::i18n::tr("Stream quality: click to cycle"))
                    .clicked()
                {
                    self.actions
                        .push(("player.quality.next".into(), serde_json::json!({})));
                }
            }
            if self.player.is_live() {
                child.label(
                    egui::RichText::new("LIVE")
                        .color(theme::ACCENT_INK)
                        .background_color(ACCENT),
                );
            }
            let looping = la.is_some() || lb.is_some();
            let label = match (la.is_some(), lb.is_some()) {
                (true, true) => "A-B",
                (true, false) => "A-",
                _ => "A-B",
            };
            if widgets::toggle_chip(&mut child, label, looping)
                .on_hover_text(crate::i18n::tr("Set loop start, then end, then clear"))
                .clicked()
            {
                self.actions.push((
                    "player.loop".into(),
                    serde_json::json!({ "action": "cycle" }),
                ));
            }
            if widgets::toggle_chip(
                &mut child,
                &format!("{rate:.2}x"),
                (rate - 1.0).abs() > 0.01,
            )
            .on_hover_text(crate::i18n::tr("Playback speed (click to cycle)"))
            .clicked()
            {
                self.actions
                    .push(("player.speed".into(), serde_json::json!({ "cycle": true })));
            }
        }
        let vol = if self.player.muted() {
            "MUTED".to_owned()
        } else {
            format!("VOL {:.0}%", self.player.volume() * 100.0)
        };
        // Volume: the label is also a slider (click or drag sideways).
        let vol_rect = Rect::from_min_max(
            pos2(rect.right() - margin - 120.0, rect.bottom() - 36.0),
            pos2(rect.right() - margin, rect.bottom() - 6.0),
        );
        let vr = ui.interact(vol_rect, ui.id().with("volume"), Sense::click_and_drag());
        if (vr.clicked() || vr.dragged())
            && let Some(p) = vr.interact_pointer_pos()
        {
            let v = ((p.x - vol_rect.left()) / vol_rect.width()).clamp(0.0, 1.0);
            self.actions.push((
                "player.volume".into(),
                serde_json::json!({ "value": f64::from(v) }),
            ));
        }
        let level = if self.player.muted() {
            0.0
        } else {
            self.player.volume() as f32
        };
        let bar_y = rect.bottom() - 10.0;
        let line = Rect::from_min_max(
            pos2(vol_rect.left(), bar_y - 1.0),
            pos2(vol_rect.right(), bar_y + 1.0),
        );
        painter.rect_filled(line, CornerRadius::ZERO, theme::LINE);
        painter.rect_filled(
            Rect::from_min_max(
                line.min,
                pos2(
                    line.left() + line.width() * level.clamp(0.0, 1.0),
                    line.max.y,
                ),
            ),
            CornerRadius::ZERO,
            MUTED,
        );
        painter.text(
            pos2(vol_rect.right(), rect.bottom() - 24.0),
            Align2::RIGHT_CENTER,
            vol,
            FontId::new(11.0, FontFamily::Monospace),
            if self.player.muted() { DANGER } else { TEXT },
        );
    }
}

impl PlayerView {
    /// Small frame popup above the timeline. The decoder runs on its own
    /// thread; until a frame arrives only the time label shows.
    fn preview_popup(
        &mut self,
        ui: &Ui,
        dur: Duration,
        at: Duration,
        x: f32,
        track_top: f32,
        discreet: bool,
    ) {
        if !self.previews_enabled {
            return;
        }
        let bucket = bucket_for(at, dur);
        if self.preview.is_none()
            && let Some((path, size, mtime)) = &self.source
        {
            let ctx = ui.ctx().clone();
            let wake: Arc<dyn Fn() + Send + Sync> =
                Arc::new(move || ctx.request_repaint_after(Duration::from_millis(1)));
            self.preview = Some(PreviewService::new(
                path.clone(),
                *size,
                *mtime,
                jizzcraft_core::paths::cache_dir(),
                wake,
            ));
        }
        if let Some(svc) = &self.preview {
            svc.request(bucket, at);
            if let Some(f) = svc.take() {
                let mk = |name: String, img: &jizzcraft_media::service::Rgba| {
                    ui.ctx().load_texture(
                        name,
                        egui::ColorImage::from_rgba_unmultiplied(
                            [img.w as usize, img.h as usize],
                            &img.data,
                        ),
                        egui::TextureOptions::LINEAR,
                    )
                };
                let full = mk(format!("pv-{}", f.bucket), &f.full);
                let blur = mk(format!("pvb-{}", f.bucket), &f.blur);
                self.preview_tex
                    .insert(f.bucket, (full, blur, Instant::now()));
                // Keep the texture cache small. GPU memory is a rental, not a home.
                while self.preview_tex.len() > 32 {
                    if let Some(k) = self
                        .preview_tex
                        .iter()
                        .min_by_key(|(_, v)| v.2)
                        .map(|(k, _)| *k)
                    {
                        self.preview_tex.remove(&k);
                    }
                }
            }
        }
        let painter = ui.ctx().layer_painter(egui::LayerId::new(
            egui::Order::Foreground,
            egui::Id::new("scrub-preview"),
        ));
        let tex = self.preview_tex.get(&bucket).or_else(|| {
            // Nearest cached bucket while the exact one is decoding. Close enough
            // is a valid strategy for horseshoes and scrub previews.
            self.preview_tex
                .iter()
                .min_by_key(|(k, _)| k.abs_diff(bucket))
                .map(|(_, v)| v)
        });
        let w = PREVIEW_W as f32;
        let h = w * 9.0 / 16.0;
        let cx = x.clamp(
            ui.max_rect().left() + w / 2.0 + 8.0,
            ui.max_rect().right() - w / 2.0 - 8.0,
        );
        let r = Rect::from_min_size(pos2(cx - w / 2.0, track_top - h - 32.0), vec2(w, h));
        painter.rect_filled(r.expand(1.0), CornerRadius::ZERO, theme::LINE_STRONG);
        painter.rect_filled(r, CornerRadius::ZERO, theme::SUNKEN);
        if let Some((full, blur, _)) = tex {
            let t = if discreet || self.blur_previews {
                blur
            } else {
                full
            };
            painter.image(
                t.id(),
                r,
                Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
                Color32::WHITE,
            );
        }
        let mut label = fmt_clock(at);
        if let Some(ms) = self
            .marker_ticks
            .iter()
            .find(|ms| Duration::from_millis(**ms).abs_diff(at) < Duration::from_secs(2))
            && !discreet
        {
            label.push_str(&format!(
                "  · MARKER {}",
                fmt_clock(Duration::from_millis(*ms))
            ));
        }
        painter.text(
            pos2(cx, r.bottom() + 6.0),
            Align2::CENTER_TOP,
            label,
            FontId::new(10.0, FontFamily::Monospace),
            TEXT,
        );
    }
}

pub fn fmt_clock(d: Duration) -> String {
    let s = d.as_secs();
    format!("{:02}:{:02}:{:02}", s / 3600, (s / 60) % 60, s % 60)
}

#[cfg(test)]
mod stream_note_tests {
    use super::*;

    fn f(paused: bool, peers: u32, bps: u64, error: Option<&str>) -> StreamFacts {
        StreamFacts {
            paused,
            peers,
            down_bps: bps,
            error: error.map(str::to_owned),
        }
    }

    #[test]
    fn nothing_while_data_flows_or_not_stalled() {
        assert_eq!(stream_note(&f(false, 4, 1000, None), true), None);
        assert_eq!(stream_note(&f(true, 0, 0, Some("error")), false), None);
    }

    #[test]
    fn priority_error_then_pause_then_peers() {
        assert_eq!(
            stream_note(&f(true, 0, 0, Some("disk full")), true),
            Some(StreamNote::DiskFull)
        );
        assert_eq!(
            stream_note(&f(false, 0, 0, Some("error")), true),
            Some(StreamNote::Failed)
        );
        assert_eq!(
            stream_note(&f(true, 3, 0, None), true),
            Some(StreamNote::Paused)
        );
        assert_eq!(
            stream_note(&f(false, 2, 0, None), true),
            Some(StreamNote::WaitingForPeers(2))
        );
    }

    #[test]
    fn texts() {
        assert_eq!(StreamNote::Paused.text(), "Download paused");
        assert_eq!(
            StreamNote::WaitingForPeers(5).text(),
            "Waiting for peers (5)"
        );
    }
}
