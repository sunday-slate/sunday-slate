# NFL Data Sources

The primary data source for NFL stats is the excellent [nflverse](https://github.com/nflverse).
This is a freely available repo of historical NFL stats that gets updated regularly.

Optionally, you can also subscribe to the paid 
[Tank01 NFL Live Stats](https://rapidapi.com/tank01/api/tank01-nfl-live-in-game-real-time-statistics-nfl).
If you add a Tank01 API key to your environment, Sunday Slate will use it to update fantasy points while
the contest is running.

Nflverse does not publish stats until the conclusion of the game. Tank01 provides live stats. 
Not required for Sunday Slate, but useful on gameday. The app considers Nflverse the official stats however.

## Fresh Cache Setup

The nflverse provider stores its rebuildable, disposable source cache in
`storage/nflverse-cache.db`. Sunday Slate's managed players, teams, contests,
salaries, and identifier links remain in `storage/sunday-slate.db`. Original
FanDuel salary uploads are retained separately in the durable
`storage/fanduel-data.db` source archive. Back it up alongside
`storage/sunday-slate.db`; deleting or rebuilding the nflverse cache does not
change the FanDuel archive.

For Rust consumers, `nfl-data` retains canonical read operations and re-exports shared model types, live contracts, and time helpers from `nfl-model`. Provider synchronization operations and status types remain private to the crate; callers use its lifecycle methods and opaque `DataRevision` instead. Its distinct `NflDataConfig` takes default values from `NflverseDataConfig` so cache settings stay aligned.

From the workspace root, run:

```sh
mkdir -p storage
just db-migrate
just nfl-sync --earliest-season 2025
```

The new cache uses a single baseline migration. Do not point it at an existing
`nfl-data.db`, copy that database over the new path, or run the new migrations
against it. Leave the old database untouched and fetch a fresh cache instead.
Only the new provider cache may be removed and recreated; never delete the
application database to refresh NFL data. SQLx preparation and the database
browser now use `NFLVERSE_DATABASE_URL` and `storage/nflverse-cache.db`.

## Application Settings

These TOML keys replace the former `nfl_database_url`, `nfl_github_token`, and
`nfl_sync_interval_secs`. Legacy names have no aliases and are no longer used.

| TOML key | Environment variable | Default |
| --- | --- | --- |
| `nflverse_database_url` | `SUNDAY_SLATE__NFLVERSE_DATABASE_URL` | `sqlite://./storage/nflverse-cache.db` |
| `fanduel_database_url` | `SUNDAY_SLATE__FANDUEL_DATABASE_URL` | `sqlite://./storage/fanduel-data.db` |
| `nflverse_github_token` | `SUNDAY_SLATE__NFLVERSE_GITHUB_TOKEN` | Unset (public GitHub access) |
| `nflverse_sync_interval_secs` | `SUNDAY_SLATE__NFLVERSE_SYNC_INTERVAL_SECS` | `0` (manual only) |

A positive interval enables automatic refresh while the server runs. Sunday Slate maps this existing setting into `NflDataConfig::refresh_interval` and calls `start_background_tasks` at service startup and `stop_background_tasks` at shutdown. The first refresh occurs after one full interval, not at startup. Manual admin refreshes and scheduled refreshes share overlap prevention. Stopping the timer does not cancel an active refresh.

## NFL Data Administration

Global administrators can open NFL Data Admin from `/admin`, or directly at
`/nfl-data-admin`; the nflverse status and manual-refresh page is at
`/nfl-data-admin/nflverse`. The `nfl-data` admin router is not independently
authenticated. Hosts must mount it behind their session and global-admin
authorization middleware; Sunday Slate applies its existing login and admin
checks to the entire subtree. Access does not require a league or team.

`NflData::data_revision()` returns an opaque equality-comparable freshness hint
for invalidating derived identity caches. It is not a provider status API or a
monotonic version.

The nflverse database is a rebuildable cache and may be recreated from source.
The FanDuel database is durable source data and belongs in routine backups with
the Sunday Slate database. Keep all three stores separate; do not remove the
FanDuel archive when rebuilding the nflverse cache.

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
