//! QQ音乐歌曲：直接调用官方公开详情与按账号权限签发的播放地址。

mod client;
mod content;
mod mapping;
mod urls;

use anyhow::{Context, Result};
use reqwest::Client;
use url::Url;

use crate::resolver::{Platform, Resolved, adapter::PlatformAdapter};

pub struct QqMusicAdapter {
    client: client::QqMusicClient,
}

impl QqMusicAdapter {
    pub fn new(http: Client, cookie: String) -> Self {
        Self {
            client: client::QqMusicClient::new(http, cookie),
        }
    }
}

#[async_trait::async_trait]
impl PlatformAdapter for QqMusicAdapter {
    fn platform(&self) -> Platform {
        Platform::QqMusic
    }

    async fn resolve(&self, source: &Url) -> Result<Resolved> {
        let canonical = self.client.expand(source).await?;
        let id = urls::song_id(&canonical).context("QQ音乐链接缺少歌曲 ID")?;
        let track = self.client.detail(&id).await?;
        let mut song = mapping::map_track(&track, &canonical)?;
        // 权限、地域或播放接口暂不可用时，公开信息卡仍然可以投递。
        match self.client.playback(&track).await {
            Ok(Some(stream)) => song.stream = Some(stream),
            Ok(None) => song.notice = Some(if self.client.logged_in() {
                "当前账号未获得本曲播放授权；会员、单曲购买或版权限制以 QQ音乐返回为准。"
            } else {
                "未登录，未获取到本曲播放授权；超级管理员可私聊发送「urlQQ音乐登录」，会员歌曲需要账号拥有相应权益。"
            }.into()),
            Err(_) => song.notice = Some("QQ音乐播放地址暂不可用，已保留歌曲信息。".into()),
        }
        Ok(Resolved::new(song))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[ignore = "匿名官方接口 smoke；用户免费/VIP歌曲分享样本，不读取账号配置"]
    async fn qqmusic_anonymous_user_samples() {
        let http = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(std::time::Duration::from_secs(15))
            .no_proxy()
            .build()
            .unwrap();
        let adapter = QqMusicAdapter::new(http, String::new());
        let free = adapter
            .resolve(&Url::parse("https://c6.y.qq.com/base/fcgi-bin/u?__=Z9qA4eN7R3na").unwrap())
            .await
            .unwrap();
        assert!(free.card().text().contains("远光点"));
        assert_eq!(
            free.card().metadata().author.as_deref(),
            Some("鸣潮先约电台 / Crywolf")
        );
        assert!(free.download_candidate(0).is_some());
        let vip = adapter
            .resolve(&Url::parse("https://c6.y.qq.com/base/fcgi-bin/u?__=2yOIWYtzKZkp").unwrap())
            .await
            .unwrap();
        assert!(vip.card().text().contains("Tamada"));
        assert_eq!(
            vip.card().metadata().author.as_deref(),
            Some("Primo Pecora")
        );
        assert!(vip.card().text().contains("未登录"));
        assert!(vip.download_candidate(0).is_none());
    }
}
