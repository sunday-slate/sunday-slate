let
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
