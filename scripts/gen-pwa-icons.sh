#!/usr/bin/env bash
# Regenerate the Remote controller PWA icons (public/pwa-*.png,
# public/apple-touch-icon.png) from public/logo.png. macOS only (uses sips).
#
# logo.png is a rounded square on a transparent margin. "any" icons keep it as
# is; the full-bleed ones (apple-touch, maskable) crop to the square's body so
# no transparent corner shows up black, and the maskable one is inset into the
# 80% safe zone, its edges stretched outward so the background has no seam.
set -euo pipefail
cd "$(dirname "$0")/../public"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

sips -z 192 192 logo.png --out pwa-icon-192.png >/dev/null
sips -z 512 512 logo.png --out pwa-icon-512.png >/dev/null

# The square's opaque body: 880px centred on the artwork.
sips -c 880 880 --cropOffset 185 187 logo.png --out "$tmp/body.png" >/dev/null
sips -z 180 180 -s format bmp "$tmp/body.png" --out "$tmp/apple.bmp" >/dev/null
node ../scripts/pwa-icon-extend.mjs "$tmp/apple.bmp" "$tmp/apple-clean.bmp" 180
sips -s format png "$tmp/apple-clean.bmp" --out apple-touch-icon.png >/dev/null
sips -z 400 400 -s format bmp "$tmp/body.png" --out "$tmp/inset.bmp" >/dev/null
node ../scripts/pwa-icon-extend.mjs "$tmp/inset.bmp" "$tmp/maskable.bmp" 512
sips -s format png "$tmp/maskable.bmp" --out pwa-icon-maskable-512.png >/dev/null
echo "PWA icons written to public/"
