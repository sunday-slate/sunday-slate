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
Migrations run at startup. The default bind address is `127.0.0.1:3000`;
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
