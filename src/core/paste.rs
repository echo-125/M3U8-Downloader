//! 浏览器侧导出的「M3U8 任务卡片」解析。
//!
//! 扩展把 Shaka Player 内存里解码出的清单连同鉴权上下文包成
//! `<<<M3U8_TASK_START>>>{JSON}<<<M3U8_TASK_END>>>` 供用户复制。卡片里的清单正文是内联的
//! （分片地址带时效签名、清单本身往往无法再次回源），因此导入时必须连正文一起落盘，
//! 不能只取一个清单地址去抓。`magic` 字段用来把这段内容和普通复制文本区分开。

use std::collections::HashMap;

use serde::Deserialize;
use url::Url;

use crate::core::{
    error::CoreError,
    headers::validate_headers,
    playlist::{parse_playlist, MediaPlaylist, Playlist},
};

pub const CARD_START_MARKER: &str = "<<<M3U8_TASK_START>>>";
pub const CARD_END_MARKER: &str = "<<<M3U8_TASK_END>>>";
pub const CARD_MAGIC: &str = "M3U8_TASK_V1";

/// 卡片解析结果。标题仍会再走一次 `sanitize_filename`，不假设导出端一定清理干净。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskCard {
    pub title: String,
    pub content: String,
    pub headers: HashMap<String, String>,
}

#[derive(Debug, Deserialize)]
struct RawCard {
    magic: String,
    #[serde(default)]
    title: String,
    content: String,
    #[serde(default)]
    headers: HashMap<String, String>,
}

/// 文本里是否出现卡片起始标记，用于把卡片与「链接|文件名」逐行格式分流。
pub fn contains_card(text: &str) -> bool {
    text.contains(CARD_START_MARKER)
}

/// 解析文本里出现的全部卡片，按出现顺序返回；没有标记时返回空表。
pub fn parse_cards(text: &str) -> Result<Vec<TaskCard>, CoreError> {
    let mut cards = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find(CARD_START_MARKER) {
        let after_start = &rest[start + CARD_START_MARKER.len()..];
        let Some(end) = after_start.find(CARD_END_MARKER) else {
            return Err(CoreError::InvalidInput(format!(
                "任务卡片缺少结束标记 {CARD_END_MARKER}"
            )));
        };
        cards.push(parse_card_body(&after_start[..end])?);
        rest = &after_start[end + CARD_END_MARKER.len()..];
    }
    Ok(cards)
}

fn parse_card_body(body: &str) -> Result<TaskCard, CoreError> {
    let raw: RawCard = serde_json::from_str(body.trim())
        .map_err(|error| CoreError::InvalidInput(format!("任务卡片不是合法的 JSON：{error}")))?;
    if raw.magic != CARD_MAGIC {
        return Err(CoreError::InvalidInput(format!(
            "任务卡片协议版本不支持：{}（预期 {CARD_MAGIC}）",
            raw.magic
        )));
    }
    if raw.content.trim().is_empty() {
        return Err(CoreError::InvalidInput("任务卡片缺少 content".into()));
    }
    Ok(TaskCard {
        title: raw.title.trim().to_string(),
        content: raw.content,
        headers: validate_headers(raw.headers)?,
    })
}

/// 解析内联清单所需的基准地址。
///
/// 优先取正文里第一个绝对地址：卡片的分片与源站同源，正文若混有相对路径，
/// 只有按这个地址 join 才能落到正确位置。正文没有绝对地址时退回 referer / origin，
/// 两者都没有就返回 None（调用方据此报错，不猜）。
pub fn card_base_url(content: &str, headers: &HashMap<String, String>) -> Option<String> {
    for raw_line in content.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if is_absolute_http_url(line) {
            return Some(line.to_string());
        }
    }
    card_header_url(headers).map(str::to_string)
}

/// 任务列表展示用的来源地址：优先 referer——真实播放页比带签名的分片地址可读得多。
pub fn card_source_url(headers: &HashMap<String, String>, base_url: &str) -> String {
    card_header_url(headers)
        .map(str::to_string)
        .unwrap_or_else(|| base_url.to_string())
}

fn card_header_url(headers: &HashMap<String, String>) -> Option<&str> {
    ["referer", "origin"].into_iter().find_map(|name| {
        let value = header_value(headers, name)?;
        is_absolute_http_url(value).then_some(value)
    })
}

fn header_value<'a>(headers: &'a HashMap<String, String>, name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

fn is_absolute_http_url(value: &str) -> bool {
    Url::parse(value)
        .map(|url| matches!(url.scheme(), "http" | "https"))
        .unwrap_or(false)
}

/// 把内联清单正文解析成可下载的媒体播放列表。
///
/// 约束与网络抓取保持一致：主清单还得再抓一层（内联场景拿不到后续地址），
/// 没有 ENDLIST 的是直播，两者都拒绝，避免建出一个必然失败的任务。
pub fn parse_inline_playlist(content: &str, base_url: &str) -> Result<MediaPlaylist, CoreError> {
    match parse_playlist(content, base_url)? {
        Playlist::Media(media) if media.has_end_list => Ok(media),
        Playlist::Media(_) => Err(CoreError::LiveStream),
        Playlist::Master(_) => Err(CoreError::InvalidPlaylistDetail(
            "任务卡片里是主播放列表，请粘贴含分片地址的媒体清单".into(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SEGMENT: &str = "https://cdn.example.com/hls/C-000.png?exp=123&auth=abc";
    const PLAY_PAGE: &str = "https://play.example.com/play/1";

    fn card_json(magic: &str) -> String {
        // 用 r##"…"## 而不是 r#"…"#：JSON 正文里含 `"#EXTM3U`，
        // 单 # 定界时那个 `"#` 会被当成原始字符串的结束符，后面的内容会被当作代码。
        format!(
            r##"{{
  "magic": "{magic}",
  "title": "示例视频",
  "content": "#EXTM3U\n#EXT-X-VERSION:3\n#EXTINF:8,\n{SEGMENT}\n#EXT-X-ENDLIST\n",
  "headers": {{"origin": "https://play.example.com", "referer": "{PLAY_PAGE}", "User-Agent": "Mozilla/5.0"}}
}}"##
        )
    }

    fn wrap(body: &str) -> String {
        format!("{CARD_START_MARKER}\n{body}\n{CARD_END_MARKER}")
    }

    #[test]
    fn parses_card_with_headers() {
        let cards = parse_cards(&wrap(&card_json(CARD_MAGIC))).unwrap();
        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0].title, "示例视频");
        assert!(cards[0].content.contains("#EXTM3U"));
        assert_eq!(cards[0].headers.get("referer").unwrap(), PLAY_PAGE);
    }

    #[test]
    fn rejects_unknown_magic() {
        let error = parse_cards(&wrap(&card_json("M3U8_TASK_V2"))).unwrap_err();
        assert!(matches!(error, CoreError::InvalidInput(_)));
    }

    #[test]
    fn rejects_card_without_end_marker() {
        let text = format!("{CARD_START_MARKER}\n{}", card_json(CARD_MAGIC));
        assert!(parse_cards(&text).is_err());
    }

    #[test]
    fn ignores_plain_text() {
        assert!(!contains_card("https://example.com/a.m3u8|名称"));
        assert!(parse_cards("https://example.com/a.m3u8")
            .unwrap()
            .is_empty());
    }

    #[test]
    fn parses_multiple_cards() {
        let text = format!(
            "{}\n{}",
            wrap(&card_json(CARD_MAGIC)),
            wrap(&card_json(CARD_MAGIC))
        );
        assert_eq!(parse_cards(&text).unwrap().len(), 2);
    }

    #[test]
    fn derives_base_url_from_first_absolute_segment() {
        let cards = parse_cards(&wrap(&card_json(CARD_MAGIC))).unwrap();
        let base = card_base_url(&cards[0].content, &cards[0].headers).unwrap();
        assert_eq!(base, SEGMENT);
    }

    #[test]
    fn falls_back_to_referer_without_absolute_segment() {
        let mut headers = HashMap::new();
        headers.insert("referer".to_string(), PLAY_PAGE.to_string());
        let content = "#EXTM3U\n#EXTINF:8,\nsegment-0.ts\n#EXT-X-ENDLIST\n";
        assert_eq!(card_base_url(content, &headers).unwrap(), PLAY_PAGE);
    }

    #[test]
    fn resolves_relative_segments_against_content_base() {
        let mut headers = HashMap::new();
        headers.insert("referer".to_string(), PLAY_PAGE.to_string());
        let content =
            format!("#EXTM3U\n#EXTINF:8,\n{SEGMENT}\n#EXTINF:8,\nC-001.png\n#EXT-X-ENDLIST\n");
        let base = card_base_url(&content, &headers).unwrap();
        let playlist = parse_inline_playlist(&content, &base).unwrap();
        assert_eq!(playlist.segments[0].url, SEGMENT);
        // 相对路径按正文里第一个绝对地址所在目录解析，而不是按播放页地址。
        assert_eq!(
            playlist.segments[1].url,
            "https://cdn.example.com/hls/C-001.png"
        );
    }

    #[test]
    fn rejects_live_playlist() {
        let content = format!("#EXTM3U\n#EXTINF:8,\n{SEGMENT}\n");
        assert!(matches!(
            parse_inline_playlist(&content, SEGMENT),
            Err(CoreError::LiveStream)
        ));
    }

    #[test]
    fn rejects_master_playlist() {
        let content =
            "#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=1000\nhttps://cdn.example.com/low.m3u8\n";
        assert!(matches!(
            parse_inline_playlist(content, "https://cdn.example.com/index.m3u8"),
            Err(CoreError::InvalidPlaylistDetail(_))
        ));
    }
}
