//! Integration test for `wait --stable <ms>`: resolves once a
//! `MutationObserver` reports no DOM mutations for the given window.
//!
//! Replaces the magic `wait 1500` sleeps that scene scripts scattered.
//! Marked `#[ignore]` because it depends on a working Chrome install.

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

// Static page — wait --stable should resolve almost immediately because the
// MutationObserver never fires.
const STATIC_FIXTURE: &str =
    "data:text/html,<html><body><h1>Hello</h1><p>static</p></body></html>";

#[test]
#[ignore = "requires Chrome"]
fn wait_stable_resolves_quickly_on_static_page() {
    let tmp = TempDir::new().unwrap();
    assert!(cli(&tmp, &["open", STATIC_FIXTURE]).status().unwrap().success());
    std::thread::sleep(std::time::Duration::from_millis(300));

    let start = Instant::now();
    let status = cli(
        &tmp,
        &["wait", "--stable", "300", "--timeout", "5000"],
    )
    .status()
    .expect("wait --stable on static page");
    let elapsed = start.elapsed();

    assert!(status.success(), "wait --stable should succeed on static page");
    assert!(
        elapsed.as_millis() < 2_000,
        "expected fast resolution (<2s), took {}ms",
        elapsed.as_millis()
    );

    let _ = cli(&tmp, &["close", "--all"]).status();
}

// A page that mutates every 100ms but stops after ~600ms. `--stable 300` must
// not return until the mutations have ceased — so total time ≥ ~900ms.
const MUTATING_FIXTURE: &str = "data:text/html,<html><body>\
    <div id=\"target\">0</div>\
    <script>\
    let i = 0;\
    const id = setInterval(() => {\
        document.getElementById('target').textContent = String(++i);\
        if (i >= 6) clearInterval(id);\
    }, 100);\
    </script>\
    </body></html>";

#[test]
#[ignore = "requires Chrome"]
fn wait_stable_waits_for_mutations_to_settle() {
    let tmp = TempDir::new().unwrap();
    assert!(cli(&tmp, &["open", MUTATING_FIXTURE]).status().unwrap().success());
    // Don't sleep too long here — we want to enter wait --stable while the
    // page is still mutating.

    let start = Instant::now();
    let status = cli(
        &tmp,
        &["wait", "--stable", "300", "--timeout", "5000"],
    )
    .status()
    .expect("wait --stable on mutating page");
    let elapsed = start.elapsed();

    assert!(status.success(), "wait --stable should succeed once settled");
    // Mutations run for 600ms then 300ms quiet = ~900ms floor. Allow
    // generous CI variance.
    assert!(
        elapsed.as_millis() >= 600,
        "expected to wait for mutations (>=600ms), took {}ms",
        elapsed.as_millis()
    );
    assert!(
        elapsed.as_millis() < 4_500,
        "expected to settle before timeout (<4.5s), took {}ms",
        elapsed.as_millis()
    );

    let _ = cli(&tmp, &["close", "--all"]).status();
}

// A page that never stops mutating must time out, not report success. The
// interval stays well under `--stable` even if Chrome throttles timers to 1s.
const CHURNING_FIXTURE: &str = "data:text/html,<html><body>\
    <div id=\"target\">0</div>\
    <script>\
    let i = 0;\
    setInterval(() => { document.getElementById('target').textContent = String(++i); }, 100);\
    </script>\
    </body></html>";

#[test]
#[ignore = "requires Chrome"]
fn wait_stable_times_out_on_churning_page() {
    let tmp = TempDir::new().unwrap();
    assert!(cli(&tmp, &["open", CHURNING_FIXTURE]).status().unwrap().success());

    let status = cli(&tmp, &["wait", "--stable", "1500", "--timeout", "3000"])
        .status()
        .expect("wait --stable on churning page");

    assert!(!status.success(), "wait --stable must fail when the DOM never settles");

    let _ = cli(&tmp, &["close", "--all"]).status();
}
