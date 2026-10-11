//! URL 安全边界。
//!
//! 这里只做安全判断，不含任何业务字段映射。三条规则各自把守不同场景：
//!
//! - [`validate_platform_url`]：页面 URL 必须属于该平台白名单 host，且端口
//!   限定 80/443。短链每一跳都重新走这条。
//! - [`validate_public_url`]：已解析出的媒体直链，放开端口（CDN 常用非标端口）
//!   但仍拒绝私有地址。DNS 解析后的地址复查在 [`crate::download`]。
//! - [`match_url`]：从聊天文本里挑出可信的平台链接。
//!
//! 端口规则的不对称是有意的：页面 URL 是用户可控输入，媒体直链来自平台
//! 自己的响应。

use anyhow::{Context, Result, bail};
use regex::Regex;
use std::{net::IpAddr, sync::OnceLock};
use url::Url;

use crate::resolver::Platform;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MatchedUrl {
    pub platform: Platform,
    pub url: Url,
}

/// 只认精确匹配或真正的子域，避免 `evil-bilibili.com` 这类前缀混淆。
fn host_matches(host: &str, allowed: &str) -> bool {
    host == allowed || host.ends_with(&format!(".{allowed}"))
}

fn host_belongs_to(host: &str, platform: Platform) -> bool {
    platform
        .page_hosts()
        .iter()
        .any(|allowed| host_matches(host, allowed))
}
fn validate_common(url: &Url) -> Result<String> {
    let host = validate_http_url(url)?;
    if !matches!(url.port(), None | Some(80) | Some(443)) {
        bail!("non-standard URL port is not allowed");
    }
    Ok(host)
}

fn validate_http_url(url: &Url) -> Result<String> {
    if !matches!(url.scheme(), "http" | "https") {
        bail!("only HTTP/HTTPS URLs are allowed");
    }
    if url.username() != "" || url.password().is_some() {
        bail!("URL credentials are not allowed");
    }
    Ok(url
        .host_str()
        .context("URL has no hostname")?
        .trim_end_matches('.')
        .to_ascii_lowercase())
}
pub fn validate_platform_url(url: &Url, platform: Platform) -> Result<()> {
    let host = validate_common(url)?;
    if !host_belongs_to(&host, platform) {
        bail!("hostname does not belong to {}", platform.as_str());
    }
    Ok(())
}
pub fn validate_public_url(url: &Url) -> Result<()> {
    let host = validate_http_url(url)?;
    if let Ok(ip) = host.trim_matches(['[', ']']).parse::<IpAddr>() {
        let blocked = match ip {
            IpAddr::V4(ip) => {
                ip.is_unspecified()
                    || ip.is_loopback()
                    || ip.is_private()
                    || ip.is_link_local()
                    || ip.is_multicast()
            }
            IpAddr::V6(ip) => {
                ip.is_unspecified()
                    || ip.is_loopback()
                    || ip.is_multicast()
                    || ip.is_unique_local()
                    || ip.is_unicast_link_local()
            }
        };
        if blocked {
            bail!("private or non-global media address is not allowed");
        }
    }
    Ok(())
}
pub fn resolve_redirect(source: &Url, location: &str, platform: Platform) -> Result<Url> {
    let target = source.join(location)?;
    validate_platform_url(&target, platform)?;
    Ok(target)
}
pub fn redact(url: &Url) -> String {
    format!(
        "{}://{}{}{}",
        url.scheme(),
        url.host_str().unwrap_or("invalid-host"),
        url.path(),
        if url.query().is_some() { "?…" } else { "" }
    )
}
pub fn match_url(text: &str) -> Option<MatchedUrl> {
    static URL_RE: OnceLock<Regex> = OnceLock::new();
    let regex = URL_RE.get_or_init(|| Regex::new(r#"(?i)https?://[^\s<>\"']+"#).unwrap());
    for found in regex.find_iter(text) {
        let candidate = found
            .as_str()
            .trim_end_matches(|c: char| ",.;:!?)]}>，。；：！？）】》」』".contains(c));
        let Ok(url) = Url::parse(candidate) else {
            continue;
        };
        let Ok(host) = validate_common(&url) else {
            continue;
        };
        if let Some(platform) = Platform::all()
            .iter()
            .copied()
            .find(|platform| host_belongs_to(&host, *platform))
        {
            return Some(MatchedUrl { platform, url });
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_platform_declares_hosts_that_match_itself() {
        // host 表漏填会让该平台的链接永远匹配不上，编译器查不出来。
        for platform in Platform::all() {
            let hosts = platform.page_hosts();
            assert!(!hosts.is_empty(), "{} 没有 host 白名单", platform.as_str());
            for host in hosts {
                assert!(host_belongs_to(host, *platform));
                assert!(host_belongs_to(&format!("www.{host}"), *platform));
            }
        }
    }

    #[test]
    fn matches_real_hostname_not_path() {
        assert!(match_url("http://127.0.0.1/douyin.com/video/1").is_none());
        let matched = match_url("看 https://www.bilibili.com/video/BV1xx411c7mD?p=2。").unwrap();
        assert_eq!(matched.platform, Platform::Bilibili);
        assert_eq!(
            matched.url.as_str(),
            "https://www.bilibili.com/video/BV1xx411c7mD?p=2"
        );
    }
    #[test]
    fn rejects_confusion_redirect_and_private_media() {
        assert!(
            validate_platform_url(
                &Url::parse("https://douyin.com.evil.test/video/1").unwrap(),
                Platform::Douyin
            )
            .is_err()
        );
        assert!(
            resolve_redirect(
                &Url::parse("https://v.douyin.com/a").unwrap(),
                "http://127.0.0.1/admin",
                Platform::Douyin
            )
            .is_err()
        );
        assert!(validate_public_url(&Url::parse("http://192.168.1.10/file.mp4").unwrap()).is_err());
    }

    #[test]
    fn allows_non_standard_media_ports_without_relaxing_page_or_ip_guards() {
        assert!(
            validate_public_url(&Url::parse("https://8.8.8.8:8443/video.m4s").unwrap()).is_ok()
        );
        assert!(
            validate_public_url(&Url::parse("http://192.168.1.10:8443/file.mp4").unwrap()).is_err()
        );
        assert!(
            validate_platform_url(
                &Url::parse("https://www.bilibili.com:8443/video/BV1xx411c7mD").unwrap(),
                Platform::Bilibili,
            )
            .is_err()
        );
    }
}
