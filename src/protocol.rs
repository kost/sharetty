use serde::{Deserialize, Serialize};

pub const ALPN_SHARETTY: &[u8] = b"sharetty";

#[derive(Serialize, Deserialize, Debug)]
pub struct HandshakeRequest {
    pub path: String,
    pub auth: Option<String>,
    pub version: String,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct HandshakeResponse {
    pub status: u16,
    pub error: Option<String>,
}
