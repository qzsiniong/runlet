//! 全局热键:用 `tauri-plugin-global-shortcut`。
//! 只承担一件事:任务运行期间注册「中止热键」(默认 Esc),按下即取消任务。
//! 空闲时不注册任何键,避免拦截 Esc 影响其他应用。

use crate::{commands, config::Config, worker};
use tauri::{async_runtime::spawn, AppHandle, Manager};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutEvent, ShortcutState};

type GsResult<T> = Result<T, tauri_plugin_global_shortcut::Error>;

/// 热键注册模式。
pub enum HotkeyMode {
    /// 空闲:不注册任何热键。
    Idle,
    /// 忙时:注册中止键(按下即取消任务)。
    Busy,
}

/// 按模式重建全部热键。内部先 unregister_all,可安全重复调用。
/// 注册失败返回错误,由调用方决定是否提示(不应阻断任务生命周期)。
pub fn set_mode(app: &AppHandle, cfg: &Config, mode: HotkeyMode) -> GsResult<()> {
    let gs = app.global_shortcut();
    gs.unregister_all()?;
    match mode {
        HotkeyMode::Idle => {}
        HotkeyMode::Busy => {
            gs.on_shortcut(cfg.stop_hotkey.as_str(), on_cancel)?;
        }
    }
    Ok(())
}

/// 忙时:中止热键 → 取消任务。
fn on_cancel(app: &AppHandle, _sc: &Shortcut, event: ShortcutEvent) {
    if event.state() == ShortcutState::Pressed {
        let app_clone = app.clone();
        spawn(async move {
            let state = app_clone.state::<worker::WorkerState>();
            let _ = commands::cancel_task(app_clone.clone(), state).await;
        });
    }
}
