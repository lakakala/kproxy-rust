//! kproxy 的命令行入口。
//!
//! 程序可以运行在 server 模式或 client 模式。解析所选 TOML 配置文件后，
//! 具体运行逻辑会交给对应模式的模块处理。

use clap::{Parser, Subcommand};

mod client;
mod config;
mod crypto;
mod protocol;
mod server;
mod socks5;

#[derive(Parser)]
#[command(
    name = "kproxy",
    about = "TCP forwarding proxy with AES-256-GCM encryption"
)]
struct Cli {
    /// 选择当前进程作为公网服务端还是本地客户端运行。
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// 运行接收加密客户端控制连接的服务端。
    Server {
        /// 服务端 TOML 配置文件路径。
        #[arg(short, long, default_value = "server.toml")]
        config: String,
    },
    /// 运行负责注册转发并在本地监听的客户端。
    Client {
        /// 客户端 TOML 配置文件路径。
        #[arg(short, long, default_value = "client.toml")]
        config: String,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // 默认启用有用的应用日志，同时允许通过 RUST_LOG 覆盖或扩展过滤规则。
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("kproxy_rust=info".parse()?),
        )
        .init();

    let cli = Cli::parse();

    // 在进入所选运行模式之前加载对应配置。
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
