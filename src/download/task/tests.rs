//! Deterministic delivery ordering checks: receipts and download completion are
//! controlled independently; no browser, network, database, or timing sleeps.

use super::*;
use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use nuono_plugin::api::waiter::WaitManager;
use nuono_plugin::core::RuntimeActionRouter;
use nuono_plugin::core::event_bus::EventBus;
use nuono_plugin::core::onebot_client::ApiClient;
use nuono_plugin::{NuonoError, NuonoResult, RuntimeActionSink};
use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot};

#[derive(Debug)]
struct PendingCall {
    action: Value,
    receipt: oneshot::Sender<NuonoResult<Value>>,
}

struct ControlledSink {
    calls: mpsc::UnboundedSender<PendingCall>,
    notifications: mpsc::UnboundedSender<Value>,
}

#[async_trait::async_trait]
impl RuntimeActionSink for ControlledSink {
    async fn send_action(&self, action: Value) -> NuonoResult<()> {
        self.notifications.send(action).unwrap();
        Ok(())
    }

    async fn call_action(&self, action: Value) -> NuonoResult<Value> {
        let (receipt, result) = oneshot::channel();
        self.calls.send(PendingCall { action, receipt }).unwrap();
        result
            .await
            .unwrap_or_else(|_| Err(NuonoError::MessageSend("test receipt closed".into())))
    }
}

struct Rig {
    bot: Arc<Bot>,
    tasks: Arc<PluginTaskScope>,
    route: RuntimeBotRoute,
    calls: mpsc::UnboundedReceiver<PendingCall>,
    notifications: mpsc::UnboundedReceiver<Value>,
}

impl Rig {
    fn new() -> Self {
        Self::with_platform("qq_official", "bot-openid")
    }

    fn onebot() -> Self {
        Self::with_platform("onebot_v11", "123456")
    }

    fn with_platform(platform: &str, bot_id: &str) -> Self {
        let (call_tx, calls) = mpsc::unbounded_channel();
        let (notification_tx, notifications) = mpsc::unbounded_channel();
        let sink = Arc::new(ControlledSink {
            calls: call_tx,
            notifications: notification_tx,
        });
        let router = Arc::new(RuntimeActionRouter::new());
        let route = RuntimeBotRoute::new(platform, "order-test", bot_id);
        router.register(route.clone(), sink.clone());
        let bot = Arc::new(
            Bot::new(
                0,
                Arc::new(EventBus::new()),
                Arc::new(WaitManager::new()),
                Arc::new(ApiClient::new()),
                None,
                Some(sink),
            )
            .with_action_router(router),
        );
        Self {
            bot,
            tasks: Arc::new(PluginTaskScope::new("card-order-test")),
            route,
            calls,
            notifications,
        }
    }

    fn request(&self, id: &str) -> (CardFirst, oneshot::Receiver<()>, delivery::ReplyTarget) {
        let event = json!({
            "route": self.route,
            "post_type": "message",
            "message_type": "group",
            "group_id": "000-group-openid",
            "user_id": "000-user-openid",
            "group_openid": "000-group-openid",
            "message_id": id,
            "event_id": format!("event-{id}"),
            "qq_target_type": "group",
            "reply_context": {"message_id": id, "event_id": format!("event-{id}")},
        });
        let target = delivery::ReplyTarget::new(self.bot.clone(), &Event::new(&event)).unwrap();
        let (confirmed, wait_for_card) = oneshot::channel();
        (
            CardFirst {
                target: target.clone(),
                confirmed,
                tasks: self.tasks.clone(),
            },
            wait_for_card,
            target,
        )
    }

    fn followup(
        &self,
        target: delivery::ReplyTarget,
        wait_for_card: oneshot::Receiver<()>,
        work: impl Future<Output = SendableMessage> + Send + 'static,
    ) -> oneshot::Receiver<bool> {
        let (done, completed) = oneshot::channel();
        assert!(self.tasks.try_spawn("test-download", async move {
            let sent = if let Some(message) = after_card(wait_for_card, work).await {
                target.send_confirmed(message).await.unwrap();
                true
            } else {
                false
            };
            let _ = done.send(sent);
        }));
        completed
    }
}

async fn bounded<T>(future: impl Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(5), future)
        .await
        .expect("delivery test stalled")
}

fn video(name: &str) -> SendableMessage {
    vec![MessageSegment::video(format!("file:///tmp/{name}.mp4"))].into()
}

fn poster() -> SendableMessage {
    vec![MessageSegment::image("base64://test-card")].into()
}

async fn confirm_followup(rig: &mut Rig, completed: oneshot::Receiver<bool>) -> Value {
    let call = bounded(rig.calls.recv()).await.unwrap();
    call.receipt.send(Ok(Value::Null)).unwrap();
    assert!(bounded(completed).await.unwrap());
    assert!(rig.notifications.try_recv().is_err());
    call.action
}

fn gallery(total: usize) -> delivery::GalleryDelivery {
    delivery::GalleryDelivery::new(
        "图集信息".into(),
        (0..total.min(delivery::MAX_GALLERY_IMAGES))
            .map(|_| MessageSegment::image("base64://AA=="))
            .collect(),
        total,
        "https://www.douyin.com/note/123",
    )
    .unwrap()
}

async fn gallery_send_call(rig: &mut Rig, total: usize) -> PendingCall {
    let mut call = bounded(rig.calls.recv()).await.unwrap();
    if total > 3 {
        assert_eq!(call.action["action"], "get_login_info");
        call.receipt
            .send(Ok(json!({"user_id":"123456", "nickname":"Nuono"})))
            .unwrap();
        call = bounded(rig.calls.recv()).await.unwrap();
        assert_eq!(call.action["action"], "send_group_forward_msg");
        assert_eq!(
            call.action["params"]["messages"].as_array().unwrap().len(),
            9
        );
    } else {
        assert_eq!(call.action["action"], "send_msg");
        assert_eq!(
            call.action["params"]["message"].as_array().unwrap().len(),
            total + 1
        );
    }
    call
}

#[tokio::test]
async fn ready_video_waits_for_direct_gallery_or_forward_receipt() {
    for total in [3, 49] {
        let mut rig = Rig::onebot();
        let (first, wait, target) = rig.request("gallery-before-video");
        let (ready, download_done) = oneshot::channel();
        let completed = rig.followup(target, wait, async move {
            ready.send(()).unwrap();
            video("gallery")
        });
        bounded(download_done).await.unwrap();
        let sending = tokio::spawn(first.send_gallery(gallery(total)));
        let call = gallery_send_call(&mut rig, total).await;
        assert!(rig.notifications.try_recv().is_err());
        call.receipt.send(Ok(Value::Null)).unwrap();
        bounded(sending).await.unwrap().unwrap();
        let sent = confirm_followup(&mut rig, completed).await;
        assert_eq!(sent["params"]["message"][0]["type"], "video");
        rig.tasks.shutdown().await;
    }
}

#[tokio::test]
async fn rejected_or_unknown_forward_never_releases_a_ready_video() {
    for receipt in [
        json!({"status":"failed", "retcode":100}),
        json!({"status":"async", "retcode":0}),
        json!({"data":{"message_id":"maybe-sent"}, "retcode":0}),
    ] {
        let mut rig = Rig::onebot();
        let (first, wait, target) = rig.request("unconfirmed-forward");
        let completed = rig.followup(target, wait, std::future::ready(video("blocked")));
        let sending = tokio::spawn(first.send_gallery(gallery(49)));
        let call = gallery_send_call(&mut rig, 49).await;
        call.receipt.send(Ok(receipt)).unwrap();
        assert!(bounded(sending).await.unwrap().is_err());
        assert!(!bounded(completed).await.unwrap());
        assert!(rig.notifications.try_recv().is_err());
        assert!(rig.calls.try_recv().is_err());
        rig.tasks.shutdown().await;
    }
}

#[tokio::test]
async fn finished_download_waits_for_card_receipt_and_preserves_reply_context() {
    let mut rig = Rig::new();
    let (first, wait, target) = rig.request("000-message-a");
    let (download_done, downloaded) = oneshot::channel();
    let completed = rig.followup(target, wait, async move {
        download_done.send(()).unwrap();
        video("a")
    });
    bounded(downloaded).await.unwrap();
    assert!(rig.notifications.try_recv().is_err());
    let card = tokio::spawn(first.send(poster()));
    let call = bounded(rig.calls.recv()).await.unwrap();
    assert_eq!(call.action["params"]["message"][0]["type"], "image");
    assert!(rig.notifications.try_recv().is_err());
    // Known successful OneBot calls return only data, which can be null without
    // a native message ID. Async/unknown raw envelopes are checked separately.
    call.receipt.send(Ok(Value::Null)).unwrap();
    bounded(card).await.unwrap().unwrap();
    let sent = confirm_followup(&mut rig, completed).await;
    assert_eq!(sent["params"]["message"][0]["type"], "video");
    for action in [&call.action, &sent] {
        assert_eq!(action["params"]["group_id"], "000-group-openid");
        assert_eq!(action["params"]["group_openid"], "000-group-openid");
        assert_eq!(action["params"]["message_id"], "000-message-a");
        assert_eq!(action["params"]["event_id"], "event-000-message-a");
        assert_eq!(action["params"]["qq_target_type"], "group");
        assert_eq!(
            action["params"]["reply_context"]["message_id"],
            "000-message-a"
        );
    }
    rig.tasks.shutdown().await;
}

#[tokio::test]
async fn ready_mp3_is_sent_as_record_only_after_card_receipt() {
    let mut rig = Rig::onebot();
    let (first, wait, target) = rig.request("song-before-record");
    let (ready, prepared) = oneshot::channel();
    let completed = rig.followup(target, wait, async move {
        ready.send(()).unwrap();
        vec![MessageSegment::record("file:///tmp/song.mp3")].into()
    });
    bounded(prepared).await.unwrap();
    assert!(rig.notifications.try_recv().is_err());
    let sending = tokio::spawn(first.send(poster()));
    let call = bounded(rig.calls.recv()).await.unwrap();
    assert_eq!(call.action["params"]["message"][0]["type"], "image");
    assert!(rig.notifications.try_recv().is_err());
    call.receipt.send(Ok(Value::Null)).unwrap();
    bounded(sending).await.unwrap().unwrap();
    let sent = confirm_followup(&mut rig, completed).await;
    assert_eq!(sent["params"]["message"][0]["type"], "record");
    assert_eq!(
        sent["params"]["message"][0]["data"]["file"],
        "file:///tmp/song.mp3"
    );
    rig.tasks.shutdown().await;
}

#[tokio::test]
async fn remote_media_waits_for_card_then_for_its_own_receipt_without_local_fallback() {
    for (kind, accepted) in [(MediaKind::Audio, true), (MediaKind::Video, false)] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("downloaded-media");
        tokio::fs::write(&path, b"downloaded-media-fixture")
            .await
            .unwrap();
        let media = Downloaded { path, thumb: None };
        let mut candidate =
            DownloadCandidate::video("https://cdn.example/media", "fixture", "https://example/");
        candidate.kind = kind;
        let mut rig = Rig::onebot();
        let (first, wait, target) = rig.request("remote-media");
        let (finished, mut completed) = oneshot::channel();
        let (ready, downloaded) = oneshot::channel();
        assert!(rig.tasks.try_spawn("remote-media-test", async move {
            let work = async {
                ready.send(()).unwrap();
                media
            };
            let confirmed = if let Some(media) = after_card(wait, work).await {
                send_media(
                    &target,
                    &candidate,
                    &media,
                    "base64",
                    &tokio::sync::Semaphore::new(1),
                )
                .await
                .is_ok()
            } else {
                false
            };
            let _ = finished.send(confirmed);
        }));
        bounded(downloaded).await.unwrap();
        assert!(rig.calls.try_recv().is_err());
        let card = tokio::spawn(first.send(poster()));
        let image = bounded(rig.calls.recv()).await.unwrap();
        assert_eq!(image.action["params"]["message"][0]["type"], "image");
        assert!(rig.calls.try_recv().is_err());
        image.receipt.send(Ok(Value::Null)).unwrap();
        bounded(card).await.unwrap().unwrap();
        let call = bounded(rig.calls.recv()).await.unwrap();
        let segment = &call.action["params"]["message"][0];
        assert_eq!(
            segment["type"],
            if kind == MediaKind::Audio {
                "record"
            } else {
                "video"
            }
        );
        use base64::Engine;
        let received = base64::engine::general_purpose::STANDARD
            .decode(
                segment["data"]["file"]
                    .as_str()
                    .unwrap()
                    .strip_prefix("base64://")
                    .unwrap(),
            )
            .unwrap();
        assert_eq!(received, b"downloaded-media-fixture");
        assert!(
            matches!(
                completed.try_recv(),
                Err(oneshot::error::TryRecvError::Empty)
            ),
            "media must still wait for its own receipt"
        );
        if accepted {
            call.receipt
                .send(Ok(json!({"message_id":"media-confirmed"})))
                .unwrap();
        } else {
            call.receipt
                .send(Err(NuonoError::MessageSend(
                    "fixture upload rejected".into(),
                )))
                .unwrap();
        }
        assert_eq!(bounded(completed).await.unwrap(), accepted);
        assert!(
            rig.calls.try_recv().is_err(),
            "failed media must not be retried with local paths"
        );
        assert!(
            rig.notifications.try_recv().is_err(),
            "media must not use fire-and-forget sends"
        );
        rig.tasks.shutdown().await;
    }
}

#[tokio::test]
async fn failed_card_does_not_read_or_send_downloaded_media_contents() {
    let mut rig = Rig::onebot();
    let (first, wait, target) = rig.request("card-rejected-before-media-read");
    // This unavailable file would fail preparation if we read it prematurely.
    let media = Downloaded {
        path: "/nonexistent-downloaded-media".into(),
        thumb: None,
    };
    let candidate =
        DownloadCandidate::video("https://cdn.example/media", "fixture", "https://example/");
    let (finished, completed) = oneshot::channel();
    assert!(rig.tasks.try_spawn("rejected-card-media-test", async move {
        let touched = if let Some(media) = after_card(wait, std::future::ready(media)).await {
            let _ = send_media(
                &target,
                &candidate,
                &media,
                "base64",
                &tokio::sync::Semaphore::new(1),
            )
            .await;
            true
        } else {
            false
        };
        let _ = finished.send(touched);
    }));
    let card = tokio::spawn(first.send(poster()));
    let image = bounded(rig.calls.recv()).await.unwrap();
    image
        .receipt
        .send(Err(NuonoError::MessageSend(
            "fixture rejected image".into(),
        )))
        .unwrap();
    assert!(bounded(card).await.unwrap().is_err());
    assert!(!bounded(completed).await.unwrap());
    assert!(rig.calls.try_recv().is_err());
    assert!(rig.notifications.try_recv().is_err());
    rig.tasks.shutdown().await;
}

#[tokio::test]
async fn delivery_slot_is_held_until_receipt_and_acquired_before_reading_the_next_file() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("first.mp3");
    tokio::fs::write(&path, b"ID3-first-media").await.unwrap();
    let mut rig = Rig::onebot();
    let (_, _, first_target) = rig.request("first-media");
    let (_, _, second_target) = rig.request("second-media");
    let slots = Arc::new(tokio::sync::Semaphore::new(1));
    let first_slots = slots.clone();
    let first = tokio::spawn(async move {
        let candidate =
            DownloadCandidate::audio("https://cdn.example/song", "song.mp3", "https://example/");
        send_media(
            &first_target,
            &candidate,
            &Downloaded { path, thumb: None },
            "base64",
            &first_slots,
        )
        .await
    });
    let call = bounded(rig.calls.recv()).await.unwrap();
    assert_eq!(slots.available_permits(), 0);
    let second_slots = slots.clone();
    let (entered, started) = oneshot::channel();
    let (done, mut completed) = oneshot::channel();
    tokio::spawn(async move {
        let candidate =
            DownloadCandidate::audio("https://cdn.example/song", "song.mp3", "https://example/");
        entered.send(()).unwrap();
        let result = send_media(
            &second_target,
            &candidate,
            &Downloaded {
                path: "/nonexistent-second-media".into(),
                thumb: None,
            },
            "base64",
            &second_slots,
        )
        .await;
        done.send(result).unwrap();
    });
    bounded(started).await.unwrap();
    assert!(
        matches!(
            completed.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ),
        "next file must not be read while the prior media still awaits its receipt"
    );
    assert!(rig.calls.try_recv().is_err());
    call.receipt.send(Ok(Value::Null)).unwrap();
    bounded(first).await.unwrap().unwrap();
    assert!(bounded(completed).await.unwrap().is_err());
    assert_eq!(slots.available_permits(), 1);
    assert!(rig.calls.try_recv().is_err());
    assert!(rig.notifications.try_recv().is_err());
    rig.tasks.shutdown().await;
}

#[tokio::test]
async fn card_receipt_does_not_wait_for_slow_download() {
    let mut rig = Rig::new();
    let (first, wait, target) = rig.request("slow-download");
    let (download, data) = oneshot::channel();
    let completed = rig.followup(target, wait, async move { data.await.unwrap() });
    let card = tokio::spawn(first.send(poster()));
    let call = bounded(rig.calls.recv()).await.unwrap();
    call.receipt
        .send(Ok(json!({"message_id":"poster"})))
        .unwrap();
    bounded(card).await.unwrap().unwrap();
    assert!(rig.notifications.try_recv().is_err());
    assert!(download.send(video("slow")).is_ok());
    let sent = confirm_followup(&mut rig, completed).await;
    assert_eq!(sent["params"]["message"][0]["type"], "video");
    rig.tasks.shutdown().await;
}

struct MarkDrop(Arc<AtomicBool>);

impl Drop for MarkDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

#[tokio::test]
async fn failed_card_cancels_pending_download_and_does_not_retry() {
    let mut rig = Rig::new();
    let (first, wait, target) = rig.request("failed-card");
    let dropped = Arc::new(AtomicBool::new(false));
    let mark = MarkDrop(dropped.clone());
    let completed = rig.followup(target, wait, async move {
        let _mark = mark;
        std::future::pending::<SendableMessage>().await
    });
    let card = tokio::spawn(first.send(poster()));
    let call = bounded(rig.calls.recv()).await.unwrap();
    call.receipt
        .send(Err(NuonoError::MessageSend("upload rejected".into())))
        .unwrap();
    assert!(bounded(card).await.unwrap().is_err());
    assert!(!bounded(completed).await.unwrap());
    assert!(dropped.load(Ordering::SeqCst));
    assert!(rig.notifications.try_recv().is_err());
    assert!(rig.calls.try_recv().is_err());
    rig.tasks.shutdown().await;
}

#[tokio::test]
async fn dropped_card_handle_discards_ready_video_or_failure_notice() {
    let mut rig = Rig::new();
    for message in [
        video("ready"),
        SendableMessage::from("媒体下载失败，已放弃发送媒体。"),
    ] {
        let (first, wait, target) = rig.request("dropped-card");
        let (ready, work_ready) = oneshot::channel();
        let completed = rig.followup(target, wait, async move {
            ready.send(()).unwrap();
            message
        });
        bounded(work_ready).await.unwrap();
        drop(first);
        assert!(!bounded(completed).await.unwrap());
    }
    assert!(rig.notifications.try_recv().is_err());
    assert!(rig.calls.try_recv().is_err());
    rig.tasks.shutdown().await;
}

#[tokio::test]
async fn plugin_shutdown_cancels_card_receipt_and_waiting_video() {
    let mut rig = Rig::new();
    let (first, wait, target) = rig.request("shutdown");
    let completed = rig.followup(target, wait, std::future::ready(video("shutdown")));
    let card = tokio::spawn(first.send(poster()));
    let mut call = bounded(rig.calls.recv()).await.unwrap();
    bounded(rig.tasks.shutdown()).await;
    assert!(bounded(card).await.unwrap().is_err());
    assert!(bounded(completed).await.is_err());
    assert!(rig.notifications.try_recv().is_err());
    bounded(call.receipt.closed()).await;
}

#[tokio::test]
async fn each_request_has_its_own_card_gate() {
    let mut rig = Rig::new();
    let (first_a, wait_a, target_a) = rig.request("request-a");
    let (first_b, wait_b, target_b) = rig.request("request-b");
    let completed_a = rig.followup(target_a, wait_a, std::future::ready(video("a")));
    let completed_b = rig.followup(target_b, wait_b, std::future::ready(video("b")));
    let card_a = tokio::spawn(first_a.send(poster()));
    let card_b = tokio::spawn(first_b.send(poster()));
    let one = bounded(rig.calls.recv()).await.unwrap();
    let two = bounded(rig.calls.recv()).await.unwrap();
    let (call_a, call_b) = if one.action["params"]["message_id"] == "request-a" {
        (one, two)
    } else {
        (two, one)
    };
    call_b
        .receipt
        .send(Ok(json!({"message_id":"poster-b"})))
        .unwrap();
    bounded(card_b).await.unwrap().unwrap();
    let sent_b = confirm_followup(&mut rig, completed_b).await;
    assert_eq!(sent_b["params"]["message_id"], "request-b");
    assert_eq!(
        sent_b["params"]["message"][0]["data"]["file"],
        "file:///tmp/b.mp4"
    );
    assert!(rig.notifications.try_recv().is_err());
    call_a
        .receipt
        .send(Ok(json!({"message_id":"poster-a"})))
        .unwrap();
    bounded(card_a).await.unwrap().unwrap();
    let sent_a = confirm_followup(&mut rig, completed_a).await;
    assert_eq!(sent_a["params"]["message_id"], "request-a");
    assert_eq!(
        sent_a["params"]["message"][0]["data"]["file"],
        "file:///tmp/a.mp4"
    );
    rig.tasks.shutdown().await;
}

#[tokio::test]
async fn non_success_receipt_envelopes_never_release_video() {
    let mut rig = Rig::onebot();
    for receipt in [
        json!({"status":"async", "retcode":0}),
        json!({"status":"unknown", "retcode":0}),
        json!({"status":"ok", "retcode":100}),
        json!({"retcode":1}),
        json!({"status":"ok", "retcode":"0"}),
    ] {
        let (first, wait, target) = rig.request("unconfirmed-card");
        let (ready, work_ready) = oneshot::channel();
        let completed = rig.followup(target, wait, async move {
            ready.send(()).unwrap();
            video("unconfirmed")
        });
        bounded(work_ready).await.unwrap();
        let card = tokio::spawn(first.send(poster()));
        let call = bounded(rig.calls.recv()).await.unwrap();
        call.receipt.send(Ok(receipt.clone())).unwrap();
        assert!(bounded(card).await.unwrap().is_err(), "accepted {receipt}");
        assert!(!bounded(completed).await.unwrap());
        assert!(rig.notifications.try_recv().is_err());
        assert!(rig.calls.try_recv().is_err());
    }
    rig.tasks.shutdown().await;
}

#[tokio::test]
async fn nested_reply_ids_are_preserved_on_card_and_video() {
    let mut rig = Rig::new();
    let event = json!({
        "route": rig.route,
        "post_type": "message",
        "message_type": "group",
        "group_id": "000-group-openid",
        "user_id": "000-user-openid",
        "reply_context": {
            "message_id": "000-nested-message",
            "event_id": "000-nested-event",
            "qq_target_type": "group",
            "group_openid": "000-group-openid",
        },
    });
    let target = delivery::ReplyTarget::new(rig.bot.clone(), &Event::new(&event)).unwrap();
    let (confirmed, wait) = oneshot::channel();
    let first = CardFirst {
        target: target.clone(),
        confirmed,
        tasks: rig.tasks.clone(),
    };
    let completed = rig.followup(target, wait, std::future::ready(video("nested")));
    let card = tokio::spawn(first.send(poster()));
    let call = bounded(rig.calls.recv()).await.unwrap();
    assert!(rig.notifications.try_recv().is_err());
    call.receipt
        .send(Ok(json!({"message_id":"poster"})))
        .unwrap();
    bounded(card).await.unwrap().unwrap();
    let sent = confirm_followup(&mut rig, completed).await;
    for action in [&call.action, &sent] {
        assert_eq!(action["params"]["group_id"], "000-group-openid");
        assert_eq!(action["params"]["group_openid"], "000-group-openid");
        assert_eq!(action["params"]["message_id"], "000-nested-message");
        assert_eq!(action["params"]["event_id"], "000-nested-event");
        assert_eq!(action["params"]["qq_target_type"], "group");
        assert_eq!(action["params"]["reply_context"], event["reply_context"]);
    }
    rig.tasks.shutdown().await;
}

#[test]
fn size_limit_failure_gets_its_own_notice() {
    let oversize = anyhow::Error::new(MediaSizeLimit {
        observed_bytes: 101 * 1024 * 1024,
        maximum_bytes: 100 * 1024 * 1024,
    })
    .context("media preparation failed");
    assert_eq!(
        failure_notice(&oversize, MediaKind::Video),
        "视频文件大小超过配置上限（100 MB），已停止下载，仅发送图文。"
    );
    let other = anyhow::anyhow!("connection reset by peer");
    assert_eq!(
        failure_notice(&other, MediaKind::Audio),
        "音频下载失败，已放弃发送音频。"
    );
    // Arbitrary error strings must not be mistaken for the downloader's limit.
    let untyped = anyhow::anyhow!("upstream says exceeds configured size limit");
    assert_eq!(
        failure_notice(&untyped, MediaKind::Video),
        "视频下载失败，已放弃发送视频。"
    );
}

mod schedule;
