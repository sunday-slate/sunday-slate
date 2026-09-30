let
  base = {
    bindAddr = "127.0.0.1:3000";
    baseUrl = "http://localhost:3000";
    mailFrom = "Sunday Slate <no-reply@example.com>";
    season = 2026;
    databaseUrl = "sqlite:///var/lib/sunday-slate/storage/sunday-slate.db";
    nflDatabaseUrl = "sqlite:///var/lib/sunday-slate/storage/nfl-data.db";
    mediaDir = "/var/lib/sunday-slate/storage/media";
    nflSyncIntervalSecs = 0;
    smtp = null;
  };
in
[
  # (a) full record → every TOML key present, smtp block absent
  {
    name = "full-record-renders-everything";
    input = base;
    contain = [
      "bind_addr"
      "base_url"
      "mail_from"
      "season"
      "database_url"
      "nfl_database_url"
      "media_dir"
      "nfl_sync_interval_secs"
    ];
    omit = [ "smtp" ];
  }

  # (b) season = null → the season key is omitted, app default applies
  {
    name = "season-null-omits-season";
    input = base // { season = null; };
    omit = [ "season" "smtp" ];
  }

  # (c) smtp = null → no smtp section
  {
    name = "smtp-null-omits-smtp";
    input = base // { smtp = null; };
    omit = [ "smtp" ];
  }

  # (d) smtp set → nested host/port/username present
  {
    name = "smtp-set-renders-block";
    input = base // {
      smtp = {
        host = "smtp.example.com";
        port = 587;
        username = "user";
      };
    };
    contain = [ "smtp" "host" "587" "username" ];
  }
]
