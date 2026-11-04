//! Tauri 命令桥接层:把核心业务模块的同步调用包装成 `#[tauri::command]`,
//! 供前端 `invoke`。命令内部通过 `WorkerState` 派发任务,通过事件推送进度。

use crate::script_runner;
use crate::worker::{Status, TaskKind, WorkerState};
use runlet_scripting::scripts::ScriptMeta;
use tauri::{AppHandle, State};

/// 中止当前任务(如有)。
#[tauri::command]
pub async fn cancel_task(app: AppHandle, state: State<'_, WorkerState>) -> Result<(), String> {
    state.cancel(&app).await;
    Ok(())
}

/// 暂停/恢复待命窗口的失焦检测（用户需要多次切换焦点时用，见 [`crate::standby::StandbyControl`]）。
///
/// 返回**生效后的「是否暂停」**（前端直接拿它当按钮状态）；没有进行中的待命时返回 `false`
/// （这一轮已经结束，不该把按钮停在「已暂停」上）。每轮待命的开始都会复位成「未暂停」。
#[tauri::command]
pub fn set_standby_paused(state: State<'_, WorkerState>, paused: bool) -> Result<bool, String> {
    Ok(state.standby_ctl.set_paused(paused))
}

/// 查询当前任务状态快照。
#[tauri::command]
pub async fn get_task_status(state: State<'_, WorkerState>) -> Result<Status, String> {
    Ok(state.status())
}

// ---------------------------------------------------------------------------
// 脚本（JS/TS）：目录 CRUD + 运行
// ---------------------------------------------------------------------------

/// 列出脚本目录里的脚本（按文件名排序）。
#[tauri::command]
pub async fn list_scripts() -> Result<Vec<ScriptMeta>, String> {
    script_runner::list()
}

/// 读取脚本源码。
#[tauri::command]
pub async fn read_script(name: String) -> Result<String, String> {
    script_runner::read(&name)
}

/// 保存脚本源码。
#[tauri::command]
pub async fn write_script(name: String, source: String) -> Result<(), String> {
    script_runner::write(&name, &source)
}

/// 删除脚本。
#[tauri::command]
pub async fn delete_script(name: String) -> Result<(), String> {
    script_runner::delete(&name)
}

/// 脚本目录路径 + 内置示例状态（供前端提示「文件在哪」）。
#[tauri::command]
pub async fn scripts_info() -> Result<serde_json::Value, String> {
    let seeded = script_runner::seed()?;
    Ok(serde_json::json!({
        "dir": script_runner::dir_display(),
        "seeded": seeded,
    }))
}

/// 当前可用的脚本能力清单 + 命名空间名字（CodeMirror 补全的数据源）。
#[tauri::command]
pub async fn list_capabilities() -> Result<runlet_scripting::CapabilityList, String> {
    Ok(crate::scripting::capability_list())
}

/// 运行一个脚本：读文件 → 必要时 TS 转译 → 交给 Worker 执行（待命窗口 → 逐键输出）。
#[tauri::command]
pub async fn start_script(
    app: AppHandle,
    state: State<'_, WorkerState>,
    name: String,
) -> Result<(), String> {
    // 先做「读 + 转译」：语法错误要立刻反馈，不要等用户点完待命窗口才报错
    let source = script_runner::load(&name)?;

    state.set_pending_script(crate::worker::ScriptRequest {
        name: name.clone(),
        source,
    });

    // 启动失败（例如已有任务在跑）时要把请求清掉：否则下一次启动脚本任务会
    // 取到这次残留的内容，跑错文件。
    if let Err(e) = state.start(TaskKind::Script, app).await {
        state.clear_pending_script();
        return Err(e);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// 脚本窗口 / Console 面板
// ---------------------------------------------------------------------------

/// 打开(或聚焦)脚本编辑窗口。
#[tauri::command]
pub async fn open_scripts_window(app: AppHandle) -> Result<(), String> {
    crate::open_scripting_window(&app);
    Ok(())
}

/// 取脚本 Console 面板的全部输出(面板挂载或窗口重开时用)。
#[tauri::command]
pub async fn get_script_console(
    state: State<'_, WorkerState>,
) -> Result<Vec<crate::console_panel::ConsoleLine>, String> {
    Ok(state.console.snapshot())
}

/// 清空脚本 Console 面板。
#[tauri::command]
pub async fn clear_script_console(state: State<'_, WorkerState>) -> Result<(), String> {
    state.console.clear();
    Ok(())
}
