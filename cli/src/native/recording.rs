use serde_json::{json, Value};
use std::io::Write;
use std::path::Path;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::io::AsyncWriteExt;
use tokio::sync::oneshot;

use super::cdp::client::CdpClient;
use super::cdp::types::{CaptureScreenshotParams, CaptureScreenshotResult};
use super::cursor::{CursorConfig, CursorTracker};

/// Default capture rate. Higher rates require ffmpeg's libvpx encoder to
/// keep up; at 1440x900 with `-threads 1` the pipe back-pressures past
/// ~10 FPS on common hardware, starving the screenshot loop. Users who
/// want smoother cursor motion can pass `--fps 30` (and `--mouse-duration`
/// >= 600 to keep the move readable at any rate).
const ROBOTIC_FPS: u32 = 10;
const NATURAL_FPS: u32 = 10;

/// Knobs that change how a recording is produced. Stored on
/// [`RecordingState`] when `record start` runs and read by every action
/// handler that wants to behave differently while a recording is active.
#[derive(Debug, Clone)]
pub struct RecordingConfig {
    pub cursor: CursorConfig,
    pub fps: u32,
    pub log_actions_path: Option<String>,
}

impl RecordingConfig {
    /// Human-paced recording: visible cursor, animated moves, typing
    /// delays, 30 FPS capture. This is the default when the user runs
    /// `agent-browser record start <path>` with no flags.
    pub fn natural() -> Self {
        Self {
            cursor: CursorConfig::natural(),
            fps: NATURAL_FPS,
            log_actions_path: None,
        }
    }

    /// Legacy zero-pacing recording: no cursor, instant clicks, bulk
    /// `Input.insertText`, 10 FPS. Selected by `--robotic`.
    pub fn robotic() -> Self {
        Self {
            cursor: CursorConfig::robotic(),
            fps: ROBOTIC_FPS,
            log_actions_path: None,
        }
    }
}

impl Default for RecordingConfig {
    fn default() -> Self {
        Self::natural()
    }
}

pub struct RecordingState {
    pub active: bool,
    pub output_path: String,
    pub frame_count: u64,
    pub capture_task: Option<tokio::task::JoinHandle<Result<(), String>>>,
    pub shared_frame_count: Option<Arc<AtomicU64>>,
    pub cancel_tx: Option<oneshot::Sender<()>>,
    pub config: RecordingConfig,
    pub cursor: CursorTracker,
    pub started_at: Option<Instant>,
    pub action_log: Option<Arc<Mutex<std::io::BufWriter<std::fs::File>>>>,
}

impl RecordingState {
    pub fn new() -> Self {
        Self {
            active: false,
            output_path: String::new(),
            frame_count: 0,
            capture_task: None,
            shared_frame_count: None,
            cancel_tx: None,
            config: RecordingConfig::default(),
            cursor: CursorTracker::default(),
            started_at: None,
            action_log: None,
        }
    }

    /// Returns true when both a recording is active and the cursor overlay
    /// is enabled. Action handlers branch on this to decide whether to
    /// animate before dispatching real CDP input.
    pub fn auto_cursor_enabled(&self) -> bool {
        self.active && self.config.cursor.auto_cursor
    }
}

pub fn recording_start(state: &mut RecordingState, path: &str) -> Result<Value, String> {
    recording_start_with_config(state, path, RecordingConfig::default())
}

pub fn recording_start_with_config(
    state: &mut RecordingState,
    path: &str,
    config: RecordingConfig,
) -> Result<Value, String> {
    if state.active {
        return Err("Recording already active".to_string());
    }

    state.active = true;
    state.output_path = path.to_string();
    state.frame_count = 0;
    state.config = config;
    state.cursor = CursorTracker::default();
    state.started_at = Some(Instant::now());

    Ok(json!({ "started": true, "path": path }))
}

pub fn recording_stop(state: &mut RecordingState) -> Result<Value, String> {
    if !state.active {
        return Err("No recording in progress".to_string());
    }

    state.active = false;
    let log_path = state.config.log_actions_path.clone();

    if let Some(writer) = state.action_log.take() {
        if let Ok(mut w) = writer.lock() {
            let _ = w.flush();
        }
    }

    if state.frame_count == 0 {
        return Err("No frames captured".to_string());
    }

    let mut result = json!({ "path": &state.output_path, "frames": state.frame_count });
    if let Some(p) = log_path {
        result["actionLog"] = json!(p);
    }
    Ok(result)
}

pub fn recording_restart(state: &mut RecordingState, path: &str) -> Result<Value, String> {
    let previous_config = state.config.clone();
    recording_restart_with_config(state, path, previous_config)
}

pub fn recording_restart_with_config(
    state: &mut RecordingState,
    path: &str,
    config: RecordingConfig,
) -> Result<Value, String> {
    let previous = if state.active {
        let stop_result = recording_stop(state);
        stop_result
            .ok()
            .and_then(|v| v.get("path").and_then(|p| p.as_str()).map(String::from))
    } else {
        None
    };

    recording_start_with_config(state, path, config)?;

    Ok(json!({
        "restarted": true,
        "previousPath": previous,
        "path": path,
    }))
}

/// Open (or create + truncate) a JSONL action log. Returns a buffered writer
/// wrapped in `Arc<Mutex<>>` so action handlers and the recording shutdown
/// path can share it.
pub fn open_action_log(
    path: &str,
) -> Result<Arc<Mutex<std::io::BufWriter<std::fs::File>>>, String> {
    if let Some(parent) = Path::new(path).parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("Failed to create action-log directory: {}", e))?;
        }
    }
    let file = std::fs::File::create(path)
        .map_err(|e| format!("Failed to open action log {}: {}", path, e))?;
    Ok(Arc::new(Mutex::new(std::io::BufWriter::new(file))))
}

/// Append a single JSONL entry to the action log. No-op if no log is
/// configured for this recording. Flushes after every line so an
/// abruptly-killed daemon still leaves a usable timeline on disk.
pub fn append_action_log(state: &RecordingState, mut entry: Value) {
    let writer = match state.action_log.as_ref() {
        Some(w) => w.clone(),
        None => return,
    };

    if let Some(started) = state.started_at {
        entry["tsMs"] = json!(started.elapsed().as_millis() as u64);
    }
    if let Some(frames) = state.shared_frame_count.as_ref() {
        entry["frameIdx"] = json!(frames.load(Ordering::Relaxed));
    }

    let line = match serde_json::to_string(&entry) {
        Ok(s) => s,
        Err(_) => return,
    };
    let mut guard = match writer.lock() {
        Ok(g) => g,
        Err(_) => return,
    };
    let _ = writeln!(guard, "{}", line);
    let _ = guard.flush();
}

pub fn build_ffmpeg_command(output_path: &str, fps: u32) -> tokio::process::Command {
    let fps = fps.max(1);
    let mut cmd = tokio::process::Command::new("ffmpeg");

    cmd.args(["-y"])
        .args(["-avioflags", "direct"])
        .args([
            "-fpsprobesize",
            "0",
            "-probesize",
            "32",
            "-analyzeduration",
            "0",
        ])
        .args([
            "-f",
            "image2pipe",
            "-c:v",
            "mjpeg",
            "-framerate",
            &fps.to_string(),
            "-i",
            "pipe:0",
        ])
        .args(["-vf", "pad=ceil(iw/2)*2:ceil(ih/2)*2"]);

    if output_path.ends_with(".webm") {
        cmd.args(["-c:v", "libvpx", "-crf", "30", "-b:v", "1M"]);
    } else {
        cmd.args(["-c:v", "libx264", "-preset", "ultrafast"]);
    }

    // libvpx is single-threaded by design; keep the encoder pinned to one
    // thread for ≤10 FPS captures (matches the historical default), but
    // hand it the whole CPU for higher rates so the pipe doesn't back up
    // and starve the screenshot loop.
    let threads = if fps > 10 { "0" } else { "1" };

    cmd.args(["-pix_fmt", "yuv420p", "-threads", threads])
        .arg(output_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    cmd
}

/// Spawn a background task that captures screenshots at the configured rate
/// and pipes them to ffmpeg in real-time.
pub fn spawn_recording_task(
    client: Arc<CdpClient>,
    session_id: String,
    output_path: String,
    fps: u32,
    shared_count: Arc<AtomicU64>,
    cancel_rx: oneshot::Receiver<()>,
) -> tokio::task::JoinHandle<Result<(), String>> {
    tokio::spawn(async move {
        let mut cancel_rx = std::pin::pin!(cancel_rx);

        let mut ffmpeg = build_ffmpeg_command(&output_path, fps).spawn().map_err(|e| {
            format!(
                "ffmpeg not found or failed to execute: {}. Install ffmpeg to enable recording.",
                e
            )
        })?;

        let mut stdin = ffmpeg
            .stdin
            .take()
            .ok_or_else(|| "Failed to open ffmpeg stdin".to_string())?;

        let interval_ms = (1000_u64 / fps.max(1) as u64).max(1);
        let mut interval = tokio::time::interval(Duration::from_millis(interval_ms));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        let params = CaptureScreenshotParams {
            format: Some("jpeg".to_string()),
            quality: Some(80),
            clip: None,
            from_surface: Some(true),
            capture_beyond_viewport: None,
        };

        loop {
            tokio::select! {
                _ = &mut cancel_rx => break,
                _ = interval.tick() => {}
            }

            let result: Result<CaptureScreenshotResult, _> = client
                .send_command_typed("Page.captureScreenshot", &params, Some(&session_id))
                .await;

            let screenshot = match result {
                Ok(s) => s,
                Err(e) => {
                    if e.contains("Target closed") || e.contains("not found") {
                        break;
                    }
                    continue;
                }
            };

            let bytes = match base64::Engine::decode(
                &base64::engine::general_purpose::STANDARD,
                &screenshot.data,
            ) {
                Ok(b) => b,
                Err(_) => continue,
            };

            if stdin.write_all(&bytes).await.is_err() {
                break;
            }
            shared_count.fetch_add(1, Ordering::Relaxed);
        }

        drop(stdin);

        let output = ffmpeg
            .wait_with_output()
            .await
            .map_err(|e| format!("ffmpeg wait failed: {}", e))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!(
                "ffmpeg failed: {}",
                stderr.chars().take(300).collect::<String>()
            ));
        }

        Ok(())
    })
}

pub async fn stop_recording_task(state: &mut RecordingState) -> Result<(), String> {
    if let Some(tx) = state.cancel_tx.take() {
        let _ = tx.send(());
    }

    let counter = state.shared_frame_count.take();
    let handle = state.capture_task.take();

    let result = if let Some(h) = handle {
        match h.await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => Err(e),
            Err(e) => Err(format!("Recording task panicked: {}", e)),
        }
    } else {
        Ok(())
    };

    if let Some(c) = counter {
        state.frame_count = c.load(Ordering::Relaxed);
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_recording_state_new() {
        let state = RecordingState::new();
        assert!(!state.active);
        assert!(state.output_path.is_empty());
        assert_eq!(state.frame_count, 0);
    }

    #[test]
    fn test_recording_start_sets_active() {
        let mut state = RecordingState::new();
        let result = recording_start(&mut state, "/tmp/test.mp4");
        assert!(result.is_ok());
        assert!(state.active);
        assert_eq!(state.output_path, "/tmp/test.mp4");
        assert_eq!(state.frame_count, 0);
    }

    #[test]
    fn test_recording_start_while_active() {
        let mut state = RecordingState::new();
        recording_start(&mut state, "/tmp/test1.mp4").unwrap();
        let result = recording_start(&mut state, "/tmp/test2.mp4");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("already active"));
    }

    #[test]
    fn test_recording_stop_not_active() {
        let mut state = RecordingState::new();
        let result = recording_stop(&mut state);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("No recording"));
    }

    #[test]
    fn test_recording_stop_no_frames() {
        let mut state = RecordingState::new();
        recording_start(&mut state, "/tmp/test.mp4").unwrap();
        let result = recording_stop(&mut state);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("No frames"));
        assert!(!state.active);
    }

    #[test]
    fn test_recording_restart_while_inactive() {
        let mut state = RecordingState::new();
        let result = recording_restart(&mut state, "/tmp/new.webm");
        assert!(result.is_ok());
        assert!(state.active);
        assert_eq!(state.output_path, "/tmp/new.webm");
    }

    #[test]
    fn test_recording_restart_while_active() {
        let mut state = RecordingState::new();
        recording_start(&mut state, "/tmp/old.webm").unwrap();
        state.frame_count = 10;
        let result = recording_restart(&mut state, "/tmp/new.webm").unwrap();
        assert!(state.active);
        assert_eq!(state.output_path, "/tmp/new.webm");
        assert_eq!(state.frame_count, 0);
        assert_eq!(result["previousPath"], "/tmp/old.webm");
    }

    #[test]
    fn test_build_ffmpeg_command_webm() {
        let cmd = build_ffmpeg_command("/tmp/out.webm", 10);
        let args: Vec<&std::ffi::OsStr> = cmd.as_std().get_args().collect();
        let args_str: Vec<&str> = args.iter().filter_map(|a| a.to_str()).collect();
        assert!(args_str.contains(&"libvpx"));
        assert!(args_str.contains(&"/tmp/out.webm"));
    }

    #[test]
    fn test_build_ffmpeg_command_mp4() {
        let cmd = build_ffmpeg_command("/tmp/out.mp4", 10);
        let args: Vec<&std::ffi::OsStr> = cmd.as_std().get_args().collect();
        let args_str: Vec<&str> = args.iter().filter_map(|a| a.to_str()).collect();
        assert!(args_str.contains(&"libx264"));
        assert!(args_str.contains(&"/tmp/out.mp4"));
    }

    #[test]
    fn test_build_ffmpeg_command_natural_fps() {
        let cmd = build_ffmpeg_command("/tmp/out.webm", 30);
        let args: Vec<&std::ffi::OsStr> = cmd.as_std().get_args().collect();
        let args_str: Vec<&str> = args.iter().filter_map(|a| a.to_str()).collect();
        let i = args_str.iter().position(|a| *a == "-framerate").unwrap();
        assert_eq!(args_str[i + 1], "30");
    }

    #[test]
    fn test_recording_config_default_is_natural() {
        let cfg = RecordingConfig::default();
        assert!(cfg.cursor.auto_cursor);
        assert_eq!(cfg.fps, NATURAL_FPS);
        assert!(cfg.log_actions_path.is_none());
    }

    #[test]
    fn test_recording_config_robotic_matches_legacy() {
        let cfg = RecordingConfig::robotic();
        assert!(!cfg.cursor.auto_cursor);
        assert_eq!(cfg.fps, ROBOTIC_FPS);
        assert_eq!(cfg.cursor.type_delay_ms, 0);
    }

    #[test]
    fn test_recording_start_with_config_stores_config() {
        let mut state = RecordingState::new();
        let mut cfg = RecordingConfig::natural();
        cfg.fps = 24;
        cfg.cursor.mouse_duration_ms = 600;
        recording_start_with_config(&mut state, "/tmp/out.webm", cfg).unwrap();
        assert_eq!(state.config.fps, 24);
        assert_eq!(state.config.cursor.mouse_duration_ms, 600);
        assert!(state.started_at.is_some());
    }

    #[test]
    fn test_auto_cursor_enabled_requires_active_recording() {
        let mut state = RecordingState::new();
        // Default config has auto_cursor on, but recording is not active.
        assert!(!state.auto_cursor_enabled());

        recording_start(&mut state, "/tmp/out.webm").unwrap();
        assert!(state.auto_cursor_enabled());
    }

    #[test]
    fn test_auto_cursor_disabled_in_robotic_mode() {
        let mut state = RecordingState::new();
        recording_start_with_config(&mut state, "/tmp/out.webm", RecordingConfig::robotic())
            .unwrap();
        assert!(state.active);
        assert!(!state.auto_cursor_enabled());
    }

    #[test]
    fn test_open_action_log_creates_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("actions.jsonl");
        let writer = open_action_log(path.to_str().unwrap()).unwrap();
        assert!(path.exists());
        // Drop the writer to flush.
        drop(writer);
    }

    #[test]
    fn test_append_action_log_writes_jsonl() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("actions.jsonl");
        let mut state = RecordingState::new();
        state.action_log = Some(open_action_log(path.to_str().unwrap()).unwrap());
        state.started_at = Some(Instant::now());
        state.shared_frame_count = Some(Arc::new(AtomicU64::new(7)));

        append_action_log(&state, json!({ "action": "click", "ok": true }));
        append_action_log(&state, json!({ "action": "fill", "ok": true }));

        // Drop the writer so the BufWriter flushes.
        if let Some(w) = state.action_log.take() {
            drop(w);
        }

        let contents = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = contents.lines().collect();
        assert_eq!(lines.len(), 2);
        let first: Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(first["action"], "click");
        assert_eq!(first["ok"], true);
        assert_eq!(first["frameIdx"], 7);
        assert!(first["tsMs"].is_u64());
    }

    #[test]
    fn test_append_action_log_noop_without_writer() {
        let state = RecordingState::new();
        // Should not panic.
        append_action_log(&state, json!({ "action": "click" }));
    }
}
