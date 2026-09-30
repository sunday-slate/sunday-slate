# Sunday Slate

Season-long DFS-style fantasy football for friends and family. Community-owned, 
privacy-first alternative to the large sportsbook-owned model.

Self-hostable and fully open source via AGPL-3.0. No tricks. No "pro" version. 

Built with Rust (axum), HTMX, Alpine.js and backed by SQLite.

## Status

This is very much in active development. The initial commit was a spike to get
the app in hands of friends and family at the start of the NFL season. It's
largely feature complete but certainly not bug free.

While everything here was steered by me, large chunks of implementation were done
via agents. The goal over the next few weeks is to audit and refactor the code
in to something to build from moving forward. 

Once the expunging of any slop is complete, I'll release a stable version. Until
then, it's low risk to use but it's entirely *your* risk if you do so.

## NFL Data Sources

The primary data source for NFL stats is the excellent [nflverse](https://github.com/nflverse).
This is a freely available repo of historical NFL stats that gets updated regularly.

Optionally, you can also subscribe to the paid 
[Tank01 NFL Live Stats](https://rapidapi.com/tank01/api/tank01-nfl-live-in-game-real-time-statistics-nfl).
If you add a Tank01 API key to your environment, Sunday Slate will use it to update fantasy points while
the contest is running.

Nflverse does not publish stats until the conclusion of the game. Tank01 provides live stats. 
Not required for Sunday Slate, but useful on gameday. The app considers Nflverse the official stats however.

## Development

I set up my dev environment via a [nix flake](flake.nix). YMMV.

```sh
direnv allow        # enters the flake devshell
just db-migrate
just dev            # or `just serve 3000` for no bacon TUI
just docs           # serve the docs site at localhost:8000
```

`just --list` shows the useful commands.

## License

### Code
All source code in this repository is licensed under the
**GNU Affero General Public License v3.0 or later**
([AGPL-3.0-or-later](https://spdx.org/licenses/AGPL-3.0-or-later.html)).

See [LICENSE](./LICENSE) for the full text.

SPDX-License-Identifier: AGPL-3.0-or-later

### Documentation
All files under [`docs/`](./docs/) are licensed under the
**Creative Commons Attribution-ShareAlike 4.0 International License**
([CC BY-SA 4.0](https://creativecommons.org/licenses/by-sa/4.0/)).

See [docs/LICENSE](./docs/LICENSE) for the full text.