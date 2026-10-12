//! 站点装配与链接路由。
//!
//! 配置只在装配时读取，各适配器只接收自己需要的字段。HTTP 客户端的 clone
//! 共享连接池；新增站点无需扩展任何公共运行上下文。

use std::{collections::HashMap, sync::Arc, time::Duration};

use anyhow::{Context, Result};
use reqwest::Client;

use crate::{
    config::ResolverConfig,
    resolver::fetch::PageFetcher,
    resolver::url_policy::MatchedUrl,
    resolver::{Platform, Resolved},
    resolver::{
        adapter::PlatformAdapter,
        bilibili::{BilibiliAdapter, Client as BilibiliClient, LiveInfo},
        douyin::DouyinAdapter,
        douyin::client::DouyinClient,
        kuaishou::KuaishouAdapter,
        kurobbs::KurobbsAdapter,
        ncm::NcmAdapter,
        qq_channel::QqChannelAdapter,
        qqmusic::QqMusicAdapter,
        twitter::TwitterAdapter,
        xiaohongshu::XiaohongshuAdapter,
    },
};

/// 仅保存扫码得到的会话；手动配置仍由各平台装配时选择。
/// 不实现 Debug，避免凭据进入日志。
#[derive(Clone, Default)]
pub(crate) struct SessionCookies {
    pub bili: Option<String>,
    pub ncm: Option<String>,
    pub qqmusic: Option<String>,
}

#[derive(Clone)]
pub struct ParserManager {
    adapters: Arc<HashMap<Platform, Box<dyn PlatformAdapter>>>,
    bili_live: BilibiliClient,
    show_comments: bool,
    sessions: SessionCookies,
}

impl ParserManager {
    pub fn new(config: ResolverConfig) -> Result<Self> {
        Self::with_sessions(config, SessionCookies::default())
    }

    /// 数据库中的扫码 Cookie 优先，普通配置仍是用户填写的手动值。
    #[cfg(test)]
    pub(crate) fn with_bili_session(config: ResolverConfig, cookie: Option<&str>) -> Result<Self> {
        Self::with_sessions(
            config,
            SessionCookies {
                bili: cookie.map(str::to_owned),
                ..SessionCookies::default()
            },
        )
    }

    pub(crate) fn with_sessions(config: ResolverConfig, sessions: SessionCookies) -> Result<Self> {
        let mut builder = Client::builder()
            .timeout(Duration::from_secs(15))
            .connect_timeout(Duration::from_secs(5))
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy();
        if !config.proxy.trim().is_empty() {
            builder = builder.proxy(reqwest::Proxy::all(config.proxy.trim())?);
        }
        let bili =
            BilibiliClient::new(sessions.bili.as_deref().or_else(|| {
                (!config.bili_cookie.is_empty()).then_some(config.bili_cookie.as_str())
            }))?
            .with_proxy((!config.proxy.is_empty()).then_some(config.proxy.as_str()))?
            .with_comments(config.render_cards && config.show_comments);
        let http = builder.build()?;
        let fetcher = PageFetcher::new(http.clone());
        let adapters = Platform::all()
            .iter()
            .map(|&platform| {
                // 不写通配分支：新增 Platform 时必须在这里决定依赖与构造方式。
                let adapter: Box<dyn PlatformAdapter> = match platform {
                    Platform::Bilibili => {
                        Box::new(BilibiliAdapter::new(bili.clone(), &config.bili_quality))
                    }
                    Platform::Douyin => Box::new(
                        DouyinAdapter::new(
                            DouyinClient::new(http.clone(), &config.douyin_cookie).with_quality(&config.douyin_quality),
                            fetcher.clone(),
                            config.douyin_cookie.clone(),
                        )
                        // 纯文字图集也需要公开统计和评论，不随 PNG 渲染开关关闭。
                        .with_engagement(true),
                    ),
                    Platform::Kuaishou => Box::new(KuaishouAdapter::new(fetcher.clone())),
                    Platform::Xiaohongshu => Box::new(
                        XiaohongshuAdapter::new(fetcher.clone(), config.xhs_cookie.clone())
                            .with_quality(&config.xhs_quality)
                            .with_comments_enabled(config.show_comments),
                    ),
                    Platform::Twitter => Box::new(TwitterAdapter::new(
                        http.clone(),
                        config.twitter_api_endpoint.clone(),
                    )),
                    Platform::Ncm => {
                        Box::new(NcmAdapter::new(http.clone(), config.ncm_api_host.clone())
                            .with_cookie(sessions.ncm.as_deref()))
                    }
                    Platform::QqMusic => Box::new(QqMusicAdapter::new(
                        http.clone(), sessions.qqmusic.clone().unwrap_or_default(),
                    )),
                    Platform::Kurobbs => Box::new(KurobbsAdapter::new(fetcher.clone())),
                    Platform::QqChannel => Box::new(QqChannelAdapter::new(fetcher.clone())),
                };
                debug_assert_eq!(
                    adapter.platform(),
                    platform,
                    "registry key and adapter disagree"
                );
                (platform, adapter)
            })
            .collect();
        Ok(Self {
            adapters: Arc::new(adapters),
            bili_live: bili,
            show_comments: config.show_comments,
            sessions,
        })
    }

    pub(crate) fn session_cookies(&self) -> SessionCookies {
        self.sessions.clone()
    }

    pub fn match_url(&self, text: &str) -> Option<MatchedUrl> {
        crate::resolver::url_policy::match_url(text)
    }

    pub async fn resolve(&self, matched: &MatchedUrl) -> Result<Resolved> {
        self.adapters
            .get(&matched.platform)
            .context("platform has no registered adapter")?
            .resolve(&matched.url)
            .await
            .map(|content| {
                content
                    .with_original_url(matched.url.as_str())
                    .with_comments_visible(self.show_comments)
            })
    }

    /// 直播订阅需要房间状态，使用独立查询入口。
    pub async fn live_room(&self, room_id: u64) -> Result<LiveInfo> {
        self.bili_live.live(room_id).await.map_err(Into::into)
    }

    #[cfg(test)]
    pub(crate) fn bili_cookie_for_test(&self) -> Option<&str> {
        self.bili_live.cookie_for_test()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registers_every_platform_with_its_own_adapter() {
        let manager = ParserManager::new(ResolverConfig::default()).unwrap();
        assert_eq!(manager.adapters.len(), Platform::all().len());
        for platform in Platform::all() {
            assert_eq!(manager.adapters[platform].platform(), *platform);
            assert!(platform.media_referer().starts_with("https://"));
        }
    }
}
