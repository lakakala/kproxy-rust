use clap::{Parser, Subcommand};

mod client;
mod config;
mod crypto;
mod protocol;
mod server;
mod socks5;

#[derive(Parser)]
#[command(name = "kproxy", about = "TCP forwarding proxy with AES-256-GCM encryption")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    Server {
        #[arg(short, long, default_value = "server.toml")]
        config: String,
    },
    Client {
        #[arg(short, long, default_value = "client.toml")]
        config: String,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("kproxy_rust=info".parse()?),
        )
        .init();

    let cli = Cli::parse();

    match cli.command {
        Commands::Server { config } => {
            let config = config::load_server_config(&config)?;
            server::run(&config).await?;
        }
        Commands::Client { config } => {
            let config = config::load_client_config(&config)?;
            client::run(&config).await?;
        }
    }

    Ok(())
}
