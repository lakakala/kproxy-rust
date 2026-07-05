//! 反向 TCP 转发代理的客户端实现。
//!
//! 客户端会向服务端建立一条加密控制连接，注册配置中的转发规则，
//! 然后监听每个本地地址。每个被接受的本地 TCP 连接都会获得一个协议连接 ID，
//! 并复用到同一条控制连接上。

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use rand::Rng;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Mutex, mpsc, watch};
use tracing::{error, info, warn};

use crate::crypto;
use crate::protocol::{self, Frame, FrameType};
use crate::socks5;

/// 重连退避的初始等待时长。
const RECONNECT_MIN: Duration = Duration::from_secs(1);
/// 重连退避的最大等待时长（指数增长到此为止）。
const RECONNECT_MAX: Duration = Duration::from_secs(30);
/// 一次会话存活超过此时长即视为"稳定"，断开后把退避重置回 [`RECONNECT_MIN`]，
/// 避免长时间正常运行后偶发断线却仍从一个很大的退避值开始重连。
const STABLE_RESET: Duration = Duration::from_secs(60);

/// 活跃本地 TCP 流表，按协议连接 ID 索引。
///
/// 每条连接保存的是其独立写任务的入队 sender（见 [`protocol::spawn_conn_writer`]），
/// 而不是直接持有写半边。这样控制循环只入队、不阻塞，从而避免单条慢连接拖垮
/// 整条多路复用控制连接。
type ConnectionMap = Arc<Mutex<HashMap<u32, mpsc::Sender<Vec<u8>>>>>;

/// 客户端入口：构造共享 cipher 后按 `connection_mode` 分发到具体运行模式。
pub async fn run(
    config: &crate::config::ClientConfig,
    shutdown: watch::Receiver<bool>,
) -> anyhow::Result<()> {
    // 由 token 派生密钥后构造一个可复用的 cipher 实例，所有会话/连接共用，
    // 避免每次重连/每帧重建 cipher 的 CPU 开销。`Arc` 用于在各 spawn 任务间共享。
    let cipher = Arc::new(crypto::new_cipher(&crypto::derive_key(&config.token)));

    match config.connection_mode {
        crate::config::ConnectionMode::Multiplex => run_multiplex(config, &cipher, shutdown).await,
        crate::config::ConnectionMode::PerConnection => {
            run_per_connection(config, &cipher, shutdown).await
        }
    }
}

/// 多路复用模式：建立单条控制连接承载所有流，断开后自动用指数退避重连。
///
/// 单次会话的逻辑见 [`run_session`]。本函数只负责重连调度：进程不会因控制
/// 连接断开而退出，本地监听会随每次重连一并重建。
async fn run_multiplex(
    config: &crate::config::ClientConfig,
    cipher: &Arc<crypto::Cipher>,
    mut shutdown: watch::Receiver<bool>,
) -> anyhow::Result<()> {
    let mut backoff = RECONNECT_MIN;
    loop {
        let start = Instant::now();
        match run_session(config, cipher, shutdown.clone()).await {
            Ok(()) => info!("Disconnected from server; will reconnect"),
            Err(e) => warn!("Session ended: {}; will reconnect", e),
        }

        // 收到关闭信号则停止重连，干净退出。
        if *shutdown.borrow() {
            info!("Client shutting down");
            break;
        }

        // 曾稳定存活的会话断开后，从最小退避重新开始，而不是继承一个大退避值。
        if start.elapsed() >= STABLE_RESET {
            backoff = RECONNECT_MIN;
        }

        // 在 [backoff/2, backoff] 区间取一个带抖动的等待时长，避免多客户端同步重连。
        let delay = {
            let mut rng = rand::thread_rng();
            let max_ms = backoff.as_millis() as u64;
            let min_ms = max_ms / 2;
            Duration::from_millis(rng.gen_range(min_ms..=max_ms))
        };
        info!("Reconnecting in {:.1}s", delay.as_secs_f64());
        // 退避等待期间也要响应关闭信号，避免关闭时还要干等一整个退避周期。
        tokio::select! {
            _ = tokio::time::sleep(delay) => {}
            _ = shutdown.changed() => {}
        }
        if *shutdown.borrow() {
            info!("Client shutting down");
            break;
        }

        backoff = (backoff * 2).min(RECONNECT_MAX);
    }

    Ok(())
}

/// 运行一次完整会话：连接服务端、认证、注册转发规则、运行本地监听器，
/// 并在控制连接断开（或心跳超时）时返回，由 [`run`] 决定是否重连。
async fn run_session(
    config: &crate::config::ClientConfig,
    cipher: &Arc<crypto::Cipher>,
    mut shutdown: watch::Receiver<bool>,
) -> anyhow::Result<()> {
    // 取得 cipher 的拥有式句柄，供本会话内各 spawn 任务克隆共享。
    let cipher = cipher.clone();

    if let Some(socks5_config) = &config.socks5 {
        info!(
            "Connecting to server {} via SOCKS5 proxy {}",
            config.server_addr, socks5_config.addr
        );
    }
    let stream = connect_to_server(config).await?;
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
        protocol::write_frame(&mut *w, &cipher, &auth_frame).await?;
    }

    let auth_result = protocol::read_frame(&mut reader, &cipher).await?;
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
            protocol::write_frame(&mut *w, &cipher, &register_frame).await?;
        }

        let result_frame = protocol::read_frame(&mut reader, &cipher).await?;
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
        let l_cipher = cipher.clone();
        let l_forward_id = forward_id;
        let mut l_shutdown = shutdown.clone();

        tasks.spawn(async move {
            loop {
                let accept = tokio::select! {
                    a = listener.accept() => a,
                    // 关闭时停止接受新本地连接。
                    _ = l_shutdown.changed() => break,
                };
                match accept {
                    Ok((stream, addr)) => {
                        info!("New connection on forward {}: {}", l_forward_id, addr);

                        let conn_id = nid.fetch_add(1, Ordering::Relaxed);

                        let _ = stream.set_nodelay(true);
                        let (read_half, write_half) = tokio::io::split(stream);
                        {
                            // 先登记本地写队列，再通知服务端有新连接。
                            // 这样服务端返回的数据一定有写入目标。写出由独立任务
                            // 处理，控制循环只入队、不阻塞。
                            let data_tx = protocol::spawn_conn_writer(write_half);
                            let mut c = conns.lock().await;
                            c.insert(conn_id, data_tx);
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
                        if protocol::send_frame(&tx, &l_cipher, &frame).await.is_err() {
                            let mut c = conns.lock().await;
                            c.remove(&conn_id);
                            continue;
                        }

                        let r_tx = tx.clone();
                        let r_conns = conns.clone();
                        let r_cipher = l_cipher.clone();
                        let mut c_shutdown = l_shutdown.clone();

                        tokio::spawn(async move {
                            // 本地 TCP -> 加密控制连接。
                            // EOF、发送失败或收到关闭信号时关闭本地流，并用关闭帧通知服务端。
                            let mut reader = read_half;
                            let mut buf = vec![0u8; 32768];
                            loop {
                                tokio::select! {
                                    read = reader.read(&mut buf) => match read {
                                        Ok(0) => break,
                                        Ok(n) => {
                                            let frame = Frame {
                                                frame_type: FrameType::Data,
                                                conn_id,
                                                data: buf[..n].to_vec(),
                                            };
                                            if protocol::send_frame(&r_tx, &r_cipher, &frame)
                                                .await
                                                .is_err()
                                            {
                                                break;
                                            }
                                        }
                                        Err(_) => break,
                                    },
                                    _ = c_shutdown.changed() => break,
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
                            let _ = protocol::send_frame(&r_tx, &r_cipher, &close_frame).await;
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

    // 心跳发送任务：周期性发送 `Ping` 保持连接活跃（穿透 NAT/防火墙空闲超时），
    // 服务端会回 `Pong`，两端据此重置各自的读超时窗口。writer 队列关闭即退出。
    let hb_tx = writer_tx.clone();
    let hb_cipher = cipher.clone();
    let mut hb_shutdown = shutdown.clone();
    let heartbeat_handle = tokio::spawn(async move {
        let mut ticker = tokio::time::interval(protocol::HEARTBEAT_INTERVAL);
        // 首个 tick 立即返回，跳过以免连接刚建立就立刻发一个 Ping。
        ticker.tick().await;
        loop {
            tokio::select! {
                _ = ticker.tick() => {}
                // 关闭时退出，释放 writer_tx 克隆，便于 writer 任务尽快结束。
                _ = hb_shutdown.changed() => break,
            }
            let ping = Frame {
                frame_type: FrameType::Ping,
                conn_id: 0,
                data: vec![],
            };
            if protocol::send_frame(&hb_tx, &hb_cipher, &ping).await.is_err() {
                break;
            }
        }
    });

    loop {
        // 服务端 -> 客户端的主控制循环。收到的 `Data` 帧会写入本地 socket；
        // 关闭帧会移除本地连接状态。
        //
        // 读操作带超时：任意收到的帧（含 `Pong`）都会重置该窗口；连续多个心跳
        // 周期都收不到任何帧即判定连接已死，退出会话触发重连。
        let frame = tokio::select! {
            read = tokio::time::timeout(
                protocol::HEARTBEAT_TIMEOUT,
                protocol::read_frame(&mut reader, &cipher),
            ) => match read {
                Ok(Ok(f)) => f,
                Ok(Err(e)) => {
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
                Err(_) => {
                    warn!("Server heartbeat timeout; closing control connection");
                    break;
                }
            },
            _ = shutdown.changed() => {
                info!("Shutting down session; draining");
                break;
            }
        };

        match frame.frame_type {
            FrameType::Data => {
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
                // 删除需要保持幂等，因为本地 reader 任务观察到 EOF 时也可能删除
                // 同一条连接。
                info!("Connection {} closed by server", frame.conn_id);
                let mut conns = connections.lock().await;
                conns.remove(&frame.conn_id);
            }
            FrameType::Ping => {
                // 服务端也可能主动探活：回 Pong。
                let pong = Frame {
                    frame_type: FrameType::Pong,
                    conn_id: 0,
                    data: vec![],
                };
                let _ = protocol::send_frame(&writer_tx, &cipher, &pong).await;
            }
            FrameType::Pong => {
                // 收到即已重置读超时窗口，无需额外处理。
            }
            _ => {
                warn!("Unexpected frame type: 0x{:02x}", frame.frame_type as u8);
            }
        }
    }

    // 收尾。停止心跳与本地监听后，丢弃这里的 writer_tx。
    //
    // 关闭路径：各 conn reader 任务也会因 shutdown 而 break，先发出各自的
    // CloseConnection 并释放 writer_tx 克隆；待所有克隆释放，writer 任务把通道内
    // 积压帧（含这些 CloseConnection）全部写出再结束，即完成 flush，有界超时兜底。
    // 断线重连路径：写侧已不可用，无需等待，直接 abort 以保证重连低延迟。
    let shutting_down = *shutdown.borrow();
    heartbeat_handle.abort();
    tasks.abort_all();
    drop(writer_tx);
    if shutting_down {
        if tokio::time::timeout(protocol::DRAIN_TIMEOUT, writer_handle)
            .await
            .is_err()
        {
            warn!("Drain timeout; some buffered data may be lost");
        }
    } else {
        writer_handle.abort();
    }

    Ok(())
}

/// 连接服务端地址，必要时经上游 SOCKS5 代理，并置上 `TCP_NODELAY`。
///
/// 两种运行模式共用：多路复用模式每次会话调用一次，每次新建连接模式则
/// 每条被代理的流调用一次。
async fn connect_to_server(
    config: &crate::config::ClientConfig,
) -> anyhow::Result<TcpStream> {
    let stream = if let Some(socks5_config) = &config.socks5 {
        let (host, port) = parse_host_port(&config.server_addr)?;
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
    Ok(stream)
}

/// 每次新建连接模式：直接按配置绑定本地监听，每个到来的本地连接单独建立一条
/// 到服务端的 TCP 并双向代理。没有常驻控制连接，因此也没有心跳/注册/重连。
async fn run_per_connection(
    config: &crate::config::ClientConfig,
    cipher: &Arc<crypto::Cipher>,
    mut shutdown: watch::Receiver<bool>,
) -> anyhow::Result<()> {
    // 各 accept 循环任务是 'static 的，需要拥有式的配置；克隆一次后用 Arc 共享。
    let config = Arc::new(config.clone());
    let mut tasks = tokio::task::JoinSet::new();

    for forward in &config.forwards {
        let listener = TcpListener::bind(&forward.local_addr).await.map_err(|e| {
            anyhow::anyhow!("Failed to bind listener on {}: {}", forward.local_addr, e)
        })?;
        info!(
            "Listening on {} -> {} (per-connection)",
            forward.local_addr, forward.remote_addr
        );

        let remote_addr = forward.remote_addr.clone();
        let l_config = config.clone();
        let l_cipher = cipher.clone();
        let mut l_shutdown = shutdown.clone();

        tasks.spawn(async move {
            loop {
                let accept = tokio::select! {
                    a = listener.accept() => a,
                    // 关闭时停止接受新本地连接。
                    _ = l_shutdown.changed() => break,
                };
                match accept {
                    Ok((stream, addr)) => {
                        let _ = stream.set_nodelay(true);
                        info!("New connection -> {}: {}", remote_addr, addr);

                        let s_config = l_config.clone();
                        let s_cipher = l_cipher.clone();
                        let s_remote = remote_addr.clone();
                        let s_shutdown = l_shutdown.clone();
                        tokio::spawn(async move {
                            if let Err(e) =
                                handle_stream(stream, &s_config, &s_cipher, &s_remote, s_shutdown)
                                    .await
                            {
                                warn!("Stream to {} ended: {}", s_remote, e);
                            }
                        });
                    }
                    Err(e) => {
                        error!("Accept error on {}: {}", remote_addr, e);
                        break;
                    }
                }
            }
        });
    }

    // 主任务等待关闭信号；收到后中止所有 accept 循环。在途的 per-stream 任务是
    // 分离的，进程退出时随之结束。
    let _ = shutdown.changed().await;
    info!("Client shutting down");
    tasks.abort_all();
    Ok(())
}

/// 处理一条被代理的本地连接（每次新建连接模式）：新开一条到服务端的 TCP，
/// 认证并发送 `OpenStream` 告知目标远端，随后在本地与服务端之间双向代理。
async fn handle_stream(
    local_stream: TcpStream,
    config: &crate::config::ClientConfig,
    cipher: &Arc<crypto::Cipher>,
    remote_addr: &str,
    mut shutdown: watch::Receiver<bool>,
) -> anyhow::Result<()> {
    let server = connect_to_server(config).await?;
    let (mut srv_reader, mut srv_writer) = tokio::io::split(server);

    // 认证。
    let auth_frame = Frame {
        frame_type: FrameType::Auth,
        conn_id: 0,
        data: config.token.as_bytes().to_vec(),
    };
    protocol::write_frame(&mut srv_writer, cipher, &auth_frame).await?;

    let auth_result = protocol::read_frame(&mut srv_reader, cipher).await?;
    if !matches!(auth_result.frame_type, FrameType::AuthResult) {
        return Err(anyhow::anyhow!("Expected AuthResult frame"));
    }
    let result = String::from_utf8(auth_result.data)?;
    if result != "ok" {
        return Err(anyhow::anyhow!("Authentication failed: {}", result));
    }

    // 告知服务端本流的目标远端。不等待 ack：服务端若连远端失败会直接关闭连接，
    // 下游读到 EOF 自然结束。
    let open_frame = Frame {
        frame_type: FrameType::OpenStream,
        conn_id: 0,
        data: remote_addr.as_bytes().to_vec(),
    };
    protocol::write_frame(&mut srv_writer, cipher, &open_frame).await?;

    // 专用连接无需 conn_id 区分，固定用 1。
    const CONN_ID: u32 = 1;
    let (mut loc_reader, mut loc_writer) = tokio::io::split(local_stream);

    // 本地 -> 服务端：读到的字节封成 Data 帧写往服务端；本地 EOF/出错/关闭时，
    // 发 CloseConnection 并半关服务端写方向。
    let up_cipher = cipher.clone();
    let mut up_shutdown = shutdown.clone();
    let up = async move {
        let mut buf = vec![0u8; 32768];
        loop {
            tokio::select! {
                read = loc_reader.read(&mut buf) => match read {
                    Ok(0) => break,
                    Ok(n) => {
                        let frame = Frame {
                            frame_type: FrameType::Data,
                            conn_id: CONN_ID,
                            data: buf[..n].to_vec(),
                        };
                        if protocol::write_frame(&mut srv_writer, &up_cipher, &frame)
                            .await
                            .is_err()
                        {
                            break;
                        }
                    }
                    Err(_) => break,
                },
                _ = up_shutdown.changed() => break,
            }
        }
        let close_frame = Frame {
            frame_type: FrameType::CloseConnection,
            conn_id: CONN_ID,
            data: vec![],
        };
        let _ = protocol::write_frame(&mut srv_writer, &up_cipher, &close_frame).await;
        let _ = srv_writer.shutdown().await;
    };

    // 服务端 -> 本地：Data 帧写入本地 socket；CloseConnection/EOF/出错时结束。
    let down_cipher = cipher.clone();
    let down = async move {
        loop {
            tokio::select! {
                frame = protocol::read_frame(&mut srv_reader, &down_cipher) => match frame {
                    Ok(f) => match f.frame_type {
                        FrameType::Data => {
                            if loc_writer.write_all(&f.data).await.is_err() {
                                break;
                            }
                        }
                        FrameType::CloseConnection => break,
                        // 专用连接上不应出现其它帧类型，忽略即可。
                        _ => {}
                    },
                    Err(_) => break,
                },
                _ = shutdown.changed() => break,
            }
        }
        let _ = loc_writer.shutdown().await;
    };

    tokio::join!(up, down);
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
