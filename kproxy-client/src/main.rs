use kproxy_core::{conn::MessageCodec, message};
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
    opt: ClientOpt,
    device_id: u64,
    sock: TcpStream,
}

impl Client {
    pub async fn new(opt: ClientOpt) -> Result<Client, Box<dyn std::error::Error>> {
        let mut sock = TcpStream::connect(opt.server_addr.clone()).await?;

        let device_id = auth(&mut sock, opt.device_token).await?;

        let cli = Client {
            opt,
            device_id,
            sock,
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
        
        Ok(())
    }
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
