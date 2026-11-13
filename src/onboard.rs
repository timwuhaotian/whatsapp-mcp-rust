use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use tokio::signal;

use crate::config::AppConfig;
use crate::mcp::tool_names;
use crate::wa::Wa;

// login and serve share Wa::connect_with, so pairing-time history lands in
// the store exactly like serve-time history does. login only adds pair-code
// config, stdout QR rendering (serve must keep stdout clean for MCP), and
// waits for pairing to complete.
pub async fn login(cfg: &AppConfig, phone: Option<String>, full_sync: bool) -> Result<()> {
    let wa = Wa::open(cfg)?;
    wa.connect_with(
        cfg,
        crate::wa::ConnectOptions {
            phone,
            full_sync,
            on_qr_stdout: true,
        },
    )
    .await?;
    let client = wa
        .client
        .read()
        .unwrap()
        .clone()
        .context("bot did not start")?;
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
    // Extra grace period for the history sync to arrive and be ingested.
    let _ = tokio::time::timeout(std::time::Duration::from_secs(120), signal::ctrl_c()).await;
    client.disconnect().await;
    let (_, messages) = wa.store.counts().unwrap_or((0, 0));
    eprintln!("login done: {messages} messages stored. Run `doctor` to verify.");
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

/// Static by default: session-file presence + local counts + ffmpeg. A live
/// connection check steals the session (StreamReplace) and would kill a
/// running serve instance, so it is opt-in via --connect and must only run
/// with the MCP client stopped.
pub async fn doctor(cfg: &AppConfig, live: bool) -> Result<()> {
    let exe = AppConfig::exe_path()?;
    let wa = Wa::open(cfg)?;
    let paired = cfg.whatsapp_db().is_file();
    let (live_paired, connected) = if live {
        match wa.connect(cfg).await {
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
        }
    } else {
        (false, false)
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
    println!("  paired (session file): {paired}");
    if live {
        println!("  live check: paired={live_paired} connected={connected}");
    } else {
        println!("  live check: skipped (use --connect with the MCP client stopped)");
    }
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
    use rusqlite::Connection;
    let src =
        Connection::open(from).with_context(|| format!("open legacy db {}", from.display()))?;
    let dst = Arc::new(Wa::open(cfg)?);
    let mut chats: i64 = 0;
    let mut messages: i64 = 0;
    let mut stmt = src.prepare("SELECT jid, name, last_message_time FROM chats")?;
    let rows: Vec<(String, Option<String>, Option<LegacyTs>)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<std::result::Result<_, _>>()?;
    for (jid, name, ts) in rows {
        let unix = ts.map(|t| t.0).unwrap_or(0);
        let jid = normalize_legacy_jid(&jid);
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
        Option<LegacyTs>,
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
        let unix = r.4.map(|t| t.0).unwrap_or(0);
        dst.store.upsert_message(&crate::store::StoredMessage {
            id: r.0,
            chat_jid: normalize_legacy_jid(&r.1),
            // Legacy senders are bare numbers; the new store keys on full JIDs
            // so suffix queries like `号码@%` keep working after import.
            sender: normalize_legacy_jid(&r.2),
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
    Ok(())
}

/// A TIMESTAMP as read back from a go-sqlite3 database. Values arrive as
/// text in Go's `2006-01-02 15:04:05.999999999-07:00` layout (nanoseconds +
/// numeric zone), as RFC3339, as zone-less datetimes, or as unix integers
/// when the file was produced by another tool.
struct LegacyTs(i64);

impl rusqlite::types::FromSql for LegacyTs {
    fn column_result(value: rusqlite::types::ValueRef<'_>) -> rusqlite::types::FromSqlResult<Self> {
        use rusqlite::types::{FromSqlError, ValueRef};
        let unix = match value {
            ValueRef::Null => 0,
            ValueRef::Integer(i) => i,
            ValueRef::Real(f) => f as i64,
            ValueRef::Text(t) => {
                let s = std::str::from_utf8(t).map_err(|e| FromSqlError::Other(Box::new(e)))?;
                parse_legacy_ts(s).map_err(|e| FromSqlError::Other(e.into()))?
            }
            ValueRef::Blob(_) => {
                return Err(FromSqlError::InvalidType);
            }
        };
        Ok(Self(unix))
    }
}

fn parse_legacy_ts(s: &str) -> Result<i64> {
    use chrono::DateTime;
    // go-sqlite3: "2026-01-02 15:04:05.999999999+08:00" — space separator,
    // up to 9 fractional digits, numeric zone. chrono needs a T and at most
    // 6 fractional digits, so normalize before parsing.
    if let Some(ts) = parse_go_sqlite_ts(s) {
        return Ok(ts);
    }
    if let Ok(d) = DateTime::parse_from_rfc3339(s) {
        return Ok(d.timestamp());
    }
    for fmt in [
        "%Y-%m-%dT%H:%M:%S%.f",
        "%Y-%m-%d %H:%M:%S%.f",
        "%Y-%m-%d %H:%M:%S",
    ] {
        if let Ok(d) = chrono::NaiveDateTime::parse_from_str(s, fmt) {
            return Ok(d.and_utc().timestamp());
        }
    }
    if let Ok(i) = s.trim().parse::<i64>() {
        return Ok(i);
    }
    anyhow::bail!("unrecognized legacy timestamp: {s}")
}

fn parse_go_sqlite_ts(s: &str) -> Option<i64> {
    use chrono::{DateTime, FixedOffset, TimeZone};
    // Split "date time<zone>" — the zone is the trailing +HH:MM/-HH:MM.
    let (head, zone) = split_go_zone(s)?;
    let head = head.replace(' ', "T");
    // Truncate fractional seconds to microseconds (chrono max).
    let head = truncate_frac_to_micros(&head);
    let with_t = format!("{head}{zone}");
    if let Ok(d) = DateTime::parse_from_rfc3339(&with_t) {
        return Some(d.timestamp());
    }
    // Zone-less datetime with fractional part.
    if let Ok(d) = chrono::NaiveDateTime::parse_from_str(&head, "%Y-%m-%dT%H:%M:%S%.f") {
        return Some(d.and_utc().timestamp());
    }
    // Bare offset without colon.
    let zone_colon = format!("{}:{}", &zone[..3], &zone[3..]);
    DateTime::parse_from_rfc3339(&format!("{head}{zone_colon}"))
        .ok()
        .map(|d| d.timestamp())
        .or_else(|| {
            FixedOffset::east_opt(0)?
                .from_local_datetime(
                    &chrono::NaiveDateTime::parse_from_str(&head, "%Y-%m-%dT%H:%M:%S%.f").ok()?,
                )
                .single()
                .map(|d| d.timestamp())
        })
}

fn split_go_zone(s: &str) -> Option<(&str, &str)> {
    let s = s.trim();
    // Trailing Z or +HH:MM/-HH:MM (also accept +HHMM).
    if let Some(stripped) = s.strip_suffix('Z') {
        return Some((stripped, "+00:00"));
    }
    if s.len() >= 6 {
        let tail6 = &s[s.len() - 6..];
        let b = tail6.as_bytes();
        if (b[0] == b'+' || b[0] == b'-')
            && b[3] == b':'
            && b[1].is_ascii_digit()
            && b[2].is_ascii_digit()
            && b[4].is_ascii_digit()
            && b[5].is_ascii_digit()
        {
            return Some((&s[..s.len() - 6], tail6));
        }
    }
    if s.len() >= 5 {
        let tail5 = &s[s.len() - 5..];
        let b = tail5.as_bytes();
        if (b[0] == b'+' || b[0] == b'-') && b[1..].iter().all(|c| c.is_ascii_digit()) {
            return Some((&s[..s.len() - 5], tail5));
        }
    }
    None
}

fn truncate_frac_to_micros(head: &str) -> String {
    let Some(dot) = head.find('.') else {
        return head.to_string();
    };
    let digits = head[dot + 1..].len();
    if digits <= 6 {
        return head.to_string();
    }
    format!("{}.{}", &head[..dot], &head[dot + 1..dot + 7])
}

/// Legacy senders/chats are bare numbers (`sender` column); the new store
/// keys on full JIDs, so qualify them for suffix queries to keep working.
fn normalize_legacy_jid(v: &str) -> String {
    if v.contains('@') || v.is_empty() {
        v.to_string()
    } else {
        format!("{v}@s.whatsapp.net")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_timestamps_parse() {
        // go-sqlite3 layout with nanoseconds + numeric zone.
        assert_eq!(
            parse_legacy_ts("2026-01-02 15:04:05.999999999+08:00").unwrap(),
            1767337445
        );
        assert_eq!(
            parse_legacy_ts("2026-01-02 15:04:05.123+08:00").unwrap(),
            1767337445
        );
        assert_eq!(
            parse_legacy_ts("2024-01-01 10:00:00+00:00").unwrap(),
            1704103200
        );
        assert_eq!(parse_legacy_ts("2024-01-01T10:00:00Z").unwrap(), 1704103200);
        assert_eq!(parse_legacy_ts("2024-01-01 10:00:00").unwrap(), 1704103200);
        assert_eq!(parse_legacy_ts("1704103200").unwrap(), 1704103200);
    }

    #[test]
    fn legacy_jid_normalization() {
        assert_eq!(normalize_legacy_jid("1555"), "1555@s.whatsapp.net");
        assert_eq!(normalize_legacy_jid("a@g.us"), "a@g.us");
    }
}
