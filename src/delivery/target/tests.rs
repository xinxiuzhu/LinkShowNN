//! Offline protocol checks. Recording sinks cannot contact any platform or person.

use std::sync::{
    Mutex,
    atomic::{AtomicBool, Ordering},
};

use nuono_plugin::api::waiter::WaitManager;
use nuono_plugin::core::RuntimeActionRouter;
use nuono_plugin::core::event_bus::EventBus;
use nuono_plugin::core::onebot_client::ApiClient;
use nuono_plugin::{
    ImageSource, MessageSegment, NuonoError, NuonoResult, RuntimeActionSink, RuntimeBotRoute,
};

use super::*;

struct RecordingSink {
    calls: Mutex<Vec<Value>>,
    login: Value,
    send_result: Value,
    send_error: AtomicBool,
}

#[async_trait::async_trait]
impl RuntimeActionSink for RecordingSink {
    async fn send_action(&self, _action: Value) -> NuonoResult<()> {
        panic!("gallery deliveries must wait for routed send receipts");
    }

    async fn call_action(&self, action: Value) -> NuonoResult<Value> {
        self.calls.lock().unwrap().push(action.clone());
        if action["action"] == "get_login_info" {
            Ok(self.login.clone())
        } else if self.send_error.load(Ordering::Acquire) {
            Err(NuonoError::MessageSend(
                "offline fixture rejected send".into(),
            ))
        } else {
            Ok(self.send_result.clone())
        }
    }
}

struct Rig {
    bot: Arc<Bot>,
    route: RuntimeBotRoute,
    sink: Arc<RecordingSink>,
    unrelated: Arc<RecordingSink>,
}

impl Rig {
    fn new(platform: &str, login: Value, send_result: Value) -> Self {
        let route = RuntimeBotRoute::new(platform, "gallery-instance", "00011");
        let sink = Arc::new(RecordingSink {
            calls: Mutex::default(),
            login,
            send_result,
            send_error: AtomicBool::new(false),
        });
        let unrelated = Arc::new(RecordingSink {
            calls: Mutex::default(),
            login: json!({"user_id":"00011", "nickname":"不相关机器人"}),
            send_result: json!({"message_id":"unrelated"}),
            send_error: AtomicBool::new(false),
        });
        let router = Arc::new(RuntimeActionRouter::new());
        router.register(route.clone(), sink.clone());
        router.register(
            RuntimeBotRoute::new(platform, "other-instance", "00011"),
            unrelated.clone(),
        );
        let bot = Arc::new(
            Bot::new(
                999,
                Arc::new(EventBus::new()),
                Arc::new(WaitManager::new()),
                Arc::new(ApiClient::new()),
                None,
                Some(unrelated.clone()),
            )
            .with_action_router(router),
        );
        Self {
            bot,
            route,
            sink,
            unrelated,
        }
    }

    fn successful(platform: &str) -> Self {
        Self::new(
            platform,
            json!({"user_id":"00011", "nickname":"测试机器人"}),
            json!({"message_id":"confirmed"}),
        )
    }

    fn target(&self, kind: &str) -> ReplyTarget {
        let event = json!({
            "route":self.route,
            "message_type":kind,
            "group_id":"000900000000000000000099",
            "user_id":"000800000000000000000088",
            "message_id":"0007",
            "event_id":"gallery-trigger",
            "sender":{"nickname":"用户昵称不可用作转发机器人"},
        });
        ReplyTarget::new(self.bot.clone(), &Event::new(&event)).unwrap()
    }

    fn calls(&self) -> Vec<Value> {
        assert!(self.unrelated.calls.lock().unwrap().is_empty());
        self.sink.calls.lock().unwrap().clone()
    }
}

async fn images(count: usize) -> Vec<MessageSegment> {
    let mut images = Vec::with_capacity(count);
    for index in 0..count {
        // The SDK validates the image header and size before producing base64.
        let mut bytes = b"\x89PNG\r\n\x1a\nfixture".to_vec();
        bytes.push(index as u8);
        images.push(ImageSource::bytes(bytes).into_segment().await.unwrap());
    }
    images
}

async fn gallery(count: usize, total: usize) -> GalleryDelivery {
    GalleryDelivery::new(
        "图集标题\n作者：公开昵称\n点赞 100 · 评论 20\nhttps://v.douyin.com/example/".into(),
        images(count).await,
        total,
        "https://v.douyin.com/example/",
    )
    .unwrap()
}

#[tokio::test]
async fn one_to_three_images_use_one_routed_message_with_text_first() {
    for count in 1..=3 {
        for kind in ["group", "private"] {
            let rig = Rig::successful("OneBot v11");
            let input = gallery(count, count).await;
            rig.target(kind)
                .send_gallery_confirmed(input)
                .await
                .unwrap();
            let calls = rig.calls();
            assert_eq!(calls.len(), 1, "no login query or extra text message");
            let action = &calls[0];
            assert_eq!(action["action"], "send_msg");
            let segments = action["params"]["message"].as_array().unwrap();
            assert_eq!(segments.len(), count + 1);
            assert_eq!(segments[0]["type"], "text");
            assert!(
                segments[0]["data"]["text"]
                    .as_str()
                    .unwrap()
                    .contains("评论 20")
            );
            for image in &segments[1..] {
                assert_eq!(image["type"], "image");
                assert!(
                    image["data"]["file"]
                        .as_str()
                        .unwrap()
                        .starts_with("base64://")
                );
            }
            assert_eq!(action["params"]["message_id"], "0007");
            assert_eq!(action["params"]["event_id"], "gallery-trigger");
        }
    }
}

#[tokio::test]
async fn forward_uses_exact_group_private_targets_and_actual_bot_identity() {
    for (kind, action_name, target_key, target_id) in [
        (
            "group",
            "send_group_forward_msg",
            "group_id",
            "000900000000000000000099",
        ),
        (
            "private",
            "send_private_forward_msg",
            "user_id",
            "000800000000000000000088",
        ),
    ] {
        let rig = Rig::successful("OneBot v11");
        let input = gallery(4, 4).await;
        rig.target(kind)
            .send_gallery_confirmed(input)
            .await
            .unwrap();
        let calls = rig.calls();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0]["action"], "get_login_info");
        assert_eq!(calls[1]["action"], action_name);
        assert_eq!(calls[1]["params"][target_key], target_id);
        let nodes = calls[1]["params"]["messages"].as_array().unwrap();
        assert_eq!(nodes.len(), 5);
        assert_eq!(nodes[0]["data"]["content"][0]["type"], "text");
        for node in nodes {
            assert_eq!(node["type"], "node");
            assert_eq!(node["data"]["uin"], "00011");
            assert_eq!(node["data"]["name"], "测试机器人");
        }
        let expected = images(4).await;
        for (node, image) in nodes[1..].iter().zip(expected) {
            assert_eq!(node["data"]["content"], json!([image.to_json()]));
        }
    }
}

#[tokio::test]
async fn forwards_at_most_eight_images_and_explains_omitted_originals() {
    let rig = Rig::successful("onebot_v11");
    rig.target("group")
        .send_gallery_confirmed(gallery(8, 49).await)
        .await
        .unwrap();
    let calls = rig.calls();
    let nodes = calls[1]["params"]["messages"].as_array().unwrap();
    assert_eq!(nodes.len(), 9);
    let text = nodes[0]["data"]["content"][0]["data"]["text"]
        .as_str()
        .unwrap();
    assert!(text.contains("图集共 49 张"));
    assert!(text.contains("最多展示前 8 张"));
    assert_eq!(text.matches("https://v.douyin.com/example/").count(), 1);
    assert!(
        nodes[1..]
            .iter()
            .all(|node| node["data"]["content"][0]["type"] == "image")
    );
}

#[tokio::test]
async fn failed_image_fetches_do_not_change_original_forward_threshold() {
    let rig = Rig::successful("onebot");
    rig.target("group")
        .send_gallery_confirmed(gallery(2, 4).await)
        .await
        .unwrap();
    let calls = rig.calls();
    assert_eq!(calls[1]["action"], "send_group_forward_msg");
    let nodes = calls[1]["params"]["messages"].as_array().unwrap();
    assert_eq!(nodes.len(), 3);
    assert!(
        nodes[0]["data"]["content"][0]["data"]["text"]
            .as_str()
            .unwrap()
            .contains("部分图片获取失败")
    );
}

#[tokio::test]
async fn other_platforms_fall_back_to_one_text_and_bounded_images_message() {
    let rig = Rig::successful("qq_official");
    rig.target("private")
        .send_gallery_confirmed(gallery(8, 49).await)
        .await
        .unwrap();
    let calls = rig.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0]["action"], "send_msg");
    assert_eq!(calls[0]["params"]["openid"], "000800000000000000000088");
    let segments = calls[0]["params"]["message"].as_array().unwrap();
    assert_eq!(segments.len(), 9);
    assert!(
        segments[0]["data"]["text"]
            .as_str()
            .unwrap()
            .contains("当前平台不支持合并转发")
    );
    assert!(segments[1..].iter().all(|image| image["type"] == "image"));
}

#[tokio::test]
async fn failed_or_unknown_forward_receipts_never_retry_as_direct_messages() {
    for response in [
        json!({"status":"failed", "retcode":100}),
        json!({"status":"async", "retcode":0}),
        json!({"status":"unknown", "retcode":0}),
        json!({"status":"ok", "retcode":100}),
        json!({"retcode":0}),
        json!({"data":{"message_id":"maybe-accepted"}}),
        json!({"echo":"correlation-only", "message_id":"maybe-accepted"}),
    ] {
        let rig = Rig::new(
            "OneBot v11",
            json!({"user_id":"00011", "nickname":"测试机器人"}),
            response,
        );
        assert!(
            rig.target("group")
                .send_gallery_confirmed(gallery(4, 4).await)
                .await
                .is_err()
        );
        let calls = rig.calls();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[1]["action"], "send_group_forward_msg");
    }
}

#[tokio::test]
async fn known_sdk_success_data_and_null_remain_valid_confirmation() {
    for response in [
        Value::Null,
        json!({"message_id":"native-id"}),
        json!({"message_id":"native-id", "forward_id":"native-forward-id"}),
        json!({"status":"ok", "retcode":0, "data":null}),
    ] {
        let rig = Rig::new(
            "OneBot v11",
            json!({"user_id":"00011", "nickname":"测试机器人"}),
            response,
        );
        rig.target("group")
            .send_gallery_confirmed(gallery(4, 4).await)
            .await
            .unwrap();
        let calls = rig.calls();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[1]["action"], "send_group_forward_msg");
    }
}

#[tokio::test]
async fn native_receipt_protocol_data_is_valid_without_onebot_envelope_fields() {
    let rig = Rig::new(
        "qq_official",
        Value::Null,
        json!({
            "message_ids":["native-message"],
            "raw":{"data":{"id":"native-message", "status":"sent"}}
        }),
    );
    rig.target("private")
        .send_gallery_confirmed(gallery(4, 4).await)
        .await
        .unwrap();
    let calls = rig.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0]["action"], "send_msg");
    let text = calls[0]["params"]["message"][0]["data"]["text"]
        .as_str()
        .unwrap();
    assert!(text.contains("当前平台不支持合并转发，发送 4 张图片。"));
}

#[tokio::test]
async fn native_adapter_error_is_not_confirmation_and_never_retries() {
    let rig = Rig::successful("qq_official");
    rig.sink.send_error.store(true, Ordering::Release);
    assert!(
        rig.target("private")
            .send_gallery_confirmed(gallery(4, 4).await)
            .await
            .is_err()
    );
    let calls = rig.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0]["action"], "send_msg");
}

#[tokio::test]
async fn unrelated_login_identity_cannot_be_used_as_forward_author() {
    let rig = Rig::new(
        "OneBot v11",
        json!({"user_id":"999", "nickname":"错误机器人"}),
        json!({"message_id":"confirmed"}),
    );
    rig.target("group")
        .send_gallery_confirmed(gallery(4, 4).await)
        .await
        .unwrap();
    let calls = rig.calls();
    let nodes = calls[1]["params"]["messages"].as_array().unwrap();
    for node in nodes {
        assert_eq!(node["data"]["uin"], "00011");
        assert_eq!(node["data"]["name"], "00011");
    }
}

#[tokio::test]
async fn gallery_plan_rejects_unfetched_urls_and_excess_image_counts() {
    assert!(
        GalleryDelivery::new(
            "text".into(),
            vec![MessageSegment::image("https://cdn.example/a.png")],
            1,
            "https://example/"
        )
        .is_err()
    );
    assert!(GalleryDelivery::new("text".into(), images(9).await, 9, "https://example/").is_err());
    assert!(GalleryDelivery::new("text".into(), images(2).await, 1, "https://example/").is_err());
    assert!(GalleryDelivery::new("text".into(), Vec::new(), 1, "https://example/").is_err());
}
