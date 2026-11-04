//! `$.type_str` / `$.confirm` / `$.request_focus` / `$.pick_path`：宿主交互扩展。
//!
//! 这几个能力不属于引擎：它们问的是「把文本送到哪儿」「谁来回答」「选哪个路径」。
//! 因此实现只做一件事 —— 通过 [`crate::host::host_ctx`] 找到使用方注入的宿主，
//! 把动作委托出去：
//!
//! * 无界面宿主：`confirm` / `pick_path` 直接给默认答案（没有界面可问）；
//! * GUI 宿主：逐键打进当前焦点窗口 / 弹系统原生确认框与选择框 / 待命窗口。
//!
//! 拿不到宿主时抛出明确错误（而不是静默什么都不做），方便定位接线问题。

use std::cell::RefCell;
use std::time::Duration;

use governor::{Quota, RateLimiter};
use rquickjs::function::Opt;
use rquickjs::{Ctx, Exception, Result as QjsResult, String as JsString, Value};

use script_engine::bindings::{cancelled, throw_cancelled};

use crate::extensions::{extension, require_host, throw_host_error};
use crate::host::{ConfirmChoice, PickKind};
use crate::HostError;

thread_local! {
    /// `$.type_str` 是否正在输出中。进度回调在宿主打字的**同时**执行，
    /// 此时再进 `$.type_str` 会在宿主的内部状态（Typer 锁）上重入死锁，必须挡下。
    static TYPING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// 进度回调的时间节流窗口（毫秒）。与 GUI 宿主进度条 `emit_progress`
/// （src-tauri/src/scripting.rs）用的是同一个值：两边语义保持一致 ——
/// 距上次回调不足 1 秒就跳过，首尾两个边界必达。
const PROGRESS_THROTTLE_MS: u64 = 1000;

extension! {
    /// `$.type_str`。
    pub struct TypeStrExtension;
    "type_str" => js_type_str,
        "type_str(text: string, delayMs?: number, onProgress?: (done: number, total: number) => void) -> void",
        "把文本交给宿主输出（GUI 下逐键打进当前焦点窗口）；onProgress 在输出过程中收到 (已输出, 总数)；被中止时抛异常";
}

extension! {
    /// `$.confirm`。
    pub struct ConfirmExtension;
    async "confirm" => js_confirm,
        "confirm(message: string) -> Promise<boolean>",
        "向用户提问；回答「是」为 true，「否」为 false，用户中止时抛异常";
}

extension! {
    /// `$.request_focus`。
    pub struct RequestFocusExtension;
    "request_focus" => js_request_focus,
        "request_focus(hint?: string) -> void",
        "请求用户把焦点切到目标窗口（GUI 下弹出待命窗口并等待确认）；hint 是显示给用户的提示";
}

extension! {
    /// `$.pick_path`。
    pub struct PickPathExtension;
    async "pick_path" => js_pick_path,
        "pick_path(prompt?: string, kind?: \"file\" | \"dir\") -> Promise<string | null>",
        "让用户挑一个文件或文件夹（prompt 是选择框标题）；取消或没有选择界面时返回 null";
}

/// `type_str(text, delayMs = 0, onProgress?) -> void`
///
/// 同步阻塞（宿主负责每个字符之间的等待），因此输出顺序与脚本执行顺序严格一致。
/// 被中止时抛「脚本已中止」，让脚本立即结束。
///
/// `onProgress(done, total)` 在输出过程中被调用：宿主每发出一个字符都会通知，
/// 但这里做了**时间节流**（与 GUI 宿主进度条 `emit_progress` 同一套语义）——
/// 首字符与最后一个字符必达，中间最多每 [`PROGRESS_THROTTLE_MS`] 一次，
/// JS 回调不会被打爆。回调里抛异常会**中止输出**并把异常原样抛给脚本；
/// 回调里不能再调 `$.type_str`（宿主仍在打字，重入会死锁）。
fn js_type_str<'js>(
    ctx: Ctx<'js>,
    text: String,
    delay_ms: Opt<u64>,
    on_progress: Opt<rquickjs::Function<'js>>,
) -> QjsResult<()> {
    if TYPING.with(|typing| typing.get()) {
        return Err(Exception::throw_message(
            &ctx,
            "$.type_str 的进度回调里不能再调用 $.type_str（宿主仍在输出，重入会死锁）",
        ));
    }

    let host = require_host(&ctx, "$.type_str")?;

    if cancelled(&ctx) {
        return Err(throw_cancelled(&ctx));
    }

    // `Opt`：JS 侧可以只传 text，delayMs 缺省为 0（`Option<u64>` 会要求参数个数完全匹配）
    let delay = delay_ms.0.unwrap_or(0);

    // 回调抛出的 JS 异常挂在 ctx 上；先把 rquickjs 错误记下来，宿主返回后优先透传
    // 它（而不是宿主自己的错误），异常才能原样到达脚本。
    let callback_error: RefCell<Option<rquickjs::Error>> = RefCell::new(None);

    // 时间节流：GCRA 限流器，每个周期放行一次（burst = 1），与 emit_progress 的
    // 「距上次不足 PROGRESS_THROTTLE_MS 就跳过」是同一窗口。首尾两个 edge 绕过限流器。
    let limiter = RateLimiter::direct(
        Quota::with_period(Duration::from_millis(PROGRESS_THROTTLE_MS)).expect("节流周期必须非零"),
    );

    let callback = on_progress.0;

    let mut report = |done: u64, total: u64| -> Result<(), crate::HostError> {
        // 节流：见函数文档。edge（首字符 / 末字符）必报，中间按时间窗口放行
        let is_edge = done == 1 || done == total;
        if !is_edge && limiter.check().is_err() {
            return Ok(());
        }

        let Some(callback) = &callback else {
            return Ok(());
        };
        match callback.call::<_, ()>((done, total)) {
            Ok(()) => Ok(()),
            Err(err) => {
                // JS 异常已在 ctx 上，记下来让输出立即中止
                *callback_error.borrow_mut() = Some(err);
                Err(crate::HostError::Failed(String::new()))
            }
        }
    };

    // 重入守卫：见函数文档。guard 结构保证宿主 panic 时标志也会被清掉。
    struct Guard;
    impl Drop for Guard {
        fn drop(&mut self) {
            TYPING.with(|typing| typing.set(false));
        }
    }
    TYPING.with(|typing| typing.set(true));
    let _guard = Guard;

    let result = host.type_str(&text, delay, Some(&mut report));

    drop(_guard);

    // 回调抛的异常优先于宿主错误：脚本能 catch 到自己回调里抛的东西
    if let Some(err) = callback_error.into_inner() {
        return Err(err);
    }

    result.map_err(|err| throw_host_error(&ctx, err))
}

/// `confirm(message) -> Promise<boolean>`
///
/// * `Yes` → `true`；`No` → `false`；
/// * `Abort` → 抛「脚本已中止」，让脚本立即结束（不是被拒绝的 Promise，避免脚本
///   `catch` 之后继续往下跑）。
async fn js_confirm<'js>(ctx: Ctx<'js>, message: String) -> QjsResult<bool> {
    let host = require_host(&ctx, "$.confirm")?;

    match host.confirm(&message) {
        Ok(ConfirmChoice::Yes) => Ok(true),
        Ok(ConfirmChoice::No) => Ok(false),
        Ok(ConfirmChoice::Abort) => Err(throw_cancelled(&ctx)),
        Err(err) => Err(throw_host_error(&ctx, err)),
    }
}

/// `request_focus(hint?) -> void`
///
/// **同步**阻塞到用户确认（或超时/中止）—— 与 [`js_type_str`] 一致：返回即表示「焦点已经在
/// 用户选定的目标窗口上」，脚本可以放心继续输出。脚本要在**中途**换一个输出目标时显式调用它
/// （例如先打进浏览器、再打进记事本）；不调用也能靠宿主的自动焦点校验兜住。
fn js_request_focus<'js>(ctx: Ctx<'js>, hint: Opt<String>) -> QjsResult<()> {
    let host = require_host(&ctx, "$.request_focus")?;

    if cancelled(&ctx) {
        return Err(throw_cancelled(&ctx));
    }

    host.request_focus(hint.0.as_deref().unwrap_or(""))
        .map_err(|err| throw_host_error(&ctx, err))
}

/// `pick_path(prompt?, kind?) -> Promise<string | null>`
///
/// * `prompt` 缺省或空串时由宿主用默认文案（「请选择文件」/「请选择文件夹」）；
/// * `kind` 只认 `"file"`（缺省）/ `"dir"`：写错了当场抛错，而不是静默按文件处理；
/// * 返回所选文件 / 文件夹的**绝对路径**；用户取消、或宿主没有选择界面（headless、
///   没有选择界面）时返回 `null` —— 取消是正常操作，不该逼脚本 `try/catch`。
async fn js_pick_path<'js>(
    ctx: Ctx<'js>,
    prompt: Opt<String>,
    kind: Opt<String>,
) -> QjsResult<Value<'js>> {
    let host = require_host(&ctx, "$.pick_path")?;

    if cancelled(&ctx) {
        return Err(throw_cancelled(&ctx));
    }

    let kind = match kind.0.as_deref() {
        None | Some("") | Some("file") => PickKind::File,
        Some("dir") => PickKind::Dir,
        Some(other) => {
            return Err(Exception::throw_message(
                &ctx,
                &format!("$.pick_path 的 kind 只支持 \"file\" / \"dir\"，收到：{other:?}"),
            ));
        }
    };

    let picked = host
        .pick_path(prompt.0.as_deref().unwrap_or(""), kind)
        .map_err(|err| throw_host_error(&ctx, err))?;

    // 显式造 `null`：rquickjs 把 `Option::None` 映射成 `undefined`，而「没选到」在
    // JS 侧的语义是 `null`（`p === null` 才是判断取消的写法）
    let Some(path) = picked else {
        return Ok(Value::new_null(ctx.clone()));
    };

    match path.to_str() {
        Some(text) => Ok(JsString::from_str(ctx.clone(), text)?.into_value()),
        None => Err(throw_host_error(
            &ctx,
            HostError::Failed(format!("所选路径不是有效的 UTF-8：{}", path.display())),
        )),
    }
}
