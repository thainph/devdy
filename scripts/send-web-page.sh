#!/usr/bin/env bash
# Push an exported web page (.md or .zip with images) to Devdy's local Inbox API.
#
# Usage: ./scripts/send-web-page.sh <file.md|file.zip> [token] [port]
#
# Same args/env as send-slack-thread.sh; DEVDY_INBOX_MODE=new forces a new capture.
set -euo pipefail
DEVDY_INBOX_ENDPOINT=web-pages exec "$(dirname "$0")/send-slack-thread.sh" "$@"
