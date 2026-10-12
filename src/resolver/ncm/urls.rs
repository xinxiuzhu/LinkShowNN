use anyhow::{Result, ensure};
use reqwest::{Client, header};
use std::time::Duration;
use url::Url;

use crate::resolver::{
    Platform,
    url_policy::{resolve_redirect, validate_platform_url},
};

const MAX_SHARE_REDIRECTS: usize = 5;

/// 复用 manager 创建的禁自动重定向客户端，每跳重新检查平台域名。
/// 直接歌曲 URL 不发额外请求；短链请求显式覆盖 Cookie，隔离登录会话。
pub(super) async fn canonical_song_url(http: &Client, source: &Url) -> Result<Url> {
    validate_platform_url(source, Platform::Ncm)?;
    if !is_short_link(source) {
        ensure!(song_id(source).is_some(), "网易云链接暂仅支持歌曲");
        return Ok(source.clone());
    }
    tokio::time::timeout(Duration::from_secs(15), async {
        let mut current = source.clone();
        for _ in 0..MAX_SHARE_REDIRECTS {
            if !is_short_link(&current) {
                ensure!(song_id(&current).is_some(), "网易云分享链接未指向歌曲");
                return Ok(current);
            }
            let response = http.get(current.clone())
                .header(header::USER_AGENT, crate::resolver::fetch::MOBILE_USER_AGENT)
                // 覆盖意外的默认 Cookie header；这个请求不接收登录会话。
                .header(header::COOKIE, "")
                .timeout(Duration::from_secs(5))
                .send().await.map_err(|_| anyhow::anyhow!("网易云分享短链请求失败"))?;
            ensure!(
                response.url() == &current,
                "网易云短链客户端必须禁用自动重定向"
            );
            ensure!(
                response.status().is_redirection(),
                "网易云分享链接未返回歌曲地址"
            );
            let location = response
                .headers()
                .get(header::LOCATION)
                .and_then(|value| value.to_str().ok())
                .filter(|value| !value.is_empty() && value.len() <= 8192)
                .ok_or_else(|| anyhow::anyhow!("网易云分享重定向地址无效"))?;
            current = resolve_redirect(&current, location, Platform::Ncm)?;
        }
        ensure!(
            !is_short_link(&current) && song_id(&current).is_some(),
            "网易云分享短链重定向次数过多"
        );
        Ok(current)
    })
    .await
    .map_err(|_| anyhow::anyhow!("网易云分享短链请求超时"))?
}

fn is_short_link(url: &Url) -> bool {
    url.host_str()
        .is_some_and(|host| host == "163cn.tv" || host.ends_with(".163cn.tv"))
}

pub(super) fn song_id(url: &Url) -> Option<u64> {
    fn id(value: &str) -> Option<u64> {
        value.parse::<u64>().ok().filter(|id| *id > 0)
    }
    let route = if let Some(fragment) = url
        .fragment()
        .filter(|fragment| fragment.starts_with("/song"))
    {
        Url::parse(&format!("https://music.163.com{fragment}")).ok()?
    } else {
        url.clone()
    };
    let path = route.path().trim_end_matches('/');
    if matches!(path, "/song" | "/m/song" | "/m/song.html") {
        route
            .query_pairs()
            .find(|(key, _)| key == "id")
            .and_then(|(_, value)| id(&value))
    } else {
        path.strip_prefix("/song/").and_then(id)
    }
}
