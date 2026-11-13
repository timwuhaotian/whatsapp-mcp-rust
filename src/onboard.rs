use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use tokio::signal;

use crate::config::AppConfig;
use crate::mcp::tool_names;
use crate::wa::Wa;

// login needs a Bot with QR/pair-code handlers, which lives behind Wa::connect.
// For onboarding we build the bot here directly so we can print QR to stdout
// (serve mode must keep stdout clean for MCP; login is the exception).
pub async fn login(cfg: &AppConfig, phone: Option<String>, _full_sync: bool) -> Result<()> {
    use whatsapp_rust::pair_code::PairCodeOptions;
    use whatsapp_rust::prelude::*;

    let backend = SqliteStore::new(cfg.whatsapp_db().to_string_lossy().as_ref())
        .await
        .context("open whatsapp session db")?;
    let mut builder = Bot::builder().with_backend(backend);
    if let Some(number) = phone {
        let digits: String = number.chars().filter(|c| c.is_ascii_digit()).collect();
        builder = builder.with_pair_code(PairCodeOptions {
            phone_number: digits,
            ..Default::default()
        });
        // Pair code arrives on stdout; QR still renders if the phone asks for it.
        builder = builder.on_pair_code(|code, timeout| async move {
            println!(
                "Enter this code on your phone (valid {:?}): {}",
                timeout, code
            );
        });
    }
    // QR to stdout (login exception to the stderr rule) + qr.txt for MCP clients.
    let qr_txt = cfg.qr_txt();
    let bot = builder
        .on_qr_code(move |code, timeout| {
            let qr_txt = qr_txt.clone();
            async move {
                println!("Scan this QR code (valid {:?}):\n{}", timeout, code);
                println!(
                    "{}{}",
                    whatsapp_rust::pair::NATIVE_CAMERA_DEEP_LINK_PREFIX,
                    code
                );
                if std::fs::write(&qr_txt, &code).is_ok() {
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt;
                        let _ = std::fs::set_permissions(
                            &qr_txt,
                            std::fs::Permissions::from_mode(0o600),
                        );
                    }
                }
                use qrcode::QrCode;
                use qrcode::render::unicode::Dense1x2;
                if let Ok(qr) = QrCode::new(code.as_bytes()) {
                    println!("{}", qr.render::<Dense1x2>().build());
                }
            }
        })
        .on_connected(|_client| async {
            eprintln!("connected, waiting for history sync... (Ctrl-C to exit)");
        })
        .build()
        .await
        .context("build bot")?;
    let handle = bot.spawn();
    let client = handle.client();
    // Wait until paired+connected, then give history sync time to land.
    let _ = tokio::time::timeout(std::time::Duration::from_secs(300), async {
        loop {
            if client.is_logged_in() && client.is_connected() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        }
    })
    .await;
    // Extra grace period for the RECENT history sync to arrive and be stored.
    let _ = tokio::time::timeout(std::time::Duration::from_secs(120), signal::ctrl_c()).await;
    client.disconnect().await;
    eprintln!("login done. Run `doctor` to verify message counts.");
    Ok(())
}

pub async fn logout(cfg: &AppConfig, purge: bool, yes: bool) -> Result<()> {
    if !yes {
        anyhow::bail!("pass --yes to confirm logout");
    }
    let wa = Wa::open(cfg)?;
    wa.connect(cfg).await?;
    let client = wa.client.read().unwrap().clone();
    if let Some(c) = client {
        c.logout().await;
    }
    std::fs::remove_file(cfg.whatsapp_db()).ok();
    for suffix in ["-journal", "-wal", "-shm"] {
        std::fs::remove_file(cfg.whatsapp_db().with_extension(format!("db{suffix}"))).ok();
    }
    if purge {
        std::fs::remove_file(cfg.messages_db()).ok();
        std::fs::remove_dir_all(cfg.media_dir()).ok();
    }
    eprintln!("logged out{}", if purge { " (data purged)" } else { "" });
    Ok(())
}

pub async fn doctor(cfg: &AppConfig) -> Result<()> {
    let exe = AppConfig::exe_path()?;
    let wa = Wa::open(cfg)?;
    let (paired, connected) = match wa.connect(cfg).await {
        Ok(()) => {
            // Give the socket a moment to settle.
            tokio::time::sleep(std::time::Duration::from_secs(3)).await;
            let c = wa.client.read().unwrap().clone();
            match c {
                Some(c) => {
                    let status = (c.is_logged_in(), c.is_connected());
                    c.disconnect().await;
                    status
                }
                None => (false, false),
            }
        }
        Err(e) => {
            eprintln!("connect skipped: {e}");
            (false, false)
        }
    };
    let (chats, messages) = wa.store.counts().unwrap_or((0, 0));
    let ffmpeg = tokio::process::Command::new("ffmpeg")
        .arg("-version")
        .output()
        .await
        .map(|o| o.status.success())
        .unwrap_or(false);
    println!("whatsapp-mcp doctor");
    println!("  version:    {}", env!("CARGO_PKG_VERSION"));
    println!("  exe:        {}", exe.display());
    println!("  data dir:   {}", cfg.data_dir.display());
    println!("  paired:     {paired}");
    println!("  connected:  {connected}");
    println!("  chats:      {chats}");
    println!("  messages:   {messages}");
    println!(
        "  ffmpeg:     {}",
        if ffmpeg {
            "yes"
        } else {
            "no (send_audio_message needs it; use send_file instead)"
        }
    );
    println!("  tools:      {}", tool_names().len());
    if !paired {
        println!("  next step:  whatsapp-mcp login");
    }
    Ok(())
}

pub async fn config(cfg: &AppConfig, write: Option<String>) -> Result<()> {
    let exe = AppConfig::exe_path()?;
    let json = serde_json::to_string_pretty(&serde_json::json!({
        "mcpServers": {
            "whatsapp": {
                "command": exe.to_string_lossy(),
                "args": ["--data-dir", cfg.data_dir.to_string_lossy()],
            }
        }
    }))?;
    match write {
        None => {
            println!("{json}");
            Ok(())
        }
        Some(target) => {
            let path: PathBuf = match target.as_str() {
                "claude" => dirs::home_dir()
                    .context("no home dir")?
                    .join("Library/Application Support/Claude/claude_desktop_config.json"),
                "cursor" => dirs::home_dir()
                    .context("no home dir")?
                    .join(".cursor/mcp.json"),
                other => PathBuf::from(other),
            };
            merge_config(&path, &json)
        }
    }
}

fn merge_config(path: &Path, snippet: &str) -> Result<()> {
    let snippet_v: serde_json::Value = serde_json::from_str(snippet)?;
    let mut base: serde_json::Value = if path.is_file() {
        let bak = path.with_extension("json.bak");
        std::fs::copy(path, &bak).ok();
        serde_json::from_str(&std::fs::read_to_string(path)?).unwrap_or(serde_json::json!({}))
    } else {
        serde_json::json!({})
    };
    if let (Some(b), Some(s)) = (base.get_mut("mcpServers"), snippet_v.get("mcpServers")) {
        if let (Some(bm), Some(sm)) = (b.as_object_mut(), s.as_object()) {
            for (k, v) in sm {
                bm.insert(k.clone(), v.clone());
            }
        }
    } else if let Some(map) = base.as_object_mut()
        && let Some(servers) = snippet_v.get("mcpServers")
    {
        map.insert("mcpServers".into(), servers.clone());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, serde_json::to_string_pretty(&base)?)?;
    println!("wrote {}", path.display());
    Ok(())
}

pub async fn import_legacy(cfg: &AppConfig, from: &Path) -> Result<()> {
    use rusqlite::{Connection, params};
    let src =
        Connection::open(from).with_context(|| format!("open legacy db {}", from.display()))?;
    let dst = Arc::new(Wa::open(cfg)?);
    let mut chats: i64 = 0;
    let mut messages: i64 = 0;
    let mut stmt = src.prepare("SELECT jid, name, last_message_time FROM chats")?;
    let rows: Vec<(String, Option<String>, Option<String>)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<std::result::Result<_, _>>()?;
    for (jid, name, ts) in rows {
        let unix = ts
            .as_deref()
            .and_then(|s| {
                chrono::DateTime::parse_from_rfc3339(s)
                    .map(|d| d.timestamp())
                    .ok()
                    .or_else(|| {
                        chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S%.f")
                            .map(|d| d.and_utc().timestamp())
                            .ok()
                    })
            })
            .unwrap_or(0);
        dst.store
            .upsert_chat(&jid, name.as_deref().unwrap_or(""), unix)?;
        chats += 1;
    }
    let mut stmt = src.prepare(
        "SELECT id, chat_jid, sender, content, timestamp, is_from_me,
                media_type, filename, url, direct_path, media_key,
                file_sha256, file_enc_sha256, file_length
         FROM messages",
    )?;
    #[allow(clippy::type_complexity)]
    let rows: Vec<(
        String,
        String,
        String,
        Option<String>,
        Option<String>,
        Option<i64>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<Vec<u8>>,
        Option<Vec<u8>>,
        Option<Vec<u8>>,
        Option<i64>,
    )> = stmt
        .query_map([], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
                r.get(6)?,
                r.get(7)?,
                r.get(8)?,
                r.get(9)?,
                r.get(10)?,
                r.get(11)?,
                r.get(12)?,
                r.get(13)?,
            ))
        })?
        .collect::<std::result::Result<_, _>>()?;
    for r in rows {
        let unix =
            r.4.as_deref()
                .and_then(|s| {
                    chrono::DateTime::parse_from_rfc3339(s)
                        .map(|d| d.timestamp())
                        .ok()
                        .or_else(|| {
                            chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S%.f")
                                .map(|d| d.and_utc().timestamp())
                                .ok()
                        })
                })
                .unwrap_or(0);
        dst.store.upsert_message(&crate::store::StoredMessage {
            id: r.0,
            chat_jid: r.1,
            sender: r.2,
            sender_name: None,
            content: r.3.unwrap_or_default(),
            timestamp: unix,
            is_from_me: r.5.unwrap_or(0) != 0,
            media_type: r.6,
            filename: r.7,
            url: r.8,
            direct_path: r.9,
            media_key: r.10,
            file_sha256: r.11,
            file_enc_sha256: r.12,
            file_length: r.13,
            mimetype: None,
        })?;
        messages += 1;
    }
    // Legacy whatsapp.db sessions can't be reused (whatsmeow vs whatsapp-rust
    // storage differ): the user must `login` again to re-pair.
    println!(
        "imported {chats} chats, {messages} messages from {}",
        from.display()
    );
    println!("note: sessions can't be migrated; run `whatsapp-mcp login` to re-pair.");
    let _ = params![];
    Ok(())
}
