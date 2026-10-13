//! Application shell: state, command registry wiring, panels.

use crate::bench::{self, BenchState};
use crate::config::Config;
use crate::player_view::PlayerView;
use crate::theme::{
    self, ACCENT_INK, APP, BG, DANGER, FAINT, LINE, LINE_STRONG, MUTED, RAISED, S, SUNKEN, TEXT,
};
use crate::widgets;
use egui::{
    Align, Align2, Color32, CornerRadius, FontFamily, FontId, Layout, Rect, Sense, Stroke,
    StrokeKind, TextureHandle, Ui, UiBuilder, vec2,
};
use jizzcraft_core::scan::ScanReport;
use jizzcraft_core::{
    Db, Filter, HasLibrary, Media, MediaKind, Registry, SortKey, Stats, paths, register_core,
};
use jizzcraft_media::player::{PlayState, Player};
use jizzcraft_media::service::{Request, Thumb, ThumbService};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};

const PEACH_PNG: &[u8] = include_bytes!("../../../assets/icons/jizzcraft-512.png");
const VIEWER_PNG: &[u8] = include_bytes!("../../../assets/icons/viewer-256.png");

const TILE_W: f32 = 184.0;

#[derive(Debug, Clone, Default)]
pub struct Options {
    pub db_path: Option<PathBuf>,
    /// Render one frame to this PNG and exit (README screenshots).
    pub screenshot: Option<PathBuf>,
    /// Import this folder at startup (dev and screenshots).
    pub import: Option<PathBuf>,
    /// How many times the word "know" appears in the headline (easter egg).
    pub know: u32,
    /// Ephemeral config (never written to disk) instead of the user's.
    pub config: Option<Config>,
    /// Pre-select this tab ("library", "images", "videos", "favorites").
    pub tab: Option<String>,
    /// Open the n-th item of the current list on startup.
    pub open_index: Option<u64>,
    /// Commands to run once the startup import (if any) has finished.
    pub commands: Vec<(String, Value)>,
    /// Play this file for N seconds and print a performance report.
    pub bench: Option<(PathBuf, u32)>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tab {
    Library,
    Images,
    Videos,
    Favorites,
}

impl Tab {
    const ALL: [Tab; 4] = [Tab::Library, Tab::Images, Tab::Videos, Tab::Favorites];

    fn label(self) -> &'static str {
        match self {
            Tab::Library => "Library",
            Tab::Images => "Images",
            Tab::Videos => "Videos",
            Tab::Favorites => "Favorites",
        }
    }

    fn parse(s: &str) -> Option<Tab> {
        Tab::ALL
            .into_iter()
            .find(|t| t.label().eq_ignore_ascii_case(s))
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ToastKind {
    Info,
    Error,
}

struct Toast {
    text: String,
    kind: ToastKind,
    until: Instant,
}

enum JobMsg {
    Scan(Result<(i64, ScanReport), String>),
    Folder(Option<PathBuf>),
    Image {
        token: u64,
        result: Result<egui::ColorImage, String>,
    },
    Meta(i64, jizzcraft_core::MediaMeta),
}

const MAX_TEXTURES: usize = 256;
/// Largest side uploaded to the GPU for the viewer (wgpu default limit is 8192+).
const MAX_VIEW_SIDE: u32 = 8192;

#[derive(Clone, Copy, PartialEq, Debug)]
enum Zoom {
    Fit,
    Scale(f32),
}

struct ImageView {
    /// Position in the filtered list.
    index: u64,
    name: String,
    token: u64,
    tex: Option<TextureHandle>,
    size: [u32; 2],
    error: Option<String>,
    zoom: Zoom,
    pan: egui::Vec2,
}

enum View {
    Library,
    Player(Box<PlayerView>),
    Image(Box<ImageView>),
}

struct ThumbTex {
    full: TextureHandle,
    blur: TextureHandle,
    size: [f32; 2],
    used: u64,
}

#[derive(Clone, Copy)]
enum ThumbState<'a> {
    Ready(&'a ThumbTex),
    Loading,
    Failed,
}

/// Window of the current list kept in memory (never the whole library).
#[derive(Default)]
struct Page {
    generation: u64,
    offset: u64,
    items: Vec<Media>,
}

pub struct JizzApp {
    ctx: egui::Context,
    db: Db,
    db_path: PathBuf,
    registry: Rc<Registry<JizzApp>>,
    cfg: Config,
    persist_cfg: bool,
    opts: Options,

    view: View,
    view_token: u64,
    render_state: Option<egui_wgpu::RenderState>,
    bench: Option<BenchState>,
    tab: Tab,
    search: String,
    selected: Option<i64>,
    selected_index: Option<u64>,
    sort: SortKey,
    sort_desc: bool,
    last_cols: usize,
    scroll_to: Option<u64>,
    grid_scroll: (f32, f32),
    focus_search: bool,
    stats: Stats,
    total: u64,
    generation: u64,
    page: Page,

    tx: Sender<JobMsg>,
    rx: Receiver<JobMsg>,
    busy: u32,
    toasts: Vec<Toast>,
    show_docs: bool,
    queue: Vec<(String, Value)>,

    thumbs: ThumbService,
    textures: HashMap<i64, ThumbTex>,
    thumb_failed: HashSet<i64>,
    meta_tried: HashSet<i64>,
    icon_peach: Arc<egui::IconData>,
    icon_viewer: Arc<egui::IconData>,
    peach: TextureHandle,
    viewer_icon: TextureHandle,
    last_title: String,
    frame_no: u64,
    screenshot_sent: bool,
    idle_frames: u32,
}

impl HasLibrary for JizzApp {
    fn db(&mut self) -> &mut Db {
        &mut self.db
    }
}

fn window_icon(bytes: &[u8]) -> Arc<egui::IconData> {
    let img = image::load_from_memory(bytes)
        .expect("bundled icon decodes")
        .to_rgba8();
    Arc::new(egui::IconData {
        width: img.width(),
        height: img.height(),
        rgba: img.into_raw(),
    })
}

fn load_icon(ctx: &egui::Context, name: &str, bytes: &[u8]) -> TextureHandle {
    let img = image::load_from_memory(bytes)
        .expect("bundled icon decodes")
        .to_rgba8();
    let size = [img.width() as usize, img.height() as usize];
    ctx.load_texture(
        name,
        egui::ColorImage::from_rgba_unmultiplied(size, img.as_raw()),
        egui::TextureOptions::LINEAR,
    )
}

impl JizzApp {
    pub fn new(cc: &eframe::CreationContext<'_>, opts: Options) -> Result<Self, String> {
        theme::apply(&cc.egui_ctx);
        let db_path = opts.db_path.clone().unwrap_or_else(paths::db_path);
        let db = Db::open(&db_path).map_err(|e| format!("cannot open library: {e}"))?;
        let (tx, rx) = channel();
        let persist_cfg = opts.config.is_none();
        let cfg = opts.config.clone().unwrap_or_else(Config::load);

        let mut registry = Registry::new();
        register_core::<JizzApp>(&mut registry);
        register_ui_commands(&mut registry);

        let mut app = JizzApp {
            ctx: cc.egui_ctx.clone(),
            db,
            db_path,
            registry: Rc::new(registry),
            cfg,
            persist_cfg,
            view: View::Library,
            view_token: 0,
            render_state: cc.wgpu_render_state.clone(),
            bench: None,
            tab: opts
                .tab
                .as_deref()
                .and_then(Tab::parse)
                .unwrap_or(Tab::Library),
            search: String::new(),
            selected: None,
            selected_index: None,
            sort: SortKey::default(),
            sort_desc: false,
            last_cols: 1,
            scroll_to: None,
            grid_scroll: (0.0, 0.0),
            focus_search: false,
            stats: Stats::default(),
            total: 0,
            generation: 1,
            page: Page::default(),
            tx,
            rx,
            busy: 0,
            toasts: Vec::new(),
            show_docs: false,
            queue: Vec::new(),
            thumbs: {
                let ctx = cc.egui_ctx.clone();
                ThumbService::new(paths::cache_dir(), Arc::new(move || ctx.request_repaint()))
            },
            textures: HashMap::new(),
            thumb_failed: HashSet::new(),
            meta_tried: HashSet::new(),
            icon_peach: window_icon(PEACH_PNG),
            icon_viewer: window_icon(VIEWER_PNG),
            peach: load_icon(&cc.egui_ctx, "peach", PEACH_PNG),
            viewer_icon: load_icon(&cc.egui_ctx, "viewer", VIEWER_PNG),
            last_title: String::new(),
            frame_no: 0,
            screenshot_sent: false,
            idle_frames: 0,
            opts,
        };
        app.sort = app
            .cfg
            .sort
            .as_deref()
            .and_then(SortKey::parse)
            .unwrap_or_default();
        app.sort_desc = app.cfg.sort_desc;
        app.refresh();
        if let Some((file, secs)) = app.opts.bench.clone() {
            let name = file
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            app.open_player(None, name, &file);
            app.bench = Some(BenchState::new(file, secs));
        } else if let Some(dir) = app.opts.import.clone() {
            app.start_scan(Some(dir));
        } else {
            if let Some(idx) = app.opts.open_index {
                app.open_index(idx);
            }
            for (name, args) in std::mem::take(&mut app.opts.commands) {
                app.queue.push((name, args));
            }
        }
        Ok(app)
    }

    // ---- state -----------------------------------------------------------

    fn filter(&self) -> Filter {
        Filter {
            kind: match self.tab {
                Tab::Images => Some(MediaKind::Image),
                Tab::Videos => Some(MediaKind::Video),
                _ => None,
            },
            favorites: self.tab == Tab::Favorites,
            name: Some(self.search.clone()).filter(|s| !s.trim().is_empty()),
            include_missing: true,
            sort: self.sort,
            desc: self.sort_desc,
            ..Default::default()
        }
    }

    /// Invalidate cached counts and page after any change to the library or filter.
    fn refresh(&mut self) {
        self.generation += 1;
        self.stats = self.db.stats().unwrap_or_default();
        self.total = self.db.count(&self.filter()).unwrap_or(0);
    }

    fn ensure_page(&mut self, start: u64, end: u64) {
        let have = self.page.generation == self.generation
            && start >= self.page.offset
            && end <= self.page.offset + self.page.items.len() as u64;
        if have {
            return;
        }
        let pad = 24;
        let offset = start.saturating_sub(pad);
        let limit = (end - start) + pad * 2;
        let f = Filter {
            limit: Some(limit),
            offset,
            ..self.filter()
        };
        self.page = Page {
            generation: self.generation,
            offset,
            items: self.db.query(&f).unwrap_or_default(),
        };
    }

    fn selected_media(&self) -> Option<Media> {
        let id = self.selected?;
        self.db.get(id).ok().flatten()
    }

    /// Select the item at `idx` and scroll it into view.
    fn select_index(&mut self, idx: u64) {
        if self.total == 0 {
            return;
        }
        let idx = idx.min(self.total - 1);
        self.ensure_page(idx, idx + 1);
        if let Some(m) = self.page.items.get((idx - self.page.offset) as usize) {
            self.selected = Some(m.id);
            self.selected_index = Some(idx);
            self.scroll_to = Some(idx);
        }
    }

    pub fn toast(&mut self, kind: ToastKind, text: impl Into<String>) {
        self.toasts.push(Toast {
            text: text.into(),
            kind,
            until: Instant::now() + Duration::from_secs(5),
        });
    }

    fn discreet(&self) -> bool {
        self.cfg.discreet
    }

    fn save_cfg(&self) {
        if self.persist_cfg {
            self.cfg.save();
        }
    }

    // ---- commands --------------------------------------------------------

    pub fn run(&mut self, name: &str, args: Value) {
        let reg = Rc::clone(&self.registry);
        match reg.run(self, name, args) {
            Ok(_) => {}
            Err(e) => self.toast(ToastKind::Error, format!("{name}: {e}")),
        }
    }

    /// Open the item at `index` of the current list: images in the viewer.
    fn open_index(&mut self, index: u64) {
        if index >= self.total {
            return;
        }
        self.ensure_page(index, index + 1);
        let Some(m) = self
            .page
            .items
            .get((index - self.page.offset) as usize)
            .cloned()
        else {
            return;
        };
        self.selected = Some(m.id);
        if m.missing {
            self.toast(ToastKind::Error, "File is missing on disk");
            return;
        }
        match m.kind {
            MediaKind::Image => {
                self.view_token += 1;
                let token = self.view_token;
                self.view = View::Image(Box::new(ImageView {
                    index,
                    name: m.name.clone(),
                    token,
                    tex: None,
                    size: [0, 0],
                    error: None,
                    zoom: Zoom::Fit,
                    pan: egui::Vec2::ZERO,
                }));
                let tx = self.tx.clone();
                let ctx = self.ctx.clone();
                let path = m.path.clone();
                std::thread::spawn(move || {
                    let result = decode_for_view(&path);
                    let _ = tx.send(JobMsg::Image { token, result });
                    ctx.request_repaint();
                });
            }
            MediaKind::Video => {
                if m.duration_ms.is_none() {
                    let (tx, ctx, path, id) =
                        (self.tx.clone(), self.ctx.clone(), m.path.clone(), m.id);
                    std::thread::spawn(move || {
                        if let Ok(meta) = jizzcraft_media::probe::probe(&path) {
                            let _ = tx.send(JobMsg::Meta(id, meta));
                            ctx.request_repaint();
                        }
                    });
                }
                let name = m.name.clone();
                self.open_player(Some(index), name, &m.path);
            }
        }
    }

    fn open_player(&mut self, index: Option<u64>, name: String, path: &std::path::Path) {
        self.close_view();
        let ctx = self.ctx.clone();
        let vol = f64::from(self.cfg.volume.unwrap_or(0.8));
        // egui turns every zero-delay `request_repaint()` into two repaints;
        // a 1 ms delay gives exactly one pass per decoded frame.
        let wake = Arc::new(move || ctx.request_repaint_after(Duration::from_millis(1)));
        let player = Player::open(path, vol, wake);
        self.view = View::Player(Box::new(PlayerView::new(index, name, player)));
    }

    /// Leave the current view, freeing its GPU resources. Dropping a `Player`
    /// only sends a message: pipeline teardown happens on its own thread.
    fn close_view(&mut self) {
        if let View::Player(pv) = &mut self.view {
            pv.release(self.render_state.as_ref());
            if pv.fullscreen {
                self.ctx
                    .send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
            }
        }
        self.view = View::Library;
    }

    fn step_view(&mut self, delta: i64) {
        let cur = match &self.view {
            View::Image(iv) => Some(iv.index),
            View::Player(pv) => pv.index,
            View::Library => None,
        };
        let Some(cur) = cur else { return };
        let next = cur as i64 + delta;
        if next >= 0 && (next as u64) < self.total {
            self.open_index(next as u64);
        }
    }

    fn start_scan(&mut self, dir: Option<PathBuf>) {
        let tx = self.tx.clone();
        let ctx = self.ctx.clone();
        let db_path = self.db_path.clone();
        self.busy += 1;
        std::thread::spawn(move || {
            let result = (|| {
                let mut db = Db::open(&db_path).map_err(|e| e.to_string())?;
                match dir {
                    Some(d) => {
                        jizzcraft_core::scan::import_folder(&mut db, &d).map_err(|e| e.to_string())
                    }
                    None => jizzcraft_core::scan::rescan_all(&mut db)
                        .map(|r| (0, r))
                        .map_err(|e| e.to_string()),
                }
            })();
            let _ = tx.send(JobMsg::Scan(result));
            ctx.request_repaint();
        });
    }

    fn pick_folder(&mut self) {
        let tx = self.tx.clone();
        let ctx = self.ctx.clone();
        std::thread::spawn(move || {
            let folder = rfd::FileDialog::new()
                .set_title("Import folder")
                .pick_folder();
            let _ = tx.send(JobMsg::Folder(folder));
            ctx.request_repaint();
        });
    }

    fn poll_jobs(&mut self) {
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                JobMsg::Folder(Some(dir)) => self.start_scan(Some(dir)),
                JobMsg::Folder(None) => {}
                JobMsg::Meta(id, meta) => {
                    if self.db.set_meta(id, &meta).is_ok() {
                        self.generation += 1;
                    }
                }
                JobMsg::Image { token, result } => {
                    if let View::Image(iv) = &mut self.view
                        && iv.token == token
                    {
                        match result {
                            Ok(img) => {
                                iv.size = [img.size[0] as u32, img.size[1] as u32];
                                let opts = egui::TextureOptions {
                                    magnification: egui::TextureFilter::Nearest,
                                    minification: egui::TextureFilter::Linear,
                                    ..Default::default()
                                };
                                iv.tex = Some(self.ctx.load_texture("viewer-image", img, opts));
                            }
                            Err(e) => iv.error = Some(e),
                        }
                    }
                }
                JobMsg::Scan(res) => {
                    self.busy = self.busy.saturating_sub(1);
                    match res {
                        Ok((_, r)) => {
                            let mut text = format!("{} added · {} unchanged", r.added, r.unchanged);
                            if r.marked_missing > 0 {
                                text += &format!(" · {} missing", r.marked_missing);
                            }
                            if r.errors > 0 {
                                text += &format!(" · {} unreadable", r.errors);
                            }
                            let kind = if r.errors > 0 {
                                ToastKind::Error
                            } else {
                                ToastKind::Info
                            };
                            self.toast(kind, text);
                            self.refresh();
                            if let Some(idx) = self.opts.open_index.take() {
                                self.open_index(idx);
                            }
                            for (name, args) in std::mem::take(&mut self.opts.commands) {
                                self.queue.push((name, args));
                            }
                        }
                        Err(e) => self.toast(ToastKind::Error, format!("Import failed: {e}")),
                    }
                }
            }
        }
    }

    /// Upload finished thumbnails (bounded per frame) and store probed metadata.
    fn poll_thumbs(&mut self, ctx: &egui::Context) {
        let mut meta_changed = false;
        for _ in 0..8 {
            let Some(done) = self.thumbs.try_recv() else {
                break;
            };
            if let Some(meta) = &done.meta
                && self.db.set_meta(done.id, meta).is_ok()
            {
                meta_changed = true;
            }
            match done.thumb {
                Ok(t) => {
                    let tex = upload_thumb(ctx, done.id, &t, self.frame_no);
                    self.textures.insert(done.id, tex);
                }
                Err(_) => {
                    self.thumb_failed.insert(done.id);
                }
            }
        }
        if meta_changed {
            self.generation += 1;
        }
        if self.textures.len() > MAX_TEXTURES {
            let mut by_age: Vec<(u64, i64)> =
                self.textures.iter().map(|(id, t)| (t.used, *id)).collect();
            by_age.sort_unstable();
            for (_, id) in by_age
                .into_iter()
                .take(self.textures.len() - MAX_TEXTURES + 32)
            {
                self.textures.remove(&id);
            }
        }
        if self.thumbs.is_busy() {
            ctx.request_repaint_after(Duration::from_millis(60));
        }
    }

    fn drain_queue(&mut self) {
        for (name, args) in std::mem::take(&mut self.queue) {
            self.run(&name, args);
        }
    }

    fn push(&mut self, name: &str, args: Value) {
        self.queue.push((name.to_owned(), args));
    }

    // ---- frame -----------------------------------------------------------

    fn handle_shortcuts(&mut self, ctx: &egui::Context) {
        use egui::{Key, KeyboardShortcut, Modifiers};
        let boss = KeyboardShortcut::new(Modifiers::COMMAND | Modifiers::SHIFT, Key::H);
        let import = KeyboardShortcut::new(Modifiers::COMMAND, Key::O);
        let rescan = KeyboardShortcut::new(Modifiers::COMMAND, Key::R);
        let (b, i, r) = ctx.input_mut(|s| {
            (
                s.consume_shortcut(&boss),
                s.consume_shortcut(&import),
                s.consume_shortcut(&rescan),
            )
        });
        if matches!(self.view, View::Image(_)) && !ctx.egui_wants_keyboard_input() {
            use egui::Key;
            let (esc, left, right, k0, k1, plus, minus, _) = ctx.input(|i| {
                (
                    i.key_pressed(Key::Escape),
                    i.key_pressed(Key::ArrowLeft),
                    i.key_pressed(Key::ArrowRight),
                    i.key_pressed(Key::Num0),
                    i.key_pressed(Key::Num1),
                    i.key_pressed(Key::Plus) || i.key_pressed(Key::Equals),
                    i.key_pressed(Key::Minus),
                    false,
                )
            });
            if esc {
                self.push("viewer.close", json!({}));
            }
            if left {
                self.push("viewer.step", json!({ "delta": -1 }));
            }
            if right {
                self.push("viewer.step", json!({ "delta": 1 }));
            }
            if k0 {
                self.push("viewer.zoom", json!({ "mode": "fit" }));
            }
            if k1 {
                self.push("viewer.zoom", json!({ "mode": "actual" }));
            }
            if plus {
                self.push("viewer.zoom", json!({ "mode": "in" }));
            }
            if minus {
                self.push("viewer.zoom", json!({ "mode": "out" }));
            }
            let fav =
                ctx.input(|i| i.key_pressed(if self.cfg.left_hand { Key::F } else { Key::S }));
            if fav && let Some(id) = self.selected {
                self.push("media.favorite.toggle", json!({ "id": id }));
            }
            if self.cfg.left_hand {
                let k = |key| ctx.input(|i| i.key_pressed(key));
                if k(Key::Q) {
                    self.push("viewer.close", json!({}));
                }
                if k(Key::A) {
                    self.push("viewer.step", json!({ "delta": -1 }));
                }
                if k(Key::D) {
                    self.push("viewer.step", json!({ "delta": 1 }));
                }
                if k(Key::W) {
                    self.push("viewer.zoom", json!({ "mode": "in" }));
                }
                if k(Key::S) {
                    self.push("viewer.zoom", json!({ "mode": "out" }));
                }
                if k(Key::Z) {
                    self.push("viewer.zoom", json!({ "mode": "fit" }));
                }
            }
        }
        let lh = self.cfg.left_hand;
        if matches!(self.view, View::Library) && !ctx.egui_wants_keyboard_input() {
            use egui::Key;
            let pressed = |k: Key| ctx.input(|i| i.key_pressed(k));
            let (left, right, up, down, open) = if lh {
                (
                    pressed(Key::A) || pressed(Key::ArrowLeft),
                    pressed(Key::D) || pressed(Key::ArrowRight),
                    pressed(Key::W) || pressed(Key::ArrowUp),
                    pressed(Key::S) || pressed(Key::ArrowDown),
                    pressed(Key::E) || pressed(Key::Space) || pressed(Key::Enter),
                )
            } else {
                (
                    pressed(Key::ArrowLeft),
                    pressed(Key::ArrowRight),
                    pressed(Key::ArrowUp),
                    pressed(Key::ArrowDown),
                    pressed(Key::Enter),
                )
            };
            for (hit, dx, dy) in [(left, -1, 0), (right, 1, 0), (up, 0, -1), (down, 0, 1)] {
                if hit {
                    self.push("view.select.move", json!({ "dx": dx, "dy": dy }));
                }
            }
            if open {
                self.push("view.open_selected", json!({}));
            }
            if pressed(Key::Home) {
                self.push("view.select.edge", json!({ "to": "first" }));
            }
            if pressed(Key::End) {
                self.push("view.select.edge", json!({ "to": "last" }));
            }
            if pressed(Key::Slash) {
                self.push("view.search.focus", json!({}));
            }
            if (pressed(Key::Escape) || (lh && pressed(Key::Q))) && !self.search.is_empty() {
                self.push("view.search.clear", json!({}));
            }
            if pressed(Key::Questionmark) {
                self.push("docs.toggle", json!({}));
            }
            for (key, tab) in [
                (Key::Num1, "library"),
                (Key::Num2, "images"),
                (Key::Num3, "videos"),
                (Key::Num4, "favorites"),
            ] {
                if pressed(key) {
                    self.push("view.filter", json!({ "tab": tab }));
                }
            }
            let fav = if lh { Key::F } else { Key::S };
            if pressed(fav)
                && let Some(id) = self.selected
            {
                self.push("media.favorite.toggle", json!({ "id": id }));
            }
        }
        if matches!(self.view, View::Player(_)) && !ctx.egui_wants_keyboard_input() {
            use egui::Key;
            let k = ctx.input(|i| {
                (
                    i.key_pressed(Key::Space),
                    i.key_pressed(Key::ArrowLeft),
                    i.key_pressed(Key::ArrowRight),
                    i.key_pressed(Key::ArrowUp),
                    i.key_pressed(Key::ArrowDown),
                    i.key_pressed(Key::M),
                    i.key_pressed(Key::F),
                    i.key_pressed(Key::Escape),
                    i.key_pressed(Key::S),
                    i.key_pressed(Key::PageUp),
                    i.key_pressed(Key::PageDown),
                )
            });
            let fullscreen = matches!(&self.view, View::Player(pv) if pv.fullscreen);
            if k.0 {
                self.push("player.toggle", json!({}));
            }
            if k.1 {
                self.push("player.seek", json!({ "delta": -5 }));
            }
            if k.2 {
                self.push("player.seek", json!({ "delta": 5 }));
            }
            if k.3 {
                self.push("player.volume", json!({ "delta": 0.05 }));
            }
            if k.4 {
                self.push("player.volume", json!({ "delta": -0.05 }));
            }
            if k.5 {
                self.push("player.mute", json!({}));
            }
            if k.6 && !self.cfg.left_hand {
                self.push("player.fullscreen", json!({}));
            }
            if k.7 {
                self.push(
                    if fullscreen {
                        "player.fullscreen"
                    } else {
                        "viewer.close"
                    },
                    json!({}),
                );
            }
            if k.8
                && !self.cfg.left_hand
                && let Some(id) = self.selected
            {
                self.push("media.favorite.toggle", json!({ "id": id }));
            }
            if k.9 {
                self.push("viewer.step", json!({ "delta": -1 }));
            }
            if k.10 {
                self.push("viewer.step", json!({ "delta": 1 }));
            }
            if self.cfg.left_hand {
                let (a, d, w, s, x, g, f, q, shift) = ctx.input(|i| {
                    (
                        i.key_pressed(Key::A),
                        i.key_pressed(Key::D),
                        i.key_pressed(Key::W),
                        i.key_pressed(Key::S),
                        i.key_pressed(Key::X),
                        i.key_pressed(Key::G),
                        i.key_pressed(Key::F),
                        i.key_pressed(Key::Q),
                        i.modifiers.shift,
                    )
                });
                if a || d {
                    let sign = if d { 1 } else { -1 };
                    if shift {
                        self.push("viewer.step", json!({ "delta": sign }));
                    } else {
                        self.push("player.seek", json!({ "delta": 5 * sign }));
                    }
                }
                if w {
                    self.push("player.volume", json!({ "delta": 0.05 }));
                }
                if s {
                    self.push("player.volume", json!({ "delta": -0.05 }));
                }
                if x {
                    self.push("player.mute", json!({}));
                }
                if g {
                    self.push("player.fullscreen", json!({}));
                }
                if f && let Some(id) = self.selected {
                    self.push("media.favorite.toggle", json!({ "id": id }));
                }
                if q {
                    self.push("viewer.close", json!({}));
                }
            }
        }
        if b {
            self.push("app.boss_key", json!({}));
        }
        if i {
            self.push("library.import", json!({}));
        }
        if r {
            self.push("library.rescan", json!({}));
        }
    }

    fn update_title(&mut self, ctx: &egui::Context) {
        let title = if self.discreet() {
            "Viewer"
        } else {
            "JizzCraft"
        };
        if self.last_title != title {
            let icon = if self.discreet() {
                &self.icon_viewer
            } else {
                &self.icon_peach
            };
            ctx.send_viewport_cmd(egui::ViewportCommand::Icon(Some(icon.clone())));
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.into()));
            self.last_title = title.into();
        }
    }

    fn drive_bench(&mut self, ctx: &egui::Context, t0: Instant) {
        let Some(b) = &mut self.bench else { return };
        let View::Player(pv) = &self.view else { return };
        ctx.request_repaint_after(Duration::from_millis(250));
        if let Some(last) = b.last_start.replace(t0)
            && b.started.is_some()
        {
            b.interval_ms
                .push(t0.duration_since(last).as_secs_f32() * 1000.0);
        }
        if b.started.is_none() {
            // Skip start-up (clock sync, first-frame hiccups).
            if pv.has_frame() && pv.player.position() > Duration::from_millis(1500) {
                let c = pv.player.counters();
                b.started = Some(bench::Started {
                    at: Instant::now(),
                    cpu0: bench::cpu_seconds_of("self").unwrap_or(0.0),
                    delivered0: c.delivered,
                    uploads0: pv.uploads,
                    qos0: c.qos_dropped,
                });
            }
            return;
        }
        b.ui_cost_ms.push(t0.elapsed().as_secs_f32() * 1000.0);
        let st = b.started.as_ref().expect("started");
        if st.at.elapsed().as_secs() < u64::from(b.secs) {
            return;
        }
        let wall = st.at.elapsed().as_secs_f64();
        let c = pv.player.counters();
        let mut cost = std::mem::take(&mut b.ui_cost_ms);
        let mut interval = std::mem::take(&mut b.interval_ms);
        cost.sort_by(f32::total_cmp);
        interval.sort_by(f32::total_cmp);
        let stats = |v: &[f32]| {
            [
                bench::percentile(v, 0.5),
                bench::percentile(v, 0.95),
                v.last().copied().unwrap_or(0.0),
            ]
        };
        let fps = pv.fps;
        let report = bench::Report {
            width: 0,
            height: 0,
            fps,
            wall,
            expected: f64::from(fps) * wall,
            delivered: c.delivered - st.delivered0,
            uploaded: pv.uploads - st.uploads0,
            qos_dropped: c.qos_dropped.saturating_sub(st.qos0),
            ui_passes: cost.len(),
            ui_cost: stats(&cost),
            interval: stats(&interval),
            cpu_pct: (bench::cpu_seconds_of("self").unwrap_or(0.0) - st.cpu0) / wall * 100.0,
            rss: bench::rss_mb_of("self").unwrap_or((0.0, 0.0)),
        };
        let (w, h) = pv.video_size();
        let report = bench::Report {
            width: w,
            height: h,
            ..report
        };
        report.print("jizzcraft");
        self.bench = None;
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }

    fn drive_screenshot(&mut self, ctx: &egui::Context) {
        let Some(path) = self.opts.screenshot.clone() else {
            return;
        };
        ctx.request_repaint();
        let shot = ctx.input(|i| {
            i.events.iter().find_map(|e| match e {
                egui::Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })
        });
        if let Some(img) = shot {
            let [w, h] = img.size;
            let mut raw = Vec::with_capacity(w * h * 4);
            for p in &img.pixels {
                raw.extend_from_slice(&[p.r(), p.g(), p.b(), 255]);
            }
            match image::RgbaImage::from_raw(w as u32, h as u32, raw).map(|i| i.save(&path)) {
                Some(Ok(())) => eprintln!("screenshot written"),
                _ => eprintln!("screenshot failed"),
            }
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        } else if !self.screenshot_sent
            && self.frame_no > 12
            && self.busy == 0
            && !self.thumbs.is_busy()
            && match &self.view {
                View::Player(pv) => {
                    pv.has_frame() && pv.player.position() > Duration::from_millis(400)
                }
                _ => true,
            }
        {
            // Let the last uploaded textures reach the screen before shooting.
            self.idle_frames += 1;
            if self.idle_frames < 6 {
                return;
            }
            self.screenshot_sent = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::default()));
        }
    }

    // ---- panels ----------------------------------------------------------

    fn top_bar(&mut self, ui: &mut Ui) {
        egui::Panel::top("top_bar")
            .exact_size(48.0)
            .frame(
                egui::Frame::new()
                    .fill(BG)
                    .inner_margin(egui::Margin::ZERO)
                    .stroke(Stroke::NONE),
            )
            .show(ui, |ui| {
                let bar = ui.max_rect();
                let narrow = bar.width() < 1000.0;
                ui.spacing_mut().item_spacing.x = 0.0;
                ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                    // Logo cell, closed by a hairline.
                    ui.add_space(16.0);
                    if self.discreet() {
                        ui.label(egui::RichText::new("Viewer").color(MUTED).size(14.0));
                    } else {
                        widgets::badge(ui, "01", 24.0);
                        ui.add_space(10.0);
                        widgets::wordmark(ui, 24.0);
                    }
                    ui.add_space(16.0);
                    let x = ui.cursor().left();
                    ui.painter().vline(x, bar.y_range(), Stroke::new(1.0, LINE));
                    ui.add_space(8.0);
                    for t in Tab::ALL {
                        if widgets::nav_tab(ui, t.label(), self.tab == t).clicked() {
                            self.push("view.filter", json!({ "tab": t.label() }));
                        }
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if widgets::button_solid_bar(ui, "Import folder").clicked() {
                            self.push("library.import", json!({}));
                        }
                        let x = ui.cursor().right();
                        ui.painter().vline(x, bar.y_range(), Stroke::new(1.0, LINE));
                        ui.add_space(12.0);
                        if narrow {
                            let discreet = self.discreet();
                            let share = self.cfg.screen_share_safe;
                            ui.menu_button(theme::hud("Privacy", MUTED), |ui| {
                                if ui.selectable_label(discreet, "Discreet").clicked() {
                                    self.push("app.discreet.toggle", json!({}));
                                    ui.close();
                                }
                                if ui.selectable_label(share, "Share-safe").clicked() {
                                    self.push("app.share_safe.toggle", json!({}));
                                    ui.close();
                                }
                            });
                        } else {
                            if widgets::toggle_chip(ui, "Discreet", self.discreet()).clicked() {
                                self.push("app.discreet.toggle", json!({}));
                            }
                            ui.add_space(8.0);
                            if widgets::toggle_chip(ui, "Share-safe", self.cfg.screen_share_safe)
                                .on_hover_text("Blur every thumbnail until hovered")
                                .clicked()
                            {
                                self.push("app.share_safe.toggle", json!({}));
                            }
                        }
                        ui.add_space(12.0);
                        let w = (ui.available_width() - S).clamp(120.0, 260.0);
                        let resp = ui.add_sized(
                            [w, 28.0],
                            egui::TextEdit::singleline(&mut self.search)
                                .hint_text("Search")
                                .id(egui::Id::new("search"))
                                .margin(egui::Margin::symmetric(8, 6)),
                        );
                        if std::mem::take(&mut self.focus_search) {
                            resp.request_focus();
                        }
                        if resp.changed() {
                            self.refresh();
                        }
                    });
                });
            });
        // Hairline under the bar.
        let r = ui.min_rect();
        ui.painter()
            .hline(r.x_range(), r.top() + 48.0, theme::hairline());
    }

    fn breadcrumb(&mut self, ui: &mut Ui) {
        egui::Panel::top("breadcrumb")
            .exact_size(40.0)
            .frame(
                egui::Frame::new()
                    .fill(BG)
                    .inner_margin(egui::Margin::symmetric(24, 0)),
            )
            .show(ui, |ui| {
                ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                    let brand = if self.discreet() {
                        "VIEWER"
                    } else {
                        "JIZZCRAFT"
                    };
                    let mut crumb = format!("01 / {brand} / {}", self.tab.label());
                    if let View::Player(pv) = &self.view {
                        let name = if self.cfg.discreet {
                            "…"
                        } else {
                            pv.name.as_str()
                        };
                        crumb += &format!(" / {name}");
                        if let Some(i) = pv.index {
                            crumb += &format!("  [{}/{}]", i + 1, self.total);
                        }
                    }
                    if let View::Image(iv) = &self.view {
                        let name = if self.cfg.discreet {
                            "…"
                        } else {
                            iv.name.as_str()
                        };
                        crumb += &format!(" / {name}  [{}/{}]", iv.index + 1, self.total);
                    }
                    widgets::mono_label(ui, &crumb, MUTED);
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if self.stats.total > 0 {
                            let on = self.cfg.inspector_open;
                            if widgets::toggle_chip(ui, "Inspector", on).clicked() {
                                self.push("inspector.toggle", json!({}));
                            }
                            let arrow = if self.sort_desc { "↓" } else { "↑" };
                            let label = format!("Sort: {} {arrow}", self.sort.as_str());
                            let mut pick: Option<SortKey> = None;
                            ui.menu_button(theme::hud(&label, MUTED), |ui| {
                                for k in SortKey::ALL {
                                    if ui.selectable_label(self.sort == k, k.as_str()).clicked() {
                                        pick = Some(k);
                                        ui.close();
                                    }
                                }
                            });
                            if let Some(k) = pick {
                                let desc = if k == self.sort {
                                    !self.sort_desc
                                } else {
                                    false
                                };
                                self.push("view.sort", json!({ "by": k.as_str(), "desc": desc }));
                            }
                            let mut text = format!("{} items", self.total);
                            if self.busy > 0 {
                                text += " · Scanning…";
                            }
                            widgets::mono_label(ui, &text, FAINT);
                        }
                    });
                });
            });
    }

    fn inspector(&mut self, ui: &mut Ui) {
        if !self.cfg.inspector_open || self.stats.total == 0 {
            return;
        }
        let media = self.selected_media();
        let discreet = self.discreet();
        egui::Panel::right("inspector")
            .default_size(272.0)
            .size_range(220.0..=420.0)
            .resizable(true)
            .frame(
                egui::Frame::new()
                    .fill(BG)
                    .inner_margin(egui::Margin::same(S as i8 * 2))
                    .stroke(theme::hairline()),
            )
            .show(ui, |ui| {
                widgets::mono_label(ui, "Inspector", MUTED);
                ui.add_space(S);
                widgets::hairline_h(ui);
                ui.add_space(S);
                let Some(m) = media else {
                    widgets::mono_label(ui, "Select an item, then press Enter to open", FAINT);
                    return;
                };
                widgets::kv(ui, "Name", if discreet { "—" } else { &m.name });
                let path = m.path.to_string_lossy();
                let hide_path = discreet || self.opts.screenshot.is_some();
                widgets::kv(ui, "Path", if hide_path { "hidden" } else { &path });
                widgets::kv(ui, "Type", m.kind.label());
                widgets::kv(ui, "Size", &fmt_size(m.size));
                widgets::kv(
                    ui,
                    "Resolution",
                    &match (m.width, m.height) {
                        (Some(w), Some(h)) => format!("{w} × {h}"),
                        _ => "—".into(),
                    },
                );
                widgets::kv(
                    ui,
                    "Duration",
                    &m.duration_ms.map_or("—".into(), fmt_duration),
                );
                widgets::kv(ui, "Codec", m.codec.as_deref().unwrap_or("—"));
                widgets::kv(ui, "Added", &fmt_date(m.added_at));
                widgets::kv(ui, "Favorite", if m.favorite { "yes" } else { "no" });
                if m.missing {
                    ui.add_space(S);
                    ui.label(theme::spaced("File missing on disk", 10.0, 1.2, DANGER));
                }
                ui.add_space(S * 2.0);
                let label = if m.favorite { "Unfavorite" } else { "Favorite" };
                if widgets::button_outline(ui, label).clicked() {
                    self.push("media.favorite.toggle", json!({ "id": m.id }));
                }
            });
    }

    fn central(&mut self, ui: &mut Ui) {
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(BG))
            .show(ui, |ui| {
                if let View::Player(pv) = &mut self.view {
                    pv.show(ui, self.cfg.discreet);
                } else if matches!(self.view, View::Image(_)) {
                    self.image_view(ui);
                } else if self.stats.total == 0 {
                    self.empty_state(ui);
                } else if self.total == 0 {
                    self.no_results(ui);
                } else {
                    self.grid(ui);
                }
            });
    }

    fn no_results(&mut self, ui: &mut Ui) {
        let rect = ui.max_rect();
        ui.painter().text(
            rect.center(),
            Align2::CENTER_CENTER,
            "NO MATCHING ITEMS",
            FontId::new(11.0, FontFamily::Monospace),
            FAINT,
        );
    }

    fn grid(&mut self, ui: &mut Ui) {
        let avail = ui.available_width();
        let cols = (((avail + 1.0) / (TILE_W + 1.0)).floor() as usize).max(1);
        let tile_w = ((avail - (cols as f32 - 1.0)) / cols as f32).floor();
        let tile_h = (tile_w * 0.62 + 44.0).round();
        let total = self.total as usize;
        let rows = total.div_ceil(cols);
        let discreet = self.discreet();
        let mut open: Option<usize> = None;
        let mut select: Option<(i64, u64)> = None;
        let mut fav_toggle: Option<i64> = None;
        let mut wanted: Vec<Request> = Vec::new();
        let hide_all = discreet;
        let share_safe = self.cfg.screen_share_safe;
        let frame_no = self.frame_no;

        self.last_cols = cols;
        let mut area = egui::ScrollArea::vertical().auto_shrink(false);
        if let Some(target) = self.scroll_to.take() {
            let (offset, view_h) = self.grid_scroll;
            let top = (target as usize / cols) as f32 * tile_h;
            if top < offset {
                area = area.vertical_scroll_offset(top);
            } else if top + tile_h > offset + view_h {
                area = area.vertical_scroll_offset(top + tile_h - view_h);
            }
        }
        let out = area.show_rows(ui, tile_h, rows, |ui, range| {
            ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
            let start = (range.start * cols) as u64;
            let end = ((range.end * cols).min(total)) as u64;
            self.ensure_page(start, end);
            for row in range {
                ui.horizontal(|ui| {
                    for col in 0..cols {
                        let idx = (row * cols + col) as u64;
                        if idx >= self.total {
                            break;
                        }
                        let Some(m) = self.page.items.get((idx - self.page.offset) as usize) else {
                            break;
                        };
                        let state = match self.textures.get_mut(&m.id) {
                            Some(t) => {
                                t.used = frame_no;
                                ThumbState::Ready(&*t)
                            }
                            None if self.thumb_failed.contains(&m.id) || m.missing => {
                                ThumbState::Failed
                            }
                            None => {
                                wanted.push(Request {
                                    id: m.id,
                                    path: m.path.clone(),
                                    kind: m.kind,
                                    size: m.size,
                                    mtime: m.mtime,
                                    need_meta: m.kind == MediaKind::Video
                                        && m.duration_ms.is_none()
                                        && !self.meta_tried.contains(&m.id),
                                });
                                ThumbState::Loading
                            }
                        };
                        let (resp, star) = draw_tile(
                            ui,
                            m,
                            self.selected == Some(m.id),
                            (hide_all, share_safe),
                            state,
                            frame_no,
                            (vec2(tile_w, tile_h), idx),
                        );
                        if star {
                            fav_toggle = Some(m.id);
                        } else if resp.clicked() {
                            select = Some((m.id, idx));
                        }
                        if resp.double_clicked() {
                            open = Some(idx as usize);
                        }
                    }
                });
            }
        });
        self.grid_scroll = (out.state.offset.y, out.inner_rect.height());
        self.thumbs.set_wanted(wanted);
        if let Some(id) = fav_toggle {
            self.push("media.favorite.toggle", json!({ "id": id }));
        }
        if let Some((id, idx)) = select {
            self.selected = Some(id);
            self.selected_index = Some(idx);
        }
        if let Some(idx) = open {
            self.push("media.open", json!({ "index": idx }));
        }
    }

    fn image_view(&mut self, ui: &mut Ui) {
        let rect = ui.max_rect();
        let resp = ui.allocate_rect(rect, Sense::click_and_drag());
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, CornerRadius::ZERO, BG);
        let View::Image(iv) = &mut self.view else {
            return;
        };

        match (&iv.tex, &iv.error) {
            (_, Some(e)) => {
                painter.text(
                    rect.center(),
                    Align2::CENTER_CENTER,
                    format!("CANNOT DISPLAY THIS IMAGE\n{}", e.to_uppercase()),
                    FontId::new(12.0, FontFamily::Monospace),
                    DANGER,
                );
            }
            (None, None) => {
                painter.text(
                    rect.center(),
                    Align2::CENTER_CENTER,
                    "LOADING…",
                    FontId::new(12.0, FontFamily::Monospace),
                    MUTED,
                );
            }
            (Some(tex), None) => {
                let img = vec2(iv.size[0] as f32, iv.size[1] as f32);
                let fit = (rect.width() / img.x).min(rect.height() / img.y);
                let cur = match iv.zoom {
                    Zoom::Fit => fit,
                    Zoom::Scale(z) => z,
                };
                // Wheel zooms around the cursor; ctrl+wheel / pinch too.
                let (scroll, pinch, hover) = ui.input(|i| {
                    (
                        i.smooth_scroll_delta.y,
                        i.zoom_delta(),
                        i.pointer.hover_pos(),
                    )
                });
                let mut zoom = cur;
                if resp.hovered() {
                    zoom = (cur * pinch * (scroll * 0.0025).exp()).clamp(0.02, 40.0);
                }
                if (zoom - cur).abs() > f32::EPSILON {
                    let center = rect.center() + iv.pan;
                    let anchor = hover.unwrap_or(rect.center());
                    let d = (anchor - center) / cur;
                    iv.pan = anchor - d * zoom - rect.center();
                    iv.zoom = Zoom::Scale(zoom);
                }
                if resp.dragged() {
                    iv.pan += resp.drag_delta();
                    if iv.zoom == Zoom::Fit {
                        iv.zoom = Zoom::Scale(cur);
                    }
                }
                if resp.double_clicked() {
                    iv.pan = egui::Vec2::ZERO;
                    iv.zoom = if iv.zoom == Zoom::Fit {
                        Zoom::Scale(1.0)
                    } else {
                        Zoom::Fit
                    };
                }
                // Keep the image from being dragged out of the viewport.
                let z = match iv.zoom {
                    Zoom::Fit => fit,
                    Zoom::Scale(z) => z,
                };
                let max = ((img * z - rect.size()) / 2.0).max(egui::Vec2::ZERO);
                iv.pan = egui::vec2(iv.pan.x.clamp(-max.x, max.x), iv.pan.y.clamp(-max.y, max.y));
                if iv.zoom == Zoom::Fit {
                    iv.pan = egui::Vec2::ZERO;
                }
                let shown = Rect::from_center_size(rect.center() + iv.pan, img * z);
                painter.image(
                    tex.id(),
                    shown,
                    Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                    Color32::WHITE,
                );
                // HUD.
                let label = match iv.zoom {
                    Zoom::Fit => format!("FIT · {}×{}", iv.size[0], iv.size[1]),
                    Zoom::Scale(z) => format!("{:.0}% · {}×{}", z * 100.0, iv.size[0], iv.size[1]),
                };
                let hud = Rect::from_min_size(
                    egui::pos2(rect.left() + S, rect.bottom() - 28.0 - S),
                    vec2(label.chars().count() as f32 * 8.2 + 20.0, 28.0),
                );
                painter.rect_filled(hud, CornerRadius::same(1), BG);
                painter.rect_stroke(
                    hud,
                    CornerRadius::same(1),
                    theme::hairline(),
                    StrokeKind::Inside,
                );
                painter.text(
                    hud.center(),
                    Align2::CENTER_CENTER,
                    label,
                    FontId::new(11.0, FontFamily::Monospace),
                    TEXT,
                );
            }
        }
    }

    fn empty_state(&mut self, ui: &mut Ui) {
        let rect = ui.max_rect();
        let p = ui.painter().clone();
        widgets::corner_crosshairs(&p, rect, 16.0);

        let discreet = self.discreet();
        let col_w = (rect.width() - 96.0).clamp(280.0, 1040.0);
        let head = (rect.width() * 0.055).clamp(36.0, 64.0);
        let col = Rect::from_center_size(rect.center(), vec2(col_w, rect.height() - 64.0));
        ui.scope_builder(
            UiBuilder::new()
                .max_rect(col)
                .layout(Layout::top_down(Align::Center)),
            |ui| {
                ui.spacing_mut().item_spacing = vec2(S * 2.0, S * 2.0);
                let used = if discreet { 280.0 } else { 520.0 };
                ui.add_space(((col.height() - used) * 0.5).max(0.0));
                // Icon with a soft offset shadow, no white box.
                let (r, _) = ui.allocate_exact_size(vec2(96.0, 96.0), Sense::hover());
                let tex = if discreet {
                    &self.viewer_icon
                } else {
                    &self.peach
                };
                let uv = Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
                ui.painter().image(
                    tex.id(),
                    r.translate(vec2(0.0, 6.0)),
                    uv,
                    Color32::from_black_alpha(70),
                );
                ui.painter().image(tex.id(), r, uv, Color32::WHITE);
                if !discreet {
                    let wm = ui.painter().layout_job(widgets::wordmark_job(24.0));
                    let w = wm.size().x + 48.0 + 12.0;
                    ui.allocate_ui_with_layout(
                        vec2(w, 32.0),
                        Layout::left_to_right(Align::Center),
                        |ui| {
                            widgets::badge(ui, "01", 24.0);
                            ui.add_space(12.0);
                            widgets::wordmark(ui, 24.0);
                        },
                    );
                }
                ui.label(headline(
                    self.opts.know.max(1),
                    discreet,
                    head,
                    col_w.min(900.0),
                ));
                let sub = if discreet {
                    "Your library, in a native app. Offline, and yours."
                } else {
                    "Point it at a folder. It indexes your images and videos in place, \
                     never touches the files, and stays offline: no account, no ads, no network."
                };
                ui.scope(|ui| {
                    ui.set_max_width(576.0);
                    ui.add(
                        egui::Label::new(egui::RichText::new(sub).size(16.0).color(MUTED)).wrap(),
                    );
                });
                ui.horizontal(|ui| {
                    let w = 220.0 + 150.0 + 12.0;
                    ui.add_space(((col_w - w) * 0.5).max(0.0));
                    if widgets::button_solid_hero(ui, "Import folder").clicked() {
                        self.push("library.import", json!({}));
                    }
                    if widgets::button_outline_hero(ui, "Shortcuts").clicked() {
                        self.push("docs.toggle", json!({}));
                    }
                });
                if !discreet {
                    ui.label(theme::hud(
                        "Linux · Offline · No ads · Free and open source",
                        FAINT,
                    ));
                    ui.add_space(S);
                    let cells = [
                        (
                            "01",
                            "Point it at a folder",
                            "Indexed in place, never modified.",
                        ),
                        (
                            "02",
                            "Browse by keyboard",
                            "Filter, favorite and play without the mouse.",
                        ),
                        ("03", "Stays offline", "No account, no ads, no network."),
                    ];
                    let w = col_w.min(900.0);
                    let (grid, _) = ui.allocate_exact_size(vec2(w, 112.0), Sense::hover());
                    let cw = (w - 2.0) / 3.0;
                    let g = ui.painter().clone();
                    g.rect_filled(grid, CornerRadius::ZERO, LINE);
                    for (i, (n, title, body)) in cells.iter().enumerate() {
                        let cell = Rect::from_min_size(
                            grid.min + vec2(i as f32 * (cw + 1.0), 0.0),
                            vec2(cw, 112.0),
                        );
                        g.rect_filled(cell, CornerRadius::ZERO, BG);
                        g.text(
                            cell.right_top() + vec2(-12.0, 12.0),
                            Align2::RIGHT_TOP,
                            *n,
                            FontId::new(10.0, FontFamily::Monospace),
                            FAINT,
                        );
                        g.text(
                            cell.min + vec2(16.0, 16.0),
                            Align2::LEFT_TOP,
                            *title,
                            FontId::new(18.0, theme::title_family()),
                            TEXT,
                        );
                        let job = egui::text::LayoutJob::simple(
                            (*body).to_owned(),
                            FontId::new(13.0, FontFamily::Proportional),
                            MUTED,
                            cw - 32.0,
                        );
                        let galley = g.layout_job(job);
                        g.galley(cell.min + vec2(16.0, 48.0), galley, MUTED);
                    }
                    g.rect_stroke(
                        grid,
                        CornerRadius::ZERO,
                        Stroke::new(1.0, LINE),
                        StrokeKind::Inside,
                    );
                }
            },
        );
    }

    /// 18+ gate, shown until confirmed (persisted in the config).
    fn age_gate(&mut self, ui: &mut Ui) {
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(BG))
            .show(ui, |ui| {
                let rect = ui.max_rect();
                let p = ui.painter().clone();
                widgets::corner_crosshairs(&p, rect, 16.0);
                let col = Rect::from_center_size(rect.center(), vec2(640.0_f32.min(rect.width() - 96.0), 320.0));
                ui.scope_builder(UiBuilder::new().max_rect(col), |ui| {
                    ui.spacing_mut().item_spacing = vec2(S * 2.0, S * 2.0);
                    ui.horizontal(|ui| {
                        widgets::badge(ui, "18", 28.0);
                        widgets::mono_label(ui, "Age check", MUTED);
                    });
                    ui.label(
                        egui::RichText::new("You must be 18 or older.")
                            .font(FontId::new(44.0, theme::title_family()))
                            .color(Color32::WHITE),
                    );
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(
                                "This application is for adults only and manages local media. \
                                 Nothing leaves this machine. By continuing you confirm that you are at least 18 years old.",
                            )
                            .size(13.0)
                            .color(MUTED),
                        )
                        .wrap(),
                    );
                    ui.add_space(S);
                    ui.horizontal(|ui| {
                        if widgets::button_solid(ui, "I am 18 or older").clicked() {
                            self.push("app.age.confirm", json!({}));
                        }
                        if widgets::button_outline(ui, "Exit").clicked() {
                            self.push("app.quit", json!({}));
                        }
                    });
                });
            });
    }

    fn toasts_ui(&mut self, ctx: &egui::Context) {
        let now = Instant::now();
        self.toasts.retain(|t| t.until > now);
        if self.toasts.is_empty() {
            return;
        }
        ctx.request_repaint_after(Duration::from_millis(500));
        egui::Area::new(egui::Id::new("toasts"))
            .anchor(Align2::RIGHT_BOTTOM, vec2(-S * 2.0, -S * 2.0))
            .order(egui::Order::Tooltip)
            .show(ctx, |ui| {
                for t in &self.toasts {
                    let (edge, fg) = match t.kind {
                        ToastKind::Info => (LINE_STRONG, TEXT),
                        ToastKind::Error => (DANGER, TEXT),
                    };
                    egui::Frame::new()
                        .fill(RAISED)
                        .stroke(Stroke::new(1.0, edge))
                        .inner_margin(egui::Margin::symmetric(12, 8))
                        .show(ui, |ui| {
                            ui.label(egui::RichText::new(&t.text).size(12.0).color(fg));
                        });
                }
            });
    }

    fn docs_window(&mut self, ctx: &egui::Context) {
        if !self.show_docs {
            return;
        }
        let mut open = true;
        egui::Window::new("DOCS")
            .open(&mut open)
            .collapsible(false)
            .default_width(420.0)
            .show(ctx, |ui| {
                let lh = self.cfg.left_hand;
                let rows: &[(&str, &str)] = if lh {
                    &[
                        (
                            "W A S D",
                            "Move in the grid, zoom in the viewer, volume in the player",
                        ),
                        ("E / Space", "Open the selected item"),
                        ("F", "Favorite"),
                        ("A / D", "Previous / next in the viewer, seek in the player"),
                        ("Shift+A / D", "Previous / next item in the player"),
                        ("X · G", "Mute · fullscreen"),
                        ("Z", "Fit the image to the window"),
                        ("Q", "Back, or clear the search"),
                        ("1 – 4", "Library, Images, Videos, Favorites"),
                        ("/", "Focus the search box"),
                        ("Ctrl+Shift+H", "Boss key: pause, mute, discreet, minimize"),
                    ]
                } else {
                    &[
                        ("Ctrl+O", "Import a folder"),
                        ("Ctrl+R", "Rescan the library"),
                        ("Ctrl+Shift+H", "Boss key: pause, mute, discreet, minimize"),
                        ("← ↑ ↓ →", "Move in the grid"),
                        ("Enter", "Open the selected item"),
                        ("Home / End", "First / last item"),
                        ("1 – 4", "Library, Images, Videos, Favorites"),
                        ("/", "Focus the search box (Esc clears it)"),
                        ("S", "Favorite"),
                        ("Double-click", "Open an item"),
                        ("Esc", "Back to the library"),
                        ("← →", "Previous / next item in a viewer"),
                        ("Space", "Play / pause video"),
                        ("F", "Fullscreen"),
                        ("?", "This window"),
                    ]
                };
                for (k, v) in rows {
                    widgets::kv(ui, k, v);
                }
                ui.add_space(S);
                if widgets::toggle_chip(ui, "Left-hand keys", lh).clicked() {
                    self.push("app.left_hand.toggle", json!({}));
                }
                ui.add_space(S);
                ui.label(
                    egui::RichText::new(
                        "Everything stays on this machine. No account, no telemetry, no network.",
                    )
                    .color(MUTED),
                );
            });
        self.show_docs = open;
    }
}

impl eframe::App for JizzApp {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.frame_no += 1;
        self.poll_jobs();
        self.poll_thumbs(&ctx);
        self.handle_shortcuts(&ctx);
        self.update_title(&ctx);

        if !self.cfg.age_confirmed {
            self.age_gate(ui);
            self.toasts_ui(&ctx);
            self.drain_queue();
            self.drive_screenshot(&ctx);
            return;
        }

        let t0 = Instant::now();
        if let (View::Player(pv), Some(rs)) = (&mut self.view, &self.render_state) {
            pv.pump(rs);
            if pv.player.state() == PlayState::Playing {
                // Timeline/clock refresh; frames themselves wake the UI.
                ctx.request_repaint_after(Duration::from_millis(100));
            }
        }
        let chrome = !matches!(&self.view, View::Player(pv) if pv.fullscreen);
        if chrome {
            self.top_bar(ui);
            self.breadcrumb(ui);
            self.inspector(ui);
        }
        self.central(ui);
        self.toasts_ui(&ctx);
        self.docs_window(&ctx);

        self.drain_queue();
        self.drive_bench(&ctx, t0);
        self.drive_screenshot(&ctx);
    }

    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        BG.to_normalized_gamma_f32()
    }
}

fn register_ui_commands(reg: &mut Registry<JizzApp>) {
    // Same names as core: in the UI, scans run off-thread.
    reg.register(
        "library.import",
        "Import a folder (dialog if no path given)",
        |app, args| {
            match args.get("path").and_then(Value::as_str) {
                Some(p) => app.start_scan(Some(PathBuf::from(p))),
                None => app.pick_folder(),
            }
            Ok(json!({ "started": true }))
        },
    );
    reg.register(
        "library.rescan",
        "Rescan every imported folder",
        |app, _| {
            app.start_scan(None);
            Ok(json!({ "started": true }))
        },
    );
    reg.register(
        "media.favorite.toggle",
        "Toggle favorite: {id}",
        |app, args| {
            let id = args
                .get("id")
                .and_then(Value::as_i64)
                .ok_or_else(|| jizzcraft_core::Error::Invalid("id".into()))?;
            let fav = app.db.toggle_favorite(id)?;
            app.refresh();
            Ok(json!({ "id": id, "favorite": fav }))
        },
    );
    reg.register("view.filter", "Switch tab: {tab}", |app, args| {
        let tab = args.get("tab").and_then(Value::as_str).and_then(Tab::parse);
        match tab {
            Some(t) => {
                app.tab = t;
                app.refresh();
                Ok(json!({}))
            }
            None => Err(jizzcraft_core::Error::Invalid("unknown tab".into())),
        }
    });
    reg.register("media.open", "Open a media item: {index}", |app, args| {
        if let Some(idx) = args.get("index").and_then(Value::as_u64) {
            app.open_index(idx);
        }
        Ok(json!({}))
    });
    reg.register("viewer.close", "Back to the library", |app, _| {
        app.close_view();
        Ok(json!({}))
    });
    reg.register(
        "viewer.step",
        "Previous / next item: {delta}",
        |app, args| {
            let d = args.get("delta").and_then(Value::as_i64).unwrap_or(1);
            app.step_view(d);
            Ok(json!({}))
        },
    );
    reg.register(
        "viewer.zoom",
        "Zoom: {mode: fit|actual|in|out}",
        |app, args| {
            if let View::Image(iv) = &mut app.view {
                let cur = match iv.zoom {
                    Zoom::Scale(z) => z,
                    Zoom::Fit => 1.0,
                };
                match args.get("mode").and_then(Value::as_str) {
                    Some("fit") => {
                        iv.zoom = Zoom::Fit;
                        iv.pan = egui::Vec2::ZERO;
                    }
                    Some("actual") => iv.zoom = Zoom::Scale(1.0),
                    Some("in") => iv.zoom = Zoom::Scale((cur * 1.25).min(40.0)),
                    Some("out") => iv.zoom = Zoom::Scale((cur / 1.25).max(0.02)),
                    _ => {}
                }
            }
            Ok(json!({}))
        },
    );
    reg.register("player.toggle", "Play / pause", |app, _| {
        if let View::Player(pv) = &app.view {
            if pv.player.state() == PlayState::Ended {
                pv.player.replay();
            } else {
                pv.player.toggle();
            }
        }
        Ok(json!({}))
    });
    reg.register(
        "player.seek",
        "Seek: {delta: seconds} or {pos: seconds}",
        |app, args| {
            if let View::Player(pv) = &app.view {
                if let Some(d) = args.get("delta").and_then(Value::as_i64) {
                    pv.player.seek_relative(d);
                } else if let Some(p) = args.get("pos").and_then(Value::as_f64) {
                    pv.player.seek(Duration::from_secs_f64(p.max(0.0)), true);
                }
            }
            Ok(json!({}))
        },
    );
    reg.register(
        "player.volume",
        "Volume: {delta} or {value} in 0..1",
        |app, args| {
            let mut new = None;
            if let View::Player(pv) = &mut app.view {
                let cur = pv.player.volume();
                let v = match (
                    args.get("value").and_then(Value::as_f64),
                    args.get("delta").and_then(Value::as_f64),
                ) {
                    (Some(v), _) => v,
                    (_, Some(d)) => cur + d,
                    _ => cur,
                };
                pv.player.set_volume(v);
                pv.player.set_muted(false);
                new = Some(pv.player.volume() as f32);
            }
            if let Some(v) = new {
                app.cfg.volume = Some(v);
                app.save_cfg();
            }
            Ok(json!({}))
        },
    );
    reg.register("player.mute", "Toggle mute", |app, _| {
        if let View::Player(pv) = &mut app.view {
            let m = !pv.player.muted();
            pv.player.set_muted(m);
        }
        Ok(json!({}))
    });
    reg.register("player.fullscreen", "Toggle fullscreen", |app, _| {
        if let View::Player(pv) = &mut app.view {
            pv.fullscreen = !pv.fullscreen;
            app.ctx
                .send_viewport_cmd(egui::ViewportCommand::Fullscreen(pv.fullscreen));
        }
        Ok(json!({}))
    });
    reg.register("player.replay", "Restart from the beginning", |app, _| {
        if let View::Player(pv) = &app.view {
            pv.player.replay();
        }
        Ok(json!({}))
    });
    reg.register(
        "app.share_safe.toggle",
        "Blur thumbnails until hovered",
        |app, _| {
            app.cfg.screen_share_safe = !app.cfg.screen_share_safe;
            app.save_cfg();
            Ok(json!({ "screen_share_safe": app.cfg.screen_share_safe }))
        },
    );
    reg.register("app.age.confirm", "Confirm the 18+ age gate", |app, _| {
        app.cfg.age_confirmed = true;
        app.save_cfg();
        Ok(json!({}))
    });
    reg.register("app.quit", "Quit the application", |app, _| {
        app.ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        Ok(json!({}))
    });
    reg.register("app.discreet.toggle", "Toggle discreet mode", |app, _| {
        app.cfg.discreet = !app.cfg.discreet;
        app.save_cfg();
        Ok(json!({ "discreet": app.cfg.discreet }))
    });
    reg.register(
        "app.boss_key",
        "Pause, mute, discreet mode, minimize",
        |app, _| {
            app.cfg.discreet = true;
            app.save_cfg();
            // Pause and mute whatever is playing, leave fullscreen, then hide.
            if let View::Player(pv) = &mut app.view {
                pv.player.pause();
                pv.player.set_muted(true);
                if pv.fullscreen {
                    pv.fullscreen = false;
                    app.ctx
                        .send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
                }
            }
            app.ctx
                .send_viewport_cmd(egui::ViewportCommand::Minimized(true));
            Ok(json!({}))
        },
    );
    reg.register(
        "inspector.toggle",
        "Show or hide the inspector",
        |app, _| {
            app.cfg.inspector_open = !app.cfg.inspector_open;
            app.save_cfg();
            Ok(json!({}))
        },
    );
    reg.register(
        "view.sort",
        "Sort the library: {by: name|added|size|duration, desc?}",
        |app, args| {
            let by = args
                .get("by")
                .and_then(Value::as_str)
                .and_then(SortKey::parse)
                .ok_or_else(|| jizzcraft_core::Error::Invalid("unknown sort key".into()))?;
            app.sort = by;
            app.sort_desc = args.get("desc").and_then(Value::as_bool).unwrap_or(false);
            app.cfg.sort = Some(by.as_str().to_owned());
            app.cfg.sort_desc = app.sort_desc;
            app.save_cfg();
            app.selected_index = None;
            app.refresh();
            Ok(json!({}))
        },
    );
    reg.register("view.search.focus", "Focus the search box", |app, _| {
        app.focus_search = true;
        Ok(json!({}))
    });
    reg.register("view.search.clear", "Clear the search box", |app, _| {
        app.search.clear();
        app.refresh();
        Ok(json!({}))
    });
    reg.register(
        "view.select.move",
        "Move the grid selection: {dx, dy}",
        |app, args| {
            let dx = args.get("dx").and_then(Value::as_i64).unwrap_or(0);
            let dy = args.get("dy").and_then(Value::as_i64).unwrap_or(0);
            let cur = app.selected_index;
            let next = move_index(cur, dx, dy, app.last_cols, app.total);
            if let Some(i) = next {
                app.select_index(i);
            }
            Ok(json!({ "index": next }))
        },
    );
    reg.register(
        "view.select.edge",
        "Select the first or last item: {to: first|last}",
        |app, args| {
            let last = args.get("to").and_then(Value::as_str) == Some("last");
            if app.total > 0 {
                app.select_index(if last { app.total - 1 } else { 0 });
            }
            Ok(json!({}))
        },
    );
    reg.register("view.open_selected", "Open the selected item", |app, _| {
        if let Some(i) = app.selected_index {
            app.open_index(i);
        }
        Ok(json!({}))
    });
    reg.register(
        "app.left_hand.toggle",
        "Toggle the left-hand (WASD) key preset",
        |app, _| {
            app.cfg.left_hand = !app.cfg.left_hand;
            app.save_cfg();
            Ok(json!({ "left_hand": app.cfg.left_hand }))
        },
    );
    reg.register("docs.toggle", "Show or hide the docs window", |app, _| {
        app.show_docs = !app.show_docs;
        Ok(json!({}))
    });
}

/// New grid index after moving by `(dx, dy)` cells; `None` when the library is empty.
/// Moves clamp at the edges, and the first move from no selection lands on 0.
fn move_index(cur: Option<u64>, dx: i64, dy: i64, cols: usize, total: u64) -> Option<u64> {
    if total == 0 {
        return None;
    }
    let Some(cur) = cur else {
        return Some(0);
    };
    let step = dx + dy * cols.max(1) as i64;
    Some((cur as i64 + step).clamp(0, total as i64 - 1) as u64)
}

fn headline(know: u32, discreet: bool, size: f32, width: f32) -> egui::text::LayoutJob {
    use egui::{TextFormat, text::LayoutJob};
    let font = FontId::new(size, theme::title_family());
    let fmt = |color| TextFormat {
        font_id: font.clone(),
        color,
        extra_letter_spacing: size * -0.035,
        ..Default::default()
    };
    let (plain, accent) = (fmt(Color32::WHITE), fmt(ACCENT_INK));
    let mut job = LayoutJob::default();
    job.wrap.max_width = width;
    job.halign = Align::Center;
    if discreet {
        job.append("Your library, offline.", 0.0, plain);
        return job;
    }
    job.append("The porn viewer you already ", 0.0, plain.clone());
    for i in 0..know {
        if i > 0 {
            job.append(" ", 0.0, plain.clone());
        }
        job.append("know", 0.0, accent.clone());
    }
    job.append(" how to use.", 0.0, plain);
    job
}

fn draw_tile(
    ui: &mut Ui,
    m: &Media,
    selected: bool,
    (discreet, share_safe): (bool, bool),
    state: ThumbState<'_>,
    frame_no: u64,
    (size, idx): (egui::Vec2, u64),
) -> (egui::Response, bool) {
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let star_rect =
        Rect::from_center_size(rect.right_bottom() + vec2(-20.0, -22.0), vec2(28.0, 28.0));
    let star_clicked = resp.clicked()
        && ui
            .input(|i| i.pointer.interact_pos())
            .is_some_and(|p| star_rect.contains(p));
    let p = ui.painter();
    p.rect_filled(rect, CornerRadius::ZERO, BG);
    let body = Rect::from_min_max(rect.min, egui::pos2(rect.max.x, rect.max.y - 44.0));
    let inner = body.shrink2(vec2(0.0, 0.0));
    p.rect_filled(inner, CornerRadius::ZERO, SUNKEN);
    match state {
        ThumbState::Ready(t) => {
            // Discreet / screen-share-safe: blurred until hovered (discreet: always).
            let blur = discreet || (share_safe && !resp.hovered());
            let tex = if blur { &t.blur } else { &t.full };
            // "Cover" fit: crop the long side so the tile is always filled.
            let (tw, th) = (t.size[0], t.size[1]);
            let tile_aspect = inner.width() / inner.height();
            let img_aspect = tw / th;
            let uv = if img_aspect > tile_aspect {
                let w = tile_aspect / img_aspect;
                Rect::from_min_max(
                    egui::pos2((1.0 - w) / 2.0, 0.0),
                    egui::pos2((1.0 + w) / 2.0, 1.0),
                )
            } else {
                let h = img_aspect / tile_aspect;
                Rect::from_min_max(
                    egui::pos2(0.0, (1.0 - h) / 2.0),
                    egui::pos2(1.0, (1.0 + h) / 2.0),
                )
            };
            p.image(tex.id(), inner, uv, Color32::WHITE);
        }
        ThumbState::Loading => {
            // Subtle pulse while the thumbnail is being generated.
            let t = (frame_no as f32 * 0.08).sin() * 0.5 + 0.5;
            let a = (10.0 + 18.0 * t) as u8;
            p.rect_filled(inner, CornerRadius::ZERO, Color32::from_white_alpha(a));
            p.text(
                body.center(),
                Align2::CENTER_CENTER,
                "…",
                FontId::new(14.0, FontFamily::Monospace),
                LINE.gamma_multiply(2.2),
            );
        }
        ThumbState::Failed => {
            p.text(
                body.center(),
                Align2::CENTER_CENTER,
                if m.missing { "MISSING" } else { "NO PREVIEW" },
                FontId::new(10.0, FontFamily::Monospace),
                if m.missing {
                    DANGER
                } else {
                    LINE.gamma_multiply(2.2)
                },
            );
        }
    }
    // Type badge.
    let badge = Rect::from_min_size(rect.min + vec2(8.0, 8.0), vec2(44.0, 16.0));
    p.rect_filled(badge, CornerRadius::ZERO, BG.gamma_multiply(0.6));
    p.text(
        badge.center(),
        Align2::CENTER_CENTER,
        m.kind.label(),
        FontId::new(9.0, FontFamily::Monospace),
        MUTED,
    );
    // Faint index number, top right.
    p.text(
        rect.right_top() + vec2(-8.0, 8.0),
        Align2::RIGHT_TOP,
        format!("{:03}", idx + 1),
        FontId::new(9.0, FontFamily::Monospace),
        FAINT,
    );
    if m.favorite || resp.hovered() {
        widgets::star_icon(
            p,
            star_rect.center(),
            14.0,
            m.favorite,
            if m.favorite { APP } else { MUTED },
        );
    }
    if m.missing {
        p.rect_stroke(
            body,
            CornerRadius::ZERO,
            Stroke::new(1.0, DANGER),
            StrokeKind::Inside,
        );
    }
    // Caption: name + duration or resolution.
    let name = if discreet {
        "• • •".to_owned()
    } else {
        m.name.clone()
    };
    let meta = match m.kind {
        MediaKind::Video => m.duration_ms.map(fmt_duration),
        MediaKind::Image => m.width.zip(m.height).map(|(w, h)| format!("{w}×{h}")),
    }
    .unwrap_or_else(|| "—".into());
    let cap = Rect::from_min_max(egui::pos2(rect.min.x, rect.max.y - 44.0), rect.max);
    let cap_text = Rect::from_min_max(cap.min, egui::pos2(cap.max.x - 32.0, cap.max.y));
    p.with_clip_rect(cap_text).text(
        cap.min + vec2(10.0, 7.0),
        Align2::LEFT_TOP,
        &name,
        FontId::new(12.0, FontFamily::Proportional),
        TEXT,
    );
    p.text(
        cap.min + vec2(10.0, 25.0),
        Align2::LEFT_TOP,
        meta,
        FontId::new(10.5, FontFamily::Monospace),
        FAINT,
    );

    // Shared hairlines: right and bottom edge only, so neighbours do not double up.
    p.vline(rect.max.x - 0.5, rect.y_range(), theme::hairline());
    p.hline(rect.x_range(), rect.max.y - 0.5, theme::hairline());
    if selected {
        p.rect_stroke(
            rect,
            CornerRadius::ZERO,
            Stroke::new(2.0, APP),
            StrokeKind::Inside,
        );
    } else if resp.hovered() {
        p.rect_stroke(
            rect,
            CornerRadius::ZERO,
            Stroke::new(1.0, LINE_STRONG),
            StrokeKind::Inside,
        );
    }
    if resp.has_focus() {
        p.rect_stroke(
            rect,
            CornerRadius::ZERO,
            Stroke::new(2.0, ACCENT_INK),
            StrokeKind::Inside,
        );
    }
    (resp, star_clicked)
}

fn upload_thumb(ctx: &egui::Context, id: i64, t: &Thumb, used: u64) -> ThumbTex {
    let mk = |name: String, r: &jizzcraft_media::service::Rgba| {
        let img = egui::ColorImage::from_rgba_unmultiplied([r.w as usize, r.h as usize], &r.data);
        ctx.load_texture(name, img, egui::TextureOptions::LINEAR)
    };
    ThumbTex {
        full: mk(format!("thumb-{id}"), &t.full),
        blur: mk(format!("blur-{id}"), &t.blur),
        size: [t.full.w as f32, t.full.h as f32],
        used,
    }
}

/// Full-resolution decode for the viewer (EXIF orientation applied, capped to
/// the GPU texture limit). Runs on a worker thread.
fn decode_for_view(path: &std::path::Path) -> Result<egui::ColorImage, String> {
    use image::ImageDecoder;
    let reader = image::ImageReader::open(path)
        .map_err(|e| format!("cannot open: {e}"))?
        .with_guessed_format()
        .map_err(|e| format!("cannot read: {e}"))?;
    let mut decoder = reader
        .into_decoder()
        .map_err(|e| format!("unsupported format: {e}"))?;
    let orientation = decoder.orientation().ok();
    let mut img =
        image::DynamicImage::from_decoder(decoder).map_err(|e| format!("decode failed: {e}"))?;
    if let Some(o) = orientation {
        img.apply_orientation(o);
    }
    if img.width().max(img.height()) > MAX_VIEW_SIDE {
        img = img.resize(
            MAX_VIEW_SIDE,
            MAX_VIEW_SIDE,
            image::imageops::FilterType::Triangle,
        );
    }
    let rgba = img.to_rgba8();
    let size = [rgba.width() as usize, rgba.height() as usize];
    Ok(egui::ColorImage::from_rgba_unmultiplied(
        size,
        rgba.as_raw(),
    ))
}

pub fn fmt_size(bytes: u64) -> String {
    const U: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = bytes as f64;
    let mut i = 0;
    while v >= 1024.0 && i < U.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{bytes} B")
    } else {
        format!("{v:.1} {}", U[i])
    }
}

pub fn fmt_duration(ms: u64) -> String {
    let s = ms / 1000;
    format!("{:02}:{:02}:{:02}", s / 3600, (s / 60) % 60, s % 60)
}

/// `YYYY-MM-DD` from Unix seconds (civil-from-days, no chrono dependency).
pub fn fmt_date(secs: i64) -> String {
    let z = secs.div_euclid(86_400) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grid_moves_clamp_at_edges() {
        assert_eq!(move_index(None, 1, 0, 4, 10), Some(0));
        assert_eq!(move_index(Some(0), -1, 0, 4, 10), Some(0));
        assert_eq!(move_index(Some(9), 1, 0, 4, 10), Some(9));
        assert_eq!(move_index(Some(2), 0, 1, 4, 10), Some(6));
        assert_eq!(move_index(Some(8), 0, 1, 4, 10), Some(9));
        assert_eq!(move_index(Some(1), 0, -1, 4, 10), Some(0));
        assert_eq!(move_index(Some(0), 0, 1, 4, 0), None);
    }

    #[test]
    fn formats() {
        assert_eq!(fmt_duration(763_000), "00:12:43");
        assert_eq!(fmt_size(1536), "1.5 KB");
        assert_eq!(fmt_date(0), "1970-01-01");
        assert_eq!(fmt_date(1_760_000_000), "2025-10-09");
    }
}
