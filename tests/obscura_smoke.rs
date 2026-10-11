//! Exercises the plugin's real page fetcher and adapters through a local-only
//! HTTP proxy fixture. It never forwards requests or contacts a real platform.
//! V8 initialization needs a synchronous main thread, hence harness = false.

// These modules include #[cfg(test)] imports; harness=false does not collect
// their unit tests. Keep that suppression confined to this fixture.
#[allow(dead_code, unused_imports)]
#[path = "../src/default_config.rs"]
mod default_config;
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
use default_config::ResolverConfig;
use nuono_plugin::{
    PluginTaskScope,
    browser::{self, PluginBrowser},
};
use resolver::{ParserManager, Platform, fetch::PageFetcher};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    task::JoinSet,
    time::timeout,
};
use url::Url;

const XHS_PAGE: &str = r#"<!doctype html><html><head><title>static XHS</title></head><body>
<script>window.__INITIAL_STATE__ = {"note":{"title":"static XHS","imageList":[]}}</script>
<script>setTimeout(() => {
  window.__INITIAL_STATE__ = {note:{title:'dynamic XHS',user:{nickname:'XHS author',avatar:'http://www.xiaohongshu.com/media/avatar.jpg'},time:1700000000123,imageList:[{urlDefault:'http://www.xiaohongshu.com/media/xhs.jpg'}]}};
}, 20);</script></body></html>"#;

const KS_PAGE: &str = r#"<!doctype html><html><head><title>static KS</title></head><body>
<script>window.INIT_STATE = {"photo":{"caption":"static KS"}}</script>
<script>setTimeout(() => {
  window.INIT_STATE = {photo:{caption:'dynamic KS',userName:'KS author',mainMvUrls:[{url:'http://www.kuaishou.com/media/ks.mp4'}]}};
}, 20);</script></body></html>"#;

const QQ_PAGE: &str = r#"<!doctype html><html><head><title>static QQ</title>
<meta property="og:title" content="static QQ"></head><body>
<script>setTimeout(() => {
  document.querySelector('meta[property="og:title"]').setAttribute('content', 'dynamic QQ');
  const video = document.createElement('video');
  video.setAttribute('src', 'http://' + 'qchannelvideo.photo.qq.com' + '/fixture' + '.mp4');
  document.body.appendChild(video);
}, 20);</script></body></html>"#;

const DY_PAGE: &str = r#"<!doctype html><html><head><title>static DY</title></head><body>
<script>setTimeout(() => {
  window._ROUTER_DATA = {loaderData:{page:{videoInfoRes:{item_list:[{desc:'dynamic DY'}]}}}};
}, 20);</script></body></html>"#;

const CROSS_PLATFORM_PAGE: &str = r#"<!doctype html><html><head><title>origin</title></head><body>
<script>setTimeout(() => { location.href = 'http://pd.qq.com/final'; }, 20);</script>
</body></html>"#;

const JS_SHORT_PAGE: &str = r#"<!doctype html><html><head><title>short URL</title></head><body>
<script>setTimeout(() => { location.href = 'http://www.xiaohongshu.com/dynamic'; }, 20);</script>
</body></html>"#;

#[derive(Debug, Clone)]
struct ObservedRequest {
    host: String,
    path: String,
    cookie: Option<String>,
}

type Observations = Arc<Mutex<Vec<ObservedRequest>>>;

fn main() -> Result<()> {
    browser::initialize().context("initialize Obscura before Tokio")?;
    ensure!(browser::is_available(), "obscura feature is required");
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?
        .block_on(smoke())
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
    let scope = Arc::new(PluginTaskScope::new("linkshownn-obscura-smoke"));
    let browser = PluginBrowser::for_scope(scope.clone());
    let config = ResolverConfig {
        proxy,
        obscura_enabled: true,
        obscura_timeout_seconds: 10,
        obscura_settle_ms: 100,
        xhs_cookie: "a=1; token=abc==".into(),
        ..ResolverConfig::default()
    };

    let result = exercise(&config, browser, scope.clone(), &observations).await;
    let shutdown = timeout(Duration::from_secs(10), scope.shutdown()).await;
    if server.is_finished() {
        server.await.context("fixture proxy task panicked")??;
        anyhow::bail!("fixture proxy stopped unexpectedly");
    }
    server.abort();
    let _ = server.await;
    result?;
    shutdown.context("browser workers did not leave the plugin scope")?;
    println!(
        "linkshownn Obscura smoke passed (real V8, local proxy, no external site or database)"
    );
    Ok(())
}

async fn exercise(
    config: &ResolverConfig,
    browser: PluginBrowser,
    scope: Arc<PluginTaskScope>,
    observations: &Observations,
) -> Result<()> {
    let parser = ParserManager::new(config.clone(), browser.clone(), scope.clone())?;
    let resolve = |url: &str| {
        parser
            .match_url(url)
            .context("fixture URL was not recognized")
    };

    let xhs = parser
        .resolve(&resolve("http://xhslink.com/short")?)
        .await?;
    ensure!(
        xhs.card().text().starts_with("dynamic XHS\n"),
        "XHS did not consume timer-updated state"
    );
    ensure!(
        xhs.card().image_urls().collect::<Vec<_>>() == ["http://www.xiaohongshu.com/media/xhs.jpg"],
        "XHS lost images from executed state"
    );
    let xhs_card = xhs.card();
    ensure!(
        xhs_card.metadata().author.as_deref() == Some("XHS author")
            && xhs_card.metadata().avatar_url.as_deref()
                == Some("http://www.xiaohongshu.com/media/avatar.jpg")
            && xhs_card.metadata().published_at == Some(1_700_000_000),
        "XHS lost author metadata from executed state"
    );
    {
        let requests = observations.lock().expect("fixture observations lock");
        let short = requests
            .iter()
            .find(|request| request.host == "xhslink.com" && request.path == "/short")
            .context("short URL never reached local proxy")?;
        ensure!(
            short.cookie.is_none(),
            "login cookies leaked to short-link domain"
        );
        let main = requests
            .iter()
            .find(|request| request.host == "www.xiaohongshu.com" && request.path == "/dynamic")
            .context("expanded XHS URL never reached local proxy")?;
        let cookies = main
            .cookie
            .as_deref()
            .context("XHS browser request lost cookies")?;
        let pairs = cookies.split(';').map(str::trim).collect::<Vec<_>>();
        ensure!(
            pairs.contains(&"a=1") && pairs.contains(&"token=abc=="),
            "cookie split lost a pair or value padding"
        );
    }

    let before_js_short = observations
        .lock()
        .expect("fixture observations lock")
        .len();
    let xhs_js = parser
        .resolve(&resolve("http://xhslink.com/js-short")?)
        .await?;
    ensure!(
        xhs_js.card().text().starts_with("dynamic XHS\n"),
        "JS-only short URL did not reach XHS content"
    );
    {
        let requests = observations.lock().expect("fixture observations lock");
        let requests = &requests[before_js_short..];
        ensure!(
            requests
                .iter()
                .any(|request| request.host == "xhslink.com" && request.path == "/js-short"),
            "JS-only short URL never reached the fixture"
        );
        ensure!(
            requests
                .iter()
                .filter(|request| request.host == "xhslink.com")
                .all(|request| request.cookie.is_none()),
            "JS-only short URL received login cookies"
        );
        let main = requests
            .iter()
            .rev()
            .find(|request| request.host == "www.xiaohongshu.com" && request.path == "/dynamic")
            .context("JS-only short URL never loaded the main site")?;
        let cookies = main
            .cookie
            .as_deref()
            .context("JS-only short URL lost main-site cookies")?;
        let pairs = cookies.split(';').map(str::trim).collect::<Vec<_>>();
        ensure!(
            pairs.contains(&"a=1") && pairs.contains(&"token=abc=="),
            "JS-only short URL did not seed all main-site cookies"
        );
    }

    let ks = parser
        .resolve(&resolve("http://www.kuaishou.com/dynamic")?)
        .await?;
    ensure!(
        ks.card().text().starts_with("dynamic KS\n"),
        "KS did not consume executed state"
    );
    ensure!(
        ks.download_candidate(0).context("KS lost video")?.url
            == "http://www.kuaishou.com/media/ks.mp4",
        "KS mapped the wrong dynamic video"
    );
    let qq = parser
        .resolve(&resolve("http://pd.qq.com/dynamic")?)
        .await?;
    ensure!(
        qq.card().text().starts_with("dynamic QQ\n"),
        "QQ did not consume dynamic metadata"
    );
    ensure!(
        qq.download_candidate(0).context("QQ lost DOM video")?.url
            == "http://qchannelvideo.photo.qq.com/fixture.mp4",
        "QQ did not extract dynamically-created video"
    );

    let http_parser = ParserManager::new(
        ResolverConfig {
            obscura_enabled: false,
            ..config.clone()
        },
        browser.clone(),
        scope.clone(),
    )?;
    for (url, title) in [
        ("http://www.xiaohongshu.com/dynamic", "static XHS"),
        ("http://www.kuaishou.com/dynamic", "static KS"),
        ("http://pd.qq.com/dynamic", "static QQ"),
    ] {
        let matched = http_parser
            .match_url(url)
            .context("HTTP fixture URL was not recognized")?;
        let content = http_parser.resolve(&matched).await?;
        ensure!(
            content.card().text().starts_with(&format!("{title}\n")),
            "HTTP mode unexpectedly used executed state"
        );
        ensure!(
            content.download_candidate(0).is_none(),
            "HTTP mode unexpectedly returned a dynamic video"
        );
    }

    let http = reqwest::Client::builder()
        .no_proxy()
        .proxy(reqwest::Proxy::all(&config.proxy)?)
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(5))
        .build()?;
    let fetcher = PageFetcher::new(http, browser, config)?;
    let douyin = fetcher
        .get_page(
            &Url::parse("http://www.douyin.com/share/video/1")?,
            Platform::Douyin,
            None,
        )
        .await?;
    ensure!(
        douyin
            .state
            .as_ref()
            .and_then(|state| state.pointer("/loaderData/page/videoInfoRes/item_list/0/desc"))
            .and_then(serde_json::Value::as_str)
            == Some("dynamic DY"),
        "Douyin page fetch lost executed router state"
    );

    let crossed = fetcher
        .get_page(
            &Url::parse("http://www.xiaohongshu.com/cross")?,
            Platform::Xiaohongshu,
            None,
        )
        .await;
    let error = crossed
        .err()
        .context("cross-platform browser navigation was accepted")?;
    ensure!(
        error.to_string().contains("最终页面不属于原平台"),
        "cross-platform page did not fail at final URL validation"
    );

    scope.shutdown().await;
    let count = observations
        .lock()
        .expect("fixture observations lock")
        .len();
    ensure!(
        fetcher
            .get_page(
                &Url::parse("http://www.xiaohongshu.com/closed")?,
                Platform::Xiaohongshu,
                None
            )
            .await
            .is_err(),
        "closed browser scope silently fell back to HTTP"
    );
    ensure!(
        observations
            .lock()
            .expect("fixture observations lock")
            .len()
            == count,
        "browser failure triggered an unexpected HTTP request"
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
    // A proxy must receive absolute-form URLs. There is deliberately no DNS,
    // forwarding, CONNECT tunnelling, or alternate upstream transport here.
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
            "Location: http://www.xiaohongshu.com/dynamic\r\n",
            "",
        ),
        ("xhslink.com", "/js-short") => ("200 OK", "", JS_SHORT_PAGE),
        ("www.xiaohongshu.com", "/dynamic" | "/closed") => ("200 OK", "", XHS_PAGE),
        ("www.kuaishou.com", "/dynamic") => ("200 OK", "", KS_PAGE),
        ("pd.qq.com", "/dynamic") => ("200 OK", "", QQ_PAGE),
        ("www.douyin.com", "/share/video/1") => ("200 OK", "", DY_PAGE),
        ("www.xiaohongshu.com", "/cross") => ("200 OK", "", CROSS_PLATFORM_PAGE),
        ("pd.qq.com", "/final") => (
            "200 OK",
            "",
            "<!doctype html><html><head><title>other platform</title></head><body>final</body></html>",
        ),
        ("qchannelvideo.photo.qq.com", "/fixture.mp4") => ("204 No Content", "", ""),
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
