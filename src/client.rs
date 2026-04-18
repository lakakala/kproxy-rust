use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, Mutex};
use tracing::{error, info, warn};

use crate::crypto;
use crate::protocol::{self, Frame, FrameType};
use crate::socks5;

pub async fn run(config: &crate::config::ClientConfig) -> anyhow::Result<()> {
    let key = crypto::derive_key(&config.token);

    let stream = if let Some(socks5_config) = &config.socks5 {
        let (host, port) = parse_host_port(&config.server_addr)?;
        info!(
            "Connecting to server {} via SOCKS5 proxy {}",
            config.server_addr, socks5_config.addr
        );
        socks5::connect(
            &socks5_config.addr,
            &host,
            port,
            socks5_config.username.as_deref(),
            socks5_config.password.as_deref(),
        )
        .await?
    } else {
        TcpStream::connect(&config.server_addr).await?
    };

    stream.set_nodelay(true)?;
    info!("Connected to server {}", config.server_addr);

    let (mut reader, writer) = tokio::io::split(stream);
    let writer = Arc::new(Mutex::new(writer));

    let auth_frame = Frame {
        frame_type: FrameType::Auth,
        conn_id: 0,
        data: config.token.as_bytes().to_vec(),
    };
    {
        let mut w = writer.lock().await;
        protocol::write_frame(&mut *w, &key, &auth_frame).await?;
    }

    let auth_result = protocol::read_frame(&mut reader, &key).await?;
    if !matches!(auth_result.frame_type, FrameType::AuthResult) {
        return Err(anyhow::anyhow!("Expected AuthResult frame"));
    }

    let result = String::from_utf8(auth_result.data)?;
    if result != "ok" {
        return Err(anyhow::anyhow!("Authentication failed: {}", result));
    }

    info!("Authenticated successfully");

    let (writer_tx, mut writer_rx) = mpsc::channel::<Vec<u8>>(4096);

    let writer_clone = writer.clone();
    let writer_handle = tokio::spawn(async move {
        while let Some(raw_frame) = writer_rx.recv().await {
            let mut w = writer_clone.lock().await;
            if let Err(e) = w.write_all(&raw_frame).await {
                error!("Control write error: {}", e);
                break;
            }
            if let Err(e) = w.flush().await {
                error!("Control flush error: {}", e);
                break;
            }
        }
    });

    let mut forward_map: HashMap<u32, (String, u32)> = HashMap::new();

    for forward in &config.forwards {
        let register_frame = Frame {
            frame_type: FrameType::RegisterForward,
            conn_id: 0,
            data: forward.remote_addr.as_bytes().to_vec(),
        };
        {
            let mut w = writer.lock().await;
            protocol::write_frame(&mut *w, &key, &register_frame).await?;
        }

        let result_frame = protocol::read_frame(&mut reader, &key).await?;
        if !matches!(result_frame.frame_type, FrameType::RegisterForwardResult) {
            return Err(anyhow::anyhow!("Expected RegisterForwardResult frame"));
        }

        if result_frame.data.is_empty() {
            return Err(anyhow::anyhow!("Invalid RegisterForwardResult"));
        }

        let status = result_frame.data[0];
        if status == 0x00 {
            if result_frame.data.len() < 5 {
                return Err(anyhow::anyhow!("Invalid RegisterForwardResult data"));
            }
            let forward_id = u32::from_be_bytes([
                result_frame.data[1],
                result_frame.data[2],
                result_frame.data[3],
                result_frame.data[4],
            ]);
            forward_map.insert(forward_id, (forward.local_addr.clone(), 0));
            info!(
                "Registered forward: {} -> {} (id={})",
                forward.local_addr, forward.remote_addr, forward_id
            );
        } else {
            let error_msg = String::from_utf8_lossy(&result_frame.data[1..]);
            return Err(anyhow::anyhow!(
                "Failed to register forward {} -> {}: {}",
                forward.local_addr,
                forward.remote_addr,
                error_msg
            ));
        }
    }

    let connections: Arc<Mutex<HashMap<u32, tokio::io::WriteHalf<TcpStream>>>> =
        Arc::new(Mutex::new(HashMap::new()));
    let next_conn_id: Arc<AtomicU32> = Arc::new(AtomicU32::new(1));

    let mut tasks = tokio::task::JoinSet::new();

    for (&forward_id, (local_addr, _)) in &forward_map {
        let listener = match TcpListener::bind(local_addr).await {
            Ok(l) => l,
            Err(e) => {
                return Err(anyhow::anyhow!(
                    "Failed to bind listener on {}: {}",
                    local_addr,
                    e
                ));
            }
        };
        info!("Listening on {} for forward {}", local_addr, forward_id);

        let tx = writer_tx.clone();
        let conns = connections.clone();
        let nid = next_conn_id.clone();
        let l_key = key;
        let l_forward_id = forward_id;

        tasks.spawn(async move {
            loop {
                match listener.accept().await {
                    Ok((stream, addr)) => {
                        info!(
                            "New connection on forward {}: {}",
                            l_forward_id, addr
                        );

                        let conn_id = nid.fetch_add(1, Ordering::Relaxed);

                        let _ = stream.set_nodelay(true);
                        let (read_half, write_half) = tokio::io::split(stream);
                        {
                            let mut c = conns.lock().await;
                            c.insert(conn_id, write_half);
                        }

                        let mut data = vec![0x00];
                        data.extend_from_slice(&l_forward_id.to_be_bytes());
                        let frame = Frame {
                            frame_type: FrameType::NewConnection,
                            conn_id,
                            data,
                        };
                        if protocol::send_frame(&tx, &l_key, &frame).is_err() {
                            let mut c = conns.lock().await;
                            c.remove(&conn_id);
                            continue;
                        }

                        let r_tx = tx.clone();
                        let r_conns = conns.clone();
                        let r_key = l_key;

                        tokio::spawn(async move {
                            let mut reader = read_half;
                            let mut buf = vec![0u8; 32768];
                            loop {
                                match reader.read(&mut buf).await {
                                    Ok(0) => break,
                                    Ok(n) => {
                                        let frame = Frame {
                                            frame_type: FrameType::Data,
                                            conn_id,
                                            data: buf[..n].to_vec(),
                                        };
                                        if protocol::send_frame(&r_tx, &r_key, &frame).is_err() {
                                            break;
                                        }
                                    }
                                    Err(_) => break,
                                }
                            }

                            {
                                let mut c = r_conns.lock().await;
                                c.remove(&conn_id);
                            }
                            let close_frame = Frame {
                                frame_type: FrameType::CloseConnection,
                                conn_id,
                                data: vec![],
                            };
                            let _ = protocol::send_frame(&r_tx, &r_key, &close_frame);
                        });
                    }
                    Err(e) => {
                        error!(
                            "Accept error on forward {}: {}",
                            l_forward_id, e
                        );
                        break;
                    }
                }
            }
        });
    }

    loop {
        let frame = match protocol::read_frame(&mut reader, &key).await {
            Ok(f) => f,
            Err(e) => {
                let msg = e.to_string();
                if msg.contains("unexpected eof")
                    || msg.contains("EOF")
                    || msg.contains("reset")
                {
                    info!("Disconnected from server");
                } else {
                    error!("Read frame error: {}", e);
                }
                break;
            }
        };

        match frame.frame_type {
            FrameType::Data => {
                let conn_id = frame.conn_id;
                let mut conns = connections.lock().await;
                if let Some(write_half) = conns.get_mut(&conn_id) {
                    if let Err(e) = write_half.write_all(&frame.data).await {
                        warn!("Write to local connection {} error: {}", conn_id, e);
                        conns.remove(&conn_id);
                        drop(conns);
                        let close_frame = Frame {
                            frame_type: FrameType::CloseConnection,
                            conn_id,
                            data: vec![],
                        };
                        let _ = protocol::send_frame(&writer_tx, &key, &close_frame);
                    }
                }
            }
            FrameType::CloseConnection => {
                let mut conns = connections.lock().await;
                conns.remove(&frame.conn_id);
            }
            _ => {
                warn!("Unexpected frame type: 0x{:02x}", frame.frame_type as u8);
            }
        }
    }

    drop(writer_tx);
    writer_handle.abort();
    tasks.abort_all();

    Ok(())
}

fn parse_host_port(addr: &str) -> anyhow::Result<(String, u16)> {
    let (host, port_str) = if addr.starts_with('[') {
        let close_bracket = addr
            .find(']')
            .ok_or_else(|| anyhow::anyhow!("Invalid IPv6 address: {}", addr))?;
        let host = addr[1..close_bracket].to_string();
        let rest = &addr[close_bracket + 1..];
        if rest.starts_with(':') {
            (host, &rest[1..])
        } else {
            return Err(anyhow::anyhow!("Missing port in address: {}", addr));
        }
    } else {
        let colon_pos = addr
            .rfind(':')
            .ok_or_else(|| anyhow::anyhow!("Missing port in address: {}", addr))?;
        (addr[..colon_pos].to_string(), &addr[colon_pos + 1..])
    };

    let port = port_str
        .parse::<u16>()
        .map_err(|_| anyhow::anyhow!("Invalid port: {}", port_str))?;

    Ok((host, port))
}
