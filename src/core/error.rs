use thiserror::Error;

#[derive(Debug, Error)]
pub enum CoreError {
    #[error("请求地址无效")]
    InvalidUrl,
    #[error("直播流暂不支持")]
    LiveStream,
    #[error("播放列表格式无效")]
    InvalidPlaylist,
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
