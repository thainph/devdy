#!/usr/bin/env bash
# Push an exported Slack thread (.md or .zip) to Devdy's local Inbox API.
#
# Usage: ./scripts/send-slack-thread.sh <file.md|file.zip> [token] [port]
#
# token: arg 2, else $DEVDY_INBOX_TOKEN (copy it from Devdy → Settings → Inbox API)
# port:  arg 3, else $DEVDY_INBOX_PORT, else auto-detect 47821..47830 via /health
# Optional env: DEVDY_INBOX_PROJECT_ID (X-Devdy-Project-Id), DEVDY_INBOX_TITLE (X-Devdy-Title),
#   DEVDY_INBOX_MODE (X-Devdy-Mode, web pages only),
#   DEVDY_INBOX_ENDPOINT (slack-threads | web-pages; default slack-threads)
set -euo pipefail

if [[ $# -lt 1 || "$1" == "-h" || "$1" == "--help" ]]; then
  sed -n '2,10p' "$0" | sed 's/^# \{0,1\}//'
  exit 1
fi

file="$1"
token="${2:-${DEVDY_INBOX_TOKEN:-}}"
port="${3:-${DEVDY_INBOX_PORT:-}}"

if [[ ! -f "$file" ]]; then
  echo "error: file not found: $file" >&2
  exit 1
fi
if [[ -z "$token" ]]; then
  echo "error: no token (pass it as arg 2 or set DEVDY_INBOX_TOKEN)" >&2
  exit 1
fi

if [[ -z "$port" ]]; then
  # Probe every port: a production and a dev build can run side by side, and
  # silently picking the first one could send to the wrong app.
  found=()
  for p in $(seq 47821 47830); do
    if curl -fsS --max-time 1 "http://127.0.0.1:$p/health" 2>/dev/null | grep -q '"app":"devdy"'; then
      found+=("$p")
    fi
  done
  if [[ ${#found[@]} -eq 0 ]]; then
    echo "error: Devdy Inbox API not found on 127.0.0.1:47821-47830 (is Devdy running?)" >&2
    exit 1
  fi
  if [[ ${#found[@]} -gt 1 ]]; then
    echo "error: several Devdy instances answer on ports: ${found[*]}" >&2
    echo "       pass the port explicitly (arg 3 or DEVDY_INBOX_PORT); see Devdy → Settings → Inbox API" >&2
    exit 1
  fi
  port="${found[0]}"
fi
echo "→ sending to 127.0.0.1:$port" >&2

case "$(printf '%s' "${file##*.}" | tr '[:upper:]' '[:lower:]')" in
  zip) content_type="application/zip" ;;
  md | markdown) content_type="text/markdown; charset=utf-8" ;;
  *)
    echo "error: expected a .md or .zip file" >&2
    exit 1
    ;;
esac

extra=()
[[ -n "${DEVDY_INBOX_PROJECT_ID:-}" ]] && extra+=(-H "X-Devdy-Project-Id: $DEVDY_INBOX_PROJECT_ID")
[[ -n "${DEVDY_INBOX_TITLE:-}" ]] && extra+=(-H "X-Devdy-Title: $DEVDY_INBOX_TITLE")
[[ -n "${DEVDY_INBOX_MODE:-}" ]] && extra+=(-H "X-Devdy-Mode: $DEVDY_INBOX_MODE")
endpoint="${DEVDY_INBOX_ENDPOINT:-slack-threads}"

curl -sS -w '\nHTTP %{http_code}\n' \
  -X POST "http://127.0.0.1:$port/v1/$endpoint" \
  -H "Authorization: Bearer $token" \
  -H "Content-Type: $content_type" \
  ${extra[@]+"${extra[@]}"} \
  --data-binary "@$file"
