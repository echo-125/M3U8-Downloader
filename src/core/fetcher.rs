use std::collections::HashMap;

use reqwest::Response;

use crate::config::Settings;
use crate::core::{
    error::CoreError,
    format::{diagnostic_message, error_detail, is_error_response},
    playlist::{parse_playlist, select_best_variant, MediaPlaylist, Playlist},
    proxy::build_client,
};

const MAX_PLAYLIST_DEPTH: usize = 5;

pub struct PlaylistFetcher {
    client: reqwest::Client,
}

impl PlaylistFetcher {
    pub fn new(
        settings: &Settings,
        request_headers: HashMap<String, String>,
    ) -> Result<Self, CoreError> {
        Ok(Self {
            client: build_client(settings, request_headers)?,
        })
    }

    pub async fn fetch_media_playlist(
        &self,
        playlist_url: &str,
    ) -> Result<MediaPlaylist, CoreError> {
        let mut current_url = playlist_url.to_string();
        for _ in 0..MAX_PLAYLIST_DEPTH {
            let content = self.fetch_text(&current_url).await?;
            if is_error_response(content.as_bytes()) {
                let detail = error_detail(content.as_bytes())
                    .unwrap_or_else(|| diagnostic_message(content.as_bytes()));
                return Err(CoreError::InvalidPlaylistDetail(detail));
            }
            match parse_playlist(&content, &current_url)? {
                Playlist::Master(variants) => {
                    let best = select_best_variant(&variants).ok_or(CoreError::InvalidPlaylist)?;
                    current_url = best.url.clone();
                }
                Playlist::Media(media) => {
                    if !media.has_end_list {
                        return Err(CoreError::LiveStream);
                    }
                    return Ok(media);
                }
            }
        }
        Err(CoreError::InvalidPlaylist)
    }

    pub async fn fetch_text(&self, url: &str) -> Result<String, CoreError> {
        let response = self.send(url, None).await?;
        let charset = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .and_then(content_type_charset);
        let bytes = response
            .bytes()
            .await
            .map_err(|_| CoreError::Network("读取播放列表失败".into()))?;
        Ok(decode_playlist(&bytes, charset.as_deref()))
    }

    pub async fn fetch_key(&self, url: &str) -> Result<Vec<u8>, CoreError> {
        let response = self.send(url, None).await?;
        response
            .bytes()
            .await
            .map(|bytes| bytes.to_vec())
            .map_err(|_| CoreError::Network("读取密钥失败".into()))
    }

    pub async fn send(&self, url: &str, range: Option<String>) -> Result<Response, CoreError> {
        let response = self.send_raw(url, range).await?;
        let status = response.status().as_u16();
        match status {
            200..=299 => Ok(response),
            403 => Err(CoreError::Forbidden),
            404 => Err(CoreError::NotFound),
            429 => Err(CoreError::TooManyRequests),
            500..=599 => Err(CoreError::ServerError(status)),
            status => Err(CoreError::HttpStatus { status }),
        }
    }

    pub async fn send_raw(&self, url: &str, range: Option<String>) -> Result<Response, CoreError> {
        let mut request = self.client.get(url);
        if let Some(range) = range {
            request = request.header(reqwest::header::RANGE, range);
        }
        let response = request.send().await.map_err(|error| {
            if error.is_timeout() {
                CoreError::Timeout
            } else {
                CoreError::Network("连接失败或响应中断".into())
            }
        })?;
        Ok(response)
    }
}

/// 播放列表解码：优先响应头声明的字符集，其次 UTF-8，最后回退中文站点常见编码。
fn decode_playlist(bytes: &[u8], declared_charset: Option<&str>) -> String {
    if let Some(charset) = declared_charset {
        if let Some(encoding) = encoding_rs::Encoding::for_label(charset.as_bytes()) {
            return encoding.decode(bytes).0.into_owned();
        }
    }
    if let Ok(text) = std::str::from_utf8(bytes) {
        return text.to_string();
    }
    for encoding in [encoding_rs::BIG5, encoding_rs::GB18030] {
        let (text, _, had_errors) = encoding.decode(bytes);
        if !had_errors {
            return text.into_owned();
        }
    }
    encoding_rs::GB18030.decode(bytes).0.into_owned()
}

fn content_type_charset(content_type: &str) -> Option<String> {
    content_type.split(';').skip(1).find_map(|parameter| {
        let (key, value) = parameter.split_once('=')?;
        key.trim()
            .eq_ignore_ascii_case("charset")
            .then(|| value.trim().trim_matches('"').to_string())
    })
}
