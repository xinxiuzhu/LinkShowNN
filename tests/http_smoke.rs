//! Exercises the real HTTP page fetcher and adapters through a local-only
//! proxy fixture. Normal test runs never forward requests or contact a platform.
//! The custom entry point also exposes an explicit anonymous media probe.

// These modules include #[cfg(test)] imports; harness=false does not collect
// their unit tests. Keep that suppression confined to this fixture.
#[allow(dead_code, unused_imports)]
#[path = "../src/config/mod.rs"]
mod config;
#[allow(dead_code, unused_imports)]
#[path = "../src/download/manager.rs"]
mod download_manager;
#[allow(dead_code, unused_imports)]
#[path = "../src/download/model.rs"]
mod download_model;
#[allow(dead_code, unused_imports)]
mod download {
    pub(crate) use super::download_model as model;
    pub use super::download_model::*;
}
#[allow(dead_code, unused_imports)]
#[path = "../src/resolver/mod.rs"]
mod resolver;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result, ensure};
use config::ResolverConfig;
use download::{DownloadCandidate, Downloaded, MediaKind};
use resolver::{ParserManager, Platform, fetch::PageFetcher};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    task::JoinSet,
    time::timeout,
};
use url::Url;

const XHS_PAGE: &str = r#"<!doctype html><html><head><title>static XHS</title></head><body>
<script>window.__INITIAL_STATE__ = {"note":{"noteId":"111111111111111111111111","type":"normal","title":"static XHS","user":{"nickname":"XHS author","avatar":"http://www.xiaohongshu.com/media/avatar.jpg"},"time":1700000000123,"imageList":[{"urlDefault":"http://www.xiaohongshu.com/media/xhs.jpg"}]}}</script>
</body></html>"#;

const KS_PAGE: &str = r#"<!doctype html><html><head><title>static KS</title></head><body>
<script>window.INIT_STATE = {"photo":{"caption":"static KS","userName":"KS author","mainMvUrls":[{"url":"http://www.kuaishou.com/media/ks.mp4"}]}}</script>
</body></html>"#;

const QQ_PAGE: &str = r#"<!doctype html><html><head><title>static QQ</title>
<meta property="og:title" content="static QQ"></head><body>
<video src="http://qchannelvideo.photo.qq.com/fixture.mp4"></video>
</body></html>"#;

const DY_PAGE: &str = r#"<!doctype html><html><head><title>static DY</title></head><body>
<script>window._ROUTER_DATA = {"loaderData":{"page":{"videoInfoRes":{"item_list":[{"desc":"static DY"}]}}}}</script>
</body></html>"#;

#[derive(Debug, Clone)]
struct ObservedRequest {
    host: String,
    path: String,
    cookie: Option<String>,
}

type Observations = Arc<Mutex<Vec<ObservedRequest>>>;

fn main() -> Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    let arguments: Vec<_> = std::env::args().skip(1).collect();
    if arguments.first().map(String::as_str) == Some("--media-probe") {
        let target = arguments
            .get(1)
            .context("probe requires one public media URL")?;
        let mode = arguments.get(2).map(String::as_str).unwrap_or("http");
        return runtime.block_on(media_probe(target, mode));
    }
    runtime.block_on(smoke())
}

/// Uses only empty/default credentials. Downloads are bounded and send no message.
/// The public network is used only when the caller explicitly selects --media-probe.
async fn media_probe(target: &str, mode: &str) -> Result<()> {
    ensure!(
        matches!(mode, "http" | "http-download"),
        "unknown media probe mode"
    );
    let target = Url::parse(target)?;
    let proxy = std::env::var("NUONO_MEDIA_PROBE_PROXY").unwrap_or_default();
    let parser = ParserManager::new(ResolverConfig {
        proxy: proxy.clone(),
        render_cards: false,
        show_comments: std::env::var("NUONO_MEDIA_PROBE_SHOW_COMMENTS")
            .map(|value| value != "false")
            .unwrap_or(true),
        bili_quality: std::env::var("NUONO_MEDIA_PROBE_BILI_QUALITY")
            .unwrap_or_else(|_| "720P".into()),
        douyin_quality: std::env::var("NUONO_MEDIA_PROBE_DOUYIN_QUALITY")
            .unwrap_or_else(|_| "1080P".into()),
        xhs_quality: std::env::var("NUONO_MEDIA_PROBE_XHS_QUALITY")
            .unwrap_or_else(|_| "1080P".into()),
        ..ResolverConfig::default()
    })?;
    let matched = parser
        .match_url(target.as_str())
        .context("probe link was not matched")?;
    ensure!(
        matches!(
            matched.platform,
            Platform::Douyin
                | Platform::Xiaohongshu
                | Platform::Bilibili
                | Platform::Ncm
                | Platform::QqMusic
        ),
        "unsupported media probe platform"
    );
    let content = parser.resolve(&matched).await?;
    let card = content.card();
    println!(
        "{}",
        serde_json::json!({"engine":"http-api", "platform":matched.platform.as_str(),
        "work_metadata":true,"cover":card.image_urls().next().is_some(),
        "images":card.image_urls().count(),"gallery":card.metadata().gallery,
        "author":card.metadata().author.is_some(),"avatar":card.metadata().avatar_url.is_some(),
        "comments":card.metadata().comments.len(),
        "video_candidate":content.download_candidate(0).is_some_and(|candidate|candidate.kind == MediaKind::Video),
        "audio_candidate":content.download_candidate(0).is_some_and(|candidate|candidate.kind == MediaKind::Audio),
        "preview":content.download_candidate(0).is_some_and(|candidate|candidate.filename.contains("_preview")),
        "video_quality":card.metadata().poster.as_ref().and_then(|poster|poster.video_quality.as_deref()),
        "quality_notice":card.metadata().poster.as_ref().and_then(|poster|poster.quality_notice.as_deref()),
        "backup_sources":content.download_candidate(0).map(|candidate|candidate.fallback_urls.len())})
    );
    if mode == "http-download" {
        let candidate = content
            .download_candidate(0)
            .context("probe found no video to download")?;
        let directory = tempfile::Builder::new()
            .prefix("linkshownn-media-probe-")
            .tempdir()?;
        let manager = download_manager::DownloadManager::new(
            (!proxy.is_empty()).then_some(proxy.as_str()),
            1,
            1,
            32,
            directory.path().to_path_buf(),
        )?;
        let downloaded = manager
            .try_reserve()
            .context("probe download queue unavailable")?
            .download(&candidate)
            .await?;
        let bytes = tokio::fs::metadata(&downloaded.path).await?.len();
        let mut file = tokio::fs::File::open(&downloaded.path).await?;
        let mut prefix = [0_u8; 12];
        file.read_exact(&mut prefix).await?;
        if candidate.kind == MediaKind::Video {
            ensure!(
                &prefix[4..8] == b"ftyp",
                "probe download is not an MP4 file"
            );
        } else {
            ensure!(
                prefix.starts_with(b"ID3")
                    || prefix[0] == 0xff
                    || prefix.starts_with(b"fLaC")
                    || prefix.starts_with(b"OggS")
                    || prefix.starts_with(b"RIFF")
                    || &prefix[4..8] == b"ftyp",
                "probe download is not a recognized audio file"
            );
        }
        drop(file);
        let _kept = directory.keep();
        println!(
            "{}",
            serde_json::json!({"downloaded_bytes":bytes,
            "media_path":downloaded.path,"thumbnail_path":downloaded.thumb})
        );
    }
    Ok(())
}

async fn smoke() -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .context("bind local fixture proxy")?;
    let proxy = format!("http://{}", listener.local_addr()?);
    let observations = Observations::default();
    let server_observations = observations.clone();
    let server = tokio::spawn(async move {
        let mut clients = JoinSet::new();
        loop {
            tokio::select! {
                accepted = listener.accept() => {
                    let (stream, _) = accepted?;
                    let observations = server_observations.clone();
                    clients.spawn(async move {
                        timeout(Duration::from_secs(5), serve_fixture(stream, observations))
                            .await.context("fixture client timed out")?
                    });
                }
                completed = clients.join_next(), if !clients.is_empty() => {
                    completed.context("fixture client disappeared")??.context("fixture request failed")?;
                }
            }
        }
        #[allow(unreachable_code)]
        Ok::<(), anyhow::Error>(())
    });
    let config = ResolverConfig {
        proxy,
        xhs_cookie: "a=1; token=abc==".into(),
        ..ResolverConfig::default()
    };
    let result = exercise(&config, &observations).await;
    if server.is_finished() {
        server.await.context("fixture proxy task panicked")??;
        anyhow::bail!("fixture proxy stopped unexpectedly");
    }
    server.abort();
    let _ = server.await;
    result?;
    println!("linkshownn HTTP smoke passed (local proxy, no external site or database)");
    Ok(())
}

async fn exercise(config: &ResolverConfig, observations: &Observations) -> Result<()> {
    let parser = ParserManager::new(config.clone())?;
    for (url, title, video) in [
        ("http://xhslink.com/short", "static XHS", None),
        (
            "http://www.kuaishou.com/static",
            "static KS",
            Some("http://www.kuaishou.com/media/ks.mp4"),
        ),
        (
            "http://pd.qq.com/static",
            "static QQ",
            Some("http://qchannelvideo.photo.qq.com/fixture.mp4"),
        ),
    ] {
        let matched = parser
            .match_url(url)
            .context("fixture URL was not recognized")?;
        let content = parser.resolve(&matched).await?;
        ensure!(
            content.card().text().starts_with(&format!("{title}\n")),
            "HTTP adapter lost inline page metadata"
        );
        ensure!(
            content.download_candidate(0).map(|candidate| candidate.url)
                == video.map(str::to_owned),
            "HTTP adapter mapped the wrong media source"
        );
        if matched.platform == Platform::Xiaohongshu {
            let card = content.card();
            ensure!(
                card.image_urls().collect::<Vec<_>>()
                    == ["http://www.xiaohongshu.com/media/xhs.jpg"],
                "XHS lost inline images"
            );
            ensure!(
                card.metadata().author.as_deref() == Some("XHS author")
                    && card.metadata().avatar_url.as_deref()
                        == Some("http://www.xiaohongshu.com/media/avatar.jpg")
                    && card.metadata().published_at == Some(1_700_000_000),
                "XHS lost inline author metadata"
            );
        }
    }

    let http = reqwest::Client::builder()
        .no_proxy()
        .proxy(reqwest::Proxy::all(&config.proxy)?)
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(5))
        .build()?;
    let fetcher = PageFetcher::new(http);
    let cookie = Some(config.xhs_cookie.as_str());
    let redirected = fetcher
        .get_page(
            &Url::parse("http://www.xiaohongshu.com/back-to-short")?,
            Platform::Xiaohongshu,
            cookie,
        )
        .await?;
    ensure!(
        redirected.url.as_str() == "http://www.xiaohongshu.com/explore/111111111111111111111111",
        "HTTP redirect did not return the expanded platform URL"
    );
    let douyin = fetcher
        .get_page(
            &Url::parse("http://www.douyin.com/share/video/1")?,
            Platform::Douyin,
            None,
        )
        .await?;
    ensure!(douyin.html.contains("static DY"), "HTTP page body was lost");

    let count = observations
        .lock()
        .expect("fixture observations lock")
        .len();
    for source in [
        "http://www.xiaohongshu.com/cross",
        "http://www.xiaohongshu.com/private",
        "http://www.xiaohongshu.com/oversized",
    ] {
        ensure!(
            fetcher
                .get_page(&Url::parse(source)?, Platform::Xiaohongshu, cookie)
                .await
                .is_err(),
            "unsafe redirect or oversized response was accepted"
        );
    }
    {
        let requests = observations.lock().expect("fixture observations lock");
        ensure!(
            requests.len() == count + 3,
            "rejected redirects unexpectedly reached their target"
        );
        for request in requests
            .iter()
            .filter(|request| request.host == "xhslink.com")
        {
            ensure!(
                request.cookie.is_none(),
                "login cookies leaked to the short-link domain"
            );
        }
        for request in requests
            .iter()
            .filter(|request| request.host == "www.xiaohongshu.com")
        {
            ensure!(
                request.cookie.as_deref() == cookie,
                "main-site cookie lost a pair or value padding"
            );
        }
        for request in requests.iter().filter(|request| {
            request.host != "www.xiaohongshu.com" && request.host != "xhslink.com"
        }) {
            ensure!(
                request.cookie.is_none(),
                "XHS cookies leaked to another platform"
            );
        }
        ensure!(
            requests
                .iter()
                .any(|request| request.host == "xhslink.com" && request.path == "/short"),
            "short-link expansion was not exercised"
        );
    }
    let count = observations
        .lock()
        .expect("fixture observations lock")
        .len();
    for source in [
        "http://pd.qq.com/final",
        "http://www.xiaohongshu.com.evil.test/note",
        "http://www.xiaohongshu.com:8080/note",
        "http://127.0.0.1/note",
    ] {
        ensure!(
            fetcher
                .get_page(&Url::parse(source)?, Platform::Xiaohongshu, cookie)
                .await
                .is_err(),
            "invalid starting URL was accepted"
        );
    }
    ensure!(
        observations
            .lock()
            .expect("fixture observations lock")
            .len()
            == count,
        "invalid starting URL reached the network"
    );
    Ok(())
}

async fn serve_fixture(mut stream: TcpStream, observations: Observations) -> Result<()> {
    let mut bytes = Vec::new();
    let mut chunk = [0_u8; 2048];
    while !bytes.windows(4).any(|window| window == b"\r\n\r\n") {
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            return Ok(());
        }
        bytes.extend_from_slice(&chunk[..read]);
        ensure!(
            bytes.len() <= 64 * 1024,
            "fixture request headers too large"
        );
    }
    let request = std::str::from_utf8(&bytes).context("fixture received non-UTF8 headers")?;
    let mut lines = request.split("\r\n");
    let first = lines.next().context("fixture request has no method line")?;
    let mut first = first.split_whitespace();
    let method = first.next().unwrap_or_default();
    let target = first.next().unwrap_or_default();
    if method != "GET" {
        return respond(&mut stream, "403 Forbidden", "", "").await;
    }
    // No DNS, forwarding, CONNECT tunnelling or alternate upstream transport.
    let url = Url::parse(target).context("fixture expected an absolute HTTP proxy URL")?;
    ensure!(url.scheme() == "http", "fixture only serves HTTP");
    let cookie = lines
        .filter_map(|line| line.split_once(':'))
        .find(|(key, _)| key.eq_ignore_ascii_case("cookie"))
        .map(|(_, value)| value.trim().to_owned());
    let host = url.host_str().context("fixture URL has no host")?;
    observations
        .lock()
        .expect("fixture observations lock")
        .push(ObservedRequest {
            host: host.to_owned(),
            path: url.path().to_owned(),
            cookie,
        });
    let (status, extra, body) = match (host, url.path()) {
        ("xhslink.com", "/short") => (
            "302 Found",
            "Location: http://www.xiaohongshu.com/explore/111111111111111111111111\r\n",
            "",
        ),
        ("www.xiaohongshu.com", "/back-to-short") => {
            ("302 Found", "Location: http://xhslink.com/short\r\n", "")
        }
        ("www.xiaohongshu.com", "/explore/111111111111111111111111") => ("200 OK", "", XHS_PAGE),
        ("www.kuaishou.com", "/static") => ("200 OK", "", KS_PAGE),
        ("pd.qq.com", "/static") => ("200 OK", "", QQ_PAGE),
        ("www.douyin.com", "/share/video/1") => ("200 OK", "", DY_PAGE),
        ("www.xiaohongshu.com", "/cross") => {
            ("302 Found", "Location: http://pd.qq.com/final\r\n", "")
        }
        ("www.xiaohongshu.com", "/private") => {
            ("302 Found", "Location: http://127.0.0.1/private\r\n", "")
        }
        ("www.xiaohongshu.com", "/oversized") => {
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 8388609\r\nConnection: close\r\n\r\n",
                )
                .await?;
            stream.shutdown().await?;
            return Ok(());
        }
        _ => ("404 Not Found", "", "unknown local fixture"),
    };
    respond(&mut stream, status, extra, body).await
}

async fn respond(stream: &mut TcpStream, status: &str, extra: &str, body: &str) -> Result<()> {
    let headers = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n{extra}\r\n",
        body.len()
    );
    stream.write_all(headers.as_bytes()).await?;
    stream.write_all(body.as_bytes()).await?;
    stream.shutdown().await?;
    Ok(())
}
