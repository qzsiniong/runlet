//! 用户配置：中止热键 + 键盘输出时序 + 进度显示，JSON 持久化于系统 config 目录。
//!
//! 中止热键字符串使用 global-hotkey 的解析格式（如 "Cmd+Shift+K" / "Esc"）。

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// 进度显示方式。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
#[derive(Default)]
pub enum ProgressDisplay {
    /// 无框悬浮条(右上角 overlay 窗口)。
    #[default]
    Floating,
    /// 托盘图标进度(状态栏图标动态变化)。
    Tray,
    /// 两者同时。
    Both,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct Config {
    /// 任务中止热键（仅任务运行期间注册）。
    pub stop_hotkey: String,
    /// 逐键发送间隔（毫秒）。
    pub key_delay_ms: u64,
    /// 触发输出前、开始打字前的修饰键释放等待（毫秒）。
    pub settle_ms: u64,
    /// 进度显示方式。
    pub progress_display: ProgressDisplay,
    /// 是否启用按键真实发送。
    pub send_real_keys: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            stop_hotkey: "Esc".into(),
            key_delay_ms: 3,
            settle_ms: 200,
            progress_display: ProgressDisplay::Floating,
            send_real_keys: true,
        }
    }
}

impl Config {
    pub fn key_delay(&self) -> std::time::Duration {
        std::time::Duration::from_millis(self.key_delay_ms)
    }

    /// 配置文件路径：config_dir/Runlet/config.json。
    ///
    /// 项目改名前的品牌目录是 `ClipBeam`：新根目录还不存在、旧目录还在时先整体迁移
    /// （见 [`migrate_legacy_dir`]），老用户的 config.json 才能被继承。
    pub fn config_path() -> Option<PathBuf> {
        let path = dirs::config_dir().map(|d| d.join("Runlet").join("config.json"))?;
        if let (Some(root), Some(parent)) = (path.parent(), path.parent().and_then(Path::parent)) {
            migrate_legacy_dir(&parent.join("ClipBeam"), root);
        }
        Some(path)
    }

    /// 读取配置；文件缺失或损坏时静默回退默认值。
    ///
    /// 应用没有设置界面，配置只能手改 JSON，所以读入时顺手把越界值夹回合法区间：
    /// 一个 `key_delay_ms: 100000` 会让每个按键睡两分钟，而且没有任何界面会提示这件事。
    pub fn load() -> Self {
        let Some(path) = Self::config_path() else {
            return Self::default();
        };
        match std::fs::read_to_string(&path) {
            Ok(text) => match serde_json::from_str::<Self>(&text) {
                Ok(cfg) => cfg.sanitized(),
                Err(_) => Self::default(),
            },
            Err(_) => Self::default(),
        }
    }

    /// 把手改出来的越界值夹回可用范围。
    fn sanitized(mut self) -> Self {
        if self.stop_hotkey.trim().is_empty() {
            self.stop_hotkey = Config::default().stop_hotkey;
        }
        self.key_delay_ms = self.key_delay_ms.min(100);
        self.settle_ms = self.settle_ms.min(5000);
        self
    }
}

/// 旧品牌目录（`ClipBeam`）一次性迁移：目标不存在且旧目录还在时整体改名。
/// 幂等；rename 失败（跨盘、被占用）静默放弃，新版按缺省逻辑重建，不丢数据。
fn migrate_legacy_dir(legacy: &std::path::Path, root: &std::path::Path) {
    if !root.exists() && legacy.is_dir() {
        let _ = std::fs::rename(legacy, root);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_roundtrip_and_missing_fields() {
        let cfg = Config::default();
        let json = serde_json::to_string(&cfg).unwrap();
        let back: Config = serde_json::from_str(&json).unwrap();
        assert_eq!(cfg, back);
        // 旧版本/缺字段 JSON 应回退默认
        let partial: Config = serde_json::from_str("{}").unwrap();
        assert_eq!(partial, Config::default());
    }

    #[test]
    fn sanitize_clamps_hand_edited_values() {
        let cfg = Config {
            key_delay_ms: 100_000,
            settle_ms: 90_000,
            stop_hotkey: "  ".into(),
            ..Config::default()
        };

        let fixed = cfg.sanitized();
        assert_eq!(fixed.key_delay_ms, 100);
        assert_eq!(fixed.settle_ms, 5000);
        assert_eq!(fixed.stop_hotkey, "Esc");
    }

    #[test]
    fn legacy_brand_dir_migrates_once() {
        let base = std::env::temp_dir().join(format!(
            "runlet-config-migrate-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .subsec_nanos()
        ));
        let _ = std::fs::remove_dir_all(&base);

        let legacy = base.join("ClipBeam");
        std::fs::create_dir_all(&legacy).unwrap();
        std::fs::write(legacy.join("config.json"), "{}").unwrap();

        let root = base.join("Runlet");
        migrate_legacy_dir(&legacy, &root);
        assert!(root.is_dir(), "旧目录应整体改名成新目录");
        assert!(!legacy.exists(), "旧目录应消失");

        // 幂等：旧目录已不在，再次调用是新目录不动
        migrate_legacy_dir(&legacy, &root);
        assert!(root.is_dir());

        let _ = std::fs::remove_dir_all(&base);
    }
}
