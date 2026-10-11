use serde::{Deserialize, Serialize};
use serde_json::Value;

/// 与 bpi-rs 共用视频主体解码，额外保留其简化模型未包含的发布日期。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct VideoViewPayload {
    #[serde(flatten)]
    pub view: bpi_rs::video::VideoView,
    #[serde(default)]
    pub pubdate: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct VideoPage {
    pub cid: Option<u64>,
    #[serde(default)]
    pub page: u32,
    #[serde(default)]
    pub part: String,
    #[serde(default)]
    pub duration: u64,
    #[serde(default)]
    pub first_frame: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct VideoInfo {
    pub bvid: Option<String>,
    pub aid: Option<u64>,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub cover_url: String,
    #[serde(default)]
    pub duration: u64,
    #[serde(default)]
    pub selected_page: Option<VideoPage>,
    #[serde(default)]
    pub is_dash: bool,
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub author_mid: Option<u64>,
    #[serde(default)]
    pub avatar_url: Option<String>,
    #[serde(default)]
    pub published_at: Option<i64>,
    #[serde(default)]
    pub view_count: Option<u64>,
    #[serde(default)]
    pub danmaku_count: Option<u64>,
    #[serde(default)]
    pub comment_count: Option<u64>,
    #[serde(default)]
    pub like_count: Option<u64>,
    #[serde(default)]
    pub coin_count: Option<u64>,
    #[serde(default)]
    pub favorite_count: Option<u64>,
    #[serde(default)]
    pub share_count: Option<u64>,
    /// 评论只供当前海报展示，不写入原始视频 API 快照。
    #[serde(skip)]
    pub comments: Vec<crate::resolver::card::CardComment>,
    #[serde(default)]
    pub pages: Vec<VideoPage>,
    #[serde(default)]
    pub video_urls: Vec<String>,
    #[serde(default)]
    pub audio_urls: Vec<String>,
    #[serde(default)]
    pub raw: Value,
}
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LiveInfo {
    pub room_id: u64,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub cover_url: Option<String>,
    #[serde(default)]
    pub live_status: u8,
    #[serde(default)]
    pub online: Option<u64>,
    #[serde(default)]
    pub area: Option<String>,
    #[serde(default)]
    pub anchor: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FavoriteItem {
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub cover_url: String,
    #[serde(default)]
    pub intro: String,
    #[serde(default)]
    pub bvid: Option<String>,
    #[serde(default)]
    pub aid: Option<u64>,
}
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FavoriteInfo {
    pub id: u64,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub owner: Option<String>,
    #[serde(default)]
    pub total: u64,
    #[serde(default)]
    pub items: Vec<FavoriteItem>,
}
