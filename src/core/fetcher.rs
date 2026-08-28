use std::collections::HashMap;

use reqwest::Response;

use crate::config::Settings;
use crate::core::{
    error::CoreError,
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
        response
            .text()
            .await
            .map_err(|_| CoreError::Network("读取播放列表失败".into()))
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
