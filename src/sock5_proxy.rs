use std::fmt;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufStream};
use tokio::net::{TcpListener, TcpStream};
use bytes::{Bytes, BytesMut, Buf, BufMut};

pub struct Sock5Proxy {}

impl Sock5Proxy {
    pub fn new() -> Sock5Proxy {
        Sock5Proxy {}
    }

    pub async fn start(&self) -> Result<()> {
        let listener = TcpListener::bind("127.0.0.1:8080").await.unwrap();

        loop {
            let (socket, _) = listener.accept().await?;
            tokio::spawn(async {
                Client::new(socket).start().await.expect("TODO: panic message");
            });
        }
    }
}

struct Client {
    buf_stream: BufStream<TcpStream>,
}

type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

impl Client {
    fn new(stream: TcpStream) -> Client {
        return Client {
            buf_stream: BufStream::new(stream),
        };
    }

    pub async fn start(&mut self) -> Result<()> {
        self.handshake().await.unwrap();
        return Ok(());
    }

    async fn handshake(&mut self) -> Result<()> {
        let req = self.handshake_req().await?;

        self.handshake_resp(HandshakeResp {
            version: VERSION,
            method: METHOD_USERNAME_PASSWORD,
        }).await?;

        let auth_req = self.auth_req().await?;

        self.auth_resp(AuthResp {
            version: 0x01,
            status: 0x00,
        }).await?;

        let connect_req = self.connect_req().await?;

        let mut remote_sock = self.connect_remote(connect_req.address).await?;

        self.connect_resp(ConnectResp {
            version: 0,
            response: 0,
            rsv: 0,
            address: connect_req.address,
            port: 0,
        }).await?;


        return Ok(());
    }

    async fn connect_remote(&self, address: Address) -> Result<TcpStream> {
        let stream: TcpStream;
        match address {
            Address::DomainName(domain_name) => {
                let mut find = false;
                for addr in tokio::net::lookup_host(domain_name).await? {
                    stream = TcpStream::connect(addr).await?;
                    find = true;
                    break;
                }

                if !find {
                    Err(eprint!(""))
                }
            }
            Address::IPV4(sockAddr) => {}
            Address::IPV6(sock_addr) => {}
        };

        return Ok(stream);
    }

    async fn handshake_req(&mut self) -> Result<HandshakeReq> {
        let version = self.buf_stream.read_u8().await?;
        let method_count: u8 = self.buf_stream.read_u8().await?;


        let mut methods = Vec::with_capacity(method_count as usize);
        methods.resize(method_count as usize, 0);
        let _ = self.buf_stream.read_exact(&mut methods).await;

        let handshake_req = HandshakeReq {
            version,
            methods,
        };
        Ok(handshake_req)
    }

    async fn auth_req(&mut self) -> Result<AuthReq> {
        let version = self.buf_stream.read_u8().await?;

        let username_len = self.buf_stream.read_u8().await?;
        let mut raw_username = Vec::new();
        raw_username.resize_with(username_len as usize, Default::default);
        self.buf_stream.read_exact(&mut raw_username).await?;

        let password_len = self.buf_stream.read_u8().await?;
        let mut raw_password = Vec::new();
        raw_password.resize_with(password_len as usize, Default::default);
        self.buf_stream.read_exact(&mut raw_password).await?;

        let username = String::from_utf8(raw_username)?;
        let password = String::from_utf8(raw_password)?;

        Ok(AuthReq {
            version,
            username,
            password,
        })
    }

    async fn auth_resp(&mut self, auth_resp: AuthResp) -> Result<()> {
        self.buf_stream.write_u8(auth_resp.version).await?;
        self.buf_stream.write_u8(auth_resp.status).await?;
        self.buf_stream.flush().await?;
        Ok(())
    }

    async fn handshake_resp(&mut self, resp: HandshakeResp) -> Result<()> {
        self.buf_stream.write_u8(resp.version).await?;
        self.buf_stream.write_u8(resp.method).await?;
        self.buf_stream.flush().await?;
        return Ok(());
    }

    async fn connect_req(&mut self) -> Result<ConnectReq> {
        let version = self.buf_stream.read_u8().await?;
        let command = self.buf_stream.read_u8().await?;
        let rsv = self.buf_stream.read_u8().await?;
        let address_type = self.buf_stream.read_u8().await?;

        let mut address: Address;
        if address_type == ADDRESS_TYPE_IP_V4 {
            let raw_ipv4_addr = self.buf_stream.read_u32().await?;

            address = Address::IPV4(std::net::Ipv4Addr::from(raw_ipv4_addr));
        } else if address_type == ADDRESS_TYPE_IP_DOMAIN_NAME {
            let domain_len = self.buf_stream.read_u8().await?;

            let mut raw_domain = Vec::new();
            raw_domain.resize_with(domain_len as usize, Default::default);

            self.buf_stream.read_exact(&mut raw_domain).await?;
            let domain_name = String::from_utf8(raw_domain)?;
            address = Address::DomainName(domain_name);
        } else if address_type == ADDRESS_TYPE_IP_V6 {
            let raw_ipv6_addr = self.buf_stream.read_u128().await?;


            address = Address::IPV6(std::net::Ipv6Addr::from(raw_ipv6_addr));
        } else {
            return Err(Box::new(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "early eof")));
        };

        let port = self.buf_stream.read_u16().await?;

        Ok(ConnectReq {
            version,
            command,
            rsv,
            address,
            port,
        })
    }

    async fn connect_resp(&mut self, resp: ConnectResp) -> Result<()> {
        self.buf_stream.write_u8(resp.version).await?;
        self.buf_stream.write_u8(resp.response).await?;
        self.buf_stream.write_u8(resp.rsv).await?;
        self.buf_stream.write_u8(resp.address.address_type()).await?;


        self.buf_stream.write_u16(resp.port).await?;

        Ok(())
    }
}


struct Error {}

impl fmt::Debug for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt("", f)
    }
}

const VERSION: u8 = 0x05;

// X'00' NO AUTHENTICATION REQUIRED
// X'01' GSSAPI
// X'02' USERNAME/PASSWORD
// X'03' to X'7F' IANA ASSIGNED
// X'80' to X'FE' RESERVED FOR PRIVATE METHODS
// X'FF' NO ACCEPTABLE METHODS
const METHOD_NO_AUTHENTICATION_REQUIRED: u8 = 0x00;
const METHOD_USERNAME_PASSWORD: u8 = 0x02;

// CONNECT X'01'
// BIND X'02'
// UDP ASSOCIATE X'03'
const COMMAND_CONNECT: u8 = 0x01;
const COMMAND_BIND: u8 = 0x01;
const COMMAND_UDP: u8 = 0x01;


struct HandshakeReq {
    version: u8,
    method_count: u8,
    methods: Vec<u8>,
}

struct HandshakeResp {
    version: u8,
    method: u8,
}

struct AuthReq {
    version: u8,
    username: String,
    password: String,
}

struct AuthResp {
    version: u8,
    status: u8,
}

struct ConnectReq {
    version: u8,
    command: u8,
    rsv: u8,
    address: Address,
    port: u16,
}

struct ConnectResp {
    version: u8,
    response: u8,
    rsv: u8,
    address: Address,
    port: u16,
}

enum Address {
    IPV4(std::net::Ipv4Addr),
    IPV6(std::net::Ipv6Addr),
    DomainName(String),
}

// IP V4 address: X'01'
// DOMAINNAME: X'03'
// IP V6 address: X'04'
const ADDRESS_TYPE_IP_V4: u8 = 0x01;
const ADDRESS_TYPE_IP_DOMAIN_NAME: u8 = 0x03;
const ADDRESS_TYPE_IP_V6: u8 = 0x04;

impl Address {
    fn address_type(&self) -> u8 {
        match self {
            Address::IPV4(_) => ADDRESS_TYPE_IP_V4,
            Address::IPV6(_) => ADDRESS_TYPE_IP_V6,
            Address::DomainName(_) => ADDRESS_TYPE_IP_DOMAIN_NAME,
        }
    }
}

enum Frame {}

