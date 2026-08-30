# 代码审查意见落地概览

针对第二轮代码审查报告（8 项 🟡 建议优化 + 2 项 🔴 决策项），全部处理完毕。
验证：`cargo fmt` / `cargo test`（47 过）/ `cargo clippy --all-targets -- -D warnings` 全通过。

## 额外发现并修复的真 bug

`manager.rs start_task` 原来用 `is_active()` 拦截重复启动，而 `is_active` 包含「等待中」。
`auto_start=false` 的任务（粘贴添加）登记为等待中且必须靠 `start_task` 启动，
此前点「开始」会被核心静默忽略。现改为 `!is_startable()`，一并成为「已完成任务不可重新开始」的核心侧防线。

## 已实施的建议

1. **task_list.rs 按引用遍历**：消除每帧克隆整个 TaskSnapshot。右键菜单改为返回
   `MenuAction` 枚举、行外执行；单行操作仅在点下菜单项时克隆一次；
   `state.rs` 拆出 `open_task_directory_paths`，删除失去调用者的 `is_task_selected`。
2. **dialogs.rs 设置窗口拆分**：拆为常规/下载/代理/ffmpeg/日志 5 个 section 函数。
3. **编辑窗口分支**：审查称「死分支」实为误报（取消按钮会置空 edit_task，条件非恒真），
   但已改为显式 `canceled` 标志，意图更清晰。
4. **Toast 换行**：多行消息按 `lines()` 逐行渲染。
5. **tray.rs 运行时保护**：队列与 handler 注册放进全局 `OnceLock`，重复创建不再 panic。
6. **路径 UTF-8 校验**：7 处 `to_string_lossy` 改为 `path_dialog_string` 显式拒绝，
   非 UTF-8 路径弹提示，防止 U+FFFD 写进配置无法恢复。
7. **BYTERANGE 测试**：新增 2 个用例（隐式偏移累加、标签只作用于紧随分片），测试 45 → 47。
8. **persist_settings**：不改——防抖 2 秒 + JSON 体量小，收益不抵风险。

## 决策项

- **A（已完成任务不可重开）**：核心 `start_task` 现在直接拒绝 Completed，与界面过滤形成
  双保险；README「行为约定」措辞已同步。
- **B（手动合并与 auto_cleanup）**：确认为合理设计（不代删用户自选文件夹的内容），
  README 使用说明第 5 条已澄清「手动合并不受成功后自动清理影响，不删除分片」。

## 遗留提醒

- GUI 拆分与本次重构都未做真机界面确认，建议启动程序人工过一遍：任务列表右键/双击、
  设置窗口五个分区、粘贴添加后点「开始」（此次核心修复的关键路径）、托盘菜单。
