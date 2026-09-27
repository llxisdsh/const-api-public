use tauri::menu::{AboutMetadata, Menu, PredefinedMenuItem, Submenu};
use tauri::{AppHandle, Runtime};

use crate::native_i18n::text;

pub(crate) fn install<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<()> {
    let menu = build(app)?;
    app.set_menu(menu)?;
    Ok(())
}

pub(crate) fn build<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<Menu<R>> {
    let package = app.package_info();
    let config = app.config();
    let about_metadata = AboutMetadata {
        name: Some(package.name.clone()),
        version: Some(package.version.to_string()),
        copyright: config.bundle.copyright.clone(),
        authors: config
            .bundle
            .publisher
            .clone()
            .map(|publisher| vec![publisher]),
        ..Default::default()
    };

    let application_menu = Submenu::with_items(
        app,
        package.name.clone(),
        true,
        &[
            &PredefinedMenuItem::about(
                app,
                Some(text("关于 CONST API", "About CONST API")),
                Some(about_metadata),
            )?,
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::services(app, Some(text("服务", "Services")))?,
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::hide(app, Some(text("隐藏 CONST API", "Hide CONST API")))?,
            &PredefinedMenuItem::hide_others(app, Some(text("隐藏其他", "Hide Others")))?,
            &PredefinedMenuItem::show_all(app, Some(text("显示全部", "Show All")))?,
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::quit(app, Some(text("退出 CONST API", "Quit CONST API")))?,
        ],
    )?;

    let file_menu = Submenu::with_items(
        app,
        text("文件", "File"),
        true,
        &[&PredefinedMenuItem::close_window(
            app,
            Some(text("关闭窗口", "Close Window")),
        )?],
    )?;

    // Keep these as predefined items so AppKit routes them through the active
    // WKWebView/text responder. This preserves native editing behavior rather
    // than reimplementing copy, paste, selection, and undo in JavaScript.
    let edit_menu = Submenu::with_items(
        app,
        text("编辑", "Edit"),
        true,
        &[
            &PredefinedMenuItem::undo(app, Some(text("撤销", "Undo")))?,
            &PredefinedMenuItem::redo(app, Some(text("重做", "Redo")))?,
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::cut(app, Some(text("剪切", "Cut")))?,
            &PredefinedMenuItem::copy(app, Some(text("复制", "Copy")))?,
            &PredefinedMenuItem::paste(app, Some(text("粘贴", "Paste")))?,
            &PredefinedMenuItem::select_all(app, Some(text("全选", "Select All")))?,
        ],
    )?;

    let view_menu = Submenu::with_items(
        app,
        text("显示", "View"),
        true,
        &[&PredefinedMenuItem::fullscreen(
            app,
            Some(text("进入全屏幕", "Enter Full Screen")),
        )?],
    )?;

    let window_menu = Submenu::with_items(
        app,
        text("窗口", "Window"),
        true,
        &[
            &PredefinedMenuItem::minimize(app, Some(text("最小化", "Minimize")))?,
            &PredefinedMenuItem::maximize(app, Some(text("缩放", "Zoom")))?,
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::close_window(app, Some(text("关闭窗口", "Close Window")))?,
        ],
    )?;

    let help_menu = Submenu::with_items(app, text("帮助", "Help"), true, &[])?;

    Menu::with_items(
        app,
        &[
            &application_menu,
            &file_menu,
            &edit_menu,
            &view_menu,
            &window_menu,
            &help_menu,
        ],
    )
}
