//! What the process holds: threads, file descriptors, resident memory.
//! Read from `/proc` on Linux; zeros elsewhere (the checks then pass).

use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, Default)]
pub struct Snapshot {
    pub threads: usize,
    pub fds: usize,
    pub rss_kib: u64,
}

pub fn snapshot() -> Snapshot {
    let count = |p: &str| std::fs::read_dir(p).map(|d| d.count()).unwrap_or(0);
    let rss_kib = std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines().find_map(|l| {
                l.strip_prefix("VmRSS:")?
                    .split_whitespace()
                    .next()?
                    .parse()
                    .ok()
            })
        })
        .unwrap_or(0);
    Snapshot {
        threads: count("/proc/self/task"),
        fds: count("/proc/self/fd"),
        rss_kib,
    }
}

/// Taken before a scenario opens anything; checked after everything is
/// dropped.
pub struct Baseline(pub Snapshot);

/// Threads and descriptors the process may keep after a scenario (GStreamer's
/// shared pool, the allocator, a lazily started resolver).
pub const THREAD_SLACK: usize = 6;
pub const FD_SLACK: usize = 8;
pub const RSS_LIMIT_KIB: u64 = 200 * 1024;

impl Baseline {
    pub fn take() -> Baseline {
        Baseline(snapshot())
    }

    /// Growth over the baseline right now, in KiB.
    pub fn rss_growth_kib(&self) -> u64 {
        snapshot().rss_kib.saturating_sub(self.0.rss_kib)
    }

    /// Wait (up to 20 s) for threads and descriptors to come back near the
    /// baseline, then check memory.
    pub fn assert_settled(&self) {
        let end = Instant::now() + Duration::from_secs(20);
        loop {
            let s = snapshot();
            let ok = s.threads <= self.0.threads + THREAD_SLACK && s.fds <= self.0.fds + FD_SLACK;
            if ok {
                break;
            }
            assert!(
                Instant::now() < end,
                "resources did not return: threads {} -> {}, fds {} -> {}",
                self.0.threads,
                s.threads,
                self.0.fds,
                s.fds
            );
            std::thread::sleep(Duration::from_millis(100));
        }
        let g = self.rss_growth_kib();
        assert!(g < RSS_LIMIT_KIB, "RSS grew by {} MiB", g / 1024);
    }
}
