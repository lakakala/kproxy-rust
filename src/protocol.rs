use anyhow::Result;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[derive(Debug, Clone, Copy)]
#[repr(u8)]
pub enum FrameType {
    Auth = 0x01,
    AuthResult = 0x02,
    RegisterForward = 0x03,
    RegisterForwardResult = 0x04,
    NewConnection = 0x05,
    Data = 0x06,
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
    pub frame_type: FrameType,
    pub conn_id: u32,
    pub data: Vec<u8>,
}

impl Frame {
    pub fn encode(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(1 + 4 + self.data.len());
        buf.push(self.frame_type as u8);
        buf.extend_from_slice(&self.conn_id.to_be_bytes());
        buf.extend_from_slice(&self.data);
        buf
    }

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

pub fn send_frame(
    tx: &tokio::sync::mpsc::Sender<Vec<u8>>,
    key: &[u8; 32],
    frame: &Frame,
) -> Result<()> {
    let plaintext = frame.encode();
    let encrypted = crate::crypto::encrypt(key, &plaintext)?;
    let mut raw = Vec::with_capacity(4 + encrypted.len());
    raw.extend_from_slice(&(encrypted.len() as u32).to_be_bytes());
    raw.extend_from_slice(&encrypted);
    tx.try_send(raw)
        .map_err(|e| anyhow::anyhow!("Channel send error: {}", e))?;
    Ok(())
}
