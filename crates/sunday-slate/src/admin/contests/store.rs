//! Contest setup workflow. `set_up` is pure policy — "Weeks 1–18 always, plus
//! the chosen optionals, once" — composed over the domain write primitives in
//! `crate::contests::store`. The reusable contest domain lives in
//! `crate::contests`; only setup-time logic lives here.

use sqlx::SqliteConnection;

use crate::contests::store::{all, create};

/// Optional contests offered at setup, in display order: `(form_field, contest_name)`.
/// Weeks 1–18 are always present and are not in this list.
pub const OPTIONAL_CONTESTS: [(&str, &str); 5] = [
    ("thanksgiving", "Thanksgiving"),
    ("christmas", "Christmas"),
    ("wild_card", "Wild Card"),
    ("divisional", "Divisional"),
    ("championship", "Championship"),
];

/// The 18 always-present regular-season contest names, "Week 1"..="Week 18".
fn regular_week_names() -> Vec<String> {
    (1..=18).map(|w| format!("Week {w}")).collect()
}

/// Create the season's contests: Weeks 1–18 always, plus each optional named in
/// `optional_on`. One-time — the list is fixed once any contest exists, because
/// removing a contest would cascade its entries and their slots away. Returns
/// whether it created anything; `false` means the season was already set up and
/// nothing changed.
pub async fn set_up(
    conn: &mut SqliteConnection,
    optional_on: &[String],
) -> Result<bool, sqlx::Error> {
    if !all(&mut *conn).await?.is_empty() {
        return Ok(false);
    }
    let mut desired: Vec<String> = regular_week_names();
    for (_, name) in OPTIONAL_CONTESTS {
        if optional_on.iter().any(|n| n == name) {
            desired.push(name.to_string());
        }
    }
    for name in &desired {
        create(conn, name).await?;
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Db;
    use crate::contests::store::{game_ids, set_games};
    use sqlx::SqlitePool;

    #[sqlx::test]
    async fn set_up_creates_weeks_and_toggled_optionals(pool: SqlitePool) {
        let db = Db::test(pool.clone());
        db.write_tx::<_, bool, sqlx::Error>(async |conn| {
            set_up(conn, &["Thanksgiving".to_string()]).await
        })
        .await
        .unwrap();

        let names: Vec<String> = all(&pool)
            .await
            .unwrap()
            .into_iter()
            .map(|c| c.name)
            .collect();
        assert_eq!(names.iter().filter(|n| n.starts_with("Week ")).count(), 18);
        assert!(names.contains(&"Thanksgiving".to_string()));
        assert!(!names.contains(&"Christmas".to_string()));
        assert!(!names.contains(&"Wild Card".to_string()));
    }

    #[sqlx::test]
    async fn set_up_changes_nothing_once_contests_exist(pool: SqlitePool) {
        let db = Db::test(pool.clone());
        db.write_tx::<_, bool, sqlx::Error>(async |conn| {
            set_up(conn, &["Wild Card".to_string()]).await
        })
        .await
        .unwrap();
        let wc = all(&pool)
            .await
            .unwrap()
            .into_iter()
            .find(|c| c.name == "Wild Card")
            .unwrap();
        db.write_tx::<_, (), sqlx::Error>(async |conn| {
            set_games(conn, wc.id, &["2025_19_LA_CAR".to_string()]).await
        })
        .await
        .unwrap();

        // A second call reports it did nothing, and neither adds Thanksgiving nor
        // drops Wild Card or its frozen slate.
        let ran = db
            .write_tx::<_, bool, sqlx::Error>(async |conn| {
                set_up(conn, &["Thanksgiving".to_string()]).await
            })
            .await
            .unwrap();
        assert!(!ran, "already configured");

        let names: Vec<String> = all(&pool)
            .await
            .unwrap()
            .into_iter()
            .map(|c| c.name)
            .collect();
        assert!(names.contains(&"Wild Card".to_string()));
        assert!(!names.contains(&"Thanksgiving".to_string()));
        assert_eq!(names.len(), 19);
        assert_eq!(game_ids(&pool, wc.id).await.unwrap().len(), 1);
    }
}
