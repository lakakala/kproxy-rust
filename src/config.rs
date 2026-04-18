use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct ServerConfig {
    pub token: String,
    pub listen_addr: String,
}

#[derive(Debug, Deserialize)]
pub struct ClientConfig {
    pub token: String,
    pub server_addr: String,
    pub forwards: Vec<ForwardConfig>,
    pub socks5: Option<Socks5Config>,
}

#[derive(Debug, Deserialize)]
pub struct Socks5Config {
    pub addr: String,
    pub username: Option<String>,
    pub password: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ForwardConfig {
    pub local_addr: String,
    pub remote_addr: String,
}

pub fn load_server_config(path: &str) -> anyhow::Result<ServerConfig> {
    let content = std::fs::read_to_string(path)?;
    let config: ServerConfig = toml::from_str(&content)?;
    Ok(config)
}

pub fn load_client_config(path: &str) -> anyhow::Result<ClientConfig> {
    let content = std::fs::read_to_string(path)?;
    let config: ClientConfig = toml::from_str(&content)?;
    Ok(config)
}
