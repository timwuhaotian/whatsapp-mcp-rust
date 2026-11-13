use anyhow::{Context, Result};

/// Port of the Go bridge's analyzeOggOpus: scan OggS pages for the OpusHead
/// (pre-skip/sample-rate) and the last granule position, then
/// duration = ceil((lastGranule - preSkip) / sampleRate), clamped to 1..=300.
pub fn analyze_ogg_opus(data: &[u8]) -> Result<(u32, Vec<u8>)> {
    if data.len() < 4 || &data[0..4] != b"OggS" {
        anyhow::bail!("not a valid Ogg file (missing OggS signature)");
    }

    let mut last_granule: u64 = 0;
    let mut sample_rate: u32 = 48000;
    let mut pre_skip: u16 = 0;
    let mut found_opus_head = false;

    let mut i = 0;
    while i < data.len() {
        if i + 27 >= data.len() {
            break;
        }
        if &data[i..i + 4] != b"OggS" {
            i += 1;
            continue;
        }
        let granule = u64::from_le_bytes(data[i + 6..i + 14].try_into().unwrap());
        let page_seq = u32::from_le_bytes(data[i + 18..i + 22].try_into().unwrap());
        let num_segments = data[i + 26] as usize;
        if i + 27 + num_segments >= data.len() {
            break;
        }
        let segment_table = &data[i + 27..i + 27 + num_segments];
        let mut page_size = 27 + num_segments;
        for seg in segment_table {
            page_size += *seg as usize;
        }
        if i + page_size > data.len() {
            break;
        }

        if !found_opus_head && page_seq <= 1 {
            let page = &data[i..i + page_size];
            if let Some(pos) = find_subslice(page, b"OpusHead") {
                // OpusHead: magic(8) ver(1) ch(1) preskip(2) rate(4) ...
                if pos + 8 + 12 <= page.len() {
                    let base = pos + 8;
                    pre_skip = u16::from_le_bytes(page[base + 2..base + 4].try_into().unwrap());
                    sample_rate = u32::from_le_bytes(page[base + 4..base + 8].try_into().unwrap());
                    found_opus_head = true;
                }
            }
        }

        if granule != 0 {
            last_granule = granule;
        }
        i += page_size;
    }

    let mut duration = if last_granule > 0 {
        let secs = (last_granule.saturating_sub(pre_skip as u64)) as f64 / sample_rate as f64;
        secs.ceil() as u32
    } else {
        // Same rough fallback as the Go bridge when no granule is found.
        (data.len() as f64 / 2000.0) as u32
    };
    duration = duration.clamp(1, 300);

    Ok((duration, placeholder_waveform(duration)))
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// Port of the Go placeholderWaveform: a synthetic 64-byte waveform WhatsApp
/// expects on voice messages. xorshift32 instead of Go's math/rand (no extra dep).
pub fn placeholder_waveform(duration: u32) -> Vec<u8> {
    const LEN: usize = 64;
    let mut out = vec![0u8; LEN];
    let mut rng = duration.wrapping_mul(0x9E37_79B9).wrapping_add(0x85EB_CA6B);
    let mut next_f64 = move || {
        rng ^= rng << 13;
        rng ^= rng >> 17;
        rng ^= rng << 5;
        (rng as f64) / (u32::MAX as f64)
    };
    let base_amplitude = 35.0;
    let freq = (duration.min(120) as f64) / 30.0;
    for (i, b) in out.iter_mut().enumerate() {
        let pos = i as f64 / LEN as f64;
        let mut val = base_amplitude * (pos * std::f64::consts::PI * freq * 8.0).sin();
        val += (base_amplitude / 2.0) * (pos * std::f64::consts::PI * freq * 16.0).sin();
        val += (next_f64() - 0.5) * 15.0;
        let fade = (pos * std::f64::consts::PI).sin();
        val *= 0.7 + 0.3 * fade;
        val += 50.0;
        *b = val.clamp(0.0, 100.0) as u8;
    }
    out
}

/// Port of audio.py: convert any audio file to Opus-in-Ogg via ffmpeg.
pub async fn convert_to_opus_ogg(input: &std::path::Path) -> Result<std::path::PathBuf> {
    if !input.is_file() {
        anyhow::bail!("Media file not found: {}", input.display());
    }
    if input.extension().and_then(|e| e.to_str()) == Some("ogg") {
        return Ok(input.to_path_buf());
    }
    let tmp = std::env::temp_dir().join(format!(
        "whatsapp-mcp-{}.ogg",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let status = tokio::process::Command::new("ffmpeg")
        .args([
            "-i",
            &input.to_string_lossy(),
            "-c:a",
            "libopus",
            "-b:a",
            "32k",
            "-ar",
            "24000",
            "-application",
            "voip",
            "-vbr",
            "on",
            "-compression_level",
            "10",
            "-frame_duration",
            "60",
            "-y",
        ])
        .arg(&tmp)
        .output()
        .await
        .context("failed to run ffmpeg (is it installed? if not, use send_file instead)")?;
    if !status.status.success() {
        anyhow::bail!(
            "ffmpeg conversion failed (use send_file instead): {}",
            String::from_utf8_lossy(&status.stderr)
        );
    }
    Ok(tmp)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_non_ogg() {
        assert!(analyze_ogg_opus(b"RIFF....WEBP").is_err());
        assert!(analyze_ogg_opus(b"").is_err());
    }

    fn minimal_ogg(packets: &[&[u8]], granules: &[u64]) -> Vec<u8> {
        // Build minimal Ogg pages: header(27) + segtable(1) + payload.
        let mut out = Vec::new();
        for (i, (pkt, g)) in packets.iter().zip(granules.iter()).enumerate() {
            let mut hdr = vec![0u8; 27];
            hdr[0..4].copy_from_slice(b"OggS");
            hdr[6..14].copy_from_slice(&g.to_le_bytes());
            hdr[18..22].copy_from_slice(&(i as u32).to_le_bytes());
            hdr[26] = 1;
            out.extend_from_slice(&hdr);
            out.push(pkt.len() as u8);
            out.extend_from_slice(pkt);
        }
        out
    }

    #[test]
    fn parses_opus_head_and_granule() {
        // OpusHead: magic + ver(1) + ch(1) + preskip(u16le) + rate(u32le)
        let mut head = b"OpusHead".to_vec();
        head.extend_from_slice(&[1, 2]);
        head.extend_from_slice(&312u16.to_le_bytes());
        head.extend_from_slice(&48000u32.to_le_bytes());
        head.extend_from_slice(&[0u8; 8]);
        let audio = [0xF8u8; 100];
        // 2s of audio at 48kHz: granule = 96000
        let data = minimal_ogg(&[&head, &audio], &[0, 96000]);
        let (dur, wave) = analyze_ogg_opus(&data).unwrap();
        // (96000 - 312) / 48000 = 1.99.. -> ceil = 2
        assert_eq!(dur, 2);
        assert_eq!(wave.len(), 64);
    }

    #[test]
    fn waveform_is_64_bytes() {
        for d in [1, 30, 400] {
            assert_eq!(placeholder_waveform(d).len(), 64);
        }
    }
}
