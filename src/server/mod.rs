use crate::e;
use crate::message;
use std::cell::RefCell;
use std::collections::HashMap;
use std::error::Error;
use std::sync::Arc;
use tokio::io::AsyncRead;
use tokio::net::TcpStream;
use tokio::sync::mpsc;

struct Server {
    router: Arc<RefCell<Router>>,
}

impl Server {}

struct Channel {
    addr: String,
    port: u16,
    sock: TcpStream,
}

impl Channel {
    async fn new(addr: String, port: u16) -> Result<Channel, Box<dyn Error>> {
        let sock = TcpStream::connect(format!("{}:{}", addr, port)).await?;

        let (tx, rx) = mpsc::channel::<Vec<u8>>(50);
        Ok(Channel { addr, port, sock })
    }
}

struct ProxyReader {}

impl AsyncRead for ProxyReader {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        todo!()
    }
}

struct ProxyClient {
    sock: TcpStream,
    router: Arc<RefCell<Router>>,

    client_id: message::ClientIDType,
    channels: HashMap<message::ChannelIDType, Channel>,
}

impl ProxyClient {
    async fn new(sock: TcpStream, router: Arc<RefCell<Router>>) -> Self {
        ProxyClient {
            sock,
            router,

            client_id: 0,
            channels: HashMap::new(),
        }
    }

    async fn start(mut self: Self) {}

    async fn do_start(self: Self) -> Result<(), e::ParseError> {
        let auth_req = read_message::<message::AuthRequest>().await?;

        let client_id = auth_req.get_client_id();

        let self_ref = Arc::new(RefCell::new(self));

        let mut cli_ptr = self_ref.borrow_mut();

        cli_ptr.client_id = client_id;

        cli_ptr.router.borrow_mut().add(client_id, self_ref.clone());

        Ok(())
    }

    async fn handle_message(&mut self, cmd: message::CommandType) -> Result<(), e::ParseError> {
        // match cmd {
        //     message::CommandType::AllocChannelNotice => {
        //         self.handle_alloc_channel_notice(
        //             read_message::<message::AllocChannelNotice>().await?,
        //         )
        //         .await?;
        //     }
        //     _ => {}
        // }

        Ok(())
    }

    async fn handle_alloc_channel_notice(
        &mut self,
        notice: message::AllocChannelNotice,
    ) -> Result<(), Box<dyn Error>> {
        let channel = match notice.get_channel_type() {
            message::ChannelType::Tcp { addr, port } => Channel::new(addr, port).await?,
        };

        self.channels.insert(notice.get_client_id(), channel);

        Ok(())
    }
}

struct Client {}

async fn read_message<T>() -> Result<T, e::ParseError> {
    todo!()
}

struct Router {
    clients: HashMap<message::ClientIDType, Arc<RefCell<ProxyClient>>>,
}

impl Router {
    fn add(
        self: &mut Self,
        cli_id: message::ClientIDType,
        cli: Arc<RefCell<ProxyClient>>,
    ) -> Result<(), e::ParseError> {
        todo!()
    }
}
