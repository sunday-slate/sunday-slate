#!/usr/bin/env bash
# Vendor all frontend assets locally so the app has zero third-party runtime
# requests (privacy-first, offline-friendly). Re-run to refresh pinned versions.
#
# Downloads into crates/sunday-slate/assets/static/vendor.
# The DaisyUI Tailwind plugins are vendored into crates/sunday-slate/styles.
set -euo pipefail

# Pinned versions
HTMX_VER="4.0.0" # https://github.com/bigskysoftware/htmx/releases
ALPINE_VER="3.x.x" # latest v3 - https://alpinejs.dev/essentials/installation
PHOSPHOR_VER="2.1.2" # https://github.com/phosphor-icons/web
DAISYUI_VER="latest" # latest - https://github.com/saadeghi/daisyui

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
crate="$root/crates/sunday-slate"
vendor="$crate/assets/static/vendor"
styles="$crate/styles"
img="$crate/assets/static/img"

mkdir -p "$vendor/js" "$vendor/fonts" "$vendor/phosphor" "$styles" "$img"

get() { echo "  → $2"; curl -fsSL -o "$2" "$1"; }

echo "JS libraries"
get "https://unpkg.com/htmx.org@${HTMX_VER}/dist/htmx.min.js" "$vendor/js/htmx.min.js"
get "https://unpkg.com/htmx.org@${HTMX_VER}/dist/ext/hx-sse.min.js" "$vendor/js/hx-sse.min.js"
get "https://unpkg.com/alpinejs@${ALPINE_VER}/dist/cdn.min.js" "$vendor/js/alpine.min.js"

echo "Geist fonts (variable woff2 from Fontsource)"
get "https://cdn.jsdelivr.net/fontsource/fonts/geist:vf@latest/latin-wght-normal.woff2" "$vendor/fonts/geist.woff2"
get "https://cdn.jsdelivr.net/fontsource/fonts/geist-mono:vf@latest/latin-wght-normal.woff2" "$vendor/fonts/geist-mono.woff2"

echo "Phosphor icons (regular + fill)"
get "https://cdn.jsdelivr.net/npm/@phosphor-icons/web@${PHOSPHOR_VER}/src/regular/style.css" "$vendor/phosphor/regular.css"
get "https://cdn.jsdelivr.net/npm/@phosphor-icons/web@${PHOSPHOR_VER}/src/regular/Phosphor.woff2" "$vendor/phosphor/Phosphor.woff2"
get "https://cdn.jsdelivr.net/npm/@phosphor-icons/web@${PHOSPHOR_VER}/src/fill/style.css" "$vendor/phosphor/fill.css"
get "https://cdn.jsdelivr.net/npm/@phosphor-icons/web@${PHOSPHOR_VER}/src/fill/Phosphor-Fill.woff2" "$vendor/phosphor/Phosphor-Fill.woff2"


# Trim @font-face to the woff2 we vendored: drop the woff/ttf/svg sources
for f in regular fill; do
  css="$vendor/phosphor/$f.css"
  sed -E \
    -e '/url\([^)]*\) format\("(woff|truetype|svg)"\),?/d' \
    -e 's/(format\("woff2"\)),/\1;/' \
    "$css" > "$css.tmp"
  mv "$css.tmp" "$css"
done

echo "DaisyUI Tailwind plugins"
get "https://github.com/saadeghi/daisyui/releases/${DAISYUI_VER}/download/daisyui.mjs" "$styles/daisyui.mjs"
get "https://github.com/saadeghi/daisyui/releases/${DAISYUI_VER}/download/daisyui-theme.mjs" "$styles/daisyui-theme.mjs"

echo "Done. Vendored assets are in $vendor and $styles."
