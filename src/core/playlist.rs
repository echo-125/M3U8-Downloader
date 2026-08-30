use serde::{Deserialize, Serialize};
use url::Url;

use crate::core::error::CoreError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ByteRange {
    pub length: u64,
    pub offset: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EncryptionInfo {
    pub method: String,
    pub key_uri: String,
    pub iv: Option<[u8; 16]>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InitializationSegment {
    pub url: String,
    pub byte_range: Option<ByteRange>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaSegment {
    pub index: usize,
    pub url: String,
    pub byte_range: Option<ByteRange>,
    pub encryption: Option<EncryptionInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaPlaylist {
    pub media_sequence: u64,
    pub initialization: Option<InitializationSegment>,
    pub segments: Vec<MediaSegment>,
    pub has_end_list: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VariantStream {
    pub url: String,
    pub bandwidth: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Playlist {
    Master(Vec<VariantStream>),
    Media(MediaPlaylist),
}

pub fn parse_playlist(content: &str, base_url: &str) -> Result<Playlist, CoreError> {
    let base = Url::parse(base_url).map_err(|_| CoreError::InvalidUrl)?;
    if !content.lines().any(|line| line.trim() == "#EXTM3U") {
        return Err(CoreError::InvalidPlaylist);
    }

    if content.contains("#EXT-X-STREAM-INF") {
        return parse_master_playlist(content, &base);
    }

    parse_media_playlist(content, &base)
}

fn parse_master_playlist(content: &str, base: &Url) -> Result<Playlist, CoreError> {
    let mut variants = Vec::new();
    let mut pending_bandwidth = None;
    for raw_line in content.lines() {
        let line = raw_line.trim();
        if let Some(attributes) = line.strip_prefix("#EXT-X-STREAM-INF:") {
            pending_bandwidth = Some(
                unquoted_attribute(attributes, "BANDWIDTH")
                    .and_then(|value| value.parse::<u64>().ok())
                    .ok_or(CoreError::InvalidPlaylist)?,
            );
        } else if !line.is_empty() && !line.starts_with('#') {
            let bandwidth = pending_bandwidth.take().ok_or(CoreError::InvalidPlaylist)?;
            let url = base.join(line).map_err(|_| CoreError::InvalidUrl)?;
            variants.push(VariantStream {
                url: url.to_string(),
                bandwidth,
            });
        }
    }

    if variants.is_empty() {
        return Err(CoreError::InvalidPlaylist);
    }
    Ok(Playlist::Master(variants))
}

fn parse_media_playlist(content: &str, base: &Url) -> Result<Playlist, CoreError> {
    let mut media_sequence = 0;
    let mut initialization = None;
    let mut segments = Vec::new();
    let mut current_encryption = None;
    let mut pending_byte_range: Option<ByteRange> = None;
    let mut next_implicit_offset = 0;

    for raw_line in content.lines() {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }
        if line == "#EXT-X-ENDLIST" {
            continue;
        }
        if !line.starts_with('#') {
            let url = base.join(line).map_err(|_| CoreError::InvalidUrl)?;
            let byte_range = pending_byte_range.take().map(|mut range| {
                // 隐式 BYTERANGE：偏移量取自上一个分片结束位置。
                let offset = range.offset.unwrap_or(next_implicit_offset);
                range.offset = Some(offset);
                next_implicit_offset = offset + range.length;
                range
            });
            let index = segments.len();
            segments.push(MediaSegment {
                index,
                url: url.to_string(),
                byte_range,
                encryption: current_encryption.clone(),
            });
            continue;
        }

        if let Some(value) = line.strip_prefix("#EXT-X-MEDIA-SEQUENCE:") {
            media_sequence = value
                .trim()
                .parse()
                .map_err(|_| CoreError::InvalidPlaylist)?;
        } else if let Some(attributes) = line.strip_prefix("#EXT-X-MAP:") {
            let uri = quoted_attribute(attributes, "URI").ok_or(CoreError::InvalidPlaylist)?;
            let url = base.join(&uri).map_err(|_| CoreError::InvalidUrl)?;
            let byte_range = parse_byte_range_attribute(attributes)?;
            initialization = Some(InitializationSegment {
                url: url.to_string(),
                byte_range,
            });
        } else if let Some(attributes) = line.strip_prefix("#EXT-X-KEY:") {
            current_encryption = parse_key(attributes, base)?;
        } else if let Some(value) = line.strip_prefix("#EXT-X-BYTERANGE:") {
            pending_byte_range = Some(parse_byte_range_value(value.trim())?);
        }
    }

    if segments.is_empty() {
        return Err(CoreError::InvalidPlaylist);
    }

    Ok(Playlist::Media(MediaPlaylist {
        media_sequence,
        initialization,
        segments,
        has_end_list: content.contains("#EXT-X-ENDLIST"),
    }))
}

fn parse_key(attributes: &str, base: &Url) -> Result<Option<EncryptionInfo>, CoreError> {
    let method = unquoted_attribute(attributes, "METHOD").unwrap_or_default();
    if method == "NONE" {
        return Ok(None);
    }
    if method != "AES-128" && method != "AES" {
        return Err(CoreError::UnsupportedEncryption(method));
    }
    let uri = quoted_attribute(attributes, "URI").ok_or(CoreError::InvalidPlaylist)?;
    let key_url = base.join(&uri).map_err(|_| CoreError::InvalidUrl)?;
    let iv = quoted_attribute(attributes, "IV")
        .map(|value| parse_iv(&value))
        .transpose()?;
    Ok(Some(EncryptionInfo {
        method: "AES-128".to_string(),
        key_uri: key_url.to_string(),
        iv,
    }))
}

fn parse_iv(value: &str) -> Result<[u8; 16], CoreError> {
    let value = value
        .trim()
        .trim_start_matches("0x")
        .trim_start_matches("0X");
    if value.len() != 32 || !value.chars().all(|character| character.is_ascii_hexdigit()) {
        return Err(CoreError::InvalidPlaylist);
    }
    let mut iv = [0_u8; 16];
    for (index, chunk) in value.as_bytes().chunks(2).enumerate() {
        let text = std::str::from_utf8(chunk).map_err(|_| CoreError::InvalidPlaylist)?;
        iv[index] = u8::from_str_radix(text, 16).map_err(|_| CoreError::InvalidPlaylist)?;
    }
    Ok(iv)
}

fn parse_byte_range_attribute(attributes: &str) -> Result<Option<ByteRange>, CoreError> {
    quoted_attribute(attributes, "BYTERANGE")
        .map(|value| parse_byte_range_value(&value))
        .transpose()
}

fn parse_byte_range_value(value: &str) -> Result<ByteRange, CoreError> {
    let mut parts = value.trim().splitn(2, '@');
    let length = parts
        .next()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .ok_or(CoreError::InvalidPlaylist)?;
    let offset = parts
        .next()
        .and_then(|value| value.trim().parse::<u64>().ok());
    Ok(ByteRange { length, offset })
}

fn quoted_attribute(attributes: &str, key: &str) -> Option<String> {
    attribute_parts(attributes)
        .into_iter()
        .find_map(|(name, value)| (name == key).then(|| value.trim_matches('"').to_string()))
}

fn unquoted_attribute(attributes: &str, key: &str) -> Option<String> {
    attribute_parts(attributes)
        .into_iter()
        .find_map(|(name, value)| (name == key).then(|| value.trim_matches('"').to_string()))
}

fn attribute_parts(attributes: &str) -> Vec<(String, String)> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    for character in attributes.chars() {
        match character {
            '"' => {
                quoted = !quoted;
                current.push(character);
            }
            ',' if !quoted => {
                push_attribute(&mut parts, &current);
                current.clear();
            }
            _ => current.push(character),
        }
    }
    push_attribute(&mut parts, &current);
    parts
}

fn push_attribute(parts: &mut Vec<(String, String)>, value: &str) {
    if let Some((name, value)) = value.trim().split_once('=') {
        parts.push((name.trim().to_string(), value.trim().to_string()));
    }
}

pub fn select_best_variant(variants: &[VariantStream]) -> Option<&VariantStream> {
    variants.iter().max_by_key(|variant| variant.bandwidth)
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: &str = "https://example.com/video/index.m3u8";

    #[test]
    fn parses_media_playlist_and_resolves_urls() {
        let content = "#EXTM3U\n#EXT-X-MEDIA-SEQUENCE:7\n#EXT-X-KEY:METHOD=AES-128,URI=\"keys/key.bin\",IV=0x000102030405060708090a0b0c0d0e0f\n#EXT-X-MAP:URI=\"init.mp4\",BYTERANGE=\"8192@0\"\n#EXT-X-BYTERANGE:1000@2000\nseg1.ts\nseg2.ts\n#EXT-X-ENDLIST\n";
        let playlist = parse_playlist(content, BASE).unwrap();
        let Playlist::Media(media) = playlist else {
            panic!("必须是媒体播放列表");
        };
        assert_eq!(media.media_sequence, 7);
        assert_eq!(media.segments.len(), 2);
        assert_eq!(media.segments[0].url, "https://example.com/video/seg1.ts");
        assert_eq!(media.segments[1].byte_range, None);
        assert_eq!(
            media.segments[0].encryption.as_ref().unwrap().iv,
            Some([0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15])
        );
        assert_eq!(
            media.initialization.as_ref().unwrap().url,
            "https://example.com/video/init.mp4"
        );
        assert!(media.has_end_list);
    }

    #[test]
    fn parses_master_playlist_and_selects_highest_bandwidth() {
        let content = "#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=1000\nlow.m3u8\n#EXT-X-STREAM-INF:BANDWIDTH=4000\nhigh.m3u8\n";
        let playlist = parse_playlist(content, BASE).unwrap();
        let Playlist::Master(variants) = playlist else {
            panic!("必须是主播放列表");
        };
        assert_eq!(
            select_best_variant(&variants).unwrap().url,
            "https://example.com/video/high.m3u8"
        );
    }

    #[test]
    fn rejects_live_playlist() {
        let content = "#EXTM3U\nseg1.ts\n";
        let playlist = parse_playlist(content, BASE).unwrap();
        let Playlist::Media(media) = playlist else {
            panic!("必须是媒体播放列表");
        };
        assert!(!media.has_end_list);
    }

    #[test]
    fn continues_implicit_byte_range_offsets() {
        // BYTERANGE 省略 @offset 时，偏移延续上一个分片的结束位置继续累加；
        // 显式给出 @offset 时直接使用，并重置后续的隐式偏移基准。
        let content = "#EXTM3U\n#EXT-X-BYTERANGE:1000\nseg1.ts\n#EXT-X-BYTERANGE:2000\nseg2.ts\n#EXT-X-BYTERANGE:500@10000\nseg3.ts\n#EXT-X-ENDLIST\n";
        let playlist = parse_playlist(content, BASE).unwrap();
        let Playlist::Media(media) = playlist else {
            panic!("必须是媒体播放列表");
        };
        assert_eq!(
            media.segments[0].byte_range,
            Some(ByteRange {
                length: 1000,
                offset: Some(0)
            })
        );
        assert_eq!(
            media.segments[1].byte_range,
            Some(ByteRange {
                length: 2000,
                offset: Some(1000)
            })
        );
        assert_eq!(
            media.segments[2].byte_range,
            Some(ByteRange {
                length: 500,
                offset: Some(10000)
            })
        );
    }

    #[test]
    fn byte_range_only_applies_to_next_segment() {
        // 一个 BYTERANGE 标签只描述紧随其后的那一个分片，
        // 没有新标签的后续分片不继承 byte_range（HLS 规范行为）。
        let content = "#EXTM3U\n#EXT-X-BYTERANGE:1000@0\nseg1.ts\nseg2.ts\n#EXT-X-ENDLIST\n";
        let playlist = parse_playlist(content, BASE).unwrap();
        let Playlist::Media(media) = playlist else {
            panic!("必须是媒体播放列表");
        };
        assert_eq!(
            media.segments[0].byte_range,
            Some(ByteRange {
                length: 1000,
                offset: Some(0)
            })
        );
        assert_eq!(media.segments[1].byte_range, None);
    }

    #[test]
    fn rejects_sample_aes() {
        let content =
            "#EXTM3U\n#EXT-X-KEY:METHOD=SAMPLE-AES,URI=\"key.bin\"\nseg1.ts\n#EXT-X-ENDLIST\n";
        assert!(matches!(
            parse_playlist(content, BASE),
            Err(CoreError::UnsupportedEncryption(_))
        ));
    }
}
