//! 插件运行时状态。
//!
//! 所有可变状态都收在这里，由 `Mutex` 保护。锁只在同步块内持有，绝不跨越
//! await——需要 await 的资源先在锁内克隆出来再放锁。

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Instant,
};

use anyhow::Result;
use nuono_plugin::prelude::*;

use crate::{
    default_config::ResolverConfig,
    download::DownloadManager,
    live::{LivePoller, LiveStore},
    render::CardRenderer,
    resolver::ParserManager,
    runtime::metrics::Metrics,
};

#[derive(Default)]
pub struct RuntimeState {
    pub config: ResolverConfig,
    pub parser: Option<ParserManager>,
    pub renderer: Option<CardRenderer>,
    pub last_calls: HashMap<String, Instant>,
    pub metrics: Metrics,
    pub tasks: Option<Arc<PluginTaskScope>>,
    pub downloads: Option<DownloadManager>,
    pub live: Option<LiveStore>,
    pub live_poller: LivePoller,
    pub host_bot: Option<Arc<Bot>>,
}

/// 在锁内跑一个同步闭包并放锁。
///
/// 闭包不是 async，所以类型系统保证锁不会跨 await 持有。中毒锁统一转成错误，
/// 免得每个调用点各写一遍 `map_err`。
pub fn with<T>(
    state: &Mutex<RuntimeState>,
    apply: impl FnOnce(&mut RuntimeState) -> T,
) -> Result<T> {
    let mut guard = state
        .lock()
        .map_err(|_| anyhow::anyhow!("resolver state lock poisoned"))?;
    Ok(apply(&mut guard))
}
