//! Read only the public metadata belonging to the requested desktop video.
//!
//! A title alone is not evidence that Douyin returned a work: anonymous error
//! pages also have titles. Require the canonical URL, the matching video DOM,
//! a static cover, and the author breadcrumb/avatar to agree before succeeding.

use anyhow::{Context, Result, ensure};
use chrono::{FixedOffset, NaiveDateTime, TimeZone};
use serde_json::{Value, json};
use url::Url;

use super::client::{AwemeRef, parse_aweme_ref, split_caption};
use crate::resolver::{MediaInfo, Platform, url_policy};

const MAX_HTML_BYTES: usize = 8 * 1024 * 1024;
const MAX_NODES: usize = 100_000;
const MAX_DEPTH: usize = 128;

#[path = "desktop_engagement.rs"]
mod extras;

pub(crate) fn map_engagement(
    html: &str,
    expected: &AwemeRef,
    source: &Url,
) -> Result<super::engagement::Engagement> {
    // Optional rows are only trusted after the same work identity checks as
    // the main metadata. A recommendation or login page cannot supply them.
    map_desktop_page(html, expected, source)?;
    let document = Document::parse(html)?;
    Ok(extras::map(&document, &expected.id))
}

pub(crate) fn map_desktop_page(
    html: &str,
    expected: &AwemeRef,
    source_url: &Url,
) -> Result<MediaInfo> {
    ensure!(
        !expected.is_note,
        "desktop metadata does not contain a complete image gallery"
    );
    url_policy::validate_platform_url(source_url, Platform::Douyin)?;
    ensure!(
        parse_aweme_ref(source_url).as_ref() == Some(expected),
        "douyin source does not match requested work"
    );
    let document = Document::parse(html)?;
    let mut canonical = None;
    for node in &document.nodes {
        if node.tag == "link"
            && node.attr("rel").is_some_and(|rel| {
                rel.split_ascii_whitespace()
                    .any(|part| part.eq_ignore_ascii_case("canonical"))
            })
        {
            let url = Url::parse(node.attr("href").context("canonical has no URL")?)?;
            url_policy::validate_platform_url(&url, Platform::Douyin)?;
            ensure!(
                parse_aweme_ref(&url).as_ref() == Some(expected),
                "douyin canonical points to a different work"
            );
            canonical = Some(url);
        }
    }
    let canonical = canonical.context("douyin desktop page has no work canonical")?;
    let (detail, root) = document
        .nodes
        .iter()
        .enumerate()
        .filter(|(_, node)| node.attr("data-e2e") == Some("detail-video-info"))
        .find_map(|(index, node)| {
            (node.attr("data-e2e-aweme-id") == Some(expected.id.as_str()))
                .then(|| {
                    document
                        .ancestor(index, |node| node.attr("data-e2e") == Some("video-detail"))
                        .map(|root| (index, root))
                })
                .flatten()
        })
        .context("douyin desktop page has no matching video content")?;
    let heading = document
        .descendants(detail)
        .find(|&index| document.nodes[index].tag == "h1")
        .map(|index| document.text(index))
        .filter(|text| !text.trim().is_empty())
        .context("douyin video has no work text")?;
    let title = match document.meta("lark:url:video_title") {
        Some(value) => {
            let value = value.trim().strip_suffix(" - 抖音").unwrap_or(value.trim());
            ensure!(
                compact(value) == compact(&heading),
                "douyin work text disagrees with page metadata"
            );
            value.to_owned()
        }
        None => heading.trim().to_owned(),
    };
    ensure!(
        !title.is_empty()
            && !matches!(
                title.as_str(),
                "抖音" | "安全验证" | "验证码" | "抱歉出错了"
            )
            && !title.starts_with("在抖音记录美好生活"),
        "douyin returned a generic page instead of work text"
    );
    let cover = document
        .meta("lark:url:video_cover_image_url")
        .context("douyin video has no static work cover")?;
    let cover = media_url(cover)?;
    let author = document
        .nodes
        .iter()
        .filter(|node| node.tag == "script" && node.attr("type") == Some("application/ld+json"))
        .filter_map(|node| {
            serde_json::from_str::<Value>(&html[node.inner_start..node.inner_end]).ok()
        })
        .find_map(|value| breadcrumb_author(&value, expected))
        .context("douyin video has no matching author breadcrumb")?;
    let avatar = document
        .descendants(root)
        .filter(|&index| document.nodes[index].attr("data-e2e") == Some("user-info"))
        .find_map(|user| {
            document.descendants(user).find_map(|index| {
                let node = &document.nodes[index];
                if node.tag != "img"
                    || node.attr("alt").map(str::trim) != Some(author.name.as_str())
                {
                    return None;
                }
                let avatar = document.ancestor(index, |ancestor| {
                    ancestor.attr("data-e2e") == Some("live-avatar")
                })?;
                if !document.within(avatar, user) {
                    return None;
                }
                let link = document.ancestor(index, |ancestor| ancestor.tag == "a")?;
                if !document.within(link, user) {
                    return None;
                }
                let href = document.nodes[link].attr("href")?;
                let profile = author_url(href)?;
                if profile.path() != author.profile.path() {
                    return None;
                }
                media_url(node.attr("src")?).ok()
            })
        })
        .context("douyin video has no matching author avatar")?;
    ensure!(cover != avatar, "douyin cover cannot be the author avatar");
    let published_at = document
        .descendants(detail)
        .find(|&index| document.nodes[index].attr("data-e2e") == Some("detail-video-publish-time"))
        .and_then(|index| parse_publish_time(&document.text(index)));
    let sec_uid = author
        .profile
        .path()
        .trim_start_matches("/user/")
        .to_owned();
    let (title, description) = split_caption(&title);
    Ok(MediaInfo {
        title,
        description,
        cover_url: Some(cover),
        video_url: player_video_url(&document, root, &expected.id),
        author: Some(author.name),
        avatar_url: Some(avatar),
        published_at,
        is_video: true,
        source_url: canonical.to_string(),
        platform: Platform::Douyin,
        raw: json!({
            "platform": "douyin",
            "metadata_source": "public_desktop_page",
            "aweme_id": expected.id,
            "author_sec_uid": sec_uid,
            "author_url": author.profile.as_str(),
        }),
        ..Default::default()
    })
}

/// Read the selected work's progressive MP4 source, not a recommendation,
/// comment attachment, blob URL or adaptive/audio-only stream. The browser
/// still blocks playback; the existing bounded downloader fetches this URL.
fn player_video_url(document: &Document<'_>, root: usize, work_id: &str) -> Option<String> {
    document.descendants(root).find_map(|index| {
        let node = &document.nodes[index];
        let video = match node.tag.as_str() {
            "video" => index,
            "source" => document.ancestor(index, |node| node.tag == "video")?,
            _ => return None,
        };
        let player = document.ancestor(video, |node| {
            node.attr("data-e2e") == Some("player-container")
        })?;
        if document.ancestor(player, |node| node.attr("data-e2e") == Some("video-detail"))
            != Some(root)
            || node.attr("type").is_some_and(|mime| {
                let mime = mime.split(';').next().unwrap_or_default().trim();
                !mime.is_empty() && mime != "video/mp4"
            })
        {
            return None;
        }
        progressive_video_url(node.attr("src")?, work_id)
    })
}

fn progressive_video_url(raw: &str, work_id: &str) -> Option<String> {
    let raw = raw.trim();
    if raw.len() > 8192 {
        return None;
    }
    let url = Url::parse(raw).ok()?;
    url_policy::validate_public_url(&url).ok()?;
    let host = url.host_str()?;
    if url.scheme() != "https"
        || url.port_or_known_default() != Some(443)
        || !["douyinvod.com", "douyinvod.net"]
            .iter()
            .any(|domain| host == *domain || host.ends_with(&format!(".{domain}")))
        || !(url.path().contains("/video/") || url.path().ends_with(".mp4"))
        || [".m3u8", ".mpd", ".m4s", ".mp3", ".m4a"]
            .iter()
            .any(|extension| url.path().to_ascii_lowercase().ends_with(extension))
    {
        return None;
    }
    let mut ids = url.query_pairs().filter(|(key, _)| key == "__vid");
    if ids.next().is_some_and(|(_, id)| id != work_id) || ids.next().is_some() {
        return None;
    }
    if url
        .query_pairs()
        .any(|(key, value)| key == "mime_type" && value != "video_mp4")
    {
        return None;
    }
    // Keep the CDN's query/signature intact. Do not synthesize a play API URL.
    Some(raw.to_owned())
}

fn compact(text: &str) -> String {
    text.chars()
        .filter(|character| !character.is_whitespace())
        .collect()
}

fn media_url(value: &str) -> Result<String> {
    let url = Url::parse(value.trim())?;
    url_policy::validate_public_url(&url)?;
    Ok(url.to_string())
}

struct Author {
    name: String,
    profile: Url,
}

fn author_url(value: &str) -> Option<Url> {
    let url = if value.starts_with("//") {
        Url::parse(&format!("https:{value}"))
    } else {
        Url::parse(value)
    }
    .ok()?;
    url_policy::validate_platform_url(&url, Platform::Douyin).ok()?;
    if !matches!(url.host_str(), Some("www.douyin.com" | "douyin.com")) {
        return None;
    }
    let id = url.path().strip_prefix("/user/")?;
    if id.is_empty()
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
    {
        return None;
    }
    Some(url)
}

fn breadcrumb_author(value: &Value, expected: &AwemeRef) -> Option<Author> {
    if let Some(values) = value.as_array().or_else(|| value.get("@graph")?.as_array()) {
        return values
            .iter()
            .find_map(|value| breadcrumb_author(value, expected));
    }
    if value.get("@type")?.as_str()? != "BreadcrumbList" {
        return None;
    }
    let items = value.get("itemListElement")?.as_array()?;
    let has_work = items.iter().any(|item| {
        item.get("item")
            .and_then(Value::as_str)
            .and_then(|value| Url::parse(value).ok())
            .filter(|url| url_policy::validate_platform_url(url, Platform::Douyin).is_ok())
            .and_then(|url| parse_aweme_ref(&url))
            .as_ref()
            == Some(expected)
    });
    if !has_work {
        return None;
    }
    items.iter().find_map(|item| {
        let profile = author_url(item.get("item")?.as_str()?)?;
        let name = item.get("name")?.as_str()?.trim();
        (!name.is_empty()).then(|| Author {
            name: name.to_owned(),
            profile,
        })
    })
}

fn parse_publish_time(text: &str) -> Option<i64> {
    let time = text.trim().strip_prefix("发布时间：")?.trim();
    let time = NaiveDateTime::parse_from_str(time, "%Y-%m-%d %H:%M")
        .or_else(|_| NaiveDateTime::parse_from_str(time, "%Y-%m-%d %H:%M:%S"))
        .ok()?;
    Some(
        FixedOffset::east_opt(8 * 3600)?
            .from_local_datetime(&time)
            .single()?
            .timestamp(),
    )
}

// An intentionally small, bounded index of actual HTML elements. Keeping parent
// relationships avoids selecting a recommendation/comment image as the author;
// skipping raw script/style bodies also excludes HTML-looking JavaScript text.
struct Element {
    tag: String,
    attrs: Vec<(String, String)>,
    parent: Option<usize>,
    inner_start: usize,
    inner_end: usize,
}

impl Element {
    fn has_class(&self, name: &str) -> bool {
        self.attr("class")
            .is_some_and(|classes| classes.split_ascii_whitespace().any(|class| class == name))
    }

    fn attr(&self, name: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }
}

struct Document<'a> {
    nodes: Vec<Element>,
    text: Vec<(usize, usize, &'a str)>,
}

impl<'a> Document<'a> {
    fn parse(html: &'a str) -> Result<Self> {
        ensure!(
            html.len() <= MAX_HTML_BYTES,
            "douyin HTML exceeds metadata limit"
        );
        let mut document = Self {
            nodes: Vec::new(),
            text: Vec::new(),
        };
        let mut stack: Vec<usize> = Vec::new();
        let mut cursor = 0;
        while cursor < html.len() {
            let next = html[cursor..]
                .find('<')
                .map(|offset| cursor + offset)
                .unwrap_or(html.len());
            if next > cursor
                && let Some(&parent) = stack.last()
            {
                document.text.push((parent, cursor, &html[cursor..next]));
            }
            if next == html.len() {
                break;
            }
            if html[next..].starts_with("<!--") {
                cursor = html[next + 4..]
                    .find("-->")
                    .map(|offset| next + 4 + offset + 3)
                    .context("unterminated HTML comment")?;
                continue;
            }
            let end = tag_end(html, next).context("unterminated HTML tag")?;
            cursor = end + 1;
            let raw = html[next + 1..end].trim();
            if raw.starts_with(['!', '?']) {
                continue;
            }
            let closing = raw.starts_with('/');
            let raw = raw.trim_start_matches('/').trim_start();
            let name_end = raw
                .bytes()
                .position(|byte| {
                    !byte.is_ascii_alphanumeric() && !matches!(byte, b'-' | b':' | b'_')
                })
                .unwrap_or(raw.len());
            if name_end == 0 {
                continue;
            }
            let tag = raw[..name_end].to_ascii_lowercase();
            if closing {
                if let Some(position) = stack
                    .iter()
                    .rposition(|&index| document.nodes[index].tag == tag)
                {
                    for index in stack.drain(position..) {
                        document.nodes[index].inner_end = next;
                    }
                }
                continue;
            }
            ensure!(
                document.nodes.len() < MAX_NODES && stack.len() < MAX_DEPTH,
                "douyin HTML structure exceeds metadata limit"
            );
            let index = document.nodes.len();
            document.nodes.push(Element {
                tag: tag.clone(),
                attrs: attributes(&raw[name_end..]),
                parent: stack.last().copied(),
                inner_start: cursor,
                inner_end: cursor,
            });
            if matches!(tag.as_str(), "script" | "style" | "textarea" | "title") {
                // The HTML raw-text terminator is case insensitive. Do not run
                // or recursively parse anything between its opening/closing tags.
                let terminator = format!("</{tag}");
                let stop = html[cursor..]
                    .as_bytes()
                    .windows(terminator.len())
                    .position(|bytes| bytes.eq_ignore_ascii_case(terminator.as_bytes()))
                    .map(|offset| cursor + offset)
                    .context("unterminated HTML raw text")?;
                document.nodes[index].inner_end = stop;
                cursor = tag_end(html, stop).context("unterminated raw text closing tag")? + 1;
            } else if matches!(
                tag.as_str(),
                "area"
                    | "base"
                    | "br"
                    | "col"
                    | "embed"
                    | "hr"
                    | "img"
                    | "input"
                    | "link"
                    | "meta"
                    | "param"
                    | "source"
                    | "track"
                    | "wbr"
            ) || raw.ends_with('/')
            {
                if tag == "br" {
                    document.text.push((index, cursor, "\n"));
                }
            } else {
                stack.push(index);
            }
        }
        for index in stack {
            document.nodes[index].inner_end = html.len();
        }
        Ok(document)
    }

    fn within(&self, index: usize, parent: usize) -> bool {
        let mut current = Some(index);
        while let Some(index) = current {
            if index == parent {
                return true;
            }
            current = self.nodes[index].parent;
        }
        false
    }

    fn ancestor(&self, index: usize, predicate: impl Fn(&Element) -> bool) -> Option<usize> {
        let mut current = self.nodes[index].parent;
        while let Some(index) = current {
            if predicate(&self.nodes[index]) {
                return Some(index);
            }
            current = self.nodes[index].parent;
        }
        None
    }

    fn descendants(&self, parent: usize) -> impl Iterator<Item = usize> + '_ {
        (parent + 1..self.nodes.len()).filter(move |&index| self.within(index, parent))
    }

    fn text(&self, parent: usize) -> String {
        let text = self
            .text
            .iter()
            .filter(|(index, _, _)| self.within(*index, parent))
            .map(|(_, _, text)| *text)
            .collect::<String>();
        decode_entities(&text)
    }

    fn rich_text(&self, parent: usize) -> String {
        let mut pieces = self
            .text
            .iter()
            .filter(|(index, _, _)| self.within(*index, parent))
            .map(|(_, offset, text)| (*offset, decode_entities(text)))
            .collect::<Vec<_>>();
        for index in self.descendants(parent) {
            let node = &self.nodes[index];
            if node.tag == "img"
                && let Some(alt) = node.attr("alt")
            {
                pieces.push((node.inner_start, alt.to_owned()));
            }
        }
        pieces.sort_by_key(|(offset, _)| *offset);
        pieces.into_iter().map(|(_, text)| text).collect()
    }

    fn meta(&self, name: &str) -> Option<&str> {
        self.nodes
            .iter()
            .find(|node| node.tag == "meta" && node.attr("name") == Some(name))?
            .attr("content")
    }
}

fn tag_end(html: &str, start: usize) -> Option<usize> {
    let mut quote = None;
    for (offset, byte) in html.as_bytes()[start + 1..].iter().copied().enumerate() {
        match (quote, byte) {
            (Some(delimiter), byte) if delimiter == byte => quote = None,
            (None, b'\'' | b'"') => quote = Some(byte),
            (None, b'>') => return Some(start + 1 + offset),
            _ => {}
        }
    }
    None
}

fn attributes(raw: &str) -> Vec<(String, String)> {
    let mut result = Vec::new();
    let mut rest = raw.trim_start();
    while !rest.is_empty() && rest != "/" {
        let name_end = rest
            .find(|character: char| {
                character.is_ascii_whitespace() || matches!(character, '=' | '/' | '>')
            })
            .unwrap_or(rest.len());
        if name_end == 0 {
            break;
        }
        let name = rest[..name_end].to_ascii_lowercase();
        rest = rest[name_end..].trim_start();
        let value = if let Some(after) = rest.strip_prefix('=') {
            rest = after.trim_start();
            if let Some(quote) = rest
                .chars()
                .next()
                .filter(|character| matches!(character, '\'' | '"'))
            {
                rest = &rest[1..];
                let end = rest.find(quote).unwrap_or(rest.len());
                let value = &rest[..end];
                rest = rest.get(end + 1..).unwrap_or("").trim_start();
                value
            } else {
                let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
                let value = &rest[..end];
                rest = rest[end..].trim_start();
                value
            }
        } else {
            ""
        };
        if !result.iter().any(|(existing, _)| existing == &name) {
            result.push((name, decode_entities(value)));
        }
    }
    result
}

/// Decode once, so `&amp;lt;` remains literal `&lt;` instead of becoming markup.
fn decode_entities(raw: &str) -> String {
    let mut result = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(start) = rest.find('&') {
        result.push_str(&rest[..start]);
        rest = &rest[start..];
        let Some(end) = rest.find(';').filter(|&end| end <= 32) else {
            result.push('&');
            rest = &rest[1..];
            continue;
        };
        let entity = &rest[1..end];
        let decoded = match entity {
            "amp" => Some('&'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "nbsp" => Some('\u{a0}'),
            _ => entity
                .strip_prefix("#x")
                .or_else(|| entity.strip_prefix("#X"))
                .and_then(|number| u32::from_str_radix(number, 16).ok())
                .or_else(|| {
                    entity
                        .strip_prefix('#')
                        .and_then(|number| number.parse::<u32>().ok())
                })
                .and_then(char::from_u32),
        };
        if let Some(character) = decoded {
            result.push(character);
            rest = &rest[end + 1..];
        } else {
            result.push('&');
            rest = &rest[1..];
        }
    }
    result.push_str(rest);
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resolver::content::ResolvedContent;

    const FIXTURE: &str = include_str!("../../../../tests/fixtures/douyin-desktop.html");
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
            Some(
                "https://p3-pc.douyinpic.com/aweme/100x100/public-author-avatar.jpeg?from=327834062"
            )
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
        let player =
            format!(r#"<div data-e2e="player-container"><video src="{good}"></video></div>"#);
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
            parse(&FIXTURE.replace("data-e2e=\"detail-video-info\"", "data-e2e=\"error\""))
                .is_err()
        );
    }

    #[test]
    fn rejects_wrong_canonical_or_work_dom_and_notes() {
        let canonical = format!("https://www.douyin.com/video/{ID}");
        assert!(
            parse(&FIXTURE.replacen(&canonical, "https://www.douyin.com/video/123", 1)).is_err()
        );
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
            let replaced = FIXTURE.replace("https://p3-pc-sign.douyinpic.com/public-work-cover.jpeg?from=327834062&amp;sc=cover", url);
            assert!(parse(&replaced).is_err(), "accepted {url}");
            let replaced = FIXTURE.replace("https://p3-pc.douyinpic.com/aweme/100x100/public-author-avatar.jpeg?from=327834062", url);
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
}
