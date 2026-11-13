use std::sync::{Arc, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use whatsapp_rust::pair::NATIVE_CAMERA_DEEP_LINK_PREFIX;
use whatsapp_rust::pair_code::PairCodeOptions;
use whatsapp_rust::prelude::*;
use whatsapp_rust::wacore::store::DevicePropsOverride;

use crate::config::{AppConfig, chat_media_dir};
use crate::extract::{self, Extracted};
use crate::store::{Store, StoredMessage};
use crate::types::NOT_PAIRED_MSG;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnState {
    Unpaired,
    Connecting,
    Connected,
    LoggedOut,
}

pub struct Wa {
    pub store: Store,
    pub data_dir: std::path::PathBuf,
    pub state: Arc<RwLock<ConnState>>,
    pub client: RwLock<Option<Arc<Client>>>,
    /// Set by login so the shared PairingQrCode handler also prints the QR
    /// and pair code to stdout (serve must keep stdout clean for MCP).
    pub stdout_qr: std::sync::atomic::AtomicBool,
}

/// Options for establishing the WhatsApp connection. Serve and login share
/// this pipeline so both ingest messages/history identically; login only adds
/// pair-code config, a stdout QR hook, and waits for pairing to complete.
#[derive(Default)]
pub struct ConnectOptions {
    pub phone: Option<String>,
    pub full_sync: bool,
    pub on_qr_stdout: bool,
}

impl Wa {
    pub fn open(cfg: &AppConfig) -> Result<Arc<Self>> {
        Ok(Arc::new(Self {
            store: Store::open(&cfg.messages_db())?,
            data_dir: cfg.data_dir.clone(),
            state: Arc::new(RwLock::new(ConnState::Unpaired)),
            client: RwLock::new(None),
            stdout_qr: std::sync::atomic::AtomicBool::new(false),
        }))
    }

    pub fn set_state(&self, s: ConnState) {
        *self.state.write().unwrap() = s;
    }

    pub fn get_state(&self) -> ConnState {
        *self.state.read().unwrap()
    }

    pub fn require_client(&self) -> Result<Arc<Client>> {
        let c = self
            .client
            .read()
            .unwrap()
            .clone()
            .context(NOT_PAIRED_MSG)?;
        if self.get_state() == ConnState::Connected || c.is_connected() {
            Ok(c)
        } else {
            anyhow::bail!(NOT_PAIRED_MSG)
        }
    }

    pub fn own_sender(&self) -> String {
        self.client
            .read()
            .unwrap()
            .as_ref()
            .and_then(|c| c.pn())
            .map(|j| j.to_string())
            .unwrap_or_default()
    }

    /// Build the bot, register all event handlers, and spawn it in the
    /// background. Returns when the first connection attempt settles; the
    /// caller decides whether to block on QR/pairing afterwards.
    pub async fn connect(self: &Arc<Self>, cfg: &AppConfig) -> Result<()> {
        self.connect_with(cfg, ConnectOptions::default()).await
    }

    pub async fn connect_with(
        self: &Arc<Self>,
        cfg: &AppConfig,
        opts: ConnectOptions,
    ) -> Result<()> {
        if self.client.read().unwrap().is_some() {
            return Ok(());
        }
        let backend = SqliteStore::new(cfg.whatsapp_db().to_string_lossy().as_ref())
            .await
            .context("open whatsapp session db")?;
        self.set_state(ConnState::Connecting);
        let me = Arc::clone(self);
        let mut builder = Bot::builder().with_backend(backend);
        if let Some(number) = opts.phone {
            let digits: String = number.chars().filter(|c| c.is_ascii_digit()).collect();
            builder = builder.with_pair_code(PairCodeOptions {
                phone_number: digits,
                ..Default::default()
            });
        }
        if opts.full_sync {
            // UWP row of the device-props branch table: ask the server for a
            // full backfill instead of recent-only. Costs more time/traffic.
            builder = builder.with_device_props(
                DevicePropsOverride::new()
                    .with_platform_type(wa::device_props::PlatformType::UWP)
                    .with_require_full_sync(true)
                    .with_history_sync_config(wa::device_props::HistorySyncConfig {
                        full_sync_days_limit: Some(365),
                        ..Default::default()
                    }),
            );
        }
        if opts.on_qr_stdout {
            self.stdout_qr
                .store(true, std::sync::atomic::Ordering::Relaxed);
        }
        let bot = builder
            .on_event(move |event, client| {
                let me = Arc::clone(&me);
                async move {
                    me.handle_event(&event, &client).await;
                }
            })
            .build()
            .await
            .context("build whatsapp bot")?;
        let handle = bot.spawn();
        let client = handle.client();
        *self.client.write().unwrap() = Some(client);
        // Leak the handle: the bot lives as long as the process. Dropping it
        // would abort the run task.
        std::mem::forget(handle);
        Ok(())
    }

    async fn handle_event(self: &Arc<Self>, event: &Event, client: &Arc<Client>) {
        match event {
            Event::PairingQrCode(qr) => {
                self.on_qr(&qr.code);
                if self.stdout_qr.load(std::sync::atomic::Ordering::Relaxed) {
                    // login is the only stdout exception: the user scans here.
                    println!("Scan this QR code (valid {:?}):", qr.timeout);
                    println!("{}{}", NATIVE_CAMERA_DEEP_LINK_PREFIX, qr.code);
                    use qrcode::QrCode;
                    use qrcode::render::unicode::Dense1x2;
                    if let Ok(qr_img) = QrCode::new(qr.code.as_bytes()) {
                        println!("{}", qr_img.render::<Dense1x2>().build());
                    }
                } else {
                    Self::render_qr_stderr(&qr.code);
                }
            }
            Event::PairingCode(pc) => {
                if self.stdout_qr.load(std::sync::atomic::Ordering::Relaxed) {
                    println!(
                        "Enter this code on your phone (valid {:?}): {}",
                        pc.timeout, pc.code
                    );
                } else {
                    tracing::info!("pair code (valid {:?}): {}", pc.timeout, pc.code);
                }
            }
            Event::Connected(_) => {
                self.set_state(ConnState::Connected);
                tracing::info!("whatsapp connected");
            }
            Event::Disconnected(_) => {
                if self.get_state() == ConnState::Connected {
                    tracing::warn!("whatsapp disconnected");
                }
            }
            Event::LoggedOut(info) => {
                self.set_state(ConnState::LoggedOut);
                tracing::error!("whatsapp logged out: {:?}", info.reason);
            }
            Event::PairSuccess(info) => {
                self.set_state(ConnState::Connected);
                tracing::info!("paired as {}", info.id);
            }
            Event::Messages(batch) => {
                for inbound in batch.messages.iter() {
                    self.ingest_message(&inbound.message, &inbound.info, client)
                        .await;
                }
            }
            Event::HistorySync(lazy) => {
                self.ingest_history(lazy, client).await;
            }
            Event::GroupUpdate(u) => {
                let name = u.notify.clone().unwrap_or_default();
                if !name.is_empty() {
                    let ts = now_unix();
                    let _ = self.store.upsert_chat(&u.group_jid.to_string(), &name, ts);
                }
            }
            _ => {}
        }
    }

    fn on_qr(&self, code: &str) {
        let path = self.data_dir.join("qr.txt");
        if std::fs::write(&path, code).is_ok() {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
            }
        }
        tracing::info!("scan to pair: {}{}", NATIVE_CAMERA_DEEP_LINK_PREFIX, code);
    }

    fn render_qr_stderr(code: &str) {
        use qrcode::QrCode;
        use qrcode::render::unicode::Dense1x2;
        match QrCode::new(code.as_bytes()) {
            Ok(qr) => {
                eprintln!(
                    "\nScan this QR with WhatsApp > Linked devices:\n{}\n",
                    qr.render::<Dense1x2>().build()
                );
            }
            Err(e) => tracing::warn!("qr render failed: {e}"),
        }
    }

    async fn ingest_message(&self, msg: &wa::Message, info: &MessageInfo, client: &Arc<Client>) {
        let chat_jid = info.source.chat.to_string();
        let sender_jid = info.source.sender.to_string();
        let ts = info.timestamp.timestamp();
        let ex: Extracted = extract::extract(msg);
        if ex.content.is_empty() && ex.media_type.as_deref().unwrap_or("").is_empty() {
            return;
        }
        let name = group_or_contact_name(
            client,
            &self.store,
            &info.source.chat,
            &chat_jid,
            &sender_jid,
            &info.push_name,
        )
        .await;
        let _ = self.store.upsert_chat(&chat_jid, &name, ts);
        let _ = self.store.upsert_message(&StoredMessage {
            id: info.id.to_string(),
            chat_jid,
            sender: sender_jid,
            sender_name: if info.push_name.is_empty() {
                None
            } else {
                Some(info.push_name.clone())
            },
            content: ex.content,
            timestamp: ts,
            is_from_me: info.source.is_from_me,
            media_type: ex.media_type,
            filename: ex.filename,
            url: ex.url,
            direct_path: ex.direct_path,
            media_key: ex.media_key,
            file_sha256: ex.file_sha256,
            file_enc_sha256: ex.file_enc_sha256,
            file_length: ex.file_length.map(|v| v as i64),
            mimetype: ex.mimetype,
        });
    }

    async fn ingest_history(
        &self,
        lazy: &whatsapp_rust::types::events::LazyHistorySync,
        client: &Arc<Client>,
    ) {
        let mut stream = lazy.stream();
        let mut conv = wa::Conversation::default();
        let mut n = 0u64;
        loop {
            match stream.next_conversation_into(&mut conv) {
                Ok(true) => {
                    self.ingest_conversation(&conv, client).await;
                    n += 1;
                    conv = wa::Conversation::default();
                }
                Ok(false) => break,
                Err(e) => {
                    tracing::warn!("history sync decode error: {e}");
                    break;
                }
            }
        }
        tracing::info!("history sync: ingested {n} conversations");
    }

    async fn ingest_conversation(&self, conv: &wa::Conversation, client: &Arc<Client>) {
        let chat_id = conv.id.clone();
        if chat_id.is_empty() {
            return;
        }
        let chat_jid: Jid = match chat_id.parse() {
            Ok(j) => j,
            Err(_) => return,
        };
        let chat_jid_str = chat_jid.to_string();
        for hsm in &conv.messages {
            let Some(wmi) = hsm.message.as_option() else {
                continue;
            };
            let Some(key) = wmi.key.as_option() else {
                continue;
            };
            let id = match &key.id {
                Some(id) => id.clone(),
                None => continue,
            };
            let Some(inner) = wmi.message.as_option() else {
                continue;
            };
            let ts = wmi.message_timestamp.unwrap_or(0) as i64;
            let ex = extract::extract(inner);
            if ex.content.is_empty() && ex.media_type.as_deref().unwrap_or("").is_empty() {
                continue;
            }
            let is_from_me = key.from_me.unwrap_or(false);
            let sender = if is_from_me {
                self.own_sender()
            } else if let Some(p) = &key.participant {
                p.clone()
            } else {
                chat_jid_str.clone()
            };
            // Chat name: authoritative stored name first, then the sync payload,
            // network lookup last. A "Group <user>" placeholder is never stored
            // (the short-circuit would otherwise pin it forever).
            let sync_name = conversation_display_name(conv);
            let existing = self
                .store
                .get_chat(&chat_jid_str, false)
                .ok()
                .flatten()
                .and_then(|c| c.name)
                .filter(|n| !n.is_empty() && !is_placeholder_name(n));
            let net: Option<String> = if existing.is_some() || sync_name.is_some() {
                None
            } else {
                Some(
                    group_or_contact_name(
                        client,
                        &self.store,
                        &chat_jid,
                        &chat_jid_str,
                        &sender,
                        wmi.push_name.as_deref().unwrap_or(""),
                    )
                    .await,
                )
            };
            let name = existing
                .or(sync_name)
                .or(net.filter(|n| !is_placeholder_name(n)))
                .unwrap_or_default();
            let _ = self.store.upsert_chat(&chat_jid_str, &name, ts);
            let _ = self.store.upsert_message(&StoredMessage {
                id,
                chat_jid: chat_jid_str.clone(),
                sender,
                sender_name: wmi.push_name.clone(),
                content: ex.content,
                timestamp: ts,
                is_from_me,
                media_type: ex.media_type.clone(),
                filename: ex.filename.clone(),
                url: ex.url.clone(),
                direct_path: ex.direct_path.clone(),
                media_key: ex.media_key.clone(),
                file_sha256: ex.file_sha256.clone(),
                file_enc_sha256: ex.file_enc_sha256.clone(),
                file_length: ex.file_length.map(|v| v as i64),
                mimetype: ex.mimetype.clone(),
            });
        }
    }

    // ---- sending ----

    pub fn parse_recipient(recipient: &str) -> Result<Jid> {
        if recipient.contains('@') {
            recipient.parse::<Jid>().context("parse recipient JID")
        } else {
            let digits: String = recipient.chars().filter(|c| c.is_ascii_digit()).collect();
            if digits.is_empty() {
                anyhow::bail!("Recipient must be provided");
            }
            Ok(Jid::pn(digits))
        }
    }

    pub async fn send_text(&self, recipient: &str, message: &str) -> Result<String> {
        let client = self.require_client()?;
        let to = Self::parse_recipient(recipient)?;
        let res = client.send_text(to.clone(), message).await?;
        // Record our own message so history stays complete.
        let ts = now_unix();
        let chat = to.to_string();
        let me = self.own_sender();
        // Empty name: the CASE merge in upsert_chat keeps the stored name.
        // (Passing the JID here used to clobber real contact names.)
        let _ = self.store.upsert_chat(&chat, "", ts);
        let _ = self.store.upsert_message(&StoredMessage {
            id: res.message_id.clone(),
            chat_jid: chat,
            sender: me,
            sender_name: None,
            content: message.to_string(),
            timestamp: ts,
            is_from_me: true,
            media_type: None,
            filename: None,
            url: None,
            direct_path: None,
            media_key: None,
            file_sha256: None,
            file_enc_sha256: None,
            file_length: None,
            mimetype: None,
        });
        Ok(format!("Message sent to {recipient}"))
    }

    pub async fn send_file(
        &self,
        recipient: &str,
        media_path: &std::path::Path,
        caption: Option<&str>,
    ) -> Result<String> {
        let client = self.require_client()?;
        if !media_path.is_file() {
            anyhow::bail!("Media file not found: {}", media_path.display());
        }
        let data =
            std::fs::read(media_path).with_context(|| format!("read {}", media_path.display()))?;
        let (media_type, mime) = classify_media(media_path);
        let up = client
            .upload(data, media_type, Default::default())
            .await
            .context("upload media")?;
        // Upload first so fields survive moving `up` into the builder
        // (UploadResponse is #[non_exhaustive]: not constructible here).
        let db_url = up.url.clone();
        let db_direct = up.direct_path.clone();
        let db_key = up.media_key.to_vec();
        let db_sha = up.file_sha256.to_vec();
        let db_enc = up.file_enc_sha256.to_vec();
        let db_len = up.file_length;
        let msg = build_media_message(media_type, up, mime, caption, media_path);
        let to = Self::parse_recipient(recipient)?;
        let res = client.send_message(to.clone(), msg).await?;
        let ts = now_unix();
        let chat = to.to_string();
        let me = self.own_sender();
        // Empty name: the CASE merge in upsert_chat keeps the stored name.
        let _ = self.store.upsert_chat(&chat, "", ts);
        let _ = self.store.upsert_message(&StoredMessage {
            id: res.message_id.clone(),
            chat_jid: chat,
            sender: me,
            sender_name: None,
            content: caption.unwrap_or_default().to_string(),
            timestamp: ts,
            is_from_me: true,
            media_type: Some(
                match media_type {
                    whatsapp_rust::download::MediaType::Image => "image",
                    whatsapp_rust::download::MediaType::Video => "video",
                    whatsapp_rust::download::MediaType::Audio => "audio",
                    _ => "document",
                }
                .into(),
            ),
            filename: media_path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned()),
            url: Some(db_url),
            direct_path: Some(db_direct),
            media_key: Some(db_key),
            file_sha256: Some(db_sha),
            file_enc_sha256: Some(db_enc),
            file_length: Some(db_len as i64),
            mimetype: Some(mime.to_string()),
        });
        Ok(format!("Message sent to {recipient}"))
    }

    pub async fn send_audio(
        &self,
        recipient: &str,
        media_path: &std::path::Path,
    ) -> Result<String> {
        let ogg = crate::audio::convert_to_opus_ogg(media_path).await?;
        let data = std::fs::read(&ogg)?;
        let (seconds, waveform) = crate::audio::analyze_ogg_opus(&data)?;
        let client = self.require_client()?;
        let up = client
            .upload(
                data,
                whatsapp_rust::download::MediaType::Audio,
                Default::default(),
            )
            .await
            .context("upload audio")?;
        let db_url = up.url.clone();
        let db_direct = up.direct_path.clone();
        let db_key = up.media_key.to_vec();
        let db_sha = up.file_sha256.to_vec();
        let db_enc = up.file_enc_sha256.to_vec();
        let db_len = up.file_length;
        let msg = whatsapp_rust::media::audio_message(
            up,
            whatsapp_rust::media::AudioOptions {
                mimetype: Some("audio/ogg; codecs=opus".into()),
                duration_seconds: Some(seconds),
                ptt: Some(true),
                waveform: Some(waveform),
                ..Default::default()
            },
        );
        let to = Self::parse_recipient(recipient)?;
        let res = client.send_message(to.clone(), msg).await?;
        let ts = now_unix();
        let chat = to.to_string();
        let me = self.own_sender();
        // Empty name: the CASE merge in upsert_chat keeps the stored name.
        let _ = self.store.upsert_chat(&chat, "", ts);
        let _ = self.store.upsert_message(&StoredMessage {
            id: res.message_id.clone(),
            chat_jid: chat,
            sender: me,
            sender_name: None,
            content: String::new(),
            timestamp: ts,
            is_from_me: true,
            media_type: Some("audio".into()),
            filename: Some(
                media_path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "audio.ogg".into()),
            ),
            url: Some(db_url),
            direct_path: Some(db_direct),
            media_key: Some(db_key),
            file_sha256: Some(db_sha),
            file_enc_sha256: Some(db_enc),
            file_length: Some(db_len as i64),
            mimetype: Some("audio/ogg; codecs=opus".into()),
        });
        Ok(format!("Message sent to {recipient}"))
    }

    // ---- download ----

    pub async fn download_media(
        &self,
        message_id: &str,
        chat_jid: &str,
    ) -> Result<std::path::PathBuf> {
        let media = self
            .store
            .get_media(message_id, chat_jid)?
            .context("not a media message")?;
        let wa_type = match media.media_type.as_str() {
            "image" => whatsapp_rust::download::MediaType::Image,
            "video" => whatsapp_rust::download::MediaType::Video,
            "audio" => whatsapp_rust::download::MediaType::Audio,
            "document" => whatsapp_rust::download::MediaType::Document,
            "sticker" => whatsapp_rust::download::MediaType::Sticker,
            other => anyhow::bail!("unsupported media type: {other}"),
        };
        let dir = chat_media_dir(&self.data_dir, chat_jid);
        std::fs::create_dir_all(&dir)?;
        // Same behavior as the Go bridge: reuse the file if already downloaded.
        let local = dir.join(&media.filename);
        if local.is_file() {
            return Ok(local);
        }
        // Direct path may be missing on messages stored before it was persisted;
        // fall back to deriving it from the CDN URL (Go bridge behavior).
        let direct_path = match &media.direct_path {
            Some(p) if !p.is_empty() => p.clone(),
            _ => extract_direct_path_from_url(&media.url),
        };
        if media.media_key.is_empty()
            || media.file_sha256.is_empty()
            || media.file_enc_sha256.is_empty()
            || media.file_length == 0
        {
            anyhow::bail!("incomplete media information for download");
        }
        let client = self.require_client()?;
        let params = whatsapp_rust::download::DownloadParams::encrypted(
            direct_path,
            &media.media_key,
            &media.file_sha256,
            &media.file_enc_sha256,
            media.file_length,
            wa_type,
        );
        let bytes = client.download_from_params(&params).await?;
        std::fs::write(&local, bytes)?;
        Ok(local)
    }
}

pub fn extract_direct_path_from_url(url: &str) -> String {
    let Some((_, rest)) = url.split_once(".net/") else {
        return url.to_string();
    };
    let path = rest.split('?').next().unwrap_or(rest);
    format!("/{path}")
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Mirrors Go GetChatName: keep the stored name, else fetch group metadata /
/// contact name, with push_name and JID fallbacks. A "Group <user>" fallback
/// is only returned, never stored: callers filter it before persisting.
async fn group_or_contact_name(
    client: &Arc<Client>,
    store: &Store,
    chat: &Jid,
    chat_jid: &str,
    sender_jid: &str,
    push_name: &str,
) -> String {
    if let Ok(Some(c)) = store.get_chat(chat_jid, false)
        && let Some(n) = c.name
        && !n.is_empty()
    {
        return n;
    }
    if chat.server == Server::Group {
        match client.groups().get_metadata(chat).await {
            Ok(meta) if !meta.subject.is_empty() => return meta.subject,
            _ => return format!("Group {}", chat.user),
        }
    }
    if !push_name.is_empty() {
        return push_name.to_string();
    }
    if !sender_jid.is_empty() {
        return sender_jid.to_string();
    }
    chat_jid.to_string()
}

/// Display name carried by a history-sync Conversation payload.
fn conversation_display_name(conv: &wa::Conversation) -> Option<String> {
    conv.name
        .clone()
        .filter(|n| !n.is_empty())
        .or_else(|| conv.display_name.clone().filter(|n| !n.is_empty()))
}

fn is_placeholder_name(name: &str) -> bool {
    name.starts_with("Group ")
}

fn classify_media(path: &std::path::Path) -> (whatsapp_rust::download::MediaType, &'static str) {
    use whatsapp_rust::download::MediaType;
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .as_deref()
    {
        Some("jpg") | Some("jpeg") => (MediaType::Image, "image/jpeg"),
        Some("png") => (MediaType::Image, "image/png"),
        Some("gif") => (MediaType::Image, "image/gif"),
        Some("webp") => (MediaType::Image, "image/webp"),
        Some("ogg") | Some("oga") => (MediaType::Audio, "audio/ogg; codecs=opus"),
        Some("mp3") => (MediaType::Audio, "audio/mpeg"),
        Some("mp4") => (MediaType::Video, "video/mp4"),
        Some("mov") => (MediaType::Video, "video/quicktime"),
        _ => (MediaType::Document, "application/octet-stream"),
    }
}

fn build_media_message(
    media_type: whatsapp_rust::download::MediaType,
    up: whatsapp_rust::upload::UploadResponse,
    mime: &str,
    caption: Option<&str>,
    path: &std::path::Path,
) -> wa::Message {
    use whatsapp_rust::download::MediaType;
    use whatsapp_rust::media::*;
    match media_type {
        MediaType::Image => image_message(
            up,
            ImageOptions {
                caption: caption.map(|c| c.to_string()),
                mimetype: Some(mime.to_string()),
                ..Default::default()
            },
        ),
        MediaType::Video => video_message(
            up,
            VideoOptions {
                caption: caption.map(|c| c.to_string()),
                mimetype: Some(mime.to_string()),
                ..Default::default()
            },
        ),
        MediaType::Audio => audio_message(
            up,
            AudioOptions {
                mimetype: Some(mime.to_string()),
                ptt: Some(false),
                ..Default::default()
            },
        ),
        _ => document_message(
            up,
            DocumentOptions {
                mimetype: Some(mime.to_string()),
                file_name: path.file_name().map(|n| n.to_string_lossy().into_owned()),
                caption: caption.map(|c| c.to_string()),
                ..Default::default()
            },
        ),
    }
}
