//! Public DOM supplements for the already-validated work: count labels,
//! comment pictures, inline emoji and lazily hydrated avatars.

use super::super::engagement::{
    Engagement, MAX_CANDIDATES, bounded, display_count, image_url, numeric_id,
};
use super::{Document, author_url, note};
use crate::resolver::douyin::client::AwemeRef;
use crate::resolver::{CardComment, CardCommentEmote, CardPublicDetails};

pub(super) fn map(
    document: &Document<'_>,
    expected: &AwemeRef,
    author_profile: &str,
) -> Engagement {
    let mut output = Engagement::default();
    let root = if expected.is_note {
        note::root(document, expected)
    } else {
        document
            .nodes
            .iter()
            .enumerate()
            .find(|(_, node)| {
                node.attr("data-e2e") == Some("detail-video-info")
                    && node.attr("data-e2e-aweme-id") == Some(expected.id.as_str())
            })
            .and_then(|(index, _)| {
                document.ancestor(index, |n| n.attr("data-e2e") == Some("video-detail"))
            })
    };
    let Some(root) = root else {
        return output;
    };
    let in_scope =
        |index| !expected.is_note || note::extras_owned_by(document, index, root, expected);
    let count = |selector| {
        document
            .descendants(root)
            .find(|&index| {
                document.nodes[index].attr("data-e2e") == Some(selector) && in_scope(index)
            })
            .and_then(|index| display_count(&document.text(index)))
            .map(|(_, label)| label)
    };
    output.stats.stats_display.likes = count("video-player-digg");
    output.stats.stats_display.comments = count("feed-comment-icon");
    output.stats.stats_display.favorites = count("video-player-collect");
    output.stats.stats_display.shares = count("video-player-share");
    output.public_details = author_details(document, root, expected, author_profile);
    let Some(list) = document.descendants(root).find(|&index| {
        document.nodes[index].attr("data-e2e") == Some("comment-list") && in_scope(index)
    }) else {
        return output;
    };
    output.public_details.suggested_search = suggested_search(document, root, list, expected);
    output.comments = document
        .descendants(list)
        .filter(|&index| {
            document.nodes[index].attr("data-e2e") == Some("comment-item")
                && in_scope(index)
                && document
                    .ancestor(index, |node| node.attr("data-e2e") == Some("comment-item"))
                    .is_none()
        })
        .take(MAX_CANDIDATES)
        .filter_map(|index| comment(document, index))
        .collect();
    output
}

/// Supplements must belong to the selected detail tree, outside comment rows
/// and nested recommended work/player trees. A note's active sidebar may be
/// present, but only when its work id matches the request.
fn owns_detail(document: &Document<'_>, index: usize, root: usize, expected: &AwemeRef) -> bool {
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
        && document
            .ancestor(index, |node| {
                node.attr("data-e2e") == Some("comment-item")
                    || node.attr("data-e2e") == Some("recommendations")
                    || node.has_class("recommendations")
            })
            .is_none()
}

fn author_details(
    document: &Document<'_>,
    root: usize,
    expected: &AwemeRef,
    author_profile: &str,
) -> CardPublicDetails {
    let mut details = CardPublicDetails::default();
    let Some(profile) = author_url(author_profile) else {
        return details;
    };
    for user in document.descendants(root).filter(|&index| {
        document.nodes[index].attr("data-e2e") == Some("user-info")
            && owns_detail(document, index, root, expected)
    }) {
        for row in document.descendants(user).filter(|&index| {
            document.nodes[index].tag == "p"
                && owns_detail(document, index, root, expected)
                && document.ancestor(index, |node| node.attr("data-e2e") == Some("user-info"))
                    == Some(user)
        }) {
            let Some(parent) = document.nodes[row].parent else {
                continue;
            };
            // Public counts sit beside the name link. Match that link rather
            // than accepting any numeric text elsewhere in the author's box.
            let matches_author = document.descendants(parent).any(|index| {
                let node = &document.nodes[index];
                node.parent == Some(parent)
                    && node.tag == "a"
                    && node
                        .attr("href")
                        .and_then(author_url)
                        .is_some_and(|url| url.path() == profile.path())
            });
            if !matches_author {
                continue;
            }
            let children = document
                .descendants(row)
                .filter(|&index| document.nodes[index].parent == Some(row))
                .collect::<Vec<_>>();
            for pair in children.windows(2) {
                if document.nodes[pair[0]].tag != "span" || document.nodes[pair[1]].tag != "span" {
                    continue;
                }
                let label = document.text(pair[0]);
                let Some((_, value)) = display_count(&document.text(pair[1])) else {
                    continue;
                };
                match label.trim() {
                    "粉丝" => details.author_followers_display = Some(value),
                    "获赞" => details.author_likes_display = Some(value),
                    _ => {}
                }
            }
            // Some rendered snapshots flatten these four spans to one text
            // node. Accept only the complete, exact label/count sequence.
            let text = document.text(row);
            if let Some((followers, likes)) = text
                .trim()
                .strip_prefix("粉丝")
                .and_then(|text| text.split_once("获赞"))
                && let (Some((_, followers)), Some((_, likes))) =
                    (display_count(followers), display_count(likes))
            {
                details.author_followers_display = Some(followers);
                details.author_likes_display = Some(likes);
            }
        }
    }
    details
}

fn suggested_search(
    document: &Document<'_>,
    root: usize,
    list: usize,
    expected: &AwemeRef,
) -> Option<String> {
    let sidebar = document.nodes[list].parent?;
    // The public recommendation header and comment list are siblings inside
    // the same sidebar. Do not search the whole page, player, or author box.
    if sidebar == root || !owns_detail(document, sidebar, root, expected) {
        return None;
    }
    document.descendants(sidebar).find_map(|label| {
        let node = &document.nodes[label];
        if node.tag != "span"
            || !matches!(document.text(label).trim(), "大家都在搜：" | "大家都在搜:")
            || document.within(label, list)
            || !owns_detail(document, label, root, expected)
            || document
                .ancestor(label, |node| {
                    matches!(
                        node.attr("data-e2e"),
                        Some("user-info" | "player-container" | "detail-video-info")
                    )
                })
                .is_some()
        {
            return None;
        }
        let row = node.parent?;
        document.descendants(row).find_map(|index| {
            let node = &document.nodes[index];
            if node.tag != "a" || !owns_detail(document, index, root, expected) {
                return None;
            }
            let href = node.attr("href")?;
            let href = if href.starts_with("//") {
                format!("https:{href}")
            } else {
                href.to_owned()
            };
            let url = url::Url::parse(&href).ok()?;
            if !matches!(url.host_str(), Some("www.douyin.com" | "douyin.com"))
                || !url.path().starts_with("/search/")
                || crate::resolver::url_policy::validate_platform_url(
                    &url,
                    crate::resolver::Platform::Douyin,
                )
                .is_err()
            {
                return None;
            }
            let query = bounded(&document.text(index), 64);
            (!query.is_empty()).then_some(query)
        })
    })
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

#[cfg(test)]
mod tests;
