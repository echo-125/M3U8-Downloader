use eframe::egui::{
    self, Align, Align2, Color32, ComboBox, Layout, Order, RichText, ScrollArea, TopBottomPanel,
    ViewportCommand,
};

use super::{
    state::{AppState, CreationTab, EditTask},
    theme::switch_label,
};
use crate::config::ProxyScheme;
use crate::core::events::{TaskSnapshot, TaskStatus};

/// 表单左侧标签的固定宽度，保证各行输入框左缘对齐。
const FORM_LABEL_WIDTH: f32 = 76.0;
/// 表单控件高度，与按钮高度一致。
const FORM_CONTROL_HEIGHT: f32 = 30.0;
/// 表单行尾操作按钮的预留宽度，容纳「粘贴」「选择」这类两字按钮。
const FORM_BUTTON_WIDTH: f32 = 64.0;
/// 多行输入框每增加一行的高度增量。
const FORM_EXTRA_ROW_HEIGHT: f32 = 19.0;
/// 设置窗口标签宽度，容纳「默认单任务线程数」这类较长文案。
const SETTINGS_LABEL_WIDTH: f32 = 128.0;
/// 设置窗口内容区宽度固定，否则输入框宽度跟随窗口变化会与窗口尺寸互相推高，
/// 导致设置窗口每帧变大。
const SETTINGS_CONTENT_WIDTH: f32 = 660.0;
/// 设置窗口滚动区最大高度。
///
/// 必须用固定值而不是「可用高度减去按钮区」：后者会让滚动区高度依赖窗口高度，
/// 而窗口高度又由内容决定，形成正反馈导致窗口逐帧变大。
const SETTINGS_SCROLL_MAX_HEIGHT: f32 = 450.0;
/// 编辑任务窗口内容区宽度。
const EDIT_CONTENT_WIDTH: f32 = 520.0;

/// 任务列表四列的宽度比例：文件名 / 状态 / 进度 / 速度信息。
/// 固定比例而非按内容自适应，否则速度和剩余时间每帧变化会让列宽持续抖动。
const TASK_COLUMN_RATIOS: [f32; 4] = [0.32, 0.12, 0.26, 0.30];
const TASK_COLUMN_TITLES: [&str; 4] = ["文件名", "状态", "进度", "速度 / 信息"];
/// 任务行高固定，行高不随内容变化。
const TASK_ROW_HEIGHT: f32 = 28.0;
const TASK_COLUMN_SPACING: f32 = 10.0;
/// 日志行高，固定值让 ScrollArea 能按可视范围只渲染可见行。
const LOG_ROW_HEIGHT: f32 = 18.0;

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
    render_toast(ctx, state);
}

/// 卡片容器：圆角面板配标题，给各功能区建立清晰的视觉边界。
fn card<R>(ui: &mut egui::Ui, title: &str, content: impl FnOnce(&mut egui::Ui) -> R) -> R {
    egui::Frame::default()
        .fill(ui.visuals().window_fill)
        .stroke(ui.visuals().widgets.noninteractive.bg_stroke)
        .rounding(8.0)
        .inner_margin(egui::Margin::same(12.0))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.label(RichText::new(title).heading());
            ui.add_space(2.0);
            content(ui)
        })
        .inner
}

/// 强调色主按钮，用于界面上唯一的主要操作；次要操作用普通按钮即可形成层级。
/// 暗色主题的强调色偏亮，白字对比度不足，因此按主题切换文字颜色。
fn primary_button(ui: &mut egui::Ui, enabled: bool, text: &str) -> egui::Response {
    let text_color = if ui.visuals().dark_mode {
        Color32::from_rgb(12, 24, 40)
    } else {
        Color32::WHITE
    };
    let button = egui::Button::new(RichText::new(text).color(text_color))
        .fill(ui.visuals().selection.bg_fill);
    ui.add_enabled(enabled, button)
}

/// 描边样式的次要按钮。与填充的主按钮并排时形成主次层级，
/// 避免一行里出现两个同样抢眼的实心按钮。
fn outline_button(ui: &mut egui::Ui, text: &str) -> egui::Response {
    let accent = ui.visuals().selection.bg_fill;
    let response = ui.add(
        egui::Button::new(RichText::new(text).color(accent))
            .fill(Color32::TRANSPARENT)
            .stroke(egui::Stroke::new(1.0_f32, accent)),
    );
    // 显式设置 fill 会盖掉 Button 内置的悬停反馈（它靠 weak_bg_fill 变化），
    // 这里补一道更粗的描边作为悬停提示，即时模式下下一帧生效。
    if response.hovered() {
        ui.painter().rect_stroke(
            response.rect,
            ui.visuals().widgets.hovered.rounding,
            egui::Stroke::new(2.0_f32, accent),
        );
    }
    response
}

/// 危险操作按钮：红色填充，用于退出确认等需要强调后果的操作。
fn danger_button(ui: &mut egui::Ui, text: &str) -> egui::Response {
    ui.add(
        egui::Button::new(RichText::new(text).color(Color32::WHITE))
            .fill(Color32::from_rgb(214, 69, 65)),
    )
}

/// 统一的单行输入框：内边距与按钮保持一致，避免各处高低不齐。
fn input<'a>(text: &'a mut String) -> egui::TextEdit<'a> {
    egui::TextEdit::singleline(text).margin(egui::Margin::symmetric(8.0, 5.0))
}

/// 统一的多行输入框。
fn input_multiline<'a>(text: &'a mut String) -> egui::TextEdit<'a> {
    egui::TextEdit::multiline(text).margin(egui::Margin::symmetric(8.0, 5.0))
}

/// 统一样式的数字输入框：固定宽度与高度，与输入框和按钮保持一致。
fn number_input<T: egui::emath::Numeric>(
    ui: &mut egui::Ui,
    value: &mut T,
    range: std::ops::RangeInclusive<T>,
    suffix: &str,
) -> egui::Response {
    let mut widget = egui::DragValue::new(value).range(range).speed(1);
    if !suffix.is_empty() {
        widget = widget.suffix(suffix);
    }
    ui.add_sized([110.0, FORM_CONTROL_HEIGHT], widget)
}

/// 表单输入框宽度：占满当前行剩余空间；行尾有按钮时按按钮宽度预留。
///
/// 表单一律用 `horizontal` 而不是 `Grid`：Grid 在测量列宽时拿到的可用宽度极小，
/// 输入框的 `desired_width` 会被截断成几十像素，列宽随即被锁死。
fn form_field_width(ui: &egui::Ui, trailing_button: bool) -> f32 {
    let reserved = if trailing_button {
        FORM_BUTTON_WIDTH + ui.spacing().item_spacing.x
    } else {
        0.0
    };
    (ui.available_width() - reserved).max(160.0)
}

/// 表单行：固定宽度标签 + 撑满剩余空间的单行输入框，行尾可带一个按钮。
/// 返回行尾按钮是否被点击。
fn form_field(
    ui: &mut egui::Ui,
    label: &str,
    text: &mut String,
    hint: &str,
    button: Option<&str>,
) -> bool {
    ui.horizontal(|ui| {
        right_label(ui, label, FORM_LABEL_WIDTH);
        let width = form_field_width(ui, button.is_some());
        ui.add_sized([width, FORM_CONTROL_HEIGHT], input(text).hint_text(hint));
        match button {
            Some(label) => ui.button(label).clicked(),
            None => false,
        }
    })
    .inner
}

/// 表单行：固定宽度标签 + 撑满剩余空间的多行输入框。
fn form_field_multiline(
    ui: &mut egui::Ui,
    label: &str,
    text: &mut String,
    hint: &str,
    rows: usize,
) {
    ui.horizontal(|ui| {
        right_label(ui, label, FORM_LABEL_WIDTH);
        let width = form_field_width(ui, false);
        let height = FORM_CONTROL_HEIGHT + (rows.saturating_sub(1) as f32) * FORM_EXTRA_ROW_HEIGHT;
        ui.add_sized([width, height], input_multiline(text).hint_text(hint));
    });
}

/// 固定高度的竖直分隔线，用于水平排列的工具栏。
///
/// 不能直接调 `ui.separator()`：它在水平布局中把高度取成当前可用高度
/// （egui `separator.rs` 的 `available_space.y`），在滚动区内会把整行撑到几百像素高。
fn vertical_divider(ui: &mut egui::Ui, height: f32) {
    let width = ui.spacing().item_spacing.x;
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::hover());
    let stroke = ui.visuals().widgets.noninteractive.bg_stroke;
    ui.painter().vline(rect.center().x, rect.y_range(), stroke);
}

/// 固定宽度、右对齐的表单标签。
fn right_label(ui: &mut egui::Ui, text: &str, width: f32) {
    ui.allocate_ui_with_layout(
        egui::vec2(width, 24.0),
        Layout::right_to_left(Align::Center),
        |ui| ui.label(text),
    );
}

/// 设置分组：分组标题 + 分割线。
fn section<R>(ui: &mut egui::Ui, title: &str, content: impl FnOnce(&mut egui::Ui) -> R) -> R {
    ui.add_space(4.0);
    ui.label(RichText::new(title).strong());
    ui.separator();
    let result = content(ui);
    ui.add_space(6.0);
    result
}

/// 设置项行：右侧对齐的标签 + 控件。
fn settings_row<R>(ui: &mut egui::Ui, label: &str, content: impl FnOnce(&mut egui::Ui) -> R) -> R {
    ui.horizontal(|ui| {
        right_label(ui, label, SETTINGS_LABEL_WIDTH);
        content(ui)
    })
    .inner
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
                    let label = switch_label(state.settings.appearance.theme);
                    if ui.button(label).clicked() {
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

fn render_creation_area(ui: &mut egui::Ui, state: &mut AppState) {
    card(ui, "新建任务", |ui| {
        ui.horizontal(|ui| {
            ui.selectable_value(&mut state.creation_tab, CreationTab::Single, "单个任务");
            ui.selectable_value(&mut state.creation_tab, CreationTab::Batch, "批量添加");
            ui.selectable_value(
                &mut state.creation_tab,
                CreationTab::ManualMerge,
                "手动合并",
            );
        });
        ui.separator();
        match state.creation_tab {
            CreationTab::Single => render_single_task_form(ui, state),
            CreationTab::Batch => render_batch_task_form(ui, state),
            CreationTab::ManualMerge => render_manual_merge_form(ui, state),
        }
    });
}

fn render_single_task_form(ui: &mut egui::Ui, state: &mut AppState) {
    if form_field(
        ui,
        "M3U8 链接",
        &mut state.single_url,
        "https://example.com/video.m3u8 或 链接|文件名|请求头JSON",
        Some("粘贴"),
    ) {
        state.paste_from_clipboard();
    }
    let pick_path = form_field(
        ui,
        "保存路径",
        &mut state.single_path,
        &state.settings.download_path,
        Some("选择"),
    );
    if pick_path {
        if let Some(path) = rfd::FileDialog::new().pick_folder() {
            state.single_path = path.to_string_lossy().into_owned();
        }
    }

    form_field(ui, "文件名", &mut state.single_name, "留空自动生成", None);
    form_field_multiline(
        ui,
        "请求头",
        &mut state.single_headers,
        r#"{"Referer":"https://example.com"}"#,
        2,
    );

    ui.add_space(2.0);
    ui.horizontal(|ui| {
        // 线程数与上方输入框共用左缘，主按钮靠右，左右职责分离，无需分隔线。
        right_label(ui, "线程数", FORM_LABEL_WIDTH);
        number_input(ui, &mut state.single_workers, 1..=64, "");
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if primary_button(ui, true, "添加任务").clicked() {
                state.add_single_task();
            }
            // 直接读剪贴板批量添加，省掉先粘贴到输入框再点添加这一步。
            if outline_button(ui, "粘贴添加").clicked() {
                state.paste_and_add_tasks();
            }
        });
    });
}

fn render_batch_task_form(ui: &mut egui::Ui, state: &mut AppState) {
    let pick_path = form_field(
        ui,
        "保存路径",
        &mut state.batch_path,
        &state.settings.download_path,
        Some("选择"),
    );
    if pick_path {
        if let Some(path) = rfd::FileDialog::new().pick_folder() {
            state.batch_path = path.to_string_lossy().into_owned();
        }
    }
    form_field_multiline(
        ui,
        "批量内容",
        &mut state.batch_text,
        "每行一条：链接|文件名|请求头JSON\nhttps://example.com/a.m3u8|视频名称|{}",
        6,
    );

    ui.add_space(2.0);
    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
        if primary_button(ui, true, "批量添加").clicked() {
            state.add_batch_tasks();
        }
    });
}

fn render_manual_merge_form(ui: &mut egui::Ui, state: &mut AppState) {
    let pick_folder = form_field(
        ui,
        "分片文件夹",
        &mut state.manual_folder,
        "选择包含 TS 或 fMP4 分片的文件夹",
        Some("选择"),
    );
    if pick_folder {
        if let Some(path) = rfd::FileDialog::new().pick_folder() {
            state.manual_folder = path.to_string_lossy().into_owned();
        }
    }
    form_field(
        ui,
        "输出名称",
        &mut state.manual_output_name,
        "manual_merge",
        None,
    );
    ui.horizontal(|ui| {
        // 空标签占位，让勾选框与上方输入框左缘对齐。
        right_label(ui, "", FORM_LABEL_WIDTH);
        ui.checkbox(
            &mut state.manual_convert_to_mp4,
            "转换为 MP4（需要 ffmpeg）",
        );
    });

    ui.add_space(2.0);
    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
        let has_segments = state
            .manual_scan
            .as_ref()
            .is_some_and(|scan| !scan.ts_segments.is_empty() || !scan.fmp4_segments.is_empty());
        if primary_button(ui, has_segments, "开始合并").clicked() {
            state.start_manual_merge();
        }
        if ui.button("扫描分片").clicked() {
            state.scan_manual_folder();
        }
    });
    if let Some(scan) = &state.manual_scan {
        ui.label(format!(
            "扫描结果：TS {} 个，fMP4 {} 个，初始化段：{}",
            scan.ts_segments.len(),
            scan.fmp4_segments.len(),
            if scan.initialization.is_some() {
                "已找到"
            } else {
                "未找到"
            }
        ));
        if !scan.fmp4_segments.is_empty() && scan.initialization.is_none() {
            ui.label(
                RichText::new("fMP4 合并需要初始化段（init.mp4）")
                    .color(Color32::from_rgb(214, 69, 65)),
            );
        }
    }
}

/// 按可用宽度和固定比例算出四列宽度。
fn task_column_widths(total_width: f32) -> [f32; 4] {
    let spacing = TASK_COLUMN_SPACING * (TASK_COLUMN_RATIOS.len() - 1) as f32;
    let usable = (total_width - spacing).max(0.0);
    let mut widths = [0.0; 4];
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
fn task_table_row<R>(
    ui: &mut egui::Ui,
    widths: &[f32; 4],
    row_index: usize,
    selected: bool,
    mut content: impl FnMut(&mut egui::Ui, usize) -> R,
) {
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
}

fn render_task_list(ui: &mut egui::Ui, state: &mut AppState) {
    card(ui, "任务列表", |ui| {
        let has_selection = !state.selected_task_ids.is_empty();
        let startable_selected = !state
            .selected_ids_where(TaskStatus::is_startable)
            .is_empty();
        let cancelable_selected = !state
            .selected_ids_where(TaskStatus::is_cancelable)
            .is_empty();
        let any_startable = state.tasks.iter().any(|task| task.status.is_startable());

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
            if ui
                .add_enabled(has_selection, egui::Button::new("删除"))
                .clicked()
            {
                state.delete_selected_tasks();
            }
            if !state.tasks.is_empty() && ui.button("全选").clicked() {
                state.select_all_tasks();
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.button("清除已结束").clicked() {
                    state.clear_finished_tasks();
                }
            });
        });
        ui.add_space(2.0);

        let widths = task_column_widths(ui.available_width());
        task_table_row(ui, &widths, 0, false, |ui, column| {
            ui.strong(TASK_COLUMN_TITLES[column])
        });
        ui.separator();

        if state.tasks.is_empty() {
            ui.label(RichText::new("暂无任务，先在上方添加一个下载任务").weak());
        }

        for index in 0..state.tasks.len() {
            let task = state.tasks[index].clone();
            let selected = state.is_task_selected(task.id);
            // 行内只记录交互结果，行外再改动状态，避免与界面查询互相借用。
            let mut toggle_selection: Option<bool> = None;
            let mut double_clicked = false;

            task_table_row(ui, &widths, index, selected, |ui, column| match column {
                0 => {
                    // 用带点击感应的 Label 而不是按钮：按钮文本会强制居中，撑满整列后短文件名会偏离左侧。
                    let text = RichText::new(task.output_name.as_str());
                    let text = if selected {
                        text.strong().color(ui.visuals().selection.bg_fill)
                    } else {
                        text
                    };
                    let response = ui.add_sized(
                        egui::vec2(widths[0], TASK_ROW_HEIGHT - 4.0),
                        egui::Label::new(text)
                            .truncate()
                            .sense(egui::Sense::click()),
                    );
                    if response.clicked() {
                        toggle_selection =
                            Some(ui.input(|input| input.modifiers.ctrl || input.modifiers.shift));
                    }
                    if response.double_clicked() {
                        double_clicked = true;
                    }
                    response.context_menu(|ui| {
                        // 右键未选中的行时，只把这一行作为操作目标。
                        if !state.is_task_selected(task.id) {
                            state.selected_task_ids = vec![task.id];
                        }
                        context_menu(ui, state, &task);
                    });
                    response
                }
                1 => status_label(ui, task.status),
                2 => ui.add_sized(
                    [widths[2], 18.0],
                    egui::ProgressBar::new(task.progress.clamp(0.0, 1.0)).show_percentage(),
                ),
                _ => ui
                    .add_sized(
                        [widths[3], TASK_ROW_HEIGHT - 4.0],
                        egui::Label::new(task_detail(&task)).truncate(),
                    )
                    .on_hover_text(task.detail.as_str()),
            });

            if let Some(additive) = toggle_selection {
                state.select_task(task.id, additive);
            }
            if double_clicked {
                match task.status {
                    TaskStatus::Waiting | TaskStatus::Failed | TaskStatus::Canceled => {
                        state.start_task(task.id)
                    }
                    TaskStatus::Downloading | TaskStatus::Canceling => state.cancel_task(task.id),
                    TaskStatus::Completed => state.open_task_directory(&task),
                }
            }
        }
        ui.label(
            RichText::new("提示：按住 Ctrl 或 Shift 点击可多选任务，双击可开始或取消")
                .small()
                .weak(),
        );
    });
}

/// 右键菜单：开始、取消、重试、删除作用于当前选中的全部任务；
/// 编辑、复制链接、打开目录只针对右键的那一行。
fn context_menu(ui: &mut egui::Ui, state: &mut AppState, task: &TaskSnapshot) {
    if ui.button("开始").clicked() {
        state.start_selected_tasks();
        ui.close_menu();
    }
    if ui.button("取消").clicked() {
        state.cancel_selected_tasks();
        ui.close_menu();
    }
    if ui.button("编辑").clicked() {
        state.edit_task = Some(EditTask {
            id: task.id,
            source_url: task.source_url.clone(),
            output_name: task.output_name.clone(),
            output_directory: task.output_directory.clone(),
            request_headers: task.request_headers.clone(),
        });
        ui.close_menu();
    }
    if ui.button("重试").clicked() {
        state.retry_selected_tasks();
        ui.close_menu();
    }
    if ui.button("删除").clicked() {
        state.delete_selected_tasks();
        ui.close_menu();
    }
    if ui.button("复制链接").clicked() {
        ui.output_mut(|writer| writer.copied_text = task.source_url.clone());
        ui.close_menu();
    }
    if ui.button("打开目录").clicked() {
        state.open_task_directory(task);
        ui.close_menu();
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

fn task_detail(task: &crate::core::events::TaskSnapshot) -> String {
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

fn render_settings_window(ctx: &egui::Context, state: &mut AppState) {
    let mut open = state.settings_open;
    if open && state.settings_before_edit.is_none() {
        state.settings_before_edit = Some(state.settings.clone());
    }

    if open {
        // 模态遮罩：压暗背景并拦截点击，点击空白处等于取消。
        let screen = ctx.screen_rect();
        let mask_clicked = egui::Area::new(egui::Id::new("settings_modal_mask"))
            .order(Order::Foreground)
            .fixed_pos(screen.min)
            .show(ctx, |ui| {
                let response = ui.allocate_rect(screen, egui::Sense::click());
                ui.painter()
                    .rect_filled(screen, 0.0, Color32::from_black_alpha(110));
                response.clicked()
            })
            .inner;

        egui::Window::new("设置")
            .open(&mut open)
            .collapsible(false)
            // 尺寸固定：内容宽度与滚动区高度都取自常量，不与窗口尺寸互相推导。
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
            .default_size([700.0, 560.0])
            .order(Order::Foreground)
            .show(ctx, |ui| {
                // 内容宽度固定：若让输入框跟随窗口宽度，二者会互相推高，窗口逐帧变大。
                ui.set_width(SETTINGS_CONTENT_WIDTH);
                egui::ScrollArea::vertical()
                    .id_salt("settings_scroll")
                    .auto_shrink([false, true])
                    .max_height(SETTINGS_SCROLL_MAX_HEIGHT)
                    .show(ui, |ui| {
                        section(ui, "常规", |ui| {
                            settings_row(ui, "默认下载路径", |ui| {
                                ui.add(
                                    input(&mut state.settings.download_path)
                                        .desired_width(ui.available_width() - 70.0),
                                );
                                if ui.button("选择").clicked() {
                                    if let Some(path) = rfd::FileDialog::new().pick_folder() {
                                        state.settings.download_path =
                                            path.to_string_lossy().into_owned();
                                    }
                                }
                            });
                        });

                        section(ui, "下载", |ui| {
                            settings_row(ui, "最大并发任务数", |ui| {
                                number_input(
                                    ui,
                                    &mut state.settings.max_concurrent_downloads,
                                    1..=16,
                                    "",
                                );
                            });
                            settings_row(ui, "默认单任务线程数", |ui| {
                                number_input(ui, &mut state.settings.max_workers, 1..=64, "");
                            });
                            settings_row(ui, "尾部加速阈值", |ui| {
                                number_input(ui, &mut state.settings.tail_threshold, 1..=99, "%");
                            });
                            settings_row(ui, "尾部加速倍数", |ui| {
                                number_input(ui, &mut state.settings.tail_boost, 1..=8, "");
                            });
                            settings_row(ui, "临时文件", |ui| {
                                ui.checkbox(&mut state.settings.auto_cleanup, "成功后自动清理");
                                ui.checkbox(&mut state.settings.keep_temp, "保留临时文件用于排查");
                            });
                        });

                        section(ui, "代理", |ui| {
                            settings_row(ui, "代理", |ui| {
                                ui.checkbox(&mut state.settings.proxy.enabled, "启用代理");
                                let mut scheme = state.settings.proxy.scheme;
                                ComboBox::from_id_salt("proxy_scheme")
                                    .selected_text(state.proxy_scheme_label())
                                    .show_ui(ui, |ui| {
                                        ui.selectable_value(&mut scheme, ProxyScheme::Http, "HTTP");
                                        ui.selectable_value(
                                            &mut scheme,
                                            ProxyScheme::Https,
                                            "HTTPS",
                                        );
                                        ui.selectable_value(
                                            &mut scheme,
                                            ProxyScheme::Socks5,
                                            "SOCKS5",
                                        );
                                    });
                                if scheme != state.settings.proxy.scheme {
                                    state.set_proxy_scheme(scheme);
                                }
                            });
                            settings_row(ui, "代理主机", |ui| {
                                ui.add(
                                    input(&mut state.settings.proxy.host)
                                        .desired_width(ui.available_width()),
                                );
                            });
                            settings_row(ui, "代理端口", |ui| {
                                number_input(ui, &mut state.settings.proxy.port, 0..=65535, "");
                            });
                            settings_row(ui, "代理认证", |ui| {
                                ui.add(
                                    input(&mut state.settings.proxy.username)
                                        .hint_text("用户名")
                                        .desired_width(130.0),
                                );
                                ui.add(
                                    input(&mut state.settings.proxy.password)
                                        .hint_text("密码")
                                        .password(true)
                                        .desired_width(130.0),
                                );
                            });
                        });

                        section(ui, "ffmpeg", |ui| {
                            settings_row(ui, "检测方式", |ui| {
                                ui.checkbox(&mut state.settings.ffmpeg.auto_detect, "自动检测");
                                ui.add(
                                    input(&mut state.settings.ffmpeg.manual_path)
                                        .hint_text("手动路径")
                                        .desired_width(ui.available_width() - 70.0),
                                );
                                if ui.button("选择").clicked() {
                                    if let Some(path) = rfd::FileDialog::new()
                                        .add_filter("ffmpeg", &["exe"])
                                        .pick_file()
                                    {
                                        state.settings.ffmpeg.manual_path =
                                            path.to_string_lossy().into_owned();
                                    }
                                }
                            });
                            settings_row(ui, "当前状态", |ui| {
                                match &state.ffmpeg_status {
                                    Some(path) => ui.label(RichText::new(path).weak()),
                                    None => ui.label(
                                        RichText::new("未检测到")
                                            .color(Color32::from_rgb(214, 166, 31)),
                                    ),
                                };
                            });
                        });

                        section(ui, "日志", |ui| {
                            settings_row(ui, "文件日志", |ui| {
                                ui.checkbox(&mut state.settings.logging.file_enabled, "启用");
                                let mut rotation = state.settings.logging.rotation.clone();
                                ComboBox::from_id_salt("logging_rotation")
                                    .selected_text(if rotation == "daily" {
                                        "按天"
                                    } else {
                                        "按大小"
                                    })
                                    .show_ui(ui, |ui| {
                                        ui.selectable_value(
                                            &mut rotation,
                                            "daily".to_string(),
                                            "按天",
                                        );
                                        ui.selectable_value(
                                            &mut rotation,
                                            "size".to_string(),
                                            "按大小",
                                        );
                                    });
                                state.settings.logging.rotation = rotation;
                                number_input(
                                    ui,
                                    &mut state.settings.logging.max_size_mb,
                                    1..=100,
                                    "MB",
                                );
                            });
                        });
                    });

                ui.add_space(8.0);
                ui.separator();
                ui.add_space(6.0);
                // 重新固定宽度，让按钮右缘与上方内容区对齐。
                ui.set_width(SETTINGS_CONTENT_WIDTH);
                // 主操作固定在右下角，位置稳定便于连续操作。
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if primary_button(ui, true, "保存设置").clicked() {
                        state.save_settings();
                    }
                    if ui.button("恢复默认").clicked() {
                        state.reset_settings();
                    }
                    if ui.button("重新检测 ffmpeg").clicked() {
                        state
                            .manager
                            .send(crate::core::events::TaskCommand::DetectFfmpeg);
                    }
                });
            });

        if mask_clicked {
            open = false;
        }
    }

    if !open {
        // 关闭设置窗口（点遮罩或右上角关闭）时丢弃未保存的修改，避免界面显示的值与核心实际使用的配置不一致。
        if let Some(backup) = state.settings_before_edit.take() {
            state.settings = backup;
        }
    }
    state.settings_open = open;
}

fn render_edit_window(ctx: &egui::Context, state: &mut AppState) {
    let mut open = state.edit_task.is_some();
    if !open {
        return;
    }
    let Some(mut edit) = state.edit_task.clone() else {
        return;
    };
    let mut save_clicked = false;
    egui::Window::new("编辑任务")
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
        // 必须给出初始宽度：窗口宽度若只由内容决定，而输入框宽度又取自窗口可用宽度，
        // 二者循环依赖会让窗口停在极窄的初始尺寸上。
        .default_width(EDIT_CONTENT_WIDTH + 40.0)
        .show(ctx, |ui| {
            ui.set_width(EDIT_CONTENT_WIDTH);
            ui.horizontal(|ui| {
                right_label(ui, "M3U8 链接", SETTINGS_LABEL_WIDTH);
                ui.add(input(&mut edit.source_url).desired_width(ui.available_width() - 8.0));
            });
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                right_label(ui, "保存路径", SETTINGS_LABEL_WIDTH);
                ui.add(
                    input(&mut edit.output_directory).desired_width(ui.available_width() - 70.0),
                );
                if ui.button("选择").clicked() {
                    if let Some(path) = rfd::FileDialog::new().pick_folder() {
                        edit.output_directory = path.to_string_lossy().into_owned();
                    }
                }
            });
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                right_label(ui, "文件名", SETTINGS_LABEL_WIDTH);
                ui.add(input(&mut edit.output_name).desired_width(ui.available_width() - 8.0));
            });
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                right_label(ui, "请求头 JSON", SETTINGS_LABEL_WIDTH);
                ui.add(
                    input_multiline(&mut edit.request_headers)
                        .hint_text(r#"{"Referer":"https://example.com"}"#)
                        .desired_rows(2)
                        .desired_width(ui.available_width() - 8.0),
                );
            });
            ui.add_space(8.0);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if primary_button(ui, true, "保存").clicked() {
                    save_clicked = true;
                }
                if ui.button("取消").clicked() {
                    state.edit_task = None;
                }
            });
        });
    if save_clicked {
        state.edit_task = Some(edit);
        state.save_edited_task();
    } else if state.edit_task.is_some() {
        state.edit_task = Some(edit);
    }
    if !open {
        state.edit_task = None;
    }
}

fn render_exit_confirmation(ctx: &egui::Context, state: &mut AppState) {
    if !state.show_exit_confirmation {
        return;
    }
    egui::Window::new("确认退出")
        .collapsible(false)
        .resizable(false)
        .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            ui.label(format!(
                "当前有 {} 个任务仍在进行，确定要退出程序吗？",
                state.exit_confirmation_count
            ));
            ui.label(
                RichText::new("退出后正在下载的任务会被中断，进度会保留")
                    .small()
                    .weak(),
            );
            ui.add_space(12.0);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if danger_button(ui, "退出程序").clicked() {
                    state.show_exit_confirmation = false;
                    state.allow_exit = true;
                    ctx.send_viewport_cmd(ViewportCommand::Close);
                }
                if ui.button("取消").clicked() {
                    state.show_exit_confirmation = false;
                }
            });
        });
}

fn render_toast(ctx: &egui::Context, state: &mut AppState) {
    let Some(toast) = &state.toast else {
        return;
    };
    let message = toast.message.clone();
    let error = toast.error;
    let dark = ctx.style().visuals.dark_mode;
    let accent = if error {
        Color32::from_rgb(214, 69, 65)
    } else {
        Color32::from_rgb(46, 160, 96)
    };
    let fill = if error {
        if dark {
            Color32::from_rgba_unmultiplied(76, 32, 32, 244)
        } else {
            Color32::from_rgb(253, 238, 236)
        }
    } else if dark {
        Color32::from_rgba_unmultiplied(26, 66, 42, 244)
    } else {
        Color32::from_rgb(237, 248, 240)
    };
    let mut close_clicked = false;
    egui::Area::new(egui::Id::new("task_toast"))
        .anchor(Align2::RIGHT_TOP, [-16.0, 56.0])
        .order(Order::Foreground)
        .show(ctx, |ui| {
            let frame = egui::Frame::default()
                .fill(fill)
                .stroke(egui::Stroke::new(1.0_f32, accent))
                .rounding(8.0)
                .inner_margin(egui::Margin::symmetric(14.0, 10.0))
                .show(ui, |ui| {
                    ui.set_min_width(280.0);
                    let close_pressed = ui
                        .horizontal(|ui| {
                            ui.label(
                                RichText::new(if error { "操作失败" } else { "完成" })
                                    .strong()
                                    .color(accent),
                            );
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                ui.small_button("×").clicked()
                            })
                            .inner
                        })
                        .inner;
                    ui.label(RichText::new(&message).small());
                    close_pressed
                });
            // 点击 Toast 任意位置或右上角关闭按钮都立即关闭。
            if frame.response.clicked() || frame.inner {
                close_clicked = true;
            }
        });
    if close_clicked {
        state.toast = None;
    }
}

fn format_bytes(bytes: u64) -> String {
    let value = bytes as f64;
    if value >= 1024.0 * 1024.0 * 1024.0 {
        format!("{:.2} GB", value / (1024.0 * 1024.0 * 1024.0))
    } else if value >= 1024.0 * 1024.0 {
        format!("{:.2} MB", value / (1024.0 * 1024.0))
    } else if value >= 1024.0 {
        format!("{:.2} KB", value / 1024.0)
    } else {
        format!("{bytes} B")
    }
}

fn format_duration(seconds: u64) -> String {
    let minutes = seconds / 60;
    let seconds = seconds % 60;
    if minutes >= 60 {
        format!("{}小时{}分", minutes / 60, minutes % 60)
    } else if minutes > 0 {
        format!("{minutes}分{seconds}秒")
    } else {
        format!("{seconds}秒")
    }
}
