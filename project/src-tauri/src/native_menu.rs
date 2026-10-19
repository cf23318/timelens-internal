//! Preserve native menu actions and shortcuts while localizing their labels.
use tauri::{menu::{Menu, MenuItemKind}, AppHandle, Runtime};

fn chinese(text: &str) -> Option<String> {
    let label = match text {
        "File" => "文件", "Edit" => "编辑", "View" => "显示", "Window" => "窗口", "Help" => "帮助",
        "Undo" => "撤销", "Redo" => "重做", "Cut" => "剪切", "Copy" => "复制", "Paste" => "粘贴",
        "Select All" => "全选", "Minimize" => "最小化", "Zoom" | "Maximize" => "缩放窗口",
        "Close" | "Close Window" => "关闭窗口", "Services" => "服务", "Hide Others" => "隐藏其他应用",
        "Show All" => "显示全部", "Enter Full Screen" | "Toggle Full Screen" => "切换全屏",
        "Exit Full Screen" => "退出全屏", "Quit" | "Exit" => "退出 TimeLens",
        _ if text.starts_with("About ") => "关于 TimeLens",
        _ if text.starts_with("Hide ") => "隐藏 TimeLens",
        _ if text.starts_with("Quit ") => "退出 TimeLens",
        _ => return None,
    };
    Some(label.into())
}

fn translate<R: Runtime>(items: Vec<MenuItemKind<R>>) -> tauri::Result<()> {
    for item in items {
        match item {
            MenuItemKind::Submenu(menu) => {
                if let Some(label) = chinese(&menu.text()?) { menu.set_text(label)?; }
                translate(menu.items()?)?;
            }
            MenuItemKind::Predefined(item) => {
                if let Some(label) = chinese(&item.text()?) { item.set_text(label)?; }
            }
            _ => {}
        }
    }
    Ok(())
}

pub fn install<R: Runtime>(app: &AppHandle<R>, language: &str) -> tauri::Result<()> {
    let menu = Menu::default(app)?;
    if language != "en" { translate(menu.items()?)?; }
    app.set_menu(menu)?;
    Ok(())
}
