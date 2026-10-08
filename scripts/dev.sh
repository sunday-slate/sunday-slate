#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."

# Bind to all interfaces so a phone on the local network can pull up the app.
# An optional port override (`just dev 4000`) moves the port
PORT="${1:-3000}"
if [[ ! "$PORT" =~ ^[0-9]+$ ]]; then
    echo "dev: port '$PORT' is not a number" >&2
    exit 2
fi

if [[ -n "${SUNDAY_SLATE__BASE_URL:-}" ]]; then
    BASE_URL="$SUNDAY_SLATE__BASE_URL"
    LAN_IP=""
elif [[ "${SUNDAY_SLATE_CONTAINER:-}" == "1" ]]; then
    BASE_URL="http://localhost:$PORT"
    LAN_IP=""
else
    # The `hostname -I` last resort can list IPv6 first; only an IPv4 address
    # makes a usable unbracketed URL, so take the first IPv4-shaped token.
    LAN_IP="$(
        ipconfig getifaddr en0 2>/dev/null ||
            ipconfig getifaddr en1 2>/dev/null ||
            ip -4 route get 1.1.1.1 2>/dev/null |
                awk '{for (i = 1; i <= NF; i++) if ($i == "src") { print $(i + 1); exit }}' ||
            hostname -I 2>/dev/null |
                awk '{for (i = 1; i <= NF; i++) if ($i ~ /^([0-9]+\.){3}[0-9]+$/) { print $i; exit }}' ||
            true
    )"
    BASE_URL="http://${LAN_IP:-localhost}:$PORT"
fi
export SUNDAY_SLATE__BIND_ADDR="0.0.0.0:$PORT"
export SUNDAY_SLATE__BASE_URL="$BASE_URL"
# To the real terminal: bacon's TUI uses the alternate screen, so these lines
# are what the terminal shows once bacon exits (and briefly before it starts).
{
    echo "local: http://localhost:$PORT"
    [[ -n "$LAN_IP" ]] && echo "lan:   http://$LAN_IP:$PORT"
} >/dev/tty 2>/dev/null || true

# Independently owned stylesheets each have a watcher.
tailwindcss \
    -i crates/sunday-slate/styles/input.css \
    -o crates/sunday-slate/assets/static/css/app.css \
    --watch=always </dev/null >/dev/null 2>&1 &
host_css_pid=$!
tailwindcss \
    -i crates/nfl-data/styles/input.css \
    -o crates/nfl-data/assets/static/css/admin.css \
    --watch=always </dev/null >/dev/null 2>&1 &
nfl_css_pid=$!
trap 'kill "$host_css_pid" "$nfl_css_pid" 2>/dev/null || true' EXIT

# SQLX_OFFLINE is set here because bacon has no per-job `env` key
if [[ "${2:-}" == "--no-bacon" ]]; then
    SQLX_OFFLINE=true \
    RUST_LOG="${RUST_LOG:-sunday_slate=debug,axum_login=debug,tower_sessions=debug,sqlx=warn,tower_http=debug}" \
        cargo run --bin sunday-slate
else
    SQLX_OFFLINE=true \
    RUST_LOG="${RUST_LOG:-sunday_slate=debug,axum_login=debug,tower_sessions=debug,sqlx=warn,tower_http=debug}" \
        bacon serve >/dev/tty 2>&1
fi
