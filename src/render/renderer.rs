//! Lifecycle-aware preparation and composition of sharing cards.

use std::{
    sync::{Arc, LazyLock},
    time::Duration,
};

use anyhow::{Result, ensure};
use nuono_plugin::PluginTaskScope;
use tokio::sync::Semaphore;

use crate::download::images::ImageFetcher;
use crate::resolver::Card;

#[derive(Clone)]
pub(crate) struct CardRenderer {
    assets: ImageFetcher,
    tasks: Arc<PluginTaskScope>,
}

impl CardRenderer {
    pub(crate) fn new(proxy: &str, tasks: Arc<PluginTaskScope>) -> Result<Self> {
        Ok(Self {
            assets: ImageFetcher::new(proxy)?,
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
                super::compose(&card, &assets)
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

    /// Anonymous music metadata and cover through the production rendering path.
    #[tokio::test]
    #[ignore = "contacts music platforms; needs NUONO_MUSIC_LIVE_URL and NUONO_MUSIC_LIVE_OUTPUT"]
    async fn public_music_live_preview() {
        use crate::{
            config::ResolverConfig,
            resolver::{ParserManager, Platform},
        };
        let source = std::env::var("NUONO_MUSIC_LIVE_URL").expect("set public song URL");
        let output = std::path::PathBuf::from(
            std::env::var_os("NUONO_MUSIC_LIVE_OUTPUT").expect("set preview directory"),
        );
        let parser = ParserManager::new(ResolverConfig::default()).unwrap();
        let matched = parser.match_url(&source).expect("song link must match");
        assert!(matches!(
            matched.platform,
            Platform::Ncm | Platform::QqMusic
        ));
        let card = parser.resolve(&matched).await.unwrap().card();
        assert!(card.metadata().poster.is_some());
        assert!(card.metadata().video_cover_url.is_none());
        assert!(!card.is_gallery());
        let assets = ImageFetcher::new("").unwrap().prepare(&card).await.unwrap();
        let png = crate::render::compose(&card, &assets).unwrap();
        tokio::fs::create_dir_all(&output).await.unwrap();
        tokio::fs::write(
            output.join(format!("{}-music-poster.png", matched.platform.as_str())),
            png,
        )
        .await
        .unwrap();
        println!("{}", card.body_text());
    }

    /// Explicit anonymous HTTP/asset/render check; never loads local credentials.
    #[tokio::test]
    #[ignore = "contacts Xiaohongshu; needs NUONO_XHS_LIVE_OUTPUT"]
    async fn public_xiaohongshu_live_preview() {
        use crate::{config::ResolverConfig, resolver::ParserManager};
        let output = std::path::PathBuf::from(
            std::env::var_os("NUONO_XHS_LIVE_OUTPUT").expect("set preview directory"),
        );
        let scope = Arc::new(PluginTaskScope::new("xhs-anonymous-live-preview"));
        let result = async {
            let proxy = std::env::var("NUONO_XHS_LIVE_PROXY").unwrap_or_default();
            let config = ResolverConfig {
                proxy: proxy.clone(),
                ..ResolverConfig::default()
            };
            let parser = ParserManager::new(config)?;
            let source = std::env::var("NUONO_XHS_LIVE_URL")
                .unwrap_or_else(|_| "https://xhslink.cn/o/6Ji5ENKJVty".into());
            let matched = parser.match_url(&source).expect("XHS link must match");
            let card = parser.resolve(&matched).await?.card();
            anyhow::ensure!(!card.is_gallery(), "preview expects a video poster");
            let poster = card.metadata().poster.as_ref().expect("video poster");
            anyhow::ensure!(
                poster.like_count.is_some() && poster.comment_count.is_some(),
                "missing public note statistics"
            );
            let comments = &card.metadata().comments;
            anyhow::ensure!(comments.len() == 3, "expected three visible comments");
            anyhow::ensure!(
                comments
                    .windows(2)
                    .all(|pair| pair[0].like_count >= pair[1].like_count),
                "comments are not sorted by likes"
            );
            tokio::fs::create_dir_all(&output).await?;
            tokio::fs::write(
                output.join("public-comments.json"),
                serde_json::to_vec_pretty(comments)?,
            )
            .await?;
            let renderer = CardRenderer::new(&proxy, scope.clone())?;
            let assets = renderer.assets.prepare(&card).await?;
            println!(
                "comments={}, loaded_avatars={}, loaded_emotes={}, loaded_pictures={}",
                comments.len(),
                comments
                    .iter()
                    .filter(|comment| comment
                        .avatar_url
                        .as_ref()
                        .is_some_and(|url| assets.contains_key(url)))
                    .count(),
                comments
                    .iter()
                    .flat_map(|comment| &comment.emotes)
                    .filter(|emote| assets.contains_key(&emote.url))
                    .count(),
                comments
                    .iter()
                    .flat_map(|comment| &comment.pictures)
                    .filter(|url| assets.contains_key(*url))
                    .count()
            );
            // Use the same lifecycle-managed composer without fetching assets twice.
            let png = scope
                .render("xhs-preview", move || super::super::compose(&card, &assets))
                .await?;
            tokio::fs::write(output.join("xiaohongshu-anonymous-poster.png"), png).await?;
            Ok::<(), anyhow::Error>(())
        }
        .await;
        scope.shutdown().await;
        result.unwrap();
    }

    /// Explicit opt-in end-to-end test: anonymous HTTP/API, real resolver,
    /// public media requests and the same poster renderer used by the plugin.
    #[tokio::test]
    #[ignore = "contacts Douyin using HTTP/API; needs NUONO_DOUYIN_LIVE_OUTPUT"]
    async fn public_douyin_live_preview() {
        use crate::{config::ResolverConfig, resolver::ParserManager};
        let output = std::env::var_os("NUONO_DOUYIN_LIVE_OUTPUT")
            .expect("set NUONO_DOUYIN_LIVE_OUTPUT to a preview directory");
        let output = std::path::PathBuf::from(output);
        let scope = Arc::new(PluginTaskScope::new("douyin-anonymous-live-preview"));
        let result = async {
            // Never read the user's configuration or stored credentials.
            let config = ResolverConfig {
                proxy: std::env::var("NUONO_DOUYIN_LIVE_PROXY").unwrap_or_default(),
                ..ResolverConfig::default()
            };
            let parser = ParserManager::new(config.clone())?;
            let source = std::env::var("NUONO_DOUYIN_LIVE_URL")
                .unwrap_or_else(|_| "https://v.douyin.com/B4nEt6lLK2w/".into());
            let expected_author =
                std::env::var("NUONO_DOUYIN_LIVE_AUTHOR").unwrap_or_else(|_| "鸣潮".into());
            let expected_text = std::env::var("NUONO_DOUYIN_LIVE_TEXT")
                .unwrap_or_else(|_| "共鸣者档案前瞻 | 莉莉".into());
            let matched = parser
                .match_url(&source)
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
                card.metadata().author.as_deref() == Some(expected_author.as_str()),
                "wrong author"
            );
            anyhow::ensure!(
                card.text().contains(&expected_text),
                "missing real work text"
            );
            anyhow::ensure!(
                card.metadata().avatar_url.is_some(),
                "missing author avatar"
            );
            anyhow::ensure!(card.image_urls().next().is_some(), "missing work cover");
            let comments = &card.metadata().comments;
            anyhow::ensure!(comments.len() <= 3, "too many public comment candidates");
            anyhow::ensure!(
                comments
                    .windows(2)
                    .all(|pair| pair[0].like_count >= pair[1].like_count),
                "comments are not sorted by likes"
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
            if card.is_gallery() {
                crate::download::gallery::live_test::preview(
                    card,
                    &config.proxy,
                    scope.clone(),
                    &output,
                )
                .await?;
                return Ok::<(), anyhow::Error>(());
            }
            let png = CardRenderer::new(&config.proxy, scope.clone())?
                .render(card.clone())
                .await?;
            tokio::fs::create_dir_all(&output).await?;
            tokio::fs::write(output.join("douyin-anonymous-poster.png"), png).await?;
            // Keep only public content; no complete DOM, browser state or tokens.
            tokio::fs::write(output.join("public-result.txt"), card.text()).await?;
            println!(
                "anonymous Douyin live preview: author={}, images={}, title/cover/avatar present",
                expected_author,
                card.image_urls().count()
            );
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
        let decoded = ::image::load_from_memory(&png).unwrap();
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
            douyin::{client::AwemeRef, desktop, engagement::DouyinContent},
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
