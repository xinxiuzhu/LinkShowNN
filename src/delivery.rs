//! 卡片与下载媒体的消息编码。目标选择和投递由宿主 SDK 负责。

use std::path::Path;
use std::{sync::Arc, time::Duration};

use anyhow::Context;
use nuono_plugin::{
    Bot, Event, ImageSource, MessageSegment, PluginResponse, RuntimeMessageTarget, SendableMessage,
};
use serde_json::{Value, json};

use crate::{download::DownloadCandidate, download::MediaKind, resolver::Card};

pub fn card_message(card: &Card) -> Vec<MessageSegment> {
    let mut segments = Vec::new();
    let text = card.text();
    let text = if text.chars().count() > 4000 {
        format!(
            "{}\n…内容较长，请查看原链接",
            text.chars().take(4000).collect::<String>()
        )
    } else {
        text
    };
    if !text.is_empty() {
        segments.push(MessageSegment::text(text));
    }
    segments.extend(card.image_urls().take(10).map(MessageSegment::image));
    if card.image_urls().count() > 10 {
        segments.push(MessageSegment::text("其余图片请查看原链接。"));
    }
    segments
}

/// 通过 SDK 字节图片发送，不依赖聊天客户端访问宿主的本地文件目录。
#[cfg(test)]
pub async fn image_reply(bytes: Vec<u8>) -> anyhow::Result<PluginResponse> {
    Ok(reply_message(image_message(bytes).await?))
}

pub async fn image_message(bytes: Vec<u8>) -> anyhow::Result<SendableMessage> {
    let image = ImageSource::bytes(bytes).into_segment().await?;
    Ok(vec![image].into())
}

pub fn reply_message(message: SendableMessage) -> PluginResponse {
    PluginResponse::Reply(json!({
        "action": "send_msg",
        "reply_current": true,
        "params": {"message": message.to_json()},
    }))
}

/// A reply destination owned by this request, including passive-reply context.
/// The SDK still encodes the platform target and preserves native string IDs.
#[derive(Clone)]
pub(crate) struct ReplyTarget {
    bot: Arc<Bot>,
    target: RuntimeMessageTarget,
    context: serde_json::Map<String, Value>,
}

impl ReplyTarget {
    pub(crate) fn new(bot: Arc<Bot>, event: &Event<'_>) -> anyhow::Result<Self> {
        let target = event.message_target()?;
        target.validate()?;
        let context = [
            "message_type",
            "user_id",
            "group_id",
            "channel_id",
            "guild_id",
            "chat_id",
            "openid",
            "group_openid",
            "event_id",
            "qq_target_type",
            "message_id",
            "reply_context",
        ]
        .into_iter()
        .filter_map(|key| {
            event
                .raw()
                .get(key)
                .or_else(|| {
                    event
                        .raw()
                        .get("reply_context")
                        .and_then(|context| context.get(key))
                })
                .cloned()
                .map(|value| (key.into(), value))
        })
        .collect();
        Ok(Self {
            bot,
            target,
            context,
        })
    }

    fn action(&self, message: SendableMessage) -> anyhow::Result<Value> {
        let mut action = nuono_plugin::api::routed::build_send_msg_action(
            &self.target.route.platform,
            &self.target,
            &message,
        )?;
        let params = action["params"]
            .as_object_mut()
            .context("reply params missing")?;
        for (key, value) in &self.context {
            params.entry(key.clone()).or_insert_with(|| value.clone());
        }
        action["reply_current"] = json!(true);
        Ok(action)
    }

    pub(crate) async fn send_confirmed(&self, message: SendableMessage) -> anyhow::Result<()> {
        let receipt = tokio::time::timeout(
            Duration::from_secs(45),
            self.bot
                .call_routed_receipt(&self.target.route, self.action(message)?),
        )
        .await
        .context("解析卡片发送回执超时")??;
        // OneBot accepts unknown/async envelopes as Ok for compatibility. An
        // enqueued action is insufficient for releasing the next message.
        anyhow::ensure!(
            receipt
                .raw
                .get("status")
                .is_none_or(|status| status.as_str() == Some("ok"))
                && receipt
                    .raw
                    .get("retcode")
                    .is_none_or(|code| code.as_i64() == Some(0)),
            "解析卡片未返回成功发送回执"
        );
        Ok(())
    }

    pub(crate) async fn notify(&self, message: SendableMessage) -> anyhow::Result<()> {
        self.bot
            .notify_routed(&self.target.route, self.action(message)?)
            .await?;
        Ok(())
    }
}

pub fn media_message(
    candidate: &DownloadCandidate,
    path: &Path,
    thumb: Option<&Path>,
) -> SendableMessage {
    let file = format!("file://{}", path.display());
    let segment = match candidate.kind {
        MediaKind::Video => MessageSegment::video(file),
        MediaKind::Audio => MessageSegment::record(file),
    };
    let mut encoded = segment.to_json();
    // SDK 的视频段尚无 thumb 字段；仅在投递处保留 OneBot 的缩略图扩展。
    // 缩略图必须是下载后的本地文件，不把需要 Referer 的封面 URL 交给平台。
    if candidate.kind == MediaKind::Video
        && let Some(thumb) = thumb
    {
        encoded["data"]["thumb"] = json!(format!("file://{}", thumb.display()));
    }
    SendableMessage::Json(json!([encoded]).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_comes_first_then_images_in_order() {
        let card = Card::new()
            .line("标题")
            .image("https://cdn.example/1.jpg")
            .image("https://cdn.example/2.jpg");
        let encoded = SendableMessage::from(card_message(&card)).to_json();
        assert_eq!(encoded[0]["type"], "text");
        assert_eq!(encoded[0]["data"]["text"], "标题");
        assert_eq!(encoded[1]["data"]["file"], "https://cdn.example/1.jpg");
        assert_eq!(encoded[2]["data"]["file"], "https://cdn.example/2.jpg");
    }

    #[test]
    fn image_only_card_emits_no_empty_text_segment() {
        let encoded = SendableMessage::from(card_message(
            &Card::new().image("https://cdn.example/1.jpg"),
        ))
        .to_json();
        assert_eq!(encoded.as_array().unwrap().len(), 1);
        assert_eq!(encoded[0]["type"], "image");
    }

    #[test]
    fn video_thumb_is_a_local_file_uri_and_never_named_cover() {
        let video = DownloadCandidate::video("https://cdn.example/v.mp4", "v.mp4", "https://e/");
        let segment = media_message(
            &video,
            Path::new("/tmp/v.mp4"),
            Some(Path::new("/tmp/c.jpg")),
        )
        .to_json();
        assert_eq!(segment[0]["data"]["thumb"], "file:///tmp/c.jpg");
        assert!(segment[0]["data"].get("cover").is_none());
    }

    #[test]
    fn audio_and_video_without_thumb_omit_the_extension() {
        let audio = DownloadCandidate::audio("https://cdn.example/a.mp3", "a.mp3", "https://e/");
        let segment = media_message(
            &audio,
            Path::new("/tmp/a.mp3"),
            Some(Path::new("/tmp/c.jpg")),
        )
        .to_json();
        assert_eq!(segment[0]["type"], "record");
        assert!(segment[0]["data"].get("thumb").is_none());
        let video = DownloadCandidate::video("https://cdn.example/v.mp4", "v.mp4", "https://e/");
        let segment = media_message(&video, Path::new("/tmp/v.mp4"), None).to_json();
        assert!(segment[0]["data"].get("thumb").is_none());
    }
}
