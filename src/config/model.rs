//! 配置加载及媒体输出目录选择；类型与默认声明位于 default_config。
//!
//! 插件加载时读取一次完整配置；保存新配置后重新加载插件或重启服务生效。

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::Value;

use super::ResolverConfig;

impl Default for ResolverConfig {
    fn default() -> Self {
        Self::from_value(&serde_json::json!({}))
            .expect("declared LinkShowNN defaults must decode into ResolverConfig")
    }
}
impl ResolverConfig {
    /// 兼容宿主尚未提供配置时的 null 快照，其余解析交给 SDK。
    pub fn load(value: Value) -> Result<Self> {
        let value = if value.is_null() {
            serde_json::json!({})
        } else {
            value
        };
        Self::from_value(&value).context("invalid URL resolver config")
    }

    #[cfg(test)]
    pub fn defaults_json() -> Value {
        serde_json::to_value(Self::default()).unwrap_or_else(|_| serde_json::json!({}))
    }

    /// 媒体落盘目录。配置优先，留空则退到宿主 OneBot 读得到的位置。
    ///
    /// 插件自己的 data 目录不是安全默认值：macOS 上 QQ 跑在 App Sandbox 里，
    /// 除了 `~/Downloads` 这类有内建例外的目录，其余绝对路径都会被 EPERM 拒绝，
    /// 表现为下载成功但 `send_msg` 报 `EPERM: operation not permitted`。
    pub fn resolve_media_dir(&self, data_dir: &Path) -> PathBuf {
        match self.media_output_dir.trim() {
            "" => sandbox_readable_dir().unwrap_or_else(|| data_dir.join("media")),
            custom => PathBuf::from(custom),
        }
    }
}

/// 宿主进程沙箱化时仍可读的目录，找不到就返回 `None` 由调用方回退。
///
/// 只在 macOS 生效：Linux 和 Windows 的 OneBot 实现通常不受这类文件系统限制，
/// 沿用插件 data 目录即可。
pub(super) fn sandbox_readable_dir() -> Option<PathBuf> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    let home = std::env::var_os("HOME")?;
    let candidate = Path::new(&home).join("Downloads");
    candidate.is_dir().then(|| candidate.join("LinkShowNN"))
}
