# Pure-eval test cases for nix/module.nix (the NixOS module).
#
# The flake's module-settings check evaluates each case through
# lib.evalModules and asserts on the module's own outputs:
#   name            – identifier shown when the case fails
#   moduleConf      – attrset deep-merged over { enable = true; } as module
#                     config
#   settingsKeys    – expected sorted attrset keys of the rendered settings
#                     record (the attrset that becomes config.toml)
#   settingsContain – JSON fragments that must appear in the rendered record
#   settingsOmit    – keys that must NOT appear in the rendered record
#   execStartContain / execStartNotContain – fragments in the ExecStart text
#   preStartContain – fragments in the preStart script text
#   loadCredential  – expected list of "name:path" credential entries
#   serviceConfig   – attrset of exact deep-equal expectations on the unit's
#                     serviceConfig fields (WorkingDirectory, ReadWritePaths…)
#   tmpfilesRules   – exact expected set of systemd.tmpfiles.rules entries
#                     the module emits (directory provisioning for non-default
#                     working/storage dirs; empty when both are covered)
#   assertionFails  – when true, evaluating config must raise an assertion
let
  # Non-default-both-dirs case expectations share shape:
  # - the unit chdirs into the custom working directory;
  # - ProtectSystem=strict gains ReadWritePaths for the dirs the app writes;
  # - tmpfiles rules (d, 0700, app uid/gid) provision them before the unit
  #   starts, so config.toml install and DB creation succeed.
  customDirsDefaults = {
    serviceConfig = {
      WorkingDirectory = "/srv/ss";
      ReadWritePaths = [ "/srv/ss" "/srv/ss/storage" ];
    };
    tmpfilesRules = [
      "d /srv/ss 0700 sunday-slate sunday-slate - -"
      "d /srv/ss/storage 0700 sunday-slate sunday-slate - -"
    ];
  };
in
[
  {
    name = "defaults-render-in-app-terms";
    moduleConf = { };
    settingsKeys = [
      "base_url"
      "bind_addr"
      "database_url"
      "mail_from"
      "media_dir"
      "nfl_database_url"
      "nfl_sync_interval_secs"
    ];
    settingsOmit = [ "season" "smtp" ];
    execStartContain = [ "sunday-slate" ];
    execStartNotContain = [ "CREDENTIALS_DIRECTORY" ];
    loadCredential = [ ];
    preStartContain = [ "install -m 600" "config.toml" ];
    serviceConfig = {
      WorkingDirectory = "/var/lib/sunday-slate";
      StateDirectory = "sunday-slate";
      ReadWritePaths = [ ];
    };
    tmpfilesRules = [ ];
  }

  {
    name = "smtp-secret-wired-through-credential";
    moduleConf = {
      services.sunday-slate.smtp = {
        host = "smtp.example.com";
        port = 587;
        username = "user";
      };
      services.sunday-slate.secrets.smtpPasswordFile = "/run/creds/ss-smtp-pass";
    };
    settingsOmit = [ "password" ];
    # The launcher script itself (which reads $CREDENTIALS_DIRECTORY) is a
    # store-path-embedded derivation text not eval-visible here; the VM test
    # greps the launcher file on the running system for that pin.
    execStartContain = [ "sunday-slate-launcher" ];
    execStartNotContain = [ "EnvironmentFile" ];
    loadCredential = [ "smtp-password:/run/creds/ss-smtp-pass" ];
  }

  (customDirsDefaults // {
    name = "working-directory-cascades-into-urls-and-media";
    moduleConf = {
      services.sunday-slate.workingDirectory = "/srv/ss";
    };
    settingsContain = [
      "sqlite:///srv/ss/storage/sunday-slate.db"
      "sqlite:///srv/ss/storage/nfl-data.db"
      "/srv/ss/storage/media"
    ];
  })

  (customDirsDefaults // {
    name = "urls-remain-overridable-independently";
    moduleConf = {
      services.sunday-slate.databaseUrl = "sqlite:///mnt/db/sunday-slate.db";
      services.sunday-slate.workingDirectory = "/srv/ss";
    };
    settingsContain = [
      "sqlite:///mnt/db/sunday-slate.db"
      "sqlite:///srv/ss/storage/nfl-data.db"
    ];
  })

  {
    name = "season-passes-through-when-set";
    moduleConf = {
      services.sunday-slate.season = 2025;
    };
    settingsContain = [ "season" ];
  }

  {
    name = "smtp-host-requires-password";
    moduleConf = {
      services.sunday-slate.smtp = {
        host = "smtp.example.com";
        username = "user";
      };
    };
    assertionFails = true;
  }
]
