# Pure-eval test cases for nix/settings.nix (mkSettings).
#
# Each case is a record:
#   name    – identifier shown in the assertion when the case fails
#   input   – the cfgAttrset exactly as the module would pass it (camelCase
#             option names, matching the Produces contract in the plan)
#   omit    – TOML keys that must NOT appear in the rendered settings
#   contain – fragments that MUST appear in the JSON serialization
let
  # The module always passes a fully-defaulted record; `null` marks the
  # options with a null default (season, smtp).
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
