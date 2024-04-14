use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite};

use crate::message::{Message, MessageType};

pub trait MessageCodec: AsyncRead + AsyncWrite {
    async fn read_message_type(&mut self) -> Result<MessageType, Box<dyn std::error::Error>>
    where
        Self: Unpin,
    {
        let cmd = self.read_u16().await?;
        return MessageType::try_from(cmd);
    }

    async fn read_message_body<E>(&mut self) -> Result<E, Box<dyn std::error::Error>>
    where
        Self: Unpin,
    {
        todo!()
    }

    async fn read_message<E>(&mut self) -> Result<E, Box<dyn std::error::Error>>
    where
        Self: Unpin + Sized,
        E: Message,
    {
        let msg_type = self.read_message_type().await?;

        if msg_type != E::command() {}

        return E::decode(self).await;
    }

    async fn write_message<E>(&mut self, msg: &E) -> Result<(), Box<dyn std::error::Error>>
    where
        E: Message,
    {
        todo!()
    }
}

impl<R> MessageCodec for R where R: AsyncRead + AsyncWrite {}
