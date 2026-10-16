//! Funscript sidecars: forgiving parse, bounded input, display-only heatmap.
use jizzcraft_core::funscript::{self, Action, MAX_ACTIONS, MAX_BYTES, Script};
use serde_json::json;

fn parse(v: serde_json::Value) -> Option<Script> {
    funscript::parse(v.to_string().as_bytes())
}

#[test]
fn unsorted_actions_are_sorted_and_deduped() {
    let s = parse(json!({"actions": [
        {"at": 300, "pos": 10}, {"at": 100, "pos": 90}, {"at": 100, "pos": 5}, {"at": 200, "pos": 50}
    ]}))
    .unwrap();
    let at: Vec<u64> = s.actions.iter().map(|a| a.at_ms).collect();
    assert_eq!(at, vec![100, 200, 300]);
}

#[test]
fn bad_entries_are_dropped_and_positions_clamped() {
    let s = parse(json!({"actions": [
        {"at": -5, "pos": 10},
        {"at": 1.5, "pos": 10},
        {"at": "100", "pos": 10},
        {"at": 100, "pos": "50"},
        {"at": 200, "pos": 150},
        {"at": 300.0, "pos": -20},
        {"at": 400, "pos": 49.6},
        {"pos": 3},
        {"at": 500}
    ]}))
    .unwrap();
    assert_eq!(
        s.actions,
        vec![
            Action {
                at_ms: 200,
                pos: 100
            },
            Action { at_ms: 300, pos: 0 },
            Action {
                at_ms: 400,
                pos: 50
            },
        ]
    );
}

#[test]
fn unknown_fields_are_ignored_and_junk_is_none() {
    let s = parse(json!({"version": "1.0", "inverted": true, "range": 90,
        "actions": [{"at": 1, "pos": 1}], "metadata": {"title": "x"}}))
    .unwrap();
    assert_eq!(s.actions.len(), 1);
    assert!(parse(json!({"actions": []})).is_none());
    assert!(parse(json!({})).is_none());
    assert!(parse(json!([1, 2])).is_none());
    assert!(funscript::parse(b"not json").is_none());
    assert!(funscript::parse(b"").is_none());
}

#[test]
fn size_and_count_are_capped() {
    let big = vec![b' '; MAX_BYTES as usize + 1];
    assert!(funscript::parse(&big).is_none());
    let actions: Vec<_> = (0..250_000u64)
        .map(|i| json!({"at": i, "pos": i % 100}))
        .collect();
    let bytes = json!({ "actions": actions }).to_string();
    assert!((bytes.len() as u64) < MAX_BYTES);
    let s = funscript::parse(bytes.as_bytes()).unwrap();
    assert_eq!(s.actions.len(), MAX_ACTIONS);
    assert_eq!(s.actions[0].at_ms, 0);
}

fn zigzag(step_ms: u64, count: u64, amp: u8) -> Script {
    Script {
        actions: (0..count)
            .map(|i| Action {
                at_ms: i * step_ms,
                pos: if i % 2 == 0 { 0 } else { amp },
            })
            .collect(),
    }
}

#[test]
fn constant_speed_gives_a_flat_heatmap() {
    // 50 units every 250 ms = 200 units/s over 10 s.
    let s = zigzag(250, 41, 50);
    let h = funscript::heatmap(&s, 10_000, 20);
    assert_eq!(h.len(), 20);
    for v in &h {
        assert!((v - 0.5).abs() < 1e-3, "{v}");
    }
}

#[test]
fn rest_is_cold_and_fast_is_capped() {
    let mut s = zigzag(100, 21, 100); // 1000 units/s for 2 s
    s.actions.push(Action {
        at_ms: 8_000,
        pos: 0,
    }); // long slow ramp afterwards
    let h = funscript::heatmap(&s, 10_000, 10);
    assert_eq!(h[0], 1.0);
    assert!(h[1] > 0.99);
    assert!(h[3] < 0.1);
    assert_eq!(h[9], 0.0);
    assert!(funscript::heatmap(&s, 0, 10).is_empty());
    assert!(funscript::heatmap(&s, 1000, 0).is_empty());
}

#[test]
fn sidecar_is_found_ignoring_case_and_axes_are_not() {
    let t = tempfile::tempdir().unwrap();
    let v = t.path().join("Clip One.mp4");
    std::fs::write(&v, b"x").unwrap();
    assert!(funscript::find_sidecar(&v).is_none());
    std::fs::write(t.path().join("clip one.roll.funscript"), b"{}").unwrap();
    std::fs::create_dir(t.path().join("clip one.funscript")).unwrap();
    assert!(funscript::find_sidecar(&v).is_none());
    std::fs::remove_dir(t.path().join("clip one.funscript")).unwrap();
    std::fs::write(
        t.path().join("CLIP ONE.FunScript"),
        json!({"actions": [{"at": 0, "pos": 0}, {"at": 500, "pos": 100}]}).to_string(),
    )
    .unwrap();
    assert!(funscript::find_sidecar(&v).is_some());
    assert_eq!(funscript::load_for(&v).unwrap().actions.len(), 2);
}
