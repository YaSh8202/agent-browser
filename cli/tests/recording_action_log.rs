//! Integration test for `record start --log-actions <path>`: every user
//! action taken during the take lands in the JSONL timeline with the fields
//! downstream video tooling aligns against.
//! Marked `#[ignore]` because it depends on a working Chrome install and ffmpeg.

use serde_json::Value;
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

fn run(tmp: &TempDir, args: &[&str]) {
    let out = cli(tmp, args).output().unwrap();
    assert!(
        out.status.success(),
        "{:?} failed: {}{}",
        args,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

const FIXTURE: &str = "data:text/html,<html><body>\
    <button id=\"b\" style=\"position:absolute;top:200px;left:300px;width:120px;height:40px\">Save</button>\
    <input id=\"email\" style=\"position:absolute;top:300px;left:300px\">\
    </body></html>";

#[test]
#[ignore = "requires Chrome and ffmpeg"]
fn record_log_actions_writes_a_timeline_of_user_actions() {
    let tmp = TempDir::new().unwrap();
    let video = tmp.path().join("take.webm");
    let log = tmp.path().join("take.actions.jsonl");

    run(&tmp, &["open", FIXTURE]);
    run(
        &tmp,
        &[
            "record",
            "start",
            video.to_str().unwrap(),
            "--log-actions",
            log.to_str().unwrap(),
            "--cursor",
        ],
    );
    run(&tmp, &["hover", "#b"]);
    run(&tmp, &["click", "#b"]);
    run(&tmp, &["fill", "#email", "a@example.com"]);
    run(
        &tmp,
        &[
            "find", "role", "button", "click", "--name", "Save", "--exact",
        ],
    );
    // Read-only commands are not user actions and stay out of the timeline.
    run(&tmp, &["get", "url"]);
    let stop = cli(&tmp, &["--json", "record", "stop"]).output().unwrap();
    let _ = cli(&tmp, &["close", "--all"]).status();
    let stop: Value = serde_json::from_slice(&stop.stdout).expect("record stop JSON");
    assert_eq!(stop["data"]["actionLog"], log.to_str().unwrap(), "{stop}");

    let entries: Vec<Value> = std::fs::read_to_string(&log)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let actions: Vec<&str> = entries
        .iter()
        .map(|e| e["action"].as_str().unwrap())
        .collect();
    assert_eq!(actions, ["hover", "click", "fill", "click"], "{entries:?}");

    let mut previous_ts = 0;
    for entry in &entries {
        assert_eq!(entry["ok"], true, "{entry}");
        let ts = entry["tsMs"].as_u64().expect("tsMs");
        assert!(ts >= previous_ts, "tsMs must not go backwards: {entries:?}");
        previous_ts = ts;
        assert!(entry["durationMs"].is_u64(), "{entry}");
        assert!(entry["frameIdx"].is_u64(), "{entry}");
    }
    assert_eq!(entries[0]["args"]["selector"], "#b");
    assert!(
        entries[0]["target"]["x"].is_i64(),
        "hover moves the pointer"
    );
    assert_eq!(entries[2]["args"]["value"], "a@example.com");
    assert_eq!(entries[3]["args"]["via"], "getbyrole");
    assert_eq!(entries[3]["args"]["name"], "Save");
}

#[test]
#[ignore = "requires Chrome and ffmpeg"]
fn human_input_mode_set_on_record_start_paces_later_commands() {
    let tmp = TempDir::new().unwrap();
    let video = tmp.path().join("take.webm");
    let log = tmp.path().join("take.actions.jsonl");

    run(&tmp, &["open", FIXTURE]);
    // A global flag on `record start` becomes the session default, so the
    // commands below glide and type like a person without repeating it.
    run(
        &tmp,
        &[
            "record",
            "start",
            video.to_str().unwrap(),
            "--log-actions",
            log.to_str().unwrap(),
            "--cursor",
            "--input-mode",
            "human",
        ],
    );
    run(&tmp, &["fill", "#email", "hello world"]);
    run(
        &tmp,
        &[
            "find", "role", "button", "click", "--name", "Save", "--exact",
        ],
    );
    let value = cli(&tmp, &["get", "value", "#email"]).output().unwrap();
    run(&tmp, &["record", "stop"]);
    let _ = cli(&tmp, &["close", "--all"]).status();
    assert_eq!(String::from_utf8_lossy(&value.stdout).trim(), "hello world");

    let entries: Vec<Value> = std::fs::read_to_string(&log)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let (fill, click) = (&entries[0], &entries[1]);
    assert_eq!(fill["action"], "fill", "{entries:?}");
    assert!(
        fill["target"]["x"].is_i64(),
        "fill should glide to the input: {fill}"
    );
    // 11 keystrokes at no less than 65ms each.
    assert!(fill["durationMs"].as_u64().unwrap() >= 700, "{fill}");
    assert_eq!(click["action"], "click", "{entries:?}");
    assert!(
        click["target"]["x"].is_i64(),
        "find click should glide: {click}"
    );
    assert!(
        click["durationMs"].as_u64().unwrap() >= 250,
        "click dwell: {click}"
    );
}
