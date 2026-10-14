//! `cargo xtask perf`: run the performance budgets and print a table.
//!
//! The measurements live in `crates/core/tests/perf.rs` (ignored by default,
//! release build only); budgets live in `perf/budgets.json`. This wrapper runs
//! the test, parses its `PERF <name> <ms> <budget_ms>` lines, prints them and
//! exits non-zero when a budget is exceeded or the test itself failed.
use std::process::Command;

pub fn run(args: Vec<String>) -> Result<(), String> {
    if let Some(a) = args.first() {
        return Err(format!("perf takes no argument (got {a})"));
    }
    let out = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()))
        .args([
            "test",
            "--release",
            "-p",
            "jizzcraft-core",
            "--test",
            "perf",
            "--",
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .output()
        .map_err(|e| format!("cannot run cargo: {e}"))?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    let mut rows = Vec::new();
    for l in stdout.lines() {
        // libtest may glue its "test ... " prefix in front of the first line.
        let Some(pos) = l.find("PERF ") else { continue };
        let mut it = l[pos + 5..].split_whitespace();
        if let (Some(n), Some(Ok(v)), Some(Ok(b))) = (
            it.next(),
            it.next().map(str::parse::<f64>),
            it.next().map(str::parse::<f64>),
        ) {
            rows.push((n.to_string(), v, b));
        }
    }
    println!("{:<24} {:>10} {:>10}  status", "metric", "ms", "budget");
    let mut over = 0;
    for (n, v, b) in &rows {
        let bad = v > b;
        over += bad as usize;
        println!(
            "{n:<24} {v:>10.2} {b:>10.0}  {}",
            if bad { "OVER" } else { "ok" }
        );
    }
    if !out.status.success() {
        eprintln!("{}", String::from_utf8_lossy(&out.stderr));
        println!("{stdout}");
        return Err(if rows.is_empty() {
            "perf test failed to run".into()
        } else {
            format!("{over} budget(s) exceeded or test failed")
        });
    }
    if rows.is_empty() {
        return Err("no PERF lines produced".into());
    }
    println!("all {} budgets hold", rows.len());
    Ok(())
}
