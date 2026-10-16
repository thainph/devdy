#!/usr/bin/env bash
# Send a Slack thread link to Devdy's local Inbox API; Devdy fetches the thread
# from Slack and translates it in the background (202 Accepted).
#
# Usage: ./scripts/send-slack-thread.sh <slack-link> [token] [port]
#
# slack-link: https://<workspace>.slack.com/archives/<channel>/p<16 digits>[?thread_ts=…&cid=…]
# token: arg 2, else $DEVDY_INBOX_TOKEN (copy it from Devdy → Settings → Inbox API)
# port:  arg 3, else $DEVDY_INBOX_PORT, else auto-detect 47821..47830 via /health
# Optional env: DEVDY_INBOX_PROJECT_ID (body "projectId"), DEVDY_INBOX_TITLE (body "title", any UTF-8)
set -euo pipefail

if [[ $# -lt 1 || "$1" == "-h" || "$1" == "--help" ]]; then
  sed -n '2,10p' "$0" | sed 's/^# \{0,1\}//'
  exit 1
fi

url="$1"
token="${2:-${DEVDY_INBOX_TOKEN:-}}"
port="${3:-${DEVDY_INBOX_PORT:-}}"

if [[ -f "$url" ]]; then
  echo "error: expected a Slack thread link, not a file (zip/markdown uploads are no longer supported)" >&2
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

# JSON string literal: escape backslash, quote and control characters.
json_str() {
  local s="$1"
  s="${s//\\/\\\\}"
  s="${s//\"/\\\"}"
  s="${s//$'\n'/\\n}"
  s="${s//$'\r'/\\r}"
  s="${s//$'\t'/\\t}"
  printf '"%s"' "$s"
}

body="{\"url\":$(json_str "$url")"
[[ -n "${DEVDY_INBOX_PROJECT_ID:-}" ]] && body+=",\"projectId\":$(json_str "$DEVDY_INBOX_PROJECT_ID")"
[[ -n "${DEVDY_INBOX_TITLE:-}" ]] && body+=",\"title\":$(json_str "$DEVDY_INBOX_TITLE")"
body+="}"

curl -sS -w '\nHTTP %{http_code}\n' \
  -X POST "http://127.0.0.1:$port/v1/slack-threads" \
  -H "Authorization: Bearer $token" \
  -H "Content-Type: application/json; charset=utf-8" \
  --data-binary "$body"
