# Deploy with Nix

Sunday Slate ships as a Nix flake with a runnable package, a NixOS service
module, and a public binary cache, so administrators on NixOS (and, via bare
installs, elsewhere) can deploy without building from source.

## Binary cache

First release your nix configuration to substitute from the Cachix cache:

```nix
nix.settings = {
  substituters = [
    "https://cache.nixos.org"
    "https://sunday-slate.cachix.org"
  ];
  trusted-public-keys = [
    "cache.nixos.org-1:6NCHdD59X431o0gWypbMrAURkbJ16ZPMQFGspcDShjY="
    "sunday-slate.cachix.org-1:<PASTE-THE-KEY-FROM-CACHIX>"
  ];
};
```

`cachix use sunday-slate` does the equivalent if the cachix CLI is around.
The key value comes from the cache's dashboard.

## NixOS service

Add the flake as an input and enable the service:

```nix
# flake.nix — the input needs no follow lines; leave it plain.
inputs.sunday-slate.url = "github:sunday-slate/sunday-slate";

# in a NixOS configuration module
{ config, pkgs, inputs, ... }:
{
  imports = [ inputs.sunday-slate.nixosModules.default ];

  services.sunday-slate = {
    enable = true;
    package = inputs.sunday-slate.packages.${pkgs.system}.sunday-slate; # cache-aligned build
    baseUrl = "https://slate.example.com";
    secrets.smtpPasswordFile = config.age.secrets.ss-smtp-pass.path;
    secrets.nflGitHubTokenFile = config.age.secrets.ss-gh-token.path;
  };
}
```

Without `package =`, the module defaults to building this repository's
packaging against the *consuming* nixpkgs — meaning a local workspace build
instead of a cached pull (the derivation CI built use a different nixpkgs
closure). Setting `package` as above is what buys the 30-second deployment.

</br>

<details>
<summary>Alternative: build against your own nixpkgs</summary>

Overriding the app flake's input instead of selecting its package is also
possible:

```nix
inputs.sunday-slate.inputs.nixpkgs.follows = "nixpkgs";
# and omit the package line — the module builds with your nixpkgs.
```

The cost stays small (a one-time workspace rebuild, then cached locally),
but it disables the substituter-hit-on-pull behavior for every future
build, so prefer the explicit `package` option in production configs.

</details>

The module is a single instance:

| Option                 | Notes                                                        |
| ---------------------- | ------------------------------------------------------------ |
| `workingDirectory`     | Service CWD; holds the generated `config.toml`. Default `/var/lib/sunday-slate`. |
| `storageDir`           | Databases + media; derived `${workingDirectory}/storage`.     |
| `databaseUrl`, `nflDatabaseUrl`, `mediaDir` | Derived off `storageDir`; each overridable.  |
| `bindAddr`             | `127.0.0.1:3000` by default; put a reverse proxy in front.    |
| `baseUrl`              | Public URL used in email links.                              |
| `mailFrom`, `season`, `nflSyncIntervalSecs` | Passed through to the app's config.      |
| `smtp`                 | `{ host, port (587), username }`; password is a secret only. |
| `secrets.*File`        | Paths to credential files (`smtpPasswordFile`, `nflGitHubTokenFile`, `tank01ApiKeyFile`). |
| `environmentVariables` | Escape hatch for env-only knobs (see below).                 |
| `openFirewall`         | Opens the port parsed from `bindAddr` when true.              |
| `package`              | Defaults to this repo's derivation as built against the consuming nixpkgs; set it to `sunday-slate.packages.${pkgs.system}.sunday-slate` for the cache-aligned build. |

### Secrets

Secrets are file paths, never literal values — raw values would land in the
world-readable nix store and systemd unit files. Sources can be sops-nix age
files, systemd credentials, or plain root-owned files; the module resolves
each into a `LoadCredential` entry, and a launcher exports it into the
governing environment variable:

- `secrets.smtpPasswordFile` → `SUNDAY_SLATE__SMTP__PASSWORD` (required when
  `smtp.host` is set)
- `secrets.nflGitHubTokenFile` → `SUNDAY_SLATE__NFL_GITHUB_TOKEN`
- `secrets.tank01ApiKeyFile` → `SUNDAY_SLATE__TANK01_API_KEY`

Migrations run automatically at startup for both databases; no migration
scripting is needed.

## Non-NixOS quick start

```console
$ nix run github:sunday-slate/sunday-slate
```

The binary reads `config.toml` from its working directory (optional) and
`SUNDAY_SLATE__*` environment overrides. Databases default to `./storage/`
under the current directory. Dev/test-only knobs are reachable as
environment variables — `SUNDAY_SLATE__LIVE_DEV_FEED`,
`SUNDAY_SLATE__LIVE_DEV_FEED_TICK_MS`, and `SUNDAY_SLATE_NOW` — and should
not be needed in production.

## Cache-hit contract

Binary-cache hits require bit-identical derivations; the package derivation
hashes in its whole toolchain closure, so a cached path is "the package at
this exact nixpkgs revision of this flake.lock".

- **Default (no input overrides):** flake resolution and CI share the same
  `flake.lock` commit, which CI rebuilds and pushes within a day of every
  nixpkgs move. Hits are the norm.
- **If the consumer sets** `inputs.sunday-slate.inputs.nixpkgs.follows =
  "nixpkgs"` — the flake above does exactly this — the package builds
  against the *consumer's* nixpkgs instead, which CI has not built. The
  official cache covers the toolchain paths, so the residual cost is one
  workspace-only rebuild (a few minutes), cached locally afterwards.
  Avoid the `follows` line if cached pulls beat local builds.
- Older pins self-heal: the first build fills the local store.

## Maintenance

- CI bumps `flake.lock` against `nixos-unstable` daily and only commits
  after the build and cache push are green, so main's lock never leads the
  cache.
- Caching the *vendored* cargo dependencies is driven by `Cargo.lock` +
  explicit hash pins in `nix/package.nix`. Only a dependency bump requires
  hash maintenance, and the failing CI makes it loud.
- The NixOS VM test (boots the service, checks HTTP + migrations +
  credential wiring) runs on every push to main in the linux leg, and as
  part of the daily bump build gate.
