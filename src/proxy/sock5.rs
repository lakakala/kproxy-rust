use std::collections::HashMap;
use std::fmt::{Display, Formatter};
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddrV4, SocketAddrV6};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Mutex};

struct Sock5ProxyServerConfig {
    bind_addr: String,
}

struct Sock5ProxyServer {
    config: Sock5ProxyServerConfig,
}

impl Sock5ProxyServer {
    fn new(config: Sock5ProxyServerConfig) -> Sock5ProxyServer {
        Sock5ProxyServer { config }
    }

    async fn start(self) -> Result<()> {
        let mut listener = TcpListener::bind(self.config.bind_addr)
            .await
            .map_err(ProxyError::map_io_err)?;

        loop {
            let (sock, _) = listener.accept().await.map_err(ProxyError::map_io_err)?;
            let client = Sock5ProxyClient::new(sock);
            tokio::spawn(client.start());
        }
        Ok(())
    }
}

struct ConnectionManager {
    conn_map: Mutex<HashMap<i64, Sock5ProxyClient>>,
}

impl ConnectionManager {
    fn new() -> ConnectionManager {
        ConnectionManager {
            conn_map: Mutex::new(HashMap::new()),
        }
    }
}

struct Sock5ProxyClient {
    sock: TcpStream,
}

impl Sock5ProxyClient {
    fn new(sock: TcpStream) -> Sock5ProxyClient {
        Sock5ProxyClient { sock }
    }

    async fn start(mut self) -> Result<()> {
        let methods = read_init_req(&mut self.sock).await?;

        write_init_resp(&mut self.sock, Method::NoAuth).await?;

        let (command, address) = read_connect_req(&mut self.sock).await?;

        if command == CommandType::Connect {}
        Ok(())
    }

    async fn connect(&mut self, address: AddressType) -> Result<TcpStream> {
        match address {
            AddressType::Ipv4(sock_addr_v4) => TcpStream::connect(sock_addr_v4).await,
            AddressType::DomainName(domain_name, port) => {
                TcpStream::connect(format!("{}:{}", domain_name, port)).await
            }
            AddressType::Ipv6(sock_addr_v6) => TcpStream::connect(sock_addr_v6).await,
        }
        .map_err(ProxyError::map_io_err)
    }
}

#[derive(Debug)]
enum ProxyErrorKind {
    ErrIO(std::io::Error),
    ErrUnknownSock5Version,
    ErrUnknownSock5Method,
    ErrUnknownSock5Command,
    ErrUnknownSock5Address,
}

#[derive(Debug)]
struct ProxyError {
    kind: ProxyErrorKind,
}

impl ProxyError {
    fn new(kind: ProxyErrorKind) -> ProxyError {
        ProxyError { kind }
    }

    fn map_io_err(err: std::io::Error) -> ProxyError {
        ProxyError::new(ProxyErrorKind::ErrIO(err))
    }
}

impl Display for ProxyError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        todo!()
    }
}

impl std::error::Error for ProxyError {}

type Result<T> = std::result::Result<T, ProxyError>;

const SOCK5_VERSION: u8 = 0x05;
const SOCK5_METHOD_NO_AUTH: u8 = 0x00;
const SOCK5_METHOD_USERNAME_PASSWD: u8 = 0x02;
const SOCK5_METHOD_NO_ACCEPTABLE_METHODS: u8 = 0xff;

const SOCK5_COMMAND_CONNECT: u8 = 0x01;
const SOCK5_COMMAND_BIND: u8 = 0x02;
const SOCK5_COMMAND_UDP: u8 = 0x03;

const SOCK5_ADDRESS_IPV4: u8 = 0x01;
const SOCK5_ADDRESS_DOMAIN_NAME: u8 = 0x03;
const SOCK5_ADDRESS_IPV6: u8 = 0x04;

const SOCK5_REPLY_SUCCESS: u8 = 0x00;
const SOCK5_REPLY_GENERAL_SERVER_FAILURE: u8 = 0x00;

// const method_map: HashMap<Method, (u8, String)> = HashMap::from_hash();

enum Method {
    NoAuth,
    UserNamePasswd,
    NoAcceptableMethods,
}

impl TryFrom<u8> for Method {
    type Error = ProxyError;

    fn try_from(value: u8) -> std::result::Result<Self, Self::Error> {
        match value {
            SOCK5_METHOD_NO_AUTH => Ok(Method::NoAuth),
            SOCK5_METHOD_USERNAME_PASSWD => Ok(Method::UserNamePasswd),
            SOCK5_METHOD_NO_ACCEPTABLE_METHODS => Ok(Method::NoAcceptableMethods),
            _ => Err(ProxyError::new(ProxyErrorKind::ErrUnknownSock5Method)),
        }
    }
}

impl Into<u8> for Method {
    fn into(self) -> u8 {
        match self {
            Method::NoAuth => SOCK5_METHOD_NO_AUTH,
            Method::UserNamePasswd => SOCK5_METHOD_USERNAME_PASSWD,
            Method::NoAcceptableMethods => SOCK5_METHOD_NO_ACCEPTABLE_METHODS,
        }
    }
}

#[derive(PartialEq)]
enum CommandType {
    Connect,
    Bind,
    UDP,
}

impl TryFrom<u8> for CommandType {
    type Error = ProxyError;

    fn try_from(value: u8) -> std::result::Result<Self, Self::Error> {
        match value {
            SOCK5_COMMAND_CONNECT => Ok(CommandType::Connect),
            SOCK5_COMMAND_BIND => Ok(CommandType::Bind),
            SOCK5_COMMAND_UDP => Ok(CommandType::UDP),
            _ => Err(ProxyError::new(ProxyErrorKind::ErrUnknownSock5Command)),
        }
    }
}

impl Into<u8> for CommandType {
    fn into(self) -> u8 {
        match self {
            CommandType::Connect => SOCK5_COMMAND_CONNECT,
            CommandType::Bind => SOCK5_COMMAND_BIND,
            CommandType::UDP => SOCK5_COMMAND_UDP,
        }
    }
}

enum AddressType {
    Ipv4(SocketAddrV4),
    DomainName(String, u16),
    Ipv6(SocketAddrV6),
}

enum ReplyType {
    Success,
    GeneralServerFailure,
}

impl Into<u8> for ReplyType {
    fn into(self) -> u8 {
        match self {
            ReplyType::Success => SOCK5_REPLY_SUCCESS,
            ReplyType::GeneralServerFailure => SOCK5_REPLY_GENERAL_SERVER_FAILURE,
        }
    }
}

async fn read_init_req<T>(reader: &mut T) -> Result<Vec<Method>>
where
    T: AsyncRead + Unpin,
{
    let version = reader.read_u8().await.map_err(ProxyError::map_io_err)?;

    if version != SOCK5_VERSION {
        return Err(ProxyError::new(ProxyErrorKind::ErrUnknownSock5Version));
    }

    let method_len = reader.read_u8().await.map_err(ProxyError::map_io_err)?;

    let mut methods: Vec<Method> = Vec::with_capacity(method_len as usize);
    for _ in 0..method_len {
        methods.push(
            reader
                .read_u8()
                .await
                .map_err(ProxyError::map_io_err)?
                .try_into()?,
        );
    }

    return Ok(methods);
}

async fn write_init_resp<T>(writer: &mut T, method: Method) -> Result<()>
where
    T: AsyncWrite + Unpin,
{
    writer
        .write_u8(SOCK5_VERSION)
        .await
        .map_err(ProxyError::map_io_err)?;

    writer
        .write_u8(method.into())
        .await
        .map_err(ProxyError::map_io_err)?;
    Ok(())
}

async fn read_connect_req<T: AsyncRead + Unpin>(
    reader: &mut T,
) -> Result<(CommandType, AddressType)> {
    let version = reader.read_u8().await.map_err(ProxyError::map_io_err)?;
    if version != SOCK5_VERSION {
        return Err(ProxyError::new(ProxyErrorKind::ErrUnknownSock5Version));
    }

    let command: CommandType = reader
        .read_u8()
        .await
        .map_err(ProxyError::map_io_err)?
        .try_into()?;

    let _ = reader.read_u8().await.map_err(ProxyError::map_io_err)?;

    let raw_address_type = reader.read_u8().await.map_err(ProxyError::map_io_err)?;

    let address_type: AddressType = match raw_address_type {
        SOCK5_ADDRESS_IPV4 => {
            let raw_ipv4 = reader.read_u32().await.map_err(ProxyError::map_io_err)?;
            let port = reader.read_u16().await.map_err(ProxyError::map_io_err)?;
            Ok(AddressType::Ipv4(SocketAddrV4::new(
                Ipv4Addr::from(raw_ipv4),
                port,
            )))
        }
        SOCK5_ADDRESS_DOMAIN_NAME => {
            let domain_len = reader.read_u8().await.map_err(ProxyError::map_io_err)?;
            let mut raw_domain_name: Vec<u8> = vec![0, domain_len];
            reader
                .read_exact(&mut raw_domain_name)
                .await
                .map_err(ProxyError::map_io_err)?;

            let domain_name = String::from_utf8(raw_domain_name)
                .map_err(|err| ProxyError::new(ProxyErrorKind::ErrUnknownSock5Address))?;
            let port = reader.read_u16().await.map_err(ProxyError::map_io_err)?;
            Ok(AddressType::DomainName(domain_name, port))
        }
        SOCK5_ADDRESS_IPV6 => {
            let raw_ipv6 = reader.read_u128().await.map_err(ProxyError::map_io_err)?;
            let port = reader.read_u16().await.map_err(ProxyError::map_io_err)?;
            Ok(AddressType::Ipv6(SocketAddrV6::new(
                Ipv6Addr::from(raw_ipv6),
                port,
                0,
                1,
            )))
        }
        _ => Err(ProxyError::new(ProxyErrorKind::ErrUnknownSock5Address)),
    }?;

    Ok((command, address_type))
}

async fn write_connect_resp<T: AsyncWrite + Unpin>(
    writer: &mut T,
    reply: ReplyType,
    address: AddressType,
) -> Result<()> {
    writer
        .write_u8(SOCK5_VERSION)
        .await
        .map_err(ProxyError::map_io_err)?;
    writer
        .write_u8(reply.into())
        .await
        .map_err(ProxyError::map_io_err)?;
    writer
        .write_u8(0x00)
        .await
        .map_err(ProxyError::map_io_err)?;

    match address {
        AddressType::Ipv4(sock_addr_v4) => {
            writer
                .write_u8(SOCK5_ADDRESS_IPV4)
                .await
                .map_err(ProxyError::map_io_err)?;
            writer
                .write_all(&mut sock_addr_v4.ip().octets())
                .await
                .map_err(ProxyError::map_io_err)?;
            writer
                .write_u16(sock_addr_v4.port())
                .await
                .map_err(ProxyError::map_io_err)?;
            Ok(())
        }
        AddressType::DomainName(domain_name, port) => {
            writer
                .write_u8(SOCK5_ADDRESS_DOMAIN_NAME)
                .await
                .map_err(ProxyError::map_io_err)?;
            writer
                .write_u8(domain_name.len() as u8)
                .await
                .map_err(ProxyError::map_io_err)?;
            writer
                .write_all(domain_name.as_bytes())
                .await
                .map_err(ProxyError::map_io_err)?;
            Ok(())
        }
        AddressType::Ipv6(sock_addr_v6) => {
            writer
                .write_u8(SOCK5_ADDRESS_IPV6)
                .await
                .map_err(ProxyError::map_io_err)?;
            writer
                .write_all(&mut sock_addr_v6.ip().octets())
                .await
                .map_err(ProxyError::map_io_err)?;
            writer
                .write_u16(sock_addr_v6.port())
                .await
                .map_err(ProxyError::map_io_err)?;
            Ok(())
        }
    }?;
    Ok(())
}
