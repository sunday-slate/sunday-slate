export DATABASE_FILE := "./storage/sunday-slate.db"
export DATABASE_URL := "sqlite://./storage/sunday-slate.db"

export NFL_DATABASE_FILE := "./storage/nfl-data.db"
export NFL_DATABASE_URL := "sqlite://./storage/nfl-data.db"

export SQLX_OFFLINE := "true"

# List recipes
default:
    @just --list

# Binds 0.0.0.0:3000 and prints the LAN URL so a phone on the local network can
# reach it; an optional PORT moves the port, e.g. `just dev 4000`.
#
# Run the dev server and CSS watcher (bacon rebuilds on Rust/template changes).
[group('dev')]
dev PORT=env("SUNDAY_SLATE_PORT", ""):
    @scripts/dev.sh {{PORT}}

# Run the server without bacon's TUI.
[group('dev')]
serve PORT=env("SUNDAY_SLATE_PORT", "3000"):
    @scripts/dev.sh {{PORT}} --no-bacon

# One-shot stylesheet build (for CI / one-offs; dev and release builds do this automatically)
[group('dev')]
css:
    tailwindcss -i crates/sunday-slate/styles/input.css -o crates/sunday-slate/assets/static/css/app.css --minify

# Re-download vendored frontend assets (htmx, Alpine, Geist, Phosphor, DaisyUI)
[group('dev')]
fetch-assets:
    ./scripts/fetch-assets.sh

# Run all tests
[group('check')]
test *name:
    cargo test --workspace {{name}}

# Check formatting and lints (CI-equivalent)
[group('check')]
lint:
    cargo fmt --all -- --check
    cargo clippy --workspace --all-targets -- -D warnings

# Auto-fix formatting and clippy suggestions
[group('check')]
fix:
    cargo fmt --all
    cargo clippy --workspace --all-targets --fix --allow-dirty --allow-staged

# Refresh the sqlx offline query cache (consolidated at the workspace-root .sqlx).
[group('db')]
sqlx-cache:
    cargo sqlx prepare --workspace


# run all migrations in both DBs. create the DBs if missing.
[group('db')]
db-migrate:
    touch $DATABASE_FILE
    cargo sqlx migrate run --source crates/sunday-slate/migrations
    touch $NFL_DATABASE_FILE
    DATABASE_URL="$NFL_DATABASE_URL" cargo sqlx migrate run --source crates/nfl-data/migrations

# Browse the app + nfl-data DBs in a Datasette web UI (read-only).
[group('db')]
db-browser:
    uvx datasette serve --crossdb $DATABASE_FILE $NFL_DATABASE_FILE

# Fetch the latest nflverse data into storage/nfl-data.db
[group('nfl')]
nfl-sync *ARGS:
    cargo run -p nfl-data --bin nfl-sync -- {{ARGS}}

# Update dependencies and run tests
[group('deps')]
update-deps: && test
    cargo update

# Show outdated dependencies
[group('deps')]
outdated:
    cargo outdated --workspace --root-deps-only

# Check licenses and advisories
[group('deps')]
deny:
    cargo deny check

# Find unused dependencies
[group('deps')]
machete:
    cargo machete
