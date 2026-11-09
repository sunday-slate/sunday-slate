# Deploy with Nix

The flake provides a Sunday Slate package and NixOS service module for
**x86-64 Linux only**. There is no project binary cache; expect an initial
build from source.

## NixOS service

Add the flake input:

```nix
inputs.sunday-slate.url = "github:sunday-slate/sunday-slate";
```

Then import and enable the module:

```nix
{ inputs, ... }:
{
  imports = [ inputs.sunday-slate.nixosModules.default ];

  services.sunday-slate = {
    enable = true;
    settings = {
      base_url = "https://slate.example.com";
      # bind_addr = "0.0.0.0:3000"; # if serving without a local reverse proxy
    };
    # environmentFile = "/run/secrets/sunday-slate";
  };
}
```

The service runs as `sunday-slate` in `/var/lib/sunday-slate`. Databases and
media use the application's default `./storage` paths beneath that directory.
Migrations run at startup. The Sunday Slate database and FanDuel source archive
are durable data and should both be included in backups. The nflverse database
is a rebuildable cache; it can be recreated without affecting either durable
store. The default bind address is `127.0.0.1:3000`;
configure a reverse proxy or host firewall separately if exposing the service.

The module has four options: `enable`, `package`, `settings`, and
`environmentFile`. `settings` uses the same snake_case names and structure as
[config.toml.example](https://github.com/sunday-slate/sunday-slate/blob/main/config.toml.example).
Omitted settings use application defaults.

## Secrets

Do not put passwords or tokens in `settings`: generated configuration enters
the world-readable Nix store. Instead, provision a root-owned, mode `0600`
systemd environment file outside the store, using a secret manager or manual
file creation. Set `environmentFile` to its absolute path as a **quoted
string**, not a Nix path literal.

Example contents (use systemd environment-file quoting rules):

```sh
SUNDAY_SLATE__SMTP__PASSWORD="replace-with-password"
SUNDAY_SLATE__NFLVERSE_GITHUB_TOKEN="replace-with-token"
SUNDAY_SLATE__TANK01_API_KEY="replace-with-key"
```

Include only variables needed for the deployment. For SMTP, also provide
`smtp.host`, `smtp.port`, and `smtp.username` in `settings`.

## Build and verify

```sh
nix build github:sunday-slate/sunday-slate#sunday-slate
```

CI builds the package and checks that the service module generates a systemd
unit. It does not boot or verify a deployment. After enabling the service and
running `nixos-rebuild switch`, check it on the target host:

```sh
systemctl status sunday-slate
journalctl -u sunday-slate -n 50
curl -fsSL http://127.0.0.1:3000/
```

Adjust the URL if a different bind address is configured. Update the flake
lock manually when needed; there is no scheduled update job.

## Logging

The service logs structured text to stdout, which systemd captures in the
journal. ANSI escapes are disabled automatically when stdout is not a
terminal, so `journalctl` output is plain text.

`RUST_LOG` selects events. When it is unset or empty, the service uses:

```text
warn,sunday_slate=info,nfl_data=info,nflverse_data=info,tank01_data=info,fanduel_data=info,utils=info
```

A set `RUST_LOG` **replaces** that default; include the crate directives you
still want. An invalid value stops startup with an error naming the filter,
rather than silently dropping every event. `RUST_LOG` is read once at startup:
change it in the systemd environment file and restart
(`systemctl restart sunday-slate`). There is no runtime reload.

### Request correlation

Every HTTP request gets a generated UUID, an `http_request` span, and an
`x-request-id` response header. The `request completed` summary is emitted as
soon as response headers are ready — before the body streams — so
`duration_ms`, `status`, `method`, and `path` describe time to headers, not
time to the last body byte. Streaming (SSE) responses are summarized when
they open. Body-stream failures are logged separately once they occur.

Correlate a response with its server-side log:

```sh
journalctl -u sunday-slate -o cat --grep='<request UUID>'
```

Server errors return a generic `Internal server error` body; the cause stays
in the correlated log event.

### Input diagnostics (opt-in)

Debugging a malformed submission can enable bounded input capture by appending
to `RUST_LOG`:

```text
...,sunday_slate::http_input=debug
```

This logs the request query string and, for `application/json`,
`application/x-www-form-urlencoded`, and `application/*+json` bodies, the first
8192 raw bytes (lossily decoded and escaped, with `truncated`/`complete`
flags). Bytes reach handlers unchanged; capture is passive and never drains or
parses the body. Multipart uploads log only filename, content type, and size
from the upload handlers.

Never captured: headers, cookies, authorization values, response bodies, and
the query/body of credential-bearing routes (`/login`, `/logout`,
`/forgot-password`, `/reset-password`, `/setup`, `/invite`, `/invite/*`,
`/__test/login/*`). The `/invites` commissioner view is not a credential route
and remains eligible. Leave this target off in normal operation: form and JSON
bodies can contain personal data.

### Background jobs

Detached jobs (`background_job` spans for avatar fetches and nflverse syncs)
log start, outcome, and duration. Snapshot updates log `snapshot published`
only after the new snapshot is atomically visible, and `snapshot update failed`
with the prior snapshot retained on failure.

Production verification requires host access: check the effective
`RUST_LOG`, restart only as authorized for the deployment, request a page, and
confirm the journal event carries the same UUID as the response header.

