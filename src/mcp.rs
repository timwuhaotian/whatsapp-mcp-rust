use std::sync::Arc;

use rmcp::handler::server::wrapper::{Json, Parameters};
use rmcp::model::{CallToolResult, ContentBlock};
use rmcp::{tool, tool_router};
use schemars::JsonSchema;
use serde::Deserialize;

use crate::config::AppConfig;
use crate::store::ListMessagesQuery;
use crate::types::{Contact, DownloadOutcome, SendOutcome};
use crate::wa::Wa;

// ---------- params (names/defaults mirror main.py exactly) ----------

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SearchContactsParams {
    pub query: String,
}

#[derive(Debug, Deserialize, JsonSchema, Default)]
pub struct ListMessagesParams {
    pub after: Option<String>,
    pub before: Option<String>,
    pub sender_phone_number: Option<String>,
    pub chat_jid: Option<String>,
    pub query: Option<String>,
    #[serde(default = "default_limit")]
    pub limit: i64,
    #[serde(default)]
    pub page: i64,
    #[serde(default = "default_true")]
    pub include_context: bool,
    #[serde(default = "default_one")]
    pub context_before: i64,
    #[serde(default = "default_one")]
    pub context_after: i64,
}

#[derive(Debug, Deserialize, JsonSchema, Default)]
pub struct ListChatsParams {
    pub query: Option<String>,
    #[serde(default = "default_limit")]
    pub limit: i64,
    #[serde(default)]
    pub page: i64,
    #[serde(default = "default_true")]
    pub include_last_message: bool,
    #[serde(default = "default_last_active")]
    pub sort_by: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GetChatParams {
    pub chat_jid: String,
    #[serde(default = "default_true")]
    pub include_last_message: bool,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GetDirectChatParams {
    pub sender_phone_number: String,
}

#[derive(Debug, Deserialize, JsonSchema, Default)]
pub struct GetContactChatsParams {
    pub jid: String,
    #[serde(default = "default_limit")]
    pub limit: i64,
    #[serde(default)]
    pub page: i64,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GetLastInteractionParams {
    pub jid: String,
}

#[derive(Debug, Deserialize, JsonSchema, Default)]
pub struct GetMessageContextParams {
    pub message_id: String,
    #[serde(default = "default_five")]
    pub before: i64,
    #[serde(default = "default_five")]
    pub after: i64,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SendMessageParams {
    pub recipient: String,
    pub message: String,
}

#[derive(Debug, Deserialize, JsonSchema, Default)]
pub struct SendFileParams {
    pub recipient: String,
    pub media_path: String,
    pub caption: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SendAudioParams {
    pub recipient: String,
    pub media_path: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct DownloadMediaParams {
    pub message_id: String,
    pub chat_jid: String,
}

fn default_limit() -> i64 {
    20
}
fn default_true() -> bool {
    true
}
fn default_one() -> i64 {
    1
}
fn default_five() -> i64 {
    5
}
fn default_last_active() -> String {
    "last_active".into()
}

// ---------- server ----------

#[derive(Clone)]
pub struct McpServer {
    wa: Arc<Wa>,
}

#[tool_router(server_handler)]
impl McpServer {
    #[tool(description = "Search WhatsApp contacts by name or phone number")]
    fn search_contacts(
        &self,
        Parameters(p): Parameters<SearchContactsParams>,
    ) -> Json<Vec<Contact>> {
        Json(self.wa.store.search_contacts(&p.query).unwrap_or_default())
    }

    #[tool(description = "Get WhatsApp messages matching specified criteria with optional context")]
    fn list_messages(
        &self,
        Parameters(p): Parameters<ListMessagesParams>,
    ) -> Json<Vec<crate::types::Message>> {
        let out = self
            .wa
            .store
            .list_messages(&ListMessagesQuery {
                after: p.after.as_deref().and_then(parse_iso),
                before: p.before.as_deref().and_then(parse_iso),
                sender: p.sender_phone_number,
                chat_jid: p.chat_jid,
                query: p.query,
                limit: p.limit,
                page: p.page,
                include_context: p.include_context,
                context_before: p.context_before,
                context_after: p.context_after,
            })
            .unwrap_or_default();
        Json(out)
    }

    #[tool(description = "Get WhatsApp chats matching specified criteria")]
    fn list_chats(
        &self,
        Parameters(p): Parameters<ListChatsParams>,
    ) -> Json<Vec<crate::types::Chat>> {
        Json(
            self.wa
                .store
                .list_chats(
                    p.query.as_deref(),
                    p.limit,
                    p.page,
                    p.include_last_message,
                    &p.sort_by,
                )
                .unwrap_or_default(),
        )
    }

    #[tool(description = "Get WhatsApp chat metadata by JID")]
    fn get_chat(
        &self,
        Parameters(p): Parameters<GetChatParams>,
    ) -> Json<Option<crate::types::Chat>> {
        Json(
            self.wa
                .store
                .get_chat(&p.chat_jid, p.include_last_message)
                .unwrap_or(None),
        )
    }

    #[tool(description = "Get WhatsApp chat metadata by sender phone number")]
    fn get_direct_chat_by_contact(
        &self,
        Parameters(p): Parameters<GetDirectChatParams>,
    ) -> Json<Option<crate::types::Chat>> {
        Json(
            self.wa
                .store
                .get_direct_chat_by_contact(&p.sender_phone_number)
                .unwrap_or(None),
        )
    }

    #[tool(description = "Get all WhatsApp chats involving the contact")]
    fn get_contact_chats(
        &self,
        Parameters(p): Parameters<GetContactChatsParams>,
    ) -> Json<Vec<crate::types::Chat>> {
        Json(
            self.wa
                .store
                .get_contact_chats(&p.jid, p.limit, p.page)
                .unwrap_or_default(),
        )
    }

    #[tool(description = "Get most recent WhatsApp message involving the contact")]
    fn get_last_interaction(
        &self,
        Parameters(p): Parameters<GetLastInteractionParams>,
    ) -> Json<Option<crate::types::Message>> {
        Json(self.wa.store.get_last_interaction(&p.jid).unwrap_or(None))
    }

    #[tool(description = "Get context around a specific WhatsApp message")]
    fn get_message_context(
        &self,
        Parameters(p): Parameters<GetMessageContextParams>,
    ) -> Json<Option<crate::types::MessageContextDto>> {
        Json(
            self.wa
                .store
                .get_message_context(&p.message_id, p.before, p.after)
                .ok(),
        )
    }

    #[tool(
        description = "Send a WhatsApp message to a person or group. For group chats use the JID"
    )]
    async fn send_message(
        &self,
        Parameters(p): Parameters<SendMessageParams>,
    ) -> Json<SendOutcome> {
        if p.recipient.is_empty() {
            return Json(SendOutcome {
                success: false,
                message: "Recipient must be provided".into(),
            });
        }
        match self.wa.send_text(&p.recipient, &p.message).await {
            Ok(message) => Json(SendOutcome {
                success: true,
                message,
            }),
            Err(e) => Json(SendOutcome {
                success: false,
                message: e.to_string(),
            }),
        }
    }

    #[tool(
        description = "Send a file such as a picture, raw audio, video or document via WhatsApp to the specified recipient"
    )]
    async fn send_file(&self, Parameters(p): Parameters<SendFileParams>) -> Json<SendOutcome> {
        if p.recipient.is_empty() {
            return Json(SendOutcome {
                success: false,
                message: "Recipient must be provided".into(),
            });
        }
        match self
            .wa
            .send_file(
                &p.recipient,
                std::path::Path::new(&p.media_path),
                p.caption.as_deref(),
            )
            .await
        {
            Ok(message) => Json(SendOutcome {
                success: true,
                message,
            }),
            Err(e) => Json(SendOutcome {
                success: false,
                message: e.to_string(),
            }),
        }
    }

    #[tool(
        description = "Send any audio file as a WhatsApp audio message to the specified recipient. If it errors due to ffmpeg not being installed, use send_file instead"
    )]
    async fn send_audio_message(
        &self,
        Parameters(p): Parameters<SendAudioParams>,
    ) -> Json<SendOutcome> {
        match self
            .wa
            .send_audio(&p.recipient, std::path::Path::new(&p.media_path))
            .await
        {
            Ok(message) => Json(SendOutcome {
                success: true,
                message,
            }),
            Err(e) => Json(SendOutcome {
                success: false,
                message: e.to_string(),
            }),
        }
    }

    #[tool(description = "Download media from a WhatsApp message and get the local file path")]
    async fn download_media(
        &self,
        Parameters(p): Parameters<DownloadMediaParams>,
    ) -> CallToolResult {
        match self.wa.download_media(&p.message_id, &p.chat_jid).await {
            Ok(path) => {
                let outcome = DownloadOutcome {
                    success: true,
                    message: "Media downloaded successfully".into(),
                    file_path: Some(path.display().to_string()),
                };
                // Images <= 5MB are also attached so the model can see them.
                let mut content = vec![ContentBlock::text(
                    serde_json::to_string(&outcome).unwrap_or_default(),
                )];
                if let Ok(bytes) = std::fs::read(&path)
                    && bytes.len() <= 5 * 1024 * 1024
                    && let Some(mime) = mime_guess(&path)
                    && mime.starts_with("image/")
                {
                    content.push(ContentBlock::image(base64_encode(&bytes), mime));
                }
                CallToolResult::success(content)
            }
            Err(e) => {
                let outcome = DownloadOutcome {
                    success: false,
                    message: e.to_string(),
                    file_path: None,
                };
                CallToolResult::success(vec![ContentBlock::text(
                    serde_json::to_string(&outcome).unwrap_or_default(),
                )])
            }
        }
    }

    // ---------- P1 extras ----------

    #[tool(description = "Show WhatsApp connection state, own JID, and local message/chat counts")]
    fn whatsapp_status(&self) -> Json<serde_json::Value> {
        let (chats, messages) = self.wa.store.counts().unwrap_or((0, 0));
        Json(serde_json::json!({
            "connected": self.wa.get_state() == crate::wa::ConnState::Connected,
            "own_jid": self.wa.own_sender(),
            "chats": chats,
            "messages": messages,
        }))
    }

    #[tool(description = "Check whether phone numbers are registered on WhatsApp")]
    async fn check_on_whatsapp(
        &self,
        Parameters(p): Parameters<CheckOnWhatsappParams>,
    ) -> Json<Vec<serde_json::Value>> {
        let Ok(client) = self.wa.require_client() else {
            return Json(vec![]);
        };
        let jids: Vec<whatsapp_rust::Jid> = p
            .phone_numbers
            .iter()
            .filter_map(|n| Wa::parse_recipient(n).ok())
            .collect();
        let Ok(results) = client.contacts().is_on_whatsapp(&jids).await else {
            return Json(vec![]);
        };
        Json(
            results
                .iter()
                .map(|r| {
                    serde_json::json!({
                        "jid": r.jid.to_string(),
                        "is_registered": r.is_registered,
                        "is_business": r.is_business,
                    })
                })
                .collect(),
        )
    }

    #[tool(description = "Get group metadata: subject and participant list")]
    async fn get_group_info(
        &self,
        Parameters(p): Parameters<GetGroupInfoParams>,
    ) -> Json<serde_json::Value> {
        let Ok(client) = self.wa.require_client() else {
            return Json(
                serde_json::json!({"success": false, "message": crate::types::NOT_PAIRED_MSG}),
            );
        };
        let Ok(jid) = Wa::parse_recipient(&p.group_jid) else {
            return Json(serde_json::json!({"success": false, "message": "invalid group JID"}));
        };
        match client.groups().get_metadata(&jid).await {
            Ok(meta) => Json(serde_json::json!({
                "success": true,
                "subject": meta.subject,
                "participants": meta.participants.iter().map(|pt| pt.jid.to_string()).collect::<Vec<_>>(),
            })),
            Err(e) => Json(serde_json::json!({"success": false, "message": e.to_string()})),
        }
    }

    #[tool(description = "Send an emoji reaction to a message (empty emoji removes it)")]
    async fn send_reaction(
        &self,
        Parameters(p): Parameters<SendReactionParams>,
    ) -> Json<SendOutcome> {
        let Ok(client) = self.wa.require_client() else {
            return Json(SendOutcome {
                success: false,
                message: crate::types::NOT_PAIRED_MSG.into(),
            });
        };
        let Ok(chat) = Wa::parse_recipient(&p.chat_jid) else {
            return Json(SendOutcome {
                success: false,
                message: "invalid chat JID".into(),
            });
        };
        // Group/status targets must carry the original sender as participant.
        let participant = self
            .wa
            .store
            .get_message_context(&p.message_id, 0, 0)
            .ok()
            .map(|c| c.message.sender)
            .filter(|s| !s.is_empty());
        let key = whatsapp_rust::prelude::wa::MessageKey {
            remote_jid: Some(chat.to_string()),
            from_me: None,
            id: Some(p.message_id.clone()),
            participant,
        };
        match client.send_reaction(chat, key, &p.emoji).await {
            Ok(_) => Json(SendOutcome {
                success: true,
                message: format!("reaction sent to {}", p.message_id),
            }),
            Err(e) => Json(SendOutcome {
                success: false,
                message: e.to_string(),
            }),
        }
    }

    #[tool(description = "Mark messages as read in a chat")]
    async fn mark_as_read(&self, Parameters(p): Parameters<MarkAsReadParams>) -> Json<SendOutcome> {
        let Ok(client) = self.wa.require_client() else {
            return Json(SendOutcome {
                success: false,
                message: crate::types::NOT_PAIRED_MSG.into(),
            });
        };
        let Ok(chat) = Wa::parse_recipient(&p.chat_jid) else {
            return Json(SendOutcome {
                success: false,
                message: "invalid chat JID".into(),
            });
        };
        let sender = p
            .sender_jid
            .as_deref()
            .and_then(|s| Wa::parse_recipient(s).ok());
        let ids: Vec<&str> = p.message_ids.iter().map(|s| s.as_str()).collect();
        match client.mark_as_read(&chat, sender.as_ref(), &ids).await {
            Ok(()) => Json(SendOutcome {
                success: true,
                message: "marked as read".into(),
            }),
            Err(e) => Json(SendOutcome {
                success: false,
                message: e.to_string(),
            }),
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CheckOnWhatsappParams {
    pub phone_numbers: Vec<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GetGroupInfoParams {
    pub group_jid: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct MarkAsReadParams {
    pub chat_jid: String,
    pub message_ids: Vec<String>,
    pub sender_jid: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SendReactionParams {
    pub chat_jid: String,
    pub message_id: String,
    pub emoji: String,
}

fn parse_iso(s: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(s)
        .map(|d| d.timestamp())
        .ok()
        .or_else(|| {
            chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S")
                .map(|d| d.and_utc().timestamp())
                .ok()
        })
}

fn mime_guess(path: &std::path::Path) -> Option<&'static str> {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .as_deref()
    {
        Some("jpg") | Some("jpeg") => Some("image/jpeg"),
        Some("png") => Some("image/png"),
        Some("gif") => Some("image/gif"),
        Some("webp") => Some("image/webp"),
        _ => None,
    }
}

fn base64_encode(bytes: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

pub async fn serve(cfg: &AppConfig) -> anyhow::Result<()> {
    let wa = Wa::open(cfg)?;
    wa.connect(cfg).await?;
    let server = McpServer { wa };
    tracing::info!("whatsapp-mcp serving on stdio");
    use rmcp::ServiceExt;
    let service = server.serve(rmcp::transport::io::stdio()).await?;
    service.waiting().await?;
    Ok(())
}

// Re-export for onboarding reuse.
pub fn tool_names() -> Vec<String> {
    let router = McpServer::tool_router();
    let mut names: Vec<String> = router.map.keys().map(|k| k.to_string()).collect();
    names.sort();
    names
}
