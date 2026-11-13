use std::path::{Path, PathBuf};

pub struct AppConfig {
    pub data_dir: PathBuf,
}

impl AppConfig {
    pub fn resolve(data_dir: Option<PathBuf>) -> anyhow::Result<Self> {
        let data_dir = match data_dir {
            Some(d) => d,
            None => dirs::data_dir()
                .map(|d| d.join("whatsapp-mcp"))
                .context("could not determine platform data dir; pass --data-dir")?,
        };
        Ok(Self { data_dir })
    }

    pub fn whatsapp_db(&self) -> PathBuf {
        self.data_dir.join("whatsapp.db")
    }

    pub fn messages_db(&self) -> PathBuf {
        self.data_dir.join("messages.db")
    }

    pub fn media_dir(&self) -> PathBuf {
        self.data_dir.join("media")
    }

    pub fn ensure_dirs(&self) -> anyhow::Result<()> {
        std::fs::create_dir_all(&self.data_dir)
            .with_context(|| format!("create data dir {}", self.data_dir.display()))?;
        std::fs::create_dir_all(self.media_dir())
            .with_context(|| format!("create media dir {}", self.media_dir().display()))?;
        Ok(())
    }

    pub fn exe_path() -> anyhow::Result<PathBuf> {
        std::env::current_exe().context("could not determine current exe path")
    }
}

use anyhow::Context;

pub fn sanitize_chat_dir(chat_jid: &str) -> String {
    chat_jid.replace([':', '/'], "_")
}

pub fn chat_media_dir(data_dir: &Path, chat_jid: &str) -> PathBuf {
    data_dir.join("media").join(sanitize_chat_dir(chat_jid))
}
