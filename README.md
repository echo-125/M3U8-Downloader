# Cat Catch Assistant

用 Rust 重写的 M3U8 视频下载器，Windows 桌面图形界面，发布形态为便携版单 exe。

本项目由 Python 版完全重构而来，目标是在保持功能等价的前提下降低内存占用、提升启动速度与运行稳定性。

## 环境要求

- 操作系统：Windows 10 及以上，x64
- Rust：stable
- 图形框架：`egui` / `eframe`
- 仅支持中文界面

## 功能

- 单个任务添加、批量添加、手动合并本地分片
- 多任务并行下载与队列自动调度
- 分片级断点续传，程序重启后自动继续未完成任务
- 任务取消、重试、删除、清除已完成
- 任务状态、进度、下载速度与剩余时间展示
- 右键菜单与双击快捷操作
- 自定义请求头
- 亮色 / 暗色主题，状态持久化
- 系统托盘：关闭窗口最小化到托盘，托盘可恢复窗口与退出
- 失败与完成 Toast 提醒
- 设置面板、GUI 日志面板与文件日志
- 全局代理（HTTP / HTTPS / SOCKS5，支持认证）
- 尾部加速，阈值与并发倍数可配置

## M3U8 支持范围

支持：

- VOD 点播播放列表
- 主播放列表递归解析，自动选择最高带宽变体
- AES-128 CBC 解密，支持显式 IV、基于 `MEDIA-SEQUENCE` 的隐式 IV 与 key rotation
- TS 分片
- fMP4 / CMAF 分片，含 `EXT-X-MAP` 初始化段与初始化段 `BYTERANGE`
- 分片格式采样诊断

不支持：

- 直播流录制
- DRM
- SAMPLE-AES
- 非标准私有扩展的深度兼容

## 下载与重试策略

- 可配置最大并发任务数与单任务下载并发数
- 429 自动重试，优先尊重 `Retry-After`
- 5xx、网络错误与超时自动重试，最多 3 次
- 404、403 不自动重试，403 会提示检查防盗链或请求头
- 所有请求设置 30 秒总超时与 10 秒连接超时
- 下载失败默认保留临时文件；下载成功后默认自动清理，也可选择保留用于排查

合并策略：

- TS 分片先二进制拼接，再调用 ffmpeg remux 为 MP4
- fMP4 直接拼接初始化段与分片
- ffmpeg 默认从 PATH 自动检测，也可在设置中手动指定路径
- 未检测到 ffmpeg 时 TS 任务保留 TS 输出

## 构建与运行

```powershell
# 调试运行
cargo run

# 发布构建，产物为 target\release\cat-catch-assistant.exe
cargo build --release
```

开发期常用命令：

```powershell
cargo fmt
cargo clippy --all-targets -- -D warnings
cargo test
```

发布产物为 Windows x64 便携版单 exe，不做代码签名、不做自动更新、不配置 CI/CD。

## 配置与数据位置

| 内容 | 位置 |
| --- | --- |
| 配置文件 | 程序同目录 `config.json`，目录不可写时降级到 `%APPDATA%\cat-catch-assistant\config.json` |
| 任务注册表 | 配置文件同目录 `tasks.json` |
| 日志文件 | 程序同目录 `logs\cat-catch.日期.log`，不可写时降级到 `%LOCALAPPDATA%\cat-catch-assistant\logs` |
| 临时分片 | 下载目录下的 `.cat-catch-tasks\<任务号>-<名称>\` |
| 默认下载路径 | 系统下载目录 |

GUI 日志保留最近 500 条，文件日志支持按天或按大小滚动。

代理密码以明文存放在配置文件中，请勿在不信任的环境填写。

## 目录结构

```text
src/
├── main.rs          # 入口，加载配置、初始化日志、启动窗口
├── app/             # egui 界面、状态、主题、布局、托盘
├── core/            # M3U8 解析、下载、解密、合并、任务调度
├── config/          # 配置结构与持久化
├── logging/         # GUI 日志缓冲与文件日志滚动
└── ffmpeg/          # ffmpeg 检测与 remux
```

架构原则：下载核心不依赖 GUI，GUI 只通过命令、事件与状态快照与核心交互；所有网络请求设置超时；任务操作通过消息队列传递，不在界面线程执行阻塞 IO。

## 使用说明

1. 在任务创建区粘贴 M3U8 链接，选择保存路径与文件名，点击「添加任务」。
2. 批量添加格式为每行一条：`链接|文件名|请求头JSON`，文件名与请求头可省略。
3. 任务列表中双击可按状态快捷开始或取消，右键可开始、取消、编辑、重试、删除、复制链接、打开目录。
4. 手动合并页选择存放 TS 或 fMP4 分片的文件夹，先「扫描分片」再「开始合并」。
5. 关闭窗口会最小化到系统托盘，从托盘菜单可恢复窗口或退出程序；有下载中任务时退出需要确认。

## 验收状态

M1–M8 代码实现已完成，`cargo fmt`、`cargo test`、`cargo clippy --all-targets -- -D warnings` 均通过。

真实网络下载、AES 与 fMP4 样例、托盘交互和 GUI 主流程仍需人工验收，验收清单见 `DEVELOPMENT_PLAN.md` 第 11 节。
