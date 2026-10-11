use super::mapping::{
    apply_dash_and_durl, bangumi_episode_title, map_favorite_info, map_live_info, map_video_page,
    map_video_view,
};
use bpi_rs::{fav::list::FavListDetailData, live::info::RoomInfoData};

use super::{
    model::{VideoInfo, VideoViewPayload},
    streams::{DashStream, DurlStream, PlayUrl, Quality, choose_quality, stream_urls},
    urls::{Target, parse_url},
};
#[test]
fn extracts_targets_and_page() {
    assert!(matches!(
        parse_url("https://www.bilibili.com/video/BV1xx411c7mD?p=2")
            .unwrap()
            .unwrap()
            .target,
        Target::Video { page: 2, .. }
    ));
    assert!(matches!(
        parse_url("https://live.bilibili.com/123")
            .unwrap()
            .unwrap()
            .target,
        Target::Live { room_id: 123 }
    ));
    assert!(matches!(
        parse_url("https://www.bilibili.com/read/cv42")
            .unwrap()
            .unwrap()
            .target,
        Target::Article { id: 42 }
    ));
}
#[test]
fn chooses_quality_and_dedupes_backups() {
    let stream = DashStream {
        id: 64,
        base_url: "a".into(),
        backup_url: vec!["a".into(), "b".into()],
        bandwidth: 1,
    };
    assert_eq!(stream_urls(&stream), vec!["a", "b"]);
    assert_eq!(choose_quality(&[stream], Quality::P1080).unwrap().id, 64);
}
fn video_fixture() -> VideoViewPayload {
    serde_json::from_value(serde_json::json!({
        "aid": 42,
        "bvid": "BV1xx411c7mD",
        "videos": 1,
        "title": "视频",
        "pic": "//cdn/cover.jpg",
        "pubdate": 1_700_000_000,
        "owner": {"mid": 7, "name": "作者", "face": "//cdn/avatar.jpg"},
        "stat": {"aid": 42, "view": 10, "danmaku": 5, "reply": 6, "favorite": 4, "fav": null, "coin": 3, "share": 7, "like": 2},
        "cid": 101,
        "pages": [{"cid": 101, "page": 1, "part": "第一P", "duration": 30}]
    }))
    .unwrap()
}

#[test]
fn video_cover_comes_from_pic_even_when_selected_page_has_another_first_frame() {
    use super::content::VideoContent;
    use crate::resolver::ResolvedContent;

    let view = video_fixture();
    let mut selected = map_video_page(&view.view.pages[0]);
    selected.first_frame = Some("https://cdn/first-frame.jpg".into());
    let info =
        map_video_view(&view, "简介".into(), vec![selected.clone()], Some(selected)).unwrap();
    assert_eq!(info.cover_url, "https://cdn/cover.jpg");
    assert_eq!(info.avatar_url.as_deref(), Some("https://cdn/avatar.jpg"));
    assert_eq!(info.published_at, Some(1_700_000_000));
    assert_eq!(info.raw["pubdate"], 1_700_000_000);
    let card = VideoContent(info).card();
    assert_eq!(
        card.image_urls().collect::<Vec<_>>(),
        ["https://cdn/cover.jpg"]
    );
    assert_eq!(
        card.metadata().video_cover_url.as_deref(),
        Some("https://cdn/cover.jpg")
    );
    assert_eq!(card.metadata().published_at, Some(1_700_000_000));
    let poster = card.metadata().poster.as_ref().unwrap();
    assert_eq!(poster.content_id.as_deref(), Some("BV1xx411c7mD"));
    assert_eq!(poster.author_id.as_deref(), Some("7"));
    assert_eq!(poster.view_count, Some(10));
    assert_eq!(poster.danmaku_count, Some(5));
    assert_eq!(poster.comment_count, Some(6));
    assert_eq!(poster.like_count, Some(2));
    assert_eq!(poster.coin_count, Some(3));
    assert_eq!(poster.favorite_count, Some(4));
    assert_eq!(poster.share_count, Some(7));
}

#[test]
fn absent_or_nonpositive_pubdate_stays_unknown() {
    for pubdate in [
        None,
        Some(serde_json::json!(null)),
        Some(serde_json::json!(0)),
        Some(serde_json::json!(-1)),
    ] {
        let mut raw = serde_json::to_value(video_fixture()).unwrap();
        match pubdate {
            Some(value) => raw["pubdate"] = value,
            None => {
                raw.as_object_mut().unwrap().remove("pubdate");
            }
        }
        let view: VideoViewPayload = serde_json::from_value(raw).unwrap();
        let info = map_video_view(&view, String::new(), Vec::new(), None).unwrap();
        assert!(info.published_at.is_none());
    }
}

fn play_url_fixture(dash: bool) -> PlayUrl {
    let value = if dash {
        serde_json::json!({
            "dash": {
                "video": [{
                    "id": 64,
                    "baseUrl": "https://cdn/video.m4s",
                    "backupUrl": ["https://cdn/video.m4s", "https://backup/video.m4s"],
                    "bandwidth": 100
                }],
                "audio": [{
                    "id": 30280,
                    "baseUrl": "https://cdn/audio.m4s",
                    "backupUrl": [],
                    "bandwidth": 50
                }],
                "flac": {"display": true, "audio": null},
                "dolby": {"type": 0, "audio": null}
            }
        })
    } else {
        serde_json::json!({
            "durl": [{
                "url": "https://cdn/video.mp4",
                "backup_url": ["https://cdn/video.mp4", "https://backup/video.mp4"]
            }]
        })
    };
    serde_json::from_value(value).unwrap()
}

fn apply_play_url(info: &mut VideoInfo, play: &PlayUrl, quality: Quality) {
    apply_dash_and_durl(
        info,
        play.dash_videos(),
        play.dash_audios(),
        play.progressive(),
        quality,
    );
}

/// 上游对无 Hi-Res 音轨的视频返回 `flac: {"display": true, "audio": null}`，
/// 且 `flac.audio` 是单对象而 `dolby.audio` 是数组。
#[test]
fn decodes_null_lossless_audio_and_falls_back_across_audio_tracks() {
    assert!(
        play_url_fixture(true)
            .dash
            .unwrap()
            .flac
            .unwrap()
            .audio
            .is_none()
    );

    let dolby: PlayUrl = serde_json::from_value(serde_json::json!({
        "dash": {
            "video": [{"id": 64, "baseUrl": "https://cdn/video.m4s"}],
            "audio": null,
            "dolby": {"type": 1, "audio": [{"id": 30250, "baseUrl": "https://cdn/dolby.m4s"}]}
        }
    }))
    .unwrap();
    assert_eq!(dolby.dash_audios().len(), 1);
    assert_eq!(dolby.dash_audios()[0].base_url, "https://cdn/dolby.m4s");

    let lossless: PlayUrl = serde_json::from_value(serde_json::json!({
        "dash": {
            "video": [{"id": 64, "baseUrl": "https://cdn/video.m4s"}],
            "flac": {"display": true, "audio": {"id": 30251, "baseUrl": "https://cdn/flac.m4s"}}
        }
    }))
    .unwrap();
    assert_eq!(lossless.dash_audios()[0].base_url, "https://cdn/flac.m4s");
}

/// 上游在同一个对象里同时下发 camelCase 与 snake_case 同义键。
#[test]
fn decodes_streams_carrying_duplicate_synonym_url_keys() {
    let stream: DashStream = serde_json::from_value(serde_json::json!({
        "id": 64,
        "baseUrl": "https://cdn/video.m4s",
        "base_url": "https://cdn/video.m4s",
        "backupUrl": ["https://backup-a/video.m4s"],
        "backup_url": ["https://backup-b/video.m4s"],
        "bandwidth": 100
    }))
    .unwrap();
    assert_eq!(stream.base_url, "https://cdn/video.m4s");
    assert_eq!(
        stream.backup_url,
        vec!["https://backup-a/video.m4s", "https://backup-b/video.m4s"]
    );
    assert_eq!(
        stream_urls(&stream),
        vec![
            "https://cdn/video.m4s",
            "https://backup-a/video.m4s",
            "https://backup-b/video.m4s"
        ]
    );
}

/// durl 目前只下发 `backup_url`，但同义键出现时也不应整体解析失败。
#[test]
fn decodes_durl_streams_with_either_backup_url_key() {
    let current: DurlStream = serde_json::from_value(serde_json::json!({
        "url": "https://cdn/video.mp4",
        "backup_url": ["https://backup-b/video.mp4"]
    }))
    .unwrap();
    assert_eq!(current.url, "https://cdn/video.mp4");
    assert_eq!(current.backup_url, vec!["https://backup-b/video.mp4"]);

    let both: DurlStream = serde_json::from_value(serde_json::json!({
        "url": "https://cdn/video.mp4",
        "backupUrl": ["https://backup-a/video.mp4"],
        "backup_url": ["https://backup-b/video.mp4"]
    }))
    .unwrap();
    assert_eq!(
        both.backup_url,
        vec!["https://backup-a/video.mp4", "https://backup-b/video.mp4"]
    );
}

#[test]
fn maps_video_stream_and_bangumi_fixtures_offline() {
    let view = video_fixture();
    let pages = view
        .view
        .pages
        .iter()
        .map(map_video_page)
        .collect::<Vec<_>>();
    let selected = pages.first().cloned();
    let mut info = map_video_view(&view, "简介".into(), pages, selected).unwrap();
    assert_eq!(info.duration, 30);
    assert_eq!(info.selected_page.as_ref().unwrap().part, "第一P");
    apply_play_url(&mut info, &play_url_fixture(true), Quality::P720);
    assert!(info.is_dash);
    assert_eq!(
        info.video_urls,
        vec!["https://cdn/video.m4s", "https://backup/video.m4s"]
    );
    assert_eq!(info.audio_urls, vec!["https://cdn/audio.m4s"]);

    let mut video_only = play_url_fixture(true);
    let video_only_dash = video_only.dash.as_mut().unwrap();
    video_only_dash.audio = None;
    video_only_dash.flac = None;
    video_only_dash.dolby = None;
    let mut video_only_info = map_video_view(
        &view,
        String::new(),
        view.view.pages.iter().map(map_video_page).collect(),
        Some(map_video_page(&view.view.pages[0])),
    )
    .unwrap();
    apply_play_url(&mut video_only_info, &video_only, Quality::P720);
    assert!(video_only_info.video_urls.is_empty());
    assert!(video_only_info.audio_urls.is_empty());
    assert!(!video_only_info.is_dash);
    assert_eq!(bangumi_episode_title("番剧", "第一话"), "番剧 - 第一话");
}

#[test]
fn falls_back_to_progressive_fixture_and_maps_favorite_live() {
    let view = video_fixture();
    let pages = view
        .view
        .pages
        .iter()
        .map(map_video_page)
        .collect::<Vec<_>>();
    let mut info =
        map_video_view(&view, String::new(), pages.clone(), pages.first().cloned()).unwrap();
    apply_play_url(&mut info, &play_url_fixture(false), Quality::P1080);
    assert!(!info.is_dash);
    assert!(info.audio_urls.is_empty());
    assert_eq!(
        info.video_urls,
        vec!["https://cdn/video.mp4", "https://backup/video.mp4"]
    );

    let favorite: FavListDetailData = serde_json::from_value(serde_json::json!({
        "info": {
            "id": 5, "fid": 5, "mid": 7, "attr": 0, "title": "收藏夹", "cover": "",
            "upper": {"mid": 7, "name": "收藏者", "face": "", "followed": null, "vip_type": null, "vip_status": null},
            "cover_type": 0, "cnt_info": {"collect": 0, "play": 0, "share": null, "thumb_up": null, "danmaku": null, "view_text_1": null},
            "type": 0, "intro": "", "ctime": 0, "mtime": 0, "state": 0, "fav_state": 1, "like_state": 0, "media_count": 1
        },
        "medias": [{
            "id": 42, "type": 2, "title": "收藏视频", "cover": "//cdn/fav.jpg", "intro": "收藏简介", "page": 1, "duration": 30,
            "upper": {"mid": 7, "name": "作者", "face": "", "followed": null, "vip_type": null, "vip_status": null},
            "attr": 0, "cnt_info": {"collect": 0, "play": 0, "share": null, "thumb_up": null, "danmaku": null, "view_text_1": null},
            "link": "", "ctime": 0, "pubtime": 0, "fav_time": 0, "bv_id": null, "bvid": "BV1xx411c7mD", "season": null
        }],
        "has_more": false, "ttl": 0
    })).unwrap();
    let mapped = map_favorite_info(&favorite);
    assert_eq!(mapped.total, 1);
    assert_eq!(mapped.items[0].aid, Some(42));
    assert_eq!(mapped.items[0].cover_url, "https://cdn/fav.jpg");
    assert_eq!(mapped.items[0].intro, "收藏简介");

    let room: RoomInfoData = serde_json::from_value(serde_json::json!({
        "uid": 7, "room_id": 123, "short_id": 0, "attention": 1, "online": 99,
        "is_portrait": false, "description": "", "live_status": 1, "area_id": 1,
        "parent_area_id": 1, "parent_area_name": "", "old_area_id": 0, "background": "",
        "title": "直播", "user_cover": "//cdn/live.jpg", "keyframe": "", "live_time": "",
        "tags": "", "room_silent_type": "", "room_silent_level": 0, "room_silent_second": 0,
        "area_name": "游戏", "hot_words": [], "hot_words_status": 0,
        "new_pendants": {"frame": {"name":"","value":"","position":0,"desc":"","area":0,"area_old":0,"bg_color":"","bg_pic":"","use_old_area":false}, "mobile_frame":null,"badge":null,"mobile_badge":null},
        "pk_status": 0, "pk_id": 0, "allow_change_area_time": 0, "allow_upload_cover_time": 0,
        "studio_info": null
    })).unwrap();
    let live = map_live_info(&room);
    assert_eq!(live.room_id, 123);
    assert_eq!(live.cover_url.as_deref(), Some("https://cdn/live.jpg"));
    assert_eq!(live.online, Some(99));
}
