//! Saving media to disk: what a job is, how fast it may go, who starts next.
//!
//! This file is pure (no socket, no file). The savers that talk to the
//! network live in the `network`-only submodules. The queue is in memory
//! only: nothing about a job is ever written anywhere but its own `.part`
//! file, and a `.part` never outlives the process that made it.

#[cfg(feature = "network")]
pub mod direct;

use std::path::Path;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

pub type JobId = u64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobKind {
    Direct,
    Hls,
    Dash,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobState {
    Queued,
    Probing,
    Running,
    Paused,
    Verifying,
    Done,
    Failed,
    Cancelled,
}

impl JobState {
    /// Over for good: nothing more will happen to this job.
    pub fn is_over(self) -> bool {
        matches!(self, Self::Done | Self::Failed | Self::Cancelled)
    }

    /// Takes one of the concurrent slots.
    pub fn holds_slot(self) -> bool {
        matches!(self, Self::Probing | Self::Running | Self::Verifying)
    }
}

/// What a front end may show about a job. Never an address or a path; the
/// title is for the screen only and stays in memory.
#[derive(Debug, Clone)]
pub struct JobView {
    pub id: JobId,
    pub state: JobState,
    pub kind: JobKind,
    pub done_bytes: u64,
    /// 0 when unknown.
    pub total_bytes: u64,
    /// 0.0..=1.0, 0.0 when the size is unknown.
    pub fraction: f32,
    pub bps: u64,
    pub eta_secs: Option<u64>,
    pub height: Option<u32>,
    pub resumable: bool,
    /// One of the fixed sentences of `errors`, for a failed or stopped job.
    pub error: Option<&'static str>,
    pub title: Option<String>,
    /// Found in an embedded player.
    pub embed: bool,
}

/// Free space of the volume holding a path (`None` = unknown).
pub type FreeSpace = Arc<dyn Fn(&Path) -> Option<u64> + Send + Sync>;

/// Asks for a fresh address of the same file (an expired signed link).
pub type Renew = Arc<dyn Fn() -> Receiver<Result<String, String>> + Send + Sync>;

/// The real thing: the platform's free-space call.
pub fn real_free_space() -> FreeSpace {
    Arc::new(crate::diskspace::free_bytes)
}

/// Shared between a saver and whoever drives it.
#[derive(Debug, Default)]
pub struct JobCtl {
    pub cancel: AtomicBool,
    pub pause: AtomicBool,
    pub done: AtomicU64,
    /// 0 = unknown.
    pub total: AtomicU64,
    pub bps: AtomicU64,
    pub resumable: AtomicBool,
    /// The saver paused itself because the volume is nearly full.
    pub disk_low: AtomicBool,
    /// The file is complete and being checked and moved into place.
    pub verifying: AtomicBool,
}

impl JobCtl {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::SeqCst)
    }

    pub fn paused(&self) -> bool {
        self.pause.load(Ordering::SeqCst)
    }

    /// Resume after a pause the user or the disk check asked for.
    pub fn resume(&self) {
        self.disk_low.store(false, Ordering::SeqCst);
        self.pause.store(false, Ordering::SeqCst);
    }
}

/// A token bucket shared by every job. Refills at the cap; a burst is at
/// most a quarter second of it. `take` says how long to wait, it never
/// sleeps itself.
#[derive(Debug)]
pub struct RateLimiter {
    inner: Mutex<Bucket>,
}

#[derive(Debug)]
struct Bucket {
    rate: Option<u64>,
    tokens: f64,
    at: Option<Instant>,
}

impl RateLimiter {
    pub fn new(bytes_per_sec: Option<u64>) -> Self {
        Self {
            inner: Mutex::new(Bucket {
                rate: bytes_per_sec.filter(|r| *r > 0),
                tokens: 0.0,
                at: None,
            }),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Bucket> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn set(&self, bytes_per_sec: Option<u64>) {
        let mut b = self.lock();
        b.rate = bytes_per_sec.filter(|r| *r > 0);
        b.tokens = 0.0;
        b.at = None;
    }

    /// Account for `n` bytes about to be written at `now`; the time the
    /// caller must wait before writing them.
    pub fn take(&self, n: usize, now: Instant) -> Duration {
        let mut b = self.lock();
        let Some(rate) = b.rate else {
            return Duration::ZERO;
        };
        let rate = rate as f64;
        if let Some(prev) = b.at {
            let gained = now.saturating_duration_since(prev).as_secs_f64() * rate;
            b.tokens = (b.tokens + gained).min(rate / 4.0);
        }
        b.at = Some(now);
        b.tokens -= n as f64;
        if b.tokens >= 0.0 {
            Duration::ZERO
        } else {
            Duration::from_secs_f64(-b.tokens / rate)
        }
    }
}

/// Which queued jobs may start now: first in, first out, so that jobs
/// holding a slot (probing, running, verifying) plus the new ones never pass
/// `limit`. Paused jobs hold none.
pub fn to_start(jobs: &[(JobId, JobState)], limit: usize) -> Vec<JobId> {
    let busy = jobs.iter().filter(|(_, s)| s.holds_slot()).count();
    let room = limit.max(1).saturating_sub(busy);
    jobs.iter()
        .filter(|(_, s)| *s == JobState::Queued)
        .map(|(id, _)| *id)
        .take(room)
        .collect()
}

/// Seconds left at the current speed. `None` when either is unknown.
pub fn eta(remaining: u64, bps: u64) -> Option<u64> {
    (bps > 0).then(|| remaining.div_ceil(bps))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rate_limiter_math() {
        let l = RateLimiter::new(Some(1000));
        let t0 = Instant::now();
        // Empty bucket: 500 bytes cost half a second.
        assert_eq!(l.take(500, t0), Duration::from_millis(500));
        // Half a second later the debt is paid; the next 500 cost the same.
        let w = l.take(500, t0 + Duration::from_millis(500));
        assert_eq!(w, Duration::from_millis(500));
        // An idle minute buys a burst of a quarter second, no more.
        let late = t0 + Duration::from_secs(60);
        assert_eq!(l.take(250, late), Duration::ZERO);
        assert!(l.take(250, late) > Duration::ZERO);
        // No cap, no wait; a cap of zero means no cap.
        assert_eq!(RateLimiter::new(None).take(1 << 30, t0), Duration::ZERO);
        assert_eq!(RateLimiter::new(Some(0)).take(1 << 30, t0), Duration::ZERO);
        l.set(None);
        assert_eq!(l.take(1 << 30, t0), Duration::ZERO);
    }

    #[test]
    fn scheduler_respects_limit_and_fifo() {
        use JobState::*;
        let jobs = [
            (1, Running),
            (2, Queued),
            (3, Paused),
            (4, Queued),
            (5, Queued),
        ];
        assert_eq!(to_start(&jobs, 2), [2]);
        assert_eq!(to_start(&jobs, 3), [2, 4]);
        assert_eq!(to_start(&jobs, 1), Vec::<JobId>::new());
        // Probing and verifying hold a slot too; a limit of 0 acts as 1.
        let jobs = [(1, Probing), (2, Verifying), (3, Queued)];
        assert!(to_start(&jobs, 2).is_empty());
        assert_eq!(to_start(&jobs, 3), [3]);
        assert_eq!(to_start(&[(7, Queued), (8, Queued)], 0), [7]);
    }

    #[test]
    fn eta_math() {
        assert_eq!(eta(1000, 100), Some(10));
        assert_eq!(eta(1001, 100), Some(11));
        assert_eq!(eta(0, 100), Some(0));
        assert_eq!(eta(1000, 0), None);
    }

    #[test]
    fn states() {
        assert!(JobState::Done.is_over());
        assert!(!JobState::Paused.is_over());
        assert!(JobState::Verifying.holds_slot());
        assert!(!JobState::Paused.holds_slot());
    }
}
