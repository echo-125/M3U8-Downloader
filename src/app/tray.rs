use std::{
    collections::VecDeque,
    sync::{Arc, Mutex, OnceLock},
};

use eframe::egui;
use tray_icon::{
    menu::{Menu, MenuEvent, MenuItem},
    TrayIcon, TrayIconBuilder,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayAction {
    Show,
    Exit,
}

/// 全局菜单事件队列。muda 的事件处理器是全局单例，`set_event_handler` 重复
/// 调用会 panic，且必须在首次菜单事件之前注册——因此把队列与注册逻辑一起
/// 放进 `OnceLock`：只在第一次 `TrayController::new` 时注册，之后的实例
/// 复用同一队列，重复创建（多窗口、测试）也不会崩溃。
static TRAY_ACTIONS: OnceLock<Arc<Mutex<VecDeque<TrayAction>>>> = OnceLock::new();

pub struct TrayController {
    _tray_icon: TrayIcon,
    _show_item: MenuItem,
    _exit_item: MenuItem,
    actions: Arc<Mutex<VecDeque<TrayAction>>>,
}

impl TrayController {
    /// `ctx` 用来在菜单事件到达时立刻唤醒界面。
    /// 界面空闲时最长 1 秒才重绘一次，只靠每帧轮询会让托盘菜单看起来「点了没反应」。
    /// 若第二次调用本函数，`ctx` 会被忽略（处理器只在首次注册时捕获）。
    pub fn new(ctx: egui::Context) -> Result<Self, String> {
        let show_item = MenuItem::with_id("cat-catch-show", "显示主窗口", true, None);
        let exit_item = MenuItem::with_id("cat-catch-exit", "退出程序", true, None);
        let menu = Menu::new();
        menu.append_items(&[&show_item, &exit_item])
            .map_err(|_| "创建托盘菜单失败".to_string())?;
        let show_id = show_item.id().clone();
        let exit_id = exit_item.id().clone();

        let actions = Arc::clone(TRAY_ACTIONS.get_or_init(|| {
            let actions: Arc<Mutex<VecDeque<TrayAction>>> = Arc::new(Mutex::new(VecDeque::new()));
            // 必须在任何菜单点击发生之前注册：muda 用 OnceCell 保存处理器，
            // 事件一旦先落进 channel，处理器就再也装不上了（此后 receiver 也收不到）。
            MenuEvent::set_event_handler(Some({
                let actions = Arc::clone(&actions);
                move |event: MenuEvent| {
                    let action = if event.id == show_id {
                        TrayAction::Show
                    } else if event.id == exit_id {
                        TrayAction::Exit
                    } else {
                        return;
                    };
                    if let Ok(mut queue) = actions.lock() {
                        queue.push_back(action);
                    }
                    ctx.request_repaint();
                }
            }));
            actions
        }));

        let (rgba, width, height) = icon_rgba();
        let icon = tray_icon::Icon::from_rgba(rgba, width, height)
            .map_err(|_| "创建托盘图标失败".to_string())?;
        let tray_icon = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_tooltip("M3U8下载器")
            .with_icon(icon)
            .build()
            .map_err(|_| "初始化系统托盘失败".to_string())?;
        Ok(Self {
            _tray_icon: tray_icon,
            _show_item: show_item,
            _exit_item: exit_item,
            actions,
        })
    }

    /// 一次性取出全部待处理动作。
    /// 每帧只取一个的话，连续点击会排到后面几帧，延迟叠加成「点了好几次才有反应」。
    pub fn take_actions(&self) -> Vec<TrayAction> {
        let Ok(mut queue) = self.actions.lock() else {
            return Vec::new();
        };
        queue.drain(..).collect()
    }
}

/// 应用图标：优先从内置的 icon.ico 解码（自动选最大帧），失败时退回生成图标。
pub fn icon_rgba() -> (Vec<u8>, u32, u32) {
    let bytes: &[u8] = include_bytes!("../../assets/icon.ico");
    if let Some(icon) = decode_ico(bytes) {
        return icon;
    }
    fallback_icon()
}

fn decode_ico(bytes: &[u8]) -> Option<(Vec<u8>, u32, u32)> {
    use image::ImageDecoder;
    let decoder = image::codecs::ico::IcoDecoder::new(std::io::Cursor::new(bytes)).ok()?;
    let (width, height) = decoder.dimensions();
    let mut rgba = vec![0_u8; (width * height * 4) as usize];
    decoder.read_image(&mut rgba).ok()?;
    Some((rgba, width, height))
}

/// 图标解码失败的兜底：圆形蓝色猫爪占位。
fn fallback_icon() -> (Vec<u8>, u32, u32) {
    let mut rgba = Vec::with_capacity(32 * 32 * 4);
    for y in 0..32 {
        for x in 0..32 {
            let distance = (x as i16 - 16).pow(2) + (y as i16 - 16).pow(2);
            let color = if distance <= 8 * 8 {
                [45, 140, 230, 255]
            } else if distance <= 14 * 14 {
                [226, 240, 253, 255]
            } else {
                [0, 0, 0, 0]
            };
            rgba.extend_from_slice(&color);
        }
    }
    (rgba, 32, 32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_bundled_icon() {
        let (rgba, width, height) = icon_rgba();
        // 不锁定具体尺寸：图标文件可以调整，只要解出有效帧且数据完整即可。
        assert!(width >= 16 && height >= 16, "应解码出有效的图标帧");
        assert_eq!(rgba.len() as u32, width * height * 4);
    }
}
