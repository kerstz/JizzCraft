//! `cargo xtask perf`: run the performance budgets and print a table.
//!
//! The measurements live in `crates/core/tests/perf.rs` (ignored by default,
//! release build only); budgets live in `perf/budgets.json`. This wrapper runs
//! the test, parses its `PERF <name> <ms> <budget_ms>` lines, prints them and
//! exits non-zero when a budget is exceeded or the test itself failed.
use std::process::Command;

const PLAYER_FIXTURE: &str = "fixtures/videos/bench-1080p30.mp4";

/// Compare the `BENCH_JSON` line of `jizzcraft --bench` with the budgets of
/// `perf/player_budgets.json`. Returns (key, measured, budget) per budget; a
/// budget whose key is missing from the line is an error, not a pass. The
/// line may carry a prefix (log noise glued in front of `BENCH_JSON`).
pub fn check_player(
    line: &str,
    budgets: &serde_json::Value,
) -> Result<Vec<(String, f64, f64)>, String> {
    let pos = line.find("BENCH_JSON ").ok_or("no BENCH_JSON line")?;
    let measured: serde_json::Value =
        serde_json::from_str(line[pos + "BENCH_JSON ".len()..].trim())
            .map_err(|e| format!("BENCH_JSON is not JSON: {e}"))?;
    let table = budgets
        .get("budgets")
        .and_then(serde_json::Value::as_object)
        .ok_or("budgets file has no \"budgets\" object")?;
    let mut rows = Vec::new();
    for (k, b) in table {
        let b = b
            .as_f64()
            .ok_or_else(|| format!("budget {k} is not a number"))?;
        let v = measured
            .get(k)
            .and_then(serde_json::Value::as_f64)
            .ok_or_else(|| format!("BENCH_JSON has no number named {k}"))?;
        rows.push((k.clone(), v, b));
    }
    if rows.is_empty() {
        return Err("no player budgets defined".into());
    }
    Ok(rows)
}

/// `cargo xtask perf --player [FILE]`: local only, needs a GPU and a display.
fn player(file: Option<&str>) -> Result<(), String> {
    let file = file.unwrap_or(PLAYER_FIXTURE);
    if !std::path::Path::new(file).is_file() {
        return Err(format!(
            "{file} not found (run `cargo xtask fixtures` or pass a file)"
        ));
    }
    let budgets: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string("perf/player_budgets.json")
            .map_err(|e| format!("perf/player_budgets.json: {e}"))?,
    )
    .map_err(|e| format!("perf/player_budgets.json: {e}"))?;
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let built = Command::new(&cargo)
        .args(["build", "--release", "-p", "jizzcraft"])
        .status()
        .map_err(|e| format!("cannot run cargo: {e}"))?;
    if !built.success() {
        return Err("release build failed".into());
    }
    let out = Command::new("target/release/jizzcraft")
        .args(["--bench", file, "--seconds", "10"])
        .output()
        .map_err(|e| format!("cannot run the app: {e}"))?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    if !out.status.success() {
        eprintln!("{}", String::from_utf8_lossy(&out.stderr));
    }
    let line = stdout
        .lines()
        .find(|l| l.contains("BENCH_JSON "))
        .ok_or("the app printed no BENCH_JSON line (no GPU or display?)")?;
    let rows = check_player(line, &budgets)?;
    println!("{:<24} {:>10} {:>10}  status", "metric", "value", "budget");
    let mut over = 0;
    for (n, v, b) in &rows {
        let bad = v > b;
        over += usize::from(bad);
        println!(
            "{n:<24} {v:>10.2} {b:>10.0}  {}",
            if bad { "OVER" } else { "ok" }
        );
    }
    if over > 0 {
        return Err(format!("{over} player budget(s) exceeded"));
    }
    println!("all {} player budgets hold", rows.len());
    Ok(())
}

pub fn run(args: Vec<String>) -> Result<(), String> {
    if args.first().map(String::as_str) == Some("--player") {
        return match &args[1..] {
            [] => player(None),
            [f] => player(Some(f)),
            [_, extra, ..] => Err(format!("perf --player takes one file (got {extra})")),
        };
    }
    if let Some(a) = args.first() {
        return Err(format!(
            "perf takes no argument but --player [FILE] (got {a})"
        ));
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

#[cfg(test)]
mod tests {
    use super::*;

    fn budgets() -> serde_json::Value {
        serde_json::json!({"budgets": {"ttff_ms": 3000, "dropped": 5}})
    }

    #[test]
    fn within_budget_passes_and_over_budget_is_reported() {
        let line = r#"BENCH_JSON {"ttff_ms":420.5,"dropped":9,"other":1}"#;
        let rows = check_player(line, &budgets()).unwrap();
        let get = |k: &str| rows.iter().find(|r| r.0 == k).unwrap().clone();
        assert!(get("ttff_ms").1 <= get("ttff_ms").2);
        assert!(get("dropped").1 > get("dropped").2);
    }

    #[test]
    fn a_missing_key_is_an_error() {
        let line = r#"BENCH_JSON {"ttff_ms":420.5}"#;
        let e = check_player(line, &budgets()).unwrap_err();
        assert!(e.contains("dropped"), "{e}");
    }

    #[test]
    fn a_prefix_glued_before_the_json_line_is_ignored() {
        let line = r#"[INFO] noiseBENCH_JSON {"ttff_ms":1.0,"dropped":0}"#;
        assert_eq!(check_player(line, &budgets()).unwrap().len(), 2);
        assert!(check_player("nothing here", &budgets()).is_err());
    }
}
