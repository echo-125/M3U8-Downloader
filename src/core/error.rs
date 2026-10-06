use thiserror::Error;

#[derive(Debug, Error)]
pub enum CoreError {
    #[error("请求地址无效")]
    InvalidUrl,
    #[error("直播流暂不支持")]
    LiveStream,
    #[error("播放列表格式无效")]
    InvalidPlaylist,
    #[error("播放列表无效：{0}")]
    InvalidPlaylistDetail(String),
    #[error("网络请求失败：{0}")]
    Network(String),
    #[error("服务器返回错误：{status}")]
    HttpStatus { status: u16 },
    #[error("请求被拒绝（403），请检查防盗链或自定义请求头")]
    Forbidden,
    #[error("资源不存在（404）")]
    NotFound,
    #[error("请求过于频繁，请稍后重试")]
    TooManyRequests,
    #[error("服务器错误（{0}）")]
    ServerError(u16),
    #[error("请求超时")]
    Timeout,
    #[error("任务已取消")]
    Canceled,
    #[error("文件操作失败：{0}")]
    Io(String),
    #[error("密钥无效")]
    InvalidKey,
    #[error("解密失败，可能密钥或 IV 不正确")]
    Decrypt,
    #[error("{count} 个分片解密失败，可能密钥或 IV 不正确，原始密文已保存到 _debug 目录")]
    UndecryptedSegments { count: usize },
    #[error("分片内容异常：{0}")]
    InvalidSegment(String),
    #[error("不支持的加密方式：{0}")]
    UnsupportedEncryption(String),
    #[error("ffmpeg 不可用或转换失败：{0}")]
    Ffmpeg(String),
    #[error("输入无效：{0}")]
    InvalidInput(String),
}

impl CoreError {
    pub fn user_message(&self) -> String {
        self.to_string()
    }
}

/// 沿 `source()` 链拼接错误的完整原因。
///
/// reqwest/hyper 的 Display 只有一句笼统的「error sending request」，
/// 真实原因（DNS 解析失败、证书无效、连接被重置）都在 source 链上；
/// 不把链拼出来，任务失败日志里就没有可排查的现场。限深 5 层，
/// 防御异常错误实现成环或链过长把日志撑爆。
pub(crate) fn error_chain(error: &(dyn std::error::Error + 'static)) -> String {
    let mut chain = error.to_string();
    let mut current = error.source();
    for _ in 0..5 {
        let Some(source) = current else {
            break;
        };
        chain.push('：');
        chain.push_str(&source.to_string());
        current = source.source();
    }
    chain
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 测试用的链式错误：消息固定，source 指向下一层。
    #[derive(Debug)]
    struct Wrapped(&'static str, Option<Box<dyn std::error::Error + 'static>>);

    impl std::fmt::Display for Wrapped {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(formatter, "{}", self.0)
        }
    }

    impl std::error::Error for Wrapped {
        fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
            self.1.as_deref()
        }
    }

    #[test]
    fn error_chain_joins_sources() {
        let error = Wrapped(
            "外层",
            Some(Box::new(Wrapped(
                "中层",
                Some(Box::new(Wrapped("根因", None))),
            ))),
        );
        assert_eq!(error_chain(&error), "外层：中层：根因");
    }

    #[test]
    fn error_chain_limits_depth() {
        // 从第 7 层往外逐层包裹：顶层是第 1 层，source 链依次是第 2 到第 7 层。
        let mut error: Box<dyn std::error::Error + 'static> = Box::new(Wrapped("第7层", None));
        for label in ["第6层", "第5层", "第4层", "第3层", "第2层", "第1层"] {
            error = Box::new(Wrapped(label, Some(error)));
        }
        let chain = error_chain(&*error);
        assert!(chain.starts_with("第1层：第2层："), "完整链：{chain}");
        assert!(chain.contains("第6层"), "限深内的 source 都应保留：{chain}");
        assert!(!chain.contains("第7层"), "超过限深的层不应拼接：{chain}");
    }
}
