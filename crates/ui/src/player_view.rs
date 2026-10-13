//! Video player view: persistent wgpu texture updated in place, timeline,
//! volume, overlays. The pipeline itself lives in `jizzcraft-media`.

use crate::theme::{self, ACCENT, BG, DANGER, MUTED, S, TEXT};
use crate::widgets;
use egui::{Align2, Color32, CornerRadius, FontFamily, FontId, Rect, Sense, Ui, pos2, vec2};
use egui_wgpu::{RenderState, wgpu};
use jizzcraft_media::player::{Frame, PlayState, Player};
use std::time::{Duration, Instant};

const BAR_H: f32 = 56.0;

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
                        "LOADING…",
                        FontId::new(12.0, FontFamily::Monospace),
                        MUTED,
                    );
                }
            }
        }

        match state {
            PlayState::Error => {
                let msg = self
                    .player
                    .error()
                    .unwrap_or_else(|| "playback failed".into());
                let job = theme::spaced("Cannot play this file", 12.0, 1.5, DANGER);
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
                if pct < 100 {
                    painter.text(
                        video_area.center(),
                        Align2::CENTER_CENTER,
                        format!("BUFFERING {pct}%"),
                        FontId::new(12.0, FontFamily::Monospace),
                        TEXT,
                    );
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
            if widgets::button_solid(&mut child, "Replay").clicked() {
                self.player.replay();
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

    fn bar(&mut self, ui: &mut Ui, rect: Rect, _discreet: bool) {
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

        // Track, elapsed portion, playhead.
        painter.rect_filled(track, CornerRadius::ZERO, theme::LINE);
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
        painter.rect_filled(
            Rect::from_center_size(pos2(x, track.center().y), vec2(3.0, 14.0)),
            CornerRadius::ZERO,
            ACCENT,
        );
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
        let vol = if self.player.muted() {
            "MUTED".to_owned()
        } else {
            format!("VOL {:.0}%", self.player.volume() * 100.0)
        };
        painter.text(
            pos2(rect.right() - margin, rect.bottom() - 18.0),
            Align2::RIGHT_CENTER,
            vol,
            FontId::new(11.0, FontFamily::Monospace),
            if self.player.muted() { DANGER } else { TEXT },
        );
    }
}

pub fn fmt_clock(d: Duration) -> String {
    let s = d.as_secs();
    format!("{:02}:{:02}:{:02}", s / 3600, (s / 60) % 60, s % 60)
}
