# NFL Data Sources

The primary data source for NFL stats is the excellent [nflverse](https://github.com/nflverse).
This is a freely available repo of historical NFL stats that gets updated regularly.

Optionally, you can also subscribe to the paid
[Tank01 NFL Live Stats](https://rapidapi.com/tank01/api/tank01-nfl-live-in-game-real-time-statistics-nfl).
If you add a Tank01 API key to your environment, Sunday Slate will use it to update fantasy points while
the contest is running.

Nflverse does not publish stats until the conclusion of the game. Tank01 provides live stats.
Not required for Sunday Slate, but useful on gameday. The app considers Nflverse the official stats however.

## Four Separate Stores

Keep these stores separate:

| Store | Path by default | Role | Backup |
| --- | --- | --- | --- |
| Sunday Slate | `storage/sunday-slate.db` | Leagues, contests, frozen game membership, lineups, and legacy migration inputs | Durable; back up |
| NFL Data | `storage/nfl-data.db` | Current salaries, confirmed FanDuel mappings, import reviews, and migration completion | Durable; back up |
| nflverse | `storage/nflverse-cache.db` | Rebuildable schedules, players, rosters, and statistics | Disposable cache; no backup required |
| FanDuel Data | `storage/fanduel-data.db` | Original salary uploads, source metadata, and interpreted rows | Durable; back up |

Include all three durable databases in routine backups. Restore them together to
preserve the application's leagues and media associations, managed prices and
mappings, and source upload archive. The nflverse cache can be deleted and
recreated without changing any of that managed or archived data. Do not restore
a cache backup over a durable store.

## Initial Setup And Upgrade

From the workspace root, create and migrate the four database schemas:

```sh
mkdir -p storage
just db-migrate
just nfl-sync --earliest-season 2025
```

`just db-migrate` creates each default database if needed and applies only that
store's migrations. To use non-default locations, override the paired file and
URL variables with `just --set NAME VALUE`, for example:

```sh
just --set DATABASE_FILE /tmp/sunday-slate.db \
  --set DATABASE_URL sqlite:///tmp/sunday-slate.db \
  --set NFLVERSE_DATABASE_FILE /tmp/nflverse-cache.db \
  --set NFLVERSE_DATABASE_URL sqlite:///tmp/nflverse-cache.db \
  --set FANDUEL_DATABASE_FILE /tmp/fanduel-data.db \
  --set FANDUEL_DATABASE_URL sqlite:///tmp/fanduel-data.db \
  --set NFL_DATA_DATABASE_FILE /tmp/nfl-data.db \
  --set NFL_DATA_DATABASE_URL sqlite:///tmp/nfl-data.db db-migrate
```

The `just` variables default to `./storage`; keep each store in its own file.

The managed salary store goes through the same schema guard during `just
db-migrate` and application startup. An unrelated pre-existing managed database
is rejected without resetting its contents.

On application startup, Sunday Slate first migrates its own database, then NFL
Data opens and migrates its managed store, then copies legacy current salaries
and confirmed FanDuel mappings into NFL Data exactly once before serving
requests. The old salary/import tables and mapping column remain in the Sunday
Slate database as inert migration inputs; runtime salary reads and writes use
NFL Data. Unexpected pending legacy salary reviews stop startup without a
pending-review migration. Resolve or otherwise investigate that state before
retrying the upgrade. Legacy committed review history is not reconstructed in
the new administration UI. Existing media associations remain in the Sunday
Slate database. The old Sunday Slate setup-reminder banner, including its salary
and slate reminders, has been removed; salary administration is now available
through NFL Data Admin.

An unrelated or historical database at the new NFL Data managed path is not
reset or repurposed. Choose a new empty path for a first install, or restore the
correct NFL Data backup when recovering an installation. After cutover, the old
Sunday Slate snapshot is not a replacement for the NFL Data backup because
later salaries, mappings, reviews, and imports are owned by NFL Data.

The NFL Data migration is distinct from cache setup and does not require an
nflverse cache or an original CSV. Do not point the new managed path at an
existing unrelated `nfl-data.db`, copy an old database over the new path, or
run new migrations against that unrelated database. Only the nflverse cache may
be removed and rebuilt to refresh source data.

## Application Settings

The host setting is `nfl_data_database_url`, with environment override
`SUNDAY_SLATE__NFL_DATA_DATABASE_URL` and default
`sqlite://./storage/nfl-data.db`. Existing nflverse and FanDuel settings remain
separate:

| TOML key | Environment variable | Default |
| --- | --- | --- |
| `nflverse_database_url` | `SUNDAY_SLATE__NFLVERSE_DATABASE_URL` | `sqlite://./storage/nflverse-cache.db` |
| `fanduel_database_url` | `SUNDAY_SLATE__FANDUEL_DATABASE_URL` | `sqlite://./storage/fanduel-data.db` |
| `nfl_data_database_url` | `SUNDAY_SLATE__NFL_DATA_DATABASE_URL` | `sqlite://./storage/nfl-data.db` |
| `nflverse_github_token` | `SUNDAY_SLATE__NFLVERSE_GITHUB_TOKEN` | Unset (public GitHub access) |
| `nflverse_sync_interval_secs` | `SUNDAY_SLATE__NFLVERSE_SYNC_INTERVAL_SECS` | `0` (manual only) |

NFL Data also receives the application's configured `season` as the managed
salary season. Its separate provider `earliest_season` setting remains the
lower bound for cache fetches.

A positive refresh interval enables automatic nflverse refresh while the server
runs. Sunday Slate maps the setting into `NflDataConfig::refresh_interval` and
calls `start_background_tasks` at service startup and
`stop_background_tasks` at shutdown. The first refresh occurs after one full
interval, not at startup. Manual admin refreshes and scheduled refreshes share
overlap prevention. Stopping the timer does not cancel an active refresh.

## NFL Data Administration And API

Global administrators can open NFL Data Admin from `/admin`, or directly at
`/nfl-data-admin`; the nflverse status and manual-refresh page is at
`/nfl-data-admin/nflverse`. Salary administration is at
`/nfl-data-admin/salary-imports`. The `nfl-data` admin router is not
independently authenticated. Hosts must mount it behind their session and
global-admin authorization middleware; Sunday Slate applies its existing login
and admin checks to the entire subtree. Access does not require a league or
team. There is no uploader identity in the new salary workflow.

Sunday Slate consumes canonical current salary values from NFL Data using the
frozen game IDs for drafts and entries. Its application-facing Rust contract is
limited to canonical salary reads and the startup-only legacy migration; import
matching, review, and write operations remain internal to NFL Data.

`NflData::data_revision()` returns an opaque equality-comparable freshness hint
for invalidating derived identity caches. It is not a provider status API or a
monotonic version.

## SQLx Preparation

The checked-in `.sqlx` cache is workspace-wide. Prepare it against disposable
Sunday Slate and nflverse schemas, never a developer's existing application or
cache database. Create temporary databases, apply each crate's migrations, then
pass both URLs to the root workspace preparation recipe:

```sh
prep_dir=$(mktemp -d)
touch "$prep_dir/sunday-slate.db" "$prep_dir/nflverse-cache.db"
DATABASE_URL="sqlite://$prep_dir/sunday-slate.db" \
  cargo sqlx migrate run --source crates/sunday-slate/migrations
DATABASE_URL="sqlite://$prep_dir/nflverse-cache.db" \
  cargo sqlx migrate run --source crates/nflverse-data/migrations
just --set DATABASE_URL "sqlite://$prep_dir/sunday-slate.db" \
  --set NFLVERSE_DATABASE_URL "sqlite://$prep_dir/nflverse-cache.db" sqlx-cache
```

`sqlx-cache` runs `cargo sqlx prepare --workspace -- --all-targets` with
`SQLX_OFFLINE=false`, so query macros validate against the two respective
crate-configured schema URLs and test-only queries are included. The managed
NFL Data schema is migrated by `just db-migrate`; its storage uses runtime
queries rather than SQLx query macros. Never prepare against a user's existing
storage or treat schema preparation as a substitute for startup migration and
the salary handoff.

## Command-Line Refresh

`just nfl-sync` runs the provider-owned `nfl-sync` binary. It performs one awaited
sync and exits, without starting a timer. Existing flags are unchanged:

| Flag | Environment variable |
| --- | --- |
| `--database-url` | `NFLVERSE_DATABASE_URL` |
| `--earliest-season` | `NFLVERSE_EARLIEST_SEASON` |
| `--github-token` | `GITHUB_TOKEN` |

The default earliest season is the current UTC year minus two. Prefer storing
credentials in the environment rather than passing them on the command line.
The application token setting and CLI `GITHUB_TOKEN` are separate. Old
`NFL_DATABASE_URL` and `NFL_EARLIEST_SEASON` variables are not used.

The command prints a dataset report and returns success only when every dataset
succeeds. A failed dataset does not prevent the others from updating, but causes
a nonzero exit code. Database/setup errors also return a nonzero exit code.
