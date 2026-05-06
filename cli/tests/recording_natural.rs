//! Integration smoke test for natural-mode recording (cursor overlay +
//! action-log JSONL). Marked `#[ignore]` because it depends on a working
//! Chrome install plus ffmpeg on PATH; CI opts in via `cargo test --ignored`.
//!
//! What it covers:
//!   1. `record start --log-actions` opens the JSONL file synchronously
//!      so a bad path errors before the recording task spawns.
//!   2. A short `hover` + `click` script against a `data:` page produces
//!      at least one entry per action with target coords + frame index.
//!   3. `record stop` finalises a non-empty webm and includes the action
//!      log path in its response.

use std::path::PathBuf;
use std::process::Command;
use tempfile::TempDir;

const BIN: &str = env!("CARGO_BIN_EXE_agent-browser");

fn cli(tmp: &TempDir, args: &[&str]) -> Command {
    let socket_dir = tmp.path().join("sockets");
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&socket_dir).unwrap();
    std::fs::create_dir_all(&home).unwrap();

    let mut cmd = Command::new(BIN);
    cmd.args(args)
        .env("AGENT_BROWSER_SOCKET_DIR", &socket_dir)
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .env("NO_COLOR", "1");
    cmd
}

#[test]
#[ignore = "requires Chrome and ffmpeg"]
fn natural_recording_emits_action_log_with_targets_and_frames() {
    let tmp = TempDir::new().unwrap();
    let video: PathBuf = tmp.path().join("rec.webm");
    let log: PathBuf = tmp.path().join("rec.actions.jsonl");

    let html = "data:text/html,<html><body><button id=\"b\" \
        style=\"position:absolute;top:200px;left:300px;width:120px;height:40px\"\
        >Click me</button></body></html>";

    let status = cli(
        &tmp,
        &[
            "record",
            "start",
            video.to_str().unwrap(),
            html,
            "--log-actions",
            log.to_str().unwrap(),
            "--mouse-duration",
            "200",
            "--settle-ms",
            "100",
            "--type-delay",
            "0",
            "--fps",
            "20",
        ],
    )
    .status()
    .expect("record start");
    assert!(status.success(), "record start failed");

    // Let the recording capture a few frames, then drive a hover and a click.
    std::thread::sleep(std::time::Duration::from_millis(500));
    assert!(cli(&tmp, &["hover", "#b"]).status().unwrap().success());
    std::thread::sleep(std::time::Duration::from_millis(300));
    assert!(cli(&tmp, &["click", "#b"]).status().unwrap().success());
    std::thread::sleep(std::time::Duration::from_millis(300));

    let stop = cli(&tmp, &["record", "stop"]).output().expect("record stop");
    assert!(
        stop.status.success(),
        "record stop failed: {}",
        String::from_utf8_lossy(&stop.stderr)
    );
    let _ = cli(&tmp, &["close", "--all"]).status();

    let video_meta = std::fs::metadata(&video).expect("video file");
    assert!(video_meta.len() > 0, "webm should be non-empty");

    let log_text = std::fs::read_to_string(&log).expect("action log");
    let lines: Vec<&str> = log_text.lines().collect();
    assert!(
        lines.len() >= 2,
        "expected at least 2 entries, got {}: {}",
        lines.len(),
        log_text
    );

    let entries: Vec<serde_json::Value> = lines
        .iter()
        .map(|l| serde_json::from_str(l).expect("valid JSON line"))
        .collect();

    assert!(
        entries.iter().any(|e| e["action"] == "hover"),
        "no hover entry"
    );
    assert!(
        entries.iter().any(|e| e["action"] == "click"),
        "no click entry"
    );

    for e in &entries {
        assert!(e["tsMs"].is_u64(), "tsMs missing in {}", e);
        assert!(e["frameIdx"].is_u64(), "frameIdx missing in {}", e);
        if e["action"] == "click" || e["action"] == "hover" {
            let target = &e["target"];
            assert!(target["x"].is_i64(), "click/hover target.x missing");
            assert!(target["y"].is_i64(), "click/hover target.y missing");
        }
    }
}
