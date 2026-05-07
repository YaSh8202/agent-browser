//! Integration test: `wait --url` must not deadlock the daemon while a
//! recording task is concurrently capturing screenshots.
//!
//! Original bug repro: with the old polling `wait_for_url` competing for the
//! shared CDP connection, recording's 100ms `Page.captureScreenshot` calls
//! could starve `Runtime.evaluate` responses past the IPC client's 30s
//! `read_timeout`. The IPC client would then retry by re-sending the wait
//! command — queuing more `state.lock()` holders behind the original. After
//! 5 retries (≈150s wall-clock) it errored with
//!   `Failed to read: Resource temporarily unavailable (os error 35)`.
//!
//! What this test asserts:
//!   1. With recording active, `wait --url <non-matching>` returns at the
//!      *configured* timeout (≈ T) — not at IPC's 30s × 5 retries (≈150s).
//!   2. The error is the daemon's `Wait timed out after Xms` (proof the
//!      daemon-side reply made it back), not the IPC retry-exhausted
//!      `Resource temporarily unavailable`.
//!
//! `#[ignore]` — needs Chrome + ffmpeg.

use std::path::PathBuf;
use std::process::Command;
use std::time::Instant;
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
fn wait_for_url_does_not_deadlock_daemon_during_recording() {
    let tmp = TempDir::new().unwrap();
    let video: PathBuf = tmp.path().join("rec.webm");

    // Start recording at high FPS to maximize CDP contention.
    let status = cli(
        &tmp,
        &[
            "record",
            "start",
            video.to_str().unwrap(),
            "data:text/html,<h1>start</h1>",
            "--mouse-duration",
            "100",
            "--settle-ms",
            "0",
            "--type-delay",
            "0",
            "--fps",
            "30",
        ],
    )
    .status()
    .expect("record start");
    assert!(status.success(), "record start failed");

    // Wait for a URL pattern that will never match, with an 8s timeout.
    // Pre-fix: this would hang the daemon, IPC would retry 5x at 30s each → ~150s.
    // Post-fix: daemon replies with "Wait timed out after 8000ms" at ~8s.
    let started = Instant::now();
    let out = cli(&tmp, &["wait", "--url", "no-match-pattern-xyz"])
        .env("AGENT_BROWSER_DEFAULT_TIMEOUT", "8000")
        .output()
        .expect("wait --url");
    let elapsed = started.elapsed();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let combined = format!("{}\n{}", stdout, stderr);

    // The wait should have failed with the daemon's timeout error (correct
    // behavior — pattern never matches), not with the IPC retry-exhausted
    // error from before the fix.
    assert!(
        combined.contains("Wait timed out after"),
        "expected daemon timeout error, got combined={} elapsed={:?}",
        combined,
        elapsed
    );
    assert!(
        !combined.contains("Resource temporarily unavailable")
            && !combined.contains("after 5 retries"),
        "expected no IPC retry-exhausted error (daemon was not hanging), got: {}",
        combined
    );
    // Allow generous slack for CI: 8s timeout + up to 7s of overhead.
    assert!(
        elapsed < std::time::Duration::from_secs(15),
        "wait took {:?} during recording — daemon was likely hanging (expected ~8s)",
        elapsed
    );

    let stop = cli(&tmp, &["record", "stop"]).output().expect("record stop");
    assert!(
        stop.status.success(),
        "record stop failed: {}",
        String::from_utf8_lossy(&stop.stderr)
    );
    let _ = cli(&tmp, &["close", "--all"]).status();

    let video_meta = std::fs::metadata(&video).expect("video file");
    assert!(video_meta.len() > 0, "webm should be non-empty");
}
