//! 消息 -> 解析 -> 回复 的主流程。
//!
//! 顺序是有意的：先做不花钱的判断（平台开关、黑名单、原文关键词、冷却），
//! 拿到并发许可后才发网络请求。关键词查两次——解析前查原文，解析后查卡片文本，
//! 因为标题里的词在解析完成前是看不到的。

use std::sync::{Arc, Mutex};
use std::time::Instant;

use anyhow::{Context, Result};
use nuono_plugin::prelude::*;
use serde_json::Value;
use tokio::sync::Semaphore;
use tracing::warn;

use super::{gate, state, state::RuntimeState};
use crate::{
    default_config::ResolverConfig, resolver::ParserManager, resolver::Platform,
    resolver::url_policy as url_guard,
};
use crate::{delivery, download::task as download_task};

/// 处理一条消息。返回 `Continue` 表示本插件不接这条消息。
pub async fn run(
    state: &Arc<Mutex<RuntimeState>>,
    limiter: &Semaphore,
    event: &Event<'_>,
    config: &ResolverConfig,
    parser: &ParserManager,
) -> Result<PluginResponse> {
    // 小程序卡片的正文在 JSON 里，不在 text 字段。
    let card_url = config
        .enable_parse_miniapp
        .then(|| extract_card_url(event.raw()))
        .flatten();
    let text = card_url.as_deref().unwrap_or_else(|| event.text());

    let Some(matched) = parser.match_url(text) else {
        return Ok(PluginResponse::Continue);
    };
    if matched.platform == Platform::Twitter && !config.twitter_enabled {
        return Ok(PluginResponse::Continue);
    }

    let user = event
        .user_id_str()
        .unwrap_or_else(|| event.user_id().to_string());
    let group = event.group_id_str();
    if let Some(rejection) = gate::check(config, matched.platform, &user, group.as_deref(), text) {
        return reject(state, rejection);
    }

    let key = format!("{}:{}", group.as_deref().unwrap_or("private"), user);
    let cooldown = state::with(state, |state| {
        let rejection = gate::take_cooldown(
            &mut state.last_calls,
            &key,
            config.user_cooldown,
            Instant::now(),
        );
        if rejection.is_none() {
            state.metrics.attempt();
        }
        rejection
    })?;
    if let Some(gate::Rejection::Cooldown { remaining }) = cooldown {
        return Ok(event.reply_text(format!("解析请求过于频繁，请 {remaining} 秒后再试。")));
    }

    let _permit = limiter
        .acquire()
        .await
        .context("resolver concurrency semaphore closed")?;

    let resolved = match parser.resolve(&matched).await {
        Ok(resolved) => resolved,
        Err(error) => {
            state::with(state, |state| state.metrics.failure())?;
            // URL 可能带 token 等敏感 query，落日志前先脱敏。
            warn!(
                platform = matched.platform.as_str(),
                url = %url_guard::redact(&matched.url),
                error = %error,
                "URL resolver failed"
            );
            return Ok(event.reply_text("未能从此链接解析到可用内容。"));
        }
    };

    // 解析后再查一次关键词：命中的可能是标题或简介里的词。
    if gate::blocks_keyword(config, &resolved.filter_text()) {
        return reject(state, gate::Rejection::BlockedKeyword);
    }
    state::with(state, |state| state.metrics.success())?;

    let first = config
        .enable_media_download
        .then(|| download_task::schedule(state, event, &resolved))
        .flatten();
    let card = resolved.card();
    let renderer = state::with(state, |state| state.renderer.clone())?;
    if let Some(renderer) = renderer {
        let response = async {
            let bytes = renderer.render(card.clone()).await?;
            delivery::image_message(bytes).await
        }
        .await;
        if renderer.is_closed() {
            return Ok(PluginResponse::Continue);
        }
        match response {
            Ok(message) => return finish(message, first).await,
            Err(error) => warn!(platform = matched.platform.as_str(), error = %error,
                "图文卡片生成失败，回退原图文消息"),
        }
    }
    finish(delivery::card_message(&card).into(), first).await
}

async fn finish(
    message: SendableMessage,
    first: Option<download_task::CardFirst>,
) -> Result<PluginResponse> {
    if let Some(first) = first {
        if let Err(error) = first.send(message).await {
            warn!(error = %error, "解析卡片未确认发送，取消后续媒体发送");
        }
        // Already sent directly (or outcome unknown); never send it twice.
        Ok(PluginResponse::Stop)
    } else {
        Ok(delivery::reply_message(message))
    }
}

/// 关键词命中要计入屏蔽统计并截断事件链；其余拒绝静默放行给后续插件。
fn reject(state: &Mutex<RuntimeState>, rejection: gate::Rejection) -> Result<PluginResponse> {
    if rejection != gate::Rejection::BlockedKeyword {
        return Ok(PluginResponse::Continue);
    }
    state::with(state, |state| state.metrics.block())?;
    Ok(PluginResponse::Stop)
}

/// 从小程序/分享卡片的 json 段里取原始链接。
///
/// 先认已知字段名，再退回逐层搜索；`data` 常是一层被转义的 JSON 字符串，
/// 所以字符串分支要再解析一次。URL 取出后还原 HTML 实体，否则 query 会断。
pub fn extract_card_url(event: &Value) -> Option<String> {
    fn find_url(value: &Value) -> Option<String> {
        match value {
            Value::Object(map) => {
                for key in ["qqdocurl", "jumpUrl", "jump_url", "url", "link"] {
                    if let Some(url) = map.get(key).and_then(Value::as_str)
                        && (url.starts_with("http://") || url.starts_with("https://"))
                    {
                        return Some(url.replace("&amp;", "&").replace("&#44;", ","));
                    }
                }
                map.values().find_map(find_url)
            }
            Value::Array(values) => values.iter().find_map(find_url),
            Value::String(raw) => serde_json::from_str::<Value>(raw)
                .ok()
                .and_then(|value| find_url(&value)),
            _ => None,
        }
    }

    event
        .get("message")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|segment| segment.get("type").and_then(Value::as_str) == Some("json"))
        .find_map(|segment| segment.get("data").and_then(find_url))
}

#[cfg(test)]
mod tests {
    use super::extract_card_url;
    use serde_json::json;

    #[test]
    fn extracts_url_from_json_card() {
        let event = json!({
            "message": [{
                "type": "json",
                "data": {"meta": {"detail_1": {"qqdocurl": "https://b23.tv/abc&amp;p=2"}}}
            }]
        });
        assert_eq!(
            extract_card_url(&event).as_deref(),
            Some("https://b23.tv/abc&p=2")
        );
    }

    #[test]
    fn unwraps_data_carried_as_escaped_json_string() {
        // QQ 常把整个卡片塞进 data 的字符串里，需要再解析一层才能看到 URL。
        let event = json!({
            "message": [{
                "type": "json",
                "data": {"data": "{\"meta\":{\"news\":{\"jumpUrl\":\"https://b23.tv/xyz\"}}}"}
            }]
        });
        assert_eq!(
            extract_card_url(&event).as_deref(),
            Some("https://b23.tv/xyz")
        );
    }

    #[test]
    fn ignores_non_json_segments_and_missing_message() {
        // 纯文本段不走卡片提取，避免把普通消息里的链接当成卡片链接。
        let text_only =
            json!({"message": [{"type": "text", "data": {"text": "https://b23.tv/a"}}]});
        assert_eq!(extract_card_url(&text_only), None);
        assert_eq!(extract_card_url(&json!({"post_type": "message"})), None);
    }
}
