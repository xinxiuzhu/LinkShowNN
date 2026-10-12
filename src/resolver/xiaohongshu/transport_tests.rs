//! 验证电脑端播放源与移动端评论的组合，以及评论补充失败时的降级。

use super::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::oneshot;

const ID: &str = "6ac6146a000000001b021019";

fn pc() -> Value {
    serde_json::json!({"note": {"noteDetailMap": {(ID): {"note": {
        "noteId": ID, "title": "公开测试笔记", "type": "video",
        "user": {"nickname": "作者"}, "interactInfo": {"likedCount": "1728", "commentCount": "44"},
        "imageList": [{"urlDefault": "https://sns.example/cover.jpg"}],
        "video": {"media": {"stream": {"h264": [{
            "width": 720, "height": 1280, "masterUrl": "https://sns.example/pc-h264.mp4"
        }]}}}
    }}}}})
}

fn mobile() -> Value {
    serde_json::json!({"noteData": {"data": {
        "noteData": {"noteId": ID, "title": "移动端标题", "type": "video",
            "imageList": [{"url": "https://sns.example/cover.jpg"}],
            "video": {"consumer": {"originVideoKey": "pre_post/original_hevc"}},
            "interactInfo": {"likedCount": "1.7万", "commentCount": "44"}},
        "commentData": {"comments": (1..=5).map(|index| serde_json::json!({
            "id": format!("{index:024x}"), "noteId": ID, "status": 0,
            "content": "评论[皱眉R]", "likeCount": 0, "likeViewCount": index.to_string(),
            "user": {"nickname": format!("评论者{index}"), "image": "https://sns.example/avatar.jpg"},
            "pictures": [{"url": "https://sns.example/comment.jpg", "width": 100, "height": 200}]
        })).collect::<Vec<_>>()}
    }}})
}

async fn resolve_with_mobile(status: u16, state: Value) -> Resolved {
    let path = format!("explore/{ID}?xsec_token=fixture");
    resolve_responses(
        vec![response(200, pc()), response(status, state)],
        vec![path.clone(), path],
    )
    .await
}

fn response(status: u16, state: Value) -> String {
    let body = format!("<script>window.__INITIAL_STATE__={state}</script>");
    format!(
        "HTTP/1.1 {status} Fixture\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

async fn resolve_responses(responses: Vec<String>, paths: Vec<String>) -> Resolved {
    resolve_responses_with_comments(responses, paths, true).await
}

async fn resolve_responses_with_comments(
    responses: Vec<String>,
    paths: Vec<String>,
    comments_enabled: bool,
) -> Resolved {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy = format!("http://{}", listener.local_addr().unwrap());
    let (finished, mut finish_signal) = oneshot::channel::<()>();
    let fixture = tokio::spawn(async move {
        let mut requests = Vec::new();
        let mut responses = responses.into_iter();
        loop {
            let (mut socket, _) = tokio::select! {
                biased;
                accepted = listener.accept() => accepted.unwrap(),
                _ = &mut finish_signal => break,
            };
            let mut request = Vec::new();
            while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                let mut chunk = [0u8; 1024];
                let read = socket.read(&mut chunk).await.unwrap();
                assert!(read > 0);
                request.extend_from_slice(&chunk[..read]);
            }
            requests.push(String::from_utf8(request).unwrap());
            let response = responses
                .next()
                .unwrap_or_else(|| response(500, Value::Null));
            socket.write_all(response.as_bytes()).await.unwrap();
            socket.shutdown().await.unwrap();
        }
        requests
    });
    let client = reqwest::Client::builder()
        .no_proxy()
        .proxy(reqwest::Proxy::all(proxy).unwrap())
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(3))
        .build()
        .unwrap();
    let fetcher = PageFetcher::new(client);
    let content = XiaohongshuAdapter::new(fetcher, String::new())
        .with_comments_enabled(comments_enabled)
        .resolve(
            &Url::parse(&format!(
                "http://www.xiaohongshu.com/explore/{ID}?xsec_token=fixture"
            ))
            .unwrap(),
        )
        .await
        .unwrap();
    finished.send(()).unwrap();
    let requests = tokio::time::timeout(Duration::from_secs(5), fixture)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(requests.len(), paths.len());
    assert!(
        requests[..requests.len() - 1]
            .iter()
            .all(|request| request.contains(PC_USER_AGENT))
    );
    let last_user_agent = if requests.len() == 1 {
        PC_USER_AGENT
    } else {
        crate::resolver::fetch::MOBILE_USER_AGENT
    };
    assert!(requests.last().unwrap().contains(last_user_agent));
    for (request, path) in requests.iter().zip(paths) {
        assert!(request.starts_with(&format!("GET http://www.xiaohongshu.com/{path} ")));
    }
    assert!(
        requests
            .iter()
            .all(|request| !request.to_ascii_lowercase().contains("\r\ncookie:"))
    );
    content
}

#[tokio::test]
async fn pc_login_redirect_does_not_replace_the_mobile_note_request() {
    let path = format!("explore/{ID}?xsec_token=fixture");
    let content = resolve_responses(
        vec![
            "HTTP/1.1 302 Found\r\nLocation: http://www.xiaohongshu.com/login\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".into(),
            response(200, Value::Null),
            response(200, mobile()),
        ], vec![path.clone(), "login".into(), path],
    ).await;
    let card = content.card();
    assert_eq!(card.metadata().comments.len(), 3);
    assert_eq!(card.metadata().poster.as_ref().unwrap().title, "移动端标题");
    assert!(content.download_candidate(0).is_some());
}

#[tokio::test]
async fn successful_pc_video_also_gets_mobile_comments_without_replacing_the_stream() {
    let content = resolve_with_mobile(200, mobile()).await;
    let candidate = content.download_candidate(0).unwrap();
    assert_eq!(candidate.url, "https://sns.example/pc-h264.mp4");
    let card = content.card();
    assert!(!card.is_gallery());
    let poster = card.metadata().poster.as_ref().unwrap();
    assert_eq!(poster.title, "公开测试笔记");
    assert_eq!(poster.video_quality.as_deref(), Some("720P"));
    assert!(poster.quality_notice.as_deref().unwrap().contains("720P"));
    assert_eq!(poster.like_count, Some(1728));
    assert_eq!(poster.stats_display.likes, None);
    assert_eq!(poster.comment_count, Some(44));
    let comments = &card.metadata().comments;
    assert_eq!(
        comments
            .iter()
            .map(|comment| comment.like_count)
            .collect::<Vec<_>>(),
        [5, 4, 3]
    );
    assert!(comments.iter().all(|comment| comment.avatar_url.is_some()
        && comment.emotes.len() == 1
        && comment.pictures.len() == 1));
}

#[tokio::test]
async fn comments_disabled_skips_mobile_when_pc_already_has_a_video() {
    let path = format!("explore/{ID}?xsec_token=fixture");
    let content =
        resolve_responses_with_comments(vec![response(200, pc())], vec![path], false).await;
    assert_eq!(
        content.download_candidate(0).unwrap().url,
        "https://sns.example/pc-h264.mp4"
    );
    let card = content.card();
    assert!(card.metadata().comments.is_empty());
    assert_eq!(
        card.metadata()
            .poster
            .as_ref()
            .unwrap()
            .video_quality
            .as_deref(),
        Some("720P")
    );
}

#[tokio::test]
async fn comments_disabled_still_uses_mobile_for_a_missing_pc_playback_source() {
    let mut pc = pc();
    pc["note"]["noteDetailMap"][ID]["note"]["video"] = Value::Null;
    let path = format!("explore/{ID}?xsec_token=fixture");
    let content = resolve_responses_with_comments(
        vec![response(200, pc), response(200, mobile())],
        vec![path.clone(), path],
        false,
    )
    .await;
    assert_eq!(
        content.download_candidate(0).unwrap().url,
        "https://sns-video-bd.xhscdn.com/pre_post/original_hevc"
    );
    let card = content.card();
    assert!(card.metadata().comments.is_empty());
    let poster = card.metadata().poster.as_ref().unwrap();
    assert_eq!(poster.video_quality, None);
    assert!(
        poster
            .quality_notice
            .as_deref()
            .unwrap()
            .contains("无法确认实际清晰度")
    );
}

#[tokio::test]
async fn comment_http_failure_or_mismatched_note_retains_the_valid_pc_result() {
    let mut mismatch = mobile();
    mismatch["noteData"]["data"]["noteData"]["noteId"] =
        serde_json::json!("6ac6146a000000001b021020");
    for (status, state) in [(403, Value::Null), (200, mismatch)] {
        let content = resolve_with_mobile(status, state).await;
        assert!(content.download_candidate(0).is_some());
        assert!(content.card().metadata().comments.is_empty());
        assert_eq!(
            content
                .card()
                .metadata()
                .poster
                .as_ref()
                .unwrap()
                .like_count,
            Some(1728)
        );
    }
}
