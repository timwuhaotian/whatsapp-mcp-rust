use whatsapp_rust::prelude::{MessageExt, wa};

#[derive(Debug, Default, Clone)]
pub struct Extracted {
    pub content: String,
    pub media_type: Option<String>,
    pub filename: Option<String>,
    pub url: Option<String>,
    pub direct_path: Option<String>,
    pub media_key: Option<Vec<u8>>,
    pub file_sha256: Option<Vec<u8>>,
    pub file_enc_sha256: Option<Vec<u8>>,
    pub file_length: Option<u64>,
    pub mimetype: Option<String>,
    pub caption: Option<String>,
}

/// Mirror of the Go bridge's extractTextContent/extractMediaInfo, with one
/// deliberate improvement: media captions are kept (the Go code dropped them).
pub fn extract(msg: &wa::Message) -> Extracted {
    let base = msg.get_base_message();
    let mut out = Extracted::default();

    if let Some(t) = base.text_content() {
        out.content = t.to_string();
    }
    if let Some(cap) = base.get_caption() {
        out.caption = Some(cap.to_string());
        // Captions are the only readable text on media messages.
        if out.content.is_empty() {
            out.content = cap.to_string();
        }
    }

    if let Some(img) = base.image_message.as_option() {
        out.media_type = Some("image".into());
        out.filename = Some(format!(
            "image_{}.jpg",
            chrono::Utc::now().format("%Y%m%d_%H%M%S")
        ));
        out.url = img.url.clone();
        out.direct_path = img.direct_path.clone();
        out.media_key = img.media_key.clone();
        out.file_sha256 = img.file_sha256.clone();
        out.file_enc_sha256 = img.file_enc_sha256.clone();
        out.file_length = img.file_length;
        out.mimetype = img.mimetype.clone();
    } else if let Some(vid) = base.video_message.as_option() {
        out.media_type = Some("video".into());
        out.filename = Some(format!(
            "video_{}.mp4",
            chrono::Utc::now().format("%Y%m%d_%H%M%S")
        ));
        out.url = vid.url.clone();
        out.direct_path = vid.direct_path.clone();
        out.media_key = vid.media_key.clone();
        out.file_sha256 = vid.file_sha256.clone();
        out.file_enc_sha256 = vid.file_enc_sha256.clone();
        out.file_length = vid.file_length;
        out.mimetype = vid.mimetype.clone();
    } else if let Some(aud) = base.audio_message.as_option() {
        out.media_type = Some("audio".into());
        out.filename = Some(format!(
            "audio_{}.ogg",
            chrono::Utc::now().format("%Y%m%d_%H%M%S")
        ));
        out.url = aud.url.clone();
        out.direct_path = aud.direct_path.clone();
        out.media_key = aud.media_key.clone();
        out.file_sha256 = aud.file_sha256.clone();
        out.file_enc_sha256 = aud.file_enc_sha256.clone();
        out.file_length = aud.file_length;
        out.mimetype = aud.mimetype.clone();
    } else if let Some(doc) = base.document_message.as_option() {
        out.media_type = Some("document".into());
        out.filename =
            Some(doc.file_name.clone().unwrap_or_else(|| {
                format!("document_{}", chrono::Utc::now().format("%Y%m%d_%H%M%S"))
            }));
        out.url = doc.url.clone();
        out.direct_path = doc.direct_path.clone();
        out.media_key = doc.media_key.clone();
        out.file_sha256 = doc.file_sha256.clone();
        out.file_enc_sha256 = doc.file_enc_sha256.clone();
        out.file_length = doc.file_length;
        out.mimetype = doc.mimetype.clone();
    } else if let Some(st) = base.sticker_message.as_option() {
        // New vs the Go bridge: stickers are recognized instead of skipped.
        out.media_type = Some("sticker".into());
        out.filename = Some(format!(
            "sticker_{}.webp",
            chrono::Utc::now().format("%Y%m%d_%H%M%S")
        ));
        out.url = st.url.clone();
        out.direct_path = st.direct_path.clone();
        out.media_key = st.media_key.clone();
        out.file_sha256 = st.file_sha256.clone();
        out.file_enc_sha256 = st.file_enc_sha256.clone();
        out.file_length = st.file_length;
        out.mimetype = st.mimetype.clone();
    }

    out
}
