use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Message {
    pub id: String,
    pub chat_jid: String,
    pub sender: String,
    pub sender_name: Option<String>,
    pub content: String,
    pub timestamp: i64,
    pub is_from_me: bool,
    pub media_type: Option<String>,
    pub filename: Option<String>,
    pub url: Option<String>,
    pub direct_path: Option<String>,
    pub mimetype: Option<String>,
    pub file_length: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Chat {
    pub jid: String,
    pub name: Option<String>,
    pub last_message_time: Option<i64>,
    pub last_message: Option<String>,
    pub last_sender: Option<String>,
    pub last_is_from_me: Option<bool>,
}

impl Chat {
    #[allow(dead_code)]
    pub fn is_group(&self) -> bool {
        self.jid.ends_with("@g.us")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Contact {
    pub phone_number: String,
    pub name: Option<String>,
    pub jid: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct MessageContextDto {
    pub message: Message,
    pub before: Vec<Message>,
    pub after: Vec<Message>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SendOutcome {
    pub success: bool,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct DownloadOutcome {
    pub success: bool,
    pub message: String,
    pub file_path: Option<String>,
}

pub const NOT_PAIRED_MSG: &str = "not paired, run: whatsapp-mcp login";
