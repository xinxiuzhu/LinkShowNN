use super::*;

const VIDEO_ID: &str = "7693165612066999571";
const NOTE_ID: &str = "7695618096421580538";
const VIDEO_HTML: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/douyin-engagement.html"
));
const NOTE_HTML: &str = include_str!("../note/fixture.html");

fn details(html: &str, note: bool) -> CardPublicDetails {
    let document = Document::parse(html).unwrap();
    map(
        &document,
        &AwemeRef {
            id: if note { NOTE_ID } else { VIDEO_ID }.into(),
            is_note: note,
        },
        if note {
            "https://www.douyin.com/user/public-note-author"
        } else {
            "https://www.douyin.com/user/public-author-sec-uid"
        },
    )
    .public_details
}

#[test]
fn both_recorded_public_page_shapes_preserve_author_count_labels() {
    let video = details(VIDEO_HTML, false);
    assert_eq!(video.author_followers_display.as_deref(), Some("261.3万"));
    assert_eq!(video.author_likes_display.as_deref(), Some("7571.0万"));
    let note = details(NOTE_HTML, true);
    assert_eq!(note.author_followers_display.as_deref(), Some("58"));
    assert_eq!(note.author_likes_display.as_deref(), Some("2359"));
    assert!(video.suggested_search.is_none());
    assert!(note.suggested_search.is_none());
}

#[test]
fn absent_or_unknown_author_counts_remain_absent_and_literal_zero_is_valid() {
    let unknown = VIDEO_HTML
        .replace("<span>261.3万</span>", "<span>--</span>")
        .replace("<span>7571.0万</span>", "<span>获赞</span>");
    assert_eq!(details(&unknown, false), CardPublicDetails::default());
    let zero = VIDEO_HTML
        .replace("<span>261.3万</span>", "<span>0</span>")
        .replace("<span>7571.0万</span>", "<span>0</span>");
    let zero = details(&zero, false);
    assert_eq!(zero.author_followers_display.as_deref(), Some("0"));
    assert_eq!(zero.author_likes_display.as_deref(), Some("0"));
}

fn search_row(query: &str) -> String {
    format!(
        "<div><span>大家都在搜：</span><span><a href=\"//www.douyin.com/search/public-query\">{query}</a></span></div>"
    )
}

fn scoped_page(note: bool, author: &str, sidebar: &str, extras: &str) -> String {
    let root = if note { "note-detail" } else { "video-detail" };
    let work = if note {
        format!("<div data-e2e=\"feed-active-video\" data-e2e-vid=\"{NOTE_ID}\"></div>")
    } else {
        format!("<div data-e2e=\"detail-video-info\" data-e2e-aweme-id=\"{VIDEO_ID}\"></div>")
    };
    format!("<main data-e2e=\"{root}\">{work}{author}{sidebar}{extras}</main>")
}

fn author_box(profile: &str, counters: &str) -> String {
    format!(
        "<div data-e2e=\"user-info\"><div><a href=\"//www.douyin.com/user/{profile}\">作者</a><p>{counters}</p></div></div>"
    )
}

#[test]
fn search_is_read_only_from_matching_comment_sidebar_for_video_and_note() {
    for note in [false, true] {
        let query = if note {
            "动态背景图获取方式"
        } else {
            "鸣潮莉莉配装推荐"
        };
        let sidebar = format!(
            "<div><div>{}</div><div data-e2e=\"comment-list\"></div></div>",
            search_row(query)
        );
        assert_eq!(
            details(&scoped_page(note, "", &sidebar, ""), note)
                .suggested_search
                .as_deref(),
            Some(query)
        );
    }
    let long = "搜".repeat(100);
    let sidebar = format!(
        "<div>{}<div data-e2e=\"comment-list\"></div></div>",
        search_row(&long)
    );
    let query = details(&scoped_page(false, "", &sidebar, ""), false)
        .suggested_search
        .unwrap();
    assert_eq!(query.chars().count(), 64);
}

#[test]
fn other_authors_recommendations_and_comment_rows_cannot_supply_public_details() {
    let counters = "<span>粉丝</span><span>999万</span><span>获赞</span><span>888万</span>";
    let other_author = author_box("different-author", counters);
    let nested_recommendation = format!(
        "<div class=\"recommendations\"><div data-e2e=\"video-detail\">{}{}</div></div>",
        author_box("public-author-sec-uid", counters),
        search_row("推荐作品")
    );
    let sidebar = format!(
        "<div><div data-e2e=\"comment-list\"><div data-e2e=\"comment-item\">{}{}</div></div></div>",
        author_box("public-author-sec-uid", counters),
        search_row("评论正文")
    );
    let html = scoped_page(false, &other_author, &sidebar, &nested_recommendation);
    assert_eq!(details(&html, false), CardPublicDetails::default());
    // An unrelated name/count block inside the selected author's box cannot
    // borrow the real author's profile from a different sibling section.
    let inner = format!(
        "<div data-e2e=\"user-info\"><a href=\"//www.douyin.com/user/public-author-sec-uid\">作者</a><div><a href=\"//www.douyin.com/user/different-author\">别人</a><p>{counters}</p></div></div>"
    );
    assert_eq!(
        details(&scoped_page(false, &inner, "", ""), false),
        CardPublicDetails::default()
    );
}

#[test]
fn search_rejects_page_text_wrong_sidebar_and_non_douyin_links() {
    let outside = scoped_page(
        false,
        "",
        "<div><div data-e2e=\"comment-list\"></div></div>",
        &search_row("页面搜索"),
    );
    assert!(details(&outside, false).suggested_search.is_none());
    let other_feed = format!(
        "<div data-e2e=\"feed-active-video\" data-e2e-vid=\"different\"><div>{}<div data-e2e=\"comment-list\"></div></div></div>",
        search_row("其他作品")
    );
    assert!(
        details(&scoped_page(true, "", &other_feed, ""), true)
            .suggested_search
            .is_none()
    );
    for invalid in [
        "https://example.test/search/query",
        "https://www.douyin.com/user/someone",
        "https://user:password@www.douyin.com/search/query",
    ] {
        let row = search_row("搜索词").replace("//www.douyin.com/search/public-query", invalid);
        let sidebar = format!("<div>{row}<div data-e2e=\"comment-list\"></div></div>");
        assert!(
            details(&scoped_page(false, "", &sidebar, ""), false)
                .suggested_search
                .is_none(),
            "{invalid}"
        );
    }
}
