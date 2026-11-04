//! `$.progress` / `$.progress_inc` / …：脚本进度条扩展。
//!
//! 只做两件事 —— 校验参数、把动作打包成 [`ProgressCmd`] 委托给宿主：
//!
//! * 其它宿主：可以按 [`ProgressCmd`] 自己渲染（headless 宿主丢弃即可）；
//! * GUI 宿主：渲染到独立的脚本进度窗口。
//!
//! 语义对齐 indicatif：`$.progress` 建条（每任务同时只一条，重复调用 = 重置），
//! `finish` / `abandon` 是终态收口。任务结束时宿主自己负责收口，脚本忘写
//! `finish` 也不会挂住进度显示。
//!
//! 拿不到宿主时抛出明确错误（与 `$.type_str` 等交互能力同一思路），方便定位接线问题。

use rquickjs::function::Opt;
use rquickjs::{Ctx, Exception, Result as QjsResult};

use crate::extensions::{extension, require_host, throw_host_error};
use crate::host::ProgressCmd;

extension! {
    /// `$.progress`。
    pub struct ProgressExtension;
    "progress" => js_progress,
        "progress(total?: number, msg?: string) -> void",
        "创建/重置脚本进度条；total 缺省为不定长模式，msg 是显示的消息（缺省为空）";
}

extension! {
    /// `$.progress_inc`。
    pub struct ProgressIncExtension;
    "progress_inc" => js_progress_inc,
        "progress_inc(n?: number) -> void",
        "进度条前进 n（缺省 1）";
}

extension! {
    /// `$.progress_set`。
    pub struct ProgressSetExtension;
    "progress_set" => js_progress_set,
        "progress_set(pos: number) -> void",
        "把进度条当前位置设为 pos";
}

extension! {
    /// `$.progress_total`。
    pub struct ProgressTotalExtension;
    "progress_total" => js_progress_total,
        "progress_total(total: number) -> void",
        "修改进度条总长（0 转为不定长模式）";
}

extension! {
    /// `$.progress_msg`。
    pub struct ProgressMsgExtension;
    "progress_msg" => js_progress_msg,
        "progress_msg(msg: string) -> void",
        "更新进度条显示的消息";
}

extension! {
    /// `$.progress_finish`。
    pub struct ProgressFinishExtension;
    "progress_finish" => js_progress_finish,
        "progress_finish(msg?: string) -> void",
        "完成进度条（总长有值时位置提满）；msg 缺省保留当前消息";
}

extension! {
    /// `$.progress_abandon`。
    pub struct ProgressAbandonExtension;
    "progress_abandon" => js_progress_abandon,
        "progress_abandon(msg?: string) -> void",
        "放弃进度条（非 100% 收口）；msg 缺省保留当前消息";
}

/// JS number → 非负整数（f64 → u64）；NaN / 无穷 / 负数 / 小数当场抛 TypeError。
///
/// 进度位置和总长没有小数语义，与其让 rquickjs 把 `1.5` 静默取整，不如让脚本作者
/// 第一时间看到参数问题。`None`（缺省参数）原样通过。
fn uint_arg<'js>(
    ctx: &Ctx<'js>,
    value: Option<f64>,
    api: &str,
    name: &str,
) -> QjsResult<Option<u64>> {
    let Some(v) = value else {
        return Ok(None);
    };
    if !v.is_finite() || v < 0.0 || v.fract() != 0.0 {
        return Err(Exception::throw_type(
            ctx,
            &format!("{api} 的 {name} 必须是非负整数，收到 {v}"),
        ));
    }
    Ok(Some(v as u64))
}

fn js_progress<'js>(ctx: Ctx<'js>, total: Opt<f64>, msg: Opt<String>) -> QjsResult<()> {
    let host = require_host(&ctx, "$.progress")?;
    let total = uint_arg(&ctx, total.0, "$.progress", "total")?;
    host.progress(ProgressCmd::Start {
        total,
        msg: msg.0.unwrap_or_default(),
    })
    .map_err(|e| throw_host_error(&ctx, e))?;
    Ok(())
}

fn js_progress_inc<'js>(ctx: Ctx<'js>, n: Opt<f64>) -> QjsResult<()> {
    let host = require_host(&ctx, "$.progress_inc")?;
    let n = uint_arg(&ctx, n.0, "$.progress_inc", "n")?.unwrap_or(1);
    host.progress(ProgressCmd::Inc(n))
        .map_err(|e| throw_host_error(&ctx, e))?;
    Ok(())
}

fn js_progress_set<'js>(ctx: Ctx<'js>, pos: f64) -> QjsResult<()> {
    let host = require_host(&ctx, "$.progress_set")?;
    let pos = uint_arg(&ctx, Some(pos), "$.progress_set", "pos")?.unwrap_or(0);
    host.progress(ProgressCmd::Set(pos))
        .map_err(|e| throw_host_error(&ctx, e))?;
    Ok(())
}

fn js_progress_total<'js>(ctx: Ctx<'js>, total: f64) -> QjsResult<()> {
    let host = require_host(&ctx, "$.progress_total")?;
    let total = uint_arg(&ctx, Some(total), "$.progress_total", "total")?.unwrap_or(0);
    host.progress(ProgressCmd::Total(total))
        .map_err(|e| throw_host_error(&ctx, e))?;
    Ok(())
}

fn js_progress_msg<'js>(ctx: Ctx<'js>, msg: String) -> QjsResult<()> {
    let host = require_host(&ctx, "$.progress_msg")?;
    host.progress(ProgressCmd::Msg(msg))
        .map_err(|e| throw_host_error(&ctx, e))?;
    Ok(())
}

fn js_progress_finish<'js>(ctx: Ctx<'js>, msg: Opt<String>) -> QjsResult<()> {
    let host = require_host(&ctx, "$.progress_finish")?;
    host.progress(ProgressCmd::Finish(msg.0))
        .map_err(|e| throw_host_error(&ctx, e))?;
    Ok(())
}

fn js_progress_abandon<'js>(ctx: Ctx<'js>, msg: Opt<String>) -> QjsResult<()> {
    let host = require_host(&ctx, "$.progress_abandon")?;
    host.progress(ProgressCmd::Abandon(msg.0))
        .map_err(|e| throw_host_error(&ctx, e))?;
    Ok(())
}
