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
use eframe::egui::{self, ViewportCommand};

pub struct CatCatchApp {
    state: AppState,
    tray: Option<TrayController>,
}

impl CatCatchApp {
    pub fn new(creation_context: &eframe::CreationContext<'_>) -> Self {
        let mut state = AppState::new();
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
        Self { state, tray }
    }

    fn request_exit(&mut self, ctx: &egui::Context) {
        if self.state.active_task_count() > 0 {
            self.state.show_exit_confirmation = true;
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
        apply(ctx, self.state.settings.appearance.theme);
        render(ctx, &mut self.state);
        self.process_tray(ctx);
        self.handle_close_request(ctx);
        ctx.request_repaint_after(std::time::Duration::from_millis(200));
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
                ctx.send_viewport_cmd(ViewportCommand::Visible(true));
            }
            TrayAction::Exit => self.request_exit(ctx),
        }
    }

    fn handle_close_request(&mut self, ctx: &egui::Context) {
        let close_requested = ctx.input(|input| input.viewport().close_requested());
        if !close_requested || self.state.allow_exit {
            return;
        }
        ctx.send_viewport_cmd(ViewportCommand::CancelClose);
        if self.state.active_task_count() > 0 {
            self.state.show_exit_confirmation = true;
            ctx.send_viewport_cmd(ViewportCommand::Visible(true));
        } else {
            ctx.send_viewport_cmd(ViewportCommand::Visible(false));
        }
    }
}
