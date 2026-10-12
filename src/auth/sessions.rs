//! 平台账号变更共用一把锁，避免重建解析器时覆盖其它平台的登录状态。

use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, ensure};

use super::{
    Credentials,
    music::{MusicCredentials, MusicPlatform},
    store::{BILIBILI_SESSION_KEY, CredentialStore},
};
use crate::{resolver::ParserManager, runtime::state::RuntimeState};

pub(crate) async fn apply_bili(
    state: &Arc<Mutex<RuntimeState>>,
    store: &Arc<dyn CredentialStore>,
    credentials: Option<&Credentials>,
) -> Result<()> {
    let cookie = credentials.map(Credentials::cookie_header);
    let encoded = credentials.map(Credentials::encode).transpose()?;
    apply(state, store, Change::Bili(cookie), encoded).await
}

pub(crate) async fn apply_music(
    state: &Arc<Mutex<RuntimeState>>,
    store: &Arc<dyn CredentialStore>,
    platform: MusicPlatform,
    credentials: Option<&MusicCredentials>,
) -> Result<()> {
    ensure!(
        credentials.is_none_or(|credentials| credentials.platform() == platform),
        "音乐登录凭据的平台不匹配"
    );
    let cookie = credentials.map(|credentials| credentials.cookie_header().to_owned());
    let encoded = credentials.map(MusicCredentials::encode).transpose()?;
    apply(state, store, Change::Music(platform, cookie), encoded).await
}

enum Change {
    Bili(Option<String>),
    Music(MusicPlatform, Option<String>),
}

async fn apply(
    state: &Arc<Mutex<RuntimeState>>,
    store: &Arc<dyn CredentialStore>,
    change: Change,
    encoded: Option<String>,
) -> Result<()> {
    let mutations = state
        .lock()
        .map_err(|_| anyhow::anyhow!("插件账号状态不可用"))?
        .auth_mutations
        .clone();
    let _mutation = mutations.lock().await;
    let (config, mut sessions) = {
        let state = state
            .lock()
            .map_err(|_| anyhow::anyhow!("插件账号状态不可用"))?;
        let parser = state.parser.as_ref().context("解析器未加载")?;
        (state.config.clone(), parser.session_cookies())
    };
    let key = match change {
        Change::Bili(cookie) => {
            sessions.bili = cookie;
            BILIBILI_SESSION_KEY
        }
        Change::Music(platform, cookie) => {
            match platform {
                MusicPlatform::Ncm => sessions.ncm = cookie,
                MusicPlatform::QqMusic => sessions.qqmusic = cookie,
            }
            platform.session_key()
        }
    };
    // 先校验完整候选，持久化成功才发布。持锁期间其它平台不会读取旧会话再覆盖。
    let parser = ParserManager::with_sessions(config, sessions)?;
    match encoded {
        Some(encoded) => store.write(key, &encoded).await?,
        None => store.remove(key).await?,
    }
    state
        .lock()
        .map_err(|_| anyhow::anyhow!("插件账号状态不可用"))?
        .parser = Some(parser);
    Ok(())
}

#[cfg(test)]
mod tests;
