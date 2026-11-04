//! 宿主接口：脚本与「外部世界」交互的几件事 —— 输出、确认、文件系统授权。
//!
//! 引擎（`script-engine`）不认识键盘、终端或 GUI：它只定义扩展机制与
//! [`ConsoleHook`](script_engine::ConsoleHook)。本模块补齐「业务宿主」这一层：
//!
//! * [`ScriptHost`] 是本 crate 定义的宿主接口，由使用方注入实现
//!   （本应用里是 Tauri 窗口 + Typer 键盘注入）；
//! * `$.type_str` / `$.confirm` / `$.pick_path` / 文件写操作都通过它落地；
//! * `console.*` **不走**这里 —— 它归引擎的 `ConsoleHook`（应用里写 Console 面板），
//!   因此 `ScriptHost` 的实现者通常也要单独实现 `ConsoleHook`。
//!
//! ## 传给 JS 的方式
//!
//! 宿主实现以 `Ctx::store_userdata` 存进上下文（见 [`store_host`]），能力实现用
//! [`host_ctx`] 取回。不往 JS 侧暴露任何对象：脚本永远看不到宿主句柄，只能通过
//! 具体能力间接使用它。存入动作由 `RuntimeOptions::prepare` 钩子完成，见
//! [`crate::runtime_options`]。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use rquickjs::{Ctx, JsLifetime, Result as QjsResult};

/// 确认类能力的回答。
///
/// * [`ConfirmChoice::Yes`] / [`ConfirmChoice::No`] 由调用方映射成布尔；
/// * [`ConfirmChoice::Abort`] 表示用户要求**终止脚本**，调用方应抛 JS 异常。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ConfirmChoice {
    /// 用户确认。
    Yes,
    /// 用户拒绝（也用于超时/无应答的兜底）。
    #[default]
    No,
    /// 用户要求中止脚本。
    Abort,
}

/// `$.pick_path` 要让用户挑什么。
///
/// 系统原生选择框**一次只能挑一种**（文件或文件夹），所以这里是枚举而不是一个
/// 「两个都能选」的开关 —— 后者在系统层面根本做不到，只会把困惑留给脚本作者。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PickKind {
    /// 挑一个文件。
    #[default]
    File,
    /// 挑一个文件夹。
    Dir,
}

/// 脚本请求**修改**文件系统时的授权结果。
///
/// 危险动作（写/删/改名/复制/建目录）每次都要问一次；用户在弹框里可以
/// 顺手把「本目录」在**本次运行内**整体放行 —— 那是一次脚本运行里的便利，
/// 不会跨运行保留（状态在扩展实例里，随运行结束一起丢掉）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileDecision {
    /// 只允许这一次。
    Allow,
    /// 允许这一次，并且本次运行内该目录下的同类动作都不再询问。
    AllowDir,
    /// 拒绝这一次（能力实现应抛 JS 异常）。
    Deny,
}

/// 宿主能力失败的原因。
///
/// 分类存在的原因是：上层要按类型给出不同文案（"已中止" / "已超时" / "当前环境不支持"），
/// 而不是把一句自由文本透传给用户。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostError {
    /// 任务被取消（用户按了中止键）。
    Cancelled,
    /// 等待用户回答超时。
    Timeout,
    /// 当前宿主不支持该能力（例如 headless 环境下的键盘输出）。
    Unsupported,
    /// 其它失败，附带可直接展示的说明。
    Failed(String),
}

impl HostError {
    /// 面向用户的中文说明（各能力可再加自己的前缀）。
    pub fn message(&self) -> String {
        match self {
            HostError::Cancelled => "脚本已中止".to_string(),
            HostError::Timeout => "等待用户确认超时".to_string(),
            HostError::Unsupported => "当前环境不支持该能力".to_string(),
            HostError::Failed(message) => message.clone(),
        }
    }
}

impl std::fmt::Display for HostError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message())
    }
}

impl std::error::Error for HostError {}

/// [`ScriptHost::progress`] 的命令：脚本通过 `$.progress*` 系列函数发出，
/// 宿主决定渲染成什么（应用里是独立的脚本进度窗口）。
///
/// 语义对齐 indicatif：一条进度条 = pos + 可选 total + msg，`Start` 可重复调用
/// （= 重置），`Finish` / `Abandon` 是终态收口。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProgressCmd {
    /// 创建/重置进度条；`total = None` 为不定长（spinner）模式。
    Start { total: Option<u64>, msg: String },
    /// 前进 n。
    Inc(u64),
    /// 设置当前位置。
    Set(u64),
    /// 修改总长（0 = 转不定长）。
    Total(u64),
    /// 更新消息。
    Msg(String),
    /// 完成：total 有值时 pos 提满；`Some(msg)` 覆盖消息。
    Finish(Option<String>),
    /// 放弃（非 100% 收口）；`Some(msg)` 覆盖消息。
    Abandon(Option<String>),
}

/// [`ScriptHost::type_str`] 的进度回调。
///
/// 宿主在输出过程中调用（每发出一个字符调一次即可，`done` / `total` 是**本次调用**的
/// 字符计数）；实现返回 `Err` 表示要**中止输出**（例如 JS 回调抛了异常），宿主应立即
/// 停止打字并把错误原样透传。
///
/// 调用频率不用宿主操心：`$.type_str` 的实现侧会再节流（短文本逐字符、长文本约 1%），
/// JS 回调不会被打爆。
pub type TypeProgress<'a> = dyn FnMut(u64, u64) -> Result<(), HostError> + 'a;

/// 脚本可以调用的宿主能力。
///
/// 实现必须是 `Send + Sync`：能力可能在任意线程被调用（GUI 里跑在 Worker 的阻塞线程上）。
///
/// `console.*` **不在这里** —— 它属于引擎的
/// [`ConsoleHook`](script_engine::ConsoleHook)，实现者按需分别实现两个 trait。
pub trait ScriptHost: Send + Sync + 'static {
    /// 逐字符输出文本（`$.type_str`）。
    ///
    /// `delay_ms` 是每个字符之间的间隔；实现应当在字符之间检查 [`ScriptHost::cancelled`]，
    /// 取消后返回 [`HostError::Cancelled`]。
    ///
    /// `on_progress` 是脚本通过第三个参数注册的进度回调（见 [`TypeProgress`]）：
    /// 实现应在输出过程中调用它（每字符一次即可）；`None` 表示脚本没注册，
    /// 此时回调开销为零。回调返回 `Err` 时立即中止输出并透传错误。
    fn type_str(
        &self,
        text: &str,
        delay_ms: u64,
        on_progress: Option<&mut TypeProgress<'_>>,
    ) -> Result<(), HostError>;

    /// 请求用户把焦点切到目标窗口（`$.request_focus`）。
    ///
    /// GUI 下弹出待命窗口并阻塞等待用户确认；`hint` 是显示给用户的提示
    /// （例如「请点击远程记事本」），空串表示用默认文案。与 [`ScriptHost::type_str`] 一样是
    /// **同步阻塞**调用：返回即表示「从现在起焦点在用户选定的目标窗口上」，随后的输出才安全。
    ///
    /// 默认空实现：headless 宿主没有待命窗口，直接返回。
    fn request_focus(&self, _hint: &str) -> Result<(), HostError> {
        Ok(())
    }

    /// 接收脚本进度条命令（`$.progress` / `$.progress_inc` / …，见 [`ProgressCmd`]）。
    ///
    /// 默认无操作：不关心脚本进度的宿主（纯 headless）可以不实现。
    /// 实现应当自行节流渲染，`Inc` 等命令可能非常频繁；收到 `Start` 时若已有
    /// 进度条则重置。任务结束时宿主自己负责收口（脚本可能忘了 finish）。
    fn progress(&self, _cmd: ProgressCmd) -> Result<(), HostError> {
        Ok(())
    }

    /// 向用户提问并等待回答（`$.confirm`）。
    ///
    /// 实现应当在**没有应答**时返回 [`ConfirmChoice::No`]（或 [`HostError::Timeout`]），
    /// 绝不能永久阻塞：脚本运行在 Worker 线程上。
    fn confirm(&self, message: &str) -> Result<ConfirmChoice, HostError>;

    /// 让用户挑一个文件或文件夹（`$.pick_path`）。
    ///
    /// * `prompt`：显示给用户的提示（GUI 下就是选择框标题），空串表示用默认文案；
    /// * `kind`：挑文件还是文件夹。
    ///
    /// 返回 `Ok(None)` 表示**没有拿到路径**：用户取消，或者当前宿主根本没有选择界面
    /// （headless 宿主没有选择界面）。调用方把它映射成 JS 的 `null`，**不是错误** ——
    /// 取消是正常操作，不该逼着脚本 `try/catch`。
    ///
    /// 没有默认实现：每个宿主都得自己想一次「我这里能不能选路径、怎么选」，
    /// 别让一个默认值把这个决定糊过去（有界面的宿主必须覆写它）。
    fn pick_path(&self, prompt: &str, kind: PickKind) -> Result<Option<PathBuf>, HostError>;

    /// 询问是否允许脚本修改文件系统（`$.write*` / `$.remove` / …）。
    ///
    /// * `action`：面向用户的动作描述，例如 `写入`、`删除`、`重命名 → /tmp/b`；
    /// * `path`：本次动作的目标路径；
    /// * `scope_dir`：用户选 [`FileDecision::AllowDir`] 时被记住的目录
    ///   （由能力实现按动作挑一个最贴合的范围，通常是目标路径的父目录）。
    ///
    /// 默认实现直接放行：只有「没有交互界面」的宿主才会落到这里，让脚本在 headless
    /// 环境下仍然可用；有界面的宿主必须覆写它。
    fn allow_file_change(
        &self,
        _action: &str,
        _path: &Path,
        _scope_dir: &Path,
    ) -> Result<FileDecision, HostError> {
        Ok(FileDecision::Allow)
    }

    /// 截屏一次并解出画面中的二维码文本（`$.scan_qr`）。
    ///
    /// 返回 `Ok(None)` 表示**这次没读到东西**：画面里没有二维码，或者当前宿主根本没有
    /// 屏幕访问（headless 宿主）。两种情形在脚本侧的语义相同 —— 该重试或降级 ——
    /// 与 [`ScriptHost::pick_path`] 在没有选择界面时返回 `Ok(None)` 是同一个思路。
    ///
    /// 实现应当只截一次、只解一次：**轮询节奏是脚本的事**（`await sleep()` 自己排），
    /// 只有脚本作者知道该等多久。
    fn scan_qr(&self) -> Result<Option<String>, HostError> {
        Ok(None)
    }

    /// 任务是否已被取消。
    fn cancelled(&self) -> bool {
        false
    }
}

/// 什么也不做的宿主：输出/确认都不可用。
///
/// 用于「只想跑纯计算脚本」的场景：能力报 `Unsupported`，而不是 panic。
pub struct NoopHost;

impl ScriptHost for NoopHost {
    fn type_str(
        &self,
        _text: &str,
        _delay_ms: u64,
        _on_progress: Option<&mut TypeProgress<'_>>,
    ) -> Result<(), HostError> {
        Err(HostError::Unsupported)
    }

    fn confirm(&self, _message: &str) -> Result<ConfirmChoice, HostError> {
        Err(HostError::Unsupported)
    }

    /// 没有选择界面：按「没拿到路径」处理（脚本侧是 `null`），而不是报错 ——
    /// 与 `$.confirm` 在没人应答时按「否」处理是同一个思路：让脚本自己兜底。
    fn pick_path(&self, _prompt: &str, _kind: PickKind) -> Result<Option<PathBuf>, HostError> {
        Ok(None)
    }
}

/// 上下文里保存的宿主句柄（`Ctx` userdata 的载体类型）。
///
/// 只装一个 `Arc`，不含任何 JS 值，因此可以安全地实现 [`JsLifetime`]。
pub(crate) struct HostRef(pub Arc<dyn ScriptHost>);

// SAFETY: `HostRef` 不含任何带 `'js` 生命周期的 JS 值，与上下文生命周期无关。
unsafe impl<'js> JsLifetime<'js> for HostRef {
    type Changed<'to> = HostRef;
}

/// 把宿主存进上下文（由 `RuntimeOptions::prepare` 钩子调用）。
///
/// 存进去的是 [`HostRef`]，能力实现用 [`host_ctx`] 取回；类型必须一一对应，
/// 存取不匹配会静默取不到。
pub fn store_host<'js>(ctx: &Ctx<'js>, host: Arc<dyn ScriptHost>) -> QjsResult<()> {
    ctx.store_userdata(HostRef(host)).map_err(|_| {
        rquickjs::Exception::throw_message(ctx, "宿主存入上下文失败（userdata 正被借用）")
    })?;
    Ok(())
}

/// 取回上下文里的宿主（克隆一个 `Arc`，调用方拿到所有权）。
pub fn host_ctx<'js>(ctx: &Ctx<'js>) -> Option<Arc<dyn ScriptHost>> {
    ctx.userdata::<HostRef>().map(|guard| guard.0.clone())
}
