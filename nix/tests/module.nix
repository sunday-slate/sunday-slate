# Pure-eval test cases for nix/module.nix (the NixOS module).
#
# The flake's module-settings check evaluates each case through
# lib.evalModules and asserts on the module's own outputs:
#   name            – identifier shown when the case fails
#   moduleConf      – attrset merged over { enable = true; } as module config
#   settingsKeys    – expected sorted attrset keys of the rendered settings
#                     record (the attrset that becomes config.toml)
#   settingsContain – JSON fragments that must appear in the rendered record
#   settingsOmit    – keys that must NOT appear in the rendered record
#   execStartContain / execStartNotContain – fragments in the ExecStart text
#   preStartContain – fragments in the preStart script text
#   loadCredential  – expected list of "name:path" credential entries
#   assertionFails  – when true, evaluating config must raise an assertion
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

  {
    name = "working-directory-cascades-into-urls-and-media";
    moduleConf = {
      services.sunday-slate.workingDirectory = "/srv/ss";
    };
    settingsContain = [
      "sqlite:///srv/ss/storage/sunday-slate.db"
      "sqlite:///srv/ss/storage/nfl-data.db"
      "/srv/ss/storage/media"
    ];
  }

  {
    name = "urls-remain-overridable-independently";
    moduleConf = {
      services.sunday-slate.databaseUrl = "sqlite:///mnt/db/sunday-slate.db";
      services.sunday-slate.workingDirectory = "/srv/ss";
    };
    settingsContain = [
      "sqlite:///mnt/db/sunday-slate.db"
      "sqlite:///srv/ss/storage/nfl-data.db"
    ];
  }

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
