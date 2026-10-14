//! Integrity checks that need GStreamer: does the file actually decode?
//! Read-only and opt-in. Only error *classes* are stored, never paths.

use crate::thumbs;
use gstreamer as gst;
use gstreamer::prelude::*;
use jizzcraft_core::integrity::{CheckReport, CheckResult, Scope, checksum_file};
use jizzcraft_core::{Db, HasLibrary, MediaKind, Registry};
use serde_json::{Value, json};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckMode {
    /// Probe plus a few seeks (cheap).
    Quick,
    /// Decode everything (slow, thorough). Bring a snack.
    Full,
}

/// Stored error class for a failed check.
pub type Class = &'static str;

pub fn classify_error(msg: &str) -> Class {
    if msg == crate::guard::REFUSED {
        "remote-media"
    } else if msg.contains("missing GStreamer") {
        "missing-plugin"
    } else if msg.contains("timed out") {
        "timeout"
    } else {
        "decode-error"
    }
}

pub fn check_media(
    path: &Path,
    kind: MediaKind,
    mode: CheckMode,
    cancel: &AtomicBool,
) -> Result<(), Class> {
    match kind {
        MediaKind::Image => check_image(path, mode),
        MediaKind::Video => check_video(path, mode, cancel),
    }
}

fn check_image(path: &Path, mode: CheckMode) -> Result<(), Class> {
    let reader = image::ImageReader::open(path)
        .map_err(|_| "unreadable")?
        .with_guessed_format()
        .map_err(|_| "decode-error")?;
    match mode {
        CheckMode::Quick => reader
            .into_dimensions()
            .map(|_| ())
            .map_err(|_| "decode-error"),
        CheckMode::Full => reader.decode().map(|_| ()).map_err(|_| "decode-error"),
    }
}

fn check_video(path: &Path, mode: CheckMode, cancel: &AtomicBool) -> Result<(), Class> {
    match mode {
        CheckMode::Quick => {
            let p = thumbs::open_paused(path, 64).map_err(|e| classify_error(&e))?;
            let dur = p
                .pipeline
                .query_duration::<gst::ClockTime>()
                .map(|d| d.nseconds())
                .unwrap_or(0);
            for pct in [0u64, 50, 95] {
                if cancel.load(Ordering::Relaxed) {
                    return Ok(());
                }
                thumbs::frame_at(&p, dur / 100 * pct, 5).map_err(|e| classify_error(&e))?;
            }
            Ok(())
        }
        CheckMode::Full => {
            crate::init().map_err(|_| "missing-plugin")?;
            let uri = crate::file_uri(path).map_err(|_| "unreadable")?;
            let pb = gst::ElementFactory::make("playbin")
                .property("uri", &uri)
                .build()
                .map_err(|_| "missing-plugin")?;
            let _ = crate::guard::watch(pb.downcast_ref::<gst::Bin>().ok_or("decode-error")?);
            for prop in ["video-sink", "audio-sink"] {
                let sink = gst::ElementFactory::make("fakesink")
                    .property("sync", false)
                    .build()
                    .map_err(|_| "missing-plugin")?;
                pb.set_property(prop, &sink);
            }
            let bus = pb.bus().ok_or("decode-error")?;
            let guard = thumbs::NullOnDrop(
                pb.clone()
                    .downcast::<gst::Pipeline>()
                    .map_err(|_| "decode-error")?,
            );
            pb.set_state(gst::State::Playing)
                .map_err(|_| "decode-error")?;
            let deadline = Instant::now() + Duration::from_secs(15 * 60);
            let mut missing: Option<String> = None;
            let result = loop {
                if cancel.load(Ordering::Relaxed) {
                    break Ok(());
                }
                if Instant::now() > deadline {
                    break Err("timeout");
                }
                let Some(msg) = bus.timed_pop_filtered(
                    gst::ClockTime::from_mseconds(100),
                    &[
                        gst::MessageType::Eos,
                        gst::MessageType::Error,
                        gst::MessageType::Element,
                    ],
                ) else {
                    continue;
                };
                match msg.view() {
                    gst::MessageView::Eos(_) => break Ok(()),
                    gst::MessageView::Error(e) => {
                        let own = e.error().message().to_owned();
                        let text = if own == crate::guard::REFUSED {
                            own
                        } else {
                            missing.unwrap_or(own)
                        };
                        break Err(classify_error(&text));
                    }
                    _ => missing = crate::describe_missing_plugin(&msg).or(missing),
                }
            };
            drop(guard);
            result
        }
    }
}

#[derive(Debug, Clone)]
pub struct IntegrityOpts {
    pub mode: CheckMode,
    /// Also hash whole files and flag silent changes (bit rot).
    pub checksum: bool,
    pub scope: Scope,
    pub limit: Option<u64>,
}

impl Default for IntegrityOpts {
    fn default() -> Self {
        Self {
            mode: CheckMode::Quick,
            checksum: false,
            scope: Scope::Unchecked,
            limit: None,
        }
    }
}

pub fn run_integrity(
    db: &mut Db,
    o: &IntegrityOpts,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(u64, u64),
) -> jizzcraft_core::Result<CheckReport> {
    let mut rep = CheckReport::default();
    let rows = db.check_pending(o.scope, o.limit.unwrap_or(u64::MAX))?;
    let total = rows.len() as u64;
    for (id, path, size, mtime, stored) in rows {
        if cancel.load(Ordering::Relaxed) {
            rep.cancelled = true;
            break;
        }
        let kind = match db.get(id)? {
            Some(m) => m.kind,
            None => continue,
        };
        rep.checked += 1;
        match check_media(&path, kind, o.mode, cancel) {
            Err(class) => {
                db.set_check(
                    id,
                    &CheckResult {
                        damaged: 1,
                        note: Some(class),
                        checksum: None,
                    },
                )?;
                rep.damaged += 1;
            }
            Ok(()) if o.checksum => match checksum_file(&path, cancel) {
                Ok(Some(sum)) => {
                    let unchanged = std::fs::metadata(&path).is_ok_and(|m| {
                        m.len() == size
                            && m.modified()
                                .ok()
                                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                                .map(|d| d.as_secs() as i64)
                                == Some(mtime)
                    });
                    let rotted =
                        unchanged && stored.as_deref().is_some_and(|s| s != sum.as_slice());
                    db.set_check(
                        id,
                        &CheckResult {
                            damaged: if rotted { 2 } else { 0 },
                            note: rotted.then_some("checksum-changed"),
                            checksum: (!rotted).then_some(sum),
                        },
                    )?;
                    if rotted {
                        rep.damaged += 1;
                    } else {
                        rep.ok += 1;
                    }
                }
                Ok(None) => {
                    rep.cancelled = true;
                    rep.checked -= 1;
                    break;
                }
                Err(_) => {
                    db.set_check(
                        id,
                        &CheckResult {
                            damaged: 1,
                            note: Some("unreadable"),
                            checksum: None,
                        },
                    )?;
                    rep.unreadable += 1;
                }
            },
            Ok(()) => {
                db.set_check(
                    id,
                    &CheckResult {
                        damaged: 0,
                        note: None,
                        checksum: None,
                    },
                )?;
                rep.ok += 1;
            }
        }
        progress(rep.checked, total);
    }
    Ok(rep)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheKind {
    Previews,
    Thumbs,
    All,
}

/// Remove cached derivatives. Only ever touches `previews`/`thumbs` under the
/// cache directory, never anything else.
pub fn clear_cache(root: &Path, what: CacheKind) -> Result<u64, String> {
    let dirs: &[&str] = match what {
        CacheKind::Previews => &["previews"],
        CacheKind::Thumbs => &["thumbs"],
        CacheKind::All => &["previews", "thumbs"],
    };
    let mut bytes = 0u64;
    for d in dirs {
        let dir = root.join(d);
        if !dir.is_dir() {
            continue;
        }
        bytes += dir_size(&dir);
        std::fs::remove_dir_all(&dir).map_err(|e| format!("cannot clear cache: {e}"))?;
    }
    Ok(bytes)
}

fn dir_size(p: &Path) -> u64 {
    std::fs::read_dir(p)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| match e.metadata() {
            Ok(m) if m.is_dir() => dir_size(&e.path()),
            Ok(m) => m.len(),
            Err(_) => 0,
        })
        .sum()
}

/// Commands that need GStreamer: `library.integrity`, `cache.clear`.
pub fn register_media<C: HasLibrary>(reg: &mut Registry<C>) {
    reg.register(
        "library.integrity",
        "Decode-test files: {mode?: quick|full, checksum?, scope?: unchecked|all, limit?}",
        |c, a| {
            let mut o = IntegrityOpts::default();
            match a.get("mode").and_then(Value::as_str) {
                None | Some("quick") => {}
                Some("full") => o.mode = CheckMode::Full,
                Some(m) => {
                    return Err(jizzcraft_core::Error::Invalid(format!(
                        "unknown mode `{m}`"
                    )));
                }
            }
            match a.get("scope").and_then(Value::as_str) {
                None | Some("unchecked") => {}
                Some("all") => o.scope = Scope::All,
                Some(s) => {
                    return Err(jizzcraft_core::Error::Invalid(format!(
                        "unknown scope `{s}`"
                    )));
                }
            }
            o.checksum = a.get("checksum").and_then(Value::as_bool).unwrap_or(false);
            o.limit = a.get("limit").and_then(Value::as_u64);
            let cancel = AtomicBool::new(false);
            let rep = run_integrity(c.db(), &o, &cancel, &mut |_, _| {})?;
            Ok(serde_json::to_value(rep).expect("report"))
        },
    );
    reg.register(
        "cache.clear",
        "Delete cached previews/thumbnails: {what?: previews|thumbs|all}",
        |_, a| {
            let what = match a.get("what").and_then(Value::as_str) {
                None | Some("all") => CacheKind::All,
                Some("previews") => CacheKind::Previews,
                Some("thumbs") => CacheKind::Thumbs,
                Some(w) => {
                    return Err(jizzcraft_core::Error::Invalid(format!(
                        "unknown cache `{w}`"
                    )));
                }
            };
            let bytes = clear_cache(&jizzcraft_core::paths::cache_dir(), what)
                .map_err(jizzcraft_core::Error::Invalid)?;
            Ok(json!({ "removed_bytes": bytes }))
        },
    );
}
