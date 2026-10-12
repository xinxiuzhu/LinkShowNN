//! Read only the public metadata belonging to the requested desktop work.
//!
//! A title alone is not evidence that Douyin returned a work: anonymous error
//! pages also have titles. Require the canonical URL, the matching work DOM,
//! a static cover, and the author breadcrumb/avatar to agree before succeeding.

use anyhow::{Context, Result, ensure};
use chrono::{FixedOffset, NaiveDateTime, TimeZone};
use serde_json::{Value, json};
use url::Url;

use super::client::{AwemeRef, parse_aweme_ref, split_caption};
use crate::resolver::{MediaInfo, Platform, url_policy};

mod dom;
mod extras;
mod note;

use dom::Document;

#[cfg(test)]
use dom::decode_entities;

#[cfg(test)]
mod tests;

pub(crate) fn map_engagement(
    html: &str,
    expected: &AwemeRef,
    source: &Url,
) -> Result<super::engagement::Engagement> {
    // Optional rows are only trusted after the same work identity checks as
    // the main metadata. A recommendation or login page cannot supply them.
    let media = map_desktop_page(html, expected, source)?;
    let document = Document::parse(html)?;
    let author_profile = media.raw["author_url"]
        .as_str()
        .context("douyin work has no validated author profile")?;
    Ok(extras::map(&document, expected, author_profile))
}

pub(crate) fn map_desktop_page(
    html: &str,
    expected: &AwemeRef,
    source_url: &Url,
) -> Result<MediaInfo> {
    url_policy::validate_platform_url(source_url, Platform::Douyin)?;
    ensure!(
        parse_aweme_ref(source_url).as_ref() == Some(expected),
        "douyin source does not match requested work"
    );
    let document = Document::parse(html)?;
    if expected.is_note {
        return note::map(html, &document, expected);
    }
    let canonical = canonical_url(&document, expected)?;
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
    validate_work_text(&title)?;
    let cover = document
        .meta("lark:url:video_cover_image_url")
        .context("douyin video has no static work cover")?;
    let cover = media_url(cover)?;
    let author = matching_author(html, &document, expected)?;
    let (avatar, _) = matching_avatar(&document, root, &author)?;
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

fn canonical_url(document: &Document<'_>, expected: &AwemeRef) -> Result<Url> {
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
    canonical.context("douyin desktop page has no work canonical")
}

fn validate_work_text(title: &str) -> Result<()> {
    ensure!(
        !title.is_empty()
            && !matches!(title, "抖音" | "安全验证" | "验证码" | "抱歉出错了")
            && !title.starts_with("在抖音记录美好生活"),
        "douyin returned a generic page instead of work text"
    );
    Ok(())
}

fn matching_author(html: &str, document: &Document<'_>, expected: &AwemeRef) -> Result<Author> {
    document
        .nodes
        .iter()
        .filter(|node| node.tag == "script" && node.attr("type") == Some("application/ld+json"))
        .filter_map(|node| {
            serde_json::from_str::<Value>(&html[node.inner_start..node.inner_end]).ok()
        })
        .find_map(|value| breadcrumb_author(&value, expected))
        .context("douyin work has no matching author breadcrumb")
}

fn matching_avatar(
    document: &Document<'_>,
    root: usize,
    author: &Author,
) -> Result<(String, usize)> {
    document
        .descendants(root)
        .filter(|&index| {
            document.nodes[index].attr("data-e2e") == Some("user-info")
                && (document.nodes[root].attr("data-e2e") != Some("note-detail")
                    || note::owned_by(document, index, root))
        })
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
                media_url(node.attr("src")?).ok().map(|url| (url, user))
            })
        })
        .context("douyin work has no matching author avatar")
}

/// Read the selected work's progressive MP4 source, not a recommendation,
/// comment attachment, blob URL or adaptive/audio-only stream.
/// The existing bounded downloader fetches the selected URL.
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
