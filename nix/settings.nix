# Options record → TOML attrset for the sunday-slate service module.
#
# mkSettings maps the module's camelCase option values onto the snake_case
# keys the app reads from config.toml (crates/sunday-slate/src/config.rs).
# `null` values are dropped: the generated config.toml simply omits them, so
# the app applies its own serde defaults. Exports exactly mkSettings.
{
  mkSettings = cfg:
    let
      withoutNulls = set:
        builtins.removeAttrs set
          (builtins.filter (k: set.${k} == null) (builtins.attrNames set));
    in
    withoutNulls {
      # Top-level: always present (the module resolves defaults itself).
      bind_addr = cfg.bindAddr;
      base_url = cfg.baseUrl;
      mail_from = cfg.mailFrom;
      database_url = cfg.databaseUrl;
      nfl_database_url = cfg.nflDatabaseUrl;
      media_dir = cfg.mediaDir;
      nfl_sync_interval_secs = cfg.nflSyncIntervalSecs;

      # Optional: null → key omitted entirely.
      season = cfg.season;
      smtp =
        if cfg.smtp == null then null
        else {
          host = cfg.smtp.host;
          port = cfg.smtp.port;
          username = cfg.smtp.username;
        };
    };
}
