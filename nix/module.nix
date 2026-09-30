{ lib, pkgs, config, ... }:

let
  cfg = config.services.sunday-slate;
  format = pkgs.formats.toml { };
  configFile = format.generate "sunday-slate-config.toml" cfg.settings;
in {
  options.services.sunday-slate = {
    enable = lib.mkEnableOption "Sunday Slate server";

    package = lib.mkOption {
      type = lib.types.package;
      default = pkgs.callPackage ./package.nix {
        src = ../.;
        tailwindcss = pkgs.tailwindcss_4;
      };
      description = "The Sunday Slate server package.";
    };

    settings = lib.mkOption {
      type = format.type;
      default = { };
      description = ''
        Non-secret config.toml settings, using the application's snake_case
        names. Omitted settings use application defaults. This file enters
        the Nix store; provide passwords and tokens through environmentFile.
      '';
    };

    environmentFile = lib.mkOption {
      type = lib.types.nullOr lib.types.str;
      default = null;
      example = "/run/secrets/sunday-slate";
      description = ''
        Absolute path to a runtime systemd environment file containing
        SUNDAY_SLATE__* variables. Keep it outside the Nix store and readable
        only by root. Use a quoted path string, not a Nix path literal.
      '';
    };
  };

  config = lib.mkIf cfg.enable {
    systemd.services.sunday-slate = {
      description = "Sunday Slate fantasy football server";
      wantedBy = [ "multi-user.target" ];
      after = [ "network-online.target" ];
      wants = [ "network-online.target" ];

      preStart = ''
        install -d -m 700 /var/lib/sunday-slate/storage
        install -m 600 ${configFile} /var/lib/sunday-slate/config.toml
      '';

      serviceConfig = {
        User = "sunday-slate";
        Group = "sunday-slate";
        WorkingDirectory = "/var/lib/sunday-slate";
        StateDirectory = "sunday-slate";
        StateDirectoryMode = "0700";
        ExecStart = "${cfg.package}/bin/sunday-slate";
        EnvironmentFile = lib.mkIf (cfg.environmentFile != null) cfg.environmentFile;
        Restart = "on-failure";
        UMask = "0077";

        NoNewPrivileges = true;
        PrivateTmp = true;
        ProtectHome = true;
        ProtectSystem = "strict";
      };
    };

    users.users.sunday-slate = {
      isSystemUser = true;
      group = "sunday-slate";
    };
    users.groups.sunday-slate = { };
  };
}
