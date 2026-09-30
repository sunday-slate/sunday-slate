# Deploy with Nix

Sunday Slate ships as a Nix flake with a runnable package, a NixOS service
module, and a public binary cache, so administrators on NixOS (and, via bare
installs, elsewhere) can deploy without building from source.

## NixOS service

### Enable Cachix Package cache

`cachix use sunday-slate` 

### Flake example

```nix
inputs.sunday-slate.url = "github:sunday-slate/sunday-slate";

# in a NixOS configuration module
{ config, inputs, ... }:
{
  imports = [ inputs.sunday-slate.nixosModules.default ];

  services.sunday-slate = {
    enable = true;
  };
}
```

The module is a single instance:

| Option                  | Default |
| ----------------------- | ------- |
| `workingDirectory`      | `/var/lib/sunday-slate` |
| `storageDir`            | `${workingDirectory}/storage` |
| `databaseUrl`           | `sqlite://<storageDir>/sunday-slate.db` |
| `nflDatabaseUrl`        | `sqlite://<storageDir>/nfl-data.db` |
| `mediaDir`              | `<storageDir>/media` |
| `bindAddr`              | `127.0.0.1:3000` |
| `baseUrl`               | `http://localhost:3000` |
| `mailFrom`              | `Sunday Slate <no-reply@example.com>` |
| `season`                | `2026` |
| `nflSyncIntervalSecs`   | `0` (disabled) |
| `smtp`                  | `null` |
| `smtp.host`             | required if smtp set |
| `smtp.port`             | `587` |
| `smtp.username`         | required if smtp set |
| `openFirewall`          | `false` |
| `package`               | `sunday-slate` |

### Secrets

Secrets are file paths, never literal values — raw values would land in the
world-readable nix store and systemd unit files. Sources can be sops-nix age
files, systemd credentials, or plain root-owned files.

- `secrets.smtpPasswordFile` → `SUNDAY_SLATE__SMTP__PASSWORD`
- `secrets.nflGitHubTokenFile` → `SUNDAY_SLATE__NFL_GITHUB_TOKEN`
- `secrets.tank01ApiKeyFile` → `SUNDAY_SLATE__TANK01_API_KEY`

### Migrations

Migrations run automatically at startup for both databases; no migration
scripting is needed.
