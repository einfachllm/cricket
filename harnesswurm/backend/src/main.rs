use harnesswurm_backend::{run, ServerConfig};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    let bind_addr = std::env::var("BIND_ADDR").unwrap_or_else(|_| "127.0.0.1:8081".to_string());
    let data_dir = std::env::current_dir()?;

    run(ServerConfig { bind_addr, data_dir }).await
}
