use super::*;
use crate::resolver::{ResolvedContent, douyin::desktop};
use url::Url;

const FIXTURE: &str = include_str!("fixture.html");
const ID: &str = "7695618096421580538";

fn expected() -> AwemeRef {
    AwemeRef {
        id: ID.to_owned(),
        is_note: true,
    }
}

fn source() -> Url {
    Url::parse(&format!(
        "https://www.iesdouyin.com/share/slides/{ID}/?is_slides=1&contains_video_type_clip=1"
    ))
    .unwrap()
}

fn parse(html: &str) -> Result<MediaInfo> {
    desktop::map_desktop_page(html, &expected(), &source())
}

#[test]
fn maps_anonymous_note_text_author_gallery_and_unique_video_slide() {
    let media = parse(FIXTURE).unwrap();
    assert_eq!(
        media.title.as_deref(),
        Some("我真是太吃这种风格的背景图了 #动态壁纸 #背景图")
    );
    assert_eq!(media.author.as_deref(), Some("公开作者"));
    assert_eq!(
        media.avatar_url.as_deref(),
        Some("https://p3-pc.douyinpic.com/aweme/100x100/note-author.jpeg")
    );
    assert_eq!(media.published_at, Some(1_791_775_716));
    assert_eq!(
        media.source_url,
        format!("https://www.douyin.com/note/{ID}")
    );
    assert_eq!(
        media.images,
        [
            "https://p3-pc-sign.douyinpic.com/note-first.webp?sign=a%2Fb&from=327834062",
            "https://p3-pc-sign.douyinpic.com/note-second.webp",
            "https://p3-pc-sign.douyinpic.com/note-third.webp",
        ]
    );
    assert_eq!(media.cover_url, media.images.get(1).cloned());
    assert!(!media.is_video);
    assert_eq!(media.raw["gallery_slide_count"], 3);
    assert_eq!(media.raw["video_clip_count"], 1);
    assert_eq!(
        media.raw["video_clip_cover_url"],
        "https://p3-pc-sign.douyinpic.com/note-second.webp"
    );
    assert_eq!(
        media.video_url.as_deref(),
        Some(
            "https://v95-web-sz.douyinvod.com/video/public-note-clip/?sign=a%2Fb&mime_type=video_mp4"
        )
    );
    assert_eq!(
        media.download_candidate(0).unwrap().url.as_str(),
        media.video_url.as_deref().unwrap()
    );
    let text = media.card().text();
    assert!(!text.contains("展开"));
    assert!(!text.contains("推荐作品"));
}

#[test]
fn notes_expose_same_work_statistics_comments_emotes_and_pictures() {
    let engagement = desktop::map_engagement(FIXTURE, &expected(), &source()).unwrap();
    assert_eq!(engagement.stats.stats_display.likes.as_deref(), Some("51"));
    assert_eq!(
        engagement.stats.stats_display.comments.as_deref(),
        Some("26")
    );
    assert_eq!(
        engagement.stats.stats_display.favorites.as_deref(),
        Some("13")
    );
    assert_eq!(engagement.stats.stats_display.shares.as_deref(), Some("21"));
    let top = engagement.top_comments();
    assert_eq!(top.len(), 1);
    assert_eq!(top[0].text, "好看[微笑]");
    assert_eq!(top[0].like_count, 9);
    assert_eq!(
        top[0].pictures,
        ["https://p3.douyinpic.com/comment-picture.jpeg"]
    );
    assert_eq!(top[0].emotes.len(), 1);
    assert_eq!(
        top[0].avatar_url.as_deref(),
        Some("https://p3.douyinpic.com/comment-avatar.jpeg")
    );
}

#[test]
fn rejects_wrong_canonical_work_marker_author_breadcrumb_or_avatar() {
    for html in [
        FIXTURE.replacen(
            &format!("https://www.douyin.com/note/{ID}"),
            "https://www.douyin.com/note/123",
            1,
        ),
        FIXTURE.replace(&format!("data-e2e-vid=\"{ID}\""), "data-e2e-vid=\"123\""),
        FIXTURE.replace("\"name\":\"公开作者\"", "\"name\":\"其他作者\""),
        FIXTURE.replace("alt=\"公开作者\"", "alt=\"其他作者\""),
        FIXTURE.replace(
            "href=\"//www.douyin.com/user/public-note-author\"",
            "href=\"//www.douyin.com/user/other-author\"",
        ),
        FIXTURE.replace(
            "<script type=\"application/ld+json\">",
            "<script type=\"application/json\">",
        ),
    ] {
        assert!(parse(&html).is_err());
        assert!(desktop::map_engagement(&html, &expected(), &source()).is_err());
    }
}

#[test]
fn soundtrack_and_other_players_never_supply_the_gallery_video() {
    let start = FIXTURE.find("<div><video>").unwrap();
    let end = start + FIXTURE[start..].find("</video></div>").unwrap() + "</video></div>".len();
    let without_clip = format!("{}{}", &FIXTURE[..start], &FIXTURE[end..]);
    let media = parse(&without_clip).unwrap();
    assert_eq!(media.images.len(), 3);
    assert!(media.video_url.is_none());
    let audio_clip = without_clip.replace("</audio>", "</audio><video src=\"https://v11-weba.douyinvod.com/video/soundtrack.mp3?mime_type=audio_mp4\"></video>");
    assert!(parse(&audio_clip).unwrap().video_url.is_none());
    let two_clips = FIXTURE.replace("<div class=\"dySwiperSlide\">\n <div><img src=\"https://p3-pc-sign.douyinpic.com/note-third.webp\">", "<div class=\"dySwiperSlide\"><video src=\"https://v11-weba.douyinvod.com/video/second.mp4\"></video>\n <div><img src=\"https://p3-pc-sign.douyinpic.com/note-third.webp\">");
    let media = parse(&two_clips).unwrap();
    assert_eq!(media.raw["video_clip_count"], 2);
    assert!(media.video_url.is_none());
    assert_eq!(media.images.len(), 3);
}

#[test]
fn a_unique_playable_video_slide_can_omit_its_static_preview() {
    let html = FIXTURE.replace(
        "<div><img src=\"https://p3-pc-sign.douyinpic.com/note-second.webp\"><div><img src=\"https://p3-pc-sign.douyinpic.com/note-second.webp\"></div></div>",
        "",
    );
    let media = parse(&html).unwrap();
    assert_eq!(media.images.len(), 2);
    assert!(media.video_url.is_some());
    assert_eq!(media.cover_url, media.images.first().cloned());
    assert!(media.raw["video_clip_cover_url"].is_null());
    let no_stream = html.replace("douyinvod.com", "cdn.example");
    assert!(parse(&no_stream).is_err());
}

#[test]
fn separate_slides_keep_repeated_images_in_their_original_positions() {
    let html = FIXTURE.replace(
        "note-third.webp",
        "note-first.webp?sign=a%2Fb&amp;from=327834062",
    );
    let media = parse(&html).unwrap();
    assert_eq!(media.images.len(), 3);
    assert_eq!(media.images[0], media.images[2]);
    assert_ne!(media.images[0], media.images[1]);
}

#[test]
fn missing_gallery_or_caption_cannot_be_filled_by_sidebar_content() {
    assert!(parse(&FIXTURE.replace("class=\"dySwiperSlide\"", "class=\"other-images\"")).is_err());
    assert!(parse(&FIXTURE.replace("--lineClamp:4", "--otherStyle:4")).is_err());
    assert!(
        parse(&FIXTURE.replace("data-e2e=\"player-container\"", "data-e2e=\"other-player\""))
            .is_err()
    );
}

#[test]
fn nested_other_work_cannot_fill_missing_statistics_or_comment_lists() {
    let html = FIXTURE.replace(
        "<div data-e2e=\"video-player-digg\">51</div>",
        "<div data-e2e=\"video-detail\"><div data-e2e=\"video-player-digg\">999</div><div data-e2e=\"comment-list\"><div data-e2e=\"comment-item\">其他作品评论</div></div></div>",
    );
    let engagement = desktop::map_engagement(&html, &expected(), &source()).unwrap();
    assert!(engagement.stats.stats_display.likes.is_none());
    assert_eq!(engagement.top_comments().len(), 1);
    assert_eq!(engagement.top_comments()[0].author, "评论者");
}

#[test]
fn oversized_gallery_is_rejected_without_silently_truncating_images() {
    let slide =
        "<div class=\"dySwiperSlide\"><img src=\"https://p3.douyinpic.com/extra.webp\"></div>";
    let oversized = FIXTURE.replace(
        "<div class=\"swiper-track\">",
        &format!("<div class=\"swiper-track\">{}", slide.repeat(MAX_SLIDES)),
    );
    assert!(parse(&oversized).is_err());
}
