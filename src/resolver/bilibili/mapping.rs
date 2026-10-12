use super::{
    error::Error,
    model::{FavoriteInfo, FavoriteItem, LiveInfo, VideoInfo, VideoPage, VideoViewPayload},
    streams::{
        DashStream, PlayUrl, Quality, choose_quality, media_urls, quality_label, stream_urls,
    },
};
use bpi_rs::{
    bangumi::videostream_url::BangumiVideoStreamData,
    fav::list::{FavListDetailData, FavListMedia},
    live::info::RoomInfoData,
    video::VideoPage as BpiVideoPage,
};

pub(crate) fn bangumi_episode_title(season_title: &str, episode_title: &str) -> String {
    if episode_title.is_empty() {
        season_title.to_string()
    } else {
        format!("{season_title} - {episode_title}")
    }
}

pub(crate) fn normalize_url(value: &str) -> String {
    if value.starts_with("//") {
        format!("https:{value}")
    } else {
        value.to_string()
    }
}

pub(crate) fn map_video_page(page: &BpiVideoPage) -> VideoPage {
    VideoPage {
        cid: Some(page.cid.get()),
        page: page.page,
        part: page.part.clone(),
        duration: page.duration,
        first_frame: None,
    }
}

pub(crate) fn map_video_view(
    payload: &VideoViewPayload,
    description: String,
    pages: Vec<VideoPage>,
    selected_page: Option<VideoPage>,
) -> Result<VideoInfo, Error> {
    let view = &payload.view;
    let favorite = view.stat.favorite.or(view.stat.fav);
    Ok(VideoInfo {
        bvid: Some(view.bvid.to_string()),
        aid: Some(view.aid.get()),
        title: view.title.clone(),
        description,
        cover_url: normalize_url(&view.pic),
        duration: selected_page
            .as_ref()
            .map(|page| page.duration)
            .unwrap_or_default(),
        selected_page,
        is_dash: false,
        author: view.owner.name.clone(),
        author_mid: Some(view.owner.mid.get()),
        avatar_url: (!view.owner.face.trim().is_empty()).then(|| normalize_url(&view.owner.face)),
        published_at: payload.pubdate.filter(|timestamp| *timestamp > 0),
        view_count: Some(view.stat.view),
        danmaku_count: Some(view.stat.danmaku),
        comment_count: Some(view.stat.reply),
        like_count: Some(view.stat.like),
        coin_count: Some(view.stat.coin),
        favorite_count: favorite,
        share_count: Some(view.stat.share),
        pages,
        raw: serde_json::to_value(payload)?,
        ..Default::default()
    })
}

pub(crate) fn apply_dash_and_durl(
    info: &mut VideoInfo,
    videos: Vec<DashStream>,
    audios: Vec<DashStream>,
    durl: Option<(&str, &[String], Option<u64>)>,
    quality: Quality,
) -> Option<u64> {
    info.video_urls.clear();
    info.audio_urls.clear();
    info.video_quality = None;
    let has_dash = !videos.is_empty() || !audios.is_empty();
    let mut actual_quality = None;
    if let Some(stream) = choose_quality(&videos, quality) {
        info.video_urls = stream_urls(stream);
        actual_quality = Some(stream.id);
    }
    if let Some(stream) = audios
        .iter()
        .filter(|stream| !stream_urls(stream).is_empty())
        .max_by_key(|stream| stream.bandwidth)
    {
        info.audio_urls = stream_urls(stream);
    }
    info.is_dash = !info.video_urls.is_empty() && !info.audio_urls.is_empty();
    if !info.is_dash
        && let Some((url, backup_urls, durl_quality)) = durl
        && durl_quality.is_some_and(|id| id <= quality.id() && quality_label(id).is_some())
    {
        info.video_urls = media_urls(url, backup_urls);
        info.audio_urls.clear();
        info.is_dash = false;
        actual_quality = durl_quality;
    } else if has_dash && !info.is_dash {
        info.video_urls.clear();
        info.audio_urls.clear();
    }
    if info.video_urls.is_empty() {
        actual_quality = None;
    }
    info.video_quality = actual_quality.and_then(quality_label).map(str::to_string);
    actual_quality
}

pub(crate) fn apply_play_url(
    info: &mut VideoInfo,
    play: &PlayUrl,
    quality: Quality,
) -> Option<u64> {
    apply_dash_and_durl(
        info,
        play.dash_videos(),
        play.dash_audios(),
        play.progressive(),
        quality,
    )
}

/// HTML5 只补充一个完整的渐进视频；失败、无可验证档位或没有实际升级时
/// 保留已经可用的 DASH（包括视频与音轨）。
pub(crate) fn supplement_html5(
    info: &mut VideoInfo,
    play: &PlayUrl,
    wanted: Quality,
    current_quality: Option<u64>,
) -> Option<u64> {
    let Some((url, backups, actual)) = play.complete_progressive() else {
        return current_quality;
    };
    if actual > wanted.id()
        || quality_label(actual).is_none()
        || (!info.video_urls.is_empty() && current_quality.is_none_or(|id| actual <= id))
    {
        return current_quality;
    }
    let urls = media_urls(url, backups);
    if urls.is_empty() {
        return current_quality;
    }
    info.video_urls = urls;
    info.audio_urls.clear();
    info.is_dash = false;
    info.video_quality = quality_label(actual).map(str::to_string);
    Some(actual)
}

pub(crate) fn needs_anonymous_html5(
    anonymous: bool,
    wanted: Quality,
    has_media: bool,
    actual_quality: Option<u64>,
) -> bool {
    !has_media
        || (anonymous
            && wanted.id() >= Quality::P720.id()
            && actual_quality.is_none_or(|actual| actual < wanted.id()))
}

pub(crate) fn set_quality_notice(
    info: &mut VideoInfo,
    no_login_credentials: bool,
    anonymous_source: bool,
    wanted: Quality,
) {
    info.quality_notice = match (
        no_login_credentials,
        anonymous_source,
        info.video_quality.as_deref(),
    ) {
        (true, _, Some(label)) => {
            let retry =
                if wanted.id() >= Quality::P720.id() && quality_label(wanted.id()) != Some(label) {
                    "；可登录后再尝试更高画质"
                } else {
                    ""
                };
            Some(format!("未登录，本次可获取{label}{retry}"))
        }
        (false, true, Some(_)) => Some("本次使用匿名播放源".into()),
        _ => None,
    };
}

pub(crate) fn apply_bangumi_play_url(
    info: &mut VideoInfo,
    play: &BangumiVideoStreamData,
    quality: Quality,
) {
    let videos = play
        .base
        .dash
        .as_ref()
        .map(|dash| {
            dash.video
                .iter()
                .map(|stream| DashStream {
                    id: u64::from(stream.id),
                    base_url: stream.base_url.clone(),
                    backup_url: stream.backup_url.clone(),
                    bandwidth: u64::from(stream.bandwidth),
                })
                .collect()
        })
        .unwrap_or_default();
    let audios = play
        .base
        .dash
        .as_ref()
        .map(|dash| {
            dash.audio
                .iter()
                .map(|stream| DashStream {
                    id: u64::from(stream.id),
                    base_url: stream.base_url.clone(),
                    backup_url: stream.backup_url.clone(),
                    bandwidth: u64::from(stream.bandwidth),
                })
                .collect()
        })
        .unwrap_or_default();
    let durl = play
        .base
        .durl
        .as_ref()
        .filter(|items| items.len() == 1 && play.base.is_preview.is_none_or(|value| value == 0))
        .and_then(|items| items.first())
        .map(|stream| {
            (
                stream.url.as_str(),
                stream.backup_url.as_slice(),
                Some(u64::from(play.base.quality)),
            )
        });
    let _ = apply_dash_and_durl(info, videos, audios, durl, quality);
}

pub(crate) fn map_live_info(room: &RoomInfoData) -> LiveInfo {
    LiveInfo {
        room_id: u64::try_from(room.room_id).unwrap_or_default(),
        title: room.title.clone(),
        cover_url: Some(normalize_url(if room.user_cover.is_empty() {
            &room.keyframe
        } else {
            &room.user_cover
        })),
        live_status: u8::try_from(room.live_status).unwrap_or_default(),
        online: u64::try_from(room.online).ok(),
        area: Some(room.area_name.clone()),
        anchor: None,
    }
}

fn map_favorite_item(item: &FavListMedia) -> FavoriteItem {
    FavoriteItem {
        title: item.title.clone(),
        cover_url: normalize_url(&item.cover),
        intro: item.intro.clone(),
        bvid: item.bvid.clone().or_else(|| item.bv_id.clone()),
        aid: Some(item.id),
    }
}

pub(crate) fn map_favorite_info(data: &FavListDetailData) -> FavoriteInfo {
    FavoriteInfo {
        id: data.info.id,
        title: data.info.title.clone(),
        owner: Some(data.info.upper.name.clone()),
        total: u64::from(data.info.media_count),
        items: data.medias.iter().map(map_favorite_item).collect(),
    }
}
