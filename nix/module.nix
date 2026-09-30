# NixOS module for the Sunday Slate server.
#
# Every option maps 1:1 to a field in crates/sunday-slate/src/config.rs.
# Config reaches the app as a generated config.toml installed into the
# service's working directory (the app reads config.toml from CWD);
# secrets never enter the store or config.toml — they flow from
# systemd credentials into SUNDAY_SLATE__* variables by the launcher.
{
  lib,
  pkgs,
  config,
  options,
  ...
}:

let
  inherit (lib)
    mkEnableOption
    mkOption

    mkIf
    mkDefault
    types
    literalExpression
    ;

  cfg = config.services.sunday-slate;

  inherit (import ./settings.nix) mkSettings;

  format = pkgs.formats.toml { };

  # camelCase option record → mkSettings (snake_case config.toml keys).
  settingsRecord = {
    bindAddr = cfg.bindAddr;
    baseUrl = cfg.baseUrl;
    mailFrom = cfg.mailFrom;
    season = cfg.season;
    databaseUrl = cfg.databaseUrl;
    nflDatabaseUrl = cfg.nflDatabaseUrl;
    mediaDir = cfg.mediaDir;
    nflSyncIntervalSecs = cfg.nflSyncIntervalSecs;
    smtp =
      if cfg.smtp == null then null
      else {
        host = cfg.smtp.host;
        port = cfg.smtp.port;
        username = cfg.smtp.username;
      };
  };

  configFile = format.generate "sunday-slate-config.toml" (mkSettings settingsRecord);

  bindPort = lib.toInt (lib.last (lib.splitString ":" cfg.bindAddr));


  secretWires = [
    {
      var = "SUNDAY_SLATE__SMTP__PASSWORD";
      file = cfg.secrets.smtpPasswordFile;
      name = "smtp-password";
    }
    {
      var = "SUNDAY_SLATE__NFL_GITHUB_TOKEN";
      file = cfg.secrets.nflGitHubTokenFile;
      name = "nfl-github-token";
    }
    {
      var = "SUNDAY_SLATE__TANK01_API_KEY";
      file = cfg.secrets.tank01ApiKeyFile;
      name = "tank01-api-key";
    }
  ];
  activeSecrets = lib.filter (s: s.file != null) secretWires;

  launcher = pkgs.writeShellScript "sunday-slate-launcher" ''
    set -euo pipefail
    ${
      lib.concatMapStringsSep "\n" (s:
        ''export ${s.var}="$(<"$CREDENTIALS_DIRECTORY/${s.name}")"'')
        activeSecrets
    }
    exec ${cfg.package}/bin/sunday-slate
  '';

  # systemd StateDirectory covers exactly these two by default; anything
  # else the admin selects gets created (and chowned) in preStart instead.
  workingDirCovered = cfg.workingDirectory == "/var/lib/sunday-slate";
  storageDirCovered = cfg.storageDir == "/var/lib/sunday-slate/storage";

in
{
  options.services.sunday-slate = {
    enable = mkEnableOption "Sunday Slate server";

    package = mkOption {
      type = types.package;
      defaultText = literalExpression ''
        pkgs.callPackage ./package.nix { src = ../.; tailwindcss = pkgs.tailwindcss_4; }
      '';
      description = ''
        The Sunday Slate package. Defaults to this repository's own
        derivation as of the consuming nixpkgs; pass the flake's
        packages.''${system}.sunday-slate for the lock-aligned cached build.
      '';
    };

    workingDirectory = mkOption {
      type = types.path;
      default = "/var/lib/sunday-slate";
      description = ''
        Service CWD. Holds the generated config.toml; mirrored by the repo's
        own dev layout (config.toml beside storage/).
      '';
    };

    storageDir = mkOption {
      type = types.path;
      defaultText = literalExpression ''"\''${workingDirectory}/storage"'';
      description = "Where the SQLite databases and media live.";
    };

    openFirewall = mkOption {
      type = types.bool;
      default = false;
      description = "Open the port computed from bindAddr in the firewall.";
    };

    databaseUrl = mkOption {
      type = types.str;
      defaultText = "sqlite://<storageDir>/sunday-slate.db";
      description = "SQLite URL for the Sunday Slate database.";
    };

    nflDatabaseUrl = mkOption {
      type = types.str;
      defaultText = "sqlite://<storageDir>/nfl-data.db";
      description = "SQLite URL for the nflverse cache database.";
    };

    mediaDir = mkOption {
      type = types.str;
      defaultText = "<storageDir>/media";
      description = "Directory for uploaded media.";
    };

    bindAddr = mkOption {
      type = types.str;
      default = "127.0.0.1:3000";
      description = "host:port the HTTP server binds to.";
    };

    baseUrl = mkOption {
      type = types.str;
      default = "http://localhost:3000";
      description = "Public URL, used for links in emails.";
    };

    mailFrom = mkOption {
      type = types.str;
      default = "Sunday Slate <no-reply@example.com>";
      description = "Email From address used by system messages.";
    };

    season = mkOption {
      type = types.nullOr types.ints.u16;
      default = null;
      defaultText = "null (app default)";
      description = "NFL season the app serves; null leaves the app default.";
    };

    nflSyncIntervalSecs = mkOption {
      # ints.u64 does not exist in nixpkgs; unsigned == >=0, which is the
      # domain the app docs define for sync intervals (seconds).
      type = types.ints.unsigned;
      default = 0;
      description = ''
        Automatic nflverse cache refresh in seconds; 0 = manual only (
        admin button).
      '';
    };

    smtp = mkOption {
      type = types.nullOr (types.submodule
        ({ ... }: {
          options = {
            host = mkOption {
              type = types.str;
              description = "SMTP host.";
            };
            port = mkOption {
              type = types.port;
              default = 587;
              description = "SMTP port.";
            };
            username = mkOption {
              type = types.str;
              description = "SMTP username.";
            };
          };
        }));
      default = null;
      description = ''
        SMTP configuration; null omits the entire [smtp] section (dev mode).
        The password is always a secret, never a plain option.
      '';
    };

    secrets = {
      smtpPasswordFile = mkOption {
        type = types.nullOr types.path;
        default = null;
        description = "Path to the SMTP password (required with smtp).";
      };
      nflGitHubTokenFile = mkOption {
        type = types.nullOr types.path;
        default = null;
        description = "Path to a GitHub token for server-started nflverse syncs.";
      };
      tank01ApiKeyFile = mkOption {
        type = types.nullOr types.path;
        default = null;
        description = "Path to the Tank01 live-score provider key.";
      };
    };

    environmentVariables = mkOption {
      type = types.attrsOf types.str;
      default = { };
      description = ''
        Extra environment variables; the escape hatch for env-only knobs (
        live_dev_feed, live_dev_feed_tick_ms, SUNDAY_SLATE_NOW …).
      '';
    };
  };

  config = mkIf cfg.enable {
    services.sunday-slate.package = mkDefault (
      pkgs.callPackage ../nix/package.nix {
        src = ../.;
        tailwindcss = pkgs.tailwindcss_4;
      });

    # Derivation cascade: each hop has mkDefault (low) priority so explicit
    # user settings win outright. (mkDerivedConfig would collide with a
    # same-priority explicit value set onto the leaf.)
    services.sunday-slate.storageDir = mkDefault "${cfg.workingDirectory}/storage";
    services.sunday-slate.databaseUrl = mkDefault "sqlite://${cfg.storageDir}/sunday-slate.db";
    services.sunday-slate.nflDatabaseUrl = mkDefault "sqlite://${cfg.storageDir}/nfl-data.db";
    services.sunday-slate.mediaDir = mkDefault "${cfg.storageDir}/media";

    assertions = [
      {
        assertion = cfg.smtp == null || cfg.secrets.smtpPasswordFile != null;
        message = ''
          services.sunday-slate.secrets.smtpPasswordFile must be set when
          services.sunday-slate.smtp.host is configured: the SMTP transport
          reads the password from SUNDAY_SLATE__SMTP__PASSWORD.
        '';
      }
    ];

    networking.firewall.allowedTCPPorts = mkIf cfg.openFirewall [ bindPort ];

    systemd.services.sunday-slate = {
      description = "Sunday Slate fantasy football server";
      wantedBy = [ "multi-user.target" ];
      after = [ "network-online.target" ];
      wants = [ "network-online.target" ];

      environment = cfg.environmentVariables;

      preStart =
        let
          customDirs =
            lib.optionals (!workingDirCovered) [ cfg.workingDirectory ]
            ++ lib.optionals (!storageDirCovered) [ cfg.storageDir ];
        in
        ''
          ${lib.concatMapStringsSep "\n" (d: "install -d -m 700 ${d}") customDirs}
          install -m 600 ${configFile} ${cfg.workingDirectory}/config.toml
        '';

      serviceConfig = {
        User = "sunday-slate";
        Group = "sunday-slate";

        ExecStart = "${launcher}";

        StateDirectory = mkIf workingDirCovered "sunday-slate";
        StateDirectoryMode = "0700";

        LoadCredential =
          mkIf (activeSecrets != [ ])
            (map (s: "${s.name}:${s.file}") activeSecrets);

        Restart = "on-failure";
        UMask = "0077";

        # Hardening (nixpkgs norms for network-facing services).
        AmbientCapabilities = "";
        CapabilityBoundingSet = "";
        DeviceAllow = "";
        LockPersonality = true;
        MemoryDenyWriteExecute = true;
        NoNewPrivileges = true;
        PrivateTmp = true;
        PrivateDevices = true;
        PrivateUsers = true;
        ProtectClock = true;
        ProtectControlGroups = true;
        ProtectHome = "read-only";
        ProtectHostname = true;
        ProtectKernelLogs = true;
        ProtectKernelModules = true;
        ProtectKernelTunables = true;
        ProtectProc = "noaccess";
        ProtectSystem = "strict";
        RestrictAddressFamilies = [ "AF_INET" "AF_INET6" "AF_UNIX" ];
        RestrictNamespaces = true;
        RestrictRealtime = true;
        RestrictSUIDSGID = true;
        SystemCallArchitectures = "native";
        SystemCallErrorNumber = "EPERM";
        SystemCallFilter = [
          "@system-service"
          "~@cpu-emulation"
          "~@debug"
          "~@keyring"
          "~@memlock"
          "~@obsolete"
          "~@privileged"
          "~@setuid"
        ];
      };
    };

    users.users.sunday-slate = {
      isSystemUser = true;
      group = "sunday-slate";
    };
    users.groups.sunday-slate = mkDefault { };
  };
}
