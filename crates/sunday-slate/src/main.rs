#[tokio::main]
async fn main() -> anyhow::Result<()> {
    sunday_slate::init_tracing()?;

    let config = match sunday_slate::Config::load() {
        Ok(config) => config,
        Err(error) => {
            tracing::error!(stage = "load_config", error = ?error, "startup failed");
            return Err(error);
        }
    };
    let bind_addr = config.bind_addr.clone();
    let app = match sunday_slate::App::build(config).await {
        Ok(app) => app,
        Err(error) => {
            tracing::error!(stage = "build_app", error = ?error, "startup failed");
            return Err(error);
        }
    };

    if let Err(error) = app.serve(&bind_addr).await {
        tracing::error!(stage = "serve", error = ?error, "server failed");
        return Err(error);
    }
    Ok(())
}
