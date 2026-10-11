//! 分享页获取与浏览器状态提取。
//!
//! HTTP 重定向逐跳检查平台域名；Obscura 检查起始和最终页面域名，中间导航
//! 和脚本子请求由 SDK 的公网访问边界约束。浏览器失败不会静默回退 HTTP。

use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail, ensure};
use nuono_plugin::browser::{BrowserRequest, BrowserWaitUntil, PluginBrowser};
use reqwest::{Client, header};
use serde_json::Value;
use tracing::info;
use url::Url;

use crate::{
    default_config::ResolverConfig,
    resolver::Platform,
    resolver::url_policy::{resolve_redirect, validate_platform_url},
};

/// 分享页普遍只对移动端 UA 下发内联数据。
pub const MOBILE_USER_AGENT: &str = "Mozilla/5.0 (iPhone; CPU iPhone OS 16_6 like Mac OS X) AppleWebKit/605.1.15 Mobile/15E148 Safari/604.1";
const MAX_REDIRECTS: usize = 5;

/// DOM 与执行后的平台状态分别传递，避免把动态数据重新拼成内联脚本。
pub struct FetchedPage {
    pub url: Url,
    pub html: String,
    pub state: Option<Value>,
}

#[derive(Clone)]
pub struct PageFetcher {
    http: Client,
    browser: PluginBrowser,
    obscura_enabled: bool,
    timeout: Duration,
    settle: Duration,
    proxy: Option<String>,
}

impl PageFetcher {
    pub fn new(http: Client, browser: PluginBrowser, config: &ResolverConfig) -> Result<Self> {
        let timeout = Duration::from_secs(config.obscura_timeout_seconds);
        let settle = Duration::from_millis(config.obscura_settle_ms);
        ensure!(
            (1..=120).contains(&config.obscura_timeout_seconds),
            "Obscura 超时必须为 1–120 秒"
        );
        ensure!(
            config.obscura_settle_ms <= 10000 && settle < timeout,
            "Obscura 页面等待必须不超过 10000 毫秒且小于超时"
        );
        Ok(Self {
            http,
            browser,
            obscura_enabled: config.obscura_enabled,
            timeout,
            settle,
            proxy: (!config.proxy.trim().is_empty()).then(|| config.proxy.trim().to_owned()),
        })
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
        validate_platform_url(source, platform)?;
        if !self.obscura_enabled {
            return self.get_http_page(source, platform, cookie).await;
        }
        // 不把登录 Cookie 种到短链域名；先用无 Cookie 的 HTTP 请求展开。
        let start = if is_short_host(source, platform) {
            self.expand_short_url(source, platform, |url| !is_short_host(url, platform))
                .await?
        } else {
            source.clone()
        };
        let started_on_short_link = is_short_host(&start, platform);
        let request = self.browser_request(
            &start,
            platform,
            if started_on_short_link { None } else { cookie },
        )?;
        let started = Instant::now();
        let mut page = self
            .browser
            .fetch(request)
            .await
            .map_err(|error| anyhow::anyhow!("Obscura 分享页解析失败：{error}"))?;
        let mut url = Url::parse(&page.url).context("Obscura 返回的页面 URL 无效")?;
        validate_platform_url(&url, platform).context("Obscura 最终页面不属于原平台")?;
        // 部分短链用 JavaScript 跳转。首次访问不带登录 Cookie；到达内容页后
        // 才在新隔离会话中注入 Cookie。两次浏览器访问共用一次超时预算。
        if started_on_short_link && cookie.is_some_and(|value| !value.trim().is_empty()) {
            ensure!(!is_short_host(&url, platform), "短链执行后未进入平台内容页");
            let mut request = self.browser_request(&url, platform, cookie)?;
            request.timeout = self
                .timeout
                .checked_sub(started.elapsed())
                .filter(|remaining| *remaining > self.settle)
                .context("Obscura 分享页解析超时")?;
            page = self
                .browser
                .fetch(request)
                .await
                .map_err(|error| anyhow::anyhow!("Obscura 登录分享页解析失败：{error}"))?;
            url = Url::parse(&page.url).context("Obscura 返回的页面 URL 无效")?;
            validate_platform_url(&url, platform).context("Obscura 最终页面不属于原平台")?;
        }
        let state = page.evaluation.filter(|value| !value.is_null());
        info!(
            platform = platform.as_str(),
            engine = "obscura",
            elapsed_ms = started.elapsed().as_millis() as u64,
            has_state = state.is_some(),
            "URL 分享页获取完成"
        );
        Ok(FetchedPage {
            url,
            html: page.content,
            state,
        })
    }

    fn browser_request(
        &self,
        source: &Url,
        platform: Platform,
        cookie: Option<&str>,
    ) -> Result<BrowserRequest> {
        let mut request = BrowserRequest::new(source.as_str());
        request.wait_until = BrowserWaitUntil::DomContentLoaded;
        request.timeout = self.timeout;
        request.settle = self.settle;
        request.user_agent = Some(MOBILE_USER_AGENT.to_owned());
        request.proxy = self.proxy.clone();
        request.cookies = cookie_seeds(cookie, source, platform)?;
        request.max_output_bytes = 8 * 1024 * 1024;
        request.evaluation = match platform {
            Platform::Xiaohongshu => Some("window.__INITIAL_STATE__ ?? null"),
            Platform::Kuaishou => Some("window.INIT_STATE ?? null"),
            Platform::Douyin => Some("window._ROUTER_DATA ?? null"),
            _ => None,
        }
        .map(str::to_owned);
        Ok(request)
    }

    pub(crate) async fn get_http_page(
        &self,
        source: &Url,
        platform: Platform,
        cookie: Option<&str>,
    ) -> Result<FetchedPage> {
        let mut current = source.clone();
        for redirects in 0..=MAX_REDIRECTS {
            let mut request = self
                .http
                .get(current.clone())
                .header(header::USER_AGENT, MOBILE_USER_AGENT);
            if cookie_domain(&current, platform).is_some()
                && let Some(cookie) = cookie.map(str::trim).filter(|value| !value.is_empty())
            {
                request = request.header(header::COOKIE, cookie);
            }
            let response = request.send().await.map_err(reqwest::Error::without_url)?;
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
            return Ok(FetchedPage {
                url: current,
                html: response.text().await?,
                state: None,
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

fn cookie_seeds(cookie: Option<&str>, source: &Url, platform: Platform) -> Result<Vec<String>> {
    let Some(raw) = cookie.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(Vec::new());
    };
    ensure!(
        !raw.contains(['\r', '\n', '\0']),
        "平台 Cookie 不能包含换行或空字符"
    );
    let domain =
        cookie_domain(source, platform).context("登录 Cookie 只能用于对应平台的内容域名")?;
    let mut seeds = Vec::new();
    for pair in raw
        .split(';')
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        let (name, value) = pair
            .split_once('=')
            .context("平台 Cookie 必须为 name=value 格式")?;
        ensure!(
            !name.is_empty()
                && name
                    .bytes()
                    .all(|ch| ch.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&ch)),
            "平台 Cookie 名称无效"
        );
        // 输入是 Cookie header，不接受 Set-Cookie 属性作为额外的 cookie。
        ensure!(
            !matches!(
                name.to_ascii_lowercase().as_str(),
                "domain" | "path" | "expires" | "max-age" | "samesite"
            ),
            "请填写 Cookie 请求头，不要填写 Set-Cookie 响应头"
        );
        let lower_name = name.to_ascii_lowercase();
        let host_only = lower_name.starts_with("__host-");
        let requires_secure = host_only || lower_name.starts_with("__secure-");
        ensure!(
            !requires_secure || source.scheme() == "https",
            "带 __Host-/__Secure- 前缀的 Cookie 需要 HTTPS 页面"
        );
        let domain_attr = if host_only {
            String::new()
        } else {
            format!("; Domain={domain}")
        };
        let secure_attr = if source.scheme() == "https" {
            "; Secure"
        } else {
            ""
        };
        seeds.push(format!("{name}={value}{domain_attr}; Path=/{secure_attr}"));
    }
    Ok(seeds)
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
    use nuono_plugin::PluginTaskScope;
    use std::sync::Arc;

    #[test]
    fn cookies_preserve_equals_and_stay_in_platform_content_domain() {
        let source = Url::parse("https://www.xiaohongshu.com/explore/123").unwrap();
        assert_eq!(
            cookie_seeds(Some(" a=1; token=abc==; "), &source, Platform::Xiaohongshu).unwrap(),
            vec![
                "a=1; Domain=xiaohongshu.com; Path=/; Secure",
                "token=abc==; Domain=xiaohongshu.com; Path=/; Secure"
            ]
        );
        for raw in [
            "a=1\r\nx=2",
            "bad name=1",
            "a=1; Domain=evil.test",
            "a=1; Path=/",
        ] {
            assert!(cookie_seeds(Some(raw), &source, Platform::Xiaohongshu).is_err());
        }
        assert!(
            cookie_seeds(
                Some("a=1"),
                &Url::parse("https://xhslink.com/abc").unwrap(),
                Platform::Xiaohongshu
            )
            .is_err()
        );
        assert!(
            cookie_seeds(
                Some("a=1"),
                &Url::parse("https://xiaohongshu.com.evil.test/").unwrap(),
                Platform::Xiaohongshu
            )
            .is_err()
        );
    }

    #[test]
    fn preserves_secure_and_host_only_cookie_prefixes() {
        let source = Url::parse("https://www.xiaohongshu.com/explore/123").unwrap();
        let raw = "__Host-session=abc; __secure-token=xyz";
        assert_eq!(
            cookie_seeds(Some(raw), &source, Platform::Xiaohongshu).unwrap(),
            vec![
                "__Host-session=abc; Path=/; Secure",
                "__secure-token=xyz; Domain=xiaohongshu.com; Path=/; Secure",
            ]
        );
        assert!(
            cookie_seeds(
                Some(raw),
                &Url::parse("http://www.xiaohongshu.com/a").unwrap(),
                Platform::Xiaohongshu
            )
            .is_err()
        );
    }

    #[test]
    fn builds_bounded_browser_request_from_config() {
        let scope = Arc::new(PluginTaskScope::new("linkshownn-request-test"));
        let config = ResolverConfig {
            proxy: "http://127.0.0.1:7890".into(),
            obscura_settle_ms: 500,
            ..ResolverConfig::default()
        };
        let fetcher =
            PageFetcher::new(Client::new(), PluginBrowser::for_scope(scope), &config).unwrap();
        let request = fetcher
            .browser_request(
                &Url::parse("https://www.xiaohongshu.com/a").unwrap(),
                Platform::Xiaohongshu,
                Some("a=1; b=2"),
            )
            .unwrap();
        assert_eq!(
            request.evaluation.as_deref(),
            Some("window.__INITIAL_STATE__ ?? null")
        );
        assert_eq!(request.settle, Duration::from_millis(500));
        assert_eq!(request.timeout, Duration::from_secs(30));
        assert_eq!(request.proxy, Some(config.proxy));
        assert_eq!(request.cookies.len(), 2);
        assert!(!request.allow_private_network);
    }
}
