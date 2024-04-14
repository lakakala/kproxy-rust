pub type ClientIDType = u64;

pub type ChannelIDType = u64;

pub type ConnectionType = u64;

pub enum CommandType {
    AuthRequest,
    AuthResponse,
    AllocChannelNotice,
    AllocChannelNoticeResponse,
}

pub struct BaseResp {
    code: i32,
}

pub struct AuthRequest {
    client_id: u64,
}

impl AuthRequest {
    pub fn get_client_id(self: &Self) -> u64 {
        return self.client_id;
    }
}

struct AuthRequestMessage();

// trait Codec {
//     async fn decode<T>(reader: &mut T) -> Result<Box<dyn Message>, Box<dyn Error>>
//     where
//         T: AsyncRead + Unpin;
// }

pub struct AuthResponse {
    base_resp: BaseResp,
}

pub enum ChannelType {
    Tcp { addr: String, port: u16 },
}

pub struct AllocChannelNotice {
    channel_id: ChannelIDType,
    channel_type: ChannelType,
}

impl AllocChannelNotice {
    pub fn get_client_id(self: &Self) -> ChannelIDType {
        self.channel_id
    }

    pub fn get_channel_type(self: &Self) -> ChannelType {
        todo!()
    }
}

pub struct AllocChannelNoticeResponse {
    base_resp: BaseResp,
}
