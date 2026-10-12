use super::mapping::{
    apply_dash_and_durl, bangumi_episode_title, map_favorite_info, map_live_info, map_video_page,
    map_video_view, needs_anonymous_html5, set_quality_notice, supplement_html5,
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
        base_url: "https://cdn/video.m4s".into(),
        backup_url: vec![
            "https://cdn/video.m4s".into(),
            "https://backup/video.m4s".into(),
        ],
        bandwidth: 1,
    };
    assert_eq!(
        stream_urls(&stream),
        vec!["https://cdn/video.m4s", "https://backup/video.m4s"]
    );
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
            "quality": 64,
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
            "quality": 32,
            "durl": [{
                "url": "https://cdn/video.mp4",
                "backup_url": ["https://cdn/video.mp4", "https://backup/video.mp4"]
            }]
        })
    };
    serde_json::from_value(value).unwrap()
}

fn apply_play_url(info: &mut VideoInfo, play: &PlayUrl, quality: Quality) {
    let _ = apply_dash_and_durl(
        info,
        play.dash_videos(),
        play.dash_audios(),
        play.progressive(),
        quality,
    );
}

fn complete_html5_fixture(quality: u64) -> PlayUrl {
    serde_json::from_value(serde_json::json!({
        "quality": quality,
        "timelength": 30_000,
        "durl": [{
            "url": "https://cdn/html5.mp4",
            "backup_url": ["https://cdn/html5.mp4", "https://backup/html5.mp4"],
            "length": 30_000
        }]
    }))
    .unwrap()
}

#[test]
fn selected_dash_quality_comes_from_the_track_not_the_top_level_claim() {
    let mut play = play_url_fixture(true);
    play.quality = Some(64);
    play.dash.as_mut().unwrap().video[0].id = 32;
    let mut info = VideoInfo::default();
    apply_play_url(&mut info, &play, Quality::P720);
    assert!(info.is_dash);
    assert_eq!(info.video_quality.as_deref(), Some("480P"));
    set_quality_notice(&mut info, true, true, Quality::P720);
    assert_eq!(
        info.quality_notice.as_deref(),
        Some("未登录，本次可获取480P；可登录后再尝试更高画质")
    );
}

#[test]
fn choosing_dash_never_exceeds_the_configured_maximum_or_selects_unknown_quality() {
    let streams = [
        DashStream {
            id: 80,
            bandwidth: 1000,
            base_url: "https://cdn/1080.m4s".into(),
            ..Default::default()
        },
        DashStream {
            id: 999,
            bandwidth: 9999,
            base_url: "https://cdn/unknown.m4s".into(),
            ..Default::default()
        },
    ];
    assert!(choose_quality(&streams, Quality::P720).is_none());
    assert_eq!(choose_quality(&streams, Quality::P1080).unwrap().id, 80);
    let only_720_60 = [DashStream {
        id: 74,
        base_url: "https://cdn/720-60.m4s".into(),
        ..Default::default()
    }];
    assert!(choose_quality(&only_720_60, Quality::P720).is_none());
    let mut sub_1080 = only_720_60.to_vec();
    sub_1080.push(DashStream {
        id: 32,
        base_url: "https://cdn/480.m4s".into(),
        ..Default::default()
    });
    assert_eq!(choose_quality(&sub_1080, Quality::P1080).unwrap().id, 74);
    let mut play = complete_html5_fixture(80);
    let mut info = VideoInfo::default();
    apply_play_url(&mut info, &play, Quality::P720);
    assert!(info.video_urls.is_empty());
    play.quality = Some(32);
    apply_play_url(&mut info, &play, Quality::P480);
    assert_eq!(info.video_quality.as_deref(), Some("480P"));
}

#[test]
fn anonymous_html5_is_only_requested_for_a_missing_target_or_unusable_primary() {
    assert!(needs_anonymous_html5(true, Quality::P720, true, Some(32)));
    assert!(needs_anonymous_html5(true, Quality::P1080, true, Some(64)));
    assert!(needs_anonymous_html5(true, Quality::P360, false, None));
    assert!(!needs_anonymous_html5(true, Quality::P720, true, Some(64)));
    assert!(!needs_anonymous_html5(true, Quality::P480, true, Some(16)));
    assert!(!needs_anonymous_html5(
        false,
        Quality::P1080,
        true,
        Some(32)
    ));
    assert!(needs_anonymous_html5(false, Quality::P1080, false, None));
}

#[test]
fn complete_higher_html5_replaces_lower_dash_and_transfers_real_quality_to_poster() {
    use super::content::VideoContent;
    use crate::resolver::ResolvedContent;

    let mut dash = play_url_fixture(true);
    dash.dash.as_mut().unwrap().video[0].id = 32;
    let mut info = VideoInfo {
        cover_url: "https://cdn/cover.jpg".into(),
        ..Default::default()
    };
    apply_play_url(&mut info, &dash, Quality::P720);
    assert_eq!(
        supplement_html5(
            &mut info,
            &complete_html5_fixture(64),
            Quality::P720,
            Some(32)
        ),
        Some(64)
    );
    assert!(!info.is_dash);
    assert!(info.audio_urls.is_empty());
    assert_eq!(
        info.video_urls,
        ["https://cdn/html5.mp4", "https://backup/html5.mp4"]
    );
    set_quality_notice(&mut info, true, true, Quality::P720);
    let card = VideoContent(info).card();
    let poster = card.metadata().poster.as_ref().unwrap();
    assert_eq!(poster.video_quality.as_deref(), Some("720P"));
    assert_eq!(
        poster.quality_notice.as_deref(),
        Some("未登录，本次可获取720P")
    );
}

#[test]
fn equal_lower_over_limit_or_incomplete_html5_preserves_complete_dash() {
    let mut incomplete = complete_html5_fixture(80);
    incomplete.durl.as_mut().unwrap()[0].length = Some(10_000);
    let mut segmented = complete_html5_fixture(80);
    segmented.durl.as_mut().unwrap().push(DurlStream {
        url: "https://cdn/part2.mp4".into(),
        ..Default::default()
    });
    let mut preview = complete_html5_fixture(80);
    preview.is_preview = true;
    let mut no_length = complete_html5_fixture(80);
    no_length.durl.as_mut().unwrap()[0].length = None;
    let mut unsafe_source = complete_html5_fixture(80);
    unsafe_source.durl.as_mut().unwrap()[0].url = "http://127.0.0.1/private.mp4".into();
    unsafe_source.durl.as_mut().unwrap()[0].backup_url.clear();
    for (play, wanted) in [
        (complete_html5_fixture(32), Quality::P1080),
        (complete_html5_fixture(64), Quality::P1080),
        (complete_html5_fixture(80), Quality::P720),
        (PlayUrl::default(), Quality::P1080),
        (incomplete, Quality::P1080),
        (segmented, Quality::P1080),
        (preview, Quality::P1080),
        (no_length, Quality::P1080),
        (unsafe_source, Quality::P1080),
    ] {
        let mut info = VideoInfo::default();
        apply_play_url(&mut info, &play_url_fixture(true), Quality::P1080);
        let before = (info.video_urls.clone(), info.audio_urls.clone());
        assert_eq!(
            supplement_html5(&mut info, &play, wanted, Some(64)),
            Some(64)
        );
        assert!(info.is_dash);
        assert_eq!((info.video_urls, info.audio_urls), before);
        assert_eq!(info.video_quality.as_deref(), Some("720P"));
    }
}

#[test]
fn html5_can_recover_an_unusable_primary_and_anonymous_notice_is_current_response_only() {
    let mut info = VideoInfo::default();
    assert_eq!(
        supplement_html5(&mut info, &complete_html5_fixture(32), Quality::P720, None),
        Some(32)
    );
    set_quality_notice(&mut info, true, true, Quality::P720);
    assert_eq!(
        info.quality_notice.as_deref(),
        Some("未登录，本次可获取480P；可登录后再尝试更高画质")
    );
    set_quality_notice(&mut info, false, false, Quality::P720);
    assert!(info.quality_notice.is_none());
    let _ = supplement_html5(
        &mut info,
        &complete_html5_fixture(64),
        Quality::P1080,
        Some(32),
    );
    set_quality_notice(&mut info, true, true, Quality::P1080);
    assert_eq!(
        info.quality_notice.as_deref(),
        Some("未登录，本次可获取720P；可登录后再尝试更高画质")
    );
    let mut no_stream = VideoInfo::default();
    set_quality_notice(&mut no_stream, true, true, Quality::P720);
    assert!(no_stream.quality_notice.is_none());
}

#[test]
fn configured_login_using_anonymous_recovery_only_reports_the_source_switch() {
    let mut info = VideoInfo::default();
    assert_eq!(
        supplement_html5(&mut info, &complete_html5_fixture(64), Quality::P1080, None),
        Some(64)
    );
    for wanted in [Quality::P720, Quality::P1080] {
        set_quality_notice(&mut info, false, true, wanted);
        assert_eq!(info.video_quality.as_deref(), Some("720P"));
        assert_eq!(info.quality_notice.as_deref(), Some("本次使用匿名播放源"));
        let notice = info.quality_notice.as_deref().unwrap();
        assert!(!notice.contains("未登录"));
        assert!(!notice.contains("可登录"));
    }
    set_quality_notice(&mut info, false, false, Quality::P1080);
    assert!(info.quality_notice.is_none());
}

#[test]
fn progressive_completeness_and_preview_flags_are_decoded_without_assuming_first_segment_is_full() {
    for flag in [serde_json::json!(1), serde_json::json!(true)] {
        let mut raw = serde_json::to_value(complete_html5_fixture(64)).unwrap();
        raw["is_preview"] = flag;
        let play: PlayUrl = serde_json::from_value(raw).unwrap();
        assert!(play.progressive().is_none());
    }
    let mut segmented = complete_html5_fixture(64);
    segmented.durl.as_mut().unwrap().push(DurlStream::default());
    assert!(segmented.progressive().is_none());
}

#[test]
fn video_info_without_quality_fields_remains_compatible_with_saved_payloads() {
    let info: VideoInfo = serde_json::from_value(serde_json::json!({})).unwrap();
    assert!(info.video_quality.is_none());
    assert!(info.quality_notice.is_none());
}

#[test]
fn bangumi_displays_selected_dash_id_and_actual_progressive_quality() {
    use super::mapping::apply_bangumi_play_url;
    use bpi_rs::bangumi::videostream_url::BangumiVideoStreamData;

    let track = |id, url| {
        serde_json::json!({
            "id": id, "base_url": url, "backup_url": [], "bandwidth": 100,
            "mime_type": "video/mp4", "codecs": "avc1", "width": 854, "height": 480,
            "frame_rate": "30", "sar": "1:1", "start_with_sap": 1,
            "segment_base": {"initialization": "0-1", "index_range": "2-3"},
            "codecid": 7, "size": 100, "md5": null
        })
    };
    let mut raw = serde_json::json!({
        "quality": 64, "accept_quality": [64, 32], "accept_format": "flv720,flv480",
        "accept_description": ["720P", "480P"], "format": "flv720", "video_codecid": 7,
        "has_paid": false, "support_formats": [], "code": 0, "fnver": 0,
        "video_project": true, "type": "DASH", "bp": 0, "vip_type": null,
        "vip_status": null, "is_drm": false, "no_rexcode": 0, "record_info": null,
        "dash": {
            "duration": 30, "min_buffer_time": 1.5,
            "video": [track(32, "https://cdn/bangumi-video.m4s")],
            "audio": [track(30280, "https://cdn/bangumi-audio.m4s")],
            "dolby": null, "flac": null
        }
    });
    let play: BangumiVideoStreamData = serde_json::from_value(raw.clone()).unwrap();
    let mut info = VideoInfo::default();
    apply_bangumi_play_url(&mut info, &play, Quality::P720);
    assert!(info.is_dash);
    assert_eq!(info.video_quality.as_deref(), Some("480P"));

    raw["dash"] = serde_json::Value::Null;
    raw["quality"] = serde_json::json!(32);
    raw["durl"] = serde_json::json!([{
        "size": 100, "ahead": "", "length": 30_000, "vhead": "", "backup_url": [],
        "url": "https://cdn/bangumi.mp4", "order": 1
    }]);
    let play: BangumiVideoStreamData = serde_json::from_value(raw).unwrap();
    apply_bangumi_play_url(&mut info, &play, Quality::P720);
    assert!(!info.is_dash);
    assert_eq!(info.video_quality.as_deref(), Some("480P"));
    assert_eq!(info.video_urls, ["https://cdn/bangumi.mp4"]);
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
