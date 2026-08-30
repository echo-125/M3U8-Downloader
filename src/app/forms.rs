//! 新建任务区：单个任务、批量添加、手动合并三个标签页的表单。

use eframe::egui::{self, Align, Color32, Layout, RichText};

use super::{
    state::{AppState, CreationTab},
    widgets::{
        card, form_field, form_field_multiline, number_input, outline_button, path_dialog_string,
        primary_button, right_label, FORM_LABEL_WIDTH,
    },
};

pub fn render_creation_area(ui: &mut egui::Ui, state: &mut AppState) {
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
            match path_dialog_string(&path) {
                Some(text) => state.single_path = text,
                None => state.notify_error("所选路径包含无法识别的字符，未能应用"),
            }
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
            match path_dialog_string(&path) {
                Some(text) => state.batch_path = text,
                None => state.notify_error("所选路径包含无法识别的字符，未能应用"),
            }
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
            match path_dialog_string(&path) {
                Some(text) => state.manual_folder = text,
                None => state.notify_error("所选路径包含无法识别的字符，未能应用"),
            }
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
