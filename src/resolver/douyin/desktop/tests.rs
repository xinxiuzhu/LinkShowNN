use super::*;
use crate::resolver::content::ResolvedContent;

const FIXTURE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/douyin-desktop.html"
));
const ID: &str = "7693165612066999571";

fn parse(html: &str) -> Result<MediaInfo> {
    map_desktop_page(
        html,
        &AwemeRef {
            id: ID.into(),
            is_note: false,
        },
        &Url::parse(&format!("https://www.iesdouyin.com/share/video/{ID}/")).unwrap(),
    )
}

#[test]
fn maps_real_public_page_without_statistics_or_session_data() {
    let media = parse(FIXTURE).unwrap();
    assert_eq!(
        media.title.as_deref(),
        Some("共鸣者档案前瞻 | 莉莉 “这个味道，嗯……希望这次，是个好梦。”")
    );
    assert_eq!(
        media.description.as_deref(),
        Some("共鸣属性：衍射\n武器类型：佩枪\n#鸣潮 #鸣潮莉莉")
    );
    assert!(media.card().text().contains("共鸣者档案前瞻 | 莉莉 “这个味道，嗯……希望这次，是个好梦。”\n共鸣属性：衍射\n武器类型：佩枪\n#鸣潮 #鸣潮莉莉"));
    assert_eq!(media.author.as_deref(), Some("鸣潮"));
    assert_eq!(
        media.cover_url.as_deref(),
        Some("https://p3-pc-sign.douyinpic.com/public-work-cover.jpeg?from=327834062&sc=cover")
    );
    assert_eq!(
        media.avatar_url.as_deref(),
        Some("https://p3-pc.douyinpic.com/aweme/100x100/public-author-avatar.jpeg?from=327834062")
    );
    assert_eq!(media.published_at, Some(1_791_428_400));
    assert!(media.is_video);
    assert!(media.video_url.is_none());
    assert!(media.images.is_empty());
    assert_eq!(media.raw["aweme_id"], ID);
    assert_eq!(media.raw["author_sec_uid"], "public-author-sec-uid");
    assert!(media.raw.get("statistics").is_none());
    assert!(!media.raw.to_string().contains("7571"));
}

#[test]
fn public_player_source_becomes_a_download_candidate_with_intact_query() {
    let source = format!(
        "https://v26-web.douyinvod.com/video/public.mp4?sign=a%2Fb&mime_type=video_mp4&__vid={ID}"
    );
    let player = format!(
        r#"<div data-e2e="player-container"><video src="blob:https://www.douyin.com/blob"><source src="{}"></video></div>"#,
        source.replace('&', "&amp;")
    );
    let html = FIXTURE.replace(
        "<div data-e2e=\"video-detail\">",
        &format!("<div data-e2e=\"video-detail\">{player}"),
    );
    let media = parse(&html).unwrap();
    let candidate = media.download_candidate(480).unwrap();
    assert_eq!(candidate.url, source);
    assert_eq!(candidate.referer, "https://www.douyin.com/");
    assert!(candidate.audio_url.is_none());
    assert_eq!(candidate.cover_url, media.cover_url);
}

#[test]
fn ignores_recommendation_players_attachments_and_non_progressive_streams() {
    let good = format!("https://v26-web.douyinvod.com/video/public.mp4?__vid={ID}");
    let player = format!(r#"<div data-e2e="player-container"><video src="{good}"></video></div>"#);
    for extra in [
        // Outside the work, nested recommendations, and comment media.
        player.clone(),
        format!(r#"<div data-e2e="video-detail">{player}</div>"#),
        format!(r#"<video src="{good}"></video>"#),
    ] {
        let html = if extra == player {
            format!("{FIXTURE}{extra}")
        } else {
            FIXTURE.replace(
                "<div data-e2e=\"video-detail\">",
                &format!("<div data-e2e=\"video-detail\">{extra}"),
            )
        };
        assert!(parse(&html).unwrap().download_candidate(480).is_none());
    }
    for url in [
        good.replace(ID, "123"),
        format!("{good}&__vid={ID}"),
        good.replace("v26-web.douyinvod.com", "douyinvod.com.example.org"),
        good.replace("v26-web.douyinvod.com", "127.0.0.1"),
        good.replace("https://", "https://user:password@"),
        good.replace("public.mp4", "audio.mp3"),
        good.replace("public.mp4", "video.m4s"),
        good.replace("public.mp4", "playlist.m3u8"),
        format!("{good}&mime_type=audio_mp4"),
        "blob:https://www.douyin.com/opaque".into(),
    ] {
        assert!(progressive_video_url(&url, ID).is_none(), "accepted {url}");
    }
    let html = FIXTURE.replace(
        "<div data-e2e=\"video-detail\">",
        &format!(r#"<div data-e2e="video-detail"><div data-e2e="player-container"><video><source src="{good}" type="audio/mp4"></video></div>"#),
    );
    assert!(parse(&html).unwrap().video_url.is_none());
}

#[test]
fn rejects_error_login_and_captcha_shells_even_with_generic_meta() {
    for html in [
        "<html><title>在抖音记录美好生活20261012 - 抖音</title></html>",
        "<html><title>安全验证</title><div>请完成验证码</div></html>",
        "<html><meta name='lark:url:video_title' content='抱歉出错了'><meta name='lark:url:video_cover_image_url' content='https://cdn.example/icon.png'></html>",
    ] {
        assert!(parse(html).is_err());
    }
    assert!(
        parse(&FIXTURE.replace("data-e2e=\"detail-video-info\"", "data-e2e=\"error\"")).is_err()
    );
}

#[test]
fn rejects_wrong_canonical_or_work_dom_and_notes() {
    let canonical = format!("https://www.douyin.com/video/{ID}");
    assert!(parse(&FIXTURE.replacen(&canonical, "https://www.douyin.com/video/123", 1)).is_err());
    assert!(
        parse(&FIXTURE.replace(
            &format!("data-e2e-aweme-id=\"{ID}\""),
            "data-e2e-aweme-id=\"123\""
        ))
        .is_err()
    );
    assert!(
        map_desktop_page(
            FIXTURE,
            &AwemeRef {
                id: ID.into(),
                is_note: true
            },
            &Url::parse(&format!("https://www.douyin.com/note/{ID}")).unwrap()
        )
        .is_err()
    );
}

#[test]
fn requires_the_cover_and_avatar_in_their_own_roles() {
    assert!(parse(&FIXTURE.replace("lark:url:video_cover_image_url", "og:image")).is_err());
    let other = FIXTURE.replace("data-e2e=\"user-info\"", "data-e2e=\"comment-item\"");
    assert!(parse(&other).is_err());
    let other = FIXTURE.replace(
        "data-e2e=\"live-avatar\"",
        "data-e2e=\"recommendation-cover\"",
    );
    assert!(parse(&other).is_err());
    let other = FIXTURE.replace("alt=\"鸣潮\"", "alt=\"别的作者\"");
    assert!(parse(&other).is_err());
}

#[test]
fn rejects_unsafe_cover_and_author_asset_urls() {
    for url in [
        "javascript:alert(1)",
        "file:///etc/passwd",
        "https://user:password@cdn.example/a.png",
        "http://127.0.0.1/a.png",
    ] {
        let replaced = FIXTURE.replace(
            "https://p3-pc-sign.douyinpic.com/public-work-cover.jpeg?from=327834062&amp;sc=cover",
            url,
        );
        assert!(parse(&replaced).is_err(), "accepted {url}");
        let replaced = FIXTURE.replace(
            "https://p3-pc.douyinpic.com/aweme/100x100/public-author-avatar.jpeg?from=327834062",
            url,
        );
        assert!(parse(&replaced).is_err(), "accepted avatar {url}");
    }
}

#[test]
fn decodes_html_entities_once_and_preserves_line_breaks() {
    assert_eq!(
        decode_entities("&quot;A&amp;B&#10;&#x1F600;&lt;tag&gt;&amp;lt;&apos;"),
        "\"A&B\n😀<tag>&lt;'"
    );
    let document = Document::parse("<meta content='A &quot;quote&quot;&#10;&lt;x&gt;' name='test'><h1>甲<span>乙</span><br>丙 &amp; 丁</h1>").unwrap();
    assert_eq!(document.meta("test"), Some("A \"quote\"\n<x>"));
    let h1 = document
        .nodes
        .iter()
        .position(|node| node.tag == "h1")
        .unwrap();
    assert_eq!(document.text(h1), "甲乙\n丙 & 丁");
}

#[test]
fn ignores_html_looking_script_comments_and_rejects_stale_text() {
    let html = r#"<!-- <link rel="canonical" href="https://www.douyin.com/video/7693165612066999571"> -->
        <script>const fake = '<meta name="lark:url:video_title" content="作品"><div data-e2e="video-detail"><h1>作品</h1></div>';</script>"#;
    let document = Document::parse(html).unwrap();
    assert!(document.meta("lark:url:video_title").is_none());
    assert!(!document.nodes.iter().any(|node| node.tag == "link"));
    assert!(parse(html).is_err());
    let html = FIXTURE.replacen("共鸣者档案前瞻", "不同作品", 1);
    // First occurrence is the generic description, which is never mapped.
    assert!(parse(&html).is_ok());
    let html = FIXTURE.replace(
        "<meta name=\"lark:url:video_title\" content=\"共鸣者档案前瞻",
        "<meta name=\"lark:url:video_title\" content=\"不同作品",
    );
    assert!(parse(&html).is_err());
}
