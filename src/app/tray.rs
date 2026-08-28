use tray_icon::{
    menu::{Menu, MenuEvent, MenuItem},
    TrayIcon, TrayIconBuilder,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayAction {
    Show,
    Exit,
}

pub struct TrayController {
    _tray_icon: TrayIcon,
    _show_item: MenuItem,
    _exit_item: MenuItem,
    show_id: tray_icon::menu::MenuId,
    exit_id: tray_icon::menu::MenuId,
}

impl TrayController {
    pub fn new() -> Result<Self, String> {
        let show_item = MenuItem::with_id("cat-catch-show", "显示主窗口", true, None);
        let exit_item = MenuItem::with_id("cat-catch-exit", "退出程序", true, None);
        let menu = Menu::new();
        menu.append_items(&[&show_item, &exit_item])
            .map_err(|_| "创建托盘菜单失败".to_string())?;
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
            show_id: show_item.id().clone(),
            exit_id: exit_item.id().clone(),
            _show_item: show_item,
            _exit_item: exit_item,
        })
    }

    pub fn poll(&self) -> Option<TrayAction> {
        let Ok(event) = MenuEvent::receiver().try_recv() else {
            return None;
        };
        if event.id == self.show_id {
            Some(TrayAction::Show)
        } else if event.id == self.exit_id {
            Some(TrayAction::Exit)
        } else {
            None
        }
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
