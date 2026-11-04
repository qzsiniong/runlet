//! Runlet 库:Tauri 2 应用初始化。
//!
//! 只保留**脚本管理、执行**相关能力:脚本目录 CRUD、编辑器、运行(待命窗口 →
//! 逐键输出 → Console 面板)、进度窗口与系统通知。本文件只做编排:
//! 把业务模块通过 `#[tauri::command]` 桥接给前端,通过 Tauri 事件系统推送进度,
//! 通过 Tauri tray API 提供常驻入口。

mod cancel;
mod capture;
mod commands;
mod config;
mod console_panel;
mod deep_link;
mod focus;
mod hotkey;
mod keymap;
mod notify;
mod progress_window;
mod script_runner;
mod scripting;
mod standby;
mod tray;
mod typer;
mod worker;

use config::Config;
use tauri::Manager;

/// 把脚本目录里的类型声明、编辑器配置刷新一遍。
///
/// 覆盖写（不是「已存在就跳过」）：这些文件是这份二进制**对外契约的类型侧表示**，
/// 与运行期不一致就会让编辑器提示一个不存在的 API 表。
/// 用户真正要写的是 `.ts` 脚本本身，那些不会被碰。
///
/// 失败只记日志：类型提示是辅助，不该拦住应用启动。
pub fn refresh_editor_types() {
    match runlet_scripting::declarations::ensure_declarations() {
        Ok(n) => log::debug!(
            "已刷新 {n} 个脚本目录类型文件（{}）",
            runlet_scripting::scripts::scripts_dir().display()
        ),
        Err(e) => log::warn!("刷新脚本目录类型文件失败: {e}"),
    }
}

/// 打开(或聚焦)脚本编辑窗口。
///
/// 应用只有这一个主窗口（见 tauri.conf.json 的 `scripting`）。
/// 入口:托盘「脚本编辑器…」、启动时（非登录项拉起）、以及前端原有的 `open_scripts_window`。
pub fn open_scripting_window(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("scripting") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
        // 显示脚本窗口期间需要在 Dock 里出现(见 `sync_dock_icon`)
        sync_dock_icon(app);
    } else {
        log::warn!("找不到 scripting 窗口(检查 tauri.conf.json)");
    }
}

/// 按**脚本窗口是否可见**同步 Dock 图标(macOS)。
///
/// 背景:本应用是托盘常驻程序,启动时把激活策略设成 `Accessory`,因此平时不占 Dock
/// (见 `setup`)。但脚本编辑器是个会长时间停留、还会被用户丢到别的屏幕的工作窗口,
/// 没有 Dock 图标就无法用 Cmd+Tab 切回来 —— 所以在它可见期间要显示 Dock 图标。
///
/// 用 [`tauri::AppHandle::set_dock_visibility`] 而不是再调一次 `set_activation_policy`:
/// tao 的 `set_dock_visibility` 内部带了防抖(切换太快会让 macOS 留下多个 Dock 图标)。
///
/// 幂等,可以随意重复调用;非 macOS 平台是空实现。
pub fn sync_dock_icon(app: &tauri::AppHandle) {
    #[cfg(target_os = "macos")]
    {
        let visible = app
            .get_webview_window("scripting")
            .and_then(|window| window.is_visible().ok())
            .unwrap_or(false);
        if let Err(e) = app.set_dock_visibility(visible) {
            log::warn!("同步 Dock 图标失败(visible={visible}): {e}");
        }
    }

    #[cfg(not(target_os = "macos"))]
    {
        let _ = app;
    }
}

/// 哪些窗口「可见时需要在 Dock 里出现」。
///
/// 目前只有脚本编辑器:进度/待命是浮动小窗,不该占 Dock。
/// 抽成函数是为了能直接单测(不涉及任何平台 API)。
fn window_wants_dock_icon(label: &str) -> bool {
    label == "scripting"
}

/// 是否由「开机时启动」的登录项拉起。
///
/// 登录项注册时带了 `--autostart` 参数（见 `run`），手动双击启动则没有 ——
/// 用它区分「开机静默常驻」与「用户主动打开」，后者才弹脚本窗口。
fn launched_by_autostart() -> bool {
    std::env::args().any(|arg| arg == "--autostart")
}

// ---------------------------------------------------------------------------
// Tauri 2 应用入口
// ---------------------------------------------------------------------------

pub fn run() {
    let mut builder = tauri::Builder::default();

    // 单实例插件必须**第一个**注册（插件文档要求）。Windows/Linux 上深链接以
    // 「新进程 + argv」的形式进来，由它把参数转发给已运行的实例，`deep-link`
    // 特性再把 URL 重新投递成事件（见 deep_link.rs）。
    #[cfg(desktop)]
    {
        builder = builder.plugin(tauri_plugin_single_instance::init(|app, argv, _cwd| {
            // 登录项重复拉起：已有实例在跑，静默退出即可，不要抢焦点
            if argv.iter().any(|arg| arg == "--autostart") {
                return;
            }
            // 普通二次启动：把脚本窗口拉到前台，符合「再点一次图标」的预期
            open_scripting_window(app);
        }));
    }

    builder
        // 登录项带 `--autostart`：启动时据此判断要不要弹脚本窗口
        .plugin(
            tauri_plugin_autostart::Builder::new()
                .arg("--autostart")
                .build(),
        )
        // 深链接 runlet://（运行期事件 + 冷启动补读，见 setup 里的 deep_link::register）
        .plugin(tauri_plugin_deep_link::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        // 脚本 $.confirm 用系统原生确认框(见 scripting.rs)
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            // macOS: 托盘常驻模式,不占 Dock(Tauri 默认是 Regular 策略)
            #[cfg(target_os = "macos")]
            app.handle()
                .set_activation_policy(tauri::ActivationPolicy::Accessory)?;
            let cfg = Config::load();
            // 首次启动把内置示例脚本写进脚本目录(已存在的文件不覆盖)
            match runlet_scripting::scripts::ensure_seed_scripts() {
                Ok(0) => {}
                Ok(n) => log::info!(
                    "已写入 {n} 个内置示例脚本到 {}",
                    script_runner::dir_display()
                ),
                Err(e) => log::warn!("初始化脚本目录失败: {e}"),
            }
            // 类型声明与编辑器配置:每次启动都覆盖(它们是随二进制走的产物,
            // 必须与运行期一致)。有了它们,用户目录里的 .ts 脚本在编辑器里才有提示。
            refresh_editor_types();

            tray::build(app, &cfg)?;

            // 深链接监听：runlet://open|scripts|edit/<name>|run/<name>
            deep_link::register(app.handle());

            // 启动时为空闲模式:不注册任何全局热键(Esc 不拦截,透传给前台应用)
            hotkey::set_mode(app.handle(), &cfg, hotkey::HotkeyMode::Idle)?;
            app.manage(worker::WorkerState::new(cfg, app.handle().clone()));

            // 启动即进入脚本编辑器；由登录项拉起时保持静默（只留托盘图标），
            // 否则每次开机都会弹一个窗口打断用户。
            if !launched_by_autostart() {
                open_scripting_window(app.handle());
            }
            Ok(())
        })
        .on_menu_event(|app, event| match event.id().as_ref() {
            tray::M_QUIT => app.exit(0),
            tray::M_SCRIPTS => open_scripting_window(app),
            tray::M_AUTOSTART => tray::toggle_autostart(app),
            _ => {}
        })
        .on_window_event(|window, event| {
            // 关闭按钮 → 隐藏到托盘(不退出)
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
                // 脚本窗口藏起来后不再有 Dock 图标;Ctx 就是窗口所属的 AppHandle
                if window_wants_dock_icon(window.label()) {
                    sync_dock_icon(window.app_handle());
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::cancel_task,
            commands::set_standby_paused,
            commands::get_task_status,
            commands::list_scripts,
            commands::read_script,
            commands::write_script,
            commands::delete_script,
            commands::scripts_info,
            commands::list_capabilities,
            commands::start_script,
            commands::open_scripts_window,
            commands::get_script_console,
            commands::clear_script_console,
        ])
        .build(tauri::generate_context!())
        .expect("Tauri 应用构建失败")
        .run(|_app, _event| {});
}

#[cfg(test)]
mod dock_icon_tests {
    use super::window_wants_dock_icon;

    #[test]
    fn only_scripting_window_wants_a_dock_icon() {
        assert!(window_wants_dock_icon("scripting"));
        for label in ["progress", "standby", "script-progress"] {
            assert!(!window_wants_dock_icon(label), "{label} 不该触发 Dock 图标");
        }
    }
}
