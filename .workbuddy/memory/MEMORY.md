# CatCatch 项目长期记忆

## 项目定位

Rust 重写的 Windows 桌面 M3U8 下载器（由 Python 版完全重构），egui GUI，发布为便携版单 exe `M3U8下载器.exe`。
包名 `cat-catch-assistant`，`[[bin]]` 名用中文，通过 `build.rs` + `embed-resource` 嵌入图标。

## 代码规模（2026-08 实测）

- 27 个 .rs 文件，约 7194 行。
- 最大的三个文件：`src/app/layout.rs`(1301)、`src/core/manager.rs`(967)、`src/app/state.rs`(824)。

## 架构要点

- 三层：GUI(`src/app`) ←命令/事件双向 mpsc→ 核心(`src/core`) → 侧翼(`config`/`logging`/`ffmpeg`)。
- 核心层自带独立 `tokio::runtime::Runtime`，不依赖 GUI；GUI 每帧只 `try_recv` 一次事件，避免高频进度刷爆界面。
- 并发用两级信号量：`task_permits`（任务并发）与 `global_permits`（全局连接数 = 任务数 × 单任务线程数 × 尾部加速倍数）。
- 合并是大块同步 IO，放进 `tokio` 阻塞线程池，避免卡住 manager 循环。
- 下载失败的语义是「取消 = 重置」：`TaskCommand::Reset` 会清空已下载分片，不保留断点续传。
- 已完成任务不可重新开始（`is_startable()` 排除 Completed），否则会重置 manifest 覆盖已有成品。

## 验证基线

`cargo fmt` / `cargo test`（45 个测试全过）/ `cargo clippy --all-targets -- -D warnings` 均通过。
注意：同时跑两个 cargo 命令会因文件锁互相阻塞导致超时被杀，必须串行执行。

## 端到端测试（2026-08-31 新增）

`src/core/e2e_tests.rs`，在 `src/core/mod.rs` 用 `#[cfg(test)] mod e2e_tests;` 声明
（本项目是二进制 crate 无 lib.rs，外部 tests/ 目录访问不到内部模块，只能放 src 内）。

- 极简 HTTP 服务器：`TcpListener::bind("127.0.0.1:0")`，按路径返回内存中的预设内容，
  只读到 `\r\n\r\n` 就停，够驱动下载核心，不实现完整 HTTP 语义。
- 5 个用例：TS 下载合并、AES-128 解密合并、主播放列表选最高带宽、分片级断点续传、404 干净失败。
- TS 数据用 `ts_segment(packets, fill)` 造：188 字节包、首字节 0x47，填充值按分片区分以校验顺序。
- `test_settings()` 必须关 ffmpeg（`auto_detect=false`）与尾部加速，否则输出路径不确定。
- 断点续传用例的技巧：只给前 N-1 个分片配路由，第 N 个预置成哨兵数据落盘，
  若被重下会 404 失败；末尾比对到哨兵数据即证明跳过了。
- 进度事件流也断言了：过程中有 Downloading 快照、progress 单调递增、终态 Completed 且进度 100%。
- 踩坑：IV 字面量手写多了一组（34 个 hex 字符而非 32），被 `parse_iv` 正确拒绝报 `InvalidPlaylist`。
  教训已写进 AGENTS.md——测试里不手写十六进制常量，由字节数组 `format!("{byte:02x}")` 生成。

## 已确认的产品决策（不要当成 bug 去"修"）

用户于 2026-08-31 明确确认：

- **「取消」= 删除已下载分片，任务回到「等待中」。** 不产生「已暂停」状态，不保留断点续传。
  这是预期行为。想保进度就别点取消，直接关程序或最小化到托盘都不会清分片。
- **代理密码明文存储，不做加密。** 便携版单 exe 无处安全存放密钥，加密只挡「记事本打开」这一种情况，
  还会让配置损坏时无法手动修复。
- **已完成的任务不可重新开始**（防覆盖成品），同样是刻意为之。

以上三条已写进 README 的「行为约定」章节。

## 托盘事件机制（2026-08-31 修 bug 时确认）

托盘菜单事件**不能靠每帧轮询**：界面空闲时 `request_repaint_after(1000ms)`，轮询会让响应延迟到
最长 1 秒，且每帧只取一个事件时连点会延迟叠加。正确做法：

- `MenuEvent::set_event_handler(Some(closure))` 注册回调，回调里推进 `Arc<Mutex<VecDeque<TrayAction>>>`
  并调用 `ctx.request_repaint()` 立即唤醒事件循环（eframe 注册了 `set_request_repaint_callback`，可靠）。
- 取动作时一次排空队列，而不是每帧一个。
- handler 与 `MenuEvent::receiver()` **互斥**（设了 handler 就不再进 channel），
  且 muda 用 `OnceCell` 保存，**必须在首次菜单事件之前注册**。

恢复窗口要 `Minimized(false)` + `Visible(true)` + `ViewportCommand::Focus`（egui 0.29 里是单元变体），
否则窗口可能仍在别的窗口背后，用户以为没反应。

## 环境

**`CARGO_HOME` = `D:\develop\cargo`**，依赖源码在 `D:\develop\cargo\registry\src\index.crates.io-*\`。
`~/.cargo` 下只有部分 crate，别去那里找。

## 界面模块结构（2026-08-31 拆分后）

依赖方向单向，不得反向：

```text
layout (192 行，总装配 + 标题栏/状态栏/日志区)
   ↓
forms (171) / task_list (322) / dialogs (439)
   ↓
widgets (229，通用控件 + 布局常量)
```

`src/app/` 下最大的文件是 `state.rs`（824 行）。

## 遗留状态

M1–M8 代码已完成，但**真实网络下载一次都没跑过**。40 项验收清单在 README 的「验收清单」章节，
其中 6 项已标注「自动」（由 e2e_tests 覆盖），其余仍需人工验证。
未覆盖且成本最高的：ffmpeg remux、代理、429/5xx 重试、key rotation、fMP4/CMAF、托盘交互。

`DEVELOPMENT_PLAN.md` 已于 2026-08-31 按用户要求删除（清单已并入 README），它原本就被 gitignore，
删除后无法从版本控制恢复。

`.workbuddy/` 是 WorkBuddy 记忆目录，git 未追踪、也不在 .gitignore 里，已提示用户自行决定。
