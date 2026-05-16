//! TOML 配置加载。
//!
//! 程序有两个模式，并分别使用不同配置文件：
//! - server 模式监听加密的客户端控制连接
//! - client 模式连接服务端，并暴露一个或多个本地监听地址

use serde::Deserialize;

/// 从 `server.toml` 加载的服务端配置。
#[derive(Debug, Deserialize)]
pub struct ServerConfig {
    /// 共享密钥 token，同时用于认证和密钥派生。
    pub token: String,
    /// 服务端接收客户端控制连接的 TCP 地址。
    pub listen_addr: String,
}

/// 从 `client.toml` 加载的客户端配置。
#[derive(Debug, Deserialize)]
pub struct ClientConfig {
    /// 共享密钥 token，必须与服务端 token 一致。
    pub token: String,
    /// 服务端控制地址，可以选择通过 SOCKS5 代理访问。
    pub server_addr: String,
    /// 认证后注册的反向转发规则。
    pub forwards: Vec<ForwardConfig>,
    /// 访问 `server_addr` 时可选的上游 SOCKS5 代理。
    pub socks5: Option<Socks5Config>,
}

/// 客户端控制连接可选的 SOCKS5 代理设置。
#[derive(Debug, Deserialize)]
pub struct Socks5Config {
    /// SOCKS5 代理 TCP 地址。
    pub addr: String,
    /// 可选用户名。仅当用户名和密码同时设置时才会使用。
    pub username: Option<String>,
    /// 可选密码。仅当用户名和密码同时设置时才会使用。
    pub password: Option<String>,
}

/// 一条反向转发规则。
///
/// `local_addr` 由客户端监听。当本地连接到达后，服务端连接
/// `remote_addr`，随后两端之间开始代理数据。
#[derive(Debug, Deserialize)]
pub struct ForwardConfig {
    /// 客户端侧监听地址，例如 `127.0.0.1:8080`。
    pub local_addr: String,
    /// 服务端侧目标地址，例如 `127.0.0.1:80`。
    pub remote_addr: String,
}

/// 加载并解析服务端 TOML 配置文件。
pub fn load_server_config(path: &str) -> anyhow::Result<ServerConfig> {
    let content = std::fs::read_to_string(path)?;
    let config: ServerConfig = toml::from_str(&content)?;
    Ok(config)
}

/// 加载并解析客户端 TOML 配置文件。
pub fn load_client_config(path: &str) -> anyhow::Result<ClientConfig> {
    let content = std::fs::read_to_string(path)?;
    let config: ClientConfig = toml::from_str(&content)?;
    Ok(config)
}
