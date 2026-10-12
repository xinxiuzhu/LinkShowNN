//! HTTP 分享页获取与短链展开。
//!
//! 重定向逐跳检查平台域名，登录 Cookie 只发往对应平台的内容域名；
//! 响应正文有大小限制，站点适配器从静态 HTML 中提取内联状态。

use anyhow::{Context, Result, bail, ensure};
use reqwest::{Client, header};
use url::Url;

use crate::{
    resolver::Platform,
    resolver::url_policy::{resolve_redirect, validate_platform_url},
};

/// 分享页普遍只对移动端 UA 下发内联数据。
pub const MOBILE_USER_AGENT: &str = "Mozilla/5.0 (iPhone; CPU iPhone OS 16_6 like Mac OS X) AppleWebKit/605.1.15 Mobile/15E148 Safari/604.1";
const MAX_REDIRECTS: usize = 5;

/// HTTP 获取的最终 URL 与静态 HTML。
pub struct FetchedPage {
    pub url: Url,
    pub html: String,
}

#[derive(Clone)]
pub struct PageFetcher {
    http: Client,
}

impl PageFetcher {
    pub fn new(http: Client) -> Self {
        Self { http }
    }

    pub fn client(&self) -> &Client {
        &self.http
    }

    pub async fn get_page(
        &self,
        source: &Url,
        platform: Platform,
        cookie: Option<&str>,
    ) -> Result<FetchedPage> {
        self.get_http_page(source, platform, cookie).await
    }

    pub(crate) async fn get_http_page(
        &self,
        source: &Url,
        platform: Platform,
        cookie: Option<&str>,
    ) -> Result<FetchedPage> {
        self.get_http_page_with_user_agent(source, platform, cookie, MOBILE_USER_AGENT)
            .await
    }

    pub(crate) async fn get_http_page_with_user_agent(
        &self,
        source: &Url,
        platform: Platform,
        cookie: Option<&str>,
        user_agent: &str,
    ) -> Result<FetchedPage> {
        const MAX_PAGE_BYTES: usize = 8 * 1024 * 1024;
        validate_platform_url(source, platform)?;
        let mut current = source.clone();
        for redirects in 0..=MAX_REDIRECTS {
            let mut request = self
                .http
                .get(current.clone())
                .header(header::USER_AGENT, user_agent);
            if platform == Platform::Xiaohongshu {
                request = request
                    .header(header::REFERER, "https://www.xiaohongshu.com/")
                    .header(
                        header::ACCEPT,
                        "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
                    );
            }
            if cookie_domain(&current, platform).is_some()
                && let Some(cookie) = cookie.map(str::trim).filter(|value| !value.is_empty())
            {
                request = request.header(header::COOKIE, cookie);
            }
            let mut response = request.send().await.map_err(reqwest::Error::without_url)?;
            if response.status().is_redirection() {
                ensure!(
                    redirects < MAX_REDIRECTS,
                    "platform URL redirected too many times"
                );
                current = resolve_redirect(&current, redirect_location(&response)?, platform)?;
                continue;
            }
            if !response.status().is_success() {
                bail!("platform page returned HTTP {}", response.status());
            }
            ensure!(
                response
                    .content_length()
                    .is_none_or(|length| length <= MAX_PAGE_BYTES as u64),
                "platform page exceeds size limit"
            );
            let mut body = Vec::new();
            while let Some(chunk) = response
                .chunk()
                .await
                .map_err(reqwest::Error::without_url)?
            {
                ensure!(
                    chunk.len() <= MAX_PAGE_BYTES.saturating_sub(body.len()),
                    "platform page exceeds size limit"
                );
                body.extend_from_slice(&chunk);
            }
            return Ok(FetchedPage {
                url: current,
                html: String::from_utf8_lossy(&body).into_owned(),
            });
        }
        bail!("platform URL redirected too many times")
    }

    /// 只展开短链，不读取正文；每一跳校验 host，`stop` 命中即提前返回。
    pub async fn expand_short_url(
        &self,
        url: &Url,
        platform: Platform,
        stop: impl Fn(&Url) -> bool,
    ) -> Result<Url> {
        validate_platform_url(url, platform)?;
        let mut current = url.clone();
        for redirects in 0..=MAX_REDIRECTS {
            let response = self
                .http
                .get(current.clone())
                .header(header::USER_AGENT, MOBILE_USER_AGENT)
                .send()
                .await
                .map_err(reqwest::Error::without_url)?;
            if !response.status().is_redirection() {
                return Ok(current);
            }
            ensure!(
                redirects < MAX_REDIRECTS,
                "platform URL redirected too many times"
            );
            current = resolve_redirect(&current, redirect_location(&response)?, platform)?;
            if stop(&current) {
                return Ok(current);
            }
        }
        bail!("platform URL redirected too many times")
    }
}

fn host_matches(url: &Url, domain: &str) -> bool {
    url.host_str()
        .is_some_and(|host| host == domain || host.ends_with(&format!(".{domain}")))
}

fn is_short_host(url: &Url, platform: Platform) -> bool {
    match platform {
        Platform::Xiaohongshu => {
            host_matches(url, "xhslink.com") || host_matches(url, "xhslink.cn")
        }
        Platform::Douyin => url.host_str() == Some("v.douyin.com"),
        Platform::Kuaishou => url.host_str() == Some("v.kuaishou.com"),
        _ => false,
    }
}

/// 配置里的登录 Cookie 以对应内容主域及其子域为作用域；初始短链不注入。
fn cookie_domain(url: &Url, platform: Platform) -> Option<&'static str> {
    if is_short_host(url, platform) {
        return None;
    }
    let domains: &[&str] = match platform {
        Platform::Xiaohongshu => &["xiaohongshu.com"],
        Platform::Douyin => &["douyin.com", "iesdouyin.com"],
        _ => &[],
    };
    domains
        .iter()
        .copied()
        .find(|domain| host_matches(url, domain))
}

fn redirect_location(response: &reqwest::Response) -> Result<&str> {
    response
        .headers()
        .get(header::LOCATION)
        .and_then(|value| value.to_str().ok())
        .context("redirect has no Location")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[test]
    fn cookies_only_belong_to_platform_content_domains() {
        for host in [
            "xiaohongshu.com",
            "www.xiaohongshu.com",
            "m.xiaohongshu.com",
        ] {
            let url = Url::parse(&format!("https://{host}/explore/123")).unwrap();
            assert_eq!(
                cookie_domain(&url, Platform::Xiaohongshu),
                Some("xiaohongshu.com")
            );
        }
        for host in ["xhslink.com", "xhslink.cn", "xiaohongshu.com.evil.test"] {
            let url = Url::parse(&format!("https://{host}/abc")).unwrap();
            assert_eq!(cookie_domain(&url, Platform::Xiaohongshu), None);
        }
        for host in ["www.douyin.com", "www.iesdouyin.com"] {
            let url = Url::parse(&format!("https://{host}/video/123")).unwrap();
            assert!(cookie_domain(&url, Platform::Douyin).is_some());
        }
        for host in ["v.douyin.com", "douyin.com.evil.test"] {
            let url = Url::parse(&format!("https://{host}/abc")).unwrap();
            assert_eq!(cookie_domain(&url, Platform::Douyin), None);
        }
    }

    #[tokio::test]
    async fn http_short_link_does_not_receive_content_cookie() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy = format!("http://{}", listener.local_addr().unwrap());
        let fixture = tokio::spawn(async move {
            let mut requests = Vec::new();
            for response in [
                "HTTP/1.1 302 Found\r\nLocation: http://www.xiaohongshu.com/explore/123\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                "HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\nwork",
            ] {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                    let mut chunk = [0u8; 1024];
                    let length = socket.read(&mut chunk).await.unwrap();
                    assert!(length > 0);
                    request.extend_from_slice(&chunk[..length]);
                }
                requests.push(String::from_utf8(request).unwrap());
                socket.write_all(response.as_bytes()).await.unwrap();
                socket.shutdown().await.unwrap();
            }
            requests
        });
        let client = Client::builder()
            .no_proxy()
            .proxy(reqwest::Proxy::all(proxy).unwrap())
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(3))
            .build()
            .unwrap();
        let page = PageFetcher::new(client)
            .get_page(
                &Url::parse("http://xhslink.cn/o/123").unwrap(),
                Platform::Xiaohongshu,
                Some("token=abc=="),
            )
            .await
            .unwrap();
        let requests = fixture.await.unwrap();
        assert_eq!(page.url.as_str(), "http://www.xiaohongshu.com/explore/123");
        assert_eq!(page.html, "work");
        assert!(!requests[0].to_ascii_lowercase().contains("\r\ncookie:"));
        assert!(
            requests[1]
                .to_ascii_lowercase()
                .contains("\r\ncookie: token=abc==\r\n")
        );
    }
}
