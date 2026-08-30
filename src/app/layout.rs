//! 界面总装配：标题栏、状态栏、日志区，以及各功能区的编排。
//!
//! 具体控件在 widgets，表单在 forms，任务列表在 task_list，浮层在 dialogs。

use eframe::egui::{self, Align, Color32, Layout, RichText, ScrollArea, TopBottomPanel};

use super::{
    dialogs::{
        render_clear_confirmation, render_edit_window, render_exit_confirmation,
        render_settings_window, render_toast,
    },
    forms::render_creation_area,
    state::AppState,
    task_list::render_task_list,
    theme::{switch_hint, switch_label},
    widgets::{card, outline_button, vertical_divider, LOG_ROW_HEIGHT},
};
use crate::core::events::TaskStatus;

pub fn render(ctx: &egui::Context, state: &mut AppState) {
    render_title_bar(ctx, state);
    render_status_bar(ctx, state);

    egui::CentralPanel::default()
        .frame(
            egui::Frame::default()
                .fill(ctx.style().visuals.panel_fill)
                .inner_margin(egui::Margin {
                    left: 14.0,
                    right: 14.0,
                    top: 10.0,
                    bottom: 6.0,
                }),
        )
        .show(ctx, |ui| {
            ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    render_creation_area(ui, state);
                    ui.add_space(10.0);
                    render_task_list(ui, state);
                    ui.add_space(10.0);
                    render_log_area(ui, state);
                    ui.add_space(4.0);
                });
        });

    render_settings_window(ctx, state);
    render_edit_window(ctx, state);
    render_exit_confirmation(ctx, state);
    render_clear_confirmation(ctx, state);
    render_toast(ctx, state);
}

fn render_title_bar(ctx: &egui::Context, state: &mut AppState) {
    TopBottomPanel::top("title_bar")
        .frame(
            egui::Frame::default()
                .fill(ctx.style().visuals.panel_fill)
                .inner_margin(egui::Margin {
                    left: 16.0,
                    right: 16.0,
                    top: 10.0,
                    bottom: 8.0,
                }),
        )
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("M3U8下载器").heading());
                ui.label(
                    RichText::new(format!("v{}", env!("CARGO_PKG_VERSION")))
                        .small()
                        .weak(),
                );
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.button("设置").clicked() {
                        state.settings_open = true;
                    }
                    let theme = state.settings.appearance.theme;
                    // 只写目标主题名，完整说明放在悬停提示里。
                    if outline_button(ui, switch_label(theme))
                        .on_hover_text(switch_hint(theme))
                        .clicked()
                    {
                        state.toggle_theme();
                    }
                });
            });
        });
}

fn render_status_bar(ctx: &egui::Context, state: &mut AppState) {
    TopBottomPanel::bottom("status_bar")
        .frame(
            egui::Frame::default()
                .fill(ctx.style().visuals.panel_fill)
                .inner_margin(egui::Margin {
                    left: 16.0,
                    right: 16.0,
                    top: 6.0,
                    bottom: 6.0,
                }),
        )
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                let completed = state
                    .tasks
                    .iter()
                    .filter(|task| task.status == TaskStatus::Completed)
                    .count();
                let failed = state
                    .tasks
                    .iter()
                    .filter(|task| task.status == TaskStatus::Failed)
                    .count();
                ui.label(format!("任务：{}", state.tasks.len()));
                vertical_divider(ui, 16.0);
                ui.label(format!("进行中：{}", state.active_task_count()));
                vertical_divider(ui, 16.0);
                ui.label(format!("完成：{completed}"));
                vertical_divider(ui, 16.0);
                ui.label(format!("失败：{failed}"));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    match &state.ffmpeg_status {
                        Some(path) => {
                            ui.label(RichText::new(format!("ffmpeg：{path}")).small().weak())
                        }
                        None => ui.label(
                            RichText::new("ffmpeg：未检测到，TS 任务将保留 TS 输出")
                                .small()
                                .color(Color32::from_rgb(214, 166, 31)),
                        ),
                    };
                });
            });
        });
}

fn render_log_area(ui: &mut egui::Ui, state: &mut AppState) {
    card(ui, "运行日志", |ui| {
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if ui.button("清空").clicked() {
                state.logs.clear();
            }
            ui.label(RichText::new("最多保留 500 条").small().weak());
        });
        if state.logs.is_empty() {
            ui.label(RichText::new("暂无日志").weak());
            return;
        }
        let entries = state.logs.entries();
        // 只渲染可视范围内的行，避免几百条日志每帧重新布局拖慢界面。
        ScrollArea::vertical()
            .max_height(180.0)
            .auto_shrink([false, false])
            .stick_to_bottom(true)
            .show_rows(ui, LOG_ROW_HEIGHT, entries.len(), |ui, range| {
                let width = ui.available_width();
                for index in range {
                    let Some(entry) = entries.get(index) else {
                        continue;
                    };
                    let color = match entry.level {
                        crate::logging::LogLevel::Info => None,
                        crate::logging::LogLevel::Warning => Some(Color32::from_rgb(214, 166, 31)),
                        crate::logging::LogLevel::Error => Some(Color32::from_rgb(214, 69, 65)),
                    };
                    let text = RichText::new(format!(
                        "{} [{}] {}",
                        entry.time,
                        entry.level.label(),
                        entry.message
                    ))
                    .monospace();
                    let text = match color {
                        Some(color) => text.color(color),
                        None => text,
                    };
                    // 显式用左对齐布局：垂直布局的 cross_align 是居中，
                    // 直接 add_sized 会把日志条目摆在行中央而不是靠左。
                    ui.allocate_ui_with_layout(
                        egui::vec2(width, LOG_ROW_HEIGHT),
                        Layout::left_to_right(Align::Center),
                        |ui| {
                            ui.add(egui::Label::new(text).truncate())
                                .on_hover_text(entry.message.as_str())
                        },
                    );
                }
            });
    });
}
