//! Drives a real `Player` the way the UI does: take frames, read state.

use jizzcraft_media::player::{PlayState, Player};
use std::sync::Arc;
use std::time::{Duration, Instant};

pub fn wake() -> Arc<dyn Fn() + Send + Sync> {
    Arc::new(|| {})
}

pub struct Pump<'a> {
    pub p: &'a Player,
    t0: Instant,
    pub frames: u64,
    /// Time from creation to the first frame.
    pub first_frame: Option<Duration>,
    /// Highest position seen since the last `rebase`; `None` until sampled.
    max_pos: Option<Duration>,
    base_rss: u64,
    pub rss_peak_kib: u64,
    last_rss: Instant,
}

/// How far back the position may jitter before it counts as going backwards.
const JITTER: Duration = Duration::from_millis(500);

impl<'a> Pump<'a> {
    pub fn new(p: &'a Player) -> Self {
        Pump {
            p,
            t0: Instant::now(),
            frames: 0,
            first_frame: None,
            max_pos: None,
            base_rss: crate::proc::snapshot().rss_kib,
            rss_peak_kib: 0,
            last_rss: Instant::now(),
        }
    }

    /// Call after a seek (or a variant switch): position may jump.
    pub fn rebase(&mut self) {
        self.max_pos = None;
    }

    /// One look: frame, state, position, memory. Panics on an error state or
    /// a backwards position.
    pub fn step(&mut self) {
        if self.p.state() == PlayState::Error {
            panic!(
                "player error after {:?}: {:?} ({:?})",
                self.t0.elapsed(),
                self.p.error(),
                self.p.error_kind()
            );
        }
        if self.p.take_frame().is_some() {
            self.frames += 1;
            self.first_frame.get_or_insert_with(|| self.t0.elapsed());
        }
        let pos = self.p.position();
        match self.max_pos {
            Some(m) if pos + JITTER < m => {
                panic!("position went backwards: {m:?} -> {pos:?}")
            }
            Some(m) if pos <= m => {}
            _ => self.max_pos = Some(pos),
        }
        if self.last_rss.elapsed() > Duration::from_millis(250) {
            self.last_rss = Instant::now();
            let g = crate::proc::snapshot()
                .rss_kib
                .saturating_sub(self.base_rss);
            self.rss_peak_kib = self.rss_peak_kib.max(g);
        }
    }

    /// Step until `f` is true; panics after `secs`.
    pub fn until(&mut self, what: &str, secs: u64, mut f: impl FnMut(&Player) -> bool) {
        let end = Instant::now() + Duration::from_secs(secs);
        loop {
            self.step();
            if f(self.p) {
                return;
            }
            assert!(
                Instant::now() < end,
                "timed out ({secs}s) waiting for {what}; state {:?}, position {:?}, frames {}",
                self.p.state(),
                self.p.position(),
                self.frames
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    pub fn first_frame(&mut self, secs: u64) -> Duration {
        let end = Instant::now() + Duration::from_secs(secs);
        while self.frames == 0 {
            self.step();
            assert!(
                Instant::now() < end,
                "no frame within {secs}s; state {:?}",
                self.p.state()
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        self.first_frame.unwrap()
    }

    pub fn to_end(&mut self, secs: u64) {
        self.until("end of stream", secs, |p| p.state() == PlayState::Ended);
    }

    /// Step for a fixed time (no condition).
    pub fn spin(&mut self, d: Duration) {
        let end = Instant::now() + d;
        while Instant::now() < end {
            self.step();
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// Memory growth peak seen while pumping, KiB.
    pub fn assert_memory(&self) {
        assert!(
            self.rss_peak_kib < crate::proc::RSS_LIMIT_KIB,
            "RSS peaked {} MiB above the start",
            self.rss_peak_kib / 1024
        );
    }
}
