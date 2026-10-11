//! 插件生命周期、命令注册与宿主资源装配。

use std::sync::{Arc, Mutex};

use crate::resolver::ParserManager;
use crate::{
    default_config::ResolverConfig,
    live,
    render::CardRenderer,
    runtime::{pipeline, state::RuntimeState},
};
use crate::{download::DownloadManager, live::LiveStore};
use anyhow::{Context, Result};
use nuono_plugin::prelude::*;
use tokio::sync::Semaphore;
use tracing::debug;

pub struct LinkShowNN {
    state: Arc<Mutex<RuntimeState>>,
    limiter: Arc<Semaphore>,
}

impl Default for LinkShowNN {
    fn default() -> Self {
        Self {
            state: Arc::new(Mutex::new(RuntimeState::default())),
            limiter: Arc::new(Semaphore::new(30)),
        }
    }
}

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
        let (tasks, poller) = {
            let state = self
                .state
                .lock()
                .map_err(|_| anyhow::anyhow!("resolver state lock poisoned"))?;
            (state.tasks.clone(), state.live_poller.clone())
        };
        if let Some(tasks) = tasks {
            tasks.shutdown().await;
        }
        poller.stop().await;
        Ok(())
    }
}

struct InfoHandler {
    state: Arc<Mutex<RuntimeState>>,
    help: bool,
}

#[async_trait]
impl TriggerHandler for InfoHandler {
    async fn handle(&self, bot: &Bot, _msg: &Message, _data: MatchData) -> Result<()> {
        if self.help {
            bot.send(
                "LinkShowNN 支持：B站、抖音、快手、小红书、Twitter/X、网易云、库街区、腾讯频道。\n直接发送链接即可自动解析。\n管理员命令：url状态。",
            )
            .await?;
            return Ok(());
        }
        let snapshot = {
            let state = self
                .state
                .lock()
                .map_err(|_| anyhow::anyhow!("resolver state lock poisoned"))?;
            let mut report = state.metrics.report(
                state.config.enable_media_download,
                state.config.concurrent_limit,
                state.config.user_cooldown,
            );
            report.push_str(if state.config.obscura_enabled {
                "\n分享页引擎: Obscura"
            } else {
                "\n分享页引擎: HTTP"
            });
            report.push_str(if state.config.douyin_anonymous_browser {
                "\n抖音: API 优先，匿名 Chrome/Edge 兜底（需本机浏览器）"
            } else {
                "\n抖音: API / HTTP 分享页（匿名浏览器已关闭）"
            });
            report.push_str(if state.config.render_cards {
                "\n解析回复: 图文卡片"
            } else {
                "\n解析回复: 原图文消息"
            });
            report
        };
        bot.send(snapshot).await?;
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
        let config = ResolverConfig::from_value(ctx.config.clone())?;
        debug!(
            enable_media_download = config.enable_media_download,
            video_max_mb = config.video_max_mb,
            download_queue_limit = config.download_queue_limit,
            obscura_enabled = config.obscura_enabled,
            "URL 解析器运行配置已加载"
        );
        let parser = ParserManager::new(config.clone(), ctx.browser(), ctx.tasks.clone())?;
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
            state.live = Some(live);
            state.host_bot = ctx.bot.clone();
            state.tasks = Some(ctx.tasks.clone());
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
        vec![
            Command::from_message(
                "帮助",
                Arc::new(InfoHandler {
                    state: self.state.clone(),
                    help: true,
                }),
            )
            .with_alias("help")
            .with_description("查看 LinkShowNN 帮助")
            .with_block(true),
            Command::from_message(
                "状态",
                Arc::new(InfoHandler {
                    state: self.state.clone(),
                    help: false,
                }),
            )
            .with_alias("status")
            .with_description("查看 LinkShowNN 运行统计")
            .with_permission(PermissionLevel::GroupAdmin)
            .with_block(true),
        ]
    }

    fn service_config(&self) -> Plugins {
        Plugins::new(self.name())
            .with_prefix(vec!["url".to_string()])
            .with_priority(5)
            .with_pm(6)
            .with_area(PluginArea::All)
    }

    fn config_fields(&self) -> Vec<PluginConfigField> {
        ResolverConfig::fields()
    }
}
