//! 端到端测试：用本地 HTTP 服务器驱动「解析 → 下载 → 解密 → 合并」全链路。
//!
//! 本项目是二进制 crate，没有 lib.rs，外部集成测试无法访问内部模块，
//! 所以测试放在 src 内部并用 `#[cfg(test)]` 隔离，只在 `cargo test` 时编译。
//! 全程不访问外网，服务器监听 127.0.0.1 随机端口，可重复运行。
//!
//! 覆盖的是验收清单里成本最高、最容易出错的几项：普通 TS 下载、AES-128 解密、
//! 主播放列表选最高带宽、分片级断点续传。

use std::{
    collections::HashMap,
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::{SystemTime, UNIX_EPOCH},
};

use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    runtime::Runtime,
    sync::{mpsc, Semaphore},
};
use tokio_util::sync::CancellationToken;

use crate::{
    config::Settings,
    core::{
        downloader::{run_task, DownloadTask},
        error::CoreError,
        events::{NewTask, TaskCommand, TaskEvent, TaskSnapshot, TaskStatus},
        fetcher::PlaylistFetcher,
        manager::TaskManager,
        merge::merge_segments,
        paste::parse_inline_playlist,
        task::{discover_task_manifests, TaskManifest, TaskRegistry},
    },
};

const TS_PACKET_SIZE: usize = 188;

/// 一次响应：状态码 + 响应体，可选 Retry-After 头与响应体延迟。
#[derive(Clone)]
struct TestResponse {
    status: u16,
    body: Vec<u8>,
    retry_after: Option<String>,
    /// 响应头发出后、写响应体之前的停顿毫秒数。用来把任务稳定停在
    /// 「已收到 200、正在读响应体」这一刻，而不是靠本机下载速度碰运气。
    delay_ms: u64,
}

impl TestResponse {
    fn ok(body: Vec<u8>) -> Self {
        Self {
            status: 200,
            body,
            retry_after: None,
            delay_ms: 0,
        }
    }

    /// 构造延迟发出响应体的 200 响应，用于模拟下载进行中的状态。
    fn delayed(body: Vec<u8>, delay_ms: u64) -> Self {
        Self {
            delay_ms,
            ..Self::ok(body)
        }
    }
}

/// 有状态路由：每命中一次消耗一条脚本响应，脚本耗尽后回落到 `fallback`。
struct ScriptedRoute {
    script: Vec<TestResponse>,
    fallback: TestResponse,
    hits: AtomicUsize,
}

impl ScriptedRoute {
    fn next_response(&self) -> TestResponse {
        let hit = self.hits.fetch_add(1, Ordering::SeqCst);
        self.script
            .get(hit)
            .cloned()
            .unwrap_or_else(|| self.fallback.clone())
    }

    fn hit_count(&self) -> usize {
        self.hits.load(Ordering::SeqCst)
    }
}

#[derive(Clone)]
enum TestRoute {
    Static(TestResponse),
    Scripted(Arc<ScriptedRoute>),
}

impl From<Vec<u8>> for TestRoute {
    fn from(body: Vec<u8>) -> Self {
        TestRoute::Static(TestResponse::ok(body))
    }
}

/// 极简 HTTP 服务器：按请求路径返回预设内容，够驱动下载核心即可，不实现完整 HTTP 语义。
/// 脚本化路由把状态放在共享计数器里，让「前 N 次 429、之后 200」这类序列可以被测试。
struct TestServer {
    address: SocketAddr,
}

impl TestServer {
    async fn start(routes: HashMap<String, TestRoute>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("绑定本地端口失败");
        let address = listener.local_addr().expect("读取本地端口失败");
        tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                let routes = routes.clone();
                tokio::spawn(async move { serve_once(&mut socket, &routes).await });
            }
        });
        Self { address }
    }

    fn url(&self, path: &str) -> String {
        format!("http://{}{}", self.address, path)
    }
}

fn status_text(status: u16) -> &'static str {
    match status {
        200 => "OK",
        404 => "Not Found",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        _ => "Unexpected",
    }
}

async fn serve_once(socket: &mut TcpStream, routes: &HashMap<String, TestRoute>) {
    let mut request = Vec::new();
    let mut chunk = [0_u8; 4096];
    // 读到请求头结束即可；请求体对 GET 没有意义，额外给一个上限防止异常连接拖住测试。
    while request.len() < 64 * 1024 {
        let read = socket.read(&mut chunk).await.unwrap_or(0);
        if read == 0 {
            break;
        }
        request.extend_from_slice(&chunk[..read]);
        if request.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
    }
    let path = String::from_utf8_lossy(&request)
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap_or("/")
        .to_string();

    let response = match routes.get(&path) {
        Some(TestRoute::Static(response)) => response.clone(),
        Some(TestRoute::Scripted(route)) => route.next_response(),
        None => TestResponse {
            status: 404,
            body: Vec::new(),
            retry_after: None,
            delay_ms: 0,
        },
    };
    let mut header = format!(
        "HTTP/1.1 {} {}\r\nContent-Length: {}\r\nContent-Type: application/octet-stream\r\nConnection: close\r\n",
        response.status,
        status_text(response.status),
        response.body.len()
    );
    if let Some(value) = &response.retry_after {
        header.push_str(&format!("Retry-After: {value}\r\n"));
    }
    header.push_str("\r\n");
    // 响应头先发出去，再按需要停顿：客户端此时已拿到 200 并停在读响应体这一步，
    // 这就是「下载进行中」的稳定时刻，也是删除 / 清空要处理的时刻。
    let _ = socket.write_all(header.as_bytes()).await;
    let _ = socket.flush().await;
    if response.delay_ms > 0 {
        tokio::time::sleep(std::time::Duration::from_millis(response.delay_ms)).await;
    }
    let _ = socket.write_all(&response.body).await;
    let _ = socket.flush().await;
}

/// 生成能被识别为 TS 的数据：若干个 188 字节的包，每个包以同步字节 0x47 开头。
/// 填充值按分片区分，合并后据此校验顺序。
fn ts_segment(packets: usize, fill: u8) -> Vec<u8> {
    let mut data = Vec::with_capacity(packets * TS_PACKET_SIZE);
    for _ in 0..packets {
        data.push(0x47);
        data.extend(vec![fill; TS_PACKET_SIZE - 1]);
    }
    data
}

fn media_playlist(base: &str, segment_count: usize, key_line: Option<&str>) -> String {
    let mut lines = vec![
        "#EXTM3U".to_string(),
        "#EXT-X-VERSION:3".to_string(),
        "#EXT-X-TARGETDURATION:10".to_string(),
        "#EXT-X-MEDIA-SEQUENCE:0".to_string(),
    ];
    if let Some(key_line) = key_line {
        lines.push(key_line.to_string());
    }
    for index in 0..segment_count {
        lines.push("#EXTINF:10.0,".to_string());
        lines.push(format!("{base}/seg{index}.ts"));
    }
    lines.push("#EXT-X-ENDLIST".to_string());
    lines.push(String::new());
    lines.join("\n")
}

fn temp_dir(tag: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_nanos())
        .unwrap_or_default();
    let path = std::env::temp_dir().join(format!("cat-catch-e2e-{tag}-{nanos}"));
    std::fs::create_dir_all(&path).expect("创建临时目录失败");
    path
}

/// 测试用配置：关掉 ffmpeg 与尾部加速，让合并路径和输出内容确定下来。
fn test_settings() -> Settings {
    let mut settings = Settings::default();
    // 测试机未必装了 ffmpeg，关掉才能确定地走「TS 直接拼接」路径。
    settings.ffmpeg.auto_detect = false;
    settings.ffmpeg.manual_path = String::new();
    // 保留临时文件，方便断言分片确实落盘。
    settings.auto_cleanup = false;
    settings.max_workers = 4;
    settings.tail_threshold = 100;
    settings.tail_boost = 1;
    settings
}

/// 返回最终快照，以及过程中上报的全部快照。
/// 界面只靠这些快照刷新进度，所以事件流本身也要端到端验证，不能只看返回值。
async fn run_download_with_events(
    directory: &Path,
    playlist_url: &str,
    name: &str,
    settings: Settings,
) -> (TaskSnapshot, Vec<TaskSnapshot>) {
    let manifest = TaskManifest::new(1, playlist_url, name, directory, 4, HashMap::new())
        .expect("创建任务失败");
    let (sender, mut receiver) = mpsc::unbounded_channel();
    let task = DownloadTask {
        manifest,
        settings,
        event_sender: sender,
        cancellation_token: CancellationToken::new(),
        global_permits: Arc::new(Semaphore::new(8)),
    };
    let snapshot = run_task(task).await.expect("任务执行失败");
    let mut snapshots = Vec::new();
    while let Ok(event) = receiver.try_recv() {
        if let TaskEvent::Snapshot(value) = event {
            snapshots.push(value);
        }
    }
    (snapshot, snapshots)
}

/// 返回 `run_task` 的原始结果，供需要断言失败路径的用例使用。
/// 成功路径请改用 `run_download`，它直接给出快照。
async fn run_download_result(
    directory: &Path,
    playlist_url: &str,
    name: &str,
    settings: Settings,
) -> Result<TaskSnapshot, CoreError> {
    run_download_result_with_logs(directory, playlist_url, name, settings)
        .await
        .0
}

/// 同 `run_download_result`，但一并给出过程中产生的日志消息，供断言日志行为。
async fn run_download_result_with_logs(
    directory: &Path,
    playlist_url: &str,
    name: &str,
    settings: Settings,
) -> (Result<TaskSnapshot, CoreError>, Vec<String>) {
    let manifest = TaskManifest::new(1, playlist_url, name, directory, 4, HashMap::new())
        .expect("创建任务失败");
    let (sender, mut receiver) = mpsc::unbounded_channel();
    let task = DownloadTask {
        manifest,
        settings,
        event_sender: sender,
        cancellation_token: CancellationToken::new(),
        global_permits: Arc::new(Semaphore::new(8)),
    };
    let result = run_task(task).await;
    let mut logs = Vec::new();
    while let Ok(event) = receiver.try_recv() {
        if let TaskEvent::Log { message, .. } = event {
            logs.push(message);
        }
    }
    (result, logs)
}

async fn run_download(
    directory: &Path,
    playlist_url: &str,
    name: &str,
    settings: Settings,
) -> TaskSnapshot {
    run_download_with_events(directory, playlist_url, name, settings)
        .await
        .0
}

fn output_of(snapshot: &TaskSnapshot) -> PathBuf {
    PathBuf::from(
        snapshot
            .output_path
            .as_ref()
            .expect("任务未给出输出文件路径"),
    )
}

fn flatten(segments: &[Vec<u8>]) -> Vec<u8> {
    segments
        .iter()
        .flat_map(|data| data.iter().copied())
        .collect()
}

#[tokio::test]
async fn downloads_and_merges_ts_playlist() {
    let directory = temp_dir("ts");
    let segments: Vec<Vec<u8>> = (0..3).map(|index| ts_segment(6, index as u8 + 1)).collect();
    let mut routes: HashMap<String, TestRoute> = HashMap::new();
    for (index, data) in segments.iter().enumerate() {
        routes.insert(format!("/seg{index}.ts"), data.clone().into());
    }
    routes.insert(
        "/video.m3u8".to_string(),
        media_playlist("", 3, None).into_bytes().into(),
    );

    let server = TestServer::start(routes).await;
    let (snapshot, snapshots) = run_download_with_events(
        &directory,
        &server.url("/video.m3u8"),
        "video",
        test_settings(),
    )
    .await;

    assert_eq!(snapshot.status, TaskStatus::Completed);
    let output = output_of(&snapshot);
    assert!(output.is_file(), "输出文件不存在：{}", output.display());
    // 合并结果是三个分片按序拼接。
    assert_eq!(
        std::fs::read(&output).expect("读取输出失败"),
        flatten(&segments)
    );

    // 界面靠快照事件刷新进度条，事件流断了界面就是静止的，这里一并验证。
    assert!(
        snapshots
            .iter()
            .any(|value| value.status == TaskStatus::Downloading),
        "过程中应当上报「下载中」快照"
    );
    assert_eq!(
        snapshots.last().map(|value| value.status),
        Some(TaskStatus::Completed),
        "最后一个快照应当停留在「已完成」"
    );
    // 完成度必须单调递增到满，否则进度条会回跳。
    let mut previous = 0.0_f32;
    for value in &snapshots {
        assert!(
            value.progress + f32::EPSILON >= previous,
            "进度回跳：{} -> {}",
            previous,
            value.progress
        );
        previous = value.progress;
    }
    assert!((previous - 1.0).abs() < f32::EPSILON, "最终进度应为 100%");

    let _ = std::fs::remove_dir_all(&directory);
}

/// 两个同名任务并发下载到同一目录时，中间文件必须不互相覆盖，
/// 否则会出现「文件名不同、内容却相同」的现象。
#[tokio::test]
async fn concurrent_same_name_tasks_produce_distinct_outputs() {
    let directory = temp_dir("concurrent");
    // 两个任务用不同填充值，合并后内容必然不同——这正是断言依据。
    let segments_a: Vec<Vec<u8>> = (0..2).map(|_| ts_segment(6, 0x11)).collect();
    let segments_b: Vec<Vec<u8>> = (0..2).map(|_| ts_segment(6, 0x22)).collect();

    let mut routes: HashMap<String, TestRoute> = HashMap::new();
    for (index, data) in segments_a.iter().enumerate() {
        routes.insert(format!("/a/seg{index}.ts"), data.clone().into());
    }
    for (index, data) in segments_b.iter().enumerate() {
        routes.insert(format!("/b/seg{index}.ts"), data.clone().into());
    }
    routes.insert(
        "/a.m3u8".to_string(),
        media_playlist("/a", 2, None).into_bytes().into(),
    );
    routes.insert(
        "/b.m3u8".to_string(),
        media_playlist("/b", 2, None).into_bytes().into(),
    );

    let server = TestServer::start(routes).await;
    let url_a = server.url("/a.m3u8");
    let url_b = server.url("/b.m3u8");
    let settings = test_settings();
    let dir = directory.clone();

    // 两个任务都用同一个名字，故意制造并发合并撞名的条件。
    let task_a = tokio::spawn(async move {
        let manifest =
            TaskManifest::new(1, &url_a, "video", &dir, 4, HashMap::new()).expect("创建任务失败");
        let (sender, _receiver) = mpsc::unbounded_channel();
        run_task(DownloadTask {
            manifest,
            settings,
            event_sender: sender,
            cancellation_token: CancellationToken::new(),
            global_permits: Arc::new(Semaphore::new(8)),
        })
        .await
        .expect("任务 A 失败")
    });
    let dir = directory.clone();
    let task_b = tokio::spawn(async move {
        let manifest =
            TaskManifest::new(2, &url_b, "video", &dir, 4, HashMap::new()).expect("创建任务失败");
        let (sender, _receiver) = mpsc::unbounded_channel();
        run_task(DownloadTask {
            manifest,
            settings: test_settings(),
            event_sender: sender,
            cancellation_token: CancellationToken::new(),
            global_permits: Arc::new(Semaphore::new(8)),
        })
        .await
        .expect("任务 B 失败")
    });

    let snapshot_a = task_a.await.expect("任务 A panic");
    let snapshot_b = task_b.await.expect("任务 B panic");

    let output_a = output_of(&snapshot_a);
    let output_b = output_of(&snapshot_b);
    // 名字相同会触发唯一化，文件名必然不同，但内容必须各是各的。
    assert_ne!(output_a, output_b, "输出文件路径不应相同");
    let content_a = std::fs::read(&output_a).expect("读取 A 输出失败");
    let content_b = std::fs::read(&output_b).expect("读取 B 输出失败");
    assert_ne!(
        content_a, content_b,
        "两个任务输出了相同内容——并发合并中间文件互相覆盖了"
    );
    assert_eq!(content_a, flatten(&segments_a));
    assert_eq!(content_b, flatten(&segments_b));

    let _ = std::fs::remove_dir_all(&directory);
}

/// 同时并发合并两个同名任务的分片，输出路径必须不同、内容必须各是各的。
///
/// 与 `concurrent_same_name_tasks_produce_distinct_outputs` 不同：这里直接调
/// `merge_segments` 并用 `tokio::join!` 让两次合并**严格同时启动**，没有下载阶段
/// 的时间差，确保合并阶段的并发重叠是确定的（而非依赖下载快慢的巧合时序）。
#[tokio::test]
async fn concurrent_merges_same_name_produce_distinct_outputs() {
    let directory = temp_dir("concurrent-merge");
    // 两个任务各自的中间分片，合并后内容必然不同。
    let segment_a = ts_segment(6, 0x33);
    let segment_b = ts_segment(6, 0x44);
    let segment_path_a = directory.join("seg_a.ts");
    let segment_path_b = directory.join("seg_b.ts");
    std::fs::write(&segment_path_a, &segment_a).expect("写入 A 分片失败");
    std::fs::write(&segment_path_b, &segment_b).expect("写入 B 分片失败");

    // 两路合并同时启动，各自输出 video.ts 应被唯一化，内容各是各的。
    // 分片列表先绑定 let：直接内联临时切片会被 async 函数持有期间提前释放（E0716）。
    let segments_a = vec![segment_path_a];
    let segments_b = vec![segment_path_b];
    let token_a = CancellationToken::new();
    let token_b = CancellationToken::new();
    let (result_a, result_b) = tokio::join!(
        merge_segments(
            &segments_a,
            None,
            &directory,
            "video",
            false,
            None,
            &token_a
        ),
        merge_segments(
            &segments_b,
            None,
            &directory,
            "video",
            false,
            None,
            &token_b
        ),
    );
    let output_a = result_a.expect("合并 A 失败").output_path;
    let output_b = result_b.expect("合并 B 失败").output_path;

    assert_ne!(output_a, output_b, "并发同名合并的输出路径必须不同");
    assert_eq!(
        std::fs::read(&output_a).expect("读取 A 输出失败"),
        segment_a
    );
    assert_eq!(
        std::fs::read(&output_b).expect("读取 B 输出失败"),
        segment_b
    );

    let _ = std::fs::remove_dir_all(&directory);
}

/// 合并完成后输出目录里不应该残留空占位文件。
///
/// `unique_output_path` 通过 create_new 占位保证路径唯一；正常路径下调用方的
/// rename / ffmpeg 覆盖会清理它，但失败路径上必须显式删除，否则会留下大小为 0
/// 的同名文件污染输出目录、并让下次同名任务拿不到干净的原名。
#[tokio::test]
async fn merge_leaves_no_empty_reservation_files() {
    let directory = temp_dir("no-residue");
    let segments: Vec<Vec<u8>> = (0..3).map(|index| ts_segment(6, index as u8 + 1)).collect();
    let mut routes: HashMap<String, TestRoute> = HashMap::new();
    for (index, data) in segments.iter().enumerate() {
        routes.insert(format!("/seg{index}.ts"), data.clone().into());
    }
    routes.insert(
        "/video.m3u8".to_string(),
        media_playlist("", 3, None).into_bytes().into(),
    );

    let server = TestServer::start(routes).await;
    let snapshot = run_download(
        &directory,
        &server.url("/video.m3u8"),
        "video",
        test_settings(),
    )
    .await;
    assert_eq!(snapshot.status, TaskStatus::Completed);

    // 扫描输出目录：合并期间生成的中间文件（.cat-catch- 前缀的临时文件、raw.mp4、
    // .temporary.ts、concat 列表）应当全部清理；空占位文件更不能留。
    // 目录（含任务临时目录 .cat-catch-tasks/）是合法存在，只检查文件。
    for entry in std::fs::read_dir(&directory)
        .expect("读取输出目录失败")
        .flatten()
    {
        let path = entry.path();
        if path.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        assert!(!name.starts_with(".cat-catch-"), "残留合并中间文件：{name}");
        // 排除合法输出 video.ts，看还有没有别的文件。
        if name == "video.ts" {
            continue;
        }
        panic!("输出目录有未预期的文件：{name}");
    }

    let _ = std::fs::remove_dir_all(&directory);
}

/// 成功后的临时目录清理放在阻塞线程池里跑，行为本身用这条用例锁定：
/// 任务完成后任务目录（manifest、分片）必须被删干净。其余用例全部
/// auto_cleanup=false，这条路径此前没有任何覆盖。
#[tokio::test]
async fn auto_cleanup_removes_task_directory() {
    let directory = temp_dir("auto-cleanup");
    let segments: Vec<Vec<u8>> = (0..3).map(|index| ts_segment(6, index as u8 + 1)).collect();
    let mut routes: HashMap<String, TestRoute> = HashMap::new();
    for (index, data) in segments.iter().enumerate() {
        routes.insert(format!("/seg{index}.ts"), data.clone().into());
    }
    routes.insert(
        "/video.m3u8".to_string(),
        media_playlist("", 3, None).into_bytes().into(),
    );

    let server = TestServer::start(routes).await;
    let manifest = TaskManifest::new(
        1,
        &server.url("/video.m3u8"),
        "video",
        &directory,
        4,
        HashMap::new(),
    )
    .expect("创建任务失败");
    let task_directory = manifest.task_directory();
    let mut settings = test_settings();
    settings.auto_cleanup = true;
    let (sender, _receiver) = mpsc::unbounded_channel();
    let snapshot = run_task(DownloadTask {
        manifest,
        settings,
        event_sender: sender,
        cancellation_token: CancellationToken::new(),
        global_permits: Arc::new(Semaphore::new(8)),
    })
    .await
    .expect("任务执行失败");

    // 清理在 run_task 返回前完成，直接断言即可。
    assert_eq!(snapshot.status, TaskStatus::Completed);
    assert!(
        !task_directory.exists(),
        "任务完成后临时目录应当被清理：{}",
        task_directory.display()
    );

    let _ = std::fs::remove_dir_all(&directory);
}

/// send_raw 的非超时错误必须带上底层原因：只看「连接失败」一句排查不了
/// 防盗链、DNS、证书这类问题。用「接受即断开」的监听器制造连接层失败。
#[tokio::test]
async fn connection_failure_carries_reason() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("绑定本地端口失败");
    let address = listener.local_addr().expect("读取本地端口失败");
    tokio::spawn(async move {
        while let Ok((socket, _)) = listener.accept().await {
            drop(socket);
        }
    });

    let fetcher = PlaylistFetcher::new(&test_settings(), HashMap::new()).expect("创建抓取器失败");
    let error = fetcher
        .fetch_text(&format!("http://{address}/video.m3u8"))
        .await
        .expect_err("连接被对端断开时应当失败");
    assert!(
        error.to_string().contains("连接失败"),
        "错误文案应标明连接失败并附底层原因：{error}"
    );
}

/// 播放列表与密钥按小文件处理，超过体积上限的响应必须被掐断，
/// 不能任由异常服务器在空闲超时的保护下持续灌数据。
#[tokio::test]
async fn small_body_rejects_oversized_response() {
    // ts_segment(93_000) 约 17.5MB，超过 16MB 上限即可触发。
    let oversized = ts_segment(93_000, 1);
    let mut routes: HashMap<String, TestRoute> = HashMap::new();
    routes.insert("/big.m3u8".to_string(), oversized.into());
    let server = TestServer::start(routes).await;

    let fetcher = PlaylistFetcher::new(&test_settings(), HashMap::new()).expect("创建抓取器失败");
    let error = fetcher
        .fetch_text(&server.url("/big.m3u8"))
        .await
        .expect_err("超过体积上限的响应应当被拒绝");
    assert!(
        error.to_string().contains("响应体积超过上限"),
        "错误文案应标明体积超限：{error}"
    );
}

#[tokio::test]
async fn decrypts_aes128_segments_before_merging() {
    use aes::Aes128;
    use cbc::cipher::{block_padding::Pkcs7, BlockEncryptMut, KeyIvInit};
    type Aes128CbcEnc = cbc::Encryptor<Aes128>;

    let directory = temp_dir("aes");
    let key = [0x2b_u8; 16];
    let iv = [0x7c_u8; 16];
    let plaintexts: Vec<Vec<u8>> = (0..3).map(|index| ts_segment(6, index as u8 + 1)).collect();
    let encrypted: Vec<Vec<u8>> = plaintexts
        .iter()
        .map(|data| {
            Aes128CbcEnc::new(&key.into(), &iv.into()).encrypt_padded_vec_mut::<Pkcs7>(data)
        })
        .collect();

    // IV 由加密用的字节数组直接生成，避免手写字面量与加密时的 IV 不一致。
    let iv_hex: String = iv.iter().map(|byte| format!("{byte:02x}")).collect();
    let key_line = format!("#EXT-X-KEY:METHOD=AES-128,URI=\"/key.bin\",IV=0x{iv_hex}");

    let mut routes: HashMap<String, TestRoute> = HashMap::new();
    for (index, data) in encrypted.iter().enumerate() {
        routes.insert(format!("/seg{index}.ts"), data.clone().into());
    }
    routes.insert("/key.bin".to_string(), key.to_vec().into());
    routes.insert(
        "/video.m3u8".to_string(),
        media_playlist("", 3, Some(&key_line)).into_bytes().into(),
    );

    let server = TestServer::start(routes).await;
    let snapshot = run_download(
        &directory,
        &server.url("/video.m3u8"),
        "encrypted",
        test_settings(),
    )
    .await;

    assert_eq!(snapshot.status, TaskStatus::Completed);
    assert!(
        !snapshot.detail.contains("解密失败"),
        "出现了解密失败：{}",
        snapshot.detail
    );
    // 输出内容必须等于原始明文，说明解密环节真的生效了。
    assert_eq!(
        std::fs::read(output_of(&snapshot)).expect("读取输出失败"),
        flatten(&plaintexts)
    );

    let _ = std::fs::remove_dir_all(&directory);
}

/// 密钥不对时解密必然失败。这条路径过去会把密文写进正常分片路径：
/// 成品被污染，而断点续传按 is_file() 判定分片「已完成」，重启后不会重下，
/// 损坏无法自愈。这里锁定新行为——只归档、不落盘、任务明确失败。
#[tokio::test]
async fn undecrypted_segments_stay_off_disk_and_fail_the_task() {
    use aes::Aes128;
    use cbc::cipher::{block_padding::Pkcs7, BlockEncryptMut, KeyIvInit};
    type Aes128CbcEnc = cbc::Encryptor<Aes128>;

    let directory = temp_dir("badkey");
    let encryption_key = [0x2b_u8; 16];
    // 服务器给出的密钥与加密用的不是同一把：解密必然失败。
    let served_key = [0x9e_u8; 16];
    let iv = [0x7c_u8; 16];
    let plaintext = ts_segment(6, 1);
    let encrypted = Aes128CbcEnc::new(&encryption_key.into(), &iv.into())
        .encrypt_padded_vec_mut::<Pkcs7>(&plaintext);

    // IV 由加密用的字节数组生成，避免手写字面量与加密时的 IV 不一致。
    let iv_hex: String = iv.iter().map(|byte| format!("{byte:02x}")).collect();
    let key_line = format!("#EXT-X-KEY:METHOD=AES-128,URI=\"/key.bin\",IV=0x{iv_hex}");

    let mut routes: HashMap<String, TestRoute> = HashMap::new();
    routes.insert("/seg0.ts".to_string(), encrypted.clone().into());
    routes.insert("/key.bin".to_string(), served_key.to_vec().into());
    routes.insert(
        "/video.m3u8".to_string(),
        media_playlist("", 1, Some(&key_line)).into_bytes().into(),
    );

    let server = TestServer::start(routes).await;
    let result = run_download_result(
        &directory,
        &server.url("/video.m3u8"),
        "badkey",
        test_settings(),
    )
    .await;

    // 不能带着损坏的成品「完成」，必须失败并报出真实原因。
    let error = result.expect_err("密钥错误时任务应当失败");
    assert!(
        matches!(error, CoreError::UndecryptedSegments { count } if count == 1),
        "错误类型不符：{error}"
    );

    let manifest = discover_task_manifests(&directory)
        .into_iter()
        .next()
        .expect("任务 manifest 应留在磁盘上");
    assert!(
        !manifest.segment_path(0).is_file(),
        "解密失败的分片不能写进正常分片路径，否则续传会把它当已完成"
    );
    assert!(
        manifest.debug_path(0).is_file(),
        "原始密文应归档到 _debug 供排查"
    );
    assert_eq!(
        manifest.completed_segment_count().expect("统计分片失败"),
        0,
        "解密失败的分片不能计为已完成，否则续传会跳过它"
    );

    let _ = std::fs::remove_dir_all(&directory);
}

/// 合并要能响应取消。拼接在阻塞线程池里跑同步 IO，任务 abort 中断不了它，
/// 只能靠循环里自查；中断后中间文件必须清干净，否则会被手动合并扫描当成正常分片。
#[tokio::test]
async fn cancelled_merge_stops_and_leaves_no_intermediate_files() {
    let directory = temp_dir("cancel-merge");
    let mut paths = Vec::new();
    for index in 0..4 {
        let path = directory.join(format!("seg{index}.ts"));
        std::fs::write(&path, ts_segment(6, index as u8 + 1)).expect("写入分片失败");
        paths.push(path);
    }

    let token = CancellationToken::new();
    token.cancel();
    let result = merge_segments(&paths, None, &directory, "video", false, None, &token).await;

    assert!(
        matches!(result, Err(CoreError::Canceled)),
        "取消后合并应立即停下：{result:?}"
    );
    let leftovers: Vec<String> = std::fs::read_dir(&directory)
        .expect("读取目录失败")
        .flatten()
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with(".cat-catch-")
        })
        .map(|entry| entry.file_name().to_string_lossy().to_string())
        .collect();
    assert!(
        leftovers.is_empty(),
        "取消后不应残留中间文件：{leftovers:?}"
    );

    let _ = std::fs::remove_dir_all(&directory);
}

/// 密钥整体错误时每个分片都会解密失败，逐条记录会刷满日志面板，
/// 只应保留前几条（对应 downloader 的 MAX_DECRYPT_ERROR_LOGS）再汇总一句。
#[tokio::test]
async fn undecrypted_segments_logs_only_first_few() {
    use aes::Aes128;
    use cbc::cipher::{block_padding::Pkcs7, BlockEncryptMut, KeyIvInit};
    type Aes128CbcEnc = cbc::Encryptor<Aes128>;

    let directory = temp_dir("badkey-logs");
    let encryption_key = [0x2b_u8; 16];
    let served_key = [0x9e_u8; 16];
    let iv = [0x7c_u8; 16];
    // 失败数要超过限流阈值，才看得出「超出后只汇总一次」。
    let segment_count = 6;
    let plaintexts: Vec<Vec<u8>> = (0..segment_count)
        .map(|index| ts_segment(6, index as u8 + 1))
        .collect();
    let encrypted: Vec<Vec<u8>> = plaintexts
        .iter()
        .map(|data| {
            Aes128CbcEnc::new(&encryption_key.into(), &iv.into())
                .encrypt_padded_vec_mut::<Pkcs7>(data)
        })
        .collect();

    let iv_hex: String = iv.iter().map(|byte| format!("{byte:02x}")).collect();
    let key_line = format!("#EXT-X-KEY:METHOD=AES-128,URI=\"/key.bin\",IV=0x{iv_hex}");

    let mut routes: HashMap<String, TestRoute> = HashMap::new();
    for (index, data) in encrypted.iter().enumerate() {
        routes.insert(format!("/seg{index}.ts"), data.clone().into());
    }
    routes.insert("/key.bin".to_string(), served_key.to_vec().into());
    routes.insert(
        "/video.m3u8".to_string(),
        media_playlist("", segment_count, Some(&key_line))
            .into_bytes()
            .into(),
    );

    let server = TestServer::start(routes).await;
    let (result, logs) = run_download_result_with_logs(
        &directory,
        &server.url("/video.m3u8"),
        "badkey-logs",
        test_settings(),
    )
    .await;

    assert!(
        matches!(result, Err(CoreError::UndecryptedSegments { count }) if count == segment_count),
        "全部分片都该解密失败：{result:?}"
    );
    let detailed = logs
        .iter()
        .filter(|message| message.contains("解密失败："))
        .count();
    assert_eq!(detailed, 3, "逐条日志应限流到 3 条：{logs:?}");
    assert!(
        logs.iter()
            .any(|message| message.contains("后续不再逐条记录")),
        "超出限流后应补一条汇总：{logs:?}"
    );

    let _ = std::fs::remove_dir_all(&directory);
}

#[tokio::test]
async fn selects_highest_bandwidth_variant() {
    let directory = temp_dir("master");
    let master = concat!(
        "#EXTM3U\n",
        "#EXT-X-STREAM-INF:BANDWIDTH=800000\n",
        "/low.m3u8\n",
        "#EXT-X-STREAM-INF:BANDWIDTH=2500000\n",
        "/high.m3u8\n",
    );
    // 低码率 2 个分片、高码率 4 个分片，用填充值区分二者。
    let low_segments: Vec<Vec<u8>> = (0..2).map(|_| ts_segment(6, 0x11)).collect();
    let high_segments: Vec<Vec<u8>> = (0..4).map(|_| ts_segment(6, 0x99)).collect();

    let mut routes: HashMap<String, TestRoute> = HashMap::new();
    for (index, data) in low_segments.iter().enumerate() {
        routes.insert(format!("/low/seg{index}.ts"), data.clone().into());
    }
    for (index, data) in high_segments.iter().enumerate() {
        routes.insert(format!("/high/seg{index}.ts"), data.clone().into());
    }
    routes.insert(
        "/low.m3u8".to_string(),
        media_playlist("/low", 2, None).into_bytes().into(),
    );
    routes.insert(
        "/high.m3u8".to_string(),
        media_playlist("/high", 4, None).into_bytes().into(),
    );
    routes.insert(
        "/master.m3u8".to_string(),
        master.as_bytes().to_vec().into(),
    );

    let server = TestServer::start(routes).await;
    let snapshot = run_download(
        &directory,
        &server.url("/master.m3u8"),
        "master",
        test_settings(),
    )
    .await;

    assert_eq!(snapshot.status, TaskStatus::Completed);
    assert_eq!(snapshot.total_segments, 4, "应当选中 4 个分片的高码率流");
    assert_eq!(
        std::fs::read(output_of(&snapshot)).expect("读取输出失败"),
        flatten(&high_segments),
        "下载的不是最高带宽变体"
    );

    let _ = std::fs::remove_dir_all(&directory);
}

#[tokio::test]
async fn skips_segments_already_on_disk() {
    let directory = temp_dir("resume");
    let segments: Vec<Vec<u8>> = (0..3).map(|index| ts_segment(6, index as u8 + 1)).collect();
    let mut routes: HashMap<String, TestRoute> = HashMap::new();
    // 只提供前两个分片：第三个若被重新请求就会 404，任务必然失败。
    for (index, data) in segments.iter().take(2).enumerate() {
        routes.insert(format!("/seg{index}.ts"), data.clone().into());
    }
    routes.insert(
        "/video.m3u8".to_string(),
        media_playlist("", 3, None).into_bytes().into(),
    );

    let server = TestServer::start(routes).await;
    let url = server.url("/video.m3u8");

    // 预先把最后一个分片写进任务目录，模拟上次中断时已下载的部分。
    let manifest =
        TaskManifest::new(1, &url, "video", &directory, 4, HashMap::new()).expect("创建任务失败");
    let sentinel = ts_segment(6, 0x7e);
    std::fs::write(
        manifest.task_directory().join("segment_00002.seg"),
        &sentinel,
    )
    .expect("写入预置分片失败");
    drop(manifest);

    let snapshot = run_download(&directory, &url, "video", test_settings()).await;

    assert_eq!(snapshot.status, TaskStatus::Completed);
    let mut expected = segments[0].clone();
    expected.extend_from_slice(&segments[1]);
    // 结尾是预置的哨兵数据，说明该分片没有被重新下载。
    expected.extend_from_slice(&sentinel);
    assert_eq!(
        std::fs::read(output_of(&snapshot)).expect("读取输出失败"),
        expected
    );

    let _ = std::fs::remove_dir_all(&directory);
}

#[tokio::test]
async fn fails_cleanly_when_playlist_is_missing() {
    let directory = temp_dir("missing");
    let server = TestServer::start(HashMap::new()).await;

    let manifest = TaskManifest::new(
        1,
        &server.url("/nope.m3u8"),
        "missing",
        &directory,
        4,
        HashMap::new(),
    )
    .expect("创建任务失败");
    let task_directory = manifest.task_directory();
    let (sender, receiver) = mpsc::unbounded_channel();
    let task = DownloadTask {
        manifest,
        settings: test_settings(),
        event_sender: sender,
        cancellation_token: CancellationToken::new(),
        global_permits: Arc::new(Semaphore::new(8)),
    };
    let result = run_task(task).await;
    drop(receiver);

    assert!(result.is_err(), "播放列表 404 时任务应当失败而不是成功");
    // 失败后默认保留临时文件，便于用户排查。
    assert!(task_directory.is_dir(), "失败后应当保留任务临时目录");

    let _ = std::fs::remove_dir_all(&directory);
}

#[test]
fn reset_keeps_manifest_for_resume() {
    let directory = temp_dir("reset");
    let mut manifest = TaskManifest::new(
        1,
        "http://127.0.0.1:1/v.m3u8",
        "video",
        &directory,
        4,
        HashMap::new(),
    )
    .expect("创建任务失败");
    // 造出「已下载完成」的状态：分片落盘 + 标记为完成。
    std::fs::write(manifest.segment_path(0), vec![0x47_u8; TS_PACKET_SIZE]).expect("写入分片失败");
    manifest
        .mark_completed(directory.join("video.ts"))
        .expect("标记完成失败");

    manifest.reset_for_redownload().expect("重置失败");

    // 分片必须清掉，否则重新下载会拼进旧数据。
    assert!(
        !manifest.segment_path(0).exists(),
        "重置后已下载的分片应当被删除"
    );
    assert!(!manifest.completed, "重置后不应再是已完成状态");
    assert!(manifest.output_path.is_none(), "重置后应清空输出路径");
    // 核心断言：manifest 存在任务目录里，目录被删又没重建的话这里就会失败，
    // 任务在界面上还是「等待中」，重启后却彻底消失。
    assert!(
        manifest.manifest_path().is_file(),
        "重置后 manifest 必须仍在磁盘上"
    );
    let reloaded = TaskManifest::load(&manifest.manifest_path()).expect("重新读取 manifest 失败");
    assert_eq!(reloaded, manifest, "落盘内容应当与内存中的 manifest 一致");
    assert!(
        discover_task_manifests(&directory)
            .iter()
            .any(|found| found.id == manifest.id),
        "重置后的任务必须能被启动扫描发现，否则断点续传会丢任务"
    );

    let _ = std::fs::remove_dir_all(&directory);
}

#[test]
fn reset_after_directory_removed_manually() {
    let directory = temp_dir("reset-missing");
    let mut manifest = TaskManifest::new(
        2,
        "http://127.0.0.1:1/v.m3u8",
        "video",
        &directory,
        4,
        HashMap::new(),
    )
    .expect("创建任务失败");
    // 模拟任务目录被外部删掉（用户手动清理或磁盘工具回收）。
    std::fs::remove_dir_all(manifest.task_directory()).expect("删除任务目录失败");

    manifest
        .reset_for_redownload()
        .expect("目录不存在时重置应当自愈");

    assert!(
        manifest.manifest_path().is_file(),
        "目录被外部删除后重置也要重新落盘"
    );

    let _ = std::fs::remove_dir_all(&directory);
}

/// 分片请求先被 429 拒绝、重试后放行：覆盖 fetch_with_retries 的 429 分支
/// 和 Retry-After 解析。Retry-After 设为 0 秒，避免测试被真实退避拖慢。
#[tokio::test]
async fn retries_segments_after_429() {
    let directory = temp_dir("retry-429");
    let segments: Vec<Vec<u8>> = (0..2).map(|index| ts_segment(6, index as u8 + 1)).collect();

    let throttled = Arc::new(ScriptedRoute {
        script: vec![
            TestResponse {
                status: 429,
                body: Vec::new(),
                retry_after: Some("0".to_string()),
                delay_ms: 0,
            },
            TestResponse {
                status: 429,
                body: Vec::new(),
                retry_after: Some("0".to_string()),
                delay_ms: 0,
            },
        ],
        fallback: TestResponse::ok(segments[0].clone()),
        hits: AtomicUsize::new(0),
    });

    let mut routes: HashMap<String, TestRoute> = HashMap::new();
    routes.insert(
        "/seg0.ts".to_string(),
        TestRoute::Scripted(throttled.clone()),
    );
    routes.insert("/seg1.ts".to_string(), segments[1].clone().into());
    routes.insert(
        "/video.m3u8".to_string(),
        media_playlist("", 2, None).into_bytes().into(),
    );

    let server = TestServer::start(routes).await;
    let snapshot = run_download(
        &directory,
        &server.url("/video.m3u8"),
        "video",
        test_settings(),
    )
    .await;

    assert_eq!(snapshot.status, TaskStatus::Completed);
    assert_eq!(
        std::fs::read(output_of(&snapshot)).expect("读取输出失败"),
        flatten(&segments)
    );
    // 前两次是 429，第三次才成功；如果重试没生效，任务会在第一次 429 失败。
    assert!(
        throttled.hit_count() >= 3,
        "应当重试到成功为止，实际请求次数：{}",
        throttled.hit_count()
    );

    let _ = std::fs::remove_dir_all(&directory);
}

/// 播放列表中间换 KEY：前两个分片用 key1，第三个改用 key2（第二个 EXT-X-KEY 行）。
/// 验证解析层的 key rotation 和下载层按 URI 缓存 key 的路径。
#[tokio::test]
async fn rotates_keys_mid_playlist() {
    use aes::Aes128;
    use cbc::cipher::{block_padding::Pkcs7, BlockEncryptMut, KeyIvInit};
    type Aes128CbcEnc = cbc::Encryptor<Aes128>;

    let directory = temp_dir("key-rotation");
    let key1 = [0x11_u8; 16];
    let iv1 = [0x01_u8; 16];
    let key2 = [0x22_u8; 16];
    let iv2 = [0x02_u8; 16];
    let plaintexts: Vec<Vec<u8>> = (0..3).map(|index| ts_segment(6, index as u8 + 1)).collect();
    let mut encrypted: Vec<Vec<u8>> = Vec::new();
    for (index, data) in plaintexts.iter().enumerate() {
        let (key, iv) = if index < 2 {
            (&key1, &iv1)
        } else {
            (&key2, &iv2)
        };
        encrypted
            .push(Aes128CbcEnc::new(key.into(), iv.into()).encrypt_padded_vec_mut::<Pkcs7>(data));
    }

    let iv1_hex: String = iv1.iter().map(|byte| format!("{byte:02x}")).collect();
    let iv2_hex: String = iv2.iter().map(|byte| format!("{byte:02x}")).collect();
    let playlist = format!(
        "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:10\n#EXT-X-MEDIA-SEQUENCE:0\n#EXT-X-KEY:METHOD=AES-128,URI=\"/key1.bin\",IV=0x{iv1_hex}\n#EXTINF:10.0,\n/seg0.ts\n#EXTINF:10.0,\n/seg1.ts\n#EXT-X-KEY:METHOD=AES-128,URI=\"/key2.bin\",IV=0x{iv2_hex}\n#EXTINF:10.0,\n/seg2.ts\n#EXT-X-ENDLIST\n"
    );

    let mut routes: HashMap<String, TestRoute> = HashMap::new();
    for (index, data) in encrypted.iter().enumerate() {
        routes.insert(format!("/seg{index}.ts"), data.clone().into());
    }
    routes.insert("/key1.bin".to_string(), key1.to_vec().into());
    routes.insert("/key2.bin".to_string(), key2.to_vec().into());
    routes.insert("/video.m3u8".to_string(), playlist.into_bytes().into());

    let server = TestServer::start(routes).await;
    let snapshot = run_download(
        &directory,
        &server.url("/video.m3u8"),
        "rotated",
        test_settings(),
    )
    .await;

    assert_eq!(snapshot.status, TaskStatus::Completed);
    assert!(
        !snapshot.detail.contains("解密失败"),
        "出现了解密失败：{}",
        snapshot.detail
    );
    assert_eq!(
        std::fs::read(output_of(&snapshot)).expect("读取输出失败"),
        flatten(&plaintexts),
        "中间换 key 后解密结果必须仍与明文拼接一致"
    );

    let _ = std::fs::remove_dir_all(&directory);
}

/// 轮询事件直到命中匹配项。任务管理器跑在独立运行时里，事件到达是异步的，
/// 测试侧按短间隔轮询，超时未命中视为失败。
fn wait_for_event<F>(manager: &TaskManager, matches: F) -> TaskEvent
where
    F: Fn(&TaskEvent) -> bool,
{
    for _ in 0..200 {
        while let Some(event) = manager.try_recv_event() {
            if matches(&event) {
                return event;
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    panic!("等待任务事件超时");
}

/// 轮询等待条件成立。
///
/// 收尾是分两步异步完成的：核心先把 `TasksRemoved` 发出来让界面立刻移除任务行，
/// 删目录随后才做完。所以「收到事件」不等于「文件已经删完」，断言清理结果必须
/// 按最终一致来等，直接在事件后断言会偶发假失败。
fn wait_until<F>(description: &str, condition: F)
where
    F: Fn() -> bool,
{
    for _ in 0..200 {
        if condition() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    panic!("等待条件超时：{description}");
}

/// 通过管理器添加一个「等待中」任务（不自动开始），并在其临时目录里预置一个分片。
///
/// 等待中属于未结束状态，工具栏「删除 / 清空」必须能把它一并移除；预置分片则是
/// 为了区分「删文件」与「保留文件」两条收尾路径。返回任务 id 与预置分片、manifest 的路径。
fn add_waiting_task(
    manager: &TaskManager,
    directory: &Path,
    name: &str,
) -> (u64, PathBuf, PathBuf) {
    manager.send(TaskCommand::Add(NewTask {
        source_url: "http://127.0.0.1:1/v.m3u8".to_string(),
        output_name: name.to_string(),
        output_directory: directory.to_path_buf(),
        max_workers: 4,
        request_headers: String::new(),
        auto_start: false,
        inline_playlist: None,
    }));
    let event = wait_for_event(manager, |event| matches!(event, TaskEvent::Snapshot(_)));
    let TaskEvent::Snapshot(snapshot) = event else {
        unreachable!("匹配条件已限定为 Snapshot");
    };
    // 直接复用管理器落盘的那份清单：重新 new 会再 save 一次，只要有一个字段与管理器
    // 写入的不同（例如并发数的默认值），测试就会悄悄把真实内容盖掉。
    let manifest = discover_task_manifests(directory)
        .into_iter()
        .find(|manifest| manifest.id == snapshot.id)
        .expect("未找到任务清单");
    std::fs::write(manifest.segment_path(0), vec![0x47_u8; TS_PACKET_SIZE]).expect("写入分片失败");
    (
        snapshot.id,
        manifest.segment_path(0),
        manifest.manifest_path(),
    )
}

/// 带历史地等待某个状态的快照：返回命中快照与之前收到的全部快照。
/// 界面靠快照流还原任务状态变迁，重复启动这类问题只有在完整事件流里才看得出来。
fn wait_for_snapshot_with_history(
    manager: &TaskManager,
    mut history: Vec<TaskSnapshot>,
    target: TaskStatus,
) -> (TaskSnapshot, Vec<TaskSnapshot>) {
    for _ in 0..400 {
        match manager.try_recv_event() {
            Some(TaskEvent::Snapshot(snapshot)) => {
                if snapshot.status == target {
                    return (snapshot, history);
                }
                history.push(snapshot);
            }
            Some(_) => {}
            None => std::thread::sleep(std::time::Duration::from_millis(25)),
        }
    }
    panic!("等待 {:?} 快照超时", target);
}

/// 任务运行期间重复下发「开始 / 全部开始」不得重启运行。
///
/// 快照状态在运行全程停留在「等待中」，核心若只按快照状态判断「是否可启动」，
/// 就会把正在下载/合并的任务再起一份：旧运行被 abort 时合并收尾（rename、
/// 同步清理分片目录）不会停下，成品照常落盘，新运行随后再合并一次，
/// 输出目录就出现文件名不同、内容相同的重复成品。这里等任务真正进入下载后
/// 重复下发开始命令，断言此后不再出现「等待中」快照（添加流程自带的两次
/// 「等待中」发生在进入下载之前，不计入）、成品只有一个。
///
/// 用 `#[test]` 而不是 `#[tokio::test]`，原因同 `remove_all_interrupts_downloading_task`。
#[test]
fn start_while_running_does_not_restart_the_run() {
    let directory = temp_dir("start-while-running");
    let mut routes: HashMap<String, TestRoute> = HashMap::new();
    for index in 0..4 {
        routes.insert(
            format!("/seg{index}.ts"),
            TestRoute::Static(TestResponse::delayed(ts_segment(6, index as u8 + 1), 1_000)),
        );
    }
    routes.insert(
        "/video.m3u8".to_string(),
        media_playlist("", 4, None).into_bytes().into(),
    );
    let runtime = Runtime::new().expect("创建测试运行时失败");
    let server = runtime.block_on(TestServer::start(routes));

    let manager = TaskManager::new(test_settings(), directory.join("tasks.json"));
    manager.send(TaskCommand::Add(NewTask {
        source_url: server.url("/video.m3u8"),
        output_name: "video".to_string(),
        output_directory: directory.to_path_buf(),
        max_workers: 2,
        request_headers: String::new(),
        auto_start: true,
        inline_playlist: None,
    }));

    // 等任务真正进入下载（响应体延迟 1 秒，此刻必然仍在传输），再重复下发开始命令，
    // 模拟用户在任务运行期间又点了「开始」和「全部开始」。
    let (downloading, _) =
        wait_for_snapshot_with_history(&manager, Vec::new(), TaskStatus::Downloading);
    let task_id = downloading.id;
    manager.send(TaskCommand::Start(task_id));
    manager.send(TaskCommand::StartAll);

    // 进入下载之后到完成之间的「等待中」快照只可能来自运行重启，一个都不该有。
    let (_completed, history) =
        wait_for_snapshot_with_history(&manager, Vec::new(), TaskStatus::Completed);
    let waiting_count = history
        .iter()
        .filter(|snapshot| snapshot.status == TaskStatus::Waiting)
        .count();
    assert_eq!(
        waiting_count, 0,
        "运行期间重复开始不得重启任务（出现「等待中」即重启了运行）"
    );

    // 成品唯一：没有 「video (1).ts」 这类重复合并产物，也没有合并中间文件残留。
    let mut outputs = Vec::new();
    for entry in std::fs::read_dir(&directory)
        .expect("读取输出目录失败")
        .flatten()
    {
        let name = entry.file_name().to_string_lossy().to_string();
        if entry.path().is_dir() {
            assert!(name == ".cat-catch-tasks", "输出目录出现意外目录：{name}");
            continue;
        }
        assert!(!name.starts_with(".cat-catch-"), "残留合并中间文件：{name}");
        if name.starts_with("video") {
            outputs.push(name);
        }
    }
    assert_eq!(outputs, vec!["video.ts".to_string()], "成品必须恰好一个");

    drop(manager);
    drop(runtime);
    let _ = std::fs::remove_dir_all(&directory);
}

/// 工具栏「删除」的语义：无视勾选，把等待中在内的所有任务一并移除并清掉临时分片目录。
/// 旧实现只移除已结束任务，等待中的会漏掉——本用例防止语义被改回去。
#[test]
fn remove_all_takes_waiting_tasks_and_deletes_segments() {
    let directory = temp_dir("remove-all");
    let manager = TaskManager::new(test_settings(), directory.join("tasks.json"));
    let (task_id, segment_path, _) = add_waiting_task(&manager, &directory, "video");

    manager.send(TaskCommand::RemoveAll);
    let event = wait_for_event(&manager, |event| {
        matches!(event, TaskEvent::TasksRemoved { .. })
    });
    let TaskEvent::TasksRemoved { ids } = event else {
        unreachable!("匹配条件已限定为 TasksRemoved");
    };

    assert_eq!(ids, vec![task_id], "等待中的任务必须一并被移除");
    wait_until("临时分片目录被清理", || !segment_path.exists());

    let _ = std::fs::remove_dir_all(&directory);
}

/// 浏览器导出的内联清单：清单正文随任务落盘，下载阶段不得再去请求清单地址。
///
/// 断言的核心是 `source_url` 指向服务器上并不存在的路径——若核心仍去回源抓清单，
/// 这里必然以 404 失败；能合并出成品即证明走的是内联正文。
#[tokio::test]
async fn uses_inline_playlist_without_fetching_source_url() {
    let directory = temp_dir("inline-playlist");
    let segments: Vec<Vec<u8>> = (0..2).map(|index| ts_segment(6, index as u8 + 1)).collect();
    let mut routes: HashMap<String, TestRoute> = HashMap::new();
    for (index, data) in segments.iter().enumerate() {
        routes.insert(format!("/seg{index}.ts"), data.clone().into());
    }
    let server = TestServer::start(routes).await;

    // 卡片里的清单：分片是带签名的绝对地址，清单本身不再回源。
    let content = format!(
        "#EXTM3U\n#EXT-X-VERSION:3\n#EXTINF:8,\n{}\n#EXTINF:8,\n{}\n#EXT-X-ENDLIST\n",
        server.url("/seg0.ts"),
        server.url("/seg1.ts")
    );
    let playlist =
        parse_inline_playlist(&content, &server.url("/seg0.ts")).expect("解析内联清单失败");

    let mut manifest = TaskManifest::new(
        1,
        &server.url("/missing.m3u8"),
        "video",
        &directory,
        4,
        HashMap::new(),
    )
    .expect("创建任务失败");
    manifest.playlist = Some(playlist);
    manifest.save().expect("保存任务清单失败");

    let (sender, _receiver) = mpsc::unbounded_channel();
    let task = DownloadTask {
        manifest,
        settings: test_settings(),
        event_sender: sender,
        cancellation_token: CancellationToken::new(),
        global_permits: Arc::new(Semaphore::new(8)),
    };
    let snapshot = run_task(task).await.expect("任务执行失败");

    assert_eq!(snapshot.status, TaskStatus::Completed);
    let output = std::fs::read(output_of(&snapshot)).expect("读取成品失败");
    assert_eq!(
        output,
        flatten(&segments),
        "成品内容必须来自内联清单里的分片"
    );

    let _ = std::fs::remove_dir_all(&directory);
}

/// 工具栏「清空」的语义：同样移除所有任务，但保留本地文件，
/// 只给未完成任务打 dismissed 标记，避免重启后作为断点续传复活。
#[test]
fn clear_all_keeps_segments_but_marks_dismissed() {
    let directory = temp_dir("clear-all");
    let manager = TaskManager::new(test_settings(), directory.join("tasks.json"));
    let (task_id, segment_path, manifest_path) = add_waiting_task(&manager, &directory, "video");

    manager.send(TaskCommand::ClearAll);
    let event = wait_for_event(&manager, |event| {
        matches!(event, TaskEvent::TasksRemoved { .. })
    });
    let TaskEvent::TasksRemoved { ids } = event else {
        unreachable!("匹配条件已限定为 TasksRemoved");
    };

    assert_eq!(ids, vec![task_id], "等待中的任务必须一并被清空");
    assert!(segment_path.is_file(), "「清空」不删除任何本地文件");
    // dismissed 标记是「不删文件」路径下防止任务复活的唯一手段：
    // resume_tasks 载入时会跳过它，否则清理出来的空任务会带着旧分片重新入队。
    let reloaded = TaskManifest::load(&manifest_path).expect("重新读取 manifest 失败");
    assert!(reloaded.dismissed, "清空后任务必须打上 dismissed 标记");

    let _ = std::fs::remove_dir_all(&directory);
}

/// 「删除」必须能中断**进行中**的下载：任务从列表移除、临时分片目录被清掉，
/// 而且之后不再冒出这个任务的快照——核心已经不认识它了，再上报就会让界面长出幽灵行。
///
/// 只覆盖「等待中」是不够的：容易出问题的是正在下载的任务，它要走完整的中断路径
/// （abort → 等协程真正退出 → 删目录）。这里让分片响应延迟发出响应体，
/// 把任务稳定停在「已收到 200、正在读响应体」这一刻，而不是靠本机下载速度碰运气。
///
/// 用 `#[test]` 而不是 `#[tokio::test]`：`TaskManager` 自带运行时，在异步上下文里
/// 创建或销毁它会 panic（tokio 不允许这样做），所以只在起服务器时临时用一次运行时。
#[test]
fn remove_all_interrupts_downloading_task() {
    let directory = temp_dir("remove-downloading");
    let mut routes: HashMap<String, TestRoute> = HashMap::new();
    for index in 0..4 {
        routes.insert(
            format!("/seg{index}.ts"),
            TestRoute::Static(TestResponse::delayed(ts_segment(6, index as u8 + 1), 1_000)),
        );
    }
    routes.insert(
        "/video.m3u8".to_string(),
        media_playlist("", 4, None).into_bytes().into(),
    );
    let runtime = Runtime::new().expect("创建测试运行时失败");
    let server = runtime.block_on(TestServer::start(routes));

    let manager = TaskManager::new(test_settings(), directory.join("tasks.json"));
    manager.send(TaskCommand::Add(NewTask {
        source_url: server.url("/video.m3u8"),
        output_name: "video".to_string(),
        output_directory: directory.to_path_buf(),
        max_workers: 2,
        request_headers: String::new(),
        auto_start: true,
        inline_playlist: None,
    }));
    // 等到真正进入下载状态，才能确保接下来打断的是一个进行中的任务。
    let event = wait_for_event(
        &manager,
        |event| matches!(event, TaskEvent::Snapshot(snapshot) if snapshot.status == TaskStatus::Downloading),
    );
    let TaskEvent::Snapshot(snapshot) = event else {
        unreachable!("匹配条件已限定为下载中快照");
    };
    let task_id = snapshot.id;
    let task_directory = discover_task_manifests(&directory)
        .into_iter()
        .find(|manifest| manifest.id == task_id)
        .expect("任务清单未落盘")
        .task_directory();

    manager.send(TaskCommand::RemoveAll);
    let event = wait_for_event(&manager, |event| {
        matches!(event, TaskEvent::TasksRemoved { .. })
    });
    let TaskEvent::TasksRemoved { ids } = event else {
        unreachable!("匹配条件已限定为 TasksRemoved");
    };
    assert_eq!(ids, vec![task_id], "进行中的任务必须被移除");
    wait_until("临时分片目录被清理", || !task_directory.exists());

    // 协程收尾还需要一点时间，期间若还上报快照，界面就会重新长出这一行任务。
    std::thread::sleep(std::time::Duration::from_millis(300));
    while let Some(event) = manager.try_recv_event() {
        if let TaskEvent::Snapshot(value) = event {
            assert_ne!(
                value.id, task_id,
                "已移除的任务又上报了快照：界面会出现无法操作的幽灵行"
            );
        }
    }

    // 两者自带运行时，都必须在同步上下文里销毁，顺序也不能反：
    // manager 的下载协程还在跑，先停服务器会让它卡到超时。
    drop(manager);
    drop(runtime);

    let _ = std::fs::remove_dir_all(&directory);
}

/// 编辑任务改保存目录后必须把新目录登记进任务注册表：重启后的任务恢复只扫描
/// 注册表里的目录加当前默认下载路径，漏登记会让任务连同断点续传一起消失。
///
/// 编辑只对已结束状态开放（等待中会被 is_active 拦下），因此指向必拒连接的
/// 地址让任务快速进入「已失败」再编辑。
#[test]
fn edit_task_registers_new_output_directory() {
    let directory = temp_dir("edit-registry");
    let old_directory = directory.join("old");
    let new_directory = directory.join("new");
    std::fs::create_dir_all(&old_directory).expect("创建旧输出目录失败");
    std::fs::create_dir_all(&new_directory).expect("创建新输出目录失败");
    let registry_path = directory.join("tasks.json");
    let manager = TaskManager::new(test_settings(), registry_path.clone());

    manager.send(TaskCommand::Add(NewTask {
        source_url: "http://127.0.0.1:1/v.m3u8".to_string(),
        output_name: "video".to_string(),
        output_directory: old_directory.clone(),
        max_workers: 4,
        request_headers: String::new(),
        auto_start: true,
        inline_playlist: None,
    }));
    wait_for_event(
        &manager,
        |event| matches!(event, TaskEvent::Snapshot(snapshot) if snapshot.status == TaskStatus::Failed),
    );

    manager.send(TaskCommand::EditTask {
        id: 1,
        source_url: "http://127.0.0.1:1/v.m3u8".to_string(),
        output_name: "video".to_string(),
        output_directory: new_directory.to_string_lossy().into_owned(),
        request_headers: String::new(),
    });

    // 注册表落盘的是 canonicalize 之后的路径，比较基准保持一致。
    let registered = new_directory.canonicalize().expect("解析新目录失败");
    wait_until("注册表包含新输出目录", || {
        TaskRegistry::load(&registry_path)
            .unwrap_or_default()
            .directories
            .contains(&registered)
    });

    drop(manager);
    let _ = std::fs::remove_dir_all(&directory);
}
