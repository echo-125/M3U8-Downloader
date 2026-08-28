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
        let icon = tray_icon::Icon::from_rgba(icon_rgba(), 32, 32)
            .map_err(|_| "创建托盘图标失败".to_string())?;
        let tray_icon = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_tooltip("Cat Catch Assistant")
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

pub fn icon_rgba() -> Vec<u8> {
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
    rgba
}
