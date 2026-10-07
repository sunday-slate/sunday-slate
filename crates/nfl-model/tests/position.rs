use nfl_model::DfsPosition;
use sqlx::SqlitePool;

#[tokio::test]
async fn shared_position_preserves_database_roundtrip() {
    let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
    sqlx::query("CREATE TABLE positions (position TEXT NOT NULL)")
        .execute(&pool)
        .await
        .unwrap();
    for position in [DfsPosition::Qb, DfsPosition::Dst] {
        sqlx::query("INSERT INTO positions (position) VALUES (?)")
            .bind(position)
            .execute(&pool)
            .await
            .unwrap();
    }
    let stored: Vec<String> = sqlx::query_scalar("SELECT position FROM positions ORDER BY rowid")
        .fetch_all(&pool)
        .await
        .unwrap();
    assert_eq!(stored, ["QB", "DST"]);
    assert_eq!(DfsPosition::Qb.label(), "QB");
    assert_eq!(DfsPosition::Dst.label(), "D/ST");
    assert!(!DfsPosition::Qb.is_defense());
    assert!(DfsPosition::Dst.is_defense());
}
