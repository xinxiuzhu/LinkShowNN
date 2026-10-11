use super::{
    error::Error,
    model::{FavoriteInfo, FavoriteItem, LiveInfo, VideoInfo, VideoPage, VideoViewPayload},
    streams::{DashStream, Quality, choose_quality, stream_urls},
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

fn unique_urls(base_url: &str, backup_urls: &[String]) -> Vec<String> {
    std::iter::once(base_url.to_string())
        .chain(backup_urls.iter().cloned())
        .filter(|url| !url.is_empty())
        .fold(Vec::new(), |mut urls, url| {
            if !urls.contains(&url) {
                urls.push(url);
            }
            urls
        })
}

pub(crate) fn apply_dash_and_durl(
    info: &mut VideoInfo,
    videos: Vec<DashStream>,
    audios: Vec<DashStream>,
    durl: Option<(&str, &[String])>,
    quality: Quality,
) {
    let has_dash = !videos.is_empty() || !audios.is_empty();
    if let Some(stream) = choose_quality(&videos, quality) {
        info.video_urls = stream_urls(stream);
    }
    if let Some(stream) = audios.iter().max_by_key(|stream| stream.bandwidth) {
        info.audio_urls = stream_urls(stream);
    }
    info.is_dash = !info.video_urls.is_empty() && !info.audio_urls.is_empty();
    if !info.is_dash
        && let Some((url, backup_urls)) = durl
    {
        info.video_urls = unique_urls(url, backup_urls);
        info.audio_urls.clear();
        info.is_dash = false;
    } else if has_dash && !info.is_dash {
        info.video_urls.clear();
        info.audio_urls.clear();
    }
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
        .and_then(|items| items.first())
        .map(|stream| (stream.url.as_str(), stream.backup_url.as_slice()));
    apply_dash_and_durl(info, videos, audios, durl, quality);
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
