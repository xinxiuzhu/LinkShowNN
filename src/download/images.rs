//! 有界、无登录凭据的图片下载。海报素材和原图投递共享配额与逐跳检查。

use std::{
    collections::{HashMap, HashSet},
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    sync::OnceLock,
    time::Duration,
};

use anyhow::{Result, anyhow, bail};
use futures_util::StreamExt;
use reqwest::{Client, Proxy, Response, header};
use tokio::{sync::Semaphore, time::Instant};
use url::{Host, Url};

use crate::resolver::Card;

const MAX_IMAGES: usize = 10;
const MAX_ASSET_BYTES: usize = 12 * 1024 * 1024;
const MAX_TOTAL_BYTES: usize = 48 * 1024 * 1024;
const PREPARE_TIMEOUT: Duration = Duration::from_secs(20);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_COMMENTS: usize = 3;
const MAX_COMMENT_EMOTES: usize = 16;
const MAX_COMMENT_AVATAR_BYTES: usize = 512 * 1024;
const MAX_COMMENT_EMOTE_BYTES: usize = 512 * 1024;
const MAX_COMMENT_PICTURE_BYTES: usize = 1024 * 1024;
const COMMENT_ASSETS_TIMEOUT: Duration = Duration::from_secs(4);

fn requests() -> &'static Semaphore {
    static REQUESTS: OnceLock<Semaphore> = OnceLock::new();
    REQUESTS.get_or_init(|| Semaphore::new(2))
}

#[derive(Clone)]
pub(crate) struct ImageFetcher {
    proxy: Option<Proxy>,
}

impl ImageFetcher {
    pub(crate) fn new(proxy: &str) -> Result<Self> {
        let proxy = if proxy.trim().is_empty() {
            None
        } else {
            Some(Proxy::all(proxy.trim()).map_err(|_| anyhow!("invalid card asset proxy"))?)
        };
        Ok(Self { proxy })
    }

    pub(crate) async fn prepare(&self, card: &Card) -> Result<HashMap<String, Vec<u8>>> {
        let deadline = Instant::now() + PREPARE_TIMEOUT;
        let mut assets = HashMap::new();
        let mut budget = ByteBudget::new(MAX_TOTAL_BYTES);
        let referer = card
            .metadata()
            .platform
            .map(|platform| platform.media_referer());
        // 正文不能悄悄缺图；失败交给装配层退回原始图文消息。
        for source in card.image_urls().take(MAX_IMAGES) {
            if assets.contains_key(source) {
                continue;
            }
            let bytes = tokio::time::timeout_at(
                deadline,
                self.download(source, referer, budget.remaining()),
            )
            .await
            .map_err(|_| anyhow!("card asset preparation timed out"))??;
            budget.consume(bytes.len())?;
            assets.insert(source.to_owned(), bytes);
        }
        // 头像超时也只是缺头像，不丢弃已经取得的正文图片。
        if let Some(source) = card.metadata().avatar_url.as_deref()
            && !assets.contains_key(source)
            && budget.remaining() > 0
            && let Ok(Ok(bytes)) = tokio::time::timeout_at(
                deadline,
                self.download(source, referer, budget.remaining()),
            )
            .await
        {
            budget.consume(bytes.len())?;
            assets.insert(source.to_owned(), bytes);
        }
        // 评论头像、表情和配图共用短时限，任何可选素材失败都保留主卡片。
        let comment_sources = comment_sources(card, &assets);
        if !comment_sources.is_empty() && budget.remaining() > 0 {
            let deadline = deadline.min(Instant::now() + COMMENT_ASSETS_TIMEOUT);
            // 先均分剩余预算，保证并发请求合计也不会超出整张卡片的上限。
            let share = budget.remaining() / comment_sources.len();
            if share > 0 {
                let fetched = futures_util::stream::iter(comment_sources)
                    .map(|(source, maximum): (String, usize)| async move {
                        let bytes = tokio::time::timeout_at(
                            deadline,
                            self.download(&source, referer, share.min(maximum)),
                        )
                        .await;
                        (source, bytes)
                    })
                    .buffer_unordered(2)
                    .collect::<Vec<_>>()
                    .await;
                for (source, result) in fetched {
                    if let Ok(Ok(bytes)) = result {
                        budget.consume(bytes.len())?;
                        assets.insert(source, bytes);
                    }
                }
            }
        }
        Ok(assets)
    }

    /// Original galleries have their own small message budget. Keep the
    /// successfully fetched first eight pictures, in source order downstream;
    /// a failed optional picture cannot cause a raw remote URL to be sent.
    pub(crate) async fn prepare_gallery(
        &self,
        card: &Card,
    ) -> Result<HashMap<String, nuono_plugin::MessageSegment>> {
        let deadline = Instant::now() + PREPARE_TIMEOUT;
        let mut assets = HashMap::new();
        let mut budget = ByteBudget::new(MAX_TOTAL_BYTES);
        let referer = card
            .metadata()
            .platform
            .map(|platform| platform.media_referer());
        for source in card.image_urls().take(crate::delivery::MAX_GALLERY_IMAGES) {
            if assets.contains_key(source) || budget.remaining() == 0 {
                continue;
            }
            if let Ok(Ok(bytes)) = tokio::time::timeout_at(
                deadline,
                self.download(
                    source,
                    referer,
                    budget.remaining().min(nuono_plugin::media::MAX_IMAGE_BYTES),
                ),
            )
            .await
            {
                let size = bytes.len();
                // Encode once; SDK validation excludes error HTML or unsupported images.
                if let Ok(segment) = nuono_plugin::ImageSource::bytes(bytes).into_segment().await {
                    budget.consume(size)?;
                    assets.insert(source.to_owned(), segment);
                }
            }
        }
        if assets.is_empty() {
            bail!("图集原图获取失败，请查看原链接");
        }
        Ok(assets)
    }

    async fn download(
        &self,
        source: &str,
        referer: Option<&str>,
        remaining: usize,
    ) -> Result<Vec<u8>> {
        let _request = requests()
            .acquire()
            .await
            .map_err(|_| anyhow!("card asset download pool is closed"))?;
        tokio::time::timeout(REQUEST_TIMEOUT, async {
            let mut current = Url::parse(source).map_err(|_| anyhow!("invalid card asset URL"))?;
            for hop in 0..=3 {
                let (host, addresses) = checked_addresses(&current).await?;
                // 不复用 DNS 或连接池：本跳只连接刚刚验证过的地址。
                let mut builder = Client::builder()
                    .no_proxy()
                    .redirect(reqwest::redirect::Policy::none())
                    .connect_timeout(Duration::from_secs(5))
                    .timeout(REQUEST_TIMEOUT)
                    .resolve_to_addrs(&host, &addresses);
                if let Some(proxy) = &self.proxy {
                    // 显式代理是用户选择的出口，目标仍执行上述 DNS 公网检查。
                    builder = builder.proxy(proxy.clone());
                }
                let client = builder
                    .build()
                    .map_err(|_| anyhow!("card asset HTTP client creation failed"))?;
                let mut request = client.get(current.clone()).header(
                    header::USER_AGENT,
                    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 Chrome/131 Safari/537.36",
                );
                if let Some(referer) = referer {
                    request = request.header(header::REFERER, referer);
                }
                let response = request
                    .send()
                    .await
                    .map_err(|_| anyhow!("card asset request failed"))?;
                if response.status().is_redirection() {
                    if hop == 3 {
                        bail!("card asset redirected too many times");
                    }
                    let location = response
                        .headers()
                        .get(header::LOCATION)
                        .and_then(|value| value.to_str().ok())
                        .ok_or_else(|| anyhow!("card asset redirect has no valid Location"))?;
                    current = current
                        .join(location)
                        .map_err(|_| anyhow!("invalid card asset redirect"))?;
                    continue;
                }
                if !response.status().is_success() {
                    bail!("card asset request returned HTTP {}", response.status().as_u16());
                }
                return read_bounded(response, remaining.min(MAX_ASSET_BYTES)).await;
            }
            unreachable!("the final redirect returns an error")
        })
        .await
        .map_err(|_| anyhow!("card asset request timed out"))?
    }
}

// Avatar requests go first, then inline emotes and comment pictures. A URL used
// in several roles is fetched once; the larger role's byte cap is still bounded.
fn comment_sources(card: &Card, assets: &HashMap<String, Vec<u8>>) -> Vec<(String, usize)> {
    let mut sources: Vec<(String, usize)> = Vec::new();
    let mut add = |url: &str, limit: usize| {
        if url.trim().is_empty() || assets.contains_key(url) {
            return;
        }
        if let Some((_, maximum)) = sources.iter_mut().find(|(source, _)| source == url) {
            *maximum = (*maximum).max(limit);
        } else {
            sources.push((url.to_owned(), limit));
        }
    };
    let comments = card.metadata().comments.iter().take(MAX_COMMENTS);
    for comment in comments.clone() {
        if let Some(url) = comment.avatar_url.as_deref() {
            add(url, MAX_COMMENT_AVATAR_BYTES);
        }
    }
    let mut emote_urls = HashSet::new();
    for comment in comments.clone() {
        for emote in comment.emotes.iter().take(8) {
            if !emote.text.is_empty()
                && comment.text.contains(&emote.text)
                && emote_urls.len() < MAX_COMMENT_EMOTES
                && emote_urls.insert(emote.url.as_str())
            {
                add(&emote.url, MAX_COMMENT_EMOTE_BYTES);
            }
        }
    }
    for comment in comments {
        for url in comment.pictures.iter().take(3) {
            add(url, MAX_COMMENT_PICTURE_BYTES);
        }
    }
    sources
}

struct ByteBudget {
    remaining: usize,
}

impl ByteBudget {
    fn new(remaining: usize) -> Self {
        Self { remaining }
    }

    fn remaining(&self) -> usize {
        self.remaining
    }

    fn consume(&mut self, bytes: usize) -> Result<()> {
        self.remaining = self
            .remaining
            .checked_sub(bytes)
            .ok_or_else(|| anyhow!("card assets exceed the byte limit"))?;
        Ok(())
    }
}

async fn read_bounded(response: Response, maximum: usize) -> Result<Vec<u8>> {
    if response
        .content_length()
        .is_some_and(|length| length > maximum as u64)
    {
        bail!("card asset exceeds the byte limit");
    }
    let mut budget = ByteBudget::new(maximum);
    let mut bytes = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| anyhow!("card asset response read failed"))?;
        budget.consume(chunk.len())?;
        bytes.extend_from_slice(&chunk);
    }
    if bytes.is_empty() {
        bail!("card asset response is empty");
    }
    Ok(bytes)
}

fn validate_url(url: &Url) -> Result<()> {
    if !matches!(url.scheme(), "http" | "https") {
        bail!("card assets require HTTP or HTTPS");
    }
    if !url.username().is_empty() || url.password().is_some() {
        bail!("card asset URL credentials are not allowed");
    }
    if url.host().is_none() || url.port_or_known_default().is_none() {
        bail!("card asset URL has no host or port");
    }
    Ok(())
}

async fn checked_addresses(url: &Url) -> Result<(String, Vec<SocketAddr>)> {
    validate_url(url)?;
    let port = url.port_or_known_default().expect("validated port");
    let (host, addresses) = match url.host().expect("validated host") {
        Host::Ipv4(ip) => (ip.to_string(), vec![SocketAddr::new(ip.into(), port)]),
        Host::Ipv6(ip) => (ip.to_string(), vec![SocketAddr::new(ip.into(), port)]),
        Host::Domain(host) => {
            let addresses = tokio::net::lookup_host((host, port))
                .await
                .map_err(|_| anyhow!("card asset hostname resolution failed"))?
                .collect();
            (host.to_owned(), addresses)
        }
    };
    validate_addresses(&addresses)?;
    Ok((host, addresses))
}

fn validate_addresses(addresses: &[SocketAddr]) -> Result<()> {
    if addresses.is_empty() || addresses.iter().any(|address| !is_public_ip(address.ip())) {
        bail!("card asset hostname must resolve only to public addresses");
    }
    Ok(())
}

fn is_public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => public_v4(ip),
        IpAddr::V6(ip) => {
            if let Some(mapped) = ip.to_ipv4_mapped() {
                return public_v4(mapped);
            }
            let value = u128::from(ip);
            // 只允许公网单播 2000::/3，拒绝特殊协议、文档及过渡地址段。
            value >> 125 == 1
                && !v6_prefix(ip, "2001::".parse().unwrap(), 23)
                && !v6_prefix(ip, "2001:db8::".parse().unwrap(), 32)
                && !v6_prefix(ip, "2002::".parse().unwrap(), 16)
                && !v6_prefix(ip, "3fff::".parse().unwrap(), 20)
        }
    }
}

fn public_v4(ip: Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    !matches!(a, 0 | 10 | 127 | 224..=255)
        && !(a == 100 && (64..=127).contains(&b))
        && !(a == 169 && b == 254)
        && !(a == 172 && (16..=31).contains(&b))
        && !(a == 192 && b == 168)
        && !(a == 192 && b == 0 && matches!(c, 0 | 2))
        && !(a == 192 && b == 88 && c == 99)
        && !(a == 198 && matches!(b, 18 | 19))
        && !(a == 198 && b == 51 && c == 100)
        && !(a == 203 && b == 0 && c == 113)
}

fn v6_prefix(ip: Ipv6Addr, network: Ipv6Addr, bits: u32) -> bool {
    u128::from(ip) >> (128 - bits) == u128::from(network) >> (128 - bits)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resolver::{CardComment, CardCommentEmote, CardMetadata, Platform};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    // 仅回复固定响应的本地 HTTP 代理，不进行任何转发或外网访问。
    async fn proxy_fixture<T: AsRef<[u8]> + Send + 'static>(
        responses: Vec<T>,
    ) -> (String, tokio::task::JoinHandle<Vec<String>>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            let mut requests = Vec::new();
            for response in responses {
                let (mut socket, _) =
                    tokio::time::timeout(Duration::from_secs(5), listener.accept())
                        .await
                        .expect("fixture did not receive the expected request")
                        .unwrap();
                let request = tokio::time::timeout(Duration::from_secs(5), async {
                    let mut request = Vec::new();
                    let mut chunk = [0u8; 1024];
                    while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                        let read = socket.read(&mut chunk).await.unwrap();
                        assert!(read > 0, "fixture request ended before its headers");
                        request.extend_from_slice(&chunk[..read]);
                        assert!(request.len() <= 16 * 1024, "fixture request is too large");
                    }
                    String::from_utf8(request).unwrap()
                })
                .await
                .expect("fixture request headers timed out");
                requests.push(request);
                socket.write_all(response.as_ref()).await.unwrap();
                socket.shutdown().await.unwrap();
            }
            requests
        });
        (proxy, task)
    }

    #[tokio::test]
    async fn prepares_public_assets_through_proxy_and_rejects_private_redirects() {
        let (proxy, fixture) = proxy_fixture(vec![
            "HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nimage",
            "HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            "HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1/private.png\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        ])
        .await;
        let fetcher = ImageFetcher::new(&proxy).unwrap();
        let source = "http://8.8.8.8/body.png?token=fixture";
        let metadata = CardMetadata {
            platform: Some(Platform::Douyin),
            avatar_url: Some("http://8.8.8.8/avatar.png".into()),
            ..Default::default()
        };
        let card = Card::new()
            .with_metadata(metadata.clone())
            .image(source)
            .image(source);
        let assets = fetcher.prepare(&card).await.unwrap();
        assert_eq!(assets.len(), 1, "failed avatar must not discard the body");
        assert_eq!(assets.get(source).unwrap(), b"image");

        let redirect = Card::new()
            .with_metadata(metadata)
            .image("http://8.8.8.8/redirect.png");
        let error = fetcher.prepare(&redirect).await.unwrap_err();
        assert_eq!(
            error.to_string(),
            "card asset hostname must resolve only to public addresses"
        );
        let requests = fixture.await.unwrap();
        assert_eq!(
            requests.len(),
            3,
            "duplicate body URLs should be fetched once"
        );
        assert!(requests[0].starts_with(&format!("GET {source} HTTP/1.1\r\n")));
        assert!(requests[1].starts_with("GET http://8.8.8.8/avatar.png HTTP/1.1\r\n"));
        assert!(requests[2].starts_with("GET http://8.8.8.8/redirect.png HTTP/1.1\r\n"));
        for request in requests {
            let headers = request.to_ascii_lowercase();
            assert!(headers.contains("\r\nreferer: https://www.douyin.com/\r\n"));
            assert!(!headers.contains("\r\ncookie:"));
            assert!(!headers.contains("\r\nauthorization:"));
        }
    }

    #[tokio::test]
    async fn gallery_fetches_only_eight_originals_and_excludes_decorative_media() {
        use ::image::ImageEncoder;
        let mut png = Vec::new();
        ::image::codecs::png::PngEncoder::new(&mut png)
            .write_image(&[255, 0, 0], 1, 1, ::image::ColorType::Rgb8.into())
            .unwrap();
        let response = |body: &[u8]| {
            let mut encoded = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
            .into_bytes();
            encoded.extend_from_slice(body);
            encoded
        };
        let mut responses = vec![response(b"<html>unavailable</html>")];
        responses.extend((1..8).map(|_| response(&png)));
        let (proxy, fixture) = proxy_fixture(responses).await;
        let card = Card::new()
            .images((0..12).map(|index| format!("http://8.8.8.8/{index}.png")))
            .with_metadata(CardMetadata {
                platform: Some(Platform::Douyin),
                avatar_url: Some("http://8.8.8.8/avatar.png".into()),
                comments: vec![CardComment {
                    avatar_url: Some("http://8.8.8.8/comment-avatar.png".into()),
                    pictures: vec!["http://8.8.8.8/comment-picture.png".into()],
                    ..Default::default()
                }],
                ..Default::default()
            });
        let fetched = ImageFetcher::new(&proxy)
            .unwrap()
            .prepare_gallery(&card)
            .await
            .unwrap();
        assert_eq!(fetched.len(), 7);
        assert!(!fetched.contains_key("http://8.8.8.8/0.png"));
        assert!(!fetched.contains_key("http://8.8.8.8/8.png"));
        let requests = fixture.await.unwrap();
        assert_eq!(requests.len(), 8);
        for (index, request) in requests.iter().enumerate() {
            assert!(request.starts_with(&format!("GET http://8.8.8.8/{index}.png ")));
            assert!(!request.to_ascii_lowercase().contains("cookie:"));
            assert!(
                request
                    .to_ascii_lowercase()
                    .contains("referer: https://www.douyin.com/")
            );
        }
    }

    #[tokio::test]
    async fn comment_avatars_are_bounded_deduplicated_and_optional() {
        const OK: &str = "HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nimage";
        const FAILED: &str =
            "HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
        let (proxy, fixture) = proxy_fixture(vec![OK, OK, OK, FAILED]).await;
        let fetcher = ImageFetcher::new(&proxy).unwrap();
        let body = "http://8.8.8.8/body.png";
        let avatar = "http://8.8.8.8/comment.png";
        let comment = |source: &str| CardComment {
            avatar_url: Some(source.into()),
            ..Default::default()
        };
        let card = Card::new().image(body).with_metadata(CardMetadata {
            platform: Some(Platform::Bilibili),
            comments: vec![
                comment(avatar),
                comment(avatar),
                comment(body),
                comment("http://8.8.8.8/fourth-must-not-be-fetched.png"),
            ],
            ..Default::default()
        });
        let assets = fetcher.prepare(&card).await.unwrap();
        assert_eq!(assets.len(), 2);
        assert_eq!(assets.get(avatar).unwrap(), b"image");

        let fallback = Card::new().image(body).with_metadata(CardMetadata {
            platform: Some(Platform::Bilibili),
            comments: vec![comment("http://8.8.8.8/missing.png")],
            ..Default::default()
        });
        let assets = fetcher.prepare(&fallback).await.unwrap();
        assert_eq!(
            assets.len(),
            1,
            "missing comment avatar must retain the cover"
        );
        assert!(assets.contains_key(body));
        let requests = fixture.await.unwrap();
        assert_eq!(requests.len(), 4);
        for request in requests {
            let headers = request.to_ascii_lowercase();
            assert!(headers.contains("\r\nreferer: https://www.bilibili.com/\r\n"));
            assert!(!headers.contains("\r\ncookie:"));
            assert!(!headers.contains("\r\nauthorization:"));
            assert!(!headers.contains("fourth-must-not-be-fetched"));
        }
    }

    #[test]
    fn comment_media_sources_have_count_and_byte_caps() {
        let comments = (0..4)
            .map(|index| CardComment {
                text: (0..10).map(|i| format!("[e{i}]")).collect(),
                emotes: (0..10)
                    .map(|i| CardCommentEmote {
                        text: format!("[e{i}]"),
                        url: format!("http://8.8.8.8/emote-{index}-{i}.png"),
                        large: false,
                    })
                    .collect(),
                pictures: (0..4)
                    .map(|i| format!("http://8.8.8.8/picture-{index}-{i}.png"))
                    .collect(),
                ..Default::default()
            })
            .collect();
        let card = Card::new().with_metadata(CardMetadata {
            comments,
            ..Default::default()
        });
        let sources = comment_sources(&card, &HashMap::new());
        assert_eq!(
            sources
                .iter()
                .filter(|(url, _)| url.contains("emote-"))
                .count(),
            16
        );
        assert_eq!(
            sources
                .iter()
                .filter(|(url, _)| url.contains("picture-"))
                .count(),
            9
        );
        assert!(sources.iter().all(|(url, limit)| *limit
            == if url.contains("emote-") {
                MAX_COMMENT_EMOTE_BYTES
            } else {
                MAX_COMMENT_PICTURE_BYTES
            }));
        assert!(!sources.iter().any(|(url, _)| url.contains("picture-3-")));
    }

    #[tokio::test]
    async fn comment_emotes_and_pictures_share_anonymous_deduplicated_downloads() {
        const OK: &str = "HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nimage";
        let (proxy, fixture) = proxy_fixture(vec![OK, OK]).await;
        let url = "http://8.8.8.8/shared.png";
        let card = Card::new()
            .image("http://8.8.8.8/cover.png")
            .with_metadata(CardMetadata {
                platform: Some(Platform::Bilibili),
                comments: vec![CardComment {
                    avatar_url: Some(url.into()),
                    text: "正文[笑哭]".into(),
                    emotes: vec![CardCommentEmote {
                        text: "[笑哭]".into(),
                        url: url.into(),
                        large: false,
                    }],
                    pictures: vec![url.into(), url.into()],
                    ..Default::default()
                }],
                ..Default::default()
            });
        let sources = comment_sources(&card, &HashMap::new());
        assert_eq!(sources, [(url.to_owned(), MAX_COMMENT_PICTURE_BYTES)]);
        let assets = ImageFetcher::new(&proxy)
            .unwrap()
            .prepare(&card)
            .await
            .unwrap();
        assert_eq!(assets.len(), 2);
        assert_eq!(assets.get(url).unwrap(), b"image");
        let requests = fixture.await.unwrap();
        assert_eq!(requests.len(), 2);
        for request in requests {
            let headers = request.to_ascii_lowercase();
            assert!(headers.contains("\r\nreferer: https://www.bilibili.com/\r\n"));
            assert!(!headers.contains("\r\ncookie:"));
            assert!(!headers.contains("\r\nauthorization:"));
        }
    }

    #[tokio::test]
    async fn streamed_assets_enforce_the_budget_without_content_length() {
        const CHUNKED: &str = "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n4\r\nabcd\r\n5\r\nefghi\r\n0\r\n\r\n";
        let (proxy, fixture) = proxy_fixture(vec![
            CHUNKED,
            CHUNKED,
            "HTTP/1.1 200 OK\r\nContent-Length: 9\r\nConnection: close\r\n\r\nabcdefghi",
        ])
        .await;
        let client = Client::builder()
            .no_proxy()
            .proxy(Proxy::all(proxy).unwrap())
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap();
        let response = client
            .get("http://8.8.8.8/streamed.png")
            .send()
            .await
            .unwrap();
        assert!(response.content_length().is_none());
        assert!(read_bounded(response, 8).await.is_err());

        let response = client.get("http://8.8.8.8/exact.png").send().await.unwrap();
        assert_eq!(read_bounded(response, 9).await.unwrap(), b"abcdefghi");

        let response = client
            .get("http://8.8.8.8/length.png")
            .send()
            .await
            .unwrap();
        assert_eq!(response.content_length(), Some(9));
        assert!(read_bounded(response, 8).await.is_err());
        assert_eq!(fixture.await.unwrap().len(), 3);
    }

    #[test]
    fn asset_urls_reject_non_http_and_embedded_credentials() {
        for value in [
            "file:///tmp/photo.png",
            "https://user:secret@example.com/a",
            "ftp://example.com/a",
        ] {
            assert!(validate_url(&Url::parse(value).unwrap()).is_err());
        }
        assert!(
            validate_url(&Url::parse("https://example.com:8443/a?token=secret").unwrap()).is_ok()
        );
    }

    #[test]
    fn special_and_mapped_private_addresses_are_rejected() {
        for value in [
            "0.1.2.3",
            "10.0.0.1",
            "100.64.0.1",
            "127.0.0.1",
            "169.254.169.254",
            "172.31.0.1",
            "192.0.0.1",
            "192.0.2.1",
            "192.168.0.1",
            "192.88.99.1",
            "198.18.0.1",
            "198.51.100.1",
            "203.0.113.1",
            "224.0.0.1",
            "240.0.0.1",
            "255.255.255.255",
            "::",
            "::1",
            "::ffff:127.0.0.1",
            "::ffff:192.168.1.1",
            "64:ff9b::a00:1",
            "fc00::1",
            "fe80::1",
            "ff02::1",
            "2001::1",
            "2001:db8::1",
            "2002:7f00:1::",
            "3fff::1",
        ] {
            assert!(!is_public_ip(value.parse().unwrap()), "{value}");
        }
        for value in [
            "8.8.8.8",
            "1.1.1.1",
            "::ffff:8.8.8.8",
            "2001:4860:4860::8888",
            "2606:4700:4700::1111",
        ] {
            assert!(is_public_ip(value.parse().unwrap()), "{value}");
        }
    }

    #[test]
    fn dns_must_return_only_public_addresses() {
        let public = "8.8.8.8:443".parse().unwrap();
        let private = "127.0.0.1:443".parse().unwrap();
        assert!(validate_addresses(&[]).is_err());
        assert!(validate_addresses(&[public, private]).is_err());
        assert!(validate_addresses(&[public]).is_ok());
    }

    #[test]
    fn byte_budget_accepts_exact_limit_and_rejects_overflow() {
        let mut budget = ByteBudget::new(12);
        budget.consume(5).unwrap();
        assert!(budget.consume(8).is_err());
        assert_eq!(budget.remaining(), 7);
        budget.consume(7).unwrap();
        assert!(budget.consume(1).is_err());
        assert!(budget.consume(usize::MAX).is_err());
    }
}
