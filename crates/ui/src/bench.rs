//! `jizzcraft --bench <file> --seconds N`: measures the real app playing a
//! file. Numbers come from /proc and from the app's own frame accounting.

use std::time::Instant;

/// Linux USER_HZ is 100 on every mainstream architecture.
const CLK_TCK: f64 = 100.0;

pub fn cpu_seconds_of(pid: &str) -> Option<f64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // Fields after the parenthesised comm; utime and stime are the 12th/13th.
    let rest = stat.rsplit_once(')')?.1;
    let f: Vec<&str> = rest.split_whitespace().collect();
    let (u, s): (f64, f64) = (f.get(11)?.parse().ok()?, f.get(12)?.parse().ok()?);
    Some((u + s) / CLK_TCK)
}

/// (current RSS MB, peak RSS MB)
pub fn rss_mb_of(pid: &str) -> Option<(f64, f64)> {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    let kb = |key: &str| -> Option<f64> {
        status
            .lines()
            .find(|l| l.starts_with(key))?
            .split_whitespace()
            .nth(1)?
            .parse::<f64>()
            .ok()
    };
    Some((kb("VmRSS:")? / 1024.0, kb("VmHWM:")? / 1024.0))
}

pub fn percentile(sorted: &[f32], p: f32) -> f32 {
    if sorted.is_empty() {
        return 0.0;
    }
    let idx = ((sorted.len() as f32 - 1.0) * p).round() as usize;
    sorted[idx.min(sorted.len() - 1)]
}

pub struct BenchState {
    pub file: std::path::PathBuf,
    pub secs: u32,
    pub started: Option<Started>,
    pub ui_cost_ms: Vec<f32>,
    pub interval_ms: Vec<f32>,
    pub last_start: Option<Instant>,
}

pub struct Started {
    pub at: Instant,
    pub cpu0: f64,
    pub delivered0: u64,
    pub uploads0: u64,
    pub qos0: u64,
}

impl BenchState {
    pub fn new(file: std::path::PathBuf, secs: u32) -> Self {
        BenchState {
            file,
            secs,
            started: None,
            ui_cost_ms: Vec::new(),
            interval_ms: Vec::new(),
            last_start: None,
        }
    }
}

pub struct Report {
    pub width: u32,
    pub height: u32,
    pub fps: f32,
    pub wall: f64,
    pub expected: f64,
    pub delivered: u64,
    pub uploaded: u64,
    pub qos_dropped: u64,
    pub ui_passes: usize,
    pub ui_cost: [f32; 3],
    pub interval: [f32; 3],
    pub cpu_pct: f64,
    pub rss: (f64, f64),
}

impl Report {
    /// Frames the pipeline produced but the user did not get: late drops
    /// reported by GStreamer QoS, plus frames overwritten in the mailbox
    /// before the UI uploaded them.
    pub fn dropped(&self) -> u64 {
        self.qos_dropped + self.delivered.saturating_sub(self.uploaded)
    }

    pub fn print(&self, label: &str) {
        println!("JIZZCRAFT BENCH — {label}");
        println!(
            "  stream          {}x{} @ {:.2} fps",
            self.width, self.height, self.fps
        );
        println!("  measured        {:.1} s", self.wall);
        println!(
            "  frames          expected {:.0} · delivered {} · uploaded {} · DROPPED {} (qos {} + ui-skipped {})",
            self.expected,
            self.delivered,
            self.uploaded,
            self.dropped(),
            self.qos_dropped,
            self.delivered.saturating_sub(self.uploaded)
        );
        println!(
            "  ui passes       {} ({:.0}/s)",
            self.ui_passes,
            self.ui_passes as f64 / self.wall
        );
        println!(
            "  ui frame cost   p50 {:.2} ms · p95 {:.2} ms · max {:.2} ms",
            self.ui_cost[0], self.ui_cost[1], self.ui_cost[2]
        );
        println!(
            "  frame interval  p50 {:.1} ms · p95 {:.1} ms · max {:.1} ms",
            self.interval[0], self.interval[1], self.interval[2]
        );
        println!("  cpu             {:.1} % of one core", self.cpu_pct);
        println!(
            "  rss             {:.0} MB (peak {:.0} MB)",
            self.rss.0, self.rss.1
        );
        println!(
            "BENCH_JSON {{\"label\":\"{label}\",\"width\":{},\"height\":{},\"fps\":{:.2},\"seconds\":{:.1},\"expected\":{:.0},\"delivered\":{},\"uploaded\":{},\"dropped\":{},\"qos_dropped\":{},\"ui_cost_p95_ms\":{:.3},\"interval_p95_ms\":{:.2},\"cpu_pct\":{:.1},\"rss_mb\":{:.0},\"rss_peak_mb\":{:.0}}}",
            self.width,
            self.height,
            self.fps,
            self.wall,
            self.expected,
            self.delivered,
            self.uploaded,
            self.dropped(),
            self.qos_dropped,
            self.ui_cost[1],
            self.interval[1],
            self.cpu_pct,
            self.rss.0,
            self.rss.1
        );
    }
}

/// Baseline: play the same file with `gst-play-1.0` for `secs` and sample its
/// CPU and RSS. The file path is passed as an argument, never through a shell.
pub fn gst_play_baseline(file: &std::path::Path, secs: u32) -> Result<(f64, (f64, f64)), String> {
    let mut child = std::process::Command::new("gst-play-1.0")
        .arg("--no-interactive")
        .arg("--quiet")
        .arg(file)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| format!("gst-play-1.0 not runnable: {e}"))?;
    let pid = child.id().to_string();
    // Let it start up, then measure a window.
    std::thread::sleep(std::time::Duration::from_secs(3));
    let c0 = cpu_seconds_of(&pid).ok_or("gst-play exited early")?;
    let t0 = Instant::now();
    let mut rss = (0.0, 0.0);
    while t0.elapsed().as_secs() < u64::from(secs) {
        std::thread::sleep(std::time::Duration::from_millis(500));
        if let Some(r) = rss_mb_of(&pid) {
            rss = r;
        }
    }
    let c1 = cpu_seconds_of(&pid).ok_or("gst-play exited early")?;
    let wall = t0.elapsed().as_secs_f64();
    let _ = child.kill();
    let _ = child.wait();
    Ok(((c1 - c0) / wall * 100.0, rss))
}
