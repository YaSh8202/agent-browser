#!/usr/bin/env bash
#
# Smoke-test the natural-mode recording flags against github.com.
#
# Records a short tour: opens github.com home, hovers a nav link, navigates
# to /login, then types into the username + password fields with
# human-paced jittered keystrokes. Outputs a .webm video and a
# .actions.jsonl sidecar that can be inspected for action timings, target
# coordinates, and frame indices.
#
# Usage:
#   bash scripts/demo-github.sh                       # writes /tmp/demo-github.{webm,actions.jsonl}
#   bash scripts/demo-github.sh ./my-demo.webm        # custom output path
#
# Requirements:
#   - agent-browser available (built locally or installed globally)
#   - ffmpeg available on PATH
#   - Chrome installed via `agent-browser install` (one-time)
#
set -uo pipefail

OUT=${1:-/tmp/demo-github.webm}
LOG=${OUT%.webm}.actions.jsonl

# Resolve the local debug or release build first, then fall back to PATH.
SCRIPT_DIR=$(cd "$(dirname "$0")" && pwd)
ROOT_DIR=$(cd "$SCRIPT_DIR/.." && pwd)
if [[ -x "$ROOT_DIR/cli/target/release/agent-browser" ]]; then
  AB="$ROOT_DIR/cli/target/release/agent-browser"
elif [[ -x "$ROOT_DIR/cli/target/debug/agent-browser" ]]; then
  AB="$ROOT_DIR/cli/target/debug/agent-browser"
else
  AB="agent-browser"
fi

echo "Using agent-browser: $AB"
echo "Output video: $OUT"
echo "Action log:   $LOG"

mkdir -p "$(dirname "$OUT")"

# Always finalise the recording on exit so a partial failure still yields
# usable output. `record stop` is a no-op if no recording is active.
finalize() {
  local rc=$?
  echo "--- finalising (rc=$rc) ---"
  "$AB" record stop >/dev/null 2>&1 || true
  "$AB" close --all >/dev/null 2>&1 || true
  return "$rc"
}
trap finalize EXIT

# Fresh canvas: ensure no leftover session is recording.
"$AB" close --all >/dev/null 2>&1 || true

"$AB" set viewport 1440 900
"$AB" record start "$OUT" \
  --type-delay 70 --type-jitter 30 \
  --mouse-duration 600 --settle-ms 250 \
  --log-actions "$LOG"

echo "Step: opening github.com"
"$AB" open https://github.com
sleep 1

echo "Step: hovering Pricing nav"
"$AB" find text "Pricing" hover || true

echo "Step: navigating to /login"
"$AB" open https://github.com/login
"$AB" wait 'input[name=login]' --timeout 10000

echo "Step: filling username (human-paced)"
"$AB" fill 'input[name=login]' "octocat-demo"

echo "Step: hopping to password with Tab"
"$AB" press Tab

echo "Step: filling password (human-paced)"
"$AB" fill 'input[name=password]' "do-not-actually-log-in"

echo "Step: idling so the viewer can read the typed credentials"
"$AB" wait 1500

echo "----------------------------------------------------------------"
echo "Done. Video: $OUT"
echo "Action log: $LOG"
echo
echo "Inspect the action log:"
echo "  cat \"$LOG\" | head"
echo
echo "Convert to mp4:"
echo "  ffmpeg -y -i \"$OUT\" -c:v libx264 -pix_fmt yuv420p -movflags +faststart \"${OUT%.webm}.mp4\""
echo "----------------------------------------------------------------"
