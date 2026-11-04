//! 键盘模拟封装：逐字符输入，每键前查取消令牌、键间可配延迟。
//! send（协议 A）与 deploy（协议 C 引导包）共用。
//!
//! macOS：使用 CoreGraphics 的 CGEventKeyboardSetUnicodeString 直接发送
//! Unicode 按键事件，不依赖 TSM 键盘布局查询（可在任意线程调用，避免
//! enigo 的 `dispatch_assert_queue_fail` 崩溃）。
//! 其他平台：使用 enigo。

use enigo::{Direction, Key, Keyboard};
use log::debug;

use crate::config::Config;
use crate::keymap::{get_key_info, KeyAction};
use std::time::Duration;

pub struct Typer {
    delay: Duration,
    send_real_keys: bool,
    // macOS: ManuallyDrop 跳过 enigo 的 Drop——其 Drop 内有累积 sleep 逻辑
    //（每次按键 update_wait_time 累加 20ms，长文本 Drop 时会阻塞数秒甚至数分钟）。
    #[cfg(target_os = "macos")]
    enigo: std::mem::ManuallyDrop<enigo::Enigo>,
    #[cfg(not(target_os = "macos"))]
    enigo: enigo::Enigo,
}

impl Typer {
    pub fn new(cfg: &Config) -> Result<Self, String> {
        let enigo = enigo::Enigo::new(&enigo::Settings::default())
            .map_err(|e| format!("无法初始化键盘模拟，请检查系统的辅助功能/输入监控权限: {e}"))?;
        Ok(Self {
            delay: cfg.key_delay(),
            send_real_keys: cfg.send_real_keys,
            #[cfg(target_os = "macos")]
            enigo: std::mem::ManuallyDrop::new(enigo),
            #[cfg(not(target_os = "macos"))]
            enigo,
        })
    }

    /// 当前是否真的发送按键（false = 演练，只统计进度不注入事件）。
    pub fn send_real_keys(&self) -> bool {
        self.send_real_keys
    }

    /// 等待指定延迟。
    fn wait_delay(&self) {
        if self.delay > Duration::ZERO {
            std::thread::sleep(self.delay);
        }
    }

    /// 模拟输入单个字符。
    ///
    /// 查 keymap 得基础字符与是否需 Shift；大写字母 / 特殊符号通过显式 Shift
    /// press/release 实现。未知字符（如非 ASCII）回退 `text()` Unicode 输入。
    pub fn send_char(&mut self, ch: char, dry_run: bool) -> Result<(), String> {
        debug!("send_char {:?}, dry_run: {}", ch, dry_run);
        match get_key_info(ch) {
            Some(KeyAction::Char {
                #[allow(unused_variables)]
                base,
                shift,
                #[allow(unused_variables)]
                mac_keycode,
            }) => {
                if shift {
                    if !dry_run {
                        let _ = self.enigo.key(Key::Shift, Direction::Press);
                    }
                    self.wait_delay();
                }
                #[cfg(target_os = "macos")]
                {
                    // macOS 直接用 raw keycode，绕过 enigo 的 get_layoutdependent_keycode
                    // （后者遍历不 break，小键盘 `.`keycode=65 覆盖主键盘 47，导致 `>`→`.`）
                    if !dry_run {
                        let _ = self.enigo.raw(mac_keycode, Direction::Click);
                    }
                }
                #[cfg(not(target_os = "macos"))]
                {
                    if !dry_run {
                        let _ = self.enigo.key(Key::Unicode(base), Direction::Click);
                    }
                }
                if shift {
                    self.wait_delay();
                    if !dry_run {
                        let _ = self.enigo.key(Key::Shift, Direction::Release);
                    }
                }
            }
            Some(KeyAction::Return) => {
                if !dry_run {
                    let _ = self.enigo.key(Key::Return, Direction::Click);
                }
            }
            Some(KeyAction::Tab) => {
                if !dry_run {
                    let _ = self.enigo.key(Key::Tab, Direction::Click);
                }
            }
            None => {
                // 非 ASCII / 未知字符：回退 Unicode 文本输入
                if !dry_run {
                    let _ = self.enigo.text(&ch.to_string());
                }
            }
        }
        Ok(())
    }
}

impl Drop for Typer {
    fn drop(&mut self) {
        // 释放可能残留的 Shift（任务中途取消时 Shift 可能处于按下状态）
        let _ = self.enigo.key(Key::Shift, Direction::Release);
        // macOS: enigo 包在 ManuallyDrop 中，其 Drop（含累积 sleep）不会运行
        // 其他平台: enigo 正常 Drop
    }
}
