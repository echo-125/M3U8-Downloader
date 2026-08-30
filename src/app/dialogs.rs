//! 浮层界面：设置窗口、任务编辑窗口、两个确认弹窗与 Toast。

use eframe::egui::{
    self, Align, Align2, Color32, ComboBox, Layout, Order, RichText, ViewportCommand,
};

use super::{
    state::AppState,
    theme,
    widgets::{
        danger_button, input, input_multiline, number_input, path_dialog_string, primary_button,
        right_label, section, settings_row, EDIT_CONTENT_WIDTH, SETTINGS_CONTENT_WIDTH,
        SETTINGS_LABEL_WIDTH, SETTINGS_SCROLL_MAX_HEIGHT,
    },
};
use crate::config::ProxyScheme;
use crate::core::events::TaskStatus;

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
            // 保存成功，直接关闭：保存后快照已刷新，此时不算脏。
            open = false;
        } else if mask_clicked || !open {
            // 用户请求关闭（点遮罩或右上角）。有未保存的修改就先撤销这一帧的关闭，
            // 交给确认弹窗处理——静默丢弃的话用户会以为改动已经生效。
            if state.is_settings_dirty() {
                open = true;
                state.show_discard_settings_confirmation = true;
            } else {
                open = false;
            }
        }
    }

    if !open {
        // 干净退出：内存里的 settings 就是用户已保存的值，丢弃编辑前的快照即可。
        state.settings_before_edit = None;
    }
    state.settings_open = open;
}

fn render_general_section(ui: &mut egui::Ui, state: &mut AppState) {
    section(ui, "常规", |ui| {
        settings_row(ui, "默认下载路径", |ui| {
            // 预留「选择」「打开」两个按钮的宽度。
            ui.add(
                input(&mut state.settings.download_path)
                    .desired_width(ui.available_width() - 130.0),
            );
            if ui.button("选择").clicked() {
                if let Some(path) = rfd::FileDialog::new().pick_folder() {
                    match path_dialog_string(&path) {
                        Some(text) => state.settings.download_path = text,
                        None => state.notify_error("所选路径包含无法识别的字符，未能应用"),
                    }
                }
            }
            // 想看看当前默认下载到哪，不用去资源管理器翻。
            if ui.button("打开").clicked() {
                state.open_download_directory();
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
        });
        settings_row(ui, "检测状态", |ui| {
            let dark_mode = ui.ctx().style().visuals.dark_mode;
            match &state.ffmpeg_status {
                Some(info) => ui
                    .label(
                        RichText::new(format!("ffmpeg {}", info.version))
                            .color(theme::success_color(dark_mode)),
                    )
                    .on_hover_text(format!("ffmpeg：{}", info.path)),
                None => ui.label(RichText::new("未安装").color(theme::warning_color(dark_mode))),
            };
        });
        // 路径行是可编辑输入框：关掉自动检测后直接在这里填或选路径。
        // 自动检测开启时输入框留空，hint 显示检测到的实际位置。
        settings_row(ui, "ffmpeg 路径", |ui| {
            let detected = state
                .ffmpeg_status
                .as_ref()
                .map(|info| info.path.as_str())
                .unwrap_or("");
            ui.add(
                input(&mut state.settings.ffmpeg.manual_path)
                    .hint_text(detected)
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

/// 删除任务前的二次确认。
///
/// 删除会中断进行中的下载、清掉任务的临时分片目录且不可恢复；
/// 行右键是轻动作，误触代价过高，所以不能点了就直接下发命令。
pub fn render_delete_confirmation(ctx: &egui::Context, state: &mut AppState) {
    if !state.show_delete_confirmation || state.pending_delete_ids.is_empty() {
        return;
    }
    let total = state.pending_delete_ids.len();
    let active = state.pending_delete_active_count();
    egui::Window::new("删除任务")
        .collapsible(false)
        .resizable(false)
        .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            ui.label(format!("确定要删除这 {total} 个任务吗？"));
            if active > 0 {
                let dark_mode = ctx.style().visuals.dark_mode;
                ui.label(
                    RichText::new(format!(
                        "其中 {active} 个正在进行，会中断下载并删除已下载的分片"
                    ))
                    .color(theme::status_color(dark_mode, TaskStatus::Failed)),
                );
            }
            ui.label(
                RichText::new("已完成的成品文件不会被删除，但任务本身无法恢复")
                    .small()
                    .weak(),
            );
            ui.add_space(12.0);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if danger_button(ui, "删除").clicked() {
                    state.confirm_delete_pending();
                }
                if ui.button("取消").clicked() {
                    state.cancel_delete_pending();
                }
            });
        });
}

/// 关闭设置窗口时的确认：未保存的修改会被丢弃，必须让用户知道并给「继续编辑」的出口。
pub fn render_discard_settings_confirmation(ctx: &egui::Context, state: &mut AppState) {
    if !state.show_discard_settings_confirmation {
        return;
    }
    egui::Window::new("放弃修改")
        .collapsible(false)
        .resizable(false)
        .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
        // 与设置窗口同层，靠绘制顺序（本函数在设置窗口之后调用）盖在遮罩之上。
        .order(Order::Foreground)
        .show(ctx, |ui| {
            ui.label("设置已修改但尚未保存，关闭后会丢失这些修改。");
            ui.add_space(12.0);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if danger_button(ui, "放弃修改").clicked() {
                    state.discard_settings_edit();
                }
                if ui.button("继续编辑").clicked() {
                    state.show_discard_settings_confirmation = false;
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
    // 标题与边框用主题化的状态色而不是固定红绿：固定红 (214,69,65)/绿 (46,160,96)
    // 在暗色底上只有约 2.9–3.8:1、亮色下绿字约 3.3:1，都不达 WCAG 4.5:1。
    let accent = if error {
        theme::status_color(dark, TaskStatus::Failed)
    } else {
        theme::status_color(dark, TaskStatus::Completed)
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
        // 落在任务列表右下角。不能放右上：那里是「设置」和主题切换按钮，
        // Toast 弹出的几秒里这两个按钮就点不到了。
        // 底部偏移要避开状态栏与折叠状态的日志面板（合计约 70px）再留些间距。
        .anchor(Align2::RIGHT_BOTTOM, [-16.0, -90.0])
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
            // 只认右上角的关闭按钮。整块都可点的话，想选中里面的错误链接
            // 复制时一点就关，反而碍事。
            if frame.inner {
                close_clicked = true;
            }
        });
    if close_clicked {
        state.toast = None;
    }
}
