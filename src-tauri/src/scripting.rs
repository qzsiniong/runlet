//! 把脚本引擎接到 Tauri 上：GUI 侧的 `ScriptHost` 与 `ConsoleHook` 实现。
//!
//! 分工：
//!
//! * [`runlet_scripting::ScriptHost`] 是「业务宿主」接口（输出 / 询问 / 文件授权）；
//! * [`script_engine::ConsoleHook`] 是引擎的 `console.*` 落点 —— 本文件同时实现两者：
//!   `console.*` 进脚本窗口的 Console 面板，宿主交互走系统弹框与 [`Typer`]；
//! * `$.type_str` 走既有的 [`Typer`]（逐键打进当前焦点窗口），`$.confirm` / `$.pick_path`
//!   与文件授权用**系统原生**弹框（`tauri-plugin-dialog`），进度通过 `worker-progress` 事件推送；
//! * 「先点目标窗口再输出」由 [`crate::standby::StandbyGate`] 保证：**惰性**地在首次
//!   `type_str` 之前弹待命窗口，纯计算脚本不会被打扰。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use governor::{DefaultDirectRateLimiter, Quota, RateLimiter};
use runlet_scripting::{
    ConfirmChoice, FileDecision, HostError, PickKind, ProgressCmd, ScriptHost, TypeProgress,
};
use script_engine::ConsoleHook;
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_dialog::{
    DialogExt, MessageDialogButtons, MessageDialogKind, MessageDialogResult,
};

use crate::cancel::CancellationToken as WorkerCancel;
use crate::console_panel::ConsoleBuffer;
use crate::standby::StandbyGate;
use crate::typer::Typer;
use crate::worker::{Progress, TaskKind, WorkerState};

/// 进度节流间隔（与 worker 的进度事件保持一致：1 秒）。
const PROGRESS_THROTTLE_MS: u64 = 1000;

/// 脚本进度条（`$.progress*`）位置类命令的推送节流间隔。
const SCRIPT_BAR_THROTTLE_MS: u64 = 100;

/// 进度条代数的**进程级**计数器。
///
/// 不能放在 `ScriptBar`/宿主实例里自增：每个脚本任务都新建宿主，任务内从 0→1 计数
/// 会让前端在多次任务间看到相同的 generation、误以为是同一条而漏掉计时重置。
/// 全局递增保证每次 Start（无论哪个任务）都带来一个前端没见过的新代数。
static SCRIPT_BAR_GENERATION: AtomicU64 = AtomicU64::new(0);

/// 脚本进度窗口 label（`$.progress*` 专用，独立于 worker 的 `progress` 窗口）。
const SCRIPT_PROGRESS_WINDOW: &str = "script-progress";

/// 进度片段的最大长度（字符）。
const SNIPPET_LIMIT: usize = 60;

/// 待命超时后的收尾动作（可被多次触发，因此用 `Fn` + `Arc` 而不是 `FnOnce`）。
pub type CancelHook = Arc<dyn Fn() + Send + Sync>;

/// 脚本窗口 label（系统确认框的父窗口；隐藏时临时显形）。
const SCRIPTING_WINDOW: &str = "scripting";

/// 普通确认框标题。
const CONFIRM_TITLE: &str = "Runlet 脚本确认";

/// 文件授权框标题。
const FILE_TITLE: &str = "Runlet 脚本要修改文件";

/// 文件授权框的三个按钮文案（结果按文案回传，所以三处必须一致）。
const FILE_ALLOW: &str = "允许一次";
const FILE_ALLOW_DIR: &str = "本次运行内该目录都允许";
const FILE_DENY: &str = "拒绝";

/// 选择框的默认标题（脚本没给 `prompt` 时）。
const PICK_FILE_TITLE: &str = "Runlet 脚本：请选择文件";
const PICK_DIR_TITLE: &str = "Runlet 脚本：请选择文件夹";

/// `script-progress` 事件载荷（独立于 `worker-progress`，脚本手动进度条专用）。
#[derive(Clone, serde::Serialize)]
struct ScriptProgressEvent {
    /// `running` | `finished` | `abandoned`。
    state: &'static str,
    /// 第几条进度条（每次 Start 自增）：前端据此重置计时，不用猜「是不是新条」。
    generation: u64,
    pos: u64,
    /// `None` = 不定长模式（前端显示 spinner）。
    total: Option<u64>,
    msg: String,
    ts: u64,
}

/// `$.progress*` 的进度条状态机（纯逻辑，与 Tauri 解耦，方便单测）。
struct ScriptBar {
    pos: u64,
    total: Option<u64>,
    msg: String,
    running: bool,
    abandoned: bool,
    /// 第几条进度条（每次 `Start` 自增）。
    generation: u64,
    last_emit_ms: u64,
}

impl ScriptBar {
    fn new() -> Self {
        Self {
            pos: 0,
            total: None,
            msg: String::new(),
            running: false,
            abandoned: false,
            generation: 0,
            last_emit_ms: 0,
        }
    }

    /// 应用一条命令；返回是否应当立即推送事件
    ///（Start/Msg/终态必发，位置类命令按 [`SCRIPT_BAR_THROTTLE_MS`] 节流、到边必发）。
    fn apply(&mut self, cmd: ProgressCmd, now: u64) -> bool {
        match cmd {
            ProgressCmd::Start { total, msg } => {
                self.pos = 0;
                self.total = total;
                self.msg = msg;
                self.running = true;
                self.abandoned = false;
                // 全局递增（见 SCRIPT_BAR_GENERATION 的注释）：跨任务也要是新代数
                self.generation = SCRIPT_BAR_GENERATION.fetch_add(1, Ordering::Relaxed) + 1;
                self.last_emit_ms = now;
                true
            }
            ProgressCmd::Inc(n) => self.bump(self.pos.saturating_add(n), now),
            ProgressCmd::Set(pos) => self.bump(pos, now),
            ProgressCmd::Total(total) => {
                self.total = if total == 0 { None } else { Some(total) };
                self.emit_if_due(now)
            }
            ProgressCmd::Msg(msg) => {
                self.msg = msg;
                self.last_emit_ms = now;
                true
            }
            ProgressCmd::Finish(msg) => {
                if let Some(m) = msg {
                    self.msg = m;
                }
                // 「完成」直觉上是满条：total 有值时把位置提到头
                if let Some(total) = self.total {
                    self.pos = self.pos.max(total);
                }
                self.running = false;
                self.abandoned = false;
                self.last_emit_ms = now;
                true
            }
            ProgressCmd::Abandon(msg) => {
                if let Some(m) = msg {
                    self.msg = m;
                }
                self.running = false;
                self.abandoned = true;
                self.last_emit_ms = now;
                true
            }
        }
    }

    /// 位置变更：到边（pos ≥ total）必发，否则节流。
    fn bump(&mut self, pos: u64, now: u64) -> bool {
        self.pos = pos;
        let is_edge = self.total.is_some_and(|total| total > 0 && pos >= total);
        if is_edge || now.saturating_sub(self.last_emit_ms) >= SCRIPT_BAR_THROTTLE_MS {
            self.last_emit_ms = now;
            true
        } else {
            false
        }
    }

    fn emit_if_due(&mut self, now: u64) -> bool {
        if now.saturating_sub(self.last_emit_ms) >= SCRIPT_BAR_THROTTLE_MS {
            self.last_emit_ms = now;
            true
        } else {
            false
        }
    }

    fn state_name(&self) -> &'static str {
        if self.running {
            "running"
        } else if self.abandoned {
            "abandoned"
        } else {
            "finished"
        }
    }
}

/// GUI 侧宿主：把脚本的输出、询问、进度接到 Tauri 窗口上。
pub struct TauriScriptHost {
    app: AppHandle,
    /// `Typer` 惰性构造：脚本可能一次都不输出，没必要提前申请键盘权限。
    typer: Mutex<Option<Typer>>,
    /// 与任务共享的取消令牌（Esc / 托盘 / 按钮）。
    cancel: WorkerCancel,
    /// 与任务共享的进度计数（`(已敲入, 总数)`）。
    progress: Arc<std::sync::RwLock<(usize, usize)>>,
    /// 待命门闩：首次输出前确保用户已把焦点切到目标窗口，之后逐字符校验焦点。
    standby: Arc<StandbyGate>,
    /// 脚本窗口 Console 面板的输出缓冲。
    console: Arc<ConsoleBuffer>,
    typed: AtomicUsize,
    /// `emit_progress` 的时间节流：距上次 emit 不足 [`PROGRESS_THROTTLE_MS`] 时跳过
    /// （edge 由调用方自己放行，不经过限流器）。
    progress_limiter: DefaultDirectRateLimiter,
    /// `$.progress*` 系列的进度条状态（每任务同时一条；None = 还没建条）。
    script_bar: Mutex<Option<ScriptBar>>,
}

impl TauriScriptHost {
    /// 为一个脚本任务创建宿主。
    pub fn new(
        app: AppHandle,
        cancel: WorkerCancel,
        standby: Arc<StandbyGate>,
        progress: Arc<std::sync::RwLock<(usize, usize)>>,
        console: Arc<ConsoleBuffer>,
    ) -> Self {
        Self {
            app,
            typer: Mutex::new(None),
            cancel,
            progress,
            standby,
            console,
            typed: AtomicUsize::new(0),
            progress_limiter: RateLimiter::direct(
                Quota::with_period(Duration::from_millis(PROGRESS_THROTTLE_MS))
                    .expect("进度节流周期必须非零"),
            ),
            script_bar: Mutex::new(None),
        }
    }

    /// 整个脚本**累计**已交给键盘的字符数（多次 `type_str` 累加），
    /// 供任务结束汇报使用；进度条显示的是单次调用计数，见 [`Self::type_str`]。
    pub fn typed_chars(&self) -> usize {
        self.typed.load(Ordering::Relaxed)
    }

    /// 输出进度：更新共享计数 + 时间节流地 emit `worker-progress`。
    ///
    /// 节流用 governor GCRA 限流器（每个 [`PROGRESS_THROTTLE_MS`] 放行一次），
    /// 与能力集 `$.type_str` 回调的节流是同一套做法；首字符与末字符是 edge，
    /// 由 [`Self::type_str`] 的调用方语义保证必达，不经过限流器。
    fn emit_progress(&self, started_at: u64, typed: usize, total: usize, snippet: &str) {
        if let Ok(mut slot) = self.progress.write() {
            *slot = (typed, total);
        }

        let is_edge = typed == 1 || typed == total;
        if !is_edge && self.progress_limiter.check().is_err() {
            return;
        }

        let _ = self.app.emit(
            "worker-progress",
            Progress {
                kind: TaskKind::Script,
                got: typed,
                total,
                started_at,
                ts: now_ms(),
                snippet: snippet.to_string(),
            },
        );
    }

    /// `$.progress*` 命令入口：Start 时确保进度窗口可见，状态有变化时推送事件。
    fn handle_progress_cmd(&self, cmd: ProgressCmd) -> Result<(), HostError> {
        let mut guard = self.script_bar.lock().unwrap();
        if matches!(cmd, ProgressCmd::Start { .. }) {
            // 建条/重置前先把窗口亮出来（窗口常驻隐藏，show 只是显形 + 摆位）
            crate::progress_window::show(&self.app, SCRIPT_PROGRESS_WINDOW);
            let bar = guard.get_or_insert_with(ScriptBar::new);
            let now = now_ms();
            bar.apply(cmd, now);
            self.push_bar(bar, now);
            return Ok(());
        }
        let Some(bar) = guard.as_mut() else {
            return Err(HostError::Failed(
                "先调用 $.progress 创建进度条，再使用 $.progress_inc 等命令".to_string(),
            ));
        };
        let now = now_ms();
        if bar.apply(cmd, now) {
            self.push_bar(bar, now);
        }
        Ok(())
    }

    /// 把当前进度条状态推给脚本进度窗口。
    fn push_bar(&self, bar: &ScriptBar, now: u64) {
        let _ = self.app.emit_to(
            SCRIPT_PROGRESS_WINDOW,
            "script-progress",
            ScriptProgressEvent {
                state: bar.state_name(),
                generation: bar.generation,
                pos: bar.pos,
                total: bar.total,
                msg: bar.msg.clone(),
                ts: now,
            },
        );
    }

    /// 任务收尾：进度条还开着（脚本忘写 finish / 被中止 / 出错）就按放弃收口并推送，
    /// 前端收到终态 3 秒后自动隐藏窗口。在 `run_source` 返回后调用，覆盖全部退出路径。
    pub fn finalize_script_bar(&self) {
        let now = now_ms();
        let mut guard = self.script_bar.lock().unwrap();
        let Some(bar) = guard.as_mut() else {
            return;
        };
        if bar.running {
            bar.apply(ProgressCmd::Abandon(None), now);
            self.push_bar(bar, now);
        }
    }

    /// 在「脚本窗口临时显形并聚焦」的状态下执行 `action`，结束后恢复原可见性。
    ///
    /// 系统弹框要应用处于激活状态才可靠地出现在最前：脚本窗口在托盘模式可能是隐藏的，
    /// 先临时显形 + 聚焦，用完再恢复（不改变用户设定的窗口可见性）。确认框与选择框
    /// 都需要这套处理，所以抽在这里。
    ///
    /// 调用方是 Worker 的阻塞线程，正是 rfd blocking API 的适用场景；**绝不能在主线程调用**。
    fn with_scripting_window<T>(
        &self,
        action: impl FnOnce(Option<&tauri::WebviewWindow>) -> T,
    ) -> T {
        let scripting_win = self.app.get_webview_window(SCRIPTING_WINDOW);
        let was_visible = scripting_win
            .as_ref()
            .and_then(|window| window.is_visible().ok())
            .unwrap_or(false);

        if let Some(window) = &scripting_win {
            if !was_visible {
                let _ = window.show();
                let _ = window.set_focus();
                // 脚本窗口临时显形期间也要有 Dock 图标，回答完恢复隐藏时再撤掉
                crate::sync_dock_icon(&self.app);
            }
        }

        let result = action(scripting_win.as_ref());

        if let Some(window) = &scripting_win {
            if !was_visible {
                let _ = window.hide();
                crate::sync_dock_icon(&self.app);
            }
        }

        result
    }

    /// 弹一个**系统原生**确认框并把结果原样返回。
    fn ask_dialog(
        &self,
        title: &str,
        message: &str,
        buttons: MessageDialogButtons,
    ) -> MessageDialogResult {
        self.with_scripting_window(|window| {
            let mut dialog = self
                .app
                .dialog()
                .message(message)
                .title(title)
                .kind(MessageDialogKind::Warning)
                .buttons(buttons);
            if let Some(window) = window {
                dialog = dialog.parent(window);
            }

            dialog.blocking_show_with_result()
        })
    }
}

impl ScriptHost for TauriScriptHost {
    fn type_str(
        &self,
        text: &str,
        delay_ms: u64,
        mut on_progress: Option<&mut TypeProgress<'_>>,
    ) -> Result<(), HostError> {
        let mut guard = self.typer.lock().unwrap();
        if guard.is_none() {
            let cfg = self
                .app
                .state::<WorkerState>()
                .config
                .read()
                .unwrap()
                .clone();
            // 与发送/部署共用同一个 Typer 实现：Return/Tab/Shift 特判、非 ASCII 回退 Unicode 输入
            *guard = Some(Typer::new(&cfg).map_err(HostError::Failed)?);
        }
        let typer = guard.as_mut().expect("刚刚已初始化");

        let total = text.chars().count();
        // 进度条按「单次调用」计数：多次 type_str 各自从 0 开始，got/total 与
        // started_at 才不会互相矛盾（got 累计而 total 单次会导致进度溢出）。
        // self.typed 是跨调用累计值，只供任务结束汇报（typed_chars），不在这里清零。
        let mut typed = 0usize;
        // 0 =「本调用尚未起算」，在首个字符实际发出时才落值：
        // 首次调用的待命窗口确认等待（可能长达数秒）不应计入 elapsed/速度
        let mut started_at = 0u64;
        let mut snippet = String::new();
        let dry_run = !typer.send_real_keys();

        // 注意：不需要重置节流器 —— edge（首/末字符）必达，中间字符由 governor 的
        // GCRA 窗口决定；跨 type_str 调用共享同一个 1 秒窗口是合理的。
        for ch in text.chars() {
            if self.cancel.is_cancelled() {
                return Err(HostError::Cancelled);
            }
            // 每个字符前（节流地）确保焦点还在用户确认过的目标窗口上：
            // 未确认过就先弹待命窗口；焦点变了就自动重新待命，确认后从断点继续。
            // 干跑不注入任何按键，焦点无关紧要 —— 不必为此弹待命窗口。
            if !dry_run {
                self.standby.ensure_ready()?;
            }
            typer.send_char(ch, dry_run).map_err(HostError::Failed)?;

            typed += 1;
            if typed == 1 {
                started_at = now_ms();
            }
            self.typed.fetch_add(1, Ordering::Relaxed);
            if snippet.chars().count() < SNIPPET_LIMIT {
                snippet.push(ch);
            }
            // 换成可见字符，避免片段里的换行把日志/界面撑乱
            let display_snippet: String = snippet
                .chars()
                .map(|c| if c.is_control() { '·' } else { c })
                .collect();
            self.emit_progress(started_at, typed, total, &display_snippet);

            // 脚本注册的进度回调：逐字符通知（`$.type_str` 扩展侧自己节流，
            // 这里调多频也不会打爆 JS）；回调报错（脚本在回调里抛了异常）立即中止输出。
            if let Some(report) = &mut on_progress {
                report(typed as u64, total as u64)?;
            }

            if delay_ms > 0 {
                std::thread::sleep(Duration::from_millis(delay_ms));
            }
        }
        Ok(())
    }

    /// `$.confirm`：弹**系统原生**三按钮对话框（是 / 否 / 取消）。
    ///
    /// 语义：是 → `true`；否 → `false`；取消 → 中止脚本；**不设超时**，一直等用户回答。
    fn confirm(&self, message: &str) -> Result<ConfirmChoice, HostError> {
        match self.ask_dialog(CONFIRM_TITLE, message, MessageDialogButtons::YesNoCancel) {
            MessageDialogResult::Yes => Ok(ConfirmChoice::Yes),
            MessageDialogResult::No => Ok(ConfirmChoice::No),
            // Cancel = 用户要求停掉脚本；Custom 只会在换用 *Custom 按钮时出现，一并按中止处理
            _ => Err(HostError::Cancelled),
        }
    }

    /// `$.pick_path`：系统原生选择框（文件 / 文件夹）。
    ///
    /// 与 `$.confirm` 一样跑在 Worker 的阻塞线程上，用 rfd 的 blocking API。
    /// 用户取消返回 `Ok(None)`（脚本侧是 `null`），不算错误。
    fn pick_path(&self, prompt: &str, kind: PickKind) -> Result<Option<PathBuf>, HostError> {
        let picked = self.with_scripting_window(|window| {
            let title = if prompt.trim().is_empty() {
                match kind {
                    PickKind::File => PICK_FILE_TITLE,
                    PickKind::Dir => PICK_DIR_TITLE,
                }
            } else {
                prompt
            };

            let mut dialog = self.app.dialog().file().set_title(title);
            if let Some(window) = window {
                dialog = dialog.set_parent(window);
            }

            match kind {
                PickKind::File => dialog.blocking_pick_file(),
                PickKind::Dir => dialog.blocking_pick_folder(),
            }
        });

        let Some(file_path) = picked else {
            // 用户点了取消（或关掉了选择框）
            return Ok(None);
        };

        // Windows 上原生选择框可能给出 `\\?\C:\...` 这种前缀路径：`simplified`
        // 去掉前缀，脚本拿到的就是日常写法（再喂给 `$.read` 也不会踩坑）
        file_path
            .simplified()
            .into_path()
            .map(Some)
            .map_err(|err| HostError::Failed(format!("所选路径无法转换成文件路径：{err}")))
    }

    /// `$.request_focus`：显式要求一轮新的焦点确认，`hint` 显示在待命窗口上。
    fn request_focus(&self, hint: &str) -> Result<(), HostError> {
        self.standby.request_focus(hint)
    }

    /// 文件修改授权：三个按钮分别是「允许一次 / 本次运行内该目录都允许 / 拒绝」。
    ///
    /// 「本次运行内都允许」由能力实现记住（`FileState`），不在这里维护状态 ——
    /// 宿主只回答这一次问题。
    fn allow_file_change(
        &self,
        action: &str,
        path: &Path,
        scope_dir: &Path,
    ) -> Result<FileDecision, HostError> {
        let message = format!(
            "脚本请求{action}\n\n文件：{}\n目录：{}\n\n选「{FILE_ALLOW_DIR}」后，本次运行中该目录下的同类操作不再询问。",
            path.display(),
            scope_dir.display()
        );

        let result = self.ask_dialog(
            FILE_TITLE,
            &message,
            MessageDialogButtons::YesNoCancelCustom(
                FILE_ALLOW.to_string(),
                FILE_ALLOW_DIR.to_string(),
                FILE_DENY.to_string(),
            ),
        );

        // 自定义按钮的结果按文案回传，用文案区分三个分支
        Ok(match result {
            MessageDialogResult::Custom(choice) if choice == FILE_ALLOW => FileDecision::Allow,
            MessageDialogResult::Custom(choice) if choice == FILE_ALLOW_DIR => {
                FileDecision::AllowDir
            }
            // 其它（含直接关掉窗口）一律按拒绝处理
            _ => FileDecision::Deny,
        })
    }

    /// `$.scan_qr`：抓一遍所有显示器，返回第一条能解出来的二维码文本。
    ///
    /// **只抓一次**——轮询节奏由脚本用 `await sleep()` 自己排（只有脚本作者知道该等多久）。
    /// 抓取与解码走协议 B 共用的 [`crate::capture`]，两条路径的可见行为一致：
    /// 同一张二维码在哪个通道都得能解出来。
    ///
    /// 扫描开始前先查取消令牌：一次抓屏 + 解码在慢机器上可能上百毫秒，脚本按
    /// `pollMs` 轮询时这个检查让 Esc 的中断延迟保持在一个轮询周期内。
    fn scan_qr(&self) -> Result<Option<String>, HostError> {
        if self.cancel.is_cancelled() {
            return Err(HostError::Cancelled);
        }
        let images = crate::capture::grab_all_gray();

        // 保存为文件（调试用）,只有在调试模式下才保存
        if cfg!(debug_assertions) {
            for (i, image) in images.iter().enumerate() {
                let path = format!("/tmp/runlet_scan_qr_{}.png", i);
                image.save(&path).unwrap();
                println!("Saved capture {} to {}", i, path);
            }
        }

        for image in images {
            let text = crate::capture::decode_qr_text(image);
            if let Some(text) = text {
                return Ok(Some(text));
            }
        }

        Ok(None)
        // Ok(images
        //     .into_iter()
        //     .find_map(crate::capture::decode_qr_text))
    }

    fn progress(&self, cmd: ProgressCmd) -> Result<(), HostError> {
        self.handle_progress_cmd(cmd)
    }

    fn cancelled(&self) -> bool {
        self.cancel.is_cancelled()
    }
}

impl ConsoleHook for TauriScriptHost {
    /// `console.*` 送到脚本窗口的 Console 面板。
    ///
    /// GUI 应用的标准流用户看不到，所以这里**不写** stdout/stderr（仅留 `log::debug!`
    /// 供排查）。
    fn write(&self, level: &str, text: &str) {
        log::debug!("[script:{level}] {text}");
        self.console.push(level, text);
    }
}

/// 当前能力清单 + 命名空间名字（喂给前端的 CodeMirror 补全与文档面板）。
///
/// 名字随清单一并下发，前端就不必再硬编码 `Runlet` / `$`。
pub fn capability_list() -> runlet_scripting::CapabilityList {
    runlet_scripting::capability_list()
}

/// 脚本源码是否可能注入键盘事件（只用于 Console 面板的运行行提示）。
///
/// 待命窗口**不再**依赖这个判定：它只由 [`crate::standby::StandbyGate`] 在首次
/// `$.type_str` 之前触发，所以纯计算脚本永远不会看到待命窗口。
/// 这里保守一点（源码里出现 `.type_str` 就算「会输出」）只是为了让 Console 里
/// `▶ 运行 xxx` 这行能提示「(含键盘输出)」，判错没有任何副作用。
pub fn may_type(source: &str) -> bool {
    source.contains(".type_str")
}

/// 当前时间（epoch 毫秒）。
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn may_type_detects_typical_calls() {
        assert!(may_type(r#"$.type_str("你好", 10)"#));
        assert!(may_type(r#"const $ = Runlet; $.type_str("x")"#));
        assert!(may_type(r#"Runlet.type_str("x")"#));
        // 变量持有函数也命中（保守判定）
        assert!(may_type(r#"const out = Runlet.type_str"#));
    }

    #[test]
    fn may_type_ignores_pure_computation() {
        assert!(!may_type(r#"const b = await $.read("a.bin"); $.md5(b)"#));
        assert!(!may_type(r#"console.log("只算不输出")"#));
        assert!(!may_type(""));
    }

    /// Start 必发、终态必发，且 Finish 会把位置提到头。
    #[test]
    fn script_bar_force_emits_start_and_terminal() {
        let mut bar = ScriptBar::new();
        assert!(
            bar.apply(
                ProgressCmd::Start {
                    total: Some(10),
                    msg: "下载".into()
                },
                1000
            ),
            "Start 必发"
        );
        assert!(bar.generation > 0, "代数来自全局计数器，必为正");
        assert!(bar.apply(ProgressCmd::Finish(Some("完成".into())), 1100));
        assert_eq!(bar.pos, 10, "Finish 提满位置");
        assert_eq!(bar.state_name(), "finished");
    }

    /// 位置类命令 100ms 节流，但到边（pos ≥ total）必发。
    #[test]
    fn script_bar_throttles_positions_until_edge() {
        let mut bar = ScriptBar::new();
        bar.apply(
            ProgressCmd::Start {
                total: Some(10),
                msg: String::new(),
            },
            1000,
        );
        // 100ms 内的普通推进被吞
        assert!(!bar.apply(ProgressCmd::Set(5), 1050));
        // 超过节流窗口照发
        assert!(bar.apply(ProgressCmd::Set(6), 1200));
        // 到边必发（不再等节流窗口）
        assert!(bar.apply(ProgressCmd::Inc(10), 1250));
        assert_eq!(bar.pos, 16);
    }

    /// 不定长模式（total=None）没有「到边」，同样受节流；Msg 必发；Abandon 收口。
    #[test]
    fn script_bar_indefinite_and_msg_and_abandon() {
        let mut bar = ScriptBar::new();
        bar.apply(
            ProgressCmd::Start {
                total: None,
                msg: String::new(),
            },
            1000,
        );
        assert!(!bar.apply(ProgressCmd::Inc(5), 1050), "不定长没有到边概念");
        assert!(
            bar.apply(ProgressCmd::Msg("扫描中".into()), 1051),
            "Msg 必发"
        );
        assert!(bar.apply(ProgressCmd::Abandon(Some("停".into())), 1100));
        assert_eq!(bar.state_name(), "abandoned");
        assert!(!bar.running);
    }

    /// 再次 Start = 重置（pos 归零、代数 +1），前端据此重置计时。
    #[test]
    fn script_bar_start_resets_and_bumps_generation() {
        let mut bar = ScriptBar::new();
        bar.apply(
            ProgressCmd::Start {
                total: Some(10),
                msg: "一".into(),
            },
            1000,
        );
        let gen1 = bar.generation;
        bar.apply(ProgressCmd::Inc(9), 1100);
        bar.apply(
            ProgressCmd::Start {
                total: Some(20),
                msg: "二".into(),
            },
            1200,
        );
        assert_eq!(bar.pos, 0);
        assert_eq!(bar.total, Some(20));
        assert_eq!(bar.msg, "二");
        // 全局计数器与其它并行测试共享，不能假设恰好 +1，只断言单调递增
        assert!(bar.generation > gen1);
        assert!(bar.running);
    }

    /// Total(0) 转不定长；Finish 的 msg 覆盖当前消息。
    #[test]
    fn script_bar_total_zero_becomes_indefinite() {
        let mut bar = ScriptBar::new();
        bar.apply(
            ProgressCmd::Start {
                total: Some(10),
                msg: String::new(),
            },
            1000,
        );
        bar.apply(ProgressCmd::Total(0), 1100);
        assert_eq!(bar.total, None);
        bar.apply(ProgressCmd::Finish(None), 1200);
        assert_eq!(bar.state_name(), "finished");
    }
}
