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
//! `conn_id == 0` 保留给认证、转发注册等控制面消息。被代理的 TCP
//! 流使用非零连接 ID，这样数据帧和关闭帧就可以在同一条加密控制连接上
//! 完成多路复用和拆分。

use anyhow::Result;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

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
    key: &[u8; 32],
    frame: &Frame,
) -> Result<()> {
    let plaintext = frame.encode();
    let encrypted = crate::crypto::encrypt(key, &plaintext)?;

    let len = (encrypted.len() as u32).to_be_bytes();
    writer.write_all(&len).await?;
    writer.write_all(&encrypted).await?;
    writer.flush().await?;
    Ok(())
}

/// 从流中读取、解密并解码一帧。
///
/// 64 MiB 限制用于避免对端通过恶意长度前缀触发无界内存分配。
pub async fn read_frame<R: AsyncReadExt + Unpin>(reader: &mut R, key: &[u8; 32]) -> Result<Frame> {
    let mut len_buf = [0u8; 4];
    reader.read_exact(&mut len_buf).await?;
    let len = u32::from_be_bytes(len_buf) as usize;

    if len > 1024 * 1024 * 64 {
        return Err(anyhow::anyhow!("Frame too large: {} bytes", len));
    }

    let mut encrypted = vec![0u8; len];
    reader.read_exact(&mut encrypted).await?;

    let plaintext = crate::crypto::decrypt(key, &encrypted)?;
    Frame::decode(&plaintext)
}

/// 编码并加密一帧，然后放入 writer 任务的发送队列。
///
/// 这里故意等待有界通道，而不是使用 `try_send`。当控制连接慢于生产者时，
/// 等待会形成背压，避免高负载下出现紧密的重试/关闭循环。
pub async fn send_frame(
    tx: &tokio::sync::mpsc::Sender<Vec<u8>>,
    key: &[u8; 32],
    frame: &Frame,
) -> Result<()> {
    let plaintext = frame.encode();
    let encrypted = crate::crypto::encrypt(key, &plaintext)?;
    let mut raw = Vec::with_capacity(4 + encrypted.len());
    raw.extend_from_slice(&(encrypted.len() as u32).to_be_bytes());
    raw.extend_from_slice(&encrypted);
    tx.send(raw)
        .await
        .map_err(|e| anyhow::anyhow!("Channel send error: {}", e))?;
    Ok(())
}
