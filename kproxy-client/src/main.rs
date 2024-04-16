use kproxy_core::{conn::MessageCodec, message};
use std::collections::HashMap;
use std::rc::Rc;
fn main() {
    info!("hello");
    println!("Hello, world!");
}
use log::info;
use tokio::{
    io::{AsyncRead, AsyncWrite},
    net::TcpStream,
};

#[derive(Debug)]
struct ClientOpt {
    server_addr: String,
    device_token: String,
}

struct Client {
    opt: Rc<ClientOpt>,
    device_id: u64,
    sock: TcpStream,

    tunnels: HashMap<u64, Tunnel>,
}

impl Client {
    pub async fn new(opt: Rc<ClientOpt>) -> Result<Client, Box<dyn std::error::Error>> {
        let mut sock = TcpStream::connect(opt.server_addr.clone()).await?;

        let device_id = auth(&mut sock, opt.device_token.clone()).await?;

        let cli = Client {
            opt: opt,
            device_id,
            sock,
            tunnels: HashMap::new(),
        };

        return Ok(cli);
    }

    async fn start(&mut self) {}

    async fn do_start(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        let cmd = self.sock.read_message_type().await?;

        match cmd {
            message::MessageType::AllocTunnelRequest => {
                let alloc_tunnel_req = self
                    .sock
                    .read_message_body::<message::AllocTunnelRequest>()
                    .await?;
            }
            _ => {}
        };
        Ok(())
    }

    async fn handle_alloc_tunnel(
        &mut self,
        tunnel_id: u64,
        addr: String,
        port: u16,
    ) -> Result<(), Box<dyn std::error::Error>> {
        if let Some(_) = self.tunnels.get(&tunnel_id) {}

        Ok(())
    }
}

trait Tunnel {
    
}

struct InPointTunnel {
    tunnal_id: u64,
    addr: String,
    port: u16,
    conns: HashMap<u64, TunnelConection>,
}

struct TunnelConection {
    conn_id: u64,
    local_stream: TcpStream,
    remote_stream: TcpStream,
}

async fn auth<T>(stream: &mut T, device_token: String) -> Result<u64, Box<dyn std::error::Error>>
where
    T: AsyncRead + AsyncWrite + Unpin,
{
    stream
        .write_message(&message::AuthRequest::new(device_token))
        .await?;

    let auth_resp = stream.read_message::<message::AuthResponse>().await?;
    return Ok(auth_resp.get_device_id());
}
