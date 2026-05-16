//! 反向 TCP 转发代理的客户端实现。
//!
//! 客户端会向服务端建立一条加密控制连接，注册配置中的转发规则，
//! 然后监听每个本地地址。每个被接受的本地 TCP 连接都会获得一个协议连接 ID，
//! 并复用到同一条控制连接上。

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Mutex, mpsc};
use tracing::{error, info, warn};

use crate::crypto;
use crate::protocol::{self, Frame, FrameType};
use crate::socks5;

/// 每条本地连接独立保护的写半边。
type SharedWriteHalf = Arc<Mutex<tokio::io::WriteHalf<TcpStream>>>;

/// 活跃本地 TCP 流表，按协议连接 ID 索引。
type ConnectionMap = Arc<Mutex<HashMap<u32, SharedWriteHalf>>>;

/// 连接服务端、注册转发规则，并运行本地监听器。
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

    // 认证在后台 writer 启动前完成，这样初始化错误可以同步返回给调用方。
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
        // 通过一个 writer 任务串行发送所有出站加密帧。当服务端或网络慢于
        // 本地生产者时，有界通道会形成背压。
        while let Some(raw_frame) = writer_rx.recv().await {
            let mut w = writer_clone.lock().await;
            if let Err(e) = w.write_all(&raw_frame).await {
                error!("Control write error: {}", e);
                break;
            }
        }
    });

    let mut forward_map: HashMap<u32, (String, u32)> = HashMap::new();

    for forward in &config.forwards {
        // 请求服务端注册服务端侧目标地址。本地监听地址只保存在客户端本地状态中。
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
            // 元组的第二个字段当前是未使用的保留状态。这个结构只限制在本模块内，
            // 以后扩展时不需要改变线上协议。
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

    let connections: ConnectionMap = Arc::new(Mutex::new(HashMap::new()));
    let next_conn_id: Arc<AtomicU32> = Arc::new(AtomicU32::new(1));

    let mut tasks = tokio::task::JoinSet::new();

    for (&forward_id, (local_addr, _)) in &forward_map {
        // 每条转发规则都有独立的本地监听任务。空闲时任务会挂起在
        // `accept().await` 上。
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
                        info!("New connection on forward {}: {}", l_forward_id, addr);

                        let conn_id = nid.fetch_add(1, Ordering::Relaxed);

                        let _ = stream.set_nodelay(true);
                        let (read_half, write_half) = tokio::io::split(stream);
                        {
                            // 先保存本地写半边，再通知服务端有新连接。
                            // 这样服务端返回的数据一定有写入目标。
                            let mut c = conns.lock().await;
                            c.insert(conn_id, Arc::new(Mutex::new(write_half)));
                        }

                        // 负载布局与服务端解析逻辑一致：
                        // 状态/保留字节，后跟转发 ID。
                        let mut data = vec![0x00];
                        data.extend_from_slice(&l_forward_id.to_be_bytes());
                        let frame = Frame {
                            frame_type: FrameType::NewConnection,
                            conn_id,
                            data,
                        };
                        if protocol::send_frame(&tx, &l_key, &frame).await.is_err() {
                            let mut c = conns.lock().await;
                            c.remove(&conn_id);
                            continue;
                        }

                        let r_tx = tx.clone();
                        let r_conns = conns.clone();
                        let r_key = l_key;

                        tokio::spawn(async move {
                            // 本地 TCP -> 加密控制连接。
                            // EOF 或发送失败时关闭本地流，并用关闭帧通知服务端。
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
                                        if protocol::send_frame(&r_tx, &r_key, &frame)
                                            .await
                                            .is_err()
                                        {
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
                            info!("Connection {} closed (local read ended)", conn_id);
                            let close_frame = Frame {
                                frame_type: FrameType::CloseConnection,
                                conn_id,
                                data: vec![],
                            };
                            let _ = protocol::send_frame(&r_tx, &r_key, &close_frame).await;
                        });
                    }
                    Err(e) => {
                        error!("Accept error on forward {}: {}", l_forward_id, e);
                        break;
                    }
                }
            }
        });
    }

    loop {
        // 服务端 -> 客户端的主控制循环。收到的 `Data` 帧会写入本地 socket；
        // 关闭帧会移除本地连接状态。
        let frame = match protocol::read_frame(&mut reader, &key).await {
            Ok(f) => f,
            Err(e) => {
                let msg = e.to_string();
                if msg.contains("unexpected eof") || msg.contains("EOF") || msg.contains("reset") {
                    info!("Disconnected from server");
                } else {
                    error!("Read frame error: {}", e);
                }
                break;
            }
        };

        match frame.frame_type {
            FrameType::Data => {
                // 持有连接表锁时只克隆单连接 writer，随后释放连接表锁再等待
                // socket 写入。
                let conn_id = frame.conn_id;
                let write_half = {
                    let conns = connections.lock().await;
                    conns.get(&conn_id).cloned()
                };
                if let Some(write_half) = write_half {
                    let mut write_half = write_half.lock().await;
                    let Err(e) = write_half.write_all(&frame.data).await else {
                        continue;
                    };

                    warn!("Write to local connection {} error: {}", conn_id, e);
                    {
                        let mut conns = connections.lock().await;
                        conns.remove(&conn_id);
                    }
                    info!("Connection {} closed (write error)", conn_id);
                    let close_frame = Frame {
                        frame_type: FrameType::CloseConnection,
                        conn_id,
                        data: vec![],
                    };
                    let _ = protocol::send_frame(&writer_tx, &key, &close_frame).await;
                }
            }
            FrameType::CloseConnection => {
                // 删除需要保持幂等，因为本地 reader 任务观察到 EOF 时也可能删除
                // 同一条连接。
                info!("Connection {} closed by server", frame.conn_id);
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

/// 为 SOCKS5 CONNECT 拆分 `host:port` 和 `[ipv6]:port` 形式的服务端地址。
fn parse_host_port(addr: &str) -> anyhow::Result<(String, u16)> {
    let (host, port_str) = if addr.starts_with('[') {
        let close_bracket = addr
            .find(']')
            .ok_or_else(|| anyhow::anyhow!("Invalid IPv6 address: {}", addr))?;
        let host = addr[1..close_bracket].to_string();
        let rest = &addr[close_bracket + 1..];
        if let Some(port) = rest.strip_prefix(':') {
            (host, port)
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
