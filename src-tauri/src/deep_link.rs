//! 深链接 `runlet://`：把外部链接翻译成应用动作。
//!
//! # 支持的链接
//!
//! | 链接 | 动作 |
//! |---|---|
//! | `runlet://open` | 唤起（或聚焦）脚本窗口 |
//! | `runlet://scripts` | 唤起窗口并聚焦脚本列表 |
//! | `runlet://edit/<脚本名>` | 唤起窗口并选中该脚本（不运行） |
//! | `runlet://run/<脚本名>` | **弹系统确认框**，确认后运行脚本 |
//!
//! # 为什么运行一定要确认
//!
//! 链接可能来自任意网页（`<a href="runlet://run/xxx">`）。脚本能读文件、能往当前
//! 焦点窗口逐键打字，等于「网页 → 本机键盘」的通道；不确认就等于给了任意网站一个
//! 键盘后门。所以 `run` 一律先弹原生确认框，把脚本名与来源 URL 都摆给用户看。
//!
//! 其余三种动作只影响界面（显示窗口、选中文件），不产生副作用，直接执行。
//!
//! # 冷启动与单实例
//!
//! * Windows/Linux：链接以「新进程 + argv」的形式进来，由 `single-instance` 插件
//!   转发给已有实例，`deep-link` 特性会把 URL 重新投递成事件；
//! * macOS：只有打包并安装到 `/Applications` 后系统才会把 `runlet://` 交给应用，
//!   `tauri dev` 下测不到；
//! * 应用是被链接拉起的时候，`on_open_url` 可能赶不上，`lib.rs` 的 setup 里会再读一次
//!   `get_current()` —— 两条路可能都投递同一个 URL，[`handle`] 里做了 3 秒去重。

use std::sync::Mutex;
use std::time::{Duration, Instant};

use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};

use crate::worker::{ScriptRequest, TaskKind, WorkerState};

/// 前端监听的事件名（`src/pages/ScriptsWindow.vue` 里同步）。
pub const NAVIGATE_EVENT: &str = "deep-link-navigate";

/// 同一条链接在 3 秒内被投递两次时只处理一次。
///
/// 冷启动时 `on_open_url` 与 `get_current` 可能都会拿到同一个 URL，
/// 不去重就会弹两个确认框。
const DEDUPE_WINDOW: Duration = Duration::from_secs(3);

static LAST_HANDLED: Mutex<Option<(String, Instant)>> = Mutex::new(None);

/// 一条链接解析出来的动作。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// `runlet://open`
    Open,
    /// `runlet://scripts`
    Scripts,
    /// `runlet://edit/<name>`
    Edit(String),
    /// `runlet://run/<name>`
    Run(String),
    /// 不认识的链接（含非法脚本名）。
    Unknown,
}

/// 解析 `runlet://<动作>[/<脚本名>]`。
///
/// 宽松接受几种常见写法：`runlet://run/x`、`runlet:run/x`、`runlet:///run/x`；
/// 查询串与 fragment 直接丢弃（`?from=web` 之类不该影响路由）。
pub fn parse(raw: &str) -> Action {
    let Some(rest) = raw
        .strip_prefix("runlet://")
        .or_else(|| raw.strip_prefix("runlet:"))
    else {
        return Action::Unknown;
    };

    let rest = rest.trim_start_matches('/');
    // 查询串/fragment 只对最后一段有意义，先整体截断再分段
    let rest = rest.split(['?', '#']).next().unwrap_or("");
    let mut parts = rest.splitn(2, '/');
    let verb = parts.next().unwrap_or("").to_ascii_lowercase();
    let arg = parts.next().unwrap_or("");
    let name = percent_decode(arg);

    match verb.as_str() {
        "" | "open" => Action::Open,
        "scripts" => Action::Scripts,
        "edit" if valid_script_name(&name) => Action::Edit(name),
        "run" if valid_script_name(&name) => Action::Run(name),
        _ => Action::Unknown,
    }
}

/// 脚本名白名单：与脚本目录共用同一套规则（拒 `..`、路径分隔符、非法扩展名）。
///
/// 这里的名字来自外部 URL，绝不能直接拼路径。
fn valid_script_name(name: &str) -> bool {
    runlet_scripting::scripts::is_valid_script_name(name)
}

/// 在 setup 阶段挂上监听：运行期事件 + 冷启动链接。
///
/// 必须在 `single-instance`（第一个插件）与 `deep-link` 插件都注册完之后调用。
pub fn register(app: &AppHandle) {
    use tauri_plugin_deep_link::DeepLinkExt;

    let app_handle = app.clone();
    app.deep_link().on_open_url(move |event| {
        handle(&app_handle, &event.urls());
    });

    // Linux/Windows 的 scheme 只在安装时写入注册表/desktop 文件；`tauri dev` 里
    // 测不到，用 register_all 把「当前这个可执行文件」临时注册上。
    // macOS 只能靠打包安装，运行时注册无效。
    #[cfg(any(target_os = "linux", all(debug_assertions, windows)))]
    if let Err(e) = app.deep_link().register_all() {
        log::warn!("注册 runlet:// scheme 失败（dev 下不影响其它功能）: {e}");
    }

    // 冷启动：应用本身就是被链接拉起的，此时 on_open_url 可能赶不上，补读一次
    match app.deep_link().get_current() {
        Ok(Some(urls)) => handle(app, &urls),
        Ok(None) => {}
        Err(e) => log::warn!("读取启动时的深链接失败: {e}"),
    }
}

/// 处理一批 URL（插件一次可能给多个，macOS API 的形状）。
///
/// 接受 `&[Url]`（插件给的）与 `&[String]`（测试/日志用）两种形态。
pub fn handle<S: AsRef<str>>(app: &AppHandle, urls: &[S]) {
    for raw in urls {
        let raw = raw.as_ref();
        if is_duplicate(raw) {
            log::debug!("深链接重复投递，已跳过: {raw}");
            continue;
        }
        match parse(raw) {
            Action::Open => {
                log::info!("深链接：唤起脚本窗口");
                crate::open_scripting_window(app);
            }
            Action::Scripts => {
                log::info!("深链接：聚焦脚本列表");
                crate::open_scripting_window(app);
                emit_navigate(app, "scripts", None);
            }
            Action::Edit(name) => {
                log::info!("深链接：打开脚本到编辑器 {name}");
                crate::open_scripting_window(app);
                emit_navigate(app, "edit", Some(&name));
            }
            Action::Run(name) => {
                log::info!("深链接：请求运行脚本 {name}（等待确认）");
                crate::open_scripting_window(app);
                confirm_run(app.clone(), name, raw.to_string());
            }
            Action::Unknown => log::warn!("忽略无法识别的深链接: {raw}"),
        }
    }
}

fn is_duplicate(raw: &str) -> bool {
    // 锁中毒也继续用：去重失败最多多弹一次确认，不该让链接失效
    let mut slot = match LAST_HANDLED.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    if let Some((prev, at)) = slot.as_ref() {
        if prev == raw && at.elapsed() < DEDUPE_WINDOW {
            return true;
        }
    }
    *slot = Some((raw.to_string(), Instant::now()));
    false
}

/// 把「选中脚本 / 聚焦列表」交给前端（窗口可能还没挂载完，事件会丢，这没关系：
/// 这些动作只是导航，重发一次链接即可）。
fn emit_navigate(app: &AppHandle, action: &str, name: Option<&str>) {
    let payload = serde_json::json!({ "action": action, "name": name });
    if let Err(e) = app.emit_to("scripting", NAVIGATE_EVENT, payload) {
        log::warn!("投递导航事件失败: {e}");
    }
}

/// 通过链接运行脚本前的确认。确认框是系统原生的，不依赖 webview 是否已经就绪。
fn confirm_run(app: AppHandle, name: String, url: String) {
    app.dialog()
        .message(format!(
            "脚本：{name}\n来源链接：{url}\n\n运行后会向当前焦点窗口逐键输出。"
        ))
        .title("通过链接运行脚本？")
        .kind(MessageDialogKind::Warning)
        .buttons(MessageDialogButtons::OkCancelCustom(
            "运行".to_string(),
            "取消".to_string(),
        ))
        .show(move |confirmed| {
            if !confirmed {
                log::info!("用户取消了深链接运行: {name}");
                return;
            }
            if let Err(e) = start_script(&app, &name) {
                log::warn!("深链接运行失败: {e}");
                app.dialog()
                    .message(e)
                    .title("无法运行脚本")
                    .kind(MessageDialogKind::Error)
                    .show(|_| {});
            }
        });
}

/// 与 `commands::start_script` 相同的两步：载入源码（顺带转译）→ 交给 Worker。
fn start_script(app: &AppHandle, name: &str) -> Result<(), String> {
    let source = crate::script_runner::load(name)?;
    let state = app.state::<WorkerState>();
    state.set_pending_script(ScriptRequest {
        name: name.to_string(),
        source,
    });

    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let state = app.state::<WorkerState>();
        if let Err(e) = state.start(TaskKind::Script, app.clone()).await {
            state.clear_pending_script();
            log::warn!("深链接启动脚本任务失败: {e}");
        }
    });
    Ok(())
}

/// `%E4%B8%AD` → `中`。脚本名可能是中文或带空格，链接里会被转义。
fn percent_decode(input: &str) -> String {
    fn hex(byte: u8) -> Option<u8> {
        match byte {
            b'0'..=b'9' => Some(byte - b'0'),
            b'a'..=b'f' => Some(byte - b'a' + 10),
            b'A'..=b'F' => Some(byte - b'A' + 10),
            _ => None,
        }
    }

    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let high = bytes.get(i + 1).copied().and_then(hex);
            let low = bytes.get(i + 2).copied().and_then(hex);
            if let (Some(high), Some(low)) = (high, low) {
                out.push(high * 16 + low);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    // 非法 UTF-8 用替换字符兜底：宁可路由到「脚本不存在」，也不要 panic
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_supported_routes() {
        assert_eq!(parse("runlet://open"), Action::Open);
        assert_eq!(parse("runlet://scripts"), Action::Scripts);
        assert_eq!(
            parse("runlet://edit/01-quick-start.js"),
            Action::Edit("01-quick-start.js".into())
        );
        assert_eq!(
            parse("runlet://run/02-ts-demo.ts"),
            Action::Run("02-ts-demo.ts".into())
        );
    }

    #[test]
    fn tolerates_loose_spellings() {
        // 三斜杠、单冒号、大小写、查询串、fragment 都不该影响路由
        assert_eq!(parse("runlet:///open"), Action::Open);
        assert_eq!(parse("runlet:scripts"), Action::Scripts);
        assert_eq!(
            parse("runlet://RUN/abc.js?from=web"),
            Action::Run("abc.js".into())
        );
        assert_eq!(parse("runlet://edit/a.js#L10"), Action::Edit("a.js".into()));
        // 只有 scheme、没有动作 = 唤起窗口
        assert_eq!(parse("runlet://"), Action::Open);
    }

    #[test]
    fn decodes_percent_escapes() {
        assert_eq!(
            parse("runlet://edit/%E4%B8%AD%E6%96%87.js"),
            Action::Edit("中文.js".into())
        );
        assert_eq!(
            parse("runlet://run/my%20script.js"),
            Action::Run("my script.js".into())
        );
    }

    #[test]
    fn rejects_unknown_or_unsafe_links() {
        for raw in [
            "runlet://delete/x.js",       // 未知动作
            "runlet://run/../etc/passwd", // 路径穿越
            "runlet://run/..%2Fescape.js",
            "runlet://run/",       // 缺名字
            "runlet://edit/",      // 缺名字
            "runlet://run/a.exe",  // 扩展名不在白名单
            "https://example.com", // 不是我们的 scheme
            "other://run/a.js",
        ] {
            assert_eq!(parse(raw), Action::Unknown, "{raw} 应当被拒绝");
        }
    }
}
