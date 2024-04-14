use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

#[derive(PartialEq)]
pub enum MessageType {
    AuthRequest,
    AuthResponse,
    AllocTunnelRequest,
    AllocTunnelResponse,
}

impl TryFrom<u16> for MessageType {
    type Error = Box<dyn std::error::Error>;

    fn try_from(value: u16) -> Result<Self, Self::Error> {
        todo!()
    }
}

pub trait Message: Sized {
    fn command() -> MessageType;

    async fn decode<T>(reader: &mut T) -> Result<Self, Box<dyn std::error::Error>>
    where
        T: tokio::io::AsyncRead + Unpin;

    async fn encode<T>(&self, writer: &mut T) -> Result<(), Box<dyn std::error::Error>>
    where
        T: tokio::io::AsyncWrite + Unpin;
}

pub async fn read_message<T, R>(reader: &mut T) -> Result<R, Box<dyn std::error::Error>>
where
    T: AsyncRead + Unpin,
    R: Message,
{
    R::decode(reader).await
}

pub async fn write_message<T, R>(
    writer: &mut T,
    message: R,
) -> Result<(), Box<dyn std::error::Error>>
where
    T: AsyncWrite + Unpin,
    R: Message,
{
    message.encode(writer).await
}

pub struct AuthRequest {
    device_token: String,
}

impl AuthRequest {
    pub fn new(device_token: String) -> AuthRequest {
        AuthRequest { device_token }
    }
}

impl Message for AuthRequest {
    async fn decode<T>(reader: &mut T) -> Result<Self, Box<dyn std::error::Error>>
    where
        T: AsyncRead + Unpin,
    {
        let device_token = read_str(reader).await?;

        Ok(AuthRequest { device_token })
    }

    async fn encode<T>(&self, writer: &mut T) -> Result<(), Box<dyn std::error::Error>>
    where
        T: tokio::io::AsyncWrite + Unpin,
    {
        write_str(writer, self.device_token.clone()).await?;

        Ok(())
    }

    fn command() -> MessageType {
        todo!()
    }
}

pub struct AuthResponse {
    client_id: u64,
    device_id: u64,
}

impl AuthResponse {
    pub fn new(client_id: u64, device_id: u64) -> AuthResponse {
        AuthResponse {
            client_id,
            device_id,
        }
    }

    pub fn get_device_id(&self) -> u64 {
        return self.device_id;
    }
}
impl Message for AuthResponse {
    async fn decode<T>(reader: &mut T) -> Result<Self, Box<dyn std::error::Error>>
    where
        T: tokio::io::AsyncRead + Unpin,
    {
        let client_id = reader.read_u64().await?;
        let device_id = reader.read_u64().await?;

        Ok(AuthResponse {
            client_id,
            device_id,
        })
    }

    async fn encode<T>(&self, writer: &mut T) -> Result<(), Box<dyn std::error::Error>>
    where
        T: tokio::io::AsyncWrite + Unpin,
    {
        writer.write_u64(self.client_id).await?;
        writer.write_u64(self.device_id).await?;

        Ok(())
    }

    fn command() -> MessageType {
        todo!()
    }
}

pub struct AllocTunnelRequest {
    tunnel_id: u64,
    addr: String,
    port: u16,
}

pub struct AllocTunnelResponse {}

async fn read_str<T>(reader: &mut T) -> Result<String, Box<dyn std::error::Error>>
where
    T: AsyncRead + Unpin,
{
    let len = reader.read_u16().await?;

    let mut data = vec![0; len as usize];
    reader.read_exact(&mut data).await?;
    match String::from_utf8(data) {
        Ok(str) => Ok(str),
        Err(utf8_err) => Err(Box::new(utf8_err)),
    }
}

async fn write_str<T>(writer: &mut T, str: String) -> Result<(), Box<dyn std::error::Error>>
where
    T: AsyncWrite + Unpin,
{
    writer.write_u16(str.len() as u16).await?;

    writer.write_all(str.as_bytes()).await?;

    Ok(())
}
