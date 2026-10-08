#!/usr/bin/env bash
# Refresh the small asset set used by the independently-owned NFL admin shell.
set -euo pipefail
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
crate="$root/crates/nfl-data"
mkdir -p "$crate/assets/static/vendor/js" "$crate/assets/static/vendor/fonts" "$crate/styles"
get() { printf '  -> %s\n' "$2"; curl -fsSL -o "$2" "$1"; }
get "https://unpkg.com/htmx.org@4.0.0/dist/htmx.min.js" "$crate/assets/static/vendor/js/htmx.min.js"
get "https://cdn.jsdelivr.net/fontsource/fonts/geist:vf@latest/latin-wght-normal.woff2" "$crate/assets/static/vendor/fonts/geist.woff2"
get "https://cdn.jsdelivr.net/fontsource/fonts/geist-mono:vf@latest/latin-wght-normal.woff2" "$crate/assets/static/vendor/fonts/geist-mono.woff2"
get "https://github.com/saadeghi/daisyui/releases/latest/download/daisyui.mjs" "$crate/styles/daisyui.mjs"
get "https://github.com/saadeghi/daisyui/releases/latest/download/daisyui-theme.mjs" "$crate/styles/daisyui-theme.mjs"
printf '%s\n' 'Preserved THIRD_PARTY_NOTICES.md; review notices if upstream licensing changes.'
