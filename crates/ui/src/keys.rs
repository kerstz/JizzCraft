//! One table of key bindings. It drives dispatch *and* the docs window, so the
//! two cannot drift. Bindings are matched on exact modifiers (Ctrl+S is not S),
//! and digits / left-hand letters match the *physical* key so they work on
//! AZERTY and other layouts.
//!
//! If two bindings ever fight over one key, the test at the bottom referees.

use egui::{Event, Key, Modifiers};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Ctx {
    Global,
    Library,
    Viewer,
    Player,
    Triage,
}

impl Ctx {
    pub fn title(self) -> &'static str {
        match self {
            Ctx::Global => "Anywhere",
            Ctx::Library => "Library",
            Ctx::Viewer => "Image viewer",
            Ctx::Player => "Video player",
            Ctx::Triage => "Triage",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Preset {
    Default,
    LeftHand,
    Both,
}

impl Preset {
    fn active(self, left_hand: bool) -> bool {
        match self {
            Preset::Both => true,
            Preset::Default => !left_hand,
            Preset::LeftHand => left_hand,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Shift {
    Off,
    On,
    /// Punctuation keys: Shift depends on the layout.
    Any,
}

pub struct Binding {
    pub ctx: Ctx,
    pub preset: Preset,
    pub key: Key,
    /// Match the physical key position instead of the layout's character.
    pub physical: bool,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: Shift,
    pub cmd: &'static str,
    /// JSON arguments.
    pub args: &'static str,
    /// Label shown in the docs window.
    pub keys: &'static str,
    pub doc: &'static str,
}

#[allow(clippy::too_many_arguments)]
const fn b(
    ctx: Ctx,
    preset: Preset,
    key: Key,
    physical: bool,
    keys: &'static str,
    cmd: &'static str,
    args: &'static str,
    doc: &'static str,
) -> Binding {
    Binding {
        ctx,
        preset,
        key,
        physical,
        ctrl: false,
        alt: false,
        shift: Shift::Off,
        cmd,
        args,
        keys,
        doc,
    }
}

const fn ctrl(mut x: Binding) -> Binding {
    x.ctrl = true;
    x
}

const fn alt(mut x: Binding) -> Binding {
    x.alt = true;
    x
}

const fn shifted(mut x: Binding) -> Binding {
    x.shift = Shift::On;
    x
}

const fn any_shift(mut x: Binding) -> Binding {
    x.shift = Shift::Any;
    x
}

use Ctx::{Global, Library, Player, Viewer};
use Preset::{Both, Default as Def, LeftHand as Lh};

pub const KEYMAP: &[Binding] = &[
    // ---- anywhere ---------------------------------------------------------
    shifted(ctrl(b(
        Global,
        Both,
        Key::H,
        false,
        "Ctrl+Shift+H",
        "app.boss_key",
        "{}",
        "Boss key: pause, mute, discreet, minimize",
    ))),
    shifted(ctrl(b(
        Global,
        Both,
        Key::M,
        false,
        "Ctrl+Shift+M",
        "app.panic.maths",
        "{}",
        "Panic: jump to the chosen video, full screen",
    ))),
    shifted(ctrl(b(
        Global,
        Both,
        Key::L,
        false,
        "Ctrl+Shift+L",
        "app.vault.lock",
        "{}",
        "Lock the encrypted library",
    ))),
    ctrl(b(
        Global,
        Both,
        Key::O,
        false,
        "Ctrl+O",
        "library.import",
        "{}",
        "Import a folder",
    )),
    ctrl(b(
        Global,
        Both,
        Key::R,
        false,
        "Ctrl+R",
        "library.rescan",
        "{}",
        "Rescan the library",
    )),
    alt(b(
        Global,
        Both,
        Key::Num0,
        true,
        "Alt+0",
        "media.rate",
        r#"{"rating":0}"#,
        "Clear the rating",
    )),
    alt(b(
        Global,
        Both,
        Key::Num1,
        true,
        "Alt+1",
        "media.rate",
        r#"{"rating":1}"#,
        "Rate 1 star",
    )),
    alt(b(
        Global,
        Both,
        Key::Num2,
        true,
        "Alt+2",
        "media.rate",
        r#"{"rating":2}"#,
        "Rate 2 stars",
    )),
    alt(b(
        Global,
        Both,
        Key::Num3,
        true,
        "Alt+3",
        "media.rate",
        r#"{"rating":3}"#,
        "Rate 3 stars",
    )),
    alt(b(
        Global,
        Both,
        Key::Num4,
        true,
        "Alt+4",
        "media.rate",
        r#"{"rating":4}"#,
        "Rate 4 stars",
    )),
    alt(b(
        Global,
        Both,
        Key::Num5,
        true,
        "Alt+5",
        "media.rate",
        r#"{"rating":5}"#,
        "Rate 5 stars",
    )),
    // ---- library ----------------------------------------------------------
    b(
        Library,
        Both,
        Key::ArrowLeft,
        false,
        "←",
        "view.select.move",
        r#"{"dx":-1,"dy":0}"#,
        "Move left",
    ),
    b(
        Library,
        Both,
        Key::ArrowRight,
        false,
        "→",
        "view.select.move",
        r#"{"dx":1,"dy":0}"#,
        "Move right",
    ),
    b(
        Library,
        Both,
        Key::ArrowUp,
        false,
        "↑",
        "view.select.move",
        r#"{"dx":0,"dy":-1}"#,
        "Move up",
    ),
    b(
        Library,
        Both,
        Key::ArrowDown,
        false,
        "↓",
        "view.select.move",
        r#"{"dx":0,"dy":1}"#,
        "Move down",
    ),
    b(
        Library,
        Lh,
        Key::A,
        true,
        "A",
        "view.select.move",
        r#"{"dx":-1,"dy":0}"#,
        "Move left",
    ),
    b(
        Library,
        Lh,
        Key::D,
        true,
        "D",
        "view.select.move",
        r#"{"dx":1,"dy":0}"#,
        "Move right",
    ),
    b(
        Library,
        Lh,
        Key::W,
        true,
        "W",
        "view.select.move",
        r#"{"dx":0,"dy":-1}"#,
        "Move up",
    ),
    b(
        Library,
        Lh,
        Key::S,
        true,
        "S",
        "view.select.move",
        r#"{"dx":0,"dy":1}"#,
        "Move down",
    ),
    b(
        Library,
        Both,
        Key::Enter,
        false,
        "Enter",
        "view.open_selected",
        "{}",
        "Open the selected item",
    ),
    b(
        Library,
        Lh,
        Key::E,
        true,
        "E",
        "view.open_selected",
        "{}",
        "Open the selected item",
    ),
    b(
        Library,
        Lh,
        Key::Space,
        false,
        "Space",
        "view.open_selected",
        "{}",
        "Open the selected item",
    ),
    b(
        Library,
        Both,
        Key::Home,
        false,
        "Home",
        "view.select.edge",
        r#"{"to":"first"}"#,
        "First item",
    ),
    b(
        Library,
        Both,
        Key::End,
        false,
        "End",
        "view.select.edge",
        r#"{"to":"last"}"#,
        "Last item",
    ),
    any_shift(b(
        Library,
        Both,
        Key::Slash,
        false,
        "/",
        "view.search.focus",
        "{}",
        "Focus the search box",
    )),
    b(
        Library,
        Both,
        Key::Escape,
        false,
        "Esc",
        "view.search.clear",
        "{}",
        "Clear the search box",
    ),
    b(
        Library,
        Lh,
        Key::Q,
        true,
        "Q",
        "view.search.clear",
        "{}",
        "Clear the search box",
    ),
    b(
        Library,
        Both,
        Key::Num1,
        true,
        "1",
        "view.filter",
        r#"{"tab":"library"}"#,
        "Library tab",
    ),
    b(
        Library,
        Both,
        Key::Num2,
        true,
        "2",
        "view.filter",
        r#"{"tab":"images"}"#,
        "Images tab",
    ),
    b(
        Library,
        Both,
        Key::Num3,
        true,
        "3",
        "view.filter",
        r#"{"tab":"videos"}"#,
        "Videos tab",
    ),
    b(
        Library,
        Both,
        Key::Num4,
        true,
        "4",
        "view.filter",
        r#"{"tab":"favorites"}"#,
        "Favorites tab",
    ),
    b(
        Library,
        Def,
        Key::S,
        false,
        "S",
        "media.favorite.toggle",
        "{}",
        "Favorite",
    ),
    b(
        Library,
        Lh,
        Key::F,
        true,
        "F",
        "media.favorite.toggle",
        "{}",
        "Favorite",
    ),
    any_shift(b(
        Library,
        Both,
        Key::Questionmark,
        false,
        "?",
        "docs.toggle",
        "{}",
        "This window",
    )),
    // ---- image viewer -----------------------------------------------------
    b(
        Viewer,
        Both,
        Key::Escape,
        false,
        "Esc",
        "viewer.close",
        "{}",
        "Back to the library",
    ),
    b(
        Viewer,
        Lh,
        Key::Q,
        true,
        "Q",
        "viewer.close",
        "{}",
        "Back to the library",
    ),
    b(
        Viewer,
        Both,
        Key::ArrowLeft,
        false,
        "←",
        "viewer.step",
        r#"{"delta":-1}"#,
        "Previous item",
    ),
    b(
        Viewer,
        Both,
        Key::ArrowRight,
        false,
        "→",
        "viewer.step",
        r#"{"delta":1}"#,
        "Next item",
    ),
    b(
        Viewer,
        Lh,
        Key::A,
        true,
        "A",
        "viewer.step",
        r#"{"delta":-1}"#,
        "Previous item",
    ),
    b(
        Viewer,
        Lh,
        Key::D,
        true,
        "D",
        "viewer.step",
        r#"{"delta":1}"#,
        "Next item",
    ),
    b(
        Viewer,
        Both,
        Key::Num0,
        true,
        "0",
        "viewer.zoom",
        r#"{"mode":"fit"}"#,
        "Fit to window",
    ),
    b(
        Viewer,
        Lh,
        Key::Z,
        true,
        "Z",
        "viewer.zoom",
        r#"{"mode":"fit"}"#,
        "Fit to window",
    ),
    b(
        Viewer,
        Both,
        Key::Num1,
        true,
        "1",
        "viewer.zoom",
        r#"{"mode":"actual"}"#,
        "Actual size",
    ),
    any_shift(b(
        Viewer,
        Both,
        Key::Plus,
        false,
        "+",
        "viewer.zoom",
        r#"{"mode":"in"}"#,
        "Zoom in",
    )),
    any_shift(b(
        Viewer,
        Both,
        Key::Equals,
        false,
        "=",
        "viewer.zoom",
        r#"{"mode":"in"}"#,
        "Zoom in",
    )),
    any_shift(b(
        Viewer,
        Both,
        Key::Minus,
        false,
        "-",
        "viewer.zoom",
        r#"{"mode":"out"}"#,
        "Zoom out",
    )),
    b(
        Viewer,
        Lh,
        Key::W,
        true,
        "W",
        "viewer.zoom",
        r#"{"mode":"in"}"#,
        "Zoom in",
    ),
    b(
        Viewer,
        Lh,
        Key::S,
        true,
        "S",
        "viewer.zoom",
        r#"{"mode":"out"}"#,
        "Zoom out",
    ),
    b(
        Viewer,
        Def,
        Key::S,
        false,
        "S",
        "media.favorite.toggle",
        "{}",
        "Favorite",
    ),
    b(
        Viewer,
        Lh,
        Key::F,
        true,
        "F",
        "media.favorite.toggle",
        "{}",
        "Favorite",
    ),
    b(
        Viewer,
        Both,
        Key::X,
        true,
        "X",
        "media.hide",
        "{}",
        "Hide / unhide (database only)",
    ),
    // ---- video player -----------------------------------------------------
    b(
        Player,
        Both,
        Key::Space,
        false,
        "Space",
        "player.toggle",
        "{}",
        "Play / pause",
    ),
    b(
        Player,
        Both,
        Key::ArrowLeft,
        false,
        "←",
        "player.seek",
        r#"{"delta":-5}"#,
        "Back 5 s",
    ),
    b(
        Player,
        Both,
        Key::ArrowRight,
        false,
        "→",
        "player.seek",
        r#"{"delta":5}"#,
        "Forward 5 s",
    ),
    b(
        Player,
        Lh,
        Key::A,
        true,
        "A",
        "player.seek",
        r#"{"delta":-5}"#,
        "Back 5 s",
    ),
    b(
        Player,
        Lh,
        Key::D,
        true,
        "D",
        "player.seek",
        r#"{"delta":5}"#,
        "Forward 5 s",
    ),
    b(
        Player,
        Both,
        Key::ArrowUp,
        false,
        "↑",
        "player.volume",
        r#"{"delta":0.05}"#,
        "Volume up",
    ),
    b(
        Player,
        Both,
        Key::ArrowDown,
        false,
        "↓",
        "player.volume",
        r#"{"delta":-0.05}"#,
        "Volume down",
    ),
    b(
        Player,
        Lh,
        Key::W,
        true,
        "W",
        "player.volume",
        r#"{"delta":0.05}"#,
        "Volume up",
    ),
    b(
        Player,
        Lh,
        Key::S,
        true,
        "S",
        "player.volume",
        r#"{"delta":-0.05}"#,
        "Volume down",
    ),
    b(
        Player,
        Both,
        Key::M,
        false,
        "M",
        "player.mute",
        "{}",
        "Mute",
    ),
    b(Player, Lh, Key::X, true, "X", "player.mute", "{}", "Mute"),
    b(
        Player,
        Def,
        Key::F,
        false,
        "F",
        "player.fullscreen",
        "{}",
        "Fullscreen",
    ),
    b(
        Player,
        Lh,
        Key::G,
        true,
        "G",
        "player.fullscreen",
        "{}",
        "Fullscreen",
    ),
    b(
        Player,
        Both,
        Key::Escape,
        false,
        "Esc",
        "player.back",
        "{}",
        "Leave fullscreen, then back",
    ),
    b(
        Player,
        Lh,
        Key::Q,
        true,
        "Q",
        "player.back",
        "{}",
        "Leave fullscreen, then back",
    ),
    b(
        Player,
        Def,
        Key::S,
        false,
        "S",
        "media.favorite.toggle",
        "{}",
        "Favorite",
    ),
    b(
        Player,
        Lh,
        Key::F,
        true,
        "F",
        "media.favorite.toggle",
        "{}",
        "Favorite",
    ),
    b(
        Player,
        Both,
        Key::PageUp,
        false,
        "Page Up",
        "viewer.step",
        r#"{"delta":-1}"#,
        "Previous item",
    ),
    b(
        Player,
        Both,
        Key::PageDown,
        false,
        "Page Down",
        "viewer.step",
        r#"{"delta":1}"#,
        "Next item",
    ),
    shifted(b(
        Player,
        Lh,
        Key::A,
        true,
        "Shift+A",
        "viewer.step",
        r#"{"delta":-1}"#,
        "Previous item",
    )),
    shifted(b(
        Player,
        Lh,
        Key::D,
        true,
        "Shift+D",
        "viewer.step",
        r#"{"delta":1}"#,
        "Next item",
    )),
    b(
        Player,
        Both,
        Key::Home,
        false,
        "Home",
        "player.replay",
        "{}",
        "Restart",
    ),
    ctrl(b(
        Global,
        Both,
        Key::Comma,
        false,
        "Ctrl+,",
        "app.settings",
        "{}",
        "Open Settings",
    )),
    // ---- bindings added by phases 9-12 (kept together so a test can pin them) ----
    ctrl(b(
        Global,
        Both,
        Key::B,
        false,
        "Ctrl+B",
        "sidebar.toggle",
        "{}",
        "Show or hide the sidebar",
    )),
    ctrl(b(
        Library,
        Both,
        Key::S,
        false,
        "Ctrl+S",
        "views.save.open",
        "{}",
        "Save the current filter as a view",
    )),
    b(
        Library,
        Both,
        Key::R,
        true,
        "R",
        "view.random",
        "{}",
        "Open a random item",
    ),
    b(
        Library,
        Both,
        Key::T,
        true,
        "T",
        "view.slideshow",
        r#"{"action":"toggle"}"#,
        "Start / stop the slideshow",
    ),
    b(
        Library,
        Both,
        Key::V,
        true,
        "V",
        "triage.start",
        "{}",
        "Triage the inbox, one item at a time",
    ),
    b(
        Viewer,
        Both,
        Key::R,
        true,
        "R",
        "view.random",
        "{}",
        "Open a random item",
    ),
    b(
        Viewer,
        Both,
        Key::T,
        true,
        "T",
        "view.slideshow",
        r#"{"action":"stop"}"#,
        "Stop the slideshow",
    ),
    b(
        Viewer,
        Both,
        Key::Space,
        false,
        "Space",
        "view.slideshow",
        r#"{"action":"pause"}"#,
        "Pause the slideshow",
    ),
    b(
        Library,
        Both,
        Key::X,
        true,
        "X",
        "media.hide",
        "{}",
        "Hide / unhide (database only)",
    ),
    b(
        Player,
        Def,
        Key::K,
        false,
        "K",
        "markers.add",
        "{}",
        "Add a marker here",
    ),
    b(
        Player,
        Lh,
        Key::T,
        true,
        "T",
        "markers.add",
        "{}",
        "Add a marker here",
    ),
    any_shift(b(
        Player,
        Def,
        Key::Minus,
        false,
        "-",
        "player.speed",
        r#"{"delta":-0.25}"#,
        "Slower",
    )),
    any_shift(b(
        Player,
        Def,
        Key::Plus,
        false,
        "+",
        "player.speed",
        r#"{"delta":0.25}"#,
        "Faster",
    )),
    any_shift(b(
        Player,
        Def,
        Key::Equals,
        false,
        "=",
        "player.speed",
        r#"{"delta":0.25}"#,
        "Faster",
    )),
    b(
        Player,
        Lh,
        Key::Z,
        true,
        "Z",
        "player.speed",
        r#"{"delta":-0.25}"#,
        "Slower",
    ),
    b(
        Player,
        Lh,
        Key::C,
        true,
        "C",
        "player.speed",
        r#"{"delta":0.25}"#,
        "Faster",
    ),
    b(
        Player,
        Both,
        Key::Backspace,
        false,
        "Backspace",
        "player.speed",
        r#"{"reset":true}"#,
        "Normal speed",
    ),
    b(
        Player,
        Both,
        Key::V,
        true,
        "V",
        "player.subs",
        r#"{"action":"cycle"}"#,
        "Cycle subtitle tracks, then off",
    ),
    alt(b(
        Player,
        Both,
        Key::ArrowLeft,
        false,
        "Alt+←",
        "player.subs",
        r#"{"delta_ms":-100}"#,
        "Subtitles earlier (0.1 s)",
    )),
    alt(b(
        Player,
        Both,
        Key::ArrowRight,
        false,
        "Alt+→",
        "player.subs",
        r#"{"delta_ms":100}"#,
        "Subtitles later (0.1 s)",
    )),
    b(
        Player,
        Def,
        Key::L,
        false,
        "L",
        "player.loop",
        r#"{"action":"cycle"}"#,
        "Set loop start, end, then clear",
    ),
    b(
        Player,
        Lh,
        Key::B,
        true,
        "B",
        "player.loop",
        r#"{"action":"cycle"}"#,
        "Set loop start, end, then clear",
    ),
    any_shift(b(
        Player,
        Def,
        Key::Comma,
        false,
        ",",
        "player.frame",
        r#"{"delta":-1}"#,
        "Previous frame",
    )),
    any_shift(b(
        Player,
        Def,
        Key::Period,
        false,
        ".",
        "player.frame",
        r#"{"delta":1}"#,
        "Next frame",
    )),
    shifted(b(
        Player,
        Lh,
        Key::E,
        true,
        "Shift+E",
        "player.frame",
        r#"{"delta":-1}"#,
        "Previous frame",
    )),
    b(
        Player,
        Lh,
        Key::E,
        true,
        "E",
        "player.frame",
        r#"{"delta":1}"#,
        "Next frame",
    ),
    b(
        Player,
        Def,
        Key::N,
        false,
        "N",
        "player.marker.jump",
        r#"{"dir":"next"}"#,
        "Next marker",
    ),
    shifted(b(
        Player,
        Def,
        Key::N,
        false,
        "Shift+N",
        "player.marker.jump",
        r#"{"dir":"prev"}"#,
        "Previous marker",
    )),
    b(
        Player,
        Lh,
        Key::R,
        true,
        "R",
        "player.marker.jump",
        r#"{"dir":"next"}"#,
        "Next marker",
    ),
    shifted(b(
        Player,
        Lh,
        Key::R,
        true,
        "Shift+R",
        "player.marker.jump",
        r#"{"dir":"prev"}"#,
        "Previous marker",
    )),
    // ---- organise: playlists, tags, categories ----------------------------
    ctrl(b(
        Global,
        Both,
        Key::Z,
        false,
        "Ctrl+Z",
        "org.undo",
        "{}",
        "Undo the last playlist, tag or category change",
    )),
    b(
        Library,
        Def,
        Key::P,
        false,
        "P",
        "org.picker",
        "{}",
        "Add to a playlist, tag or category",
    ),
    shifted(b(
        Library,
        Lh,
        Key::F,
        true,
        "Shift+F",
        "org.picker",
        "{}",
        "Add to a playlist, tag or category",
    )),
    shifted(b(
        Library,
        Def,
        Key::P,
        false,
        "Shift+P",
        "org.assign.repeat",
        "{}",
        "Repeat the last assignment",
    )),
    shifted(b(
        Library,
        Lh,
        Key::C,
        true,
        "Shift+C",
        "org.assign.repeat",
        "{}",
        "Repeat the last assignment",
    )),
    b(
        Library,
        Both,
        Key::Delete,
        false,
        "Delete",
        "org.remove",
        "{}",
        "Remove from the current playlist",
    ),
    shifted(b(
        Library,
        Lh,
        Key::X,
        true,
        "Shift+X",
        "org.remove",
        "{}",
        "Remove from the current playlist",
    )),
    alt(b(
        Library,
        Both,
        Key::ArrowUp,
        false,
        "Alt+↑",
        "org.move",
        r#"{"delta":-1}"#,
        "Move up in the playlist",
    )),
    alt(b(
        Library,
        Both,
        Key::ArrowDown,
        false,
        "Alt+↓",
        "org.move",
        r#"{"delta":1}"#,
        "Move down in the playlist",
    )),
    alt(b(
        Library,
        Lh,
        Key::W,
        true,
        "Alt+W",
        "org.move",
        r#"{"delta":-1}"#,
        "Move up in the playlist",
    )),
    alt(b(
        Library,
        Lh,
        Key::S,
        true,
        "Alt+S",
        "org.move",
        r#"{"delta":1}"#,
        "Move down in the playlist",
    )),
    shifted(b(
        Library,
        Both,
        Key::ArrowLeft,
        false,
        "Shift+←",
        "org.select.extend",
        r#"{"dx":-1,"dy":0}"#,
        "Extend the selection",
    )),
    shifted(b(
        Library,
        Both,
        Key::ArrowRight,
        false,
        "Shift+→",
        "org.select.extend",
        r#"{"dx":1,"dy":0}"#,
        "Extend the selection",
    )),
    shifted(b(
        Library,
        Both,
        Key::ArrowUp,
        false,
        "Shift+↑",
        "org.select.extend",
        r#"{"dx":0,"dy":-1}"#,
        "Extend the selection",
    )),
    shifted(b(
        Library,
        Both,
        Key::ArrowDown,
        false,
        "Shift+↓",
        "org.select.extend",
        r#"{"dx":0,"dy":1}"#,
        "Extend the selection",
    )),
    shifted(b(
        Library,
        Lh,
        Key::A,
        true,
        "Shift+A",
        "org.select.extend",
        r#"{"dx":-1,"dy":0}"#,
        "Extend the selection",
    )),
    shifted(b(
        Library,
        Lh,
        Key::D,
        true,
        "Shift+D",
        "org.select.extend",
        r#"{"dx":1,"dy":0}"#,
        "Extend the selection",
    )),
    shifted(b(
        Library,
        Lh,
        Key::W,
        true,
        "Shift+W",
        "org.select.extend",
        r#"{"dx":0,"dy":-1}"#,
        "Extend the selection",
    )),
    shifted(b(
        Library,
        Lh,
        Key::S,
        true,
        "Shift+S",
        "org.select.extend",
        r#"{"dx":0,"dy":1}"#,
        "Extend the selection",
    )),
    ctrl(b(
        Library,
        Both,
        Key::A,
        false,
        "Ctrl+A",
        "org.select.all",
        "{}",
        "Select every item of the list",
    )),
    shifted(b(
        Library,
        Both,
        Key::Enter,
        false,
        "Shift+Enter",
        "org.play",
        "{}",
        "Play the list from the top",
    )),
    shifted(b(
        Library,
        Lh,
        Key::E,
        true,
        "Shift+E",
        "org.play",
        "{}",
        "Play the list from the top",
    )),
    b(
        Viewer,
        Def,
        Key::P,
        false,
        "P",
        "org.picker",
        "{}",
        "Add to a playlist, tag or category",
    ),
    shifted(b(
        Viewer,
        Lh,
        Key::F,
        true,
        "Shift+F",
        "org.picker",
        "{}",
        "Add to a playlist, tag or category",
    )),
    b(
        Player,
        Def,
        Key::P,
        false,
        "P",
        "org.picker",
        "{}",
        "Add to a playlist, tag or category",
    ),
    shifted(b(
        Player,
        Lh,
        Key::F,
        true,
        "Shift+F",
        "org.picker",
        "{}",
        "Add to a playlist, tag or category",
    )),
];

// Triage reuses the open viewer/player but takes over the decision keys.
// Left/right seeking in a video moves to Shift+arrow while triaging.
use Ctx::Triage;

/// Bindings that only exist in the network build; empty otherwise, so the
/// default build never lists a command it does not have.
#[cfg(feature = "network")]
pub const NETWORK_KEYMAP: &[Binding] = &[
    ctrl(b(
        Global,
        Both,
        Key::J,
        false,
        "Ctrl+J",
        "view.downloads",
        "{}",
        "Downloads: indexers and torrents (network build)",
    )),
    shifted(ctrl(b(
        Global,
        Both,
        Key::B,
        false,
        "Ctrl+Shift+B",
        "browser.open",
        "{}",
        "Open the built-in browser (online edition)",
    ))),
];
#[cfg(not(feature = "network"))]
pub const NETWORK_KEYMAP: &[Binding] = &[];

pub const TRIAGE_KEYMAP: &[Binding] = &[
    b(
        Triage,
        Both,
        Key::ArrowRight,
        false,
        "→",
        "triage.keep",
        "{}",
        "Keep, leave the inbox",
    ),
    b(
        Triage,
        Lh,
        Key::D,
        true,
        "D",
        "triage.keep",
        "{}",
        "Keep, leave the inbox",
    ),
    b(
        Triage,
        Both,
        Key::ArrowDown,
        false,
        "↓",
        "triage.hide",
        "{}",
        "Hide (reversible)",
    ),
    b(
        Triage,
        Both,
        Key::X,
        true,
        "X",
        "triage.hide",
        "{}",
        "Hide (reversible)",
    ),
    b(
        Triage,
        Lh,
        Key::S,
        true,
        "S",
        "triage.hide",
        "{}",
        "Hide (reversible)",
    ),
    b(
        Triage,
        Both,
        Key::ArrowUp,
        false,
        "↑",
        "triage.fav",
        "{}",
        "Favorite and keep",
    ),
    b(
        Triage,
        Def,
        Key::S,
        false,
        "S",
        "triage.fav",
        "{}",
        "Favorite and keep",
    ),
    b(
        Triage,
        Lh,
        Key::W,
        true,
        "W",
        "triage.fav",
        "{}",
        "Favorite and keep",
    ),
    b(
        Triage,
        Both,
        Key::ArrowLeft,
        false,
        "←",
        "triage.undo",
        "{}",
        "Undo the last decision",
    ),
    b(
        Triage,
        Lh,
        Key::A,
        true,
        "A",
        "triage.undo",
        "{}",
        "Undo the last decision",
    ),
    b(
        Triage,
        Both,
        Key::Escape,
        false,
        "Esc",
        "triage.exit",
        "{}",
        "Leave triage",
    ),
    b(
        Triage,
        Lh,
        Key::Q,
        true,
        "Q",
        "triage.exit",
        "{}",
        "Leave triage",
    ),
    b(
        Triage,
        Both,
        Key::Space,
        false,
        "Space",
        "player.toggle",
        "{}",
        "Play / pause",
    ),
    shifted(b(
        Triage,
        Both,
        Key::ArrowLeft,
        false,
        "Shift+←",
        "player.seek",
        r#"{"delta":-5}"#,
        "Back 5 s",
    )),
    shifted(b(
        Triage,
        Both,
        Key::ArrowRight,
        false,
        "Shift+→",
        "player.seek",
        r#"{"delta":5}"#,
        "Forward 5 s",
    )),
    b(
        Triage,
        Both,
        Key::M,
        false,
        "M",
        "player.mute",
        "{}",
        "Mute",
    ),
    b(
        Triage,
        Def,
        Key::F,
        false,
        "F",
        "player.fullscreen",
        "{}",
        "Fullscreen",
    ),
    b(
        Triage,
        Lh,
        Key::G,
        true,
        "G",
        "player.fullscreen",
        "{}",
        "Fullscreen",
    ),
    b(
        Triage,
        Def,
        Key::P,
        false,
        "P",
        "org.picker",
        "{}",
        "Add to a playlist, tag or category",
    ),
    shifted(b(
        Triage,
        Lh,
        Key::F,
        true,
        "Shift+F",
        "org.picker",
        "{}",
        "Add to a playlist, tag or category",
    )),
];

/// Does `ev` trigger `bind`?
pub fn hit(ev: &Event, bind: &Binding) -> bool {
    let Event::Key {
        key,
        physical_key,
        pressed: true,
        repeat,
        modifiers,
        ..
    } = ev
    else {
        return false;
    };
    // Held keys repeat only for movement-like actions.
    let repeatable = bind.cmd.starts_with("view.select.move")
        || bind.cmd == "viewer.step"
        || bind.cmd == "player.seek"
        || bind.cmd == "player.volume"
        || bind.cmd == "viewer.zoom"
        || bind.cmd == "org.select.extend";
    if *repeat && !repeatable {
        return false;
    }
    let k = if bind.physical {
        physical_key.unwrap_or(*key)
    } else {
        *key
    };
    mods_match(modifiers, bind) && k == bind.key
}

fn mods_match(m: &Modifiers, bind: &Binding) -> bool {
    m.command == bind.ctrl
        && m.alt == bind.alt
        && match bind.shift {
            Shift::Off => !m.shift,
            Shift::On => m.shift,
            Shift::Any => true,
        }
}

/// Bindings active in `ctx` for the chosen preset.
pub fn active(ctx: Ctx, left_hand: bool) -> impl Iterator<Item = &'static Binding> {
    // Triage rows come first so they win over anything global.
    let triage: &'static [Binding] = if ctx == Ctx::Triage {
        TRIAGE_KEYMAP
    } else {
        &[]
    };
    triage
        .iter()
        .chain(KEYMAP.iter())
        .chain(NETWORK_KEYMAP.iter())
        .filter(move |x| (x.ctx == ctx || x.ctx == Ctx::Global) && x.preset.active(left_hand))
}

/// Bindings for the docs window, grouped in display order.
pub fn docs(left_hand: bool) -> Vec<(Ctx, Vec<&'static Binding>)> {
    [
        Ctx::Global,
        Ctx::Library,
        Ctx::Viewer,
        Ctx::Player,
        Ctx::Triage,
    ]
    .into_iter()
    .map(|c| {
        let rows: Vec<_> = KEYMAP
            .iter()
            .chain(NETWORK_KEYMAP.iter())
            .filter(|x| x.ctx == c && x.preset.active(left_hand))
            .collect();
        (c, rows)
    })
    .filter(|(_, rows)| !rows.is_empty())
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key_event(key: Key, physical: Option<Key>, mods: Modifiers) -> Event {
        Event::Key {
            key,
            physical_key: physical,
            pressed: true,
            repeat: false,
            modifiers: mods,
        }
    }

    fn find(cmd: &str, args: &str, ctx: Ctx, preset: Preset) -> &'static Binding {
        KEYMAP
            .iter()
            .find(|b| b.cmd == cmd && b.args == args && b.ctx == ctx && b.preset == preset)
            .unwrap_or_else(|| panic!("no binding for {cmd} {args}"))
    }

    #[test]
    fn no_duplicate_chords_within_a_context_and_preset() {
        for lh in [false, true] {
            for ctx in [Ctx::Library, Ctx::Viewer, Ctx::Player, Ctx::Triage] {
                let mut seen: Vec<(Key, bool, bool, bool, Shift)> = Vec::new();
                for x in active(ctx, lh) {
                    let sig = (x.key, x.physical, x.ctrl, x.alt, x.shift);
                    assert!(
                        !seen.contains(&sig),
                        "duplicate chord {:?} in {ctx:?} (left_hand={lh})",
                        x.keys
                    );
                    seen.push(sig);
                }
            }
        }
    }

    #[test]
    fn settings_and_browser_keys_do_not_clash() {
        let sig = |b: &Binding| (b.key, b.ctrl, b.alt, b.shift);
        let settings = KEYMAP.iter().find(|b| b.cmd == "app.settings").unwrap();
        assert_eq!(settings.keys, "Ctrl+,");
        assert!(settings.ctrl && !settings.alt && settings.shift == Shift::Off);
        // Nothing else uses Ctrl+Comma, in any context.
        let same: Vec<_> = KEYMAP
            .iter()
            .chain(TRIAGE_KEYMAP)
            .chain(NETWORK_KEYMAP)
            .filter(|b| b.ctrl && sig(b) == sig(settings))
            .collect();
        assert_eq!(same.len(), 1);
        // The browser key lives in the network table only: the offline
        // build lists no command it does not have.
        assert!(KEYMAP.iter().all(|b| b.cmd != "browser.open"));
        for b in NETWORK_KEYMAP.iter().filter(|b| b.cmd == "browser.open") {
            assert_eq!(b.keys, "Ctrl+Shift+B");
            assert!(b.ctrl && b.shift == Shift::On && b.preset == Preset::Both);
            // Ctrl+B (sidebar) stays distinct: exact modifiers decide.
            let sidebar = KEYMAP.iter().find(|x| x.cmd == "sidebar.toggle").unwrap();
            assert_ne!(sig(b), sig(sidebar));
        }
        // The Welcome tour has no key.
        assert!(
            KEYMAP
                .iter()
                .chain(NETWORK_KEYMAP)
                .all(|b| !b.cmd.contains("onboarding") && !b.cmd.contains("welcome"))
        );
    }

    #[test]
    fn every_binding_has_valid_json_args() {
        for x in KEYMAP {
            serde_json::from_str::<serde_json::Value>(x.args)
                .unwrap_or_else(|e| panic!("{}: {e}", x.keys));
        }
    }

    #[test]
    fn azerty_digit_four_matches_by_physical_key() {
        // On AZERTY the `4` key produces `'`, which egui reports as Quote.
        // Keyboards: standardised, in the sense that each country has one.
        let b4 = find(
            "view.filter",
            r#"{"tab":"favorites"}"#,
            Ctx::Library,
            Preset::Both,
        );
        let ev = key_event(Key::Quote, Some(Key::Num4), Modifiers::NONE);
        assert!(hit(&ev, b4));
    }

    #[test]
    fn left_hand_letters_match_physical_position() {
        // AZERTY: the key at QWERTY-W reports logical Z.
        let up = find(
            "view.select.move",
            r#"{"dx":0,"dy":-1}"#,
            Ctx::Library,
            Preset::LeftHand,
        );
        let ev = key_event(Key::Z, Some(Key::W), Modifiers::NONE);
        assert!(hit(&ev, up));
    }

    #[test]
    fn modifiers_must_match_exactly() {
        let fav = find("media.favorite.toggle", "{}", Ctx::Library, Preset::Default);
        assert!(hit(&key_event(Key::S, Some(Key::S), Modifiers::NONE), fav));
        assert!(!hit(
            &key_event(Key::S, Some(Key::S), Modifiers::COMMAND),
            fav
        ));
        assert!(!hit(&key_event(Key::S, Some(Key::S), Modifiers::ALT), fav));
        assert!(!hit(
            &key_event(Key::S, Some(Key::S), Modifiers::SHIFT),
            fav
        ));
        let tab1 = find(
            "view.filter",
            r#"{"tab":"library"}"#,
            Ctx::Library,
            Preset::Both,
        );
        assert!(!hit(
            &key_event(Key::Num1, Some(Key::Num1), Modifiers::ALT),
            tab1
        ));
    }

    #[test]
    fn left_hand_swaps_s_and_f_meanings() {
        let in_player = |lh| -> Vec<&'static str> {
            active(Ctx::Player, lh)
                .filter(|x| x.key == Key::F || x.key == Key::S)
                .map(|x| x.cmd)
                .collect()
        };
        assert!(in_player(false).contains(&"player.fullscreen"));
        assert!(in_player(true).contains(&"player.volume"));
        assert!(!in_player(true).contains(&"player.fullscreen"));
    }

    #[test]
    fn docs_cover_both_presets() {
        assert!(!docs(false).is_empty() && !docs(true).is_empty());
        let rows: usize = docs(false).iter().map(|(_, r)| r.len()).sum();
        assert!(rows > 20);
    }

    #[test]
    fn features_documented_in_the_readme_have_bindings() {
        // Guards against rows that silently never got added.
        for cmd in [
            "sidebar.toggle",
            "views.save.open",
            "view.random",
            "view.slideshow",
            "triage.start",
            "markers.add",
            "player.speed",
            "player.loop",
            "player.frame",
            "player.marker.jump",
            "player.subs",
        ] {
            assert!(KEYMAP.iter().any(|b| b.cmd == cmd), "no key for {cmd}");
        }
    }

    #[test]
    fn organise_keys_exist_in_both_presets_without_clashes() {
        // The picker: P, or Shift+F with the left-hand preset, in every
        // context that can show an item.
        for (ctx, lh, key, shift) in [
            (Ctx::Library, false, Key::P, false),
            (Ctx::Library, true, Key::F, true),
            (Ctx::Viewer, false, Key::P, false),
            (Ctx::Player, false, Key::P, false),
            (Ctx::Triage, false, Key::P, false),
            (Ctx::Triage, true, Key::F, true),
        ] {
            assert!(
                active(ctx, lh).any(|b| b.cmd == "org.picker"
                    && b.key == key
                    && (b.shift == Shift::On) == shift),
                "no picker key in {ctx:?} (left_hand={lh})"
            );
        }
        for cmd in [
            "org.assign.repeat",
            "org.remove",
            "org.move",
            "org.select.extend",
            "org.select.all",
            "org.play",
            "org.undo",
        ] {
            for lh in [false, true] {
                assert!(
                    active(Ctx::Library, lh).any(|b| b.cmd == cmd),
                    "{cmd} unbound in Library (left_hand={lh})"
                );
            }
        }
        // Bound for the docs window too (the table is the docs).
        let docs_rows: Vec<&str> = docs(false)
            .into_iter()
            .flat_map(|(_, r)| r)
            .map(|b| b.keys)
            .collect();
        assert!(docs_rows.contains(&"P") && docs_rows.contains(&"Ctrl+Z"));
    }
}
