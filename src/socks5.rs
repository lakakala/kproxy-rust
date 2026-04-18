use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

pub async fn connect(
    proxy_addr: &str,
    target_addr: &str,
    target_port: u16,
    username: Option<&str>,
    password: Option<&str>,
) -> anyhow::Result<TcpStream> {
    let mut stream = TcpStream::connect(proxy_addr).await?;
    stream.set_nodelay(true)?;

    let has_auth = username.is_some() && password.is_some();

    if has_auth {
        stream.write_all(&[0x05, 0x02, 0x00, 0x02]).await?;
    } else {
        stream.write_all(&[0x05, 0x01, 0x00]).await?;
    }
    stream.flush().await?;

    let mut greeting = [0u8; 2];
    stream.read_exact(&mut greeting).await?;
    if greeting[0] != 0x05 {
        return Err(anyhow::anyhow!("Invalid SOCKS5 version: 0x{:02x}", greeting[0]));
    }

    match greeting[1] {
        0x00 => {}
        0x02 => {
            let user = username.unwrap_or("");
            let pass = password.unwrap_or("");
            if user.is_empty() || pass.is_empty() {
                return Err(anyhow::anyhow!(
                    "SOCKS5 proxy requires authentication but no credentials provided"
                ));
            }
            if user.len() > 255 || pass.len() > 255 {
                return Err(anyhow::anyhow!("SOCKS5 username/password too long"));
            }

            let mut auth_req = Vec::with_capacity(3 + user.len() + pass.len());
            auth_req.push(0x01);
            auth_req.push(user.len() as u8);
            auth_req.extend_from_slice(user.as_bytes());
            auth_req.push(pass.len() as u8);
            auth_req.extend_from_slice(pass.as_bytes());

            stream.write_all(&auth_req).await?;
            stream.flush().await?;

            let mut auth_resp = [0u8; 2];
            stream.read_exact(&mut auth_resp).await?;
            if auth_resp[1] != 0x00 {
                return Err(anyhow::anyhow!("SOCKS5 authentication failed"));
            }
        }
        0xFF => {
            return Err(anyhow::anyhow!(
                "SOCKS5 proxy: no acceptable authentication method"
            ));
        }
        method => {
            return Err(anyhow::anyhow!(
                "SOCKS5 proxy: unsupported auth method 0x{:02x}",
                method
            ));
        }
    }

    let mut connect_req = Vec::with_capacity(1 + 1 + 1 + 1 + target_addr.len() + 2);
    connect_req.push(0x05);
    connect_req.push(0x01);
    connect_req.push(0x00);
    connect_req.push(0x03);
    connect_req.push(target_addr.len() as u8);
    connect_req.extend_from_slice(target_addr.as_bytes());
    connect_req.extend_from_slice(&target_port.to_be_bytes());

    stream.write_all(&connect_req).await?;
    stream.flush().await?;

    let mut resp_header = [0u8; 4];
    stream.read_exact(&mut resp_header).await?;
    if resp_header[0] != 0x05 {
        return Err(anyhow::anyhow!("Invalid SOCKS5 response version"));
    }
    match resp_header[1] {
        0x00 => {}
        0x01 => return Err(anyhow::anyhow!("SOCKS5: general SOCKS server failure")),
        0x02 => return Err(anyhow::anyhow!("SOCKS5: connection not allowed by ruleset")),
        0x03 => return Err(anyhow::anyhow!("SOCKS5: network unreachable")),
        0x04 => return Err(anyhow::anyhow!("SOCKS5: host unreachable")),
        0x05 => return Err(anyhow::anyhow!("SOCKS5: connection refused")),
        0x06 => return Err(anyhow::anyhow!("SOCKS5: TTL expired")),
        0x07 => return Err(anyhow::anyhow!("SOCKS5: command not supported")),
        0x08 => return Err(anyhow::anyhow!("SOCKS5: address type not supported")),
        code => return Err(anyhow::anyhow!("SOCKS5: unknown error 0x{:02x}", code)),
    }

    match resp_header[3] {
        0x01 => {
            let mut buf = [0u8; 4 + 2];
            stream.read_exact(&mut buf).await?;
        }
        0x03 => {
            let mut len_buf = [0u8; 1];
            stream.read_exact(&mut len_buf).await?;
            let domain_len = len_buf[0] as usize;
            let mut buf = vec![0u8; domain_len + 2];
            stream.read_exact(&mut buf).await?;
        }
        0x04 => {
            let mut buf = [0u8; 16 + 2];
            stream.read_exact(&mut buf).await?;
        }
        atyp => {
            return Err(anyhow::anyhow!("SOCKS5: unknown address type 0x{:02x}", atyp));
        }
    }

    Ok(stream)
}
