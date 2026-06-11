//! 反向 TCP 转发代理的服务端实现。
//!
//! 服务端接收来自客户端的一条加密控制连接。客户端会注册一条或多条转发规则；
//! 当客户端后续报告有本地连接进入时，服务端会连接配置中的远端目标，
//! 并把这条 TCP 流复用到控制连接上。

use std::collections::HashMap;
use std::sync::Arc;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Mutex, mpsc};
use tracing::{error, info, warn};

use crate::crypto;
use crate::protocol::{self, Frame, FrameType};

/// 活跃代理流表，按协议连接 ID 索引。
///
/// 每条连接保存的是其独立写任务的入队 sender（见 [`protocol::spawn_conn_writer`]），
/// 控制循环只入队、不直接 `await` 单条 socket 的写，从而避免单条慢连接拖垮整条
/// 多路复用控制连接。
type ConnectionMap = Arc<Mutex<HashMap<u32, mpsc::Sender<Vec<u8>>>>>;

/// 启动服务端监听，并为每个客户端控制连接启动处理任务。
pub async fn run(config: &crate::config::ServerConfig) -> anyhow::Result<()> {
    let listener = TcpListener::bind(&config.listen_addr).await?;
    info!("Server listening on {}", config.listen_addr);

    // 构造一次可复用的 cipher 实例，所有客户端控制连接共享，避免每帧重建。
    let cipher = Arc::new(crypto::new_cipher(&crypto::derive_key(&config.token)));
    let expected_token = config.token.clone();

    loop {
        // `accept().await` 会在没有新控制连接时挂起任务，因此空闲时不会自旋。
        let (stream, addr) = listener.accept().await?;
        info!("New connection from {}", addr);

        let cipher = cipher.clone();
        let token = expected_token.clone();

        tokio::spawn(async move {
            if let Err(e) = handle_client(stream, cipher, &token).await {
                error!("Client handler error: {}", e);
            }
        });
    }
}

async fn handle_client(
    stream: TcpStream,
    cipher: Arc<crypto::Cipher>,
    expected_token: &str,
) -> anyhow::Result<()> {
    stream.set_nodelay(true)?;
    let (mut reader, writer) = tokio::io::split(stream);
    let writer = Arc::new(Mutex::new(writer));

    // 第一帧必须完成客户端认证，认证通过前不接受任何转发状态。
    let frame = protocol::read_frame(&mut reader, &cipher).await?;
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
        protocol::write_frame(&mut *w, &cipher, &response).await?;
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
        protocol::write_frame(&mut *w, &cipher, &response).await?;
    }

    let (writer_tx, mut writer_rx) = mpsc::channel::<Vec<u8>>(4096);

    let writer_clone = writer.clone();
    let writer_handle = tokio::spawn(async move {
        // 单独的 writer 任务负责把所有加密帧串行写入控制连接。
        // 生产者通过 `writer_tx` 放入已经封装好的完整帧字节。
        while let Some(raw_frame) = writer_rx.recv().await {
            let mut w = writer_clone.lock().await;
            if let Err(e) = w.write_all(&raw_frame).await {
                error!("Control write error: {}", e);
                break;
            }
        }
    });

    let connections: ConnectionMap = Arc::new(Mutex::new(HashMap::new()));
    let mut forward_map: HashMap<u32, String> = HashMap::new();
    let mut forward_id_counter: u32 = 0;

    loop {
        // 主控制面循环。这里收到的帧可能修改转发元数据、创建/关闭代理流，
        // 或携带代理流数据。
        let frame = match protocol::read_frame(&mut reader, &cipher).await {
            Ok(f) => f,
            Err(e) => {
                let msg = e.to_string();
                if msg.contains("unexpected eof") || msg.contains("EOF") || msg.contains("reset") {
                    info!("Client disconnected");
                } else {
                    error!("Read frame error: {}", e);
                }
                break;
            }
        };

        match frame.frame_type {
            FrameType::RegisterForward => {
                // 客户端只发送服务端侧目标地址。服务端分配一个紧凑的数字 ID，
                // 后续 `NewConnection` 帧会用这个 ID 标识转发规则。
                let remote_addr = String::from_utf8(frame.data)?;
                forward_id_counter += 1;
                let forward_id = forward_id_counter;
                forward_map.insert(forward_id, remote_addr.clone());

                info!("Registered forward {}: -> {}", forward_id, remote_addr);

                let mut data = vec![0x00];
                data.extend_from_slice(&forward_id.to_be_bytes());
                let response = Frame {
                    frame_type: FrameType::RegisterForwardResult,
                    conn_id: 0,
                    data,
                };
                let _ = protocol::send_frame(&writer_tx, &cipher, &response).await;
            }
            FrameType::NewConnection => {
                // 负载布局：状态/保留字节，后跟注册阶段分配的转发 ID（大端序）。
                if frame.data.len() < 5 {
                    warn!("Invalid NewConnection frame");
                    let close_frame = Frame {
                        frame_type: FrameType::CloseConnection,
                        conn_id: frame.conn_id,
                        data: vec![0x01],
                    };
                    let _ = protocol::send_frame(&writer_tx, &cipher, &close_frame).await;
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
                        let _ = protocol::send_frame(&writer_tx, &cipher, &close_frame).await;
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
                        let _ = protocol::send_frame(&writer_tx, &cipher, &close_frame).await;
                        continue;
                    }
                };

                remote_stream.set_nodelay(true)?;
                info!("Connected to {} for connection {}", remote_addr, conn_id);

                // 登记写队列，这样后续来自客户端的 `Data` 帧可以入队写入这条
                // 服务端侧 TCP 连接。写出由独立任务处理，控制循环只入队、不阻塞。
                let (read_half, write_half) = tokio::io::split(remote_stream);
                {
                    let data_tx = protocol::spawn_conn_writer(write_half);
                    let mut conns = connections.lock().await;
                    conns.insert(conn_id, data_tx);
                }

                let tx = writer_tx.clone();
                let conns = connections.clone();
                let r_cipher = cipher.clone();

                tokio::spawn(async move {
                    // 服务端侧 TCP -> 加密控制连接。
                    // 当远端目标关闭，或控制连接 writer 不可用时，清理连接并通知客户端。
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
                                if protocol::send_frame(&tx, &r_cipher, &frame).await.is_err() {
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
                    info!("Connection {} closed (remote read ended)", conn_id);
                    let close_frame = Frame {
                        frame_type: FrameType::CloseConnection,
                        conn_id,
                        data: vec![],
                    };
                    let _ = protocol::send_frame(&tx, &r_cipher, &close_frame).await;
                });
            }
            FrameType::Data => {
                // 加密控制连接 -> 服务端侧 TCP。
                // 持有连接表锁时只克隆单连接的写队列 sender，随后释放锁。
                // 入队用 `try_send`：控制循环绝不因某条连接而阻塞。
                let conn_id = frame.conn_id;
                let sender = {
                    let conns = connections.lock().await;
                    conns.get(&conn_id).cloned()
                };
                if let Some(sender) = sender {
                    // 入队成功即继续读下一帧。队列满（对端消费过慢）或已关闭时，
                    // 关闭这一条连接，而不是拖垮整条多路复用控制连接。
                    if sender.try_send(frame.data).is_ok() {
                        continue;
                    }

                    {
                        let mut conns = connections.lock().await;
                        conns.remove(&conn_id);
                    }
                    info!(
                        "Connection {} closed (slow consumer / write queue full)",
                        conn_id
                    );
                    let close_frame = Frame {
                        frame_type: FrameType::CloseConnection,
                        conn_id,
                        data: vec![],
                    };
                    let _ = protocol::send_frame(&writer_tx, &cipher, &close_frame).await;
                }
            }
            FrameType::CloseConnection => {
                // 丢弃保存的写半边会关闭服务端侧 TCP 流的输出方向。reader 任务
                // 观察到 EOF 时也会删除同一个 key，因此删除操作需要保持幂等。
                info!("Connection {} closed by client", frame.conn_id);
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
