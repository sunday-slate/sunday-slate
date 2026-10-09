use std::env;

#[tokio::main]
async fn main() {
    let database_url = env::var("NFL_DATA_DATABASE_URL")
        .unwrap_or_else(|_| "sqlite://./storage/nfl-data.db".to_owned());
    if let Err(error) = nfl_data::migrate_managed_database(&database_url).await {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
