//! Production scheduling checks: real limit decisions and HTTP errors, controlled
//! platform receipts. The proxy serves fixtures without forwarding to any site.

use super::*;
use crate::{
    config::ResolverConfig,
    download::DownloadManager,
    resolver::{Card, ResolvedContent, bilibili::content::VideoContent},
};

fn clip(duration: u64) -> Resolved {
    let mut content = VideoContent(Default::default());
    content.0.title = "scheduled video fixture".into();
    content.0.duration = duration;
    content.0.video_urls = vec!["http://8.8.8.8/fixture.mp4".into()];
    Resolved::new(content)
}

fn state(
    rig: &Rig,
    directory: &tempfile::TempDir,
    duration: u64,
    max_mb: i64,
    proxy: Option<&str>,
) -> Arc<Mutex<RuntimeState>> {
    let config = ResolverConfig {
        enable_media_download: true,
        video_duration_maximum: duration,
        video_max_mb: max_mb,
        download_queue_limit: 1,
        ..Default::default()
    };
    Arc::new(Mutex::new(RuntimeState {
        downloads: Some(
            DownloadManager::new(proxy, 1, 1, max_mb, directory.path().to_owned()).unwrap(),
        ),
        config,
        tasks: Some(rig.tasks.clone()),
        media_delivery_slots: Some(Arc::new(tokio::sync::Semaphore::new(1))),
        host_bot: Some(rig.bot.clone()),
        ..Default::default()
    }))
}

fn event(rig: &Rig) -> Value {
    json!({
        "route": rig.route,
        "post_type": "message",
        "message_type": "group",
        "group_id": "000-group-openid",
        "user_id": "000-user-openid",
        "message_id": "configured-limit-test",
    })
}

async fn confirm_card(rig: &mut Rig, first: CardFirst) {
    let send = tokio::spawn(first.send(poster()));
    let image = bounded(rig.calls.recv()).await.unwrap();
    assert_eq!(image.action["params"]["message"][0]["type"], "image");
    assert!(
        rig.calls.try_recv().is_err(),
        "reason must wait for the image receipt"
    );
    assert!(rig.notifications.try_recv().is_err());
    image.receipt.send(Ok(Value::Null)).unwrap();
    bounded(send).await.unwrap().unwrap();
}

async fn confirm_notice(rig: &mut Rig, expected: &[&str]) {
    let notice = bounded(rig.calls.recv()).await.unwrap();
    let segments = notice.action["params"]["message"].as_array().unwrap();
    assert_eq!(segments.len(), 1);
    assert_eq!(segments[0]["type"], "text");
    let text = segments[0]["data"]["text"].as_str().unwrap();
    for part in expected {
        assert!(text.contains(part), "{text} lacks {part}");
    }
    assert_eq!(
        notice.action["params"]["message_id"],
        "configured-limit-test"
    );
    assert!(
        rig.notifications.try_recv().is_err(),
        "reason must await a platform receipt"
    );
    notice.receipt.send(Ok(Value::Null)).unwrap();
    rig.tasks.shutdown().await;
    assert!(
        rig.calls.try_recv().is_err(),
        "no video may follow a skipped download"
    );
}

#[tokio::test]
async fn production_duration_skip_sends_an_explanation_after_the_card() {
    let mut rig = Rig::onebot();
    let directory = tempfile::tempdir().unwrap();
    let runtime = state(&rig, &directory, 60, 49, None);
    let incoming = event(&rig);
    let first = schedule(&runtime, &Event::new(&incoming), &clip(120)).unwrap();
    assert!(rig.calls.try_recv().is_err());
    confirm_card(&mut rig, first).await;
    confirm_notice(&mut rig, &["时长", "120", "60", "跳过下载"]).await;
    assert_eq!(runtime.lock().unwrap().metrics.skipped_duration, 1);
    assert!(
        std::fs::read_dir(directory.path())
            .unwrap()
            .next()
            .is_none()
    );
}

#[tokio::test]
async fn rejected_card_cancels_the_production_skip_notice() {
    let mut rig = Rig::onebot();
    let directory = tempfile::tempdir().unwrap();
    let runtime = state(&rig, &directory, 60, 49, None);
    let incoming = event(&rig);
    let first = schedule(&runtime, &Event::new(&incoming), &clip(120)).unwrap();
    let send = tokio::spawn(first.send(poster()));
    let image = bounded(rig.calls.recv()).await.unwrap();
    image
        .receipt
        .send(Err(NuonoError::MessageSend(
            "fixture image rejected".into(),
        )))
        .unwrap();
    assert!(bounded(send).await.unwrap().is_err());
    rig.tasks.shutdown().await;
    assert!(rig.calls.try_recv().is_err());
    assert!(rig.notifications.try_recv().is_err());
}

#[tokio::test]
async fn full_download_queue_also_preserves_the_card_then_reason_order() {
    let mut rig = Rig::onebot();
    let directory = tempfile::tempdir().unwrap();
    let runtime = state(&rig, &directory, 480, 49, None);
    let held = runtime
        .lock()
        .unwrap()
        .downloads
        .as_ref()
        .unwrap()
        .try_reserve()
        .unwrap();
    let incoming = event(&rig);
    let first = schedule(&runtime, &Event::new(&incoming), &clip(120)).unwrap();
    confirm_card(&mut rig, first).await;
    confirm_notice(&mut rig, &["队列已满", "跳过下载"]).await;
    assert_eq!(runtime.lock().unwrap().metrics.skipped, 1);
    drop(held);
}

#[tokio::test]
async fn pure_text_content_has_no_media_skip_notice() {
    #[derive(Debug)]
    struct Text;
    impl ResolvedContent for Text {
        fn card(&self) -> Card {
            Card::new().line("ordinary text")
        }
    }
    let rig = Rig::onebot();
    let directory = tempfile::tempdir().unwrap();
    let runtime = state(&rig, &directory, 480, 49, None);
    let incoming = event(&rig);
    assert!(schedule(&runtime, &Event::new(&incoming), &Resolved::new(Text)).is_none());
    rig.tasks.shutdown().await;
}

struct Proxy {
    url: String,
    served: oneshot::Receiver<()>,
    task: tokio::task::JoinHandle<()>,
}
impl Proxy {
    async fn new(response: &'static str) -> Self {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let (served, received) = oneshot::channel();
        let task = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut buffer = [0u8; 1024];
            while !request.windows(4).any(|part| part == b"\r\n\r\n") {
                let size = socket.read(&mut buffer).await.unwrap();
                assert!(size > 0);
                request.extend_from_slice(&buffer[..size]);
                assert!(request.len() <= 16 * 1024);
            }
            let request = String::from_utf8(request).unwrap();
            assert!(request.starts_with("GET http://8.8.8.8/fixture.mp4 "));
            assert!(!request.to_lowercase().contains("\r\ncookie:"));
            socket.write_all(response.as_bytes()).await.unwrap();
            socket.shutdown().await.unwrap();
            served.send(()).unwrap();
        });
        Self {
            url,
            served: received,
            task,
        }
    }
}
impl Drop for Proxy {
    fn drop(&mut self) {
        self.task.abort();
    }
}

#[tokio::test]
async fn actual_declared_and_streamed_size_errors_produce_a_confirmed_followup_notice() {
    for (max_mb, response, limit) in [
        (
            1,
            "HTTP/1.1 200 OK\r\nContent-Type: video/mp4\r\nContent-Length: 1048577\r\nConnection: close\r\n\r\nunused",
            "1 MB",
        ),
        (
            0,
            "HTTP/1.1 200 OK\r\nContent-Type: video/mp4\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n6\r\n123456\r\n0\r\n\r\n",
            "0 MB",
        ),
    ] {
        let mut rig = Rig::onebot();
        let directory = tempfile::tempdir().unwrap();
        let mut proxy = Proxy::new(response).await;
        let runtime = state(&rig, &directory, 480, max_mb, Some(&proxy.url));
        let incoming = event(&rig);
        let first = schedule(&runtime, &Event::new(&incoming), &clip(120)).unwrap();
        bounded(&mut proxy.served).await.unwrap();
        confirm_card(&mut rig, first).await;
        confirm_notice(
            &mut rig,
            &["视频文件大小", "超过配置上限", limit, "停止下载"],
        )
        .await;
        assert_eq!(runtime.lock().unwrap().metrics.failed, 1);
        assert!(
            std::fs::read_dir(directory.path())
                .unwrap()
                .next()
                .is_none()
        );
        bounded(&mut proxy.task).await.unwrap();
    }
}
