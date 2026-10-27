use std::process::ExitCode;

use clap::Parser;
use nfl_data::migrate_salary_links;

#[derive(Parser)]
#[command(about = "One-time transfer of Sunday Slate player links into the reconciliation store")]
struct Args {
    /// SQLite URL of the pre-cutover Sunday Slate database (read-only source)
    #[arg(long)]
    source: String,

    /// SQLite URL of the reconciliation database (created if missing)
    #[arg(long)]
    destination: String,
}

#[tokio::main]
async fn main() -> ExitCode {
    let args = Args::parse();
    run(
        &args.source,
        &args.destination,
        &mut std::io::stdout(),
        &mut std::io::stderr(),
    )
    .await
}

async fn run(
    source: &str,
    destination: &str,
    stdout: &mut impl std::io::Write,
    stderr: &mut impl std::io::Write,
) -> ExitCode {
    match migrate_salary_links(source, destination).await {
        Ok(report) => {
            let _ = writeln!(
                stdout,
                "Copied {} player links; {} already present.",
                report.copied, report.already_present
            );
            ExitCode::SUCCESS
        }
        Err(e) => {
            let _ = writeln!(stderr, "error: {e:#}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_db(name: &str) -> (tempfile::TempDir, String) {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join(format!("{name}.db"));
        std::fs::File::create(&file).unwrap();
        (dir, format!("sqlite://{}", file.display()))
    }

    #[tokio::test]
    async fn copies_links_and_preserves_timestamps() {
        let (_dir, source) = tmp_db("legacy");
        let (_dir2, dest) = tmp_db("links");

        let pool = sqlx::SqlitePool::connect(&source).await.unwrap();
        sqlx::raw_sql(
            "CREATE TABLE nfl_players (
                id INTEGER PRIMARY KEY,
                gsis_player_id TEXT NOT NULL UNIQUE,
                fd_player_id INTEGER UNIQUE,
                created_at TEXT NOT NULL DEFAULT (datetime('now','subsec')),
                updated_at TEXT NOT NULL DEFAULT (datetime('now','subsec'))
            );
            INSERT INTO nfl_players (gsis_player_id, fd_player_id, created_at, updated_at)
            VALUES ('00-AAA', 111, '2026-01-01 10:00:00', '2026-01-02 10:00:00'),
                   ('00-BBB', 222, '2026-01-03 10:00:00', '2026-01-04 10:00:00');
            INSERT INTO nfl_players (gsis_player_id, created_at, updated_at)
            VALUES ('00-NULL-FD', '2026-01-05 10:00:00', '2026-01-05 10:00:00');",
        )
        .execute(&pool)
        .await
        .unwrap();

        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = run(&source, &dest, &mut out, &mut err).await;
        assert_eq!(code, ExitCode::SUCCESS);
        assert_eq!(
            String::from_utf8_lossy(&out),
            "Copied 2 player links; 0 already present.\n"
        );

        let dest_pool = sqlx::SqlitePool::connect(&dest).await.unwrap();
        let rows: Vec<(i64, String, String, String)> = sqlx::query_as(
            "SELECT fd_player_id, gsis_player_id, created_at, updated_at FROM player_links ORDER BY fd_player_id",
        )
        .fetch_all(&dest_pool)
        .await
        .unwrap();
        assert_eq!(
            rows,
            vec![
                (
                    111,
                    "00-AAA".into(),
                    "2026-01-01 10:00:00".into(),
                    "2026-01-02 10:00:00".into()
                ),
                (
                    222,
                    "00-BBB".into(),
                    "2026-01-03 10:00:00".into(),
                    "2026-01-04 10:00:00".into()
                ),
            ]
        );
        let tables: Vec<String> =
            sqlx::query_scalar("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
                .fetch_all(&dest_pool)
                .await
                .unwrap();
        assert_eq!(
            tables,
            vec!["_sqlx_migrations".to_owned(), "player_links".to_owned()]
        );

        // Source untouched.
        let source_rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM nfl_players")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(source_rows, 3);

        // Second run: idempotent.
        let mut out2 = Vec::new();
        let code2 = run(&source, &dest, &mut out2, &mut Vec::new()).await;
        assert_eq!(code2, ExitCode::SUCCESS);
        assert_eq!(
            String::from_utf8_lossy(&out2),
            "Copied 0 player links; 2 already present.\n"
        );
    }

    #[tokio::test]
    async fn rejects_same_file_and_missing_source() {
        let (_dir, url) = tmp_db("same");

        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = run(&url, &url, &mut out, &mut err).await;
        assert_eq!(code, ExitCode::FAILURE);
        assert!(String::from_utf8_lossy(&err).contains("same SQLite file"));
        assert!(out.is_empty());

        // Missing source file: fails and must not create the destination.
        let (_, dest_missing) = tmp_db("never-created");
        let mut err2 = Vec::new();
        let code2 = run(
            "sqlite:///tmp/does-not-exist-anywhere-9c3e.db",
            &dest_missing,
            &mut Vec::new(),
            &mut err2,
        )
        .await;
        assert_eq!(code2, ExitCode::FAILURE);
        assert!(String::from_utf8_lossy(&err2).contains("legacy player links are unavailable"));
        assert!(
            !std::path::Path::new(dest_missing.trim_start_matches("sqlite://")).exists(),
            "destination must not be created on failure"
        );
    }

    #[tokio::test]
    async fn rejects_alias_paths_to_the_same_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("alias.db");
        std::fs::File::create(&file).unwrap();
        // Create a real nfl_players table so the failure comes from the
        // same-file guard, not the missing legacy table.
        let pool = sqlx::SqlitePool::connect(&format!("sqlite://{}", file.display()))
            .await
            .unwrap();
        sqlx::raw_sql("CREATE TABLE nfl_players (id INTEGER PRIMARY KEY);")
            .execute(&pool)
            .await
            .unwrap();

        // A `.`-spelling of the same file vs. its plain absolute path: distinct
        // strings, same inode.
        let absolute = format!("sqlite://{}", file.display());
        let dotted = format!("sqlite://{}/./alias.db", dir.path().display());
        let mut err = Vec::new();
        let code = run(&dotted, &absolute, &mut Vec::new(), &mut err).await;
        assert_eq!(code, ExitCode::FAILURE, "dot spelling vs absolute path");
        assert!(String::from_utf8_lossy(&err).contains("same file through different paths"));

        // Symlink to the same file.
        let symlink = dir.path().join("alias-link.db");
        std::os::unix::fs::symlink(&file, &symlink).unwrap();
        let mut err = Vec::new();
        let code = run(
            &absolute,
            &format!("sqlite://{}", symlink.display()),
            &mut Vec::new(),
            &mut err,
        )
        .await;
        assert_eq!(code, ExitCode::FAILURE, "symlink alias");
        assert!(String::from_utf8_lossy(&err).contains("same file through different paths"));

        // Hard link to the same file.
        let hardlink = dir.path().join("alias-hard.db");
        std::fs::hard_link(&file, &hardlink).unwrap();
        let mut err = Vec::new();
        let code = run(
            &absolute,
            &format!("sqlite://{}", hardlink.display()),
            &mut Vec::new(),
            &mut err,
        )
        .await;
        assert_eq!(code, ExitCode::FAILURE, "hard link alias");
        assert!(String::from_utf8_lossy(&err).contains("same file through different paths"));

        // A distinct file passes the guard (fails later on the missing legacy
        // column, proving the guard itself did not reject it).
        let other = dir.path().join("other.db");
        std::fs::File::create(&other).unwrap();
        let mut err = Vec::new();
        let code = run(
            &absolute,
            &format!("sqlite://{}", other.display()),
            &mut Vec::new(),
            &mut err,
        )
        .await;
        assert_eq!(code, ExitCode::FAILURE, "distinct file passes the guard");
        assert!(
            !String::from_utf8_lossy(&err).contains("same file"),
            "guard must not reject distinct files: {err:?}"
        );
    }

    #[tokio::test]
    async fn rejects_percent_encoded_alias_paths() {
        // SQLx percent-decodes the URL filename, so `app%2Edb` opens
        // `app.db`; the guard must see the decoded path sqlx will use, not
        // the encoded spelling.
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("app.db");
        std::fs::File::create(&file).unwrap();
        let pool = sqlx::SqlitePool::connect(&format!("sqlite://{}", file.display()))
            .await
            .unwrap();
        sqlx::raw_sql("CREATE TABLE nfl_players (id INTEGER PRIMARY KEY);")
            .execute(&pool)
            .await
            .unwrap();

        let plain = format!("sqlite://{}", file.display());
        let encoded = format!("sqlite://{}", dir.path().join("app%2Edb").display());
        assert_ne!(plain, encoded, "spellings differ textually");

        let mut err = Vec::new();
        let code = run(&plain, &encoded, &mut Vec::new(), &mut err).await;
        assert_eq!(code, ExitCode::FAILURE, "encoded spelling vs plain path");
        let err = String::from_utf8_lossy(&err);
        assert!(
            err.contains("same SQLite file") || err.contains("same file through different paths"),
            "guard must reject the encoded alias: {err}"
        );

        // The store never opened/created the encoded alias as a new file.
        let alias_path = dir.path().join("app%2Edb");
        assert!(
            !alias_path.exists(),
            "the encoded spelling must not become its own file"
        );

        // Query parameters and `?mode=ro` spellings also resolve to the file.
        let with_params = format!("{plain}?mode=rw&cache=private");
        let mut err = Vec::new();
        let code = run(&with_params, &plain, &mut Vec::new(), &mut err).await;
        assert_eq!(code, ExitCode::FAILURE, "URL with parameters vs plain path");
        let err = String::from_utf8_lossy(&err);
        assert!(
            err.contains("same SQLite file") || err.contains("same file through different paths"),
            "guard must reject the parameterized alias: {err}"
        );
    }

    #[tokio::test]
    async fn empty_legacy_table_succeeds_with_zero_links() {
        let (_dir, source) = tmp_db("empty-legacy");
        let (_dir2, dest) = tmp_db("empty-dest");

        let pool = sqlx::SqlitePool::connect(&source).await.unwrap();
        sqlx::raw_sql(
            "CREATE TABLE nfl_players (
                id INTEGER PRIMARY KEY,
                gsis_player_id TEXT NOT NULL UNIQUE,
                fd_player_id INTEGER UNIQUE,
                created_at TEXT NOT NULL DEFAULT (datetime('now','subsec')),
                updated_at TEXT NOT NULL DEFAULT (datetime('now','subsec'))
            );",
        )
        .execute(&pool)
        .await
        .unwrap();

        let mut out = Vec::new();
        let code = run(&source, &dest, &mut out, &mut Vec::new()).await;
        assert_eq!(code, ExitCode::SUCCESS);
        assert_eq!(
            String::from_utf8_lossy(&out),
            "Copied 0 player links; 0 already present.\n"
        );
    }

    #[tokio::test]
    async fn source_without_fd_column_fails() {
        let (_dir, source) = tmp_db("no-fd");
        let (_dir2, dest) = tmp_db("no-fd-dest");

        let pool = sqlx::SqlitePool::connect(&source).await.unwrap();
        sqlx::raw_sql(
            "CREATE TABLE nfl_players (id INTEGER PRIMARY KEY, gsis_player_id TEXT NOT NULL UNIQUE);",
        )
        .execute(&pool)
        .await
        .unwrap();

        let mut err = Vec::new();
        let code = run(&source, &dest, &mut Vec::new(), &mut err).await;
        assert_eq!(code, ExitCode::FAILURE);
        assert!(String::from_utf8_lossy(&err).contains("legacy player links are unavailable"));
    }
}
