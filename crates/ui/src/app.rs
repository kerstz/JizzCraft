//! Application shell: state, command registry wiring, panels.

use crate::config::Config;
use crate::theme::{self, ACCENT, BG, DANGER, HAIRLINE, PANEL, RAISED, S, TEXT, TEXT_DIM};
use crate::widgets;
use egui::{
    Align, Align2, Color32, CornerRadius, FontFamily, FontId, Layout, Rect, Sense, Stroke,
    StrokeKind, TextureHandle, Ui, UiBuilder, vec2,
};
use jizzcraft_core::scan::ScanReport;
use jizzcraft_core::{
    Db, Filter, HasLibrary, Media, MediaKind, Registry, Stats, paths, register_core,
};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};

const PEACH_PNG: &[u8] = include_bytes!("../../../assets/icons/jizzcraft-512.png");
const VIEWER_PNG: &[u8] = include_bytes!("../../../assets/icons/viewer-256.png");

const TILE_W: f32 = 184.0;
const TILE_H: f32 = 148.0;

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

    tab: Tab,
    search: String,
    selected: Option<i64>,
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

    peach: TextureHandle,
    viewer_icon: TextureHandle,
    last_title: String,
    frame_no: u64,
    screenshot_sent: bool,
}

impl HasLibrary for JizzApp {
    fn db(&mut self) -> &mut Db {
        &mut self.db
    }
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
            tab: opts
                .tab
                .as_deref()
                .and_then(Tab::parse)
                .unwrap_or(Tab::Library),
            search: String::new(),
            selected: None,
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
            peach: load_icon(&cc.egui_ctx, "peach", PEACH_PNG),
            viewer_icon: load_icon(&cc.egui_ctx, "viewer", VIEWER_PNG),
            last_title: String::new(),
            frame_no: 0,
            screenshot_sent: false,
            opts,
        };
        app.refresh();
        if let Some(dir) = app.opts.import.clone() {
            app.start_scan(Some(dir));
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
                        }
                        Err(e) => self.toast(ToastKind::Error, format!("Import failed: {e}")),
                    }
                }
            }
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
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.into()));
            self.last_title = title.into();
        }
    }

    fn drive_screenshot(&mut self, ctx: &egui::Context) {
        let Some(path) = self.opts.screenshot.clone() else {
            return;
        };
        self.frame_no += 1;
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
        } else if !self.screenshot_sent && self.frame_no > 12 && self.busy == 0 {
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
                    .fill(PANEL)
                    .inner_margin(egui::Margin::symmetric(S as i8 * 2, 0))
                    .stroke(Stroke::NONE),
            )
            .show(ui, |ui| {
                ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                    if self.discreet() {
                        ui.label(egui::RichText::new("Viewer").color(TEXT_DIM).size(14.0));
                    } else {
                        widgets::badge(ui, "01", 24.0);
                        widgets::wordmark(ui, 20.0);
                    }
                    ui.add_space(S * 3.0);
                    for t in Tab::ALL {
                        if widgets::nav_tab(ui, t.label(), self.tab == t).clicked() {
                            self.push("view.filter", json!({ "tab": t.label() }));
                        }
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if widgets::button_solid(ui, "Import folder").clicked() {
                            self.push("library.import", json!({}));
                        }
                        if widgets::toggle_chip(ui, "Discreet", self.discreet()).clicked() {
                            self.push("app.discreet.toggle", json!({}));
                        }
                        // Whatever the nav tabs leave over, up to 220 px.
                        let w = (ui.available_width() - S).clamp(72.0, 220.0);
                        let resp = ui.add_sized(
                            [w, 28.0],
                            egui::TextEdit::singleline(&mut self.search)
                                .hint_text("SEARCH FILENAME")
                                .margin(egui::Margin::symmetric(8, 6)),
                        );
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
            .exact_size(32.0)
            .frame(
                egui::Frame::new()
                    .fill(BG)
                    .inner_margin(egui::Margin::symmetric(S as i8 * 2, 0)),
            )
            .show(ui, |ui| {
                ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                    let brand = if self.discreet() {
                        "VIEWER"
                    } else {
                        "JIZZCRAFT"
                    };
                    let crumb = format!("01 / {brand} / {}", self.tab.label());
                    widgets::mono_label(ui, &crumb, TEXT_DIM);
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if self.stats.total > 0 {
                            let on = self.cfg.inspector_open;
                            if widgets::toggle_chip(ui, "Inspector", on).clicked() {
                                self.push("inspector.toggle", json!({}));
                            }
                            let mut text = format!("{} ITEMS", self.total);
                            if self.busy > 0 {
                                text += " · SCANNING…";
                            }
                            widgets::mono_label(ui, &text, TEXT_DIM);
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
                    .fill(PANEL)
                    .inner_margin(egui::Margin::same(S as i8 * 2))
                    .stroke(theme::hairline()),
            )
            .show(ui, |ui| {
                widgets::mono_label(ui, "Inspector", ACCENT);
                ui.add_space(S);
                widgets::hairline_h(ui);
                ui.add_space(S);
                let Some(m) = media else {
                    widgets::mono_label(ui, "Select an item", TEXT_DIM);
                    return;
                };
                widgets::kv(ui, "Name", if discreet { "—" } else { &m.name });
                let path = m.path.to_string_lossy();
                widgets::kv(ui, "Path", if discreet { "hidden" } else { &path });
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
                if self.stats.total == 0 {
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
        widgets::grid_background(ui.painter(), rect);
        ui.painter().text(
            rect.center(),
            Align2::CENTER_CENTER,
            "NO MATCHING ITEMS",
            FontId::new(12.0, FontFamily::Monospace),
            TEXT_DIM,
        );
    }

    fn grid(&mut self, ui: &mut Ui) {
        let avail = ui.available_width();
        let gap = S;
        let cols = (((avail - gap) / (TILE_W + gap)).floor() as usize).max(1);
        let total = self.total as usize;
        let rows = total.div_ceil(cols);
        let discreet = self.discreet();
        let mut open: Option<usize> = None;
        let mut select: Option<i64> = None;

        egui::ScrollArea::vertical()
            .auto_shrink(false)
            .show_rows(ui, TILE_H, rows, |ui, range| {
                ui.spacing_mut().item_spacing = vec2(gap, gap);
                let start = (range.start * cols) as u64;
                let end = ((range.end * cols).min(total)) as u64;
                self.ensure_page(start, end);
                for row in range {
                    ui.horizontal(|ui| {
                        ui.add_space(gap);
                        for col in 0..cols {
                            let idx = (row * cols + col) as u64;
                            if idx >= self.total {
                                break;
                            }
                            let Some(m) = self.page.items.get((idx - self.page.offset) as usize)
                            else {
                                break;
                            };
                            let resp = draw_tile(ui, m, self.selected == Some(m.id), discreet);
                            if resp.clicked() {
                                select = Some(m.id);
                            }
                            if resp.double_clicked() {
                                open = Some(idx as usize);
                            }
                        }
                    });
                }
            });
        if let Some(id) = select {
            self.selected = Some(id);
        }
        if let Some(idx) = open {
            self.push("media.open", json!({ "index": idx }));
        }
    }

    fn empty_state(&mut self, ui: &mut Ui) {
        let rect = ui.max_rect();
        let p = ui.painter().clone();
        widgets::grid_background(&p, rect);
        widgets::corner_crosshairs(&p, rect, 16.0);
        widgets::ruler(&p, rect);

        let discreet = self.discreet();
        if !discreet {
            p.text(
                rect.right_top() + vec2(-48.0, 36.0),
                Align2::RIGHT_CENTER,
                "J I Z Z   C R A F T",
                FontId::new(11.0, FontFamily::Monospace),
                TEXT,
            );
        }

        // Table of contents, bottom right.
        let toc = ["Highlights", "Library", "Player", "Privacy", "Get started"];
        let mut y = rect.bottom() - 40.0;
        for (i, item) in toc.iter().enumerate().rev() {
            let label = format!("{:02}  {}", i + 1, item.to_uppercase());
            p.text(
                egui::pos2(rect.right() - 64.0, y),
                Align2::RIGHT_BOTTOM,
                label,
                FontId::new(10.0, FontFamily::Monospace),
                TEXT_DIM,
            );
            y -= 18.0;
        }

        // Left column.
        let col = Rect::from_min_size(
            egui::pos2(
                rect.left() + 64.0,
                rect.top() + (rect.height() * 0.5 - 190.0).max(40.0),
            ),
            vec2((rect.width() - 128.0).min(820.0), 400.0),
        );
        ui.scope_builder(UiBuilder::new().max_rect(col), |ui| {
            ui.spacing_mut().item_spacing = vec2(S * 2.0, S * 2.0);
            ui.horizontal(|ui| {
                let (r, _) = ui.allocate_exact_size(vec2(64.0, 64.0), Sense::hover());
                ui.painter().rect_filled(r, CornerRadius::same(2), Color32::WHITE);
                let tex = if discreet { &self.viewer_icon } else { &self.peach };
                ui.painter().image(
                    tex.id(),
                    r.shrink(6.0),
                    Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                    Color32::WHITE,
                );
                if !discreet {
                    widgets::badge(ui, "01", 22.0);
                    widgets::wordmark(ui, 22.0);
                }
            });
            ui.add_space(S);
            ui.label(headline(self.opts.know.max(1), discreet));
            ui.add_space(S);
            let sub = if discreet {
                "Your library, in a native app. Offline, and yours."
            } else {
                "Your library, in a native app written entirely in Rust. Open source, offline, and yours."
            };
            ui.add(egui::Label::new(egui::RichText::new(sub).size(14.0).color(TEXT_DIM)).wrap());
            ui.add_space(S * 2.0);
            ui.horizontal(|ui| {
                if widgets::button_solid(ui, "Import folder").clicked() {
                    self.push("library.import", json!({}));
                }
                if widgets::button_outline(ui, "Read the docs").clicked() {
                    self.push("docs.toggle", json!({}));
                }
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
                        ToastKind::Info => (ACCENT, TEXT),
                        ToastKind::Error => (DANGER, TEXT),
                    };
                    egui::Frame::new()
                        .fill(RAISED)
                        .stroke(Stroke::new(1.0, edge))
                        .inner_margin(egui::Margin::symmetric(12, 8))
                        .show(ui, |ui| {
                            ui.label(theme::spaced(&t.text, 10.5, 0.8, fg));
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
                for (k, v) in [
                    ("Ctrl+O", "Import a folder"),
                    ("Ctrl+R", "Rescan the library"),
                    ("Ctrl+Shift+H", "Boss key: pause, mute, discreet, minimize"),
                    ("Double-click", "Open an item"),
                    ("Esc", "Back to the library"),
                    ("← →", "Previous / next item"),
                    ("Space", "Play / pause video"),
                    ("F", "Fullscreen"),
                ] {
                    widgets::kv(ui, k, v);
                }
                ui.add_space(S);
                ui.label(
                    egui::RichText::new(
                        "Everything stays on this machine. No account, no telemetry, no network.",
                    )
                    .color(TEXT_DIM),
                );
            });
        self.show_docs = open;
    }
}

impl eframe::App for JizzApp {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.poll_jobs();
        self.handle_shortcuts(&ctx);
        self.update_title(&ctx);

        self.top_bar(ui);
        self.breadcrumb(ui);
        self.inspector(ui);
        self.central(ui);
        self.toasts_ui(&ctx);
        self.docs_window(&ctx);

        self.drain_queue();
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
            app.ensure_page(idx, idx + 1);
            if let Some(m) = app.page.items.get((idx - app.page.offset) as usize) {
                app.selected = Some(m.id);
            }
        }
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
    reg.register("docs.toggle", "Show or hide the docs window", |app, _| {
        app.show_docs = !app.show_docs;
        Ok(json!({}))
    });
}

fn headline(know: u32, _discreet: bool) -> egui::text::LayoutJob {
    use egui::{TextFormat, text::LayoutJob};
    let font = FontId::new(52.0, theme::title_family());
    let plain = TextFormat::simple(font.clone(), Color32::WHITE);
    let accent = TextFormat::simple(font, ACCENT);
    let mut job = LayoutJob::default();
    job.wrap.max_width = 760.0;
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

fn draw_tile(ui: &mut Ui, m: &Media, selected: bool, discreet: bool) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(TILE_W, TILE_H), Sense::click());
    let p = ui.painter();
    p.rect_filled(rect, CornerRadius::same(1), RAISED);
    // Placeholder body until a thumbnail exists.
    let body = Rect::from_min_max(rect.min, egui::pos2(rect.max.x, rect.max.y - 36.0));
    p.rect_filled(
        body.shrink(1.0),
        CornerRadius::ZERO,
        Color32::from_rgb(0x18, 0x1b, 0x1f),
    );
    p.text(
        body.center(),
        Align2::CENTER_CENTER,
        if m.missing { "MISSING" } else { m.kind.label() },
        FontId::new(10.0, FontFamily::Monospace),
        if m.missing {
            DANGER
        } else {
            HAIRLINE.gamma_multiply(2.2)
        },
    );
    // Type badge.
    let badge = Rect::from_min_size(rect.min + vec2(6.0, 6.0), vec2(40.0, 16.0));
    p.rect_filled(badge, CornerRadius::same(1), BG);
    p.rect_stroke(
        badge,
        CornerRadius::same(1),
        theme::hairline(),
        StrokeKind::Inside,
    );
    p.text(
        badge.center(),
        Align2::CENTER_CENTER,
        m.kind.label(),
        FontId::new(9.0, FontFamily::Monospace),
        TEXT_DIM,
    );
    if m.favorite {
        p.text(
            rect.right_top() + vec2(-10.0, 14.0),
            Align2::CENTER_CENTER,
            "★",
            FontId::new(14.0, FontFamily::Proportional),
            ACCENT,
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
    let cap = Rect::from_min_max(egui::pos2(rect.min.x, rect.max.y - 36.0), rect.max);
    p.with_clip_rect(cap).text(
        cap.min + vec2(8.0, 6.0),
        Align2::LEFT_TOP,
        &name,
        FontId::new(11.0, FontFamily::Monospace),
        TEXT,
    );
    p.text(
        cap.min + vec2(8.0, 21.0),
        Align2::LEFT_TOP,
        meta,
        FontId::new(9.5, FontFamily::Monospace),
        TEXT_DIM,
    );

    let border = if selected {
        Stroke::new(2.0, ACCENT)
    } else if resp.hovered() {
        Stroke::new(1.0, TEXT_DIM)
    } else {
        theme::hairline()
    };
    p.rect_stroke(rect, CornerRadius::same(1), border, StrokeKind::Inside);
    if resp.has_focus() {
        p.rect_stroke(
            rect.expand(2.0),
            CornerRadius::same(2),
            Stroke::new(2.0, ACCENT),
            StrokeKind::Outside,
        );
    }
    resp
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
    fn formats() {
        assert_eq!(fmt_duration(763_000), "00:12:43");
        assert_eq!(fmt_size(1536), "1.5 KB");
        assert_eq!(fmt_date(0), "1970-01-01");
        assert_eq!(fmt_date(1_760_000_000), "2025-10-09");
    }
}
