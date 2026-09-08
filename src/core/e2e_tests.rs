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
    sync::{mpsc, Semaphore},
};
use tokio_util::sync::CancellationToken;

use crate::{
    config::Settings,
    core::{
        downloader::{run_task, DownloadTask},
        error::CoreError,
        events::{TaskEvent, TaskSnapshot, TaskStatus},
        merge::merge_segments,
        task::{discover_task_manifests, TaskManifest},
    },
};

const TS_PACKET_SIZE: usize = 188;

/// 一次响应：状态码 + 响应体，可选 Retry-After 头。
#[derive(Clone)]
struct TestResponse {
    status: u16,
    body: Vec<u8>,
    retry_after: Option<String>,
}

impl TestResponse {
    fn ok(body: Vec<u8>) -> Self {
        Self {
            status: 200,
            body,
            retry_after: None,
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
    let mut bytes = header.into_bytes();
    bytes.extend_from_slice(&response.body);
    let _ = socket.write_all(&bytes).await;
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
            },
            TestResponse {
                status: 429,
                body: Vec::new(),
                retry_after: Some("0".to_string()),
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
