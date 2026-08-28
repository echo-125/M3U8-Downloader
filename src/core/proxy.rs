use std::{collections::HashMap, time::Duration};

use reqwest::{Client, Proxy};

use crate::{
    config::{ProxyScheme, Settings},
    core::{error::CoreError, headers::validate_headers},
};

pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

pub fn build_client(
    settings: &Settings,
    request_headers: HashMap<String, String>,
) -> Result<Client, CoreError> {
    let headers = validate_headers(request_headers)?;
    let mut builder = Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .connect_timeout(CONNECT_TIMEOUT)
        .redirect(reqwest::redirect::Policy::limited(10));

    if settings.proxy.enabled {
        let proxy = build_proxy(
            &settings.proxy.scheme,
            &settings.proxy.host,
            settings.proxy.port,
        )?;
        let proxy = if !settings.proxy.username.is_empty() || !settings.proxy.password.is_empty() {
            proxy.basic_auth(&settings.proxy.username, &settings.proxy.password)
        } else {
            proxy
        };
        builder = builder.proxy(proxy);
    }

    let mut default_headers = reqwest::header::HeaderMap::new();
    if !headers.contains_key("User-Agent") {
        default_headers.insert(
            "User-Agent",
            "CatCatchAssistant/0.1"
                .parse()
                .map_err(|_| CoreError::InvalidInput("默认 User-Agent 无效".into()))?,
        );
    }
    for (name, value) in headers {
        default_headers.insert(
            reqwest::header::HeaderName::from_bytes(name.as_bytes())
                .map_err(|_| CoreError::InvalidInput(format!("请求头 {name} 无效")))?,
            reqwest::header::HeaderValue::from_str(&value)
                .map_err(|_| CoreError::InvalidInput(format!("请求头 {name} 的值无效")))?,
        );
    }

    builder
        .default_headers(default_headers)
        .build()
        .map_err(|_| CoreError::Network("创建 HTTP 客户端失败".into()))
}

fn build_proxy(scheme: &ProxyScheme, host: &str, port: u16) -> Result<Proxy, CoreError> {
    let host = host.trim_matches(|character| character == '[' || character == ']');
    let scheme = match scheme {
        ProxyScheme::Http => "http",
        ProxyScheme::Https => "https",
        ProxyScheme::Socks5 => "socks5",
    };
    let url = format!("{scheme}://{host}:{port}");
    Proxy::all(url).map_err(|_| CoreError::InvalidInput("代理地址无效".into()))
}
