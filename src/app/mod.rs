pub mod dialogs;
pub mod forms;
pub mod layout;
pub mod state;
pub mod task_list;
pub mod theme;
pub mod tray;
pub mod widgets;

use self::{
    layout::render,
    state::AppState,
    theme::{apply, install_fonts},
    tray::{TrayAction, TrayController},
};
use std::time::Instant;

use eframe::egui::{self, ViewportCommand};

use crate::config::{Settings, ThemeKind};
use crate::core::events::TaskStatus;

pub struct CatCatchApp {
    state: AppState,
    tray: Option<TrayController>,
    window_size_dirty: bool,
    last_window_size_saved: Instant,
    /// 已应用到 egui 的主题。样式只在主题变化时重建，避免每帧失效文本布局缓存。
    applied_theme: Option<ThemeKind>,
    /// 上一帧是否有进行中任务，用于检测「全部结束」的瞬间。
    had_active_tasks: bool,
}

impl CatCatchApp {
    pub fn new(
        creation_context: &eframe::CreationContext<'_>,
        settings: Settings,
        load_warning: Option<String>,
    ) -> Self {
        let mut state = AppState::new(settings, load_warning);
        if let Some(warning) = install_fonts(&creation_context.egui_ctx) {
            state.logs.push_warning(warning);
        }
        let tray = match TrayController::new(creation_context.egui_ctx.clone()) {
            Ok(tray) => Some(tray),
            Err(error) => {
                state.logs.push_warning(error);
                None
            }
        };
        Self {
            state,
            tray,
            window_size_dirty: false,
            last_window_size_saved: Instant::now(),
            applied_theme: None,
            had_active_tasks: false,
        }
    }

    /// 全部任务从「有进行中」转为「全结束」的瞬间给一次汇总反馈。
    /// 用户常常把窗口缩到托盘等下载完，此刻若窗口最小化就恢复置前，否则提醒白给了。
    fn notify_all_finished(&mut self, ctx: &egui::Context) {
        let has_active = self.state.active_task_count() > 0;
        if self.had_active_tasks && !has_active {
            let completed = self
                .state
                .tasks
                .iter()
                .filter(|task| task.status == TaskStatus::Completed)
                .count();
            let failed = self
                .state
                .tasks
                .iter()
                .filter(|task| task.status == TaskStatus::Failed)
                .count();
            self.state.show_toast(
                format!("全部任务结束：{completed} 个成功，{failed} 个失败"),
                failed > 0,
            );
            if ctx.input(|input| input.viewport().minimized.unwrap_or(false)) {
                self.restore_window(ctx);
            }
        }
        self.had_active_tasks = has_active;
    }

    /// 把窗口从最小化 / 后台状态带到用户面前。
    /// 只取消最小化不够：窗口可能还压在别的窗口背后，用户会以为点了没反应。
    fn restore_window(&self, ctx: &egui::Context) {
        ctx.send_viewport_cmd(ViewportCommand::Minimized(false));
        ctx.send_viewport_cmd(ViewportCommand::Visible(true));
        ctx.send_viewport_cmd(ViewportCommand::Focus);
    }

    fn request_exit(&mut self, ctx: &egui::Context) {
        if self.state.active_task_count() > 0 {
            // 有任务在跑就先恢复窗口再弹确认框。
            // 不能因为窗口缩在托盘里就静默结束进程——下载中的任务必须能拦住退出。
            self.state.request_exit_confirmation();
            self.restore_window(ctx);
            return;
        }
        self.state.allow_exit = true;
        // 最小化窗口收不到 Close 事件（winit 不给最小化窗口派发关闭事件），
        // 走正常关闭流程会一直挂着。此时直接结束进程：任务状态随变化即时落盘，
        // 文件日志逐行 flush，不丢数据。
        if ctx.input(|input| input.viewport().minimized.unwrap_or(false)) {
            std::process::exit(0);
        }
        ctx.send_viewport_cmd(ViewportCommand::Close);
    }
}

impl eframe::App for CatCatchApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.state.process_events();
        self.state.expire_toast();
        self.notify_all_finished(ctx);
        let theme = self.state.settings.appearance.theme;
        if self.applied_theme != Some(theme) {
            apply(ctx, theme);
            self.applied_theme = Some(theme);
        }
        render(ctx, &mut self.state);
        self.process_tray(ctx);
        self.handle_close_request(ctx);
        self.sync_window_size(ctx);
        // 只在有任务在跑或有 Toast 需要消失时高频重绘，空闲时降低到 1 秒一次。
        // 常驻的错误 Toast 不算——它不倒计时，没必要拖着重绘频率不放。
        let idle = self.state.active_task_count() == 0 && !self.state.has_expiring_toast();
        let interval = if idle { 1000 } else { 200 };
        ctx.request_repaint_after(std::time::Duration::from_millis(interval));
    }
}

impl CatCatchApp {
    /// 记录窗口尺寸变化并防抖落盘，下次启动时按上次尺寸恢复。
    fn sync_window_size(&mut self, ctx: &egui::Context) {
        let Some(rect) = ctx.input(|input| input.viewport().inner_rect) else {
            return;
        };
        let width = rect.width().round().clamp(820.0, 4096.0);
        let height = rect.height().round().clamp(560.0, 4096.0);
        let appearance = &mut self.state.settings.appearance;
        if (appearance.window_width - width).abs() >= 1.0
            || (appearance.window_height - height).abs() >= 1.0
        {
            appearance.window_width = width;
            appearance.window_height = height;
            self.window_size_dirty = true;
        }
        if self.window_size_dirty
            && self.last_window_size_saved.elapsed() >= std::time::Duration::from_secs(2)
            // 设置窗口打开期间内存里的配置只是草稿，此时写盘会把未保存的修改固化下来。
            && self.state.settings_before_edit.is_none()
        {
            self.state.persist_settings();
            self.window_size_dirty = false;
            self.last_window_size_saved = Instant::now();
        }
    }
}

impl CatCatchApp {
    fn process_tray(&mut self, ctx: &egui::Context) {
        let Some(tray) = self.tray.as_ref() else {
            return;
        };
        for action in tray.take_actions() {
            match action {
                TrayAction::Show => {
                    self.state.show_exit_confirmation = false;
                    self.restore_window(ctx);
                }
                TrayAction::Exit => {
                    self.request_exit(ctx);
                    // 退出请求已经发出，队列里剩下的动作没有意义了。
                    return;
                }
            }
        }
    }

    fn handle_close_request(&mut self, ctx: &egui::Context) {
        let close_requested = ctx.input(|input| input.viewport().close_requested());
        if !close_requested || self.state.allow_exit {
            return;
        }
        ctx.send_viewport_cmd(ViewportCommand::CancelClose);
        if self.state.active_task_count() > 0 {
            self.state.request_exit_confirmation();
            ctx.send_viewport_cmd(ViewportCommand::Visible(true));
        } else {
            // 最小化到托盘。注意不能用隐藏：隐藏后 egui 帧循环会停止，
            // 托盘「显示」和「退出」的事件再也轮询不到，程序会一直卡住。
            ctx.send_viewport_cmd(ViewportCommand::Minimized(true));
        }
    }
}
