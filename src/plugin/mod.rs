//! 插件入口与状态所有权。

mod commands;
mod lifecycle;

use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};

use tokio::sync::Semaphore;

use crate::runtime::state::RuntimeState;

pub struct LinkShowNN {
    state: Arc<Mutex<RuntimeState>>,
    limiter: Arc<Semaphore>,
    /// Keep the host's plugin directory for rebuilding download configuration.
    data_dir: Option<PathBuf>,
}

impl Default for LinkShowNN {
    fn default() -> Self {
        Self {
            state: Arc::new(Mutex::new(RuntimeState::default())),
            limiter: Arc::new(Semaphore::new(30)),
            data_dir: None,
        }
    }
}
