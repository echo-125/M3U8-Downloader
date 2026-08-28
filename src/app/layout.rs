use eframe::egui::{
    self, Align, Align2, Color32, ComboBox, Layout, Order, ScrollArea, TopBottomPanel,
    ViewportCommand,
};

use super::{
    state::{AppState, CreationTab, EditTask},
    theme::switch_label,
};
use crate::config::ProxyScheme;
use crate::core::events::TaskStatus;

pub fn render(ctx: &egui::Context, state: &mut AppState) {
    render_title_bar(ctx, state);
    render_status_bar(ctx, state);

    egui::CentralPanel::default().show(ctx, |ui| {
        ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                render_creation_area(ui, state);
                ui.add_space(8.0);
                render_task_list(ui, state);
                ui.add_space(8.0);
                render_log_area(ui, state);
            });
    });

    render_settings_window(ctx, state);
    render_edit_window(ctx, state);
    render_exit_confirmation(ctx, state);
    render_toast(ctx, state);
}

fn render_title_bar(ctx: &egui::Context, state: &mut AppState) {
    TopBottomPanel::top("title_bar").show(ctx, |ui| {
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.heading("Cat Catch Assistant");
            ui.label(format!("v{}", env!("CARGO_PKG_VERSION")));
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
        ui.add_space(6.0);
    });
}

fn render_status_bar(ctx: &egui::Context, state: &mut AppState) {
    TopBottomPanel::bottom("status_bar").show(ctx, |ui| {
        ui.separator();
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
            ui.separator();
            ui.label(format!("进行中：{}", state.active_task_count()));
            ui.separator();
            ui.label(format!("完成：{completed}"));
            ui.separator();
            ui.label(format!("失败：{failed}"));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                match &state.ffmpeg_status {
                    Some(path) => ui.label(format!("ffmpeg：{path}")),
                    None => ui.colored_label(Color32::from_rgb(214, 166, 31), "ffmpeg：未检测到"),
                };
            });
        });
    });
}

fn render_creation_area(ui: &mut egui::Ui, state: &mut AppState) {
    ui.group(|ui| {
        ui.horizontal(|ui| {
            ui.selectable_value(&mut state.creation_tab, CreationTab::Single, "单个任务");
            ui.selectable_value(&mut state.creation_tab, CreationTab::Batch, "批量添加");
            ui.selectable_value(
                &mut state.creation_tab,
                CreationTab::ManualMerge,
                "手动合并",
            );
        });
        ui.add_space(6.0);
        match state.creation_tab {
            CreationTab::Single => render_single_task_form(ui, state),
            CreationTab::Batch => render_batch_task_form(ui, state),
            CreationTab::ManualMerge => render_manual_merge_form(ui, state),
        }
    });
}

fn render_single_task_form(ui: &mut egui::Ui, state: &mut AppState) {
    ui.horizontal(|ui| {
        ui.label("M3U8 链接");
        ui.add(
            egui::TextEdit::singleline(&mut state.single_url)
                .hint_text("https://example.com/video.m3u8")
                .desired_width(ui.available_width() - 70.0),
        );
    });
    ui.horizontal(|ui| {
        ui.label("保存路径");
        ui.add(
            egui::TextEdit::singleline(&mut state.single_path)
                .hint_text(&state.settings.download_path)
                .desired_width(ui.available_width() - 130.0),
        );
        if ui.button("选择").clicked() {
            if let Some(path) = rfd::FileDialog::new().pick_folder() {
                state.single_path = path.to_string_lossy().into_owned();
            }
        }
    });
    ui.horizontal(|ui| {
        ui.label("文件名");
        ui.add(
            egui::TextEdit::singleline(&mut state.single_name)
                .hint_text("留空自动生成")
                .desired_width(ui.available_width() * 0.42),
        );
        ui.label("线程数");
        ui.add(egui::DragValue::new(&mut state.single_workers).range(1..=64));
        if ui.button("添加任务").clicked() {
            state.add_single_task();
        }
    });
    ui.horizontal(|ui| {
        ui.label("请求头");
        ui.add(
            egui::TextEdit::multiline(&mut state.single_headers)
                .hint_text(r#"{"Referer":"https://example.com"}"#)
                .desired_rows(2)
                .desired_width(ui.available_width()),
        );
    });
}

fn render_batch_task_form(ui: &mut egui::Ui, state: &mut AppState) {
    ui.horizontal(|ui| {
        ui.label("保存路径");
        ui.add(
            egui::TextEdit::singleline(&mut state.batch_path)
                .hint_text(&state.settings.download_path)
                .desired_width(ui.available_width() - 130.0),
        );
        if ui.button("选择").clicked() {
            if let Some(path) = rfd::FileDialog::new().pick_folder() {
                state.batch_path = path.to_string_lossy().into_owned();
            }
        }
    });
    ui.add_space(4.0);
    ui.add(
        egui::TextEdit::multiline(&mut state.batch_text)
            .hint_text("链接|文件名|请求头JSON\nhttps://example.com/a.m3u8|视频名称|{}")
            .desired_rows(6)
            .desired_width(ui.available_width()),
    );
    ui.add_space(4.0);
    if ui.button("批量添加").clicked() {
        state.add_batch_tasks();
    }
}

fn render_manual_merge_form(ui: &mut egui::Ui, state: &mut AppState) {
    ui.horizontal(|ui| {
        ui.label("分片文件夹");
        ui.add(
            egui::TextEdit::singleline(&mut state.manual_folder)
                .hint_text("选择包含 TS 或 fMP4 分片的文件夹")
                .desired_width(ui.available_width() - 130.0),
        );
        if ui.button("选择").clicked() {
            if let Some(path) = rfd::FileDialog::new().pick_folder() {
                state.manual_folder = path.to_string_lossy().into_owned();
            }
        }
    });
    ui.horizontal(|ui| {
        ui.label("输出名称");
        ui.add(
            egui::TextEdit::singleline(&mut state.manual_output_name)
                .hint_text("manual_merge")
                .desired_width(ui.available_width() * 0.42),
        );
        ui.checkbox(&mut state.manual_convert_to_mp4, "TS 转换为 MP4");
    });
    ui.horizontal(|ui| {
        if ui.button("扫描分片").clicked() {
            state.scan_manual_folder();
        }
        let has_segments = state
            .manual_scan
            .as_ref()
            .is_some_and(|scan| !scan.ts_segments.is_empty() || !scan.fmp4_segments.is_empty());
        if ui
            .add_enabled(has_segments, egui::Button::new("开始合并"))
            .clicked()
        {
            state.start_manual_merge();
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
            ui.colored_label(
                Color32::from_rgb(214, 69, 65),
                "fMP4 合并需要初始化段（init.mp4）",
            );
        }
    }
}

fn render_task_list(ui: &mut egui::Ui, state: &mut AppState) {
    ui.group(|ui| {
        ui.horizontal(|ui| {
            ui.heading("任务列表");
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.button("清除已完成").clicked() {
                    state.clear_completed_tasks();
                }
                if ui.button("删除").clicked() {
                    if let Some(id) = state.selected_task_id {
                        state.delete_task(id);
                    }
                }
                if ui.button("重试").clicked() {
                    if let Some(id) = state.selected_task_id {
                        state.retry_task(id);
                    }
                }
                if ui.button("取消").clicked() {
                    if let Some(id) = state.selected_task_id {
                        state.cancel_task(id);
                    }
                }
                if ui.button("全部开始").clicked() {
                    state.start_all_tasks();
                }
                if ui
                    .add_enabled(state.selected_task_id.is_some(), egui::Button::new("开始"))
                    .clicked()
                {
                    if let Some(id) = state.selected_task_id {
                        state.start_task(id);
                    }
                }
            });
        });
        ui.add_space(4.0);

        egui::Grid::new("task_list")
            .num_columns(4)
            .striped(true)
            .min_col_width(ui.available_width() / 6.0)
            .show(ui, |ui| {
                ui.strong("文件名");
                ui.strong("状态");
                ui.strong("进度");
                ui.strong("速度 / 信息");
                ui.end_row();

                if state.tasks.is_empty() {
                    ui.label("暂无任务");
                    ui.label("—");
                    ui.label("—");
                    ui.label("—");
                    ui.end_row();
                }

                for index in 0..state.tasks.len() {
                    let task = state.tasks[index].clone();
                    let selected = state.selected_task_id == Some(task.id);
                    let response = ui.selectable_label(selected, &task.output_name);
                    if response.clicked() {
                        state.selected_task_id = Some(task.id);
                    }
                    if response.double_clicked() {
                        match task.status {
                            TaskStatus::Waiting | TaskStatus::Failed | TaskStatus::Canceled => {
                                state.start_task(task.id)
                            }
                            TaskStatus::Downloading | TaskStatus::Canceling => {
                                state.cancel_task(task.id)
                            }
                            TaskStatus::Completed => state.open_task_directory(&task),
                        }
                    }
                    response.context_menu(|ui| {
                        if ui.button("开始").clicked() {
                            state.start_task(task.id);
                            ui.close_menu();
                        }
                        if ui.button("取消").clicked() {
                            state.cancel_task(task.id);
                            ui.close_menu();
                        }
                        if ui.button("编辑").clicked() {
                            state.edit_task = Some(EditTask {
                                id: task.id,
                                source_url: task.source_url.clone(),
                                output_name: task.output_name.clone(),
                            });
                            ui.close_menu();
                        }
                        if ui.button("重试").clicked() {
                            state.retry_task(task.id);
                            ui.close_menu();
                        }
                        if ui.button("删除").clicked() {
                            state.delete_task(task.id);
                            ui.close_menu();
                        }
                        if ui.button("复制链接").clicked() {
                            ui.output_mut(|writer| writer.copied_text = task.source_url.clone());
                            ui.close_menu();
                        }
                        if ui.button("打开目录").clicked() {
                            state.open_task_directory(&task);
                            ui.close_menu();
                        }
                    });
                    status_label(ui, task.status);
                    ui.add(
                        egui::ProgressBar::new(task.progress.clamp(0.0, 1.0))
                            .show_percentage()
                            .desired_width(ui.available_width() * 0.8),
                    );
                    ui.label(task_detail(&task));
                    ui.end_row();
                }
            });
    });
}

fn status_label(ui: &mut egui::Ui, status: TaskStatus) {
    let color = match status {
        TaskStatus::Waiting => Color32::GRAY,
        TaskStatus::Downloading => Color32::from_rgb(48, 122, 216),
        TaskStatus::Canceling => Color32::from_rgb(214, 166, 31),
        TaskStatus::Completed => Color32::from_rgb(55, 149, 82),
        TaskStatus::Failed => Color32::from_rgb(214, 69, 65),
        TaskStatus::Canceled => Color32::from_rgb(226, 138, 40),
    };
    ui.colored_label(color, status.label());
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
    ui.group(|ui| {
        ui.horizontal(|ui| {
            ui.heading("日志");
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.button("清空").clicked() {
                    state.logs.clear();
                }
            });
        });
        ui.add_space(4.0);
        ScrollArea::vertical()
            .max_height(180.0)
            .auto_shrink([false, false])
            .stick_to_bottom(true)
            .show(ui, |ui| {
                if state.logs.is_empty() {
                    ui.label("暂无日志");
                }
                for entry in state.logs.entries() {
                    let color = match entry.level {
                        crate::logging::LogLevel::Info => None,
                        crate::logging::LogLevel::Warning => Some(Color32::from_rgb(214, 166, 31)),
                        crate::logging::LogLevel::Error => Some(Color32::from_rgb(214, 69, 65)),
                    };
                    let text = format!("[{}] {}", entry.level.label(), entry.message);
                    if let Some(color) = color {
                        ui.colored_label(color, text);
                    } else {
                        ui.label(text);
                    }
                }
            });
    });
}

fn render_settings_window(ctx: &egui::Context, state: &mut AppState) {
    let mut open = state.settings_open;
    if open && state.settings_before_edit.is_none() {
        state.settings_before_edit = Some(state.settings.clone());
    }
    egui::Window::new("设置")
        .open(&mut open)
        .collapsible(false)
        .default_width(560.0)
        .show(ctx, |ui| {
            egui::Grid::new("settings_grid")
                .num_columns(2)
                .spacing([12.0, 6.0])
                .show(ui, |ui| {
                    ui.label("默认下载路径");
                    ui.horizontal(|ui| {
                        ui.add(
                            egui::TextEdit::singleline(&mut state.settings.download_path)
                                .desired_width(ui.available_width() - 70.0),
                        );
                        if ui.button("选择").clicked() {
                            if let Some(path) = rfd::FileDialog::new().pick_folder() {
                                state.settings.download_path = path.to_string_lossy().into_owned();
                            }
                        }
                    });
                    ui.end_row();

                    ui.label("最大并发任务数");
                    ui.add(
                        egui::DragValue::new(&mut state.settings.max_concurrent_downloads)
                            .range(1..=16),
                    );
                    ui.end_row();

                    ui.label("默认单任务线程数");
                    ui.add(egui::DragValue::new(&mut state.settings.max_workers).range(1..=64));
                    ui.end_row();

                    ui.label("临时文件");
                    ui.horizontal(|ui| {
                        ui.checkbox(&mut state.settings.auto_cleanup, "成功后自动清理");
                        ui.checkbox(&mut state.settings.keep_temp, "保留临时文件用于排查");
                    });
                    ui.end_row();

                    ui.label("尾部加速阈值");
                    ui.add(
                        egui::DragValue::new(&mut state.settings.tail_threshold)
                            .range(1..=99)
                            .suffix("%"),
                    );
                    ui.end_row();

                    ui.label("尾部加速倍数");
                    ui.add(egui::DragValue::new(&mut state.settings.tail_boost).range(1..=8));
                    ui.end_row();

                    ui.label("代理");
                    ui.horizontal(|ui| {
                        ui.checkbox(&mut state.settings.proxy.enabled, "启用");
                        let mut scheme = state.settings.proxy.scheme;
                        ComboBox::from_id_salt("proxy_scheme")
                            .selected_text(state.proxy_scheme_label())
                            .show_ui(ui, |ui| {
                                ui.selectable_value(&mut scheme, ProxyScheme::Http, "HTTP");
                                ui.selectable_value(&mut scheme, ProxyScheme::Https, "HTTPS");
                                ui.selectable_value(&mut scheme, ProxyScheme::Socks5, "SOCKS5");
                            });
                        if scheme != state.settings.proxy.scheme {
                            state.set_proxy_scheme(scheme);
                        }
                    });
                    ui.end_row();

                    ui.label("代理主机");
                    ui.add(
                        egui::TextEdit::singleline(&mut state.settings.proxy.host)
                            .desired_width(ui.available_width() * 0.7),
                    );
                    ui.end_row();

                    ui.label("代理端口");
                    ui.add(egui::DragValue::new(&mut state.settings.proxy.port).range(0..=65535));
                    ui.end_row();

                    ui.label("代理认证");
                    ui.horizontal(|ui| {
                        ui.add(
                            egui::TextEdit::singleline(&mut state.settings.proxy.username)
                                .hint_text("用户名")
                                .desired_width(120.0),
                        );
                        ui.add(
                            egui::TextEdit::singleline(&mut state.settings.proxy.password)
                                .hint_text("密码")
                                .password(true)
                                .desired_width(120.0),
                        );
                    });
                    ui.end_row();

                    ui.label("ffmpeg");
                    ui.horizontal(|ui| {
                        ui.checkbox(&mut state.settings.ffmpeg.auto_detect, "自动检测");
                        ui.add(
                            egui::TextEdit::singleline(&mut state.settings.ffmpeg.manual_path)
                                .hint_text("手动路径")
                                .desired_width(ui.available_width() - 120.0),
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
                    ui.end_row();

                    ui.label("ffmpeg 状态");
                    match &state.ffmpeg_status {
                        Some(path) => ui.label(path),
                        None => ui.colored_label(Color32::from_rgb(214, 166, 31), "未检测到"),
                    };
                    ui.end_row();

                    ui.label("文件日志");
                    ui.horizontal(|ui| {
                        ui.checkbox(&mut state.settings.logging.file_enabled, "启用");
                        let mut rotation = state.settings.logging.rotation.clone();
                        ComboBox::from_id_salt("logging_rotation")
                            .selected_text(if rotation == "daily" {
                                "按天"
                            } else {
                                "按大小"
                            })
                            .show_ui(ui, |ui| {
                                ui.selectable_value(&mut rotation, "daily".to_string(), "按天");
                                ui.selectable_value(&mut rotation, "size".to_string(), "按大小");
                            });
                        state.settings.logging.rotation = rotation;
                        ui.add(
                            egui::DragValue::new(&mut state.settings.logging.max_size_mb)
                                .range(1..=100)
                                .suffix("MB"),
                        );
                    });
                    ui.end_row();
                });

            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button("保存").clicked() {
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
    if !open {
        // 关闭设置窗口时丢弃未保存的修改，避免界面显示的值与核心实际使用的配置不一致。
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
        .show(ctx, |ui| {
            ui.label("M3U8 链接");
            ui.add(
                egui::TextEdit::singleline(&mut edit.source_url)
                    .desired_width(ui.available_width()),
            );
            ui.label("文件名");
            ui.add(
                egui::TextEdit::singleline(&mut edit.output_name)
                    .desired_width(ui.available_width()),
            );
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button("保存").clicked() {
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
                state.active_task_count()
            ));
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button("取消").clicked() {
                    state.show_exit_confirmation = false;
                }
                if ui.button("退出程序").clicked() {
                    state.show_exit_confirmation = false;
                    state.allow_exit = true;
                    ctx.send_viewport_cmd(ViewportCommand::Close);
                }
            });
        });
}

fn render_toast(ctx: &egui::Context, state: &mut AppState) {
    let Some(toast) = &state.toast else {
        return;
    };
    let fill = if toast.error {
        Color32::from_rgba_unmultiplied(120, 28, 28, 235)
    } else {
        Color32::from_rgba_unmultiplied(24, 78, 44, 235)
    };
    egui::Area::new(egui::Id::new("task_toast"))
        .anchor(Align2::RIGHT_TOP, [-16.0, 16.0])
        .order(Order::Foreground)
        .show(ctx, |ui| {
            egui::Frame::default()
                .fill(fill)
                .stroke(egui::Stroke::new(1.0_f32, Color32::BLACK))
                .rounding(8.0)
                .inner_margin(10.0)
                .show(ui, |ui| {
                    ui.set_min_width(220.0);
                    ui.colored_label(Color32::WHITE, &toast.message);
                });
        });
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
