//! Public DOM supplements for the already-validated video: count labels,
//! comment pictures, inline emoji and lazily hydrated avatars.

use super::super::engagement::{
    Engagement, MAX_CANDIDATES, bounded, display_count, image_url, numeric_id,
};
use super::{Document, author_url};
use crate::resolver::{CardComment, CardCommentEmote};

pub(super) fn map(document: &Document<'_>, expected_id: &str) -> Engagement {
    let mut output = Engagement::default();
    let Some(root) = document
        .nodes
        .iter()
        .enumerate()
        .find(|(_, node)| {
            node.attr("data-e2e") == Some("detail-video-info")
                && node.attr("data-e2e-aweme-id") == Some(expected_id)
        })
        .and_then(|(index, _)| {
            document.ancestor(index, |n| n.attr("data-e2e") == Some("video-detail"))
        })
    else {
        return output;
    };
    let count = |selector| {
        document
            .descendants(root)
            .find(|&index| document.nodes[index].attr("data-e2e") == Some(selector))
            .and_then(|index| display_count(&document.text(index)))
            .map(|(_, label)| label)
    };
    output.stats.stats_display.likes = count("video-player-digg");
    output.stats.stats_display.comments = count("feed-comment-icon");
    output.stats.stats_display.favorites = count("video-player-collect");
    output.stats.stats_display.shares = count("video-player-share");
    let Some(list) = document
        .descendants(root)
        .find(|&index| document.nodes[index].attr("data-e2e") == Some("comment-list"))
    else {
        return output;
    };
    output.comments = document
        .descendants(list)
        .filter(|&index| {
            document.nodes[index].attr("data-e2e") == Some("comment-item")
                && document
                    .ancestor(index, |node| node.attr("data-e2e") == Some("comment-item"))
                    .is_none()
        })
        .take(MAX_CANDIDATES)
        .filter_map(|index| comment(document, index))
        .collect();
    output
}

fn comment(document: &Document<'_>, root: usize) -> Option<CardComment> {
    let nodes = || document.descendants(root);
    let id = nodes().find_map(|index| {
        numeric_id(document.nodes[index].attr("id")?.strip_prefix("tooltip_")?)
    })?;
    let header =
        nodes().find(|&index| document.nodes[index].has_class("comment-item-info-wrap"))?;
    let (author, profile) = document.descendants(header).find_map(|index| {
        let node = &document.nodes[index];
        if node.tag != "a" {
            return None;
        }
        let profile = author_url(node.attr("href")?)?;
        let name = bounded(&document.text(index), 64);
        (!name.is_empty()).then_some((name, profile))
    })?;
    // The body is the first sibling after the stable info header, excluding
    // the timestamp/action/reply controls which follow it.
    let body = (header + 1..document.nodes.len())
        .find(|&index| document.nodes[index].parent == document.nodes[header].parent)?;
    if !document.within(body, root) {
        return None;
    }
    let text = bounded(&document.rich_text(body), 2048);
    let mut emotes = Vec::new();
    let mut pictures = Vec::new();
    for index in document.descendants(body) {
        let node = &document.nodes[index];
        if node.tag != "img" {
            continue;
        }
        let Some(url) = node.attr("src").and_then(image_url) else {
            continue;
        };
        if let Some(token) = node
            .attr("alt")
            .filter(|token| token.starts_with('[') && token.ends_with(']') && token.len() <= 128)
        {
            if emotes.len() < 8
                && text.contains(token)
                && !emotes
                    .iter()
                    .any(|emote: &CardCommentEmote| emote.text == token)
            {
                emotes.push(CardCommentEmote {
                    text: token.to_owned(),
                    url,
                    large: false,
                });
            }
        } else if pictures.len() < 3 && !pictures.contains(&url) {
            pictures.push(url);
        }
    }
    if text.is_empty() && pictures.is_empty() {
        return None;
    }
    let avatar_url = nodes()
        .filter(|&index| document.nodes[index].has_class("comment-item-avatar"))
        .find_map(|avatar| {
            document.descendants(avatar).find_map(|index| {
                let node = &document.nodes[index];
                if node.tag != "img" {
                    return None;
                }
                let link = document.ancestor(index, |node| node.tag == "a")?;
                if !document.within(link, avatar)
                    || author_url(document.nodes[link].attr("href")?)?.path() != profile.path()
                {
                    return None;
                }
                image_url(node.attr("src")?)
            })
        });
    let (likes, label) = nodes()
        .filter(|&index| {
            document.nodes[index].tag == "p"
                && index > body
                && document
                    .descendants(index)
                    .any(|child| document.nodes[child].tag == "svg")
        })
        .find_map(|index| display_count(&document.text(index)))?;
    Some(CardComment {
        id,
        author,
        author_id: Some(profile.path().trim_start_matches("/user/").to_owned()),
        avatar_url,
        text,
        emotes,
        pictures,
        like_count: likes,
        like_count_display: Some(label),
        published_at: None,
    })
}
