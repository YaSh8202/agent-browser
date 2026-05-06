// Visible cursor overlay + human-paced timing knobs for natural-looking
// recordings. Drawn entirely in the page DOM (fixed-position <div> with an
// SVG pointer) so the recorded video shows where the agent is acting.
//
// The actual CDP `Input.dispatchMouseEvent` is still fired only at the final
// coordinates -- the page sees one move, not the animation path -- so hover
// handlers don't fire spuriously along the route.

use serde_json::{json, Value};
use std::time::Duration;

use super::cdp::client::CdpClient;
use super::cdp::types::{EvaluateParams, EvaluateResult};

/// Floor for mouse animation duration. At any reasonable capture rate, a
/// move shorter than this looks jumpy (1-2 frames at 10 FPS, 4-5 at 30 FPS).
pub const MOUSE_DURATION_FLOOR_MS: u64 = 200;

#[derive(Debug, Clone, Copy)]
pub struct CursorConfig {
    pub auto_cursor: bool,
    pub mouse_duration_ms: u64,
    pub settle_ms: u64,
    pub type_delay_ms: u64,
    pub type_jitter_ms: u64,
}

impl CursorConfig {
    /// Defaults that make recordings look human-paced.
    pub fn natural() -> Self {
        Self {
            auto_cursor: true,
            mouse_duration_ms: 450,
            settle_ms: 250,
            type_delay_ms: 65,
            type_jitter_ms: 25,
        }
    }

    /// Legacy zero-delay behaviour for callers that explicitly opt out
    /// (`record start ... --robotic`).
    pub fn robotic() -> Self {
        Self {
            auto_cursor: false,
            mouse_duration_ms: 0,
            settle_ms: 0,
            type_delay_ms: 0,
            type_jitter_ms: 0,
        }
    }
}

impl Default for CursorConfig {
    fn default() -> Self {
        Self::natural()
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct CursorTracker {
    pub x: f64,
    pub y: f64,
}

/// JS that defines `window.__abCursor`. Idempotent: repeated injection is
/// a no-op. Installed via both `Page.addScriptToEvaluateOnNewDocument`
/// (so it survives navigations) and a one-shot `Runtime.evaluate` (so it
/// applies to the document already loaded when recording starts).
pub const CURSOR_INIT_SCRIPT: &str = r#"(function(){
if (window.__abCursor) return;
var SVG = '<svg width="24" height="24" viewBox="0 0 24 24" xmlns="http://www.w3.org/2000/svg"><path d="M3 2 L3 18 L7.5 14 L10 20 L13 19 L10.5 13 L17 13 Z" fill="white" stroke="black" stroke-width="1.2" stroke-linejoin="round"/></svg>';
function root(){ return document.getElementById('__ab_cursor_root'); }
function init(){
  if (root()) return;
  var doc = document.documentElement || document.body;
  if (!doc) return;
  var el = document.createElement('div');
  el.id = '__ab_cursor_root';
  el.setAttribute('aria-hidden','true');
  el.style.cssText = 'position:fixed;top:0;left:0;width:24px;height:24px;margin:0;padding:0;pointer-events:none;z-index:2147483647;transform:translate3d(0px,0px,0);transition:transform 0ms linear;will-change:transform';
  el.innerHTML = SVG;
  doc.appendChild(el);
}
window.__abCursor = {
  init: init,
  moveTo: function(x, y, ms){
    var el = root();
    if (!el){ init(); el = root(); }
    if (!el) return;
    el.style.transition = 'transform ' + ((ms|0) || 0) + 'ms ease-out';
    el.style.transform = 'translate3d(' + (x|0) + 'px,' + (y|0) + 'px,0)';
  }
};
if (document.readyState === 'loading') {
  document.addEventListener('DOMContentLoaded', init);
} else {
  init();
}
})();"#;

/// Inject the cursor overlay so it survives every future navigation in
/// this session, and also paint it on the document that's currently loaded.
pub async fn install(client: &CdpClient, session_id: &str) -> Result<(), String> {
    client
        .send_command(
            "Page.addScriptToEvaluateOnNewDocument",
            Some(json!({ "source": CURSOR_INIT_SCRIPT })),
            Some(session_id),
        )
        .await?;

    let _ = client
        .send_command_typed::<_, EvaluateResult>(
            "Runtime.evaluate",
            &EvaluateParams {
                expression: CURSOR_INIT_SCRIPT.to_string(),
                return_by_value: Some(true),
                await_promise: Some(false),
            },
            Some(session_id),
        )
        .await;

    Ok(())
}

/// Animate the on-screen cursor to (x, y), then sleep long enough that the
/// recording captures the move plus a brief settle. Updates `tracker` to the
/// final position. No-op when `cfg.auto_cursor` is false.
pub async fn animate_to(
    client: &CdpClient,
    session_id: &str,
    tracker: &mut CursorTracker,
    x: f64,
    y: f64,
    cfg: &CursorConfig,
) -> Result<(), String> {
    tracker.x = x;
    tracker.y = y;

    if !cfg.auto_cursor {
        return Ok(());
    }

    let duration_ms = cfg.mouse_duration_ms.max(MOUSE_DURATION_FLOOR_MS);
    let expr = format!(
        "window.__abCursor && window.__abCursor.moveTo({}, {}, {})",
        x as i64, y as i64, duration_ms
    );

    let _ = client
        .send_command_typed::<_, Value>(
            "Runtime.evaluate",
            &EvaluateParams {
                expression: expr,
                return_by_value: Some(true),
                await_promise: Some(false),
            },
            Some(session_id),
        )
        .await;

    tokio::time::sleep(Duration::from_millis(duration_ms + cfg.settle_ms)).await;
    Ok(())
}

/// Uniform jitter in `[base, base + jitter]` ms drawn from system entropy.
/// Use [`jittered_delay_seeded`] in tests for determinism.
pub fn jittered_delay_ms(base: u64, jitter: u64) -> u64 {
    if jitter == 0 {
        return base;
    }
    let mut buf = [0u8; 8];
    if getrandom::getrandom(&mut buf).is_err() {
        return base;
    }
    base + (u64::from_le_bytes(buf) % (jitter + 1))
}

/// Deterministic xorshift64 variant of [`jittered_delay_ms`] for unit tests.
pub fn jittered_delay_seeded(base: u64, jitter: u64, rng: &mut u64) -> u64 {
    if jitter == 0 {
        return base;
    }
    if *rng == 0 {
        *rng = 0x9E3779B97F4A7C15;
    }
    *rng ^= *rng << 13;
    *rng ^= *rng >> 7;
    *rng ^= *rng << 17;
    base + (*rng % (jitter + 1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn natural_defaults_enable_cursor() {
        let cfg = CursorConfig::natural();
        assert!(cfg.auto_cursor);
        assert!(cfg.mouse_duration_ms >= MOUSE_DURATION_FLOOR_MS);
        assert!(cfg.type_delay_ms > 0);
    }

    #[test]
    fn robotic_defaults_disable_everything() {
        let cfg = CursorConfig::robotic();
        assert!(!cfg.auto_cursor);
        assert_eq!(cfg.mouse_duration_ms, 0);
        assert_eq!(cfg.type_delay_ms, 0);
        assert_eq!(cfg.type_jitter_ms, 0);
        assert_eq!(cfg.settle_ms, 0);
    }

    #[test]
    fn default_is_natural() {
        let a = CursorConfig::default();
        let b = CursorConfig::natural();
        assert_eq!(a.auto_cursor, b.auto_cursor);
        assert_eq!(a.mouse_duration_ms, b.mouse_duration_ms);
        assert_eq!(a.type_delay_ms, b.type_delay_ms);
    }

    #[test]
    fn cursor_init_script_is_idempotent() {
        // The guard at the very top is what prevents double-installation
        // across navigations + the manual one-shot evaluate.
        assert!(CURSOR_INIT_SCRIPT.contains("if (window.__abCursor) return"));
        // And the overlay element is also guarded inside init().
        assert!(CURSOR_INIT_SCRIPT.contains("if (root()) return"));
    }

    #[test]
    fn cursor_init_script_uses_top_zindex_and_no_pointer_events() {
        // Overlay must never intercept clicks or be hidden behind site UI.
        assert!(CURSOR_INIT_SCRIPT.contains("pointer-events:none"));
        assert!(CURSOR_INIT_SCRIPT.contains("2147483647"));
        assert!(CURSOR_INIT_SCRIPT.contains("position:fixed"));
    }

    #[test]
    fn cursor_init_script_exposes_move_to() {
        assert!(CURSOR_INIT_SCRIPT.contains("moveTo: function"));
        assert!(CURSOR_INIT_SCRIPT.contains("translate3d("));
    }

    #[test]
    fn jittered_delay_zero_jitter_is_exact() {
        assert_eq!(jittered_delay_ms(50, 0), 50);
        let mut rng: u64 = 1;
        assert_eq!(jittered_delay_seeded(50, 0, &mut rng), 50);
    }

    #[test]
    fn jittered_delay_seeded_is_deterministic_and_in_bounds() {
        let mut rng: u64 = 42;
        for _ in 0..200 {
            let v = jittered_delay_seeded(60, 30, &mut rng);
            assert!((60..=90).contains(&v), "v={} out of [60,90]", v);
        }
    }

    #[test]
    fn jittered_delay_unseeded_in_bounds() {
        for _ in 0..200 {
            let v = jittered_delay_ms(100, 50);
            assert!((100..=150).contains(&v));
        }
    }
}
