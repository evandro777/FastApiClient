#!/usr/bin/env bash
# Renders pages of the app on a private Xvfb display with canned data (FASTAPICLIENT_FIXTURES) and saves PNGs to
# target/ui-snapshots/<page>-<mode>.png. The real system is never scanned or changed.
# Usage: scripts/ui-snapshot.sh [page ...]   (names or numbers; default all)   MODES="empty single many" to narrow.
set -euo pipefail
cd "$(dirname "$0")/.."

# Add one entry per page, with the number the app's `tab` property uses (request tabs; the response panel is on all).
# MODES="error" renders the failed-request state; the default modes cover empty, one and many rows.
# SELECT=project or SELECT=folder opens a project or folder (headers and auth only) instead of a request;
# SELECT=other opens the second project (modes with one project fall back to the first).
declare -A PAGES=([params]=0 [headers]=1 [body]=2 [auth]=3 [about]=about [menu]=menu [envs]=envs [export]=export [import]=import [insomnia]=insomnia [manage]=manage [sendfile]=sendfile [vars]=vars)
MODES=${MODES:-"empty single many"}
OUT=target/ui-snapshots
DISPLAY_NUM=:99
mkdir -p "$OUT"

cargo build --quiet

Xvfb "$DISPLAY_NUM" -screen 0 1920x1080x24 >/dev/null 2>&1 &
XVFB=$!
APP=
cleanup() { [ -n "$APP" ] && kill "$APP" 2>/dev/null || true; kill "$XVFB" 2>/dev/null || true; }
trap cleanup EXIT
sleep 1

[ $# -gt 0 ] && names=("$@") || names=("${!PAGES[@]}")

for name in "${names[@]}"; do
  page=${PAGES[$name]:-$name}
  for mode in $MODES; do
    DISPLAY=$DISPLAY_NUM FASTAPICLIENT_FIXTURES=$mode FASTAPICLIENT_PAGE=$page FASTAPICLIENT_SELECT=${SELECT:-} target/debug/fastapiclient >/dev/null 2>&1 &
    APP=$!
    for _ in $(seq 50); do
      DISPLAY=$DISPLAY_NUM xdotool search --name FastApiClient >/dev/null 2>&1 && break
      sleep 0.2
    done
    sleep 1
    file="$OUT/$name${SELECT:+-$SELECT}-$mode.png"
    if command -v import >/dev/null; then
      DISPLAY=$DISPLAY_NUM import -window root "$file"
    else # no ImageMagick: xwd + ffmpeg
      xwd -root -silent -display "$DISPLAY_NUM" | ffmpeg -y -loglevel error -f xwd_pipe -i - "$file"
    fi
    kill "$APP" 2>/dev/null || true
    wait "$APP" 2>/dev/null || true
    APP=
    echo "$file"
  done
done
