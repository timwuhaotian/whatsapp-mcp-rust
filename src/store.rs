use std::collections::HashSet;
use std::path::Path;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, params};

use crate::types::{Chat, Contact, Message, MessageContextDto};

pub struct MediaRow {
    pub media_type: String,
    pub filename: String,
    pub url: String,
    pub direct_path: Option<String>,
    pub media_key: Vec<u8>,
    pub file_sha256: Vec<u8>,
    pub file_enc_sha256: Vec<u8>,
    pub file_length: u64,
}

#[derive(Clone)]
pub struct Store {
    conn: Arc<Mutex<Connection>>,
    has_fts: bool,
}

pub struct StoredMessage {
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
    pub media_key: Option<Vec<u8>>,
    pub file_sha256: Option<Vec<u8>>,
    pub file_enc_sha256: Option<Vec<u8>>,
    pub file_length: Option<i64>,
    pub mimetype: Option<String>,
}

#[derive(Default)]
pub struct ListMessagesQuery {
    pub after: Option<i64>,
    pub before: Option<i64>,
    pub sender: Option<String>,
    pub chat_jid: Option<String>,
    pub query: Option<String>,
    pub limit: i64,
    pub page: i64,
    pub include_context: bool,
    pub context_before: i64,
    pub context_after: i64,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create db dir {}", parent.display()))?;
        }
        let conn = Connection::open(path)
            .with_context(|| format!("open messages db {}", path.display()))?;
        Self::init(conn)
    }

    #[cfg(test)]
    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self> {
        conn.execute_batch("PRAGMA journal_mode=WAL;")?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS chats (
                jid TEXT PRIMARY KEY,
                name TEXT,
                last_message_time INTEGER
            );
            CREATE TABLE IF NOT EXISTS messages (
                id TEXT,
                chat_jid TEXT,
                sender TEXT,
                sender_name TEXT,
                content TEXT,
                timestamp INTEGER,
                is_from_me INTEGER,
                media_type TEXT,
                filename TEXT,
                url TEXT,
                direct_path TEXT,
                media_key BLOB,
                file_sha256 BLOB,
                file_enc_sha256 BLOB,
                file_length INTEGER,
                mimetype TEXT,
                PRIMARY KEY (id, chat_jid)
            );
            CREATE INDEX IF NOT EXISTS idx_messages_chat_ts ON messages (chat_jid, timestamp);
            CREATE INDEX IF NOT EXISTS idx_messages_ts ON messages (timestamp);",
        )?;
        // FTS5 is preferred for content search; if the bundled SQLite lacks it
        // (or creation fails), every query falls back to LIKE automatically.
        let has_fts = conn
            .execute_batch(
                "CREATE VIRTUAL TABLE IF NOT EXISTS messages_fts
                 USING fts5(content, chat_jid, tokenize='unicode61');",
            )
            .is_ok();
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            has_fts,
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().expect("messages db mutex poisoned")
    }

    pub fn upsert_chat(&self, jid: &str, name: &str, last_message_time: i64) -> Result<()> {
        self.lock().execute(
            "INSERT INTO chats (jid, name, last_message_time) VALUES (?, ?, ?)
             ON CONFLICT(jid) DO UPDATE SET
               name = CASE WHEN excluded.name <> '' THEN excluded.name ELSE chats.name END,
               last_message_time = max(chats.last_message_time, excluded.last_message_time)",
            params![jid, name, last_message_time],
        )?;
        Ok(())
    }

    pub fn upsert_message(&self, m: &StoredMessage) -> Result<()> {
        if m.content.is_empty() && m.media_type.as_deref().unwrap_or("").is_empty() {
            return Ok(());
        }
        let conn = self.lock();
        conn.execute(
            "INSERT INTO messages
             (id, chat_jid, sender, sender_name, content, timestamp, is_from_me,
              media_type, filename, url, direct_path, media_key, file_sha256,
              file_enc_sha256, file_length, mimetype)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(id, chat_jid) DO UPDATE SET
               sender=excluded.sender, sender_name=excluded.sender_name,
               content=excluded.content, timestamp=excluded.timestamp,
               is_from_me=excluded.is_from_me, media_type=excluded.media_type,
               filename=excluded.filename, url=excluded.url,
               direct_path=excluded.direct_path, media_key=excluded.media_key,
               file_sha256=excluded.file_sha256,
               file_enc_sha256=excluded.file_enc_sha256,
               file_length=excluded.file_length, mimetype=excluded.mimetype",
            params![
                m.id,
                m.chat_jid,
                m.sender,
                m.sender_name,
                m.content,
                m.timestamp,
                m.is_from_me as i32,
                m.media_type,
                m.filename,
                m.url,
                m.direct_path,
                m.media_key,
                m.file_sha256,
                m.file_enc_sha256,
                m.file_length,
                m.mimetype,
            ],
        )?;
        if self.has_fts {
            let rowid: Option<i64> = conn
                .query_row(
                    "SELECT rowid FROM messages WHERE id = ? AND chat_jid = ?",
                    params![m.id, m.chat_jid],
                    |r| r.get(0),
                )
                .optional()?;
            if let Some(rowid) = rowid {
                let _ = conn.execute("DELETE FROM messages_fts WHERE rowid = ?", params![rowid]);
                let _ = conn.execute(
                    "INSERT INTO messages_fts (rowid, content, chat_jid) VALUES (?, ?, ?)",
                    params![rowid, m.content, m.chat_jid],
                );
            }
        }
        Ok(())
    }

    fn row_to_message(row: &rusqlite::Row) -> rusqlite::Result<Message> {
        Ok(Message {
            id: row.get("id")?,
            chat_jid: row.get("chat_jid")?,
            sender: row.get("sender")?,
            sender_name: row.get("sender_name")?,
            content: row.get("content")?,
            timestamp: row.get("timestamp")?,
            is_from_me: row.get::<_, i32>("is_from_me")? != 0,
            media_type: row.get("media_type")?,
            filename: row.get("filename")?,
            url: row.get("url")?,
            direct_path: row.get("direct_path")?,
            mimetype: row.get("mimetype")?,
            file_length: row.get("file_length")?,
        })
    }

    const MSG_COLS: &'static str = "id, chat_jid, sender, sender_name, content, timestamp, is_from_me, media_type, filename, url, direct_path, mimetype, file_length";

    pub fn list_messages(&self, q: &ListMessagesQuery) -> Result<Vec<Message>> {
        let limit = q.limit.max(0);
        let offset = (q.page.max(0) * limit).max(0);
        let mut sql = format!("SELECT {} FROM messages", Self::MSG_COLS);
        let mut clauses: Vec<String> = Vec::new();
        let mut args: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
        let push = |args: &mut Vec<Box<dyn rusqlite::ToSql>>, v: String| {
            args.push(Box::new(v));
        };

        if let Some(after) = q.after {
            clauses.push("timestamp > ?".into());
            args.push(Box::new(after));
        }
        if let Some(before) = q.before {
            clauses.push("timestamp < ?".into());
            args.push(Box::new(before));
        }
        if let Some(sender) = &q.sender {
            // sender stores the full JID now; accept a bare number or a full JID.
            let phone = sender.split('@').next().unwrap_or(sender);
            clauses.push("(sender = ? OR sender LIKE ?)".into());
            push(&mut args, sender.clone());
            push(&mut args, format!("{phone}@%"));
        }
        if let Some(chat) = &q.chat_jid {
            clauses.push("chat_jid = ?".into());
            push(&mut args, chat.clone());
        }
        let mut use_fts = false;
        if let Some(query) = &q.query {
            if self.has_fts && !query.trim().is_empty() {
                // Token-prefix FTS (fast) UNION substring LIKE (parity with the
                // original `%query%` semantics: FTS phrase MATCH is whole-word
                // and would miss e.g. "meet" in "meeting").
                let prefix = query
                    .split_whitespace()
                    .map(|t| format!("{}*", t.replace('"', "\"\"")))
                    .collect::<Vec<_>>()
                    .join(" ");
                clauses.push(
                    "(rowid IN (SELECT rowid FROM messages_fts WHERE messages_fts MATCH ?) OR LOWER(content) LIKE LOWER(?))".into(),
                );
                push(&mut args, prefix);
                push(&mut args, format!("%{query}%"));
                use_fts = true;
            } else {
                clauses.push("LOWER(content) LIKE LOWER(?)".into());
                push(&mut args, format!("%{query}%"));
            }
        }
        if !clauses.is_empty() {
            sql.push_str(" WHERE ");
            sql.push_str(&clauses.join(" AND "));
        }
        sql.push_str(" ORDER BY timestamp DESC, rowid DESC LIMIT ? OFFSET ?");
        args.push(Box::new(limit));
        args.push(Box::new(offset));
        let refs: Vec<&dyn rusqlite::ToSql> = args.iter().map(|b| b.as_ref()).collect();

        let run = |sql: &str, refs: &[&dyn rusqlite::ToSql]| -> Result<Vec<Message>> {
            let conn = self.lock();
            let mut stmt = conn.prepare(sql)?;
            let rows = stmt.query_map(refs, Self::row_to_message)?;
            rows.collect::<std::result::Result<Vec<_>, _>>()
                .context("read messages")
        };

        let matched = match run(&sql, &refs) {
            Ok(v) => v,
            Err(_) if use_fts => {
                // FTS failed (e.g. query too long): fall back to pure LIKE.
                let mut sql2 = format!("SELECT {} FROM messages", Self::MSG_COLS);
                let mut c2: Vec<String> = clauses
                    .into_iter()
                    .filter(|c| !c.contains("messages_fts"))
                    .collect();
                c2.push("LOWER(content) LIKE LOWER(?)".into());
                sql2.push_str(" WHERE ");
                sql2.push_str(&c2.join(" AND "));
                sql2.push_str(" ORDER BY timestamp DESC, rowid DESC LIMIT ? OFFSET ?");
                let mut args2: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
                if let Some(after) = q.after {
                    args2.push(Box::new(after));
                }
                if let Some(before) = q.before {
                    args2.push(Box::new(before));
                }
                if let Some(sender) = &q.sender {
                    let phone = sender.split('@').next().unwrap_or(sender);
                    args2.push(Box::new(sender.clone()));
                    args2.push(Box::new(format!("{phone}@%")));
                }
                if let Some(chat) = &q.chat_jid {
                    args2.push(Box::new(chat.clone()));
                }
                args2.push(Box::new(format!("%{}%", q.query.as_deref().unwrap_or(""))));
                args2.push(Box::new(limit));
                args2.push(Box::new(offset));
                let refs2: Vec<&dyn rusqlite::ToSql> = args2.iter().map(|b| b.as_ref()).collect();
                run(&sql2, &refs2)?
            }
            Err(e) => return Err(e),
        };

        if !q.include_context || matched.is_empty() {
            return Ok(matched);
        }
        // Attach context windows, newest match first (original order), with
        // dedup so overlapping windows don't repeat messages.
        let mut seen: HashSet<(String, String)> = HashSet::new();
        let mut out = Vec::new();
        for m in &matched {
            let ctx = self.context_in_chat(
                &m.chat_jid,
                m.timestamp,
                &m.id,
                q.context_before,
                q.context_after,
            )?;
            for b in ctx.before {
                if seen.insert((b.chat_jid.clone(), b.id.clone())) {
                    out.push(b);
                }
            }
            if seen.insert((m.chat_jid.clone(), m.id.clone())) {
                out.push(m.clone());
            }
            for a in ctx.after {
                if seen.insert((a.chat_jid.clone(), a.id.clone())) {
                    out.push(a);
                }
            }
        }
        Ok(out)
    }

    fn context_in_chat(
        &self,
        chat_jid: &str,
        ts: i64,
        id: &str,
        before_n: i64,
        after_n: i64,
    ) -> Result<MessageContextDto> {
        let conn = self.lock();
        let target: Message = conn
            .query_row(
                &format!(
                    "SELECT {} FROM messages WHERE chat_jid = ? AND id = ?",
                    Self::MSG_COLS
                ),
                params![chat_jid, id],
                Self::row_to_message,
            )
            .with_context(|| format!("message {id} not found"))?;
        let mut stmt = conn.prepare(&format!(
            "SELECT {} FROM messages WHERE chat_jid = ? AND timestamp < ?
             ORDER BY timestamp DESC, rowid DESC LIMIT ?",
            Self::MSG_COLS
        ))?;
        let mut before: Vec<Message> = stmt
            .query_map(params![chat_jid, ts, before_n.max(0)], Self::row_to_message)?
            .collect::<std::result::Result<_, _>>()?;
        // Original code returned `before` newest-first; fix to ascending so the
        // combined before+message+after list reads chronologically.
        before.reverse();
        let mut stmt = conn.prepare(&format!(
            "SELECT {} FROM messages WHERE chat_jid = ? AND timestamp > ?
             ORDER BY timestamp ASC, rowid ASC LIMIT ?",
            Self::MSG_COLS
        ))?;
        let after: Vec<Message> = stmt
            .query_map(params![chat_jid, ts, after_n.max(0)], Self::row_to_message)?
            .collect::<std::result::Result<_, _>>()?;
        Ok(MessageContextDto {
            message: target,
            before,
            after,
        })
    }

    pub fn get_message_context(
        &self,
        message_id: &str,
        before: i64,
        after: i64,
    ) -> Result<MessageContextDto> {
        let (chat_jid, ts): (String, i64) = self
            .lock()
            .query_row(
                "SELECT chat_jid, timestamp FROM messages WHERE id = ? LIMIT 1",
                params![message_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .with_context(|| format!("message {message_id} not found"))?;
        self.context_in_chat(&chat_jid, ts, message_id, before, after)
    }

    fn row_to_chat(include_last: bool) -> impl Fn(&rusqlite::Row) -> rusqlite::Result<Chat> {
        move |row| {
            Ok(Chat {
                jid: row.get("jid")?,
                name: row.get("name")?,
                last_message_time: row.get("last_message_time")?,
                last_message: if include_last {
                    row.get("last_message")?
                } else {
                    None
                },
                last_sender: if include_last {
                    row.get("last_sender")?
                } else {
                    None
                },
                last_is_from_me: if include_last {
                    row.get::<_, Option<i32>>("last_is_from_me")?
                        .map(|v| v != 0)
                } else {
                    None
                },
            })
        }
    }

    pub fn list_chats(
        &self,
        query: Option<&str>,
        limit: i64,
        page: i64,
        include_last_message: bool,
        sort_by: &str,
    ) -> Result<Vec<Chat>> {
        let order = if sort_by == "name" {
            "chats.name"
        } else {
            "chats.last_message_time DESC"
        };
        let mut sql = String::from(
            "SELECT chats.jid AS jid, chats.name AS name,
                    chats.last_message_time AS last_message_time",
        );
        if include_last_message {
            sql.push_str(
                ", messages.content AS last_message, messages.sender AS last_sender,
                  messages.is_from_me AS last_is_from_me
                 FROM chats LEFT JOIN messages ON chats.jid = messages.chat_jid
                   AND chats.last_message_time = messages.timestamp",
            );
        } else {
            sql.push_str(" FROM chats");
        }
        let mut args: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
        if let Some(q) = query {
            sql.push_str(" WHERE (LOWER(chats.name) LIKE LOWER(?) OR chats.jid LIKE ?)");
            args.push(Box::new(format!("%{q}%")));
            args.push(Box::new(format!("%{q}%")));
        }
        sql.push_str(&format!(" ORDER BY {order} LIMIT ? OFFSET ?"));
        let limit = limit.max(0);
        args.push(Box::new(limit));
        args.push(Box::new((page.max(0) * limit).max(0)));
        let refs: Vec<&dyn rusqlite::ToSql> = args.iter().map(|b| b.as_ref()).collect();
        let conn = self.lock();
        let mut stmt = conn.prepare(&sql)?;
        stmt.query_map(&refs[..], Self::row_to_chat(include_last_message))?
            .collect::<std::result::Result<Vec<_>, _>>()
            .context("list chats")
    }

    pub fn search_contacts(&self, query: &str) -> Result<Vec<Contact>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT DISTINCT jid, name FROM chats
             WHERE (LOWER(name) LIKE LOWER(?) OR LOWER(jid) LIKE LOWER(?))
               AND jid NOT LIKE '%@g.us'
             ORDER BY name, jid LIMIT 50",
        )?;
        let pat = format!("%{query}%");
        stmt.query_map(params![pat, pat], |row| {
            let jid: String = row.get(0)?;
            Ok(Contact {
                phone_number: jid.split('@').next().unwrap_or(&jid).to_string(),
                name: row.get(1)?,
                jid,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()
        .context("search contacts")
    }

    pub fn get_contact_chats(&self, jid: &str, limit: i64, page: i64) -> Result<Vec<Chat>> {
        let phone = jid.split('@').next().unwrap_or(jid);
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT DISTINCT c.jid AS jid, c.name AS name,
                    c.last_message_time AS last_message_time,
                    m.content AS last_message, m.sender AS last_sender,
                    m.is_from_me AS last_is_from_me
             FROM chats c
             LEFT JOIN messages m ON c.jid = m.chat_jid
               AND c.last_message_time = m.timestamp
             WHERE EXISTS (SELECT 1 FROM messages m2 WHERE m2.chat_jid = c.jid
                             AND (m2.sender = ? OR m2.sender LIKE ?))
                OR c.jid = ?
             ORDER BY c.last_message_time DESC LIMIT ? OFFSET ?",
        )?;
        let limit = limit.max(0);
        stmt.query_map(
            params![
                jid,
                format!("{phone}@%"),
                jid,
                limit,
                (page.max(0) * limit).max(0)
            ],
            Self::row_to_chat(true),
        )?
        .collect::<std::result::Result<Vec<_>, _>>()
        .context("contact chats")
    }

    pub fn get_last_interaction(&self, jid: &str) -> Result<Option<Message>> {
        let phone = jid.split('@').next().unwrap_or(jid);
        let conn = self.lock();
        conn.query_row(
            &format!(
                "SELECT {} FROM messages m WHERE m.sender = ? OR m.sender LIKE ?
                 OR m.chat_jid = ? ORDER BY m.timestamp DESC LIMIT 1",
                Self::MSG_COLS
                    .split(", ")
                    .map(|c| format!("m.{c}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            params![jid, format!("{phone}@%"), jid],
            Self::row_to_message,
        )
        .optional()
        .context("last interaction")
    }

    /// Full media columns for one message; None when the id/chat is unknown
    /// or the message has no media.
    pub fn get_media(&self, message_id: &str, chat_jid: &str) -> Result<Option<MediaRow>> {
        self.lock()
            .query_row(
                "SELECT media_type, filename, url, direct_path, media_key,
                        file_sha256, file_enc_sha256, file_length
                 FROM messages WHERE id = ? AND chat_jid = ?",
                params![message_id, chat_jid],
                |r| {
                    Ok(MediaRow {
                        media_type: r.get::<_, Option<String>>(0)?.unwrap_or_default(),
                        filename: r.get::<_, Option<String>>(1)?.unwrap_or_default(),
                        url: r.get::<_, Option<String>>(2)?.unwrap_or_default(),
                        direct_path: r.get(3)?,
                        media_key: r.get::<_, Option<Vec<u8>>>(4)?.unwrap_or_default(),
                        file_sha256: r.get::<_, Option<Vec<u8>>>(5)?.unwrap_or_default(),
                        file_enc_sha256: r.get::<_, Option<Vec<u8>>>(6)?.unwrap_or_default(),
                        file_length: r.get::<_, Option<i64>>(7)?.unwrap_or(0).max(0) as u64,
                    })
                },
            )
            .optional()
            .context("get media")
            .map(|row| {
                row.filter(|m| {
                    !m.media_type.is_empty() && !m.filename.is_empty() && !m.url.is_empty()
                })
            })
    }

    pub fn get_chat(&self, chat_jid: &str, include_last_message: bool) -> Result<Option<Chat>> {
        let mut sql = String::from(
            "SELECT c.jid AS jid, c.name AS name,
                    c.last_message_time AS last_message_time",
        );
        if include_last_message {
            sql.push_str(
                ", m.content AS last_message, m.sender AS last_sender,
                  m.is_from_me AS last_is_from_me
                 FROM chats c LEFT JOIN messages m ON c.jid = m.chat_jid
                   AND c.last_message_time = m.timestamp",
            );
        } else {
            sql.push_str(" FROM chats c");
        }
        sql.push_str(" WHERE c.jid = ?");
        self.lock()
            .query_row(
                &sql,
                params![chat_jid],
                Self::row_to_chat(include_last_message),
            )
            .optional()
            .context("get chat")
    }

    pub fn get_direct_chat_by_contact(&self, sender_phone_number: &str) -> Result<Option<Chat>> {
        self.lock()
            .query_row(
                "SELECT c.jid AS jid, c.name AS name, c.last_message_time AS last_message_time,
                        m.content AS last_message, m.sender AS last_sender,
                        m.is_from_me AS last_is_from_me
                 FROM chats c LEFT JOIN messages m ON c.jid = m.chat_jid
                   AND c.last_message_time = m.timestamp
                 WHERE c.jid LIKE ? AND c.jid NOT LIKE '%@g.us' LIMIT 1",
                params![format!("%{sender_phone_number}%")],
                Self::row_to_chat(true),
            )
            .optional()
            .context("direct chat")
    }

    #[allow(dead_code)]
    pub fn get_sender_name(&self, sender_jid: &str) -> String {
        let conn = self.lock();
        if let Ok(Some(n)) = conn.query_row(
            "SELECT name FROM chats WHERE jid = ? LIMIT 1",
            params![sender_jid],
            |r| r.get::<_, Option<String>>(0),
        ) && !n.is_empty()
        {
            return n;
        }
        let phone = sender_jid.split('@').next().unwrap_or(sender_jid);
        if let Ok(Some(n)) = conn.query_row(
            "SELECT name FROM chats WHERE jid LIKE ? LIMIT 1",
            params![format!("%{phone}%")],
            |r| r.get::<_, Option<String>>(0),
        ) && !n.is_empty()
        {
            return n;
        }
        sender_jid.to_string()
    }

    pub fn counts(&self) -> Result<(i64, i64)> {
        let conn = self.lock();
        let chats: i64 = conn.query_row("SELECT COUNT(*) FROM chats", [], |r| r.get(0))?;
        let messages: i64 = conn.query_row("SELECT COUNT(*) FROM messages", [], |r| r.get(0))?;
        Ok((chats, messages))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seeded() -> Store {
        let s = Store::open_in_memory().unwrap();
        for (jid, name, ts) in [
            ("alice@s.whatsapp.net", "Alice", 200),
            ("bob@s.whatsapp.net", "Bob", 300),
            ("group@g.us", "Study Group", 600),
        ] {
            s.upsert_chat(jid, name, ts).unwrap();
        }
        let msg =
            |id: &str, chat: &str, sender: &str, content: &str, ts: i64, me: bool| StoredMessage {
                id: id.into(),
                chat_jid: chat.into(),
                sender: sender.into(),
                sender_name: None,
                content: content.into(),
                timestamp: ts,
                is_from_me: me,
                media_type: None,
                filename: None,
                url: None,
                direct_path: None,
                media_key: None,
                file_sha256: None,
                file_enc_sha256: None,
                file_length: None,
                mimetype: None,
            };
        s.upsert_message(&msg(
            "m1",
            "alice@s.whatsapp.net",
            "alice@s.whatsapp.net",
            "hello alice",
            100,
            false,
        ))
        .unwrap();
        s.upsert_message(&msg(
            "m2",
            "alice@s.whatsapp.net",
            "me@s.whatsapp.net",
            "hi Alice, how are you",
            200,
            true,
        ))
        .unwrap();
        s.upsert_message(&msg(
            "m3",
            "bob@s.whatsapp.net",
            "bob@lid",
            "project update",
            300,
            false,
        ))
        .unwrap();
        s.upsert_message(&msg(
            "m4",
            "group@g.us",
            "alice@s.whatsapp.net",
            "meeting at noon",
            400,
            false,
        ))
        .unwrap();
        s.upsert_message(&msg(
            "m5",
            "group@g.us",
            "bob@lid",
            "see you there",
            500,
            false,
        ))
        .unwrap();
        s.upsert_message(&msg(
            "m6",
            "group@g.us",
            "bob@lid",
            "weekend meetup",
            600,
            false,
        ))
        .unwrap();
        s
    }

    #[test]
    fn filter_by_chat_and_order_desc() {
        let s = seeded();
        let v = s
            .list_messages(&ListMessagesQuery {
                chat_jid: Some("group@g.us".into()),
                limit: 10,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(v.len(), 3);
        assert_eq!(v[0].id, "m6");
        assert_eq!(v[1].id, "m5");
        assert_eq!(v[2].id, "m4");
    }

    #[test]
    fn sender_matches_bare_number_and_lid() {
        let s = seeded();
        // bare number matches full-JID sender
        let v = s
            .list_messages(&ListMessagesQuery {
                sender: Some("alice".into()),
                limit: 10,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(v.len(), 2);
        // stored @lid sender matches bare number too
        let v = s
            .list_messages(&ListMessagesQuery {
                sender: Some("bob".into()),
                limit: 10,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(v.len(), 3);
    }

    #[test]
    fn after_before_and_pagination() {
        let s = seeded();
        let v = s
            .list_messages(&ListMessagesQuery {
                after: Some(150),
                before: Some(450),
                limit: 10,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(
            v.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
            vec!["m4", "m3", "m2"]
        );
        let p0 = s
            .list_messages(&ListMessagesQuery {
                limit: 2,
                page: 0,
                ..Default::default()
            })
            .unwrap();
        let p1 = s
            .list_messages(&ListMessagesQuery {
                limit: 2,
                page: 1,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(p0.len(), 2);
        assert_eq!(p1.len(), 2);
        assert_ne!(p0[0].id, p1[0].id);
    }

    #[test]
    fn query_matches_partial_words() {
        // "meet" must hit "meeting" and "meetup": FTS phrase MATCH alone
        // is whole-word and would miss both (original was %meet% LIKE).
        let s = seeded();
        let v = s
            .list_messages(&ListMessagesQuery {
                query: Some("meet".into()),
                limit: 10,
                include_context: false,
                ..Default::default()
            })
            .unwrap();
        let ids: Vec<&str> = v.iter().map(|m| m.id.as_str()).collect();
        assert!(ids.contains(&"m4"), "meet in meeting: {ids:?}");
    }

    #[test]
    fn query_search_and_context_dedup() {
        let s = seeded();
        let v = s
            .list_messages(&ListMessagesQuery {
                query: Some("alice".into()),
                limit: 10,
                include_context: true,
                context_before: 1,
                context_after: 1,
                ..Default::default()
            })
            .unwrap();
        // m1 and m2 match; context windows overlap -> each id exactly once
        let mut ids: Vec<&str> = v.iter().map(|m| m.id.as_str()).collect();
        let uniq: HashSet<&str> = ids.iter().copied().collect();
        assert_eq!(ids.len(), uniq.len());
        assert!(uniq.contains("m1") && uniq.contains("m2"));
        ids.sort();
        assert_eq!(ids, vec!["m1", "m2"]);
    }

    #[test]
    fn message_context_ascending() {
        let s = seeded();
        // add neighbors around m4
        let ctx = s.get_message_context("m4", 5, 5).unwrap();
        assert_eq!(ctx.message.id, "m4");
        assert!(ctx.after.iter().any(|m| m.id == "m5"));
        let mut ts: Vec<i64> = ctx.before.iter().map(|m| m.timestamp).collect();
        ts.push(ctx.message.timestamp);
        ts.extend(ctx.after.iter().map(|m| m.timestamp));
        let mut sorted = ts.clone();
        sorted.sort();
        assert_eq!(ts, sorted);
    }

    #[test]
    fn chats_search_sort_and_get() {
        let s = seeded();
        let chats = s.list_chats(None, 10, 0, true, "last_active").unwrap();
        assert_eq!(chats.len(), 3);
        assert_eq!(chats[0].jid, "group@g.us");
        assert_eq!(chats[0].last_message.as_deref(), Some("weekend meetup"));
        let by_name = s.list_chats(None, 10, 0, false, "name").unwrap();
        assert_eq!(by_name[0].name.as_deref(), Some("Alice"));
        let q = s
            .list_chats(Some("alice"), 10, 0, true, "last_active")
            .unwrap();
        assert_eq!(q.len(), 1);
        let c = s.get_chat("bob@s.whatsapp.net", true).unwrap().unwrap();
        assert_eq!(c.name.as_deref(), Some("Bob"));
        assert!(s.get_chat("nope@s.whatsapp.net", true).unwrap().is_none());
        let d = s.get_direct_chat_by_contact("alice").unwrap().unwrap();
        assert_eq!(d.jid, "alice@s.whatsapp.net");
    }

    #[test]
    fn contacts_and_last_interaction() {
        let s = seeded();
        let c = s.search_contacts("ali").unwrap();
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].phone_number, "alice");
        // groups excluded
        assert!(s.search_contacts("group").unwrap().is_empty());
        let last = s.get_last_interaction("bob@lid").unwrap().unwrap();
        assert_eq!(last.id, "m6");
        let cc = s.get_contact_chats("alice@s.whatsapp.net", 10, 0).unwrap();
        assert_eq!(cc.len(), 2);
        assert_eq!(s.get_sender_name("alice@s.whatsapp.net"), "Alice");
        assert_eq!(
            s.get_sender_name("unknown@s.whatsapp.net"),
            "unknown@s.whatsapp.net"
        );
    }

    #[test]
    fn upsert_chat_keeps_name_and_bumps_time() {
        let s = seeded();
        s.upsert_chat("alice@s.whatsapp.net", "", 5000).unwrap();
        let c = s.get_chat("alice@s.whatsapp.net", false).unwrap().unwrap();
        assert_eq!(c.name.as_deref(), Some("Alice"));
        assert_eq!(c.last_message_time, Some(5000));
    }
}
