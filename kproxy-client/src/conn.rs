use log::{error, info};
use std::sync::{Arc, Mutex};
use std::{collections::HashMap, vec};
use tokio::sync;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
};

type CallbackFn = fn(cmd: Command);

struct Message {
    cmd: Command,
    trigger: sync::oneshot::Sender<Result<Command, Box<dyn std::error::Error>>>,
}

struct Conn {
    stream: TcpStream,
    sender: sync::mpsc::Sender<Message>,
    receiver: sync::mpsc::Receiver<Message>,

    msgs: Mutex<HashMap<u32, Message>>,

    callbacks: Mutex<HashMap<u16, CallbackFn>>,
}

impl Conn {
    // fn new(stream: TcpStream) -> Conn {
    //     let (sender, receiver) = sync::mpsc::channel::<Command>(20);

    //     Conn { stream }
    // }

    pub async fn send(&mut self, cmd: Command) -> Result<Command, Box<dyn std::error::Error>> {
        let (sender, receiver) =
            sync::oneshot::channel::<Result<Command, Box<dyn std::error::Error>>>();
        let send = self
            .sender
            .send(Message {
                cmd: cmd,
                trigger: sender,
            })
            .await;

        return receiver.await?;
    }

    pub fn listen<F>(&mut self, cmd: u16, cb: F) -> Result<(), Box<dyn std::error::Error>>
    where
        F: Fn(),
    {
        Ok(())
    }

    async fn do_write(
        &mut self,
        mut rx: sync::mpsc::Receiver<Message>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        loop {
            if let Some(msg) = rx.recv().await {
                info!("conn rec");
                {
                    let mut msgs = self.msgs.lock().unwrap();

                    if msgs.contains_key(&msg.cmd.tx_id) {
                        // 异常
                        error!("conn tx_id {} aleard exist", msg.cmd.tx_id);
                    } else {
                        self.stream.write_command(&msg.cmd).await?;
                        msgs.insert(msg.cmd.tx_id, msg);
                    }
                }
            } else {
                break;
            }
        }

        Ok(())
    }

    async fn do_read(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        loop {
            let cmd = self.stream.read_command().await?;

            match cmd.cmd_type {
                CommandType::Request => {
                    let mut callbacks = self.callbacks.lock().unwrap();

                    if let Some(callback) = callbacks.get(&cmd.cmd) {
                        callback(cmd);
                    } else {
                    }
                }
                CommandType::Response => {
                    let mut msgs = self.msgs.lock().unwrap();

                    if let Some(msg) = msgs.remove(&cmd.tx_id) {
                        msg.trigger.send(Ok(cmd));
                    } else {
                        error!("conn receive cmd tx_id {} not exist", cmd.tx_id);
                    }
                }
            };
        }
        todo!()
    }
}

enum CommandType {
    Request,
    Response,
}

struct Command {
    cmd_type: CommandType,
    cmd: u16,
    tx_id: u32,
    len: u32,
    data: Vec<u8>,
}

trait CommandCodec: AsyncReadExt + AsyncWriteExt {
    async fn read_command(&mut self) -> Result<Command, Box<dyn std::error::Error>>
    where
        Self: Unpin,
    {
        let cmd = self.read_u16().await?;
        let tx_id = self.read_u32().await?;
        let len = self.read_u32().await?;

        let mut data = vec![0; 3];
        self.read_exact(&mut data).await?;

        return Ok(Command {
            cmd_type: CommandType::Request,
            cmd,
            tx_id,
            len,
            data,
        });
    }

    async fn write_command(&mut self, cmd: &Command) -> Result<(), Box<dyn std::error::Error>>
    where
        Self: Unpin,
    {
        self.write_u16(cmd.cmd).await?;
        self.write_u32(cmd.tx_id).await?;
        self.write_u32(cmd.len).await?;
        self.write_all(&cmd.data).await?;

        Ok(())
    }
}

impl<R: AsyncReadExt + AsyncWriteExt + ?Sized> CommandCodec for R {}

trait MessageCodec<T> {
    async fn decode<R>(reader: &mut R) -> Result<T, Box<dyn std::error::Error>>
    where
        R: tokio::io::AsyncRead + Unpin;

    async fn encode<R>(writer: &mut R, req: &T) -> Result<(), Box<dyn std::error::Error>>
    where
        R: tokio::io::AsyncWrite + Unpin;
}

struct DefaultCodec {

}

struct AuthRequest{}
struct AuthResponse{}

impl MessageCodec<AuthRequest> for DefaultCodec {
    async fn decode<R>(reader: &mut R) -> Result<AuthRequest, Box<dyn std::error::Error>>
    where
        R: tokio::io::AsyncRead + Unpin {
        todo!()
    }

    async fn encode<R>(writer: &mut R, req: &AuthRequest) -> Result<(), Box<dyn std::error::Error>>
    where
        R: tokio::io::AsyncWrite + Unpin {
        todo!()
    }
}

struct Con {
    stream: TcpStream,
}

impl Con {
    pub async fn send<T, R>(&mut self, req: &T) -> Result<R, Box<dyn std::error::Error>> {
        let cmd = self.stream.read_command().await?;

        DefaultCodec::encode(&mut self.stream, req).await?;

        todo!()
    }
}
