//! 任务列表：工具栏、表头、行渲染、右键菜单与状态展示。

use eframe::egui::{self, Align, Color32, Layout, RichText};

use super::{
    state::{AppState, EditTask},
    widgets::{card, format_bytes, format_duration, primary_button, vertical_divider},
};
use crate::core::events::{TaskSnapshot, TaskStatus};

/// 任务列表五列的宽度比例：勾选 / 文件名 / 状态 / 进度 / 速度信息。
/// 固定比例而非按内容自适应，否则速度和剩余时间每帧变化会让列宽持续抖动。
const TASK_COLUMN_RATIOS: [f32; 5] = [0.07, 0.28, 0.11, 0.26, 0.28];
const TASK_COLUMN_TITLES: [&str; 5] = ["勾选", "文件名", "状态", "进度", "速度 / 信息"];
/// 任务行高固定，行高不随内容变化。
const TASK_ROW_HEIGHT: f32 = 28.0;
const TASK_COLUMN_SPACING: f32 = 10.0;

/// 右键菜单选中的动作：批量操作作用于勾选集合，单行操作针对本行。
/// 菜单只记录选择，动作统一在行外执行，让渲染闭包保持对 state 的只读借用。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MenuAction {
    StartSelected,
    CancelSelected,
    EditThis,
    RetrySelected,
    DeleteSelected,
    CopyLink,
    OpenThis,
}

pub fn render_task_list(ui: &mut egui::Ui, state: &mut AppState) {
    card(ui, "任务列表", |ui| {
        let startable_selected = !state
            .selected_ids_where(TaskStatus::is_startable)
            .is_empty();
        let cancelable_selected = !state
            .selected_ids_where(TaskStatus::is_cancelable)
            .is_empty();
        let any_startable = state.tasks.iter().any(|task| task.status.is_startable());
        let has_finished = state
            .tasks
            .iter()
            .any(|task| matches!(task.status, TaskStatus::Completed | TaskStatus::Failed));
        let all_checked =
            !state.tasks.is_empty() && state.selected_task_ids.len() == state.tasks.len();

        ui.horizontal(|ui| {
            if primary_button(ui, startable_selected, "开始").clicked() {
                state.start_selected_tasks();
            }
            if primary_button(ui, any_startable, "全部开始").clicked() {
                state.start_all_tasks();
            }
            vertical_divider(ui, 20.0);
            if ui
                .add_enabled(cancelable_selected, egui::Button::new("取消"))
                .clicked()
            {
                state.cancel_selected_tasks();
            }
            if ui
                .add_enabled(startable_selected, egui::Button::new("重试"))
                .clicked()
            {
                state.retry_selected_tasks();
            }
            // 删除：无视勾选，移除所有已完成/已失败任务。
            if ui
                .add_enabled(has_finished, egui::Button::new("删除"))
                .clicked()
            {
                state.remove_finished_tasks();
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if !state.tasks.is_empty() && ui.button("全选").clicked() {
                    // 已全部勾选时再点一次取消全选。
                    if state.selected_task_ids.len() == state.tasks.len() {
                        state.clear_checks();
                    } else {
                        state.select_all_tasks();
                    }
                }
                if !state.tasks.is_empty() && ui.button("清空").clicked() {
                    state.show_clear_confirmation = true;
                }
            });
        });
        ui.add_space(2.0);

        let widths = task_column_widths(ui.available_width());
        // 表头：勾选列是全选 checkbox，其余列是标题。
        let mut toggle_all = false;
        task_table_row(ui, &widths, 0, false, false, |ui, column| match column {
            0 => {
                if state.tasks.is_empty() {
                    // 无任务时表头不显示全选框，避免看起来像一行空任务。
                    ui.strong(TASK_COLUMN_TITLES[0])
                } else {
                    let mut checked = all_checked;
                    let response = ui.checkbox(&mut checked, "");
                    if response.clicked() {
                        toggle_all = true;
                    }
                    response
                }
            }
            _ => ui.strong(TASK_COLUMN_TITLES[column]),
        });
        if toggle_all {
            if all_checked {
                state.clear_checks();
            } else {
                state.select_all_tasks();
            }
        }
        ui.separator();

        if state.tasks.is_empty() {
            ui.label(RichText::new("暂无任务，先在上方添加一个下载任务").weak());
        }

        for index in 0..state.tasks.len() {
            // 按引用遍历：闭包内只读任务数据，交互结果先记进局部标志，行结束后
            // 再改动 state。避免每帧克隆整个 TaskSnapshot 在列表较长时触发高频堆分配。
            let task = &state.tasks[index];
            let checked = state.selected_task_ids.contains(&task.id);
            let id = task.id;
            let status = task.status;
            // 行内只记录交互结果，行外再改动状态，避免与界面查询互相借用。
            let mut toggle = false;
            let mut double_clicked = false;
            let mut menu_action: Option<MenuAction> = None;

            let row_clicked = task_table_row(ui, &widths, index, checked, true, |ui, column| {
                match column {
                    0 => {
                        let mut flag = checked;
                        let response = ui.checkbox(&mut flag, "");
                        if response.clicked() {
                            toggle = true;
                        }
                        response
                    }
                    1 => {
                        // 用带点击感应的 Label 而不是按钮：按钮文本会强制居中，
                        // 撑满整列后短文件名会偏离左侧。
                        let text = RichText::new(task.output_name.as_str());
                        let text = if checked {
                            text.strong().color(ui.visuals().selection.bg_fill)
                        } else {
                            text
                        };
                        let response = ui.add(
                            egui::Label::new(text)
                                .truncate()
                                .sense(egui::Sense::click()),
                        );
                        if response.clicked() {
                            toggle = true;
                        }
                        if response.double_clicked() {
                            double_clicked = true;
                        }
                        response.context_menu(|ui| {
                            menu_action = context_menu(ui);
                        });
                        response
                    }
                    2 => status_label(ui, status),
                    3 => ui.add(
                        egui::ProgressBar::new(task.progress.clamp(0.0, 1.0))
                            .show_percentage()
                            .desired_height(12.0),
                    ),
                    _ => ui
                        .add(egui::Label::new(task_detail(task)).truncate())
                        .on_hover_text(task.detail.as_str()),
                }
            });

            if row_clicked {
                toggle = true;
            }
            if toggle {
                state.toggle_check(id);
            }
            if double_clicked {
                match status {
                    TaskStatus::Waiting | TaskStatus::Failed | TaskStatus::Canceled => {
                        state.start_task(id)
                    }
                    TaskStatus::Downloading | TaskStatus::Canceling => state.reset_single_task(id),
                    TaskStatus::Completed => {
                        // 打开目录要独占 state，重新按索引取出路径字段拷贝，
                        // 借用随语句结束，不与这里的可变借用冲突。
                        let (output_path, output_directory) = {
                            let task = &state.tasks[index];
                            (task.output_path.clone(), task.output_directory.clone())
                        };
                        state.open_task_directory_paths(output_path.as_deref(), &output_directory);
                    }
                }
            }
            if let Some(action) = menu_action {
                // 右键未勾选的行时，只把这一行作为操作目标；右键浏览本身不改
                // 变勾选，点下菜单项才生效。
                if !checked {
                    state.selected_task_ids = vec![id];
                }
                apply_menu_action(ui, state, action, index);
            }
        }
        ui.label(
            RichText::new("提示：点击行或勾选框选择任务，双击可开始或取消，右键有更多操作")
                .small()
                .weak(),
        );
    });
}

/// 按可用宽度和固定比例算出五列宽度。
fn task_column_widths(total_width: f32) -> [f32; 5] {
    let spacing = TASK_COLUMN_SPACING * (TASK_COLUMN_RATIOS.len() - 1) as f32;
    let usable = (total_width - spacing).max(0.0);
    let mut widths = [0.0; 5];
    for (index, ratio) in TASK_COLUMN_RATIOS.iter().enumerate() {
        widths[index] = usable * ratio;
    }
    widths
}

/// 选中行的背景：强调色的低透明度版本，既区别于斑马纹又不遮挡文字。
fn selection_row_color(ui: &egui::Ui) -> Color32 {
    let base = ui.visuals().selection.bg_fill;
    Color32::from_rgba_unmultiplied(base.r(), base.g(), base.b(), 46)
}

/// 渲染一行固定列宽的表格内容，并绘制斑马纹或选中背景。
/// 每列宽度由调用方给定，控件一律撑满所在列，不反向影响布局。
/// 返回整行是否被点击（用于切换勾选）；表头行传 `interactive: false`。
fn task_table_row<R>(
    ui: &mut egui::Ui,
    widths: &[f32; 5],
    row_index: usize,
    selected: bool,
    interactive: bool,
    mut content: impl FnMut(&mut egui::Ui, usize) -> R,
) -> bool {
    let total = widths.iter().sum::<f32>() + TASK_COLUMN_SPACING * (widths.len() - 1) as f32;
    // 背景用 painter 先画，不占用布局空间，随后 horizontal 会落在同一位置。
    let row_rect = egui::Rect::from_min_size(ui.cursor().min, egui::vec2(total, TASK_ROW_HEIGHT));
    let background = if selected {
        Some(selection_row_color(ui))
    } else if row_index % 2 == 1 {
        Some(ui.visuals().faint_bg_color)
    } else {
        None
    };
    if let Some(color) = background {
        ui.painter().rect_filled(row_rect.expand(2.0), 4.0, color);
    }
    // 整行点击区域：点击行任意位置切换勾选。
    let row_clicked = if interactive {
        ui.interact(
            row_rect,
            ui.id().with(("task_row", row_index)),
            egui::Sense::click(),
        )
        .clicked()
    } else {
        false
    };
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = TASK_COLUMN_SPACING;
        for (column, width) in widths.iter().enumerate() {
            ui.allocate_ui_with_layout(
                egui::vec2(*width, TASK_ROW_HEIGHT),
                Layout::left_to_right(Align::Center),
                |ui| content(ui, column),
            );
        }
    });
    row_clicked
}

/// 右键菜单：开始、取消、重试、删除作用于当前选中的全部任务；
/// 编辑、复制链接、打开目录只针对右键的那一行。
/// 这里只记录选择，实际动作由 `apply_menu_action` 在行外执行。
fn context_menu(ui: &mut egui::Ui) -> Option<MenuAction> {
    let mut action = None;
    if ui.button("开始").clicked() {
        action = Some(MenuAction::StartSelected);
        ui.close_menu();
    }
    if ui.button("取消").clicked() {
        action = Some(MenuAction::CancelSelected);
        ui.close_menu();
    }
    if ui.button("编辑").clicked() {
        action = Some(MenuAction::EditThis);
        ui.close_menu();
    }
    if ui.button("重试").clicked() {
        action = Some(MenuAction::RetrySelected);
        ui.close_menu();
    }
    if ui.button("删除").clicked() {
        action = Some(MenuAction::DeleteSelected);
        ui.close_menu();
    }
    if ui.button("复制链接").clicked() {
        action = Some(MenuAction::CopyLink);
        ui.close_menu();
    }
    if ui.button("打开目录").clicked() {
        action = Some(MenuAction::OpenThis);
        ui.close_menu();
    }
    action
}

/// 执行右键菜单选中的动作。批量操作作用于当前勾选集合；
/// 单行操作在这里才按索引取任务数据（仅在用户点下菜单项时发生一次克隆），
/// 避免渲染期间的借用延伸到可变操作里。
fn apply_menu_action(ui: &mut egui::Ui, state: &mut AppState, action: MenuAction, index: usize) {
    match action {
        MenuAction::StartSelected => state.start_selected_tasks(),
        MenuAction::CancelSelected => state.cancel_selected_tasks(),
        MenuAction::RetrySelected => state.retry_selected_tasks(),
        MenuAction::DeleteSelected => state.delete_selected_tasks(),
        MenuAction::CopyLink => {
            let task = state.tasks[index].clone();
            ui.output_mut(|writer| writer.copied_text = task.source_url);
        }
        MenuAction::EditThis => {
            let task = state.tasks[index].clone();
            state.edit_task = Some(EditTask {
                id: task.id,
                source_url: task.source_url,
                output_name: task.output_name,
                output_directory: task.output_directory,
                request_headers: task.request_headers,
            });
        }
        MenuAction::OpenThis => {
            let task = state.tasks[index].clone();
            state.open_task_directory(&task);
        }
    }
}

fn status_label(ui: &mut egui::Ui, status: TaskStatus) -> egui::Response {
    let color = match status {
        TaskStatus::Waiting => Color32::GRAY,
        TaskStatus::Downloading => Color32::from_rgb(48, 122, 216),
        TaskStatus::Canceling => Color32::from_rgb(214, 166, 31),
        TaskStatus::Completed => Color32::from_rgb(55, 149, 82),
        TaskStatus::Failed => Color32::from_rgb(214, 69, 65),
        TaskStatus::Canceled => Color32::from_rgb(226, 138, 40),
    };
    ui.colored_label(color, status.label())
}

fn task_detail(task: &TaskSnapshot) -> String {
    if task.status == TaskStatus::Failed || task.status == TaskStatus::Canceled {
        return task.detail.clone();
    }
    if task.status.is_active() && task.speed_bytes_per_second > 0 {
        return format!(
            "{}/s，剩余 {}",
            format_bytes(task.speed_bytes_per_second),
            format_duration(task.estimated_seconds_remaining)
        );
    }
    task.detail.clone()
}
