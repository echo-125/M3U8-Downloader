use std::borrow::Cow;
use std::io::Read;

use flate2::read::GzDecoder;

/// gzip 文件的固定两字节标识。
const GZIP_MAGIC: [u8; 2] = [0x1f, 0x8b];
/// 解压后允许的最大字节数。错误响应通常很小，设上限是为了避免异常压缩数据撑爆内存。
const MAX_DECOMPRESSED_BYTES: u64 = 16 * 1024 * 1024;

/// 按 gzip 标识尝试解压，失败或不是 gzip 时原样返回。
///
/// reqwest 只在响应带 `Content-Encoding: gzip` 时自动解压，部分 CDN 会返回
/// gzip 数据却不声明该头部（错误响应尤其常见），这里按标识兜底解压，
/// 否则后续格式诊断只会看到一串乱码，报不出真实原因。
fn try_decompress_gzip(data: &[u8]) -> Cow<'_, [u8]> {
    if data.len() < GZIP_MAGIC.len() || data[..2] != GZIP_MAGIC {
        return Cow::Borrowed(data);
    }
    match decompress_gzip(data) {
        Ok(decoded) => Cow::Owned(decoded),
        Err(_) => Cow::Borrowed(data),
    }
}

fn decompress_gzip(data: &[u8]) -> std::io::Result<Vec<u8>> {
    // 多读一个字节用于判断是否超限，take 本身只截断不报错。
    let mut decoder = GzDecoder::new(data).take(MAX_DECOMPRESSED_BYTES + 1);
    let mut decoded = Vec::new();
    decoder.read_to_end(&mut decoded)?;
    if decoded.len() as u64 > MAX_DECOMPRESSED_BYTES {
        return Err(std::io::Error::other("解压后的数据过大"));
    }
    Ok(decoded)
}

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

const ERROR_TEXT_KEYS: &[&str] = &["message", "msg", "error", "detail", "reason", "info"];
const ERROR_CODE_KEYS: &[&str] = &["code", "status", "errcode", "errno"];

/// 检测分片内容的实际格式，必要时先解压 gzip。
pub fn detect_format(data: &[u8]) -> SegmentFormat {
    detect_format_decoded(&try_decompress_gzip(data))
}

/// 对已经解压过的内容做格式检测。
fn detect_format_decoded(data: &[u8]) -> SegmentFormat {
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

/// 分片内容是否属于错误响应，这类内容不应重试。
pub fn is_error_response(data: &[u8]) -> bool {
    matches!(
        detect_format(data),
        SegmentFormat::Html | SegmentFormat::Json
    )
}

pub fn diagnostic_message(data: &[u8]) -> String {
    let data = try_decompress_gzip(data);
    let data: &[u8] = &data;
    let format = detect_format_decoded(data);
    if let Some(detail) = error_detail_decoded(data) {
        return format!("服务器返回{}：{detail}", format.label());
    }
    match format {
        SegmentFormat::Html | SegmentFormat::Json => {
            let preview = String::from_utf8_lossy(&data[..data.len().min(160)]);
            format!(
                "{}，可能是错误响应：{}",
                format.label(),
                truncate_chars(&preview, 80)
            )
        }
        _ => format!("分片格式：{}", format.label()),
    }
}

/// 从 JSON 或 HTML 错误响应里提取人类可读的原因，提取不到时返回 None。
pub fn error_detail(data: &[u8]) -> Option<String> {
    error_detail_decoded(&try_decompress_gzip(data))
}

/// 对已经解压过的内容提取错误原因。
fn error_detail_decoded(data: &[u8]) -> Option<String> {
    match detect_format_decoded(data) {
        SegmentFormat::Html => html_error_detail(data),
        SegmentFormat::Json => json_error_detail(data),
        _ => None,
    }
}

fn html_error_detail(data: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(&data[..data.len().min(16 * 1024)]);
    let lower = text.to_ascii_lowercase();
    let tag_start = lower.find("<title")?;
    let open = text[tag_start..].find('>')?;
    let rest = &text[tag_start + open + 1..];
    let close = rest.to_ascii_lowercase().find("</title>")?;
    let title = rest[..close].trim();
    (!title.is_empty()).then(|| truncate_chars(title, 120))
}

fn json_error_detail(data: &[u8]) -> Option<String> {
    let value: serde_json::Value = serde_json::from_slice(data).ok()?;
    let mut parts = Vec::new();
    if let Some(code) = ERROR_CODE_KEYS
        .iter()
        .find_map(|key| scalar_text(value.get(*key)))
    {
        parts.push(format!("错误码 {code}"));
    }
    if let Some(message) = find_message(&value) {
        parts.push(message);
    }
    (!parts.is_empty()).then(|| parts.join("，"))
}

fn find_message(value: &serde_json::Value) -> Option<String> {
    for key in ERROR_TEXT_KEYS {
        if let Some(text) = scalar_text(value.get(*key)) {
            return Some(truncate_chars(&text, 160));
        }
    }
    // 兼容把错误包在 data / error 对象里的接口
    for container in ["data", "error"] {
        let Some(nested) = value.get(container) else {
            continue;
        };
        if let Some(text) = ERROR_TEXT_KEYS
            .iter()
            .find_map(|key| scalar_text(nested.get(*key)))
        {
            return Some(truncate_chars(&text, 160));
        }
        if let Some(code) = ERROR_CODE_KEYS
            .iter()
            .find_map(|key| scalar_text(nested.get(*key)))
        {
            return Some(format!("错误码 {code}"));
        }
    }
    None
}

fn scalar_text(value: Option<&serde_json::Value>) -> Option<String> {
    let text = match value? {
        serde_json::Value::String(text) => text.trim().to_string(),
        serde_json::Value::Number(number) => number.to_string(),
        serde_json::Value::Bool(flag) => flag.to_string(),
        _ => return None,
    };
    (!text.is_empty()).then_some(text)
}

fn truncate_chars(text: &str, limit: usize) -> String {
    text.chars().take(limit).collect()
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

    #[test]
    fn extracts_json_error_detail() {
        let body = r#"{"code":40301,"message":"签名已过期"}"#.as_bytes();
        assert_eq!(
            error_detail(body).as_deref(),
            Some("错误码 40301，签名已过期")
        );
        let nested = r#"{"data":{"msg":"地区限制"}}"#.as_bytes();
        assert_eq!(error_detail(nested).as_deref(), Some("地区限制"));
    }

    #[test]
    fn extracts_html_error_detail() {
        let body = b"<!DOCTYPE html><html><head><title>403 Forbidden</title></head></html>";
        assert_eq!(error_detail(body).as_deref(), Some("403 Forbidden"));
    }

    #[test]
    fn decompresses_gzipped_error_response() {
        // CDN 返回 gzip 压缩的错误响应却不声明 Content-Encoding 时，
        // 不解压的话格式检测只会看到乱码，报不出真实原因。
        let body = r#"{"code":40301,"message":"签名已过期"}"#.as_bytes();
        let gzipped = gzip_compress(body);
        assert_ne!(&gzipped[..2], &body[..2]);
        assert_eq!(detect_format(&gzipped), SegmentFormat::Json);
        assert_eq!(
            error_detail(&gzipped).as_deref(),
            Some("错误码 40301，签名已过期")
        );
        assert!(is_error_response(&gzipped));
    }

    #[test]
    fn leaves_non_gzip_data_untouched() {
        let ts_packet = vec![0x47; 376];
        assert_eq!(detect_format(&ts_packet), SegmentFormat::Ts);
        // 截断的 gzip 数据解压失败，应原样返回而不是报错。
        let broken = vec![0x1f, 0x8b, 0x08, 0x00];
        assert_eq!(detect_format(&broken), SegmentFormat::Unknown);
    }

    /// 用 flate2 生成 gzip 数据，仅测试使用。
    fn gzip_compress(data: &[u8]) -> Vec<u8> {
        use flate2::{write::GzEncoder, Compression};
        use std::io::Write;
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(data).unwrap();
        encoder.finish().unwrap()
    }

    #[test]
    fn marks_error_response() {
        assert!(is_error_response(br#"{"code":1}"#));
        assert!(!is_error_response(&[0x47; 376]));
    }
}
