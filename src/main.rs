//! kproxy 的命令行入口。
//!
//! 程序可以运行在 server 模式或 client 模式。解析所选 TOML 配置文件后，
//! 具体运行逻辑会交给对应模式的模块处理。

use clap::{Parser, Subcommand};
use tracing::info;

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

    // 关闭信号通过 watch 通道广播给运行循环及其所有子任务，使其能优雅收尾
    // （flush 积压数据、通知对端关闭）后正常返回，而不是被进程强杀。
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    tokio::spawn(async move {
        shutdown_signal().await;
        let _ = shutdown_tx.send(true);
    });

    // 在进入所选运行模式之前加载对应配置。run 内部观察 shutdown 并自行 drain，
    // drain 完成后返回，main 据此干净退出（退出码 0）。
    match cli.command {
        Commands::Server { config } => {
            let config = config::load_server_config(&config)?;
            server::run(&config, shutdown_rx).await?;
        }
        Commands::Client { config } => {
            let config = config::load_client_config(&config)?;
            client::run(&config, shutdown_rx).await?;
        }
    }

    info!("Shutdown complete");
    Ok(())
}

/// 等待进程级关闭信号：SIGINT（Ctrl-C）或 SIGTERM。
///
/// 非 unix 平台只监听 Ctrl-C，SIGTERM 分支用永不就绪的 future 占位。
async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut sig) => {
                sig.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }

    info!("Shutdown signal received; draining...");
}
