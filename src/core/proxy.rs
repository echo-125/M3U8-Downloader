use std::{collections::HashMap, time::Duration};

use reqwest::{Client, Proxy};

use crate::{
    config::{ProxyScheme, Settings},
    core::{error::CoreError, headers::validate_headers},
};

/// 连接超时：建立连接（含 TLS 握手）的上限。
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// 单个请求的总超时，从开始连接一直覆盖到响应体读完。
///
/// 不能照「请求应该很快」的直觉设成几十秒：分片可能有几十 MB，慢速网络下光传输就
/// 要几分钟，而这个超时覆盖整个过程——设短了会让大分片必然超时，再被重试逻辑重下
/// 三次，最后仍是失败。区分「传得慢」和「根本没在传」是下载侧空闲超时的事
/// （见 `downloader::STALLED_TIMEOUT`），这里只兜底防止极慢的流无限占着连接。
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(300);

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
    // IPv6 地址必须包上方括号才是合法 URL：先剥掉用户输入里的方括号，
    // 再按是否含冒号统一补回，带不带括号的写法都能构建。
    let host = host
        .trim()
        .trim_matches(|character| character == '[' || character == ']');
    let host = if host.contains(':') {
        format!("[{host}]")
    } else {
        host.to_string()
    };
    let scheme = match scheme {
        ProxyScheme::Http => "http",
        ProxyScheme::Https => "https",
        ProxyScheme::Socks5 => "socks5",
    };
    let url = format!("{scheme}://{host}:{port}");
    Proxy::all(url).map_err(|_| CoreError::InvalidInput("代理地址无效".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_ipv6_host_with_or_without_brackets() {
        // IPv6 主机不带方括号拼出的 URL 非法，构建代理必然失败；两种写法都要能过。
        assert!(build_proxy(&ProxyScheme::Http, "[::1]", 8080).is_ok());
        assert!(build_proxy(&ProxyScheme::Http, "::1", 8080).is_ok());
        assert!(build_proxy(&ProxyScheme::Socks5, "2001:db8::1", 1080).is_ok());
    }

    #[test]
    fn keeps_plain_host_untouched() {
        assert!(build_proxy(&ProxyScheme::Http, "127.0.0.1", 8080).is_ok());
        assert!(build_proxy(&ProxyScheme::Https, "proxy.example.com", 443).is_ok());
    }
}
