//! 浮层界面：设置窗口、任务编辑窗口、两个确认弹窗与 Toast。

use eframe::egui::{
    self, Align, Align2, Color32, ComboBox, Layout, Order, RichText, ViewportCommand,
};

use super::{
    state::AppState,
    widgets::{
        danger_button, input, input_multiline, number_input, path_dialog_string, primary_button,
        right_label, section, settings_row, EDIT_CONTENT_WIDTH, SETTINGS_CONTENT_WIDTH,
        SETTINGS_LABEL_WIDTH, SETTINGS_SCROLL_MAX_HEIGHT,
    },
};
use crate::config::ProxyScheme;

pub fn render_settings_window(ctx: &egui::Context, state: &mut AppState) {
    let mut open = state.settings_open;
    // 保存设置成功后由这里关闭窗口，见下方按钮处理。
    let mut close_after_save = false;
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
                        render_general_section(ui, state);
                        render_download_section(ui, state);
                        render_proxy_section(ui, state);
                        render_ffmpeg_section(ui, state);
                        render_logging_section(ui, state);
                    });

                ui.add_space(8.0);
                ui.separator();
                ui.add_space(6.0);
                // 重新固定宽度，让按钮右缘与上方内容区对齐。
                ui.set_width(SETTINGS_CONTENT_WIDTH);
                // 主操作固定在右下角，位置稳定便于连续操作。
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    // 保存成功后关闭窗口。不能直接改 open：.open(&mut open) 与闭包
                    // 对 open 的借用冲突，这里只置标志，show 结束后统一处理。
                    if primary_button(ui, true, "保存设置").clicked() && state.save_settings() {
                        close_after_save = true;
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

        if close_after_save {
            open = false;
        }
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

fn render_general_section(ui: &mut egui::Ui, state: &mut AppState) {
    section(ui, "常规", |ui| {
        settings_row(ui, "默认下载路径", |ui| {
            ui.add(
                input(&mut state.settings.download_path).desired_width(ui.available_width() - 70.0),
            );
            if ui.button("选择").clicked() {
                if let Some(path) = rfd::FileDialog::new().pick_folder() {
                    match path_dialog_string(&path) {
                        Some(text) => state.settings.download_path = text,
                        None => state.notify_error("所选路径包含无法识别的字符，未能应用"),
                    }
                }
            }
        });
    });
}

fn render_download_section(ui: &mut egui::Ui, state: &mut AppState) {
    section(ui, "下载", |ui| {
        settings_row(ui, "最大并发任务数", |ui| {
            number_input(ui, &mut state.settings.max_concurrent_downloads, 1..=16, "");
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
}

fn render_proxy_section(ui: &mut egui::Ui, state: &mut AppState) {
    section(ui, "代理", |ui| {
        settings_row(ui, "代理", |ui| {
            ui.checkbox(&mut state.settings.proxy.enabled, "启用代理");
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
        settings_row(ui, "代理主机", |ui| {
            ui.add(input(&mut state.settings.proxy.host).desired_width(ui.available_width()));
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
}

fn render_ffmpeg_section(ui: &mut egui::Ui, state: &mut AppState) {
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
                    match path_dialog_string(&path) {
                        Some(text) => state.settings.ffmpeg.manual_path = text,
                        None => state.notify_error("所选路径包含无法识别的字符，未能应用"),
                    }
                }
            }
        });
        // 状态与路径分两行：状态行给明确的检测结论，路径行放完整位置。
        settings_row(ui, "检测状态", |ui| {
            match &state.ffmpeg_status {
                Some(_) => {
                    ui.label(RichText::new("已检测到").color(Color32::from_rgb(55, 149, 82)))
                }
                None => ui.label(RichText::new("未检测到").color(Color32::from_rgb(214, 166, 31))),
            };
        });
        settings_row(ui, "ffmpeg 路径", |ui| {
            match &state.ffmpeg_status {
                Some(path) => ui.label(RichText::new(path).weak()),
                None => ui.label(RichText::new("—").weak()),
            };
        });
    });
}

fn render_logging_section(ui: &mut egui::Ui, state: &mut AppState) {
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
                    ui.selectable_value(&mut rotation, "daily".to_string(), "按天");
                    ui.selectable_value(&mut rotation, "size".to_string(), "按大小");
                });
            state.settings.logging.rotation = rotation;
            number_input(ui, &mut state.settings.logging.max_size_mb, 1..=100, "MB");
        });
    });
}

pub fn render_edit_window(ctx: &egui::Context, state: &mut AppState) {
    let mut open = state.edit_task.is_some();
    if !open {
        return;
    }
    let Some(mut edit) = state.edit_task.clone() else {
        return;
    };
    let mut save_clicked = false;
    let mut canceled = false;
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
                        match path_dialog_string(&path) {
                            Some(text) => edit.output_directory = text,
                            None => state.notify_error("所选路径包含无法识别的字符，未能应用"),
                        }
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
                    canceled = true;
                    state.edit_task = None;
                }
            });
        });
    if save_clicked {
        state.edit_task = Some(edit);
        state.save_edited_task();
    } else if !canceled && open {
        // 未保存也未取消：写回编辑中的内容，下一帧重建窗口时不丢输入。
        // 点了取消（edit_task 已被置空）则不写回。
        state.edit_task = Some(edit);
    }
    if !open {
        state.edit_task = None;
    }
}

pub fn render_exit_confirmation(ctx: &egui::Context, state: &mut AppState) {
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

/// 清空任务列表前的二次确认弹窗。
pub fn render_clear_confirmation(ctx: &egui::Context, state: &mut AppState) {
    if !state.show_clear_confirmation {
        return;
    }
    egui::Window::new("清空任务列表")
        .collapsible(false)
        .resizable(false)
        .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            ui.label("确定要清空任务列表吗？将移除所有已结束的任务。");
            ui.add_space(12.0);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if danger_button(ui, "清空").clicked() {
                    state.clear_finished_tasks();
                    state.show_clear_confirmation = false;
                }
                if ui.button("取消").clicked() {
                    state.show_clear_confirmation = false;
                }
            });
        });
}

pub fn render_toast(ctx: &egui::Context, state: &mut AppState) {
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
                    // 提示文案可能带换行（如批量粘贴的逐行错误），逐行渲染保证换行可见。
                    for line in message.lines() {
                        ui.label(RichText::new(line).small());
                    }
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
