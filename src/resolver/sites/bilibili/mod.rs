//! Bilibili 业务模块：链接识别、API 请求、响应映射与消息内容。

use anyhow::{Context, Result};
pub(crate) use client::Client;
pub(crate) use model::LiveInfo;
use streams::Quality;
use url::Url;
use urls::{Target, parse_url};

mod client;
pub(crate) mod comments;
pub mod content;
mod error;
mod mapping;
mod model;
mod streams;
mod urls;

#[cfg(test)]
mod tests;

use content::{FavoriteContent, LiveContent, RawContent, VideoContent};

use crate::{
    resolver::page::{collect_strings, find_string},
    resolver::sites::PlatformAdapter,
    resolver::{CardMetadata, Platform, Resolved},
};

pub struct BilibiliAdapter {
    client: Client,
    quality: Quality,
}

impl BilibiliAdapter {
    pub fn new(client: Client, configured_quality: &str) -> Self {
        Self {
            client,
            quality: quality(configured_quality),
        }
    }
}

#[async_trait::async_trait]
impl PlatformAdapter for BilibiliAdapter {
    fn platform(&self) -> Platform {
        Platform::Bilibili
    }

    async fn resolve(&self, url: &Url) -> Result<Resolved> {
        let target_url = if matches!(url.host_str(), Some("b23.tv" | "bili2233.cn")) {
            self.client.resolve_short_url(url).await?
        } else {
            url.clone()
        };
        let mut target = parse_url(target_url.as_str())?
            .context("unsupported Bilibili URL")?
            .target;
        inherit_source_page(&mut target, source_page(url));
        dispatch(&self.client, target, self.quality).await
    }
}

/// 短链本身可能带 `?p=`，展开后的目标页却回到第 1 P，此时沿用来源分 P。
fn source_page(url: &Url) -> u32 {
    parse_url(url.as_str())
        .ok()
        .flatten()
        .and_then(|value| match value.target {
            Target::Video { page, .. } | Target::Bangumi { page, .. } => Some(page),
            _ => None,
        })
        .unwrap_or(1)
}

fn inherit_source_page(target: &mut Target, source_page: u32) {
    if source_page <= 1 {
        return;
    }
    match target {
        Target::Video { page, .. } | Target::Bangumi { page, .. } if *page == 1 => {
            *page = source_page;
        }
        _ => {}
    }
}

fn quality(configured: &str) -> Quality {
    match configured {
        "1080P" => Quality::P1080,
        "480P" => Quality::P480,
        "360P" => Quality::P360,
        _ => Quality::P720,
    }
}

async fn dispatch(client: &Client, target: Target, quality: Quality) -> Result<Resolved> {
    Ok(match target {
        Target::Video { bvid, aid, page } => Resolved::new(VideoContent(
            client.video(bvid.as_deref(), aid, page, quality).await?,
        )),
        Target::Bangumi {
            ep_id,
            season_id,
            media_id,
            page,
        } => Resolved::new(VideoContent(
            client
                .bangumi(ep_id, season_id, media_id, page, quality)
                .await?,
        )),
        Target::Live { room_id } => Resolved::new(LiveContent(client.live(room_id).await?)),
        Target::Favorite { id } => Resolved::new(FavoriteContent(client.favorite(id).await?)),
        other => {
            let data = client.raw_target(&other).await?;
            let author = data.pointer("/item/modules/module_author");
            let source_url = match &other {
                Target::Opus { id } => Some(format!("https://www.bilibili.com/opus/{id}")),
                Target::Article { id } => Some(format!("https://www.bilibili.com/read/cv{id}")),
                _ => None,
            };
            Resolved::new(RawContent {
                title: find_string(&data, &["title", "season_title", "name"])
                    .unwrap_or_else(|| "B站内容".to_string()),
                content: collect_strings(
                    &data,
                    &["desc", "description", "summary", "content", "text"],
                    20,
                )
                .join("\n"),
                cover_url: find_string(&data, &["cover", "cover_url", "pic", "image_url"]),
                metadata: CardMetadata {
                    platform: Some(Platform::Bilibili),
                    author: author
                        .and_then(|author| author.get("name"))
                        .or_else(|| data.get("author_name"))
                        .and_then(serde_json::Value::as_str)
                        .filter(|name| !name.trim().is_empty())
                        .map(str::to_string),
                    avatar_url: author
                        .and_then(|author| author.get("face"))
                        .and_then(serde_json::Value::as_str)
                        .filter(|face| !face.trim().is_empty())
                        .map(mapping::normalize_url),
                    published_at: author
                        .and_then(|author| author.get("pub_ts"))
                        .and_then(serde_json::Value::as_i64)
                        .filter(|time| *time > 0),
                    source_url,
                    ..Default::default()
                },
            })
        }
    })
}

#[cfg(test)]
mod adapter_tests {
    use super::*;

    #[test]
    fn short_link_page_survives_expansion_to_first_page() {
        let mut target = Target::Video {
            bvid: Some("BV1xx411c7mD".into()),
            aid: None,
            page: 1,
        };
        inherit_source_page(&mut target, 3);
        assert!(matches!(target, Target::Video { page: 3, .. }));
    }

    #[test]
    fn explicit_target_page_wins_over_source_page() {
        let mut target = Target::Video {
            bvid: Some("BV1xx411c7mD".into()),
            aid: None,
            page: 5,
        };
        inherit_source_page(&mut target, 3);
        assert!(matches!(target, Target::Video { page: 5, .. }));
    }

    #[test]
    fn maps_configured_quality_with_720p_default() {
        assert_eq!(quality("1080P"), Quality::P1080);
        assert_eq!(quality("360P"), Quality::P360);
        assert_eq!(quality("unknown"), Quality::P720);
    }
}
