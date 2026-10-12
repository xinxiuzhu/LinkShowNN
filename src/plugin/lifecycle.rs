//! 宿主资源装配、启停及事件入口。

use std::sync::Arc;

use anyhow::{Context, Result};
use nuono_plugin::prelude::*;
use tokio::sync::Semaphore;
use tracing::debug;

use crate::{
    auth::{
        BilibiliLogin, database_store,
        music::{MusicLogin, MusicPlatform},
        ncm::NcmQrClient,
        qqmusic::QqMusicQrClient,
    },
    config::ResolverConfig,
    download::DownloadManager,
    live::{self, LiveStore},
    render::CardRenderer,
    resolver::{ParserManager, SessionCookies},
    runtime::pipeline,
};

use super::LinkShowNN;

impl LinkShowNN {
    fn parser_and_config(&self) -> Result<(ResolverConfig, ParserManager)> {
        let state = self
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("resolver state lock poisoned"))?;
        Ok((
            state.config.clone(),
            state
                .parser
                .clone()
                .context("resolver parser is not loaded")?,
        ))
    }

    async fn stop_background(&self) -> Result<()> {
        let (tasks, poller, login, ncm_login, qqmusic_login) = {
            let state = self
                .state
                .lock()
                .map_err(|_| anyhow::anyhow!("resolver state lock poisoned"))?;
            (
                state.tasks.clone(),
                state.live_poller.clone(),
                state.bili_login.clone(),
                state.ncm_login.clone(),
                state.qqmusic_login.clone(),
            )
        };
        if let Some(login) = login {
            login.shutdown().await;
        }
        for login in [ncm_login, qqmusic_login].into_iter().flatten() {
            login.shutdown().await;
        }
        if let Some(tasks) = tasks {
            tasks.shutdown().await;
        }
        poller.stop().await;
        Ok(())
    }
}

#[async_trait]
impl Plugin for LinkShowNN {
    fn name(&self) -> &str {
        "LinkShowNN"
    }

    fn version(&self) -> &str {
        env!("CARGO_PKG_VERSION")
    }

    fn description(&self) -> &str {
        "多平台 URL 解析与 Bilibili Rust SDK 插件"
    }

    async fn on_load(&mut self, ctx: &PluginContext) -> Result<()> {
        self.stop_background().await?;
        ctx.ensure_data_dir().await?;
        let config = ResolverConfig::load(ctx.config.clone())?;
        debug!(
            enable_media_download = config.enable_media_download,
            video_max_mb = config.video_max_mb,
            download_queue_limit = config.download_queue_limit,
            "URL 解析器运行配置已加载"
        );
        let credential_store = database_store(ctx.secrets.clone());
        let saved = BilibiliLogin::read_saved(&credential_store).await?;
        let ncm_saved = MusicLogin::read_saved(MusicPlatform::Ncm, &credential_store).await?;
        let qqmusic_saved =
            MusicLogin::read_saved(MusicPlatform::QqMusic, &credential_store).await?;
        let parser = if saved.is_none() && ncm_saved.is_none() && qqmusic_saved.is_none() {
            ParserManager::new(config.clone())?
        } else {
            ParserManager::with_sessions(
                config.clone(),
                SessionCookies {
                    bili: saved.as_ref().map(crate::auth::Credentials::cookie_header),
                    ncm: ncm_saved
                        .as_ref()
                        .map(|session| session.cookie_header().to_owned()),
                    qqmusic: qqmusic_saved
                        .as_ref()
                        .map(|session| session.cookie_header().to_owned()),
                },
            )?
        };
        let login = BilibiliLogin::new(
            &self.state,
            ctx.tasks.clone(),
            ctx.permission_manager.clone(),
            credential_store.clone(),
            saved.as_ref(),
            &config.proxy,
        )?;
        let proxy = (!config.proxy.trim().is_empty()).then_some(config.proxy.as_str());
        let ncm_login = MusicLogin::new(
            MusicPlatform::Ncm,
            &self.state,
            ctx.tasks.clone(),
            ctx.permission_manager.clone(),
            credential_store.clone(),
            Arc::new(NcmQrClient::new(proxy)?),
            ncm_saved.as_ref(),
        );
        let qqmusic_login = MusicLogin::new(
            MusicPlatform::QqMusic,
            &self.state,
            ctx.tasks.clone(),
            ctx.permission_manager.clone(),
            credential_store,
            Arc::new(QqMusicQrClient::new(proxy)?),
            qqmusic_saved.as_ref(),
        );
        let renderer = config
            .render_cards
            .then(|| CardRenderer::new(&config.proxy, ctx.tasks.clone()))
            .transpose()?;
        let downloads = DownloadManager::new(
            (!config.proxy.is_empty()).then_some(config.proxy.as_str()),
            config.download_concurrent_limit,
            config.download_queue_limit,
            config.video_max_mb,
            config.resolve_media_dir(&ctx.data_dir),
        )?;
        let live = LiveStore::load(ctx.data_dir.join("live_subscriptions.json")).await?;
        {
            let mut state = self
                .state
                .lock()
                .map_err(|_| anyhow::anyhow!("resolver state lock poisoned"))?;
            state.config = config.clone();
            state.parser = Some(parser);
            state.renderer = renderer;
            state.downloads = Some(downloads);
            state.media_delivery_slots = Some(Arc::new(Semaphore::new(1)));
            state.live = Some(live);
            state.host_bot = ctx.bot.clone();
            state.tasks = Some(ctx.tasks.clone());
            state.bili_login = Some(login);
            state.ncm_login = Some(ncm_login);
            state.qqmusic_login = Some(qqmusic_login);
        }
        self.limiter.close();
        self.limiter = Arc::new(Semaphore::new(config.concurrent_limit));
        live::start_polling(&self.state).await
    }

    async fn on_enable(&mut self) -> Result<()> {
        live::start_polling(&self.state).await
    }

    async fn on_disable(&mut self) -> Result<()> {
        self.stop_background().await
    }

    async fn on_unload(&mut self) -> Result<()> {
        self.stop_background().await
    }

    async fn on_event(&self, event: &Event) -> Result<PluginResponse> {
        if event.post_type() != "message" {
            return Ok(PluginResponse::Continue);
        }
        if let Some(response) = live::handle(&self.state, event, event.text()).await? {
            return Ok(response);
        }
        let (config, parser) = self.parser_and_config()?;
        pipeline::run(&self.state, &self.limiter, event, &config, &parser).await
    }

    fn commands(&self) -> Vec<Command> {
        self.registered_commands()
    }

    fn service_config(&self) -> Plugins {
        Plugins::new(self.name())
            .with_prefix(vec!["url".to_string()])
            .with_pm(6)
            .with_area(PluginArea::All)
    }

    fn config_fields(&self) -> Vec<PluginConfigField> {
        ResolverConfig::get_schema()
    }
}
