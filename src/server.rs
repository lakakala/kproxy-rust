use std::collections::HashMap;
use std::sync::Arc;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, Mutex};
use tracing::{error, info, warn};

use crate::crypto;
use crate::protocol::{self, Frame, FrameType};

pub async fn run(config: &crate::config::ServerConfig) -> anyhow::Result<()> {
    let listener = TcpListener::bind(&config.listen_addr).await?;
    info!("Server listening on {}", config.listen_addr);

    let key = crypto::derive_key(&config.token);
    let expected_token = config.token.clone();

    loop {
        let (stream, addr) = listener.accept().await?;
        info!("New connection from {}", addr);

        let token = expected_token.clone();

        tokio::spawn(async move {
            if let Err(e) = handle_client(stream, key, &token).await {
                error!("Client handler error: {}", e);
            }
        });
    }
}

async fn handle_client(
    stream: TcpStream,
    key: [u8; 32],
    expected_token: &str,
) -> anyhow::Result<()> {
    stream.set_nodelay(true)?;
    let (mut reader, writer) = tokio::io::split(stream);
    let writer = Arc::new(Mutex::new(writer));

    let frame = protocol::read_frame(&mut reader, &key).await?;
    if !matches!(frame.frame_type, FrameType::Auth) {
        return Err(anyhow::anyhow!("Expected Auth frame"));
    }

    let token = String::from_utf8(frame.data)?;
    if token != expected_token {
        let response = Frame {
            frame_type: FrameType::AuthResult,
            conn_id: 0,
            data: b"auth failed".to_vec(),
        };
        let mut w = writer.lock().await;
        protocol::write_frame(&mut *w, &key, &response).await?;
        return Err(anyhow::anyhow!("Authentication failed"));
    }

    info!("Client authenticated");

    let response = Frame {
        frame_type: FrameType::AuthResult,
        conn_id: 0,
        data: b"ok".to_vec(),
    };
    {
        let mut w = writer.lock().await;
        protocol::write_frame(&mut *w, &key, &response).await?;
    }

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

    let connections: Arc<Mutex<HashMap<u32, tokio::io::WriteHalf<TcpStream>>>> =
        Arc::new(Mutex::new(HashMap::new()));
    let mut forward_map: HashMap<u32, String> = HashMap::new();
    let mut forward_id_counter: u32 = 0;

    loop {
        let frame = match protocol::read_frame(&mut reader, &key).await {
            Ok(f) => f,
            Err(e) => {
                let msg = e.to_string();
                if msg.contains("unexpected eof")
                    || msg.contains("EOF")
                    || msg.contains("reset")
                {
                    info!("Client disconnected");
                } else {
                    error!("Read frame error: {}", e);
                }
                break;
            }
        };

        match frame.frame_type {
            FrameType::RegisterForward => {
                let remote_addr = String::from_utf8(frame.data)?;
                forward_id_counter += 1;
                let forward_id = forward_id_counter;
                forward_map.insert(forward_id, remote_addr.clone());

                info!(
                    "Registered forward {}: -> {}",
                    forward_id, remote_addr
                );

                let mut data = vec![0x00];
                data.extend_from_slice(&forward_id.to_be_bytes());
                let response = Frame {
                    frame_type: FrameType::RegisterForwardResult,
                    conn_id: 0,
                    data,
                };
                let _ = protocol::send_frame(&writer_tx, &key, &response);
            }
            FrameType::NewConnection => {
                if frame.data.len() < 5 {
                    warn!("Invalid NewConnection frame");
                    let close_frame = Frame {
                        frame_type: FrameType::CloseConnection,
                        conn_id: frame.conn_id,
                        data: vec![0x01],
                    };
                    let _ = protocol::send_frame(&writer_tx, &key, &close_frame);
                    continue;
                }
                let forward_id = u32::from_be_bytes([
                    frame.data[1],
                    frame.data[2],
                    frame.data[3],
                    frame.data[4],
                ]);
                let conn_id = frame.conn_id;

                let remote_addr = match forward_map.get(&forward_id) {
                    Some(addr) => addr.clone(),
                    None => {
                        warn!("Unknown forward id: {}", forward_id);
                        let close_frame = Frame {
                            frame_type: FrameType::CloseConnection,
                            conn_id,
                            data: vec![0x01],
                        };
                        let _ = protocol::send_frame(&writer_tx, &key, &close_frame);
                        continue;
                    }
                };

                let remote_stream = match TcpStream::connect(&remote_addr).await {
                    Ok(s) => s,
                    Err(e) => {
                        warn!("Failed to connect to {}: {}", remote_addr, e);
                        let close_frame = Frame {
                            frame_type: FrameType::CloseConnection,
                            conn_id,
                            data: vec![0x01],
                        };
                        let _ = protocol::send_frame(&writer_tx, &key, &close_frame);
                        continue;
                    }
                };

                remote_stream.set_nodelay(true)?;
                info!(
                    "Connected to {} for connection {}",
                    remote_addr, conn_id
                );

                let (read_half, write_half) = tokio::io::split(remote_stream);
                {
                    let mut conns = connections.lock().await;
                    conns.insert(conn_id, write_half);
                }

                let tx = writer_tx.clone();
                let conns = connections.clone();
                let r_key = key;

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
                                if protocol::send_frame(&tx, &r_key, &frame).is_err() {
                                    break;
                                }
                            }
                            Err(_) => break,
                        }
                    }

                    {
                        let mut c = conns.lock().await;
                        c.remove(&conn_id);
                    }
                    let close_frame = Frame {
                        frame_type: FrameType::CloseConnection,
                        conn_id,
                        data: vec![],
                    };
                    let _ = protocol::send_frame(&tx, &r_key, &close_frame);
                });
            }
            FrameType::Data => {
                let conn_id = frame.conn_id;
                let mut conns = connections.lock().await;
                if let Some(write_half) = conns.get_mut(&conn_id) {
                    if let Err(e) = write_half.write_all(&frame.data).await {
                        warn!("Write to connection {} error: {}", conn_id, e);
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

    Ok(())
}
