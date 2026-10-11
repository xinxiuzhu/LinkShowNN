//! 分享结果的图文渲染。网络准备在异步任务中进行，解码和绘图由 SDK 配额管理。
mod assets;
mod raster;

use std::{
    sync::{Arc, LazyLock},
    time::Duration,
};

use anyhow::{Result, ensure};
use nuono_plugin::PluginTaskScope;
use tokio::sync::Semaphore;

use crate::resolver::Card;
use assets::AssetFetcher;

#[derive(Clone)]
pub(crate) struct CardRenderer {
    assets: AssetFetcher,
    tasks: Arc<PluginTaskScope>,
}

impl CardRenderer {
    pub(crate) fn new(proxy: &str, tasks: Arc<PluginTaskScope>) -> Result<Self> {
        Ok(Self {
            assets: AssetFetcher::new(proxy)?,
            tasks,
        })
    }

    pub(crate) fn is_closed(&self) -> bool {
        self.tasks.is_closed()
    }

    pub(crate) async fn render(&self, card: Card) -> Result<Vec<u8>> {
        ensure!(!self.is_closed(), "插件已停用");
        // 限制整个准备/绘图阶段，已下载的图片不会无限堆进 SDK 绘图队列。
        static ADMITTED: LazyLock<Arc<Semaphore>> = LazyLock::new(|| Arc::new(Semaphore::new(11)));
        static ACTIVE: LazyLock<Arc<Semaphore>> = LazyLock::new(|| Arc::new(Semaphore::new(3)));
        let admitted = ADMITTED
            .clone()
            .try_acquire_owned()
            .map_err(|_| anyhow::anyhow!("图文卡片队列已满"))?;
        // 准备素材也登记到作用域，插件停用时会取消请求和等待中的下载。
        let fetcher = self.assets.clone();
        let prepared_card = card.clone();
        let (mut sender, receiver) = tokio::sync::oneshot::channel();
        self.tasks.spawn("url-card-assets", async move {
            tokio::select! {
                _ = sender.closed() => {}
                result = async {
                    let permit = tokio::time::timeout(Duration::from_secs(10), ACTIVE.clone().acquire_owned())
                        .await.map_err(|_| anyhow::anyhow!("图文卡片等待超时"))?
                        .map_err(|_| anyhow::anyhow!("图文卡片队列已关闭"))?;
                    Ok::<_, anyhow::Error>((admitted, permit, fetcher.prepare(&prepared_card).await?))
                } => { let _ = sender.send(result); }
            }
        });
        let (admitted, permit, assets) = receiver
            .await
            .map_err(|_| anyhow::anyhow!("卡片素材准备已取消"))??;
        let result = self
            .tasks
            .render("url-card", move || {
                let (_admitted, _permit) = (admitted, permit);
                raster::render(&card, &assets)
            })
            .await;
        ensure!(!self.is_closed(), "插件已停用");
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nuono_plugin::PluginResponse;

    /// Explicit opt-in end-to-end test: fresh anonymous browser, real resolver,
    /// public media requests and the same poster renderer used by the plugin.
    #[tokio::test]
    #[ignore = "contacts Douyin and launches an isolated local browser; needs NUONO_DOUYIN_LIVE_OUTPUT"]
    async fn anonymous_douyin_live_preview() {
        use crate::{default_config::ResolverConfig, resolver::ParserManager};
        let output = std::env::var_os("NUONO_DOUYIN_LIVE_OUTPUT")
            .expect("set NUONO_DOUYIN_LIVE_OUTPUT to a preview directory");
        let output = std::path::PathBuf::from(output);
        let scope = Arc::new(PluginTaskScope::new("douyin-anonymous-live-preview"));
        let result = async {
            // Never read the user's configuration or existing browser profile.
            let config = ResolverConfig {
                proxy: std::env::var("NUONO_DOUYIN_LIVE_PROXY").unwrap_or_default(),
                obscura_enabled: false,
                ..ResolverConfig::default()
            };
            let parser = ParserManager::new(
                config.clone(),
                nuono_plugin::browser::PluginBrowser::for_scope(scope.clone()),
                scope.clone(),
            )?;
            let matched = parser
                .match_url("https://v.douyin.com/B4nEt6lLK2w/")
                .expect("Douyin short link must match");
            let resolved = parser.resolve(&matched).await?;
            if std::env::var("NUONO_DOUYIN_LIVE_DOWNLOAD").as_deref() == Ok("1") {
                let candidate = resolved
                    .download_candidate(config.video_duration_maximum)
                    .ok_or_else(|| anyhow::anyhow!("missing anonymous video download candidate"))?;
                let downloads = crate::download::DownloadManager::new(
                    Some(&config.proxy),
                    1,
                    1,
                    config.video_max_mb,
                    output.join("media"),
                )?;
                let media = downloads
                    .try_reserve()
                    .unwrap()
                    .download(&candidate)
                    .await?;
                let bytes = tokio::fs::read(&media.path).await?;
                anyhow::ensure!(
                    bytes.len() > 1024 && bytes.get(4..8) == Some(b"ftyp"),
                    "anonymous download did not produce an MP4 file"
                );
                println!(
                    "anonymous MP4 downloaded: bytes={}, path={}",
                    bytes.len(),
                    media.path.display()
                );
            }
            let card = resolved.card();
            anyhow::ensure!(
                card.metadata().author.as_deref() == Some("鸣潮"),
                "wrong author"
            );
            anyhow::ensure!(
                card.text().contains("共鸣者档案前瞻 | 莉莉"),
                "missing real work text"
            );
            anyhow::ensure!(
                card.metadata().avatar_url.is_some(),
                "missing author avatar"
            );
            anyhow::ensure!(
                card.metadata().video_cover_url.is_some(),
                "missing work cover"
            );
            let comments = &card.metadata().comments;
            anyhow::ensure!(
                comments.len() == 3,
                "expected three anonymous visible comments"
            );
            anyhow::ensure!(
                comments
                    .windows(2)
                    .all(|pair| pair[0].like_count >= pair[1].like_count),
                "comments are not sorted by likes"
            );
            let poster = card.metadata().poster.as_ref().unwrap();
            anyhow::ensure!(
                poster.like_count.is_some() || poster.stats_display.likes.is_some(),
                "missing visible video statistics"
            );
            tokio::fs::create_dir_all(&output).await?;
            tokio::fs::write(
                output.join("public-comments.json"),
                serde_json::to_vec_pretty(comments)?,
            )
            .await?;
            println!(
                "comments={}, avatars={}, attached_pictures={}, inline_emotes={}",
                comments.len(),
                comments.iter().filter(|c| c.avatar_url.is_some()).count(),
                comments.iter().map(|c| c.pictures.len()).sum::<usize>(),
                comments.iter().map(|c| c.emotes.len()).sum::<usize>()
            );
            let png = CardRenderer::new(&config.proxy, scope.clone())?
                .render(card.clone())
                .await?;
            tokio::fs::create_dir_all(&output).await?;
            tokio::fs::write(output.join("douyin-anonymous-poster.png"), png).await?;
            // Keep only public content; no complete DOM, browser state or tokens.
            tokio::fs::write(output.join("public-result.txt"), card.text()).await?;
            println!("anonymous Douyin live preview: author=鸣潮, title/cover/avatar present");
            Ok::<(), anyhow::Error>(())
        }
        .await;
        scope.shutdown().await;
        result.unwrap();
    }

    #[tokio::test]
    async fn renders_through_host_scope_and_replies_with_one_inline_image() {
        let scope = Arc::new(PluginTaskScope::new("card-render-test"));
        let renderer = CardRenderer::new("", scope.clone()).unwrap();
        let png = renderer
            .render(Card::new().line("图文卡片测试"))
            .await
            .unwrap();
        let decoded = image::load_from_memory(&png).unwrap();
        assert_eq!(decoded.width(), 800);
        let PluginResponse::Reply(reply) = crate::delivery::image_reply(png).await.unwrap() else {
            panic!("expected a reply");
        };
        assert_eq!(reply["reply_current"], true);
        let segments = reply["params"]["message"].as_array().unwrap();
        assert_eq!(segments.len(), 1);
        assert_eq!(segments[0]["type"], "image");
        assert!(
            segments[0]["data"]["file"]
                .as_str()
                .unwrap()
                .starts_with("base64://")
        );
        scope.shutdown().await;
        assert!(
            renderer
                .render(Card::new().line("停用后不绘图"))
                .await
                .is_err()
        );
    }

    /// Reproduce a previously captured anonymous page with the production DOM
    /// mapping and asset renderer; useful when live top comments have no images.
    #[tokio::test]
    #[ignore = "downloads public comment images from NUONO_DOUYIN_CAPTURED_HTML; needs NUONO_DOUYIN_LIVE_OUTPUT"]
    async fn captured_douyin_comment_media_preview() {
        use crate::resolver::{
            ResolvedContent,
            sites::douyin::{client::AwemeRef, desktop, engagement::DouyinContent},
        };
        let source = url::Url::parse("https://www.douyin.com/video/7693165612066999571").unwrap();
        let expected = AwemeRef {
            id: "7693165612066999571".into(),
            is_note: false,
        };
        let html = std::fs::read_to_string(
            std::env::var_os("NUONO_DOUYIN_CAPTURED_HTML").expect("set captured HTML path"),
        )
        .unwrap();
        let mut content =
            DouyinContent::new(desktop::map_desktop_page(&html, &expected, &source).unwrap());
        content.engagement = desktop::map_engagement(&html, &expected, &source).unwrap();
        let card = content.card();
        assert_eq!(card.metadata().comments.len(), 3);
        assert!(
            card.metadata()
                .comments
                .iter()
                .any(|c| !c.pictures.is_empty())
        );
        assert!(
            card.metadata()
                .comments
                .iter()
                .any(|c| !c.emotes.is_empty())
        );
        let scope = Arc::new(PluginTaskScope::new("douyin-captured-media-preview"));
        let result = CardRenderer::new("", scope.clone())
            .unwrap()
            .render(card)
            .await;
        scope.shutdown().await;
        let output = std::path::PathBuf::from(
            std::env::var_os("NUONO_DOUYIN_LIVE_OUTPUT").expect("set preview directory"),
        );
        std::fs::create_dir_all(&output).unwrap();
        std::fs::write(output.join("douyin-comment-media.png"), result.unwrap()).unwrap();
    }
}
