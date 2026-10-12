use super::*;
use crate::resolver::ResolvedContent;
use content::map_song;
use serde_json::json;

fn song() -> Value {
    json!({"id":109125,"name":"夏天","al":{"name":"好玖","picUrl":"https://p1.music.126.net/cover.jpg"},"ar":[{"name":"李玖哲"},{"name":"合作歌手"}],"dt":226222,"fee":1})
}
fn source() -> Url {
    Url::parse("https://music.163.com/song?id=109125").unwrap()
}

#[test]
fn reads_song_routes_without_accepting_other_resource_ids() {
    for url in [
        "https://music.163.com/#/song?id=123",
        "https://music.163.com/song/123",
        "https://music.163.com/song?id=123&uct2=share",
        "https://music.163.com/m/song?id=123",
    ] {
        assert_eq!(urls::song_id(&Url::parse(url).unwrap()), Some(123));
    }
    for url in [
        "https://music.163.com/artist?id=123",
        "https://music.163.com/playlist?id=123",
        "https://music.163.com/song?id=0",
        "https://music.163.com/song/not-a-number",
    ] {
        assert_eq!(urls::song_id(&Url::parse(url).unwrap()), None);
    }
}

#[test]
fn preview_is_named_and_shown_as_preview() {
    let content = map_song(
        &song(),
        Some(
            &json!({"data":[{"id":109125,"url":"https://m7.music.126.net/preview.mp3","type":"mp3","time":30040,"freeTrialInfo":{"start":0,"end":30}}]}),
        ),
        &source(),
    );
    let card = content.card();
    assert!(card.text().contains("仅试听：00:30"));
    assert!(card.text().contains("歌曲时长：03:46"));
    let description = card
        .metadata()
        .poster
        .as_ref()
        .unwrap()
        .description
        .as_deref()
        .unwrap();
    assert!(description.contains("仅试听：00:30"));
    assert!(description.contains("歌曲时长：03:46"));
    assert!(card.metadata().video_cover_url.is_none());
    assert_eq!(content.media.author.as_deref(), Some("李玖哲 / 合作歌手"));
    assert_eq!(
        content.download_candidate(0).unwrap().filename,
        "ncm_109125_preview.mp3"
    );
}

#[test]
fn fee_does_not_turn_a_full_source_into_a_preview() {
    let mut song = song();
    song["fee"] = json!(8);
    let content = map_song(
        &song,
        Some(
            &json!({"data":[{"id":109125,"url":"https://m7.music.126.net/full.mp3","freeTrialInfo":null,"type":"mp3"}]}),
        ),
        &source(),
    );
    assert!(!content.card().text().contains("试听"));
    assert_eq!(
        content.download_candidate(0).unwrap().filename,
        "ncm_109125.mp3"
    );
}

#[test]
fn clearly_short_source_without_trial_flag_is_labeled_as_possible_preview() {
    let content = map_song(
        &song(),
        Some(
            &json!({"data":[{"id":109125,"url":"https://m7.music.126.net/preview.mp3","type":"mp3","time":30040}]}),
        ),
        &source(),
    );
    let text = content.card().text();
    assert!(text.contains("音源片段：00:30（可能为试听）"));
    assert!(text.contains("歌曲时长：03:46"));
    assert!(!text.contains("仅试听"));
    assert_eq!(
        content.download_candidate(0).unwrap().filename,
        "ncm_109125_preview.mp3"
    );
}

#[test]
fn encoding_tail_differences_and_unreliable_time_do_not_imply_trial() {
    for time in [226222, 226000, 225000, 150000, 0, 1000] {
        let content = map_song(
            &song(),
            Some(
                &json!({"data":[{"id":109125,"url":"https://m7.music.126.net/full.mp3","freeTrialInfo":null,"type":"mp3","time":time}]}),
            ),
            &source(),
        );
        assert!(!content.card().text().contains("试听"));
        assert!(!content.card().text().contains("片段"));
        assert_eq!(
            content.download_candidate(0).unwrap().filename,
            "ncm_109125.mp3"
        );
    }
}

#[test]
fn unavailable_and_mismatched_sources_keep_public_song_card() {
    for audio in [
        None,
        Some(json!({"data":[{"id":12,"url":"https://m7.music.126.net/wrong.mp3"}]})),
        Some(json!({"data":[{"id":109125,"url":null}]})),
        Some(json!({"data":[{"id":109125,"url":"http://127.0.0.1/private"}]})),
    ] {
        let content = map_song(&song(), audio.as_ref(), &source());
        assert_eq!(content.media.title.as_deref(), Some("夏天"));
        assert!(content.download_candidate(0).is_none());
        assert!(content.card().text().contains("暂未取得可用音源"));
    }
}

#[test]
fn session_validation_prevents_header_injection_and_drops_unrelated_cookies() {
    let pairs = client::session_pairs("MUSIC_U=token; __csrf=csrf; tracking=ignored").unwrap();
    assert_eq!(pairs.len(), 2);
    assert!(client::session_pairs("MUSIC_U=token\r\nX-Test: injected").is_err());
    assert!(client::session_pairs("MUSIC_U=a; MUSIC_U=b").is_err());
}

#[tokio::test]
async fn native_http_contract_sends_encrypted_body_and_only_platform_cookies() {
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut bytes = Vec::new();
        loop {
            let mut chunk = [0; 4096];
            let n = socket.read(&mut chunk).await.unwrap();
            assert!(n > 0 && bytes.len() + n <= 16 * 1024);
            bytes.extend_from_slice(&chunk[..n]);
            if let Some(header_end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
                let headers = String::from_utf8_lossy(&bytes[..header_end]);
                let length = headers
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().unwrap())
                    })
                    .unwrap();
                if bytes.len() >= header_end + 4 + length {
                    break;
                }
            }
        }
        let request = String::from_utf8_lossy(&bytes).to_string();
        assert!(request.starts_with("POST /eapi/song/enhance/player/url/v1 "));
        assert!(
            request.to_ascii_lowercase().contains(
                "cookie: os=pc; appver=3.1.17.204416; MUSIC_U=test-token"
                    .to_ascii_lowercase()
                    .as_str()
            )
        );
        assert!(request.contains("params="));
        assert!(!request.contains("tracking=ignored"));
        let body = r#"{"code":200,"data":[{"id":109125,"url":null}]}"#;
        socket
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .as_bytes(),
            )
            .await
            .unwrap();
    });
    let http = Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let client = NcmClient::new(http).with_base(base);
    let result = client
        .song_audio(109125, "MUSIC_U=test-token; tracking=ignored")
        .await
        .unwrap();
    assert_eq!(result.get("code"), Some(&json!(200)));
    server.await.unwrap();
}

#[tokio::test]
async fn oversized_response_is_rejected_before_reading_body() {
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = [0; 4096];
        socket.read(&mut request).await.unwrap();
        socket
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 4194305\r\nConnection: close\r\n\r\n")
            .await
            .unwrap();
    });
    let http = Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let result = NcmClient::new(http)
        .with_base(base)
        .song_detail(109125)
        .await;
    assert!(result.is_err());
    assert!(result.err().unwrap().to_string().contains("响应过大"));
    server.await.unwrap();
}

async fn share_proxy(
    locations: Vec<&'static str>,
) -> (Client, tokio::task::JoinHandle<Vec<String>>) {
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let mut observed = Vec::new();
        for location in locations {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                let mut bytes = [0; 1024];
                let n = socket.read(&mut bytes).await.unwrap();
                assert!(n > 0 && request.len() + n <= 16384);
                request.extend_from_slice(&bytes[..n]);
            }
            observed.push(String::from_utf8(request).unwrap());
            let response = format!(
                "HTTP/1.1 302 Found\r\nLocation: {location}\r\nSet-Cookie: MUSIC_U=must-not-forward; Path=/\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            );
            socket.write_all(response.as_bytes()).await.unwrap();
        }
        observed
    });
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        reqwest::header::COOKIE,
        "MUSIC_U=must-not-forward".parse().unwrap(),
    );
    let client = Client::builder()
        .no_proxy()
        .proxy(reqwest::Proxy::all(proxy).unwrap())
        .redirect(reqwest::redirect::Policy::none())
        .cookie_store(true)
        .default_headers(headers)
        .build()
        .unwrap();
    (client, server)
}

#[tokio::test]
async fn share_short_links_follow_checked_hops_without_login_cookies() {
    let (http, server) = share_proxy(vec![
        "http://163cn.tv/second",
        "https://music.163.com/#/song?id=109125&uct2=original-share",
    ])
    .await;
    let canonical = urls::canonical_song_url(&http, &Url::parse("http://163cn.tv/first").unwrap())
        .await
        .unwrap();
    assert_eq!(urls::song_id(&canonical), Some(109125));
    let requests = server.await.unwrap();
    assert_eq!(requests.len(), 2);
    assert!(requests[0].starts_with("GET http://163cn.tv/first "));
    assert!(requests[1].starts_with("GET http://163cn.tv/second "));
    assert!(
        requests
            .iter()
            .all(|request| !request.contains("MUSIC_U") && !request.contains("must-not-forward"))
    );
}

#[tokio::test]
async fn share_redirect_to_another_platform_or_private_address_is_rejected() {
    for location in [
        "http://127.0.0.1/admin",
        "https://music.163.com.evil.test/song?id=109125",
        "https://music.163.com:8443/song?id=109125",
    ] {
        let (http, server) = share_proxy(vec![location]).await;
        assert!(
            urls::canonical_song_url(&http, &Url::parse("http://163cn.tv/test").unwrap())
                .await
                .is_err()
        );
        assert_eq!(server.await.unwrap().len(), 1);
    }
}

#[tokio::test]
async fn share_redirect_limit_stops_cycles_after_five_requests() {
    let (http, server) = share_proxy(vec!["http://163cn.tv/cycle"; 5]).await;
    let result =
        urls::canonical_song_url(&http, &Url::parse("http://163cn.tv/cycle").unwrap()).await;
    assert!(result.err().unwrap().to_string().contains("次数过多"));
    assert_eq!(server.await.unwrap().len(), 5);
}

#[tokio::test]
async fn direct_song_and_unsupported_resource_do_not_send_page_requests() {
    let http = Client::builder()
        .no_proxy()
        .proxy(reqwest::Proxy::all("http://127.0.0.1:1").unwrap())
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let url = source();
    assert_eq!(urls::canonical_song_url(&http, &url).await.unwrap(), url);
    for url in [
        "https://music.163.com/artist?id=109125",
        "https://music.163.com/event?id=109125",
        "https://music.163.com/#/playlist?id=109125",
    ] {
        let error = urls::canonical_song_url(&http, &Url::parse(url).unwrap())
            .await
            .err()
            .unwrap();
        assert!(error.to_string().contains("仅支持歌曲"));
    }
}
