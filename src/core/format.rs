#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SegmentFormat {
    Ts,
    Fmp4,
    Html,
    Json,
    Image,
    WebM,
    Unknown,
}

impl SegmentFormat {
    pub fn label(self) -> &'static str {
        match self {
            Self::Ts => "TS",
            Self::Fmp4 => "fMP4",
            Self::Html => "HTML",
            Self::Json => "JSON",
            Self::Image => "图片",
            Self::WebM => "WebM",
            Self::Unknown => "未知格式",
        }
    }
}

pub fn detect_format(data: &[u8]) -> SegmentFormat {
    if data.first() == Some(&0x47) {
        return SegmentFormat::Ts;
    }
    if data.len() >= 12
        && (&data[4..8] == b"ftyp" || &data[4..8] == b"moof" || &data[4..8] == b"styp")
    {
        return SegmentFormat::Fmp4;
    }
    if data.starts_with(b"<!DOCTYPE")
        || data.starts_with(b"<!doctype")
        || data.starts_with(b"<html")
    {
        return SegmentFormat::Html;
    }
    let leading = data
        .iter()
        .copied()
        .find(|byte| !byte.is_ascii_whitespace());
    if leading == Some(b'{') || leading == Some(b'[') {
        return SegmentFormat::Json;
    }
    if data.starts_with(&[0xff, 0xd8])
        || data.starts_with(&[0x89, b'P', b'N', b'G'])
        || data.starts_with(b"GIF8")
    {
        return SegmentFormat::Image;
    }
    if data.starts_with(&[0x1a, 0x45, 0xdf, 0xa3]) {
        return SegmentFormat::WebM;
    }
    SegmentFormat::Unknown
}

pub fn diagnostic_message(data: &[u8]) -> String {
    let format = detect_format(data);
    match format {
        SegmentFormat::Html | SegmentFormat::Json => {
            let preview = String::from_utf8_lossy(&data[..data.len().min(160)]);
            format!(
                "{}，可能是错误响应：{}",
                format.label(),
                preview.chars().take(80).collect::<String>()
            )
        }
        _ => format!("分片格式：{}", format.label()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_ts_packet() {
        let data = vec![0x47; 376];
        assert_eq!(detect_format(&data), SegmentFormat::Ts);
    }

    #[test]
    fn detects_fmp4_and_error_page() {
        let mut fmp4 = vec![0; 12];
        fmp4[4..8].copy_from_slice(b"ftyp");
        assert_eq!(detect_format(&fmp4), SegmentFormat::Fmp4);
        assert_eq!(detect_format(b"<!DOCTYPE html>"), SegmentFormat::Html);
    }
}
