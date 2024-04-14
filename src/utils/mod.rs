use std::borrow::BorrowMut;
use std::error::Error;
use std::fmt;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use tokio::net::TcpSocket;

async fn write_string<T: AsyncWrite + Unpin>(
    writer: &mut T,
    str: &String,
) -> Result<(), Box<dyn Error>> {
    let size = str.len();

    writer.write_u16(size as u16).await?;

    writer.write_all(str.as_bytes()).await?;
    Ok(())
}

struct BaseResp {
    code: i32,
}

struct AuthRequest {
    client_id: u64,
}

struct AuthRequestMessage();
trait Codec {
    async fn decode<T>(reader: &mut T) -> Result<Box<dyn Message>, Box<dyn Error>>
    where
        T: AsyncRead + Unpin;
}

struct AuthResponse {
    base_resp: BaseResp,
}

enum ChannelType {
    Tcp { addr: String, port: u16 },
}

struct AllocChannelRequest {
    channel_id: u64,
    channel_type: ChannelType,
}

struct AllocChannelResponse {
    base_resp: BaseResp,
}

const TCP_CHANNEL_TYPE: u8 = 1;

pub trait AsyncCustomReadExt: AsyncRead {
    async fn read_auth_request(&mut self) -> Result<AuthRequest, Box<dyn Error>>
    where
        Self: Unpin,
    {
        let client_id = self.read_u64().await?;

        Ok(AuthRequest {
            client_id: client_id,
        })
    }

    async fn read_alloc_channel_request(&mut self) -> Result<AllocChannelRequest, Box<dyn Error>>
    where
        Self: Unpin + Sized,
    {
        let channel_id = self.read_u64().await?;
        let raw_channel_type = self.read_u8().await?;

        let channel_type = match raw_channel_type {
            TCP_CHANNEL_TYPE => {
                let addr = read_string(self).await?;
                let port = self.read_u16().await?;

                Ok(ChannelType::Tcp {
                    addr: addr,
                    port: port,
                })
            }
            _ => Err(ParseError::new()),
        }?;

        Ok(AllocChannelRequest {
            channel_id: channel_id,
            channel_type: channel_type,
        })
    }
}

async fn read_string<T>(reader: &mut T) -> Result<String, Box<dyn Error>>
where
    T: AsyncRead + Unpin,
{
    let size = reader.read_u16().await?;

    let mut buf: Vec<u8> = vec![0, 3];

    reader.read_exact(&mut buf).await?;
    let str = String::from_utf8(buf)?;

    return Ok(str);
}

async fn read_base_resp<T: AsyncRead + Unpin>(reader: &mut T) -> Result<BaseResp, Box<dyn Error>> {
    let code = reader.read_i32().await?;

    Ok(BaseResp { code: code })
}

pub trait AsyncCustomWriteExt: AsyncWrite {
    async fn write_auth_request(&mut self, req: AuthRequest) -> Result<(), Box<dyn Error>> {
        Ok(())
    }
}


const COMMAND_AUTH_REQUEST: u16 = 1;

trait Message {}

trait Command {
    type Item;

    fn command() -> u16;

    async fn decode<T>(reader: &mut T) -> Result<Self::Item, Box<dyn Error>>
    where
        T: AsyncRead + Unpin;

    async fn encode<T>(writer: &mut T, command: &Self::Item) -> Result<(), Box<dyn Error>>
    where
        T: AsyncWrite + Unpin;
}

struct AuthRequestCommand();

impl Command for AuthRequestCommand {
    type Item = AuthRequest;

    fn command() -> u16 {
        COMMAND_AUTH_REQUEST
    }

    async fn decode<T>(reader: &mut T) -> Result<Self::Item, Box<dyn Error>>
    where
        T: AsyncRead + Unpin,
    {
        let client_id = reader.read_u64().await?;

        Ok(AuthRequest {
            client_id: client_id,
        })
    }

    async fn encode<T>(writer: &mut T, command: &Self::Item) -> Result<(), Box<dyn Error>>
    where
        T: AsyncWrite + Unpin,
    {
        writer.write_u64(command.client_id).await?;
        Ok(())
    }
}

struct AuthResponseCommand();

impl Command for AuthResponseCommand {
    type Item = AuthResponse;

    fn command() -> u16 {
        todo!()
    }

    async fn decode<T>(reader: &mut T) -> Result<Self::Item, Box<dyn Error>>
    where
        T: AsyncRead + Unpin,
    {
        todo!()
    }

    async fn encode<T>(writer: &mut T, command: &Self::Item) -> Result<(), Box<dyn Error>>
    where
        T: AsyncWrite + Unpin,
    {
        todo!()
    }
}

use std::collections::HashMap;

use crate::e::ParseError;

struct Server {
    commands: HashMap<u16, Box<dyn Message>>,
}

impl Server {
    fn new() -> Server {
        Server {
            commands: HashMap::new(),
        }
    }
}

