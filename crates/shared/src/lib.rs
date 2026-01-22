use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use base64::{Engine as _, engine::general_purpose::STANDARD};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct TunnelRequest {
    pub id: String,
    pub method: String,
    pub path: String,
    pub headers: HashMap<String, String>,
    pub body: Option<String>, // Base64 encoded
}

impl TunnelRequest {
    pub fn get_body_bytes(&self) -> Vec<u8> {
        self.body.as_ref()
            .and_then(|b| STANDARD.decode(b).ok())
            .unwrap_or_default()
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct TunnelResponse {
    pub request_id: String,
    pub status: u16,
    pub headers: HashMap<String, String>,
    pub body: Option<String>, // Base64 encoded
}

impl TunnelResponse {
    pub fn get_body_bytes(&self) -> Vec<u8> {
        self.body.as_ref()
            .and_then(|b| STANDARD.decode(b).ok())
            .unwrap_or_default()
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "type", content = "payload")]
pub enum TunnelMessage {
    #[serde(rename = "control:ready")]
    Ready { url: String },
    #[serde(rename = "data:request")]
    Request(TunnelRequest),
    #[serde(rename = "data:response")]
    Response(TunnelResponse),
}
