//! 客户端和服务端共用的帧封装层。
//!
//! 每条逻辑消息的线上编码格式如下：
//!
//! ```text
//! 4 字节大端序加密长度
//! AES-GCM 加密后的负载
//! ```
//!
//! 加密负载解密后是协议内部的帧体：
//!
//! ```text
//! 1 字节帧类型
//! 4 字节大端序连接 ID
//! 帧类型对应的负载字节
//! ```
//!
//! `conn_id == 0` 保留给认证、转发注册、心跳（`Ping`/`Pong`）等控制面消息。
//! 被代理的 TCP 流使用非零连接 ID，这样数据帧和关闭帧就可以在同一条加密控制
//! 连接上完成多路复用和拆分。

use std::time::Duration;

use anyhow::Result;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::crypto::Cipher;

/// 客户端发送 `Ping` 心跳的周期。
///
/// 该值远小于 [`HEARTBEAT_TIMEOUT`]：空闲时心跳流量保持 NAT/防火墙映射存活，
/// 同时让任意一端能在数个心跳周期内发现对端已不可达。
pub const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(15);

/// 控制连接读超时。任意收到的帧都会重置该窗口。
///
/// 约为 [`HEARTBEAT_INTERVAL`] 的三倍：需要连续丢失约 3 个心跳才判定断线，
/// 因此短暂网络抖动不会触发误重连，而真正的死连接会在该时长内被发现。
pub const HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(45);

/// 优雅关闭时等待积压数据 flush / 对端 `CloseConnection` 通知写出的最长时间，
/// 超过该时长则放弃等待、强制退出，避免关闭过程被卡死的连接无限拖住。
pub const DRAIN_TIMEOUT: Duration = Duration::from_secs(5);

/// 多路复用控制连接使用的帧类型。
///
/// 这些数值是线上协议的一部分，修改它们会破坏已有客户端和服务端的兼容性。
#[derive(Debug, Clone, Copy)]
#[repr(u8)]
pub enum FrameType {
    /// 客户端 -> 服务端的认证帧，负载是配置的 token。
    Auth = 0x01,
    /// 服务端 -> 客户端的认证结果，负载是 `ok` 或错误信息。
    AuthResult = 0x02,
    /// 客户端 -> 服务端，请求注册一个反向转发目标。
    RegisterForward = 0x03,
    /// 服务端 -> 客户端的注册结果，负载以状态字节开头。
    RegisterForwardResult = 0x04,
    /// 客户端 -> 服务端，通知有新的本地 TCP 连接到达。
    NewConnection = 0x05,
    /// `conn_id` 对应的双向代理流数据。
    Data = 0x06,
    /// 双向关闭信号，表示某条代理流应被关闭。
    CloseConnection = 0x07,
    /// 控制连接心跳请求（`conn_id == 0`，无负载）。收到后对端应回 `Pong`。
    Ping = 0x08,
    /// 控制连接心跳应答（`conn_id == 0`，无负载）。
    Pong = 0x09,
    /// 客户端 -> 服务端，「每次新建连接」模式下单流数据连接认证成功后的第一帧，
    /// 负载为目标远端地址（UTF-8）。服务端据此把该连接识别为专用数据连接，
    /// 连接远端后在此连接上双向代理。多路复用客户端永不发送此帧。
    OpenStream = 0x0a,
}

impl FrameType {
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0x01 => Some(FrameType::Auth),
            0x02 => Some(FrameType::AuthResult),
            0x03 => Some(FrameType::RegisterForward),
            0x04 => Some(FrameType::RegisterForwardResult),
            0x05 => Some(FrameType::NewConnection),
            0x06 => Some(FrameType::Data),
            0x07 => Some(FrameType::CloseConnection),
            0x08 => Some(FrameType::Ping),
            0x09 => Some(FrameType::Pong),
            0x0a => Some(FrameType::OpenStream),
            _ => None,
        }
    }
}

pub struct Frame {
    /// 当前帧表示的操作类型。
    pub frame_type: FrameType,
    /// 被多路复用的代理连接 ID；控制面帧使用 0。
    pub conn_id: u32,
    /// 帧类型对应的负载。对 `Data` 帧来说，这里是原始 TCP 字节。
    pub data: Vec<u8>,
}

impl Frame {
    /// 编码加密前的明文帧体。
    ///
    /// 这里故意不处理长度前缀，因为线上写入的是加密后的长度。
    pub fn encode(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(1 + 4 + self.data.len());
        buf.push(self.frame_type as u8);
        buf.extend_from_slice(&self.conn_id.to_be_bytes());
        buf.extend_from_slice(&self.data);
        buf
    }

    /// 解码已经解密的帧体。
    ///
    /// 合法帧体至少要包含帧类型和连接 ID。剩余字节都作为负载处理，
    /// 负载结构由调用方根据具体帧类型继续校验。
    pub fn decode(data: &[u8]) -> Result<Self> {
        if data.len() < 5 {
            return Err(anyhow::anyhow!("Frame data too short"));
        }
        let frame_type = FrameType::from_u8(data[0])
            .ok_or_else(|| anyhow::anyhow!("Unknown frame type: 0x{:02x}", data[0]))?;
        let conn_id = u32::from_be_bytes([data[1], data[2], data[3], data[4]]);
        let payload = data[5..].to_vec();
        Ok(Frame {
            frame_type,
            conn_id,
            data: payload,
        })
    }
}

/// 加密一帧并直接写入流。
///
/// 这用于同步初始化阶段，此时专用 writer 任务和有界通道还没有启动。
pub async fn write_frame<W: AsyncWriteExt + Unpin>(
    writer: &mut W,
    cipher: &Cipher,
    frame: &Frame,
) -> Result<()> {
    let plaintext = frame.encode();
    let encrypted = crate::crypto::encrypt(cipher, &plaintext)?;

    let len = (encrypted.len() as u32).to_be_bytes();
    writer.write_all(&len).await?;
    writer.write_all(&encrypted).await?;
    writer.flush().await?;
    Ok(())
}

/// 从流中读取、解密并解码一帧。
///
/// 64 MiB 限制用于避免对端通过恶意长度前缀触发无界内存分配。
pub async fn read_frame<R: AsyncReadExt + Unpin>(reader: &mut R, cipher: &Cipher) -> Result<Frame> {
    let mut len_buf = [0u8; 4];
    reader.read_exact(&mut len_buf).await?;
    let len = u32::from_be_bytes(len_buf) as usize;

    if len > 1024 * 1024 * 64 {
        return Err(anyhow::anyhow!("Frame too large: {} bytes", len));
    }

    let mut encrypted = vec![0u8; len];
    reader.read_exact(&mut encrypted).await?;

    let plaintext = crate::crypto::decrypt(cipher, &encrypted)?;
    Frame::decode(&plaintext)
}

/// 编码并加密一帧，然后放入 writer 任务的发送队列。
///
/// 这里故意等待有界通道，而不是使用 `try_send`。当控制连接慢于生产者时，
/// 等待会形成背压，避免高负载下出现紧密的重试/关闭循环。
pub async fn send_frame(
    tx: &tokio::sync::mpsc::Sender<Vec<u8>>,
    cipher: &Cipher,
    frame: &Frame,
) -> Result<()> {
    let plaintext = frame.encode();
    let encrypted = crate::crypto::encrypt(cipher, &plaintext)?;
    let mut raw = Vec::with_capacity(4 + encrypted.len());
    raw.extend_from_slice(&(encrypted.len() as u32).to_be_bytes());
    raw.extend_from_slice(&encrypted);
    tx.send(raw)
        .await
        .map_err(|e| anyhow::anyhow!("Channel send error: {}", e))?;
    Ok(())
}

/// 每条被代理连接的写队列容量（以帧为单位）。
///
/// 控制循环只向该队列入队，永不直接 `await` 单条 socket 的写。容量满意味着
/// 这条连接的对端消费严重滞后；此时关闭该连接，而不是阻塞整条多路复用控制
/// 连接（否则会引发队头阻塞，乃至双向大流量下的互锁）。
pub const CONN_WRITE_QUEUE: usize = 256;

/// 为一条代理连接启动独立的写任务，返回用于入队待写数据的 sender。
///
/// 过去控制循环在收到 `Data` 帧时会内联 `write_all` 到目标 socket，一旦该
/// socket 因对端读得慢而背压，整条控制连接（连同复用其上的所有连接）都会
/// 卡死。这里把每条连接的写出隔离到独立任务：控制循环只负责入队，绝不因
/// 单条慢连接而停顿。
///
/// sender 被丢弃（连接从连接表移除）后，写任务会读到通道关闭并 `shutdown`
/// socket 的写方向，从而回收半开连接。
pub fn spawn_conn_writer(
    mut write_half: tokio::io::WriteHalf<tokio::net::TcpStream>,
) -> tokio::sync::mpsc::Sender<Vec<u8>> {
    let (tx, mut rx) = tokio::sync::mpsc::channel::<Vec<u8>>(CONN_WRITE_QUEUE);
    tokio::spawn(async move {
        while let Some(data) = rx.recv().await {
            if write_half.write_all(&data).await.is_err() {
                break;
            }
        }
        let _ = write_half.shutdown().await;
    });
    tx
}
