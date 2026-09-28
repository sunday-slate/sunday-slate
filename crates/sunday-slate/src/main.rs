#[tokio::main]
async fn main() -> anyhow::Result<()> {
    sunday_slate::init_tracing()?;

    let config = sunday_slate::Config::load()?;
    let bind_addr = config.bind_addr.clone();
    let app = sunday_slate::App::build(config).await?;

    app.serve(&bind_addr).await
}
