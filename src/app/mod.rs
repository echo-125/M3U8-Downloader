pub mod layout;
pub mod state;
pub mod theme;
pub mod tray;

use self::{
    layout::render,
    state::AppState,
    theme::{apply, install_fonts},
    tray::{TrayAction, TrayController},
};
use std::time::Instant;

use eframe::egui::{self, ViewportCommand};

use crate::config::{Settings, ThemeKind};

pub struct CatCatchApp {
    state: AppState,
    tray: Option<TrayController>,
    window_size_dirty: bool,
    last_window_size_saved: Instant,
    /// 已应用到 egui 的主题。样式只在主题变化时重建，避免每帧失效文本布局缓存。
    applied_theme: Option<ThemeKind>,
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
        let tray = match TrayController::new() {
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
        }
    }

    fn request_exit(&mut self, ctx: &egui::Context) {
        if self.state.active_task_count() > 0 {
            self.state.request_exit_confirmation();
            // 先恢复窗口再弹确认框：托盘退出时窗口可能处于最小化状态。
            ctx.send_viewport_cmd(ViewportCommand::Minimized(false));
            ctx.send_viewport_cmd(ViewportCommand::Visible(true));
            return;
        }
        self.state.allow_exit = true;
        ctx.send_viewport_cmd(ViewportCommand::Close);
    }
}

impl eframe::App for CatCatchApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.state.process_events();
        self.state.expire_toast();
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
        let idle = self.state.active_task_count() == 0 && self.state.toast.is_none();
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
        let Some(action) = self.tray.as_ref().and_then(TrayController::poll) else {
            return;
        };
        match action {
            TrayAction::Show => {
                self.state.show_exit_confirmation = false;
                // 最小化状态下恢复：先取消最小化再显示。
                ctx.send_viewport_cmd(ViewportCommand::Minimized(false));
                ctx.send_viewport_cmd(ViewportCommand::Visible(true));
            }
            TrayAction::Exit => {
                // 窗口最小化驻留托盘时，Close 命令对隐藏窗口不生效——帧循环虽在运行，
                // winit 不会给最小化窗口派发关闭事件，走正常流程会一直挂着。
                // 此时直接退出进程：任务状态随变化即时落盘，文件日志逐行 flush，不丢数据。
                let minimized = ctx.input(|input| input.viewport().minimized.unwrap_or(false));
                if minimized {
                    std::process::exit(0);
                }
                self.request_exit(ctx);
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
