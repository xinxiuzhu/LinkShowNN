//! The proxy only serves canned responses. The media host is a public literal,
//! so guards are exercised without external DNS, forwarding or platform access.

use std::{collections::VecDeque, sync::Mutex};

use tokio::{io::AsyncReadExt, net::TcpListener, sync::oneshot, task::JoinHandle};

use super::*;

struct ProxyFixture {
    url: String,
    requests: Arc<Mutex<Vec<String>>>,
    stop: Option<oneshot::Sender<()>>,
    task: Option<JoinHandle<()>>,
}

impl ProxyFixture {
    async fn new(responses: Vec<String>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let observations = requests.clone();
        let (stop, mut stopped) = oneshot::channel();
        let task = tokio::spawn(async move {
            let mut responses = VecDeque::from(responses);
            loop {
                let mut socket = tokio::select! {
                    _ = &mut stopped => return,
                    accepted = listener.accept() => accepted.unwrap().0,
                };
                let request = tokio::time::timeout(Duration::from_secs(5), async {
                    let mut request = Vec::new();
                    let mut buffer = [0; 1024];
                    while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                        let size = socket.read(&mut buffer).await.unwrap();
                        assert!(size > 0, "fixture request ended before headers");
                        request.extend_from_slice(&buffer[..size]);
                        assert!(request.len() <= 16 * 1024);
                    }
                    String::from_utf8(request).unwrap()
                })
                .await
                .expect("fixture request headers timed out");
                let target = request
                    .lines()
                    .next()
                    .unwrap()
                    .split_whitespace()
                    .nth(1)
                    .unwrap();
                assert!(
                    target.starts_with("http://8.8.8.8/"),
                    "fixture never forwards another host"
                );
                observations.lock().unwrap().push(request);
                let response = responses
                    .pop_front()
                    .expect("unexpected extra media request");
                socket.write_all(response.as_bytes()).await.unwrap();
                socket.shutdown().await.unwrap();
            }
        });
        Self {
            url,
            requests,
            stop: Some(stop),
            task: Some(task),
        }
    }

    async fn finish(mut self) -> Vec<String> {
        let _ = self.stop.take().unwrap().send(());
        self.task.take().unwrap().await.unwrap();
        self.requests.lock().unwrap().clone()
    }
}

impl Drop for ProxyFixture {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

fn response(status: &str, content_type: Option<&str>, body: &str) -> String {
    format!(
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\n{}Connection: close\r\n\r\n{body}",
        body.len(),
        content_type
            .map(|mime| format!("Content-Type: {mime}\r\n"))
            .unwrap_or_default(),
    )
}

fn manager(fixture: &ProxyFixture, directory: &tempfile::TempDir) -> DownloadManager {
    DownloadManager::new(Some(&fixture.url), 1, 1, -1, directory.path().to_owned()).unwrap()
}

fn candidate() -> DownloadCandidate {
    DownloadCandidate::video(
        "http://8.8.8.8/primary.mp4",
        "v.mp4",
        "https://www.douyin.com/",
    )
    .with_fallback_urls(["http://8.8.8.8/backup.mp4".to_owned()])
}

async fn download(manager: &DownloadManager, candidate: &DownloadCandidate) -> Result<Downloaded> {
    tokio::time::timeout(
        Duration::from_secs(5),
        manager.try_reserve().unwrap().download(candidate),
    )
    .await
    .expect("download fixture stalled")
}

fn files(directory: &tempfile::TempDir) -> Vec<PathBuf> {
    std::fs::read_dir(directory.path())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect()
}

#[tokio::test]
async fn forbidden_primary_uses_backup_and_keeps_referer_without_cookie() {
    let fixture = ProxyFixture::new(vec![
        response("403 Forbidden", None, ""),
        response("200 OK", Some("video/mp4"), "complete-media"),
    ])
    .await;
    let directory = tempfile::tempdir().unwrap();
    let result = download(&manager(&fixture, &directory), &candidate())
        .await
        .unwrap();
    assert_eq!(std::fs::read(&result.path).unwrap(), b"complete-media");
    assert_eq!(files(&directory), [result.path]);
    let requests = fixture.finish().await;
    assert_eq!(requests.len(), 2);
    assert!(requests[0].starts_with("GET http://8.8.8.8/primary.mp4 "));
    assert!(requests[1].starts_with("GET http://8.8.8.8/backup.mp4 "));
    for request in requests {
        let request = request.to_ascii_lowercase();
        assert!(request.contains("\r\nreferer: https://www.douyin.com/\r\n"));
        assert!(!request.contains("\r\ncookie:"));
    }
}

#[tokio::test]
async fn truncated_primary_is_removed_before_backup_reuses_the_partial_path() {
    let fixture = ProxyFixture::new(vec![
        "HTTP/1.1 200 OK\r\nContent-Type: video/mp4\r\nContent-Length: 128\r\nConnection: close\r\n\r\npartial".into(),
        response("200 OK", None, "backup-only"),
    ]).await;
    let directory = tempfile::tempdir().unwrap();
    let result = download(&manager(&fixture, &directory), &candidate())
        .await
        .unwrap();
    assert_eq!(std::fs::read(&result.path).unwrap(), b"backup-only");
    assert_eq!(files(&directory), [result.path]);
    assert_eq!(fixture.finish().await.len(), 2);
}

#[tokio::test]
async fn html_and_json_are_not_published_and_manual_backups_still_stop_after_three_sources() {
    let fixture = ProxyFixture::new(vec![
        response(
            "200 OK",
            Some("Text/HTML; charset=utf-8"),
            "<html>verification</html>",
        ),
        response("200 OK", Some("application/json"), "{\"error\":1}"),
        response("404 Not Found", None, ""),
        response("200 OK", Some("video/mp4"), "must-not-request"),
    ])
    .await;
    let directory = tempfile::tempdir().unwrap();
    let mut sources = candidate();
    sources.fallback_urls = vec![
        "http://8.8.8.8/backup.mp4".into(),
        "http://8.8.8.8/third.mp4".into(),
        "http://8.8.8.8/fourth.mp4".into(),
    ];
    assert!(
        download(&manager(&fixture, &directory), &sources)
            .await
            .is_err()
    );
    assert!(files(&directory).is_empty());
    assert_eq!(fixture.finish().await.len(), 3);
}

#[tokio::test]
async fn declared_and_streamed_size_limits_do_not_request_smaller_backups() {
    for primary in [
        response("200 OK", Some("video/mp4"), "123456"),
        "HTTP/1.1 200 OK\r\nContent-Type: video/mp4\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n6\r\n123456\r\n0\r\n\r\n".into(),
    ] {
        let fixture = ProxyFixture::new(vec![primary, response("200 OK", Some("video/mp4"), "tiny")]).await;
        let directory = tempfile::tempdir().unwrap();
        let mut downloader = manager(&fixture, &directory);
        downloader.max_bytes = Some(4);
        let error = download(&downloader, &candidate()).await.unwrap_err();
        assert!(error.downcast_ref::<MediaSizeLimit>().is_some());
        assert!(files(&directory).is_empty());
        assert_eq!(fixture.finish().await.len(), 1);
    }
}

#[tokio::test]
async fn private_fallback_and_redirect_are_rejected_before_requesting_the_private_target() {
    let fixture = ProxyFixture::new(vec![response("403 Forbidden", None, "")]).await;
    let directory = tempfile::tempdir().unwrap();
    let private = DownloadCandidate::video(
        "http://8.8.8.8/primary.mp4",
        "v.mp4",
        "https://www.douyin.com/",
    )
    .with_fallback_urls(["http://127.0.0.1/private.mp4".into()]);
    let error = download(&manager(&fixture, &directory), &private)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("private or non-global"));
    assert!(files(&directory).is_empty());
    assert_eq!(fixture.finish().await.len(), 1);

    let fixture = ProxyFixture::new(vec![
        "HTTP/1.1 302 Found\r\nLocation: http://192.168.1.2/private.mp4\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".into(),
        response("200 OK", Some("video/mp4"), "safe-backup"),
    ]).await;
    let result = download(&manager(&fixture, &directory), &candidate())
        .await
        .unwrap();
    assert_eq!(std::fs::read(&result.path).unwrap(), b"safe-backup");
    let requests = fixture.finish().await;
    assert_eq!(requests.len(), 2);
    assert!(
        requests
            .iter()
            .all(|request| request.starts_with("GET http://8.8.8.8/"))
    );
}
