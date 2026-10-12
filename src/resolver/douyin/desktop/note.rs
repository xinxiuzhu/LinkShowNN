//! Anonymous note pages expose their gallery in the main swiper, separately
//! from the author/caption and the recommendation/comment sidebar.

use anyhow::{Context, Result, ensure};
use serde_json::json;

use super::{
    Document, canonical_url, matching_author, matching_avatar, media_url, parse_publish_time,
    progressive_video_url, split_caption, validate_work_text,
};
use crate::resolver::{MediaInfo, Platform, douyin::client::AwemeRef};

// Preserve all observed slides for downstream rendering budgets. An oversized
// gallery is an error rather than silently claiming a partial set is complete.
const MAX_SLIDES: usize = 200;

pub(super) fn root(document: &Document<'_>, expected: &AwemeRef) -> Option<usize> {
    document.nodes.iter().enumerate().find_map(|(index, node)| {
        (node.attr("data-e2e") == Some("feed-active-video")
            && node.attr("data-e2e-vid") == Some(expected.id.as_str()))
        .then(|| document.ancestor(index, |node| node.attr("data-e2e") == Some("note-detail")))
        .flatten()
    })
}

/// Reject nested recommendation players and other work detail trees even if
/// they happen to use the same swiper class as the selected work.
pub(super) fn owned_by(document: &Document<'_>, index: usize, root: usize) -> bool {
    document.ancestor(index, |node| {
        matches!(
            node.attr("data-e2e"),
            Some("note-detail" | "video-detail" | "feed-active-video")
        )
    }) == Some(root)
}

pub(super) fn extras_owned_by(
    document: &Document<'_>,
    index: usize,
    root: usize,
    expected: &AwemeRef,
) -> bool {
    // Comments live in the selected note's active sidebar, while reactions
    // live beside the main player. Nested work details or a different active
    // sidebar cannot fill absent fields for this work.
    document.ancestor(index, |node| {
        matches!(node.attr("data-e2e"), Some("note-detail" | "video-detail"))
    }) == Some(root)
        && document
            .ancestor(index, |node| {
                node.attr("data-e2e") == Some("feed-active-video")
            })
            .is_none_or(|feed| {
                document.nodes[feed].attr("data-e2e-vid") == Some(expected.id.as_str())
            })
}

pub(super) fn map(html: &str, document: &Document<'_>, expected: &AwemeRef) -> Result<MediaInfo> {
    let canonical = canonical_url(document, expected)?;
    let root =
        root(document, expected).context("douyin desktop page has no matching note content")?;
    let author = matching_author(html, document, expected)?;
    let (avatar, user) = matching_avatar(document, root, &author)?;
    let (caption, published_at) = caption(document, root, user)?;
    validate_work_text(&caption)?;
    let player = document
        .descendants(root)
        .find(|&index| {
            document.nodes[index].attr("data-e2e") == Some("player-container")
                && owned_by(document, index, root)
        })
        .context("douyin note has no main gallery player")?;
    let slides = document
        .descendants(player)
        .filter(|&index| {
            document.nodes[index].has_class("dySwiperSlide")
                && owned_by(document, index, root)
                && document.ancestor(index, |node| {
                    node.attr("data-e2e") == Some("player-container")
                }) == Some(player)
                && document
                    .ancestor(index, |node| node.has_class("dySwiperSlide"))
                    .is_none()
        })
        .collect::<Vec<_>>();
    ensure!(
        !slides.is_empty() && slides.len() <= MAX_SLIDES,
        "douyin note gallery has no slides or exceeds the slide limit"
    );
    let mut images = Vec::new();
    let mut video_slides = Vec::new();
    let mut video_slide_covers = Vec::new();
    let mut video_slides_without_image = 0;
    for slide in &slides {
        let has_video = document.descendants(*slide).any(|index| {
            document.nodes[index].tag == "video"
                && owned_by(document, index, root)
                && document.ancestor(index, |node| node.has_class("dySwiperSlide")) == Some(*slide)
        });
        // Each slide has both a blurred background and its foreground copy.
        // Keep their first valid static URL once, in the original slide order.
        let image = document
            .descendants(*slide)
            .filter(|&index| {
                document.nodes[index].tag == "img"
                    && document.ancestor(index, |node| node.has_class("dySwiperSlide"))
                        == Some(*slide)
                    && owned_by(document, index, root)
            })
            .find_map(|index| {
                media_url(document.nodes[index].attr("src")?)
                    .ok()
                    .filter(|url| url != &avatar)
            });
        if has_video {
            video_slide_covers.push(image.clone());
        }
        if let Some(image) = image {
            // Collapse the copies inside one slide, preserving repeated slides.
            images.push(image);
        } else if has_video {
            video_slides_without_image += 1;
        } else {
            anyhow::bail!("douyin note slide has no valid static image");
        }
        if has_video {
            video_slides.push(*slide);
        }
    }
    // The existing downloader sends one file per work. A gallery with exactly
    // one video slide can use it; several clips retain their static gallery,
    // without sending an arbitrary first clip as the complete work.
    let video_url = match video_slides.as_slice() {
        [slide] => clip_url(document, *slide, root, &expected.id),
        _ => None,
    };
    ensure!(
        !images.is_empty()
            && (video_slides_without_image == 0
                || (video_slides_without_image == 1 && video_url.is_some())),
        "douyin note gallery has no cover or an unavailable clip without a static image"
    );
    // A playable clip's thumbnail belongs to its own slide. The gallery's
    // first static picture is only a presentation fallback when that precise
    // clip preview is absent; callers must not turn it into a video thumbnail.
    let video_clip_cover_url = video_url
        .as_ref()
        .and_then(|_| video_slide_covers.first().cloned().flatten());
    let cover_url = video_clip_cover_url
        .clone()
        .or_else(|| images.first().cloned());
    let sec_uid = author
        .profile
        .path()
        .trim_start_matches("/user/")
        .to_owned();
    let (title, description) = split_caption(&caption);
    Ok(MediaInfo {
        title,
        description,
        cover_url,
        video_url,
        images,
        author: Some(author.name),
        avatar_url: Some(avatar),
        published_at,
        is_video: false,
        source_url: canonical.to_string(),
        platform: Platform::Douyin,
        raw: json!({
            "platform": "douyin",
            "metadata_source": "public_desktop_note",
            "aweme_id": expected.id,
            "author_sec_uid": sec_uid,
            "author_url": author.profile.as_str(),
            "gallery_slide_count": slides.len(),
            "video_clip_count": video_slides.len(),
            "video_clip_cover_url": video_clip_cover_url,
        }),
        ..Default::default()
    })
}

fn caption(document: &Document<'_>, root: usize, user: usize) -> Result<(String, Option<i64>)> {
    // The note caption is the next sibling of the validated author header.
    // Its line-clamped text excludes the following expand button/publication
    // label; neither the sidebar nor a recommendation can supply missing text.
    let panel = (user + 1..document.nodes.len())
        .find(|&index| document.nodes[index].parent == document.nodes[user].parent)
        .filter(|&index| document.within(index, root) && owned_by(document, index, root))
        .context("douyin note has no author-adjacent caption")?;
    let text = document
        .descendants(panel)
        .find(|&index| {
            document.nodes[index]
                .attr("style")
                .is_some_and(|style| style.contains("--lineClamp:"))
        })
        .map(|index| document.rich_text(index))
        .filter(|text| !text.trim().is_empty())
        .context("douyin note has no work text")?;
    let published_at = document
        .descendants(panel)
        .filter(|&index| document.nodes[index].tag == "span")
        .find_map(|index| parse_publish_time(&document.text(index)));
    Ok((text.trim().to_owned(), published_at))
}

fn clip_url(document: &Document<'_>, slide: usize, root: usize, work_id: &str) -> Option<String> {
    document.descendants(slide).find_map(|index| {
        let node = &document.nodes[index];
        let video = match node.tag.as_str() {
            "video" => index,
            "source" => document.ancestor(index, |node| node.tag == "video")?,
            _ => return None,
        };
        if !owned_by(document, index, root)
            || document.ancestor(video, |node| node.has_class("dySwiperSlide")) != Some(slide)
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

#[cfg(test)]
mod tests;
