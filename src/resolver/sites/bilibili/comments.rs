//! 公开热评候选：独立匿名 HTTP 客户端，不继承视频解析的 Cookie。
//!
//! `reply/main` 的 mode=3 是平台热度排序，不是全站点赞排序。仅从本次可见
//! 根评论（包括置顶项）选出点赞最多的三条，展示层不能把它称作全站前三。

use super::error::Error;
use crate::resolver::{CardComment, CardCommentEmote};
use reqwest_012::{
    Client, Proxy, RequestBuilder,
    header::{REFERER, USER_AGENT},
};
use serde_json::Value;
use std::{collections::HashSet, time::Duration};

const ENDPOINT: &str = "https://api.bilibili.com/x/v2/reply/main";
const TIMEOUT: Duration = Duration::from_secs(3);
const MAX_RESPONSE_BYTES: usize = 1024 * 1024;
const MAX_CANDIDATES: usize = 50;
const MAX_COMMENT_CHARS: usize = 2048;
const MAX_COMMENT_EMOTES: usize = 8;
const MAX_EMOTE_TOKEN_CHARS: usize = 128;
const MAX_COMMENT_PICTURES: usize = 3;

#[derive(Clone)]
pub(super) struct AnonymousComments {
    http: Client,
}

impl AnonymousComments {
    /// 只接收代理配置，刻意不接收 Cookie 或已配置认证的 HTTP 客户端。
    pub(super) fn new(proxy: Option<&str>) -> Result<Self, Error> {
        let mut builder = Client::builder()
            .connect_timeout(Duration::from_secs(2))
            .timeout(TIMEOUT)
            .redirect(reqwest_012::redirect::Policy::none())
            .no_proxy();
        if let Some(proxy) = proxy.filter(|value| !value.trim().is_empty()) {
            builder = builder.proxy(Proxy::all(proxy.trim())?);
        }
        Ok(Self {
            http: builder.build()?,
        })
    }

    pub(super) fn request(&self, aid: u64) -> RequestBuilder {
        self.http
            .get(ENDPOINT)
            .query(&[
                ("type", 1),
                ("oid", aid),
                ("mode", 3),
                ("next", 0),
                ("ps", 20),
            ])
            .header(USER_AGENT, "Mozilla/5.0")
            .header(REFERER, "https://www.bilibili.com/")
    }

    /// 超时、风控、关评或响应异常都只隐藏评论，不让视频解析失败。
    pub(super) async fn fetch(&self, aid: u64) -> Vec<CardComment> {
        if aid == 0 {
            return Vec::new();
        }
        match tokio::time::timeout(TIMEOUT, self.fetch_inner(aid)).await {
            Ok(Ok(comments)) => comments,
            _ => {
                // 不记录底层错误：其中可能带有显式代理的 URL/认证信息。
                tracing::debug!("Bilibili anonymous comments unavailable; omitting comments");
                Vec::new()
            }
        }
    }

    async fn fetch_inner(&self, aid: u64) -> Result<Vec<CardComment>, Error> {
        let mut response = self.request(aid).send().await?.error_for_status()?;
        if response
            .content_length()
            .is_some_and(|size| size > MAX_RESPONSE_BYTES as u64)
        {
            return Err(Error::Schema("comment response exceeds byte limit".into()));
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await? {
            if chunk.len() > MAX_RESPONSE_BYTES.saturating_sub(bytes.len()) {
                return Err(Error::Schema("comment response exceeds byte limit".into()));
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok(map_comments(&serde_json::from_slice(&bytes)?))
    }
}

/// 接收完整 API 响应，供离线预览与线上路径共用相同筛选、去重、排序。
pub(crate) fn map_comments(response: &Value) -> Vec<CardComment> {
    if response.get("code").and_then(Value::as_i64) != Some(0) {
        return Vec::new();
    }
    let Some(data) = response.get("data") else {
        return Vec::new();
    };
    let array = |key| {
        data.get(key)
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
    };
    let pinned = data
        .get("top")
        .and_then(Value::as_object)
        .into_iter()
        .flat_map(|top| top.values());
    let mut comments = array("replies")
        .chain(array("top_replies"))
        .chain(pinned)
        .take(MAX_CANDIDATES)
        .filter_map(map_comment)
        .collect::<Vec<_>>();
    // Stable sort 保留同赞数的接口先后顺序；重复项保留较高的点赞快照。
    comments.sort_by(|left, right| right.like_count.cmp(&left.like_count));
    let mut ids = HashSet::new();
    comments.retain(|comment| ids.insert(comment.id.clone()));
    comments.truncate(3);
    comments
}

fn map_comment(reply: &Value) -> Option<CardComment> {
    if reply.get("root").and_then(Value::as_u64).unwrap_or(0) != 0
        || reply.get("state").and_then(Value::as_i64).unwrap_or(0) != 0
    {
        return None;
    }
    let id = reply.get("rpid").and_then(numeric_id)?;
    let like_count = reply.get("like").and_then(Value::as_u64)?;
    let author = reply
        .pointer("/member/uname")
        .and_then(Value::as_str)?
        .trim();
    let text = reply
        .pointer("/content/message")
        .and_then(Value::as_str)?
        .trim();
    if author.is_empty() {
        return None;
    }
    let text = bounded_text(text, MAX_COMMENT_CHARS);
    let emotes = map_emotes(reply.pointer("/content/emote"), &text);
    let mut picture_urls = HashSet::new();
    let pictures = reply
        .pointer("/content/pictures")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|picture| picture.get("img_src").and_then(Value::as_str))
        .filter_map(image_url)
        .filter(|source| picture_urls.insert(source.clone()))
        .take(MAX_COMMENT_PICTURES)
        .collect::<Vec<_>>();
    if text.is_empty() && pictures.is_empty() {
        return None;
    }
    Some(CardComment {
        id,
        author: bounded_text(author, 64),
        author_id: reply.pointer("/member/mid").and_then(numeric_id),
        avatar_url: reply
            .pointer("/member/avatar")
            .and_then(Value::as_str)
            .and_then(image_url),
        text,
        emotes,
        pictures,
        like_count,
        like_count_display: None,
        published_at: reply
            .get("ctime")
            .and_then(Value::as_i64)
            .filter(|value| *value > 0),
    })
}

fn map_emotes(value: Option<&Value>, text: &str) -> Vec<CardCommentEmote> {
    let mut emotes = value
        .and_then(Value::as_object)
        .into_iter()
        .flat_map(|values| values.iter())
        .filter_map(|(token, emote)| {
            if token.trim().is_empty()
                || token.chars().count() > MAX_EMOTE_TOKEN_CHARS
                || token.chars().any(char::is_control)
                || emote
                    .get("state")
                    .is_some_and(|state| state.as_u64() != Some(0))
            {
                return None;
            }
            // 使用接口对象的键作精确标记，不依赖可能缺失或不一致的 text 字段。
            let offset = text.find(token)?;
            let url = emote
                .get("url")
                .and_then(Value::as_str)
                .and_then(image_url)?;
            Some((
                offset,
                CardCommentEmote {
                    text: token.clone(),
                    url,
                    large: emote.pointer("/meta/size").and_then(Value::as_u64) == Some(2),
                },
            ))
        })
        .collect::<Vec<_>>();
    // JSON 对象的键天然去重；按正文出现顺序截取，避免对象的字典序影响取舍。
    emotes.sort_by(|(left_offset, left), (right_offset, right)| {
        left_offset
            .cmp(right_offset)
            .then_with(|| right.text.len().cmp(&left.text.len()))
            .then_with(|| left.text.cmp(&right.text))
    });
    emotes
        .into_iter()
        .take(MAX_COMMENT_EMOTES)
        .map(|(_, emote)| emote)
        .collect()
}

fn numeric_id(value: &Value) -> Option<String> {
    value
        .as_u64()
        .or_else(|| value.as_str()?.parse().ok())
        .filter(|id| *id > 0)
        .map(|id| id.to_string())
}

fn image_url(value: &str) -> Option<String> {
    if value.len() > 2048 {
        return None;
    }
    let value = super::mapping::normalize_url(value.trim());
    let url = url::Url::parse(&value).ok()?;
    (matches!(url.scheme(), "http" | "https")
        && url.host_str().is_some()
        && url.username().is_empty()
        && url.password().is_none())
    .then_some(value)
}

fn bounded_text(value: &str, limit: usize) -> String {
    let mut chars = value.chars();
    let mut text: String = chars.by_ref().take(limit).collect();
    if chars.next().is_some() {
        text.push('…');
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn reply(id: u64, likes: u64) -> Value {
        json!({"rpid":id, "root":0, "state":0, "like":likes, "ctime":1790915390,
            "member":{"mid":"12345", "uname":"评论者", "avatar":"//i2.hdslb.com/avatar.jpg"},
            "content":{"message":"这条评论很有意思[doge]"}})
    }

    #[test]
    fn ranks_visible_roots_by_likes_not_api_order_and_deduplicates_pinned_items() {
        let mut child = reply(6, 100_000);
        child["root"] = json!(1);
        let result = map_comments(&json!({"code":0,"data":{
            "replies":[reply(1,217),reply(2,58),reply(3,366),reply(4,3169),child],
            "top_replies":[reply(4,3169),reply(5,2696)], "top":{"upper":reply(7,2489)}
        }}));
        assert_eq!(
            result
                .iter()
                .map(|item| (item.id.as_str(), item.like_count))
                .collect::<Vec<_>>(),
            [("4", 3169), ("5", 2696), ("7", 2489)]
        );
        assert_eq!(result[0].author_id.as_deref(), Some("12345"));
        assert_eq!(
            result[0].avatar_url.as_deref(),
            Some("https://i2.hdslb.com/avatar.jpg")
        );
        assert_eq!(result[0].published_at, Some(1790915390));
    }

    #[test]
    fn keeps_real_zero_counts_but_skips_missing_counts_hidden_or_empty_comments() {
        let mut missing = reply(2, 1);
        missing.as_object_mut().unwrap().remove("like");
        let mut hidden = reply(3, 100);
        hidden["state"] = json!(-1);
        let mut empty = reply(4, 100);
        empty["content"]["message"] = json!("  ");
        let response = json!({"code":0,"data":{"replies":[reply(1,0),missing,hidden,empty]}});
        assert_eq!(map_comments(&response).len(), 1);
        assert_eq!(map_comments(&response)[0].like_count, 0);
        for data in [
            json!({"code":-101,"data":response["data"]}),
            json!({"code":0,"data":null}),
        ] {
            assert!(map_comments(&data).is_empty());
        }
    }

    #[test]
    fn bounds_candidates_and_text_and_rejects_non_web_or_authenticated_avatars() {
        let mut replies = (1..=51).map(|id| reply(id, id)).collect::<Vec<_>>();
        replies[49]["content"]["message"] = json!("好".repeat(10_000));
        replies[49]["member"]["avatar"] = json!("https://user:secret@cdn.example/a.png");
        let result = map_comments(&json!({"code":0,"data":{"replies":replies}}));
        assert_eq!(result[0].id, "50");
        assert_eq!(result[0].text.chars().count(), MAX_COMMENT_CHARS + 1);
        assert!(result[0].text.ends_with('…'));
        assert!(result[0].avatar_url.is_none());
        assert!(image_url("file:///private/a.png").is_none());
    }

    #[test]
    fn maps_public_emotes_in_text_order_including_paid_packs() {
        let mut value = reply(1, 10);
        value["content"] = json!({
            "message":"先[Mygo表情包_Love]再[笑哭]，又[笑哭][无效][认证][本地]",
            "emote":{
                "[笑哭]":{"url":"//i0.hdslb.com/emote.png","state":0,"type":1,"meta":{"size":1}},
                "[Mygo表情包_Love]":{"url":"http://i0.hdslb.com/paid.png","state":0,"type":3,"meta":{"size":2}},
                "[未出现]":{"url":"https://i0.hdslb.com/unused.png","state":0},
                "[无效]":{"url":"https://i0.hdslb.com/hidden.png","state":1},
                "[认证]":{"url":"https://user:secret@cdn.example/auth.png","state":0},
                "[本地]":{"url":"file:///private/emote.png","state":0}
            }
        });
        let comment = map_comment(&value).unwrap();
        assert_eq!(
            comment.emotes,
            [
                CardCommentEmote {
                    text: "[Mygo表情包_Love]".into(),
                    url: "http://i0.hdslb.com/paid.png".into(),
                    large: true,
                },
                CardCommentEmote {
                    text: "[笑哭]".into(),
                    url: "https://i0.hdslb.com/emote.png".into(),
                    large: false,
                },
            ]
        );
    }

    #[test]
    fn bounds_emotes_and_only_maps_complete_tokens_in_retained_text() {
        let mut value = reply(1, 10);
        let tokens = (0..12).rev().map(|index| format!("[表情{index}]"));
        let tokens = tokens.collect::<Vec<_>>();
        value["content"]["message"] = json!(tokens.join(""));
        value["content"]["emote"] = Value::Object(
            tokens
                .iter()
                .map(|token| (token.clone(), json!({"url":"https://cdn.example/e.png"})))
                .collect(),
        );
        let comment = map_comment(&value).unwrap();
        assert_eq!(comment.emotes.len(), MAX_COMMENT_EMOTES);
        assert_eq!(
            comment
                .emotes
                .iter()
                .map(|emote| &emote.text)
                .collect::<Vec<_>>(),
            tokens.iter().take(MAX_COMMENT_EMOTES).collect::<Vec<_>>()
        );

        let long_token = format!("[{}]", "长".repeat(MAX_EMOTE_TOKEN_CHARS));
        let message = format!("{long_token}[坏状态][换\n行]{}[末尾]", "好".repeat(3000));
        value["content"]["message"] = json!(message);
        value["content"]["emote"] = json!({
            long_token:{"url":"https://cdn.example/long.png"},
            "[坏状态]":{"url":"https://cdn.example/bad.png","state":"invalid"},
            "[换\n行]":{"url":"https://cdn.example/control.png"},
            "[末尾]":{"url":"https://cdn.example/tail.png"}
        });
        assert!(map_comment(&value).unwrap().emotes.is_empty());

        value["content"]["message"] =
            json!(format!("{}[末尾]", "好".repeat(MAX_COMMENT_CHARS - 1)));
        assert!(map_comment(&value).unwrap().emotes.is_empty());
    }

    #[test]
    fn maps_at_most_three_unique_public_comment_pictures() {
        let mut value = reply(1, 10);
        value["content"]["pictures"] = json!([
            {"img_src":"//i0.hdslb.com/one.jpg"},
            {"img_src":"https://i0.hdslb.com/one.jpg"},
            {"img_src":"file:///private/no.jpg"},
            {"img_src":"https://user:secret@cdn.example/no.jpg"},
            {"img_src":"https://i0.hdslb.com/two.jpg"},
            {"img_src":"https://i0.hdslb.com/three.jpg"},
            {"img_src":"https://i0.hdslb.com/four.jpg"}
        ]);
        assert_eq!(
            map_comment(&value).unwrap().pictures,
            [
                "https://i0.hdslb.com/one.jpg",
                "https://i0.hdslb.com/two.jpg",
                "https://i0.hdslb.com/three.jpg",
            ]
        );
    }

    #[test]
    fn preserves_image_only_comments_but_rejects_empty_or_unnamed_ones() {
        let mut value = reply(1, 10);
        value["content"]["message"] = json!(" \n ");
        value["content"]["pictures"] = json!([
            {"img_src":"https://i0.hdslb.com/comment.jpg"}
        ]);
        let comment = map_comment(&value).unwrap();
        assert!(comment.text.is_empty());
        assert_eq!(comment.pictures, ["https://i0.hdslb.com/comment.jpg"]);

        value["member"]["uname"] = json!(" ");
        assert!(map_comment(&value).is_none());
        value["member"]["uname"] = json!("有名字");
        value["content"]["pictures"] = json!([
            {"img_src":"file:///private/comment.jpg"}
        ]);
        assert!(map_comment(&value).is_none());
    }
}
