//! Integration test for `find text` / `find role` disambiguation.
//! Marked `#[ignore]` because it depends on a working Chrome install.
//!
//! Page under test contains both `<a href="#">Login</a>` and
//! `<button>Login</button>`. The link wraps a different element so they're
//! independent in the DOM. Asserts:
//!   1. Default `find text "Login" click` exits non-zero with an "Ambiguous"
//!      error (no longer silently picks first leaf).
//!   2. `find text "Login" click --allow-ambiguous` succeeds.
//!   3. `find role button --name "Login" --exact click` succeeds.
//!   4. `find text "Login" click --no-prefer-interactive` exits non-zero
//!      (preference disabled, multiple non-interactive paths possible).

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

const FIXTURE: &str = "data:text/html,<html><body>\
    <a href=\"javascript:void(0)\" id=\"login-link\">Login</a>\
    <button id=\"login-btn\">Login</button>\
    <h1 id=\"login-heading\">Login to your account</h1>\
    </body></html>";

#[test]
#[ignore = "requires Chrome"]
fn find_text_default_fails_on_ambiguous_match() {
    let tmp = TempDir::new().unwrap();
    assert!(cli(&tmp, &["open", FIXTURE]).status().unwrap().success());
    std::thread::sleep(std::time::Duration::from_millis(500));

    let out = cli(&tmp, &["find", "text", "Login", "click"])
        .output()
        .expect("find text");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success() || stdout.contains("✗"),
        "expected failure, got status={:?} stdout={} stderr={}",
        out.status,
        stdout,
        stderr
    );
    let combined = format!("{}\n{}", stdout, stderr);
    assert!(
        combined.contains("Ambiguous"),
        "expected 'Ambiguous' in output, got: {}",
        combined
    );

    let _ = cli(&tmp, &["close", "--all"]).status();
}

#[test]
#[ignore = "requires Chrome"]
fn find_text_allow_ambiguous_succeeds() {
    let tmp = TempDir::new().unwrap();
    assert!(cli(&tmp, &["open", FIXTURE]).status().unwrap().success());
    std::thread::sleep(std::time::Duration::from_millis(500));

    let status = cli(
        &tmp,
        &["find", "text", "Login", "click", "--allow-ambiguous"],
    )
    .status()
    .expect("find text --allow-ambiguous");
    assert!(status.success(), "expected success with --allow-ambiguous");

    let _ = cli(&tmp, &["close", "--all"]).status();
}

#[test]
#[ignore = "requires Chrome"]
fn find_role_button_with_exact_name_succeeds() {
    let tmp = TempDir::new().unwrap();
    assert!(cli(&tmp, &["open", FIXTURE]).status().unwrap().success());
    std::thread::sleep(std::time::Duration::from_millis(500));

    let status = cli(
        &tmp,
        &[
            "find", "role", "button", "click", "--name", "Login", "--exact",
        ],
    )
    .status()
    .expect("find role button --name Login --exact");
    assert!(
        status.success(),
        "expected success for find role button --name Login --exact"
    );

    let _ = cli(&tmp, &["close", "--all"]).status();
}

#[test]
#[ignore = "requires Chrome"]
fn find_text_no_prefer_interactive_still_disambiguates() {
    let tmp = TempDir::new().unwrap();
    assert!(cli(&tmp, &["open", FIXTURE]).status().unwrap().success());
    std::thread::sleep(std::time::Duration::from_millis(500));

    let out = cli(
        &tmp,
        &[
            "find",
            "text",
            "Login",
            "click",
            "--no-prefer-interactive",
        ],
    )
    .output()
    .expect("find text --no-prefer-interactive");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let combined = format!("{}\n{}", stdout, stderr);
    assert!(
        combined.contains("Ambiguous"),
        "expected ambiguity error without interactive preference, got: {}",
        combined
    );

    let _ = cli(&tmp, &["close", "--all"]).status();
}

// Visibility filter: SSR-hydration ghosts and hidden mobile-menu duplicates
// should not push us into the ambiguity branch by default.
const HIDDEN_DUPES_FIXTURE: &str = "data:text/html,<html><body>\
    <button id=\"visible-login\">Login</button>\
    <button hidden>Login</button>\
    <div style=\"display:none\"><button>Login</button></div>\
    <nav aria-hidden=\"true\"><button>Login</button></nav>\
    </body></html>";

#[test]
#[ignore = "requires Chrome"]
fn find_role_filters_hidden_duplicates_by_default() {
    let tmp = TempDir::new().unwrap();
    assert!(cli(&tmp, &["open", HIDDEN_DUPES_FIXTURE])
        .status()
        .unwrap()
        .success());
    std::thread::sleep(std::time::Duration::from_millis(500));

    let out = cli(
        &tmp,
        &[
            "find", "role", "button", "click", "--name", "Login", "--exact",
        ],
    )
    .output()
    .expect("find role button --exact");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "expected success (only 1 visible match), got status={:?} stdout={} stderr={}",
        out.status,
        stdout,
        stderr
    );

    let _ = cli(&tmp, &["close", "--all"]).status();
}

#[test]
#[ignore = "requires Chrome"]
fn find_role_include_hidden_resurfaces_ambiguity() {
    let tmp = TempDir::new().unwrap();
    assert!(cli(&tmp, &["open", HIDDEN_DUPES_FIXTURE])
        .status()
        .unwrap()
        .success());
    std::thread::sleep(std::time::Duration::from_millis(500));

    let out = cli(
        &tmp,
        &[
            "find",
            "role",
            "button",
            "click",
            "--name",
            "Login",
            "--exact",
            "--include-hidden",
        ],
    )
    .output()
    .expect("find role button --include-hidden");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let combined = format!("{}\n{}", stdout, stderr);
    assert!(
        combined.contains("Ambiguous"),
        "expected ambiguity with --include-hidden (4 buttons), got: {}",
        combined
    );

    let _ = cli(&tmp, &["close", "--all"]).status();
}

#[test]
#[ignore = "requires Chrome"]
fn find_text_filters_hidden_duplicates_by_default() {
    let tmp = TempDir::new().unwrap();
    assert!(cli(&tmp, &["open", HIDDEN_DUPES_FIXTURE])
        .status()
        .unwrap()
        .success());
    std::thread::sleep(std::time::Duration::from_millis(500));

    let out = cli(&tmp, &["find", "text", "Login", "click", "--exact"])
        .output()
        .expect("find text --exact");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "expected success (only 1 visible match), got status={:?} stdout={} stderr={}",
        out.status,
        stdout,
        stderr
    );

    let _ = cli(&tmp, &["close", "--all"]).status();
}
