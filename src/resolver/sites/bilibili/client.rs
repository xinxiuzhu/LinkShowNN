use super::{
    comments::AnonymousComments,
    error::Error,
    mapping::{
        apply_bangumi_play_url, apply_dash_and_durl, bangumi_episode_title, map_favorite_info,
        map_live_info, map_video_page, map_video_view, normalize_url,
    },
    model::{FavoriteInfo, LiveInfo, VideoInfo, VideoPage, VideoViewPayload},
    streams::{PlayUrl, Quality},
    urls::Target,
};
use bpi_rs::{
    BpiClient,
    article::ArticleInfoParams,
    bangumi::{BangumiDetailParams, BangumiInfoParams, BangumiVideoStreamParams},
    dynamic::DynamicDetailParams,
    fav::FavListDetailParams,
    ids::{Aid, Bvid, DynamicId, EpisodeId, MediaId, SeasonId},
    models::Fnval,
    request::BilibiliRequest,
    video::VideoDescParams,
};
use reqwest_012::{
    Client as HttpClient, Proxy as HttpProxy,
    header::{REFERER, USER_AGENT},
};
use serde_json::Value;
use std::sync::Arc;
use url::Url;

#[derive(Clone)]
pub struct Client {
    api: Arc<BpiClient>,
    http: HttpClient,
    cookie: Option<String>,
    comments: AnonymousComments,
    comments_enabled: bool,
}

#[cfg(test)]
mod comment_client_tests {
    use super::*;

    #[test]
    fn anonymous_comment_request_never_inherits_configured_video_cookie() {
        let client = Client::new(Some("SESSDATA=only-for-video; bili_jct=private"))
            .unwrap()
            .with_comments(true)
            .with_proxy(None)
            .unwrap();
        assert!(client.cookie.is_some());
        let request = client.comments.request(117332500289291).build().unwrap();
        assert!(!request.headers().contains_key(reqwest_012::header::COOKIE));
        assert!(
            !request
                .headers()
                .contains_key(reqwest_012::header::AUTHORIZATION)
        );
        assert_eq!(request.url().host_str(), Some("api.bilibili.com"));
        let query = request
            .url()
            .query_pairs()
            .collect::<std::collections::HashMap<_, _>>();
        assert_eq!(query.get("mode").map(|value| value.as_ref()), Some("3"));
        assert_eq!(query.get("ps").map(|value| value.as_ref()), Some("20"));
    }

    #[tokio::test]
    async fn comments_are_disabled_unless_rendering_explicitly_enables_them() {
        let client = Client::new(None).unwrap();
        assert!(!client.comments_enabled);
        assert!(client.comments_for(117332500289291).await.is_empty());
    }
}
impl Client {
    pub fn new(cookie: Option<&str>) -> Result<Self, Error> {
        let api = BpiClient::builder()
            .no_proxy(true)
            .timeout(std::time::Duration::from_secs(15))
            .connect_timeout(std::time::Duration::from_secs(5));
        let api = if let Some(cookie) = cookie.filter(|value| !value.trim().is_empty()) {
            api.cookie(cookie.trim())
        } else {
            api
        }
        .build()?;
        Ok(Self {
            api: Arc::new(api),
            http: HttpClient::builder()
                .connect_timeout(std::time::Duration::from_secs(5))
                .timeout(std::time::Duration::from_secs(15))
                .redirect(reqwest_012::redirect::Policy::none())
                .no_proxy()
                .build()?,
            cookie: cookie
                .filter(|value| !value.trim().is_empty())
                .map(|value| value.trim().to_string()),
            comments: AnonymousComments::new(None)?,
            comments_enabled: false,
        })
    }
    pub fn with_comments(mut self, enabled: bool) -> Self {
        self.comments_enabled = enabled;
        self
    }
    pub fn with_proxy(mut self, proxy: Option<&str>) -> Result<Self, Error> {
        let mut api = BpiClient::builder()
            .no_proxy(true)
            .timeout(std::time::Duration::from_secs(15))
            .connect_timeout(std::time::Duration::from_secs(5));
        if let Some(cookie) = self.cookie.as_deref() {
            api = api.cookie(cookie);
        }
        if let Some(proxy) = proxy.filter(|value| !value.trim().is_empty()) {
            api = api.proxy(reqwest_012::Proxy::all(proxy.trim())?);
        }
        self.api = Arc::new(api.build()?);
        let mut http = HttpClient::builder()
            .connect_timeout(std::time::Duration::from_secs(5))
            .timeout(std::time::Duration::from_secs(15))
            .redirect(reqwest_012::redirect::Policy::none())
            .no_proxy();
        if let Some(proxy) = proxy.filter(|value| !value.trim().is_empty()) {
            http = http.proxy(HttpProxy::all(proxy.trim())?);
        } else {
            http = http.no_proxy();
        }
        self.http = http.build()?;
        self.comments = AnonymousComments::new(proxy)?;
        Ok(self)
    }

    async fn comments_for(&self, aid: u64) -> Vec<crate::resolver::card::CardComment> {
        if self.comments_enabled {
            self.comments.fetch(aid).await
        } else {
            Vec::new()
        }
    }
    pub async fn resolve_short_url(&self, source: &Url) -> Result<Url, Error> {
        let mut current = source.clone();
        for _ in 0..=5 {
            let mut request = self
                .http
                .get(current.clone())
                .header(USER_AGENT, "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 Chrome/131 Safari/537.36")
                .header(REFERER, "https://www.bilibili.com/");
            if let Some(cookie) = self.cookie.as_deref() {
                request = request.header(reqwest_012::header::COOKIE, cookie);
            }
            let response = request.send().await?;
            if !response.status().is_redirection() {
                return Ok(current);
            }
            let location = response
                .headers()
                .get(reqwest_012::header::LOCATION)
                .and_then(|value| value.to_str().ok())
                .ok_or_else(|| Error::InvalidUrl("redirect has no Location".to_string()))?;
            let target = current
                .join(location)
                .map_err(|error| Error::InvalidUrl(error.to_string()))?;
            let host = target
                .host_str()
                .unwrap_or_default()
                .trim_end_matches('.')
                .to_ascii_lowercase();
            let allowed = (host == "bilibili.com"
                || host.ends_with(".bilibili.com")
                || host == "b23.tv"
                || host.ends_with(".b23.tv")
                || host == "bili2233.cn"
                || host.ends_with(".bili2233.cn"))
                && matches!(target.scheme(), "http" | "https")
                && target.username().is_empty()
                && target.password().is_none()
                && matches!(target.port(), None | Some(80) | Some(443));
            if !allowed {
                return Err(Error::InvalidUrl(
                    "short URL redirected to an unsafe Bilibili URL".to_string(),
                ));
            }
            current = target;
        }
        Err(Error::InvalidUrl("too many redirects".to_string()))
    }
    /// 自行请求并解析 playurl。bpi-rs 0.3.0 已把 `dash.flac.audio` 从必需的
    /// `Vec` 改为可空、并兼容单对象，但 stream 对象仍会同时下发 `baseUrl`/`base_url`
    /// 两个同义键，`#[serde(alias)]` 会判定字段重复而解码失败，因此保留本层逐键容错解析。
    async fn fetch_play_url(
        &self,
        bvid: Option<&str>,
        aid: Option<u64>,
        cid: u64,
        quality: Quality,
    ) -> Result<PlayUrl, Error> {
        let mut url = Url::parse("https://api.bilibili.com/x/player/playurl")
            .map_err(|error| Error::Schema(error.to_string()))?;
        {
            let mut query = url.query_pairs_mut();
            if let Some(bvid) = bvid {
                query.append_pair("bvid", bvid);
            } else if let Some(aid) = aid {
                query.append_pair("avid", &aid.to_string());
            } else {
                return Err(Error::Schema("video target has no bvid or aid".to_string()));
            }
            query.append_pair("cid", &cid.to_string());
            query.append_pair("qn", &quality.id().to_string());
            query.append_pair("fnval", "4048");
            query.append_pair("fnver", "0");
            query.append_pair("fourk", "1");
        }
        let mut request = self
            .http
            .get(url)
            .header(
                USER_AGENT,
                "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 Chrome/131 Safari/537.36",
            )
            .header(REFERER, "https://www.bilibili.com/");
        if let Some(cookie) = self.cookie.as_deref() {
            request = request.header(reqwest_012::header::COOKIE, cookie);
        }
        let body = request
            .send()
            .await?
            .error_for_status()?
            .json::<Value>()
            .await?;
        let code = body.get("code").and_then(Value::as_i64).unwrap_or(-1);
        if code != 0 {
            let message = body
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("unknown playurl error");
            return Err(Error::Schema(format!("playurl failed: {code} {message}")));
        }
        let data = body
            .get("data")
            .ok_or_else(|| Error::Schema("playurl response has no data".to_string()))?;
        Ok(serde_json::from_value(data.clone())?)
    }

    /// 保留 pubdate，同时沿用 bpi-rs 的 Cookie、代理、响应及 API 错误校验。
    /// 仍只请求一次原 view 接口，不为海报元数据增加额外网络往返。
    async fn fetch_video_view(
        &self,
        bvid: Option<&str>,
        aid: Option<u64>,
    ) -> Result<VideoViewPayload, Error> {
        let query = if let Some(bvid) = bvid {
            ("bvid", Bvid::new(bvid)?.to_string())
        } else if let Some(aid) = aid {
            ("aid", Aid::new(aid)?.to_string())
        } else {
            return Err(Error::Schema("video target has no bvid or aid".to_string()));
        };
        Ok(self
            .api
            .get("https://api.bilibili.com/x/web-interface/view")
            .query(&[query])
            .send_bpi_payload("video.view")
            .await?)
    }

    pub async fn video(
        &self,
        bvid: Option<&str>,
        aid: Option<u64>,
        page: u32,
        quality: Quality,
    ) -> Result<VideoInfo, Error> {
        let payload = self.fetch_video_view(bvid, aid).await?;
        let view = &payload.view;
        let desc_params = if let Some(bvid) = bvid {
            VideoDescParams::from_bvid(Bvid::new(bvid)?)
        } else {
            VideoDescParams::from_aid(Aid::new(aid.unwrap_or_default())?)
        };
        let video_api = self.api.video();
        let (description, comments) = tokio::join!(
            video_api.desc(desc_params),
            self.comments_for(view.aid.get())
        );
        let description = description.unwrap_or_default();
        let pages = if view.pages.is_empty() {
            vec![VideoPage {
                cid: Some(view.cid.get()),
                page: 1,
                part: view.title.clone(),
                duration: 0,
                first_frame: Some(normalize_url(&view.pic)),
            }]
        } else {
            view.pages.iter().map(map_video_page).collect()
        };
        let selected_page = pages
            .iter()
            .find(|item| item.page == page)
            .or_else(|| pages.first())
            .cloned();
        let mut info = map_video_view(&payload, description, pages, selected_page.clone())?;
        info.comments = comments;
        if let Some(selected_page) = selected_page
            && let Some(cid) = selected_page.cid
        {
            let play = self
                .fetch_play_url(info.bvid.as_deref(), info.aid, cid, quality)
                .await?;
            apply_dash_and_durl(
                &mut info,
                play.dash_videos(),
                play.dash_audios(),
                play.progressive(),
                quality,
            );
        }
        Ok(info)
    }

    pub async fn bangumi(
        &self,
        ep_id: Option<u64>,
        season_id: Option<u64>,
        media_id: Option<u64>,
        page: u32,
        quality: Quality,
    ) -> Result<VideoInfo, Error> {
        let season_id = if season_id.is_none()
            && ep_id.is_none()
            && let Some(media_id) = media_id
        {
            Some(
                self.api
                    .bangumi()
                    .info(BangumiInfoParams::new(MediaId::new(media_id)?))
                    .await?
                    .media
                    .season_id,
            )
        } else {
            season_id
        };
        let detail_params = if let Some(ep_id) = ep_id {
            BangumiDetailParams::from_episode_id(EpisodeId::new(ep_id)?)
        } else if let Some(season_id) = season_id {
            BangumiDetailParams::from_season_id(SeasonId::new(season_id)?)
        } else {
            return Err(Error::Schema(
                "Bangumi target has no episode, season, or media id".to_string(),
            ));
        };
        let detail = self.api.bangumi().detail(detail_params).await?;
        let episode = ep_id
            .and_then(|wanted| {
                detail
                    .episodes
                    .iter()
                    .find(|episode| episode.id == wanted || episode.ep_id == wanted)
            })
            .or_else(|| detail.episodes.get(page.saturating_sub(1) as usize))
            .or_else(|| detail.episodes.first())
            .ok_or_else(|| Error::Schema("Bangumi response has no episode".to_string()))?;
        let payload = self.fetch_video_view(Some(&episode.bvid), None).await?;
        let view = &payload.view;
        let video_api = self.api.video();
        let (description, comments) = tokio::join!(
            video_api.desc(VideoDescParams::from_bvid(Bvid::new(&episode.bvid)?)),
            self.comments_for(view.aid.get())
        );
        let description = description.unwrap_or_default();
        let pages = if view.pages.is_empty() {
            vec![VideoPage {
                cid: Some(episode.cid),
                page: 1,
                part: episode.title.clone(),
                duration: episode.duration / 1000,
                first_frame: None,
            }]
        } else {
            view.pages.iter().map(map_video_page).collect()
        };
        let selected_page = pages.first().cloned();
        let mut info = map_video_view(&payload, description, pages, selected_page)?;
        info.comments = comments;
        let play = self
            .api
            .bangumi()
            .video_stream(
                BangumiVideoStreamParams::from_episode_id(EpisodeId::new(episode.ep_id)?)
                    .with_quality(quality.bpi())
                    .with_fnval(Fnval::DASH),
            )
            .await?;
        apply_bangumi_play_url(&mut info, &play, quality);
        info.title = bangumi_episode_title(&detail.season_title, &episode.long_title);
        info.cover_url = normalize_url(&episode.cover);
        Ok(info)
    }

    pub async fn live(&self, room_id: u64) -> Result<LiveInfo, Error> {
        let room_id = i64::try_from(room_id)
            .map_err(|_| Error::Schema("live room id exceeds i64".to_string()))?;
        let room = self.api.live().room_info(room_id).await?;
        Ok(map_live_info(&room))
    }

    pub async fn favorite(&self, id: u64) -> Result<FavoriteInfo, Error> {
        let params = FavListDetailParams::new(MediaId::new(id)?).page(1)?;
        let data = self.api.fav().list_detail(params).await?;
        Ok(map_favorite_info(&data))
    }
    pub async fn raw_target(&self, target: &Target) -> Result<Value, Error> {
        match target {
            Target::Opus { id } => Ok(serde_json::to_value(
                self.api
                    .dynamic()
                    .detail(DynamicDetailParams::new(DynamicId::new(id.to_string())?))
                    .await?,
            )?),
            Target::Article { id } => Ok(serde_json::to_value(
                self.api
                    .article()
                    .info(ArticleInfoParams::new(i64::try_from(*id).map_err(
                        |_| Error::Schema("article id exceeds i64".to_string()),
                    )?)?)
                    .await?,
            )?),
            Target::Bangumi {
                ep_id,
                season_id,
                media_id,
                page,
            } => Ok(serde_json::to_value(
                self.bangumi(*ep_id, *season_id, *media_id, *page, Quality::P720)
                    .await?,
            )?),
            Target::Video {
                bvid, aid, page, ..
            } => Ok(serde_json::to_value(
                self.video(bvid.as_deref(), *aid, *page, Quality::P720)
                    .await?,
            )?),
            Target::Live { room_id } => Ok(serde_json::to_value(self.live(*room_id).await?)?),
            Target::Favorite { id } => Ok(serde_json::to_value(self.favorite(*id).await?)?),
        }
    }
}
