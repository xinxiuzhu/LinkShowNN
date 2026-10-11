//! 站点装配与链接路由。
//!
//! 配置只在装配时读取，各适配器只接收自己需要的字段。HTTP 客户端的 clone
//! 共享连接池；新增站点无需扩展任何公共运行上下文。

pub mod card;
pub mod content;
pub mod fetch;
pub mod media;
pub mod page;
pub mod site;
pub mod sites;
pub mod url_policy;

pub use card::{Card, CardComment, CardCommentEmote, CardMetadata, CardPoster};
pub use content::{Resolved, ResolvedContent};
pub use media::MediaInfo;
pub use site::Platform;

use std::{collections::HashMap, sync::Arc, time::Duration};

use anyhow::{Context, Result};
use nuono_plugin::PluginTaskScope;
use nuono_plugin::browser::PluginBrowser;
use reqwest::Client;

use crate::{
    default_config::ResolverConfig,
    resolver::fetch::PageFetcher,
    resolver::sites::{
        PlatformAdapter,
        bilibili::{BilibiliAdapter, Client as BilibiliClient, LiveInfo},
        douyin::DouyinAdapter,
        douyin::client::DouyinClient,
        kuaishou::KuaishouAdapter,
        kurobbs::KurobbsAdapter,
        ncm::NcmAdapter,
        qq_channel::QqChannelAdapter,
        twitter::TwitterAdapter,
        xiaohongshu::XiaohongshuAdapter,
    },
    resolver::url_policy::MatchedUrl,
};

#[derive(Clone)]
pub struct ParserManager {
    adapters: Arc<HashMap<Platform, Box<dyn PlatformAdapter>>>,
    bili_live: BilibiliClient,
}

impl ParserManager {
    pub fn new(
        config: ResolverConfig,
        browser: PluginBrowser,
        tasks: Arc<PluginTaskScope>,
    ) -> Result<Self> {
        let mut builder = Client::builder()
            .timeout(Duration::from_secs(15))
            .connect_timeout(Duration::from_secs(5))
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy();
        if !config.proxy.trim().is_empty() {
            builder = builder.proxy(reqwest::Proxy::all(config.proxy.trim())?);
        }
        let bili = BilibiliClient::new(
            (!config.bili_cookie.is_empty()).then_some(config.bili_cookie.as_str()),
        )?
        .with_proxy((!config.proxy.is_empty()).then_some(config.proxy.as_str()))?
        .with_comments(config.render_cards);
        let http = builder.build()?;
        let fetcher = PageFetcher::new(http.clone(), browser, &config)?;
        let anonymous_browser = config
            .douyin_anonymous_browser
            .then(|| {
                sites::douyin::anonymous_browser::AnonymousBrowser::new(
                    &config.proxy,
                    &config.douyin_browser_path,
                    tasks.clone(),
                )
            })
            .transpose()?;
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
                            DouyinClient::new(http.clone(), &config.douyin_cookie),
                            fetcher.clone(),
                            config.douyin_cookie.clone(),
                            anonymous_browser.clone(),
                        )
                        .with_engagement(config.render_cards),
                    ),
                    Platform::Kuaishou => Box::new(KuaishouAdapter::new(fetcher.clone())),
                    Platform::Xiaohongshu => Box::new(XiaohongshuAdapter::new(
                        fetcher.clone(),
                        config.xhs_cookie.clone(),
                    )),
                    Platform::Twitter => Box::new(TwitterAdapter::new(
                        http.clone(),
                        config.twitter_api_endpoint.clone(),
                    )),
                    Platform::Ncm => {
                        Box::new(NcmAdapter::new(http.clone(), config.ncm_api_host.clone()))
                    }
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
        })
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
    }

    /// 直播订阅需要房间状态，使用独立查询入口。
    pub async fn live_room(&self, room_id: u64) -> Result<LiveInfo> {
        self.bili_live.live(room_id).await.map_err(Into::into)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registers_every_platform_with_its_own_adapter() {
        let scope = Arc::new(nuono_plugin::PluginTaskScope::new(
            "linkshownn-registry-test",
        ));
        let browser = PluginBrowser::for_scope(scope.clone());
        let manager =
            ParserManager::new(ResolverConfig::default(), browser, scope.clone()).unwrap();
        assert_eq!(manager.adapters.len(), Platform::all().len());
        for platform in Platform::all() {
            assert_eq!(manager.adapters[platform].platform(), *platform);
            assert!(platform.media_referer().starts_with("https://"));
        }
    }
}
