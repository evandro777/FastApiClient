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
# WINDOW_SIZE=900x560 checks the minimum window size; FASTAPICLIENT_THEME=light|dark selects the palette.
declare -A PAGES=([params]=0 [headers]=1 [body]=2 [auth]=3 [about]=about [menu]=menu [envs]=envs [export]=export [import]=import [insomnia]=insomnia [manage]=manage [sendfile]=sendfile [vars]=vars [validation-json]=validation-json [validation-xml]=validation-xml [validation-dtd]=validation-dtd [payload-errors]=payload-errors [payload-lines]=payload-lines [payload-empty]=payload-empty [highlight-json]=highlight-json [highlight-xml]=highlight-xml [highlight-limit]=highlight-limit [response-raw]=response-raw [response-normal]=response-normal [response-find]=response-find [response-notice]=response-notice)
MODES=${MODES:-"empty single many"}
PAGES[filter]=filter
PAGES[filter-none]=filter-none
PAGES[prompt]=prompt
PAGES[new-folder]=new-folder
PAGES[new-request]=new-request
PAGES[folder-menu]=folder-menu
PAGES[save-error]=save-error
PAGES[load-error]=load-error
PAGES[startup-error]=startup-error
PAGES[cache-limit]=cache-limit
PAGES[delete-error]=delete-error
PAGES[import-headers]=import-headers
PAGES[validation-limit]=validation-limit
PAGES[response-xml]=response-xml
PAGES[sending]=sending
OUT=${OUT:-target/ui-snapshots}
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
    WINDOW_ID=
    for _ in $(seq 50); do
      if WINDOW_ID=$(DISPLAY=$DISPLAY_NUM xdotool search --onlyvisible --limit 1 --name FastApiClient); then break; fi
      sleep 0.2
    done
    [ -n "$WINDOW_ID" ] || { echo "No visible app window for $name-$mode" >&2; exit 1; }
    if [[ -n ${WINDOW_SIZE:-} ]]; then
      DISPLAY=$DISPLAY_NUM xdotool windowsize "$WINDOW_ID" "${WINDOW_SIZE%x*}" "${WINDOW_SIZE#*x}"
    fi
    sleep 1
    file="$OUT/$name${SELECT:+-$SELECT}-$mode.png"
    if command -v import >/dev/null; then
      DISPLAY=$DISPLAY_NUM import -window "$WINDOW_ID" "$file"
    else # no ImageMagick: xwd + ffmpeg
      xwd -id "$WINDOW_ID" -silent -display "$DISPLAY_NUM" | ffmpeg -y -loglevel error -f xwd_pipe -i - "$file"
    fi
    kill "$APP" 2>/dev/null || true
    wait "$APP" 2>/dev/null || true
    APP=
    echo "$file"
  done
done
