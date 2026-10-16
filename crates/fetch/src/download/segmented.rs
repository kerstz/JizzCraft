//! Save an HLS or DASH stream as one file.
//!
//! MPEG-TS segments become one `.ts`; fragmented MP4 (including streams that
//! keep picture and sound apart, merged by the reader) becomes one
//! fragmented `.mp4`. Nothing is converted. The reader must be in download
//! mode (`for_download`): a lost segment ends the save instead of leaving a
//! hole. Runs on its own thread; the reader blocks.

use super::{FreeSpace, JobCtl, RateLimiter};
use crate::diskspace::{RUN_MARGIN, is_full, is_low};
use crate::savename::{create_part, part_path, remove_own_part, unique_path};
use crate::segmented::SegmentedReader;
use crate::service::proxy_blocked;
use jizzcraft_core::timed::{Chunk, TimedSource};
use std::fs::File;
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

const SLICE: Duration = Duration::from_millis(100);
/// After this long paused the stream is read again from its start.
pub const LONG_PAUSE: Duration = Duration::from_secs(600);

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Ts,
    Mp4,
}

impl Kind {
    fn ext(self) -> &'static str {
        match self {
            Kind::Ts => "ts",
            Kind::Mp4 => "mp4",
        }
    }
}

fn gone(ctl: &JobCtl) -> Option<String> {
    if ctl.cancelled() {
        Some("cancelled".into())
    } else if proxy_blocked() {
        Some("proxy required".into())
    } else {
        None
    }
}

fn nap(ctl: &JobCtl, d: Duration) -> Result<(), String> {
    let end = Instant::now() + d;
    loop {
        if let Some(e) = gone(ctl) {
            return Err(e);
        }
        let left = end.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Ok(());
        }
        std::thread::sleep(left.min(SLICE));
    }
}

fn is_init(c: &Chunk) -> bool {
    c.pts.is_none()
}

fn kind_of(first: &Chunk) -> Result<Kind, String> {
    let b = &first.bytes;
    if b.len() >= 8 && (&b[4..8] == b"ftyp" || &b[4..8] == b"moov" || &b[4..8] == b"styp") {
        Ok(Kind::Mp4)
    } else if !is_init(first) && b.first() == Some(&0x47) {
        Ok(Kind::Ts)
    } else {
        Err("unsupported stream".into())
    }
}

/// Pull the next chunk, waiting through `WouldBlock`, a pause and nothing
/// else. `Ok(None)` is the end of the stream.
fn next_chunk(reader: &mut SegmentedReader, ctl: &JobCtl) -> Result<Option<Chunk>, String> {
    loop {
        if let Some(e) = gone(ctl) {
            return Err(e);
        }
        match reader.pull() {
            Ok(c) => return Ok(c),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(e) => return Err(e.to_string()),
        }
    }
}

struct Out {
    file: File,
    kind: Kind,
    init: Option<Vec<u8>>,
    done: u64,
}

impl Out {
    fn put(&mut self, c: &Chunk) -> Result<(), String> {
        if self.kind == Kind::Ts && c.bytes.first() != Some(&0x47) {
            return Err("unsupported stream".into());
        }
        if self.kind == Kind::Mp4 && is_init(c) {
            match &self.init {
                None => self.init = Some(c.bytes.clone()),
                // The same init again (a seek, a map repeated): once is enough.
                Some(first) if *first == c.bytes => return Ok(()),
                Some(_) => return Err("unsupported stream".into()),
            }
        }
        self.file.write_all(&c.bytes).map_err(|e| e.to_string())?;
        self.done += c.bytes.len() as u64;
        Ok(())
    }
}

/// Save the stream in `dir` as `<stem>.ts` or `<stem>.mp4`. Blocking; call
/// it on a thread of its own.
pub fn save_segmented(
    mut reader: SegmentedReader,
    dir: &Path,
    stem: &str,
    ctl: &Arc<JobCtl>,
    limiter: &Arc<RateLimiter>,
    free: &FreeSpace,
) -> Result<PathBuf, String> {
    if proxy_blocked() {
        return Err("proxy required".into());
    }
    let Some(duration) = reader.duration() else {
        return Err("cannot save live".into());
    };
    let est = reader.estimated_bytes();
    ctl.timed.store(true, Ordering::SeqCst);
    ctl.total.store(est.unwrap_or(0), Ordering::SeqCst);
    ctl.resumable.store(false, Ordering::SeqCst);

    // Nothing is created before the first media chunk is in hand.
    let mut lead: Vec<Chunk> = Vec::new();
    let kind = loop {
        let Some(c) = next_chunk(&mut reader, ctl)? else {
            return Err("no media".into());
        };
        let first = lead.is_empty();
        let media = !is_init(&c);
        lead.push(c);
        if first {
            kind_of(&lead[0])?;
        }
        if media {
            break kind_of(&lead[0])?;
        }
        if lead.len() > 4 {
            return Err("unsupported stream".into());
        }
    };

    if !std::fs::metadata(dir).is_ok_and(|m| m.is_dir()) {
        return Err("no folder".into());
    }
    let dir0 = std::fs::canonicalize(dir).map_err(|e| e.to_string())?;
    if let Some(e) = est
        && is_full(free(&dir0), e / 10 * 9)
    {
        return Err("disk full".into());
    }
    let (final_path, file) = reserve(&dir0, stem, kind.ext())?;
    let part = part_path(&final_path);
    let mut out = Out {
        file,
        kind,
        init: None,
        done: 0,
    };
    let r = write_all(
        &mut reader,
        &mut out,
        lead,
        &Ctx {
            ctl,
            limiter,
            free,
            dir0: &dir0,
            est,
            duration,
        },
    );
    let r = r.and_then(|()| {
        ctl.verifying.store(true, Ordering::SeqCst);
        super::place(&out.file, &part, &final_path, &dir0, out.done)
    });
    if r.is_err() {
        let _ = remove_own_part(&out.file, &part);
    }
    r
}

struct Ctx<'a> {
    ctl: &'a Arc<JobCtl>,
    limiter: &'a Arc<RateLimiter>,
    free: &'a FreeSpace,
    dir0: &'a Path,
    est: Option<u64>,
    duration: Duration,
}

fn reserve(dir: &Path, stem: &str, ext: &str) -> Result<(PathBuf, File), String> {
    let mut last = String::from("no folder");
    for _ in 0..4 {
        let final_path = unique_path(dir, stem, ext);
        match create_part(&part_path(&final_path)) {
            Ok(f) => return Ok((final_path, f)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => last = e.to_string(),
        }
    }
    Err(last)
}

fn publish(cx: &Ctx<'_>, out: &Out, pts: Option<Duration>) {
    cx.ctl.done.store(out.done, Ordering::SeqCst);
    if let Some(t) = pts
        && !cx.duration.is_zero()
    {
        let f = (t.as_secs_f64() / cx.duration.as_secs_f64()).clamp(0.0, 1.0);
        cx.ctl
            .fraction_ppm
            .store((f * 1_000_000.0) as u64, Ordering::SeqCst);
    }
    // An estimate that turned out too small grows with the file.
    cx.ctl
        .total
        .store(cx.est.unwrap_or(0).max(out.done), Ordering::SeqCst);
}

fn write_all(
    reader: &mut SegmentedReader,
    out: &mut Out,
    lead: Vec<Chunk>,
    cx: &Ctx<'_>,
) -> Result<(), String> {
    for c in &lead {
        out.put(c)?;
    }
    publish(cx, out, lead.last().and_then(|c| c.pts));
    let mut window = (Instant::now(), out.done);
    let mut last_free = Instant::now();
    loop {
        if cx.ctl.paused() {
            cx.ctl.bps.store(0, Ordering::SeqCst);
            let since = Instant::now();
            let mut restarted = false;
            while cx.ctl.paused() {
                nap(cx.ctl, SLICE)?;
                if !restarted && since.elapsed() >= LONG_PAUSE {
                    // Buffered segments and signed links go stale: begin
                    // again from the start on our own handle.
                    reader.seek(Duration::ZERO);
                    out.file.set_len(0).map_err(|e| e.to_string())?;
                    out.file
                        .seek(SeekFrom::Start(0))
                        .map_err(|e| e.to_string())?;
                    out.done = 0;
                    out.init = None;
                    cx.ctl.fraction_ppm.store(0, Ordering::SeqCst);
                    publish(cx, out, None);
                    restarted = true;
                }
            }
            window = (Instant::now(), out.done);
            last_free = Instant::now();
            if restarted {
                // The first chunk after a seek is an init (or TS) again.
                continue;
            }
        }
        let Some(c) = next_chunk(reader, cx.ctl)? else {
            return Ok(());
        };
        let wait = cx.limiter.take(c.bytes.len(), Instant::now());
        if !wait.is_zero() {
            nap(cx.ctl, wait)?;
        }
        out.put(&c)?;
        publish(cx, out, c.pts);
        let now = Instant::now();
        let span = now.saturating_duration_since(window.0);
        if span >= Duration::from_millis(500) {
            let moved = out.done.saturating_sub(window.1);
            cx.ctl
                .bps
                .store((moved as f64 / span.as_secs_f64()) as u64, Ordering::SeqCst);
            window = (now, out.done);
        }
        if now.saturating_duration_since(last_free) >= Duration::from_secs(1) {
            last_free = now;
            let remaining = cx.est.map_or(0, |e| e.saturating_sub(out.done));
            if is_low((cx.free)(cx.dir0), remaining, RUN_MARGIN) {
                cx.ctl.disk_low.store(true, Ordering::SeqCst);
                cx.ctl.pause.store(true, Ordering::SeqCst);
            }
        }
    }
}
