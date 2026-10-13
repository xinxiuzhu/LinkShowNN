//! 配置加载及媒体输出目录选择；类型与默认声明位于 default_config。
//!
//! 插件加载及宿主配置变更通知均读取完整快照；校验并装配成功后整体发布新状态。

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

    /// 媒体在宿主进程上的保存目录。配置优先，留空则沿用系统默认目录。
    ///
    /// 路径展开与规范化由下载器执行；这里不推断 OneBot 的部署位置或读取权限。
    /// 本地路径传输仅适用于 OneBot 能访问同一文件的同机或共享挂载部署。
    pub fn resolve_media_dir(&self, data_dir: &Path) -> PathBuf {
        match self.media_output_dir.trim() {
            "" => sandbox_readable_dir().unwrap_or_else(|| data_dir.join("media")),
            custom => PathBuf::from(custom),
        }
    }
}

/// macOS 沿用 Downloads 下的保存目录，找不到就返回 `None` 由调用方回退。
///
/// 这只是宿主的下载默认值，不保证客户端或远程 OneBot 可以直接读取。
pub(super) fn sandbox_readable_dir() -> Option<PathBuf> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    let home = std::env::var_os("HOME")?;
    let candidate = Path::new(&home).join("Downloads");
    candidate.is_dir().then(|| candidate.join("LinkShowNN"))
}
