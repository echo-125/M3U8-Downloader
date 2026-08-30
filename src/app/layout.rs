//! 界面总装配：标题栏、新建任务区、任务列表、日志面板、状态栏，以及各浮层的编排。
//!
//! 具体控件在 widgets，表单在 forms，任务列表在 task_list，浮层在 dialogs。
//!
//! 竖向分区：标题栏固定在顶部；创建区与任务列表一起放在中央滚动区里，
//! 窗口高度不够时整体可滚动，不会出现页面下半部分够不到的情况；
//! 日志做成窗口底部的可折叠面板，状态栏压在最底部。
//! 注意 egui 的面板按声明顺序由外向内堆叠，先声明的 bottom panel 压在最下面。

use eframe::egui::{self, Align, Layout, RichText, ScrollArea, TopBottomPanel};

use super::{
    dialogs::{
        render_clear_confirmation, render_delete_confirmation,
        render_discard_settings_confirmation, render_edit_window, render_exit_confirmation,
        render_settings_window, render_toast,
    },
    forms::render_creation_area,
    state::AppState,
    task_list::render_task_list,
    theme,
    widgets::{chevron_button, theme_switch_button, vertical_divider, LOG_ROW_HEIGHT},
};
use crate::core::events::TaskStatus;

pub fn render(ctx: &egui::Context, state: &mut AppState) {
    render_title_bar(ctx, state);
    // 先声明的 bottom panel 压在最底部，所以状态栏要排在日志面板之前。
    render_status_bar(ctx, state);
    render_log_panel(ctx, state);

    // CentralPanel 必须最后声明，它拿走剩下全部空间。
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
            // 创建区与任务列表放进同一个滚动区：窗口高度不够时整体可滚。
            // 任务列表内部还有自己的滚动区（虚拟化渲染大列表），滚到边界后
            // 剩余滚轮量会自动传给外层，页面下半部分始终可达。
            egui::ScrollArea::vertical()
                .id_salt("main_scroll")
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    render_creation_area(ui, state);
                    ui.add_space(6.0);
                    render_task_list(ui, state);
                });
        });

    render_settings_window(ctx, state);
    render_discard_settings_confirmation(ctx, state);
    render_edit_window(ctx, state);
    render_exit_confirmation(ctx, state);
    render_clear_confirmation(ctx, state);
    render_delete_confirmation(ctx, state);
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
                    // 图标表示点击后切换到的主题：亮色显示月亮、暗色显示太阳，
                    // 完整说明在悬停提示里。
                    if theme_switch_button(ui, state.settings.appearance.theme).clicked() {
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
                        Some(info) => ui
                            .label(
                                RichText::new(format!("ffmpeg {}", info.version))
                                    .small()
                                    .color(theme::success_color(ctx.style().visuals.dark_mode)),
                            )
                            .on_hover_text(format!("ffmpeg：{}", info.path)),
                        None => ui.label(
                            RichText::new("ffmpeg：未安装")
                                .small()
                                .color(theme::warning_color(ctx.style().visuals.dark_mode)),
                        ),
                    };
                });
            });
        });
}

/// 日志面板折叠时只留一条标题栏的高度。
const LOG_PANEL_COLLAPSED_HEIGHT: f32 = 36.0;
/// 日志面板展开时的总高度，固定值：让高度跟随内容会让内容推高面板、
/// 面板又给出更多高度，形成正反馈导致面板逐帧变大。
const LOG_PANEL_EXPANDED_HEIGHT: f32 = 200.0;

/// 窗口底部的日志面板，可折叠。默认折叠，把竖向空间让给任务列表。
fn render_log_panel(ctx: &egui::Context, state: &mut AppState) {
    let expanded = state.settings.appearance.log_panel_expanded;
    TopBottomPanel::bottom("log_panel")
        .exact_height(if expanded {
            LOG_PANEL_EXPANDED_HEIGHT
        } else {
            LOG_PANEL_COLLAPSED_HEIGHT
        })
        .resizable(false)
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
                if chevron_button(ui, expanded)
                    .on_hover_text(if expanded {
                        "收起日志"
                    } else {
                        "展开日志"
                    })
                    .clicked()
                {
                    state.settings.appearance.log_panel_expanded = !expanded;
                    state.persist_settings();
                }
                ui.strong("运行日志");
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.button("清空").clicked() {
                        state.logs.clear();
                    }
                    ui.label(
                        RichText::new(format!("{} 条", state.logs.entries().len()))
                            .small()
                            .weak(),
                    );
                });
            });
            if !expanded {
                return;
            }
            if state.logs.is_empty() {
                ui.label(RichText::new("暂无日志").weak());
                return;
            }
            render_log_rows(ui, state);
        });
}

fn render_log_rows(ui: &mut egui::Ui, state: &mut AppState) {
    let entries = state.logs.entries();
    let height = ui.available_height();
    let dark_mode = ui.ctx().style().visuals.dark_mode;
    // 只渲染可视范围内的行，避免几百条日志每帧重新布局拖慢界面。
    ScrollArea::vertical()
        .max_height(height)
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
                    crate::logging::LogLevel::Warning => Some(theme::warning_color(dark_mode)),
                    crate::logging::LogLevel::Error => {
                        Some(theme::status_color(dark_mode, TaskStatus::Failed))
                    }
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
}
