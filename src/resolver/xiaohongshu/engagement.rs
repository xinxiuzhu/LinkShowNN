//! 小红书公开状态中的作品统计与根评论。只读取已校验笔记的明确字段。

use std::{collections::BTreeMap, sync::OnceLock};

use serde_json::Value;

use crate::resolver::{CardComment, CardCommentEmote, CardPoster, url_policy};

const MAX_CANDIDATES: usize = 50;
const MAX_EMOTES: usize = 8;
const MAX_PICTURES: usize = 3;

#[derive(Debug, Clone, Default)]
pub(crate) struct Engagement {
    pub stats: CardPoster,
    pub comments: Vec<CardComment>,
}

impl Engagement {
    #[cfg(test)]
    pub fn from_state(state: &Value, expected_id: &str) -> Self {
        Self::from_state_with_comments(state, expected_id, true)
    }

    pub fn from_state_with_comments(
        state: &Value,
        expected_id: &str,
        comments_enabled: bool,
    ) -> Self {
        let Ok(note) = super::state::requested_note(state, expected_id) else {
            return Self::default();
        };
        let mut result = Self::from_note(note);
        if !comments_enabled {
            return result;
        }
        // PC 状态把评论放在指定笔记的 map entry 中；移动端把它们放在
        // noteData 的同级 commentData 中，必须再次确认同级笔记身份。
        if let Some(entry) = state
            .pointer("/note/noteDetailMap")
            .and_then(|map| map.get(expected_id))
        {
            result.add_comments(entry.pointer("/comments/list"), expected_id);
        }
        if state
            .pointer("/noteData/data/noteData")
            .is_some_and(|note| bound_note(note, expected_id))
        {
            result.add_comments(
                state.pointer("/noteData/data/commentData/comments"),
                expected_id,
            );
        }
        result
    }

    fn from_note(note: &Value) -> Self {
        let info = &note["interactInfo"];
        let mut stats = CardPoster::default();
        (stats.like_count, stats.stats_display.likes) = statistic(&info["likedCount"]);
        (stats.comment_count, stats.stats_display.comments) = statistic(&info["commentCount"]);
        (stats.favorite_count, stats.stats_display.favorites) = statistic(&info["collectedCount"]);
        (stats.share_count, stats.stats_display.shares) = statistic(&info["shareCount"]);
        Self {
            stats,
            comments: Vec::new(),
        }
    }

    fn add_comments(&mut self, values: Option<&Value>, expected_id: &str) {
        for value in values
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .take(MAX_CANDIDATES)
        {
            if let Some(comment) = map_comment(value, expected_id) {
                self.add_comment(comment);
            }
        }
    }

    fn add_comment(&mut self, mut comment: CardComment) {
        if let Some(old) = self.comments.iter_mut().find(|old| old.id == comment.id) {
            // 同一根评论可从 PC/H5 两份状态取得。文字与作者保持首次读取，
            // 合并缺失素材，并使用较高的已知赞数进行候选排序。
            old.avatar_url = old.avatar_url.take().or(comment.avatar_url.take());
            old.published_at = old.published_at.or(comment.published_at);
            if comment.like_count > old.like_count {
                old.like_count = comment.like_count;
                old.like_count_display = comment.like_count_display;
            }
            for emote in comment.emotes {
                if old.emotes.len() < MAX_EMOTES
                    && !old.emotes.iter().any(|old| old.text == emote.text)
                {
                    old.emotes.push(emote);
                }
            }
            for picture in comment.pictures {
                if old.pictures.len() < MAX_PICTURES && !old.pictures.contains(&picture) {
                    old.pictures.push(picture);
                }
            }
        } else if self.comments.len() < MAX_CANDIDATES {
            self.comments.push(comment);
        } else if let Some(lowest) = self.comments.iter().map(|comment| comment.like_count).min()
            && comment.like_count > lowest
        {
            // 固定候选预算内仍保留 PC/H5 合并后的最高赞评论。预算已满
            // 不能直接忽略新高赞；优先移除同为最低赞的最后一条，保留
            // 较早候选的相同赞数顺序，再把新来源的评论追加到队尾。
            let index = self
                .comments
                .iter()
                .rposition(|comment| comment.like_count == lowest)
                .expect("the minimum candidate count must exist");
            self.comments.remove(index);
            self.comments.push(comment);
        }
    }

    pub fn merge(&mut self, other: Self) {
        // 保留先取得的 PC 精确统计。仅有缩略标签时，可用移动端精确数
        // 提升；移动端未知值不能清掉已取得的标签。
        merge_statistic(
            &mut self.stats.like_count,
            &mut self.stats.stats_display.likes,
            other.stats.like_count,
            other.stats.stats_display.likes,
        );
        merge_statistic(
            &mut self.stats.comment_count,
            &mut self.stats.stats_display.comments,
            other.stats.comment_count,
            other.stats.stats_display.comments,
        );
        merge_statistic(
            &mut self.stats.favorite_count,
            &mut self.stats.stats_display.favorites,
            other.stats.favorite_count,
            other.stats.stats_display.favorites,
        );
        merge_statistic(
            &mut self.stats.share_count,
            &mut self.stats.stats_display.shares,
            other.stats.share_count,
            other.stats.stats_display.shares,
        );
        for comment in other.comments {
            self.add_comment(comment);
        }
    }

    pub fn top_comments(&self) -> Vec<CardComment> {
        let mut comments = self.comments.clone();
        comments.retain(|comment| !comment.text.is_empty() || !comment.pictures.is_empty());
        // stable sort 在赞数相同的情况下保留公开状态给出的原始顺序。
        comments.sort_by(|left, right| right.like_count.cmp(&left.like_count));
        comments.truncate(3);
        comments
    }
}

fn merge_statistic(
    exact: &mut Option<u64>,
    display: &mut Option<String>,
    other_exact: Option<u64>,
    other_display: Option<String>,
) {
    if exact.is_none() && (other_exact.is_some() || display.is_none()) {
        *exact = other_exact;
        *display = other_display;
    }
}

fn bound_note(note: &Value, expected_id: &str) -> bool {
    let mut has_id = false;
    for key in ["noteId", "note_id", "id"] {
        if let Some(value) = note.get(key).filter(|value| !value.is_null()) {
            if value.as_str().and_then(canonical_id).as_deref() != Some(expected_id) {
                return false;
            }
            has_id = true;
        }
    }
    has_id
}

fn map_comment(value: &Value, expected_id: &str) -> Option<CardComment> {
    if value["noteId"].as_str().and_then(canonical_id).as_deref() != Some(expected_id)
        || value
            .get("status")
            .is_some_and(|status| status.as_u64() != Some(0))
        || value.get("targetComment").is_some_and(|target| {
            !target.is_null() && target.as_object().is_none_or(|target| !target.is_empty())
        })
    {
        return None;
    }
    let id = canonical_id(value["id"].as_str()?)?;
    let user = &value["user"];
    let author = bounded(user["nickname"].as_str()?, 64);
    if author.is_empty() {
        return None;
    }
    let text = value["content"]
        .as_str()
        .map(|text| bounded(text, 2048))
        .unwrap_or_default();
    // 移动端 SSR 的 likeCount 可固定为 0，而 likeViewCount 保留公开显示值。
    // 有效的显示值优先；显示值缺失时才使用明确的非负整数。非空但无法
    // 解析的显示值不能退回 H5 占位 0，避免把隐藏/未知赞数当作已知零赞。
    let displayed = &value["likeViewCount"];
    let (like_count, like_count_display) = count(displayed);
    let (like_count, like_count_display) = match like_count {
        Some(count) => (count, like_count_display),
        None if displayed.is_null()
            || displayed
                .as_str()
                .is_some_and(|label| label.trim().is_empty()) =>
        {
            (count(&value["likeCount"]).0?, None)
        }
        None => return None,
    };
    let avatar_url = user["image"].as_str().and_then(image_url);
    let author_id = user["userId"].as_str().and_then(canonical_id);
    let published_at = value["time"]
        .as_i64()
        .filter(|time| *time > 0)
        .map(|time| time / 1000);
    // 官方 H5 评论组件的 ImageEmbed 接收 data.pictures，并读取每项 url。
    // 不遍历子评论、封面或任意 JSON 图片键来补评论附图。
    let mut pictures = Vec::new();
    for url in value["pictures"]
        .as_array()
        .into_iter()
        .flatten()
        .take(12)
        .filter_map(|picture| picture["url"].as_str().and_then(image_url))
    {
        if !pictures.contains(&url) {
            pictures.push(url);
            if pictures.len() == MAX_PICTURES {
                break;
            }
        }
    }
    let emotes = emotes(&text);
    Some(CardComment {
        id,
        author,
        author_id,
        avatar_url,
        text,
        like_count,
        like_count_display,
        published_at,
        emotes,
        pictures,
    })
}

fn emotes(text: &str) -> Vec<CardCommentEmote> {
    // 只收录官方 NoteText 组件的 token -> 图片元数据；不复制组件源码。
    // Source (2026-10-12): https://fe-static.xhscdn.com/formula-static/ranchi/public/js/1937.8eb6e91.chunk.js
    static EMOTES: OnceLock<BTreeMap<String, String>> = OnceLock::new();
    let known = EMOTES.get_or_init(|| {
        serde_json::from_str(include_str!("emotes.json"))
            .expect("bundled Xiaohongshu emote metadata must be valid JSON")
    });
    let mut emotes = Vec::new();
    // 按正文顺序映射完整方括号标记；未知或失效的素材保留原文字。
    let mut remaining = text;
    while let Some(start) = remaining.find('[') {
        remaining = &remaining[start..];
        let Some(end) = remaining.find(']') else {
            break;
        };
        let token = &remaining[..=end];
        if let Some(url) = known.get(token).and_then(|url| image_url(url))
            && !emotes
                .iter()
                .any(|emote: &CardCommentEmote| emote.text == token)
        {
            emotes.push(CardCommentEmote {
                text: token.to_owned(),
                url,
                large: false,
            });
            if emotes.len() == MAX_EMOTES {
                break;
            }
        }
        remaining = &remaining[end + 1..];
    }
    emotes
}

fn canonical_id(value: &str) -> Option<String> {
    (value.len() == 24 && value.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .then(|| value.to_ascii_lowercase())
}

fn bounded(value: &str, max: usize) -> String {
    value.trim().chars().take(max).collect()
}

pub(super) fn image_url(value: &str) -> Option<String> {
    let value = value.trim();
    let absolute = if value.starts_with("//") {
        format!("https:{value}")
    } else {
        value.to_owned()
    };
    let url = url::Url::parse(&absolute).ok()?;
    url_policy::validate_public_url(&url).ok()?;
    // 保留签名 URL 的原始 query 字节，不进行 CDN 路径或质量参数猜测。
    Some(absolute)
}

/// 精确计数保留整数；万/亿级显示值仅用于排序估值并保留原标签。
fn count(value: &Value) -> (Option<u64>, Option<String>) {
    if let Some(count) = value.as_u64() {
        return (Some(count), None);
    }
    let Some(label) = value.as_str().map(str::trim) else {
        return (None, None);
    };
    let parsed = parse_count(label);
    match parsed {
        Some((number, abbreviated)) => (Some(number), abbreviated.then(|| label.to_owned())),
        None => (None, None),
    }
}

/// CardPoster 的数字字段只容纳精确值；缩略值由 stats_display 显示。
/// 评论则继续使用 count 的估值排序，因此不要合并这两个语义。
fn statistic(value: &Value) -> (Option<u64>, Option<String>) {
    let (number, display) = count(value);
    if display.is_some() {
        (None, display)
    } else {
        (number, None)
    }
}

fn parse_count(label: &str) -> Option<(u64, bool)> {
    if label.is_empty() || label.len() > 32 {
        return None;
    }
    // 官方 H5 formatter 超过十万时返回“10万+”，保留下限标签而非精确数。
    let bounded_label = label.strip_suffix('+').unwrap_or(label);
    let (number, scale) = if let Some(number) = bounded_label.strip_suffix('万') {
        (number, 10_000_u64)
    } else if let Some(number) = bounded_label.strip_suffix('亿') {
        (number, 100_000_000_u64)
    } else {
        if bounded_label != label {
            return None;
        }
        (bounded_label, 1)
    };
    let (whole, fraction) = number.split_once('.').unwrap_or((number, ""));
    if whole.is_empty()
        || (number.contains('.') && fraction.is_empty())
        || !fraction.bytes().all(|byte| byte.is_ascii_digit())
        || fraction.len() > 4
        || (scale == 1 && !fraction.is_empty())
    {
        return None;
    }
    let groups = whole.split(',').collect::<Vec<_>>();
    if groups
        .iter()
        .any(|group| group.is_empty() || !group.bytes().all(|byte| byte.is_ascii_digit()))
        || (groups.len() > 1
            && (groups[0].len() > 3 || groups[1..].iter().any(|group| group.len() != 3)))
    {
        return None;
    }
    let mut number = groups.join("").parse::<u64>().ok()?.checked_mul(scale)?;
    if !fraction.is_empty() {
        number = number.checked_add(
            fraction.parse::<u64>().ok()?.checked_mul(scale)? / 10_u64.pow(fraction.len() as u32),
        )?;
    }
    Some((number, scale != 1))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const ID: &str = "6ac6146a000000001b021019";
    const OTHER_ID: &str = "6ac6146a000000001b021020";

    fn comment(index: u32, likes: &str) -> Value {
        json!({
            "id": format!("6ac7403e00000000{index:08x}"), "noteId": ID,
            "content": "公开评论[皱眉R]", "likeCount": 0, "likeViewCount": likes,
            "status": 0, "time": 1791443006123_i64, "commentType": 0,
            "user": {"userId": "5bad2c9f2d833c0001ea7f50", "nickname": "评论作者",
                "image": "https://sns-avatar.example/avatar.jpg?imageView2/2/w/120"},
            "subComments": [{"id": "6ac7403e000000000000ffff", "noteId": ID,
                "content": "楼中楼不参与排名", "likeViewCount": "9999", "status": 0,
                "user": {"nickname": "回复作者"}}]
        })
    }

    fn mobile(comments: Vec<Value>) -> Value {
        json!({"noteData": {"data": {
            "noteData": {"noteId": ID, "interactInfo": {
                "likedCount": "1,728", "collectedCount": "208",
                "commentCount": "44", "shareCount": "211"
            }}, "commentData": {"comments": comments, "commentCount": 44}
        }}})
    }

    #[test]
    fn real_mobile_fields_keep_stats_and_rank_displayed_likes_over_zero_raw_likes() {
        let engagement = Engagement::from_state(
            &mobile(vec![
                comment(1, "1"),
                comment(2, "8"),
                comment(3, "1"),
                comment(4, "1"),
            ]),
            ID,
        );
        assert_eq!(engagement.stats.like_count, Some(1728));
        assert_eq!(engagement.stats.favorite_count, Some(208));
        assert_eq!(engagement.stats.comment_count, Some(44));
        assert_eq!(engagement.stats.share_count, Some(211));
        let comments = engagement.top_comments();
        assert_eq!(comments.len(), 3);
        assert_eq!(
            comments
                .iter()
                .map(|comment| comment.like_count)
                .collect::<Vec<_>>(),
            [8, 1, 1]
        );
        assert_eq!(comments[1].id, comment(1, "1")["id"].as_str().unwrap());
        assert_eq!(comments[0].author, "评论作者");
        assert_eq!(
            comments[0].author_id.as_deref(),
            Some("5bad2c9f2d833c0001ea7f50")
        );
        assert!(
            comments[0]
                .avatar_url
                .as_ref()
                .unwrap()
                .contains("?imageView2/2/w/120")
        );
        assert_eq!(comments[0].published_at, Some(1_791_443_006));
    }

    #[test]
    fn explicit_pc_comment_entry_is_bound_to_requested_note_not_recommendations() {
        let state = json!({"note": {"firstNoteId": ID, "noteDetailMap": {
            ID: {"note": {"noteId": ID, "interactInfo": {"likedCount": "2.3万"}},
                "comments": {"list": [comment(1, "32")] }},
            OTHER_ID: {"note": {"noteId": OTHER_ID}, "comments": {"list": [comment(2, "9999")]}}
        }}, "recommendations": {"comments": [comment(3, "8888")]}});
        let engagement = Engagement::from_state(&state, ID);
        assert_eq!(engagement.stats.like_count, None);
        assert_eq!(
            engagement.stats.stats_display.likes.as_deref(),
            Some("2.3万")
        );
        assert_eq!(engagement.top_comments().len(), 1);
        assert_eq!(engagement.top_comments()[0].like_count, 32);
        assert!(Engagement::from_state(&state, OTHER_ID).comments.is_empty());
    }

    #[test]
    fn conflicting_mobile_note_cannot_supply_comment_sibling_to_a_valid_pc_note() {
        let mut state = mobile(vec![comment(1, "1")]);
        state["noteData"]["data"]["noteData"]["noteId"] = json!(OTHER_ID);
        state["note"] = json!({"noteId": ID});
        assert!(Engagement::from_state(&state, ID).comments.is_empty());
    }

    #[test]
    fn comments_skip_wrong_note_replies_deleted_rows_and_invalid_counts() {
        let base = comment(1, "1");
        let mut invalids = Vec::new();
        for (key, value) in [
            ("noteId", json!(OTHER_ID)),
            ("id", json!("bad")),
            ("status", json!(1)),
            ("targetComment", json!({"id": "parent"})),
            ("likeViewCount", json!("unknown")),
        ] {
            let mut row = base.clone();
            row[key] = value;
            invalids.push(row);
        }
        invalids.push(base);
        let result = Engagement::from_state(&mobile(invalids), ID);
        assert_eq!(result.comments.len(), 1);
        assert_eq!(result.comments[0].like_count, 1);
    }

    #[test]
    fn unknown_statistics_stay_empty_and_abbreviated_counts_preserve_labels() {
        let mut state = mobile(Vec::new());
        state["noteData"]["data"]["noteData"]["interactInfo"] = json!({
            "likedCount": "12.3万", "collectedCount": -1,
            "commentCount": null, "shareCount": "尚无数据"
        });
        let stats = Engagement::from_state(&state, ID).stats;
        assert_eq!(stats.like_count, None);
        assert_eq!(stats.stats_display.likes.as_deref(), Some("12.3万"));
        assert!(stats.favorite_count.is_none());
        assert!(stats.comment_count.is_none());
        assert!(stats.share_count.is_none());
        for text in [
            "",
            "-1",
            "1.2",
            "1.万",
            "1.2.3万",
            "1,,234",
            "1,23",
            "18446744073709551615亿",
            "获赞1万",
        ] {
            assert_eq!(count(&json!(text)), (None, None), "{text}");
        }
        assert_eq!(count(&json!("0")), (Some(0), None));
        assert_eq!(count(&json!("10万+")), (Some(100000), Some("10万+".into())));
    }

    #[test]
    fn merged_candidates_deduplicate_ids_and_keep_the_three_highest_known_likes() {
        let mut first = Engagement::from_state(&mobile(vec![comment(1, "1"), comment(2, "3")]), ID);
        let second = Engagement::from_state(
            &mobile(vec![comment(1, "20"), comment(3, "9"), comment(4, "4")]),
            ID,
        );
        first.merge(second);
        assert_eq!(first.comments.len(), 4);
        assert_eq!(
            first
                .top_comments()
                .iter()
                .map(|comment| comment.like_count)
                .collect::<Vec<_>>(),
            [20, 9, 4]
        );
    }

    #[test]
    fn full_candidate_budget_keeps_new_high_likes_and_merges_existing_comment_media() {
        let mut first = Engagement::from_state(
            &mobile(
                (1..=50)
                    .map(|index| comment(index, &index.to_string()))
                    .collect(),
            ),
            ID,
        );
        let mut duplicate = comment(40, "200");
        duplicate["pictures"] = json!([{ "url": "https://sns.example/duplicate-picture.png" }]);
        let newer = Engagement::from_state(
            &mobile(vec![comment(51, "100"), duplicate, comment(52, "0")]),
            ID,
        );
        first.merge(newer);
        assert_eq!(first.comments.len(), MAX_CANDIDATES);
        let top = first.top_comments();
        assert_eq!(
            top.iter()
                .map(|comment| comment.like_count)
                .collect::<Vec<_>>(),
            [200, 100, 50]
        );
        assert_eq!(
            top[0].pictures,
            ["https://sns.example/duplicate-picture.png"]
        );
        assert!(
            !first
                .comments
                .iter()
                .any(|row| row.id == comment(1, "1")["id"].as_str().unwrap())
        );
        assert!(
            !first
                .comments
                .iter()
                .any(|row| row.id == comment(52, "0")["id"].as_str().unwrap())
        );
    }

    #[test]
    fn mobile_enrichment_only_fills_missing_counts_without_replacing_exact_pc_statistics() {
        let mut pc = Engagement::from_state(
            &json!({"note": {"noteId": ID, "interactInfo": {
                "likedCount": "123456", "commentCount": 12
            }}}),
            ID,
        );
        let mut h5 = mobile(Vec::new());
        h5["noteData"]["data"]["noteData"]["interactInfo"]["likedCount"] = json!("12.3万");
        pc.merge(Engagement::from_state(&h5, ID));
        assert_eq!(pc.stats.like_count, Some(123456));
        assert!(pc.stats.stats_display.likes.is_none());
        assert_eq!(pc.stats.comment_count, Some(12));
        assert_eq!(pc.stats.favorite_count, Some(208));
        assert_eq!(pc.stats.share_count, Some(211));
    }

    #[test]
    fn abbreviated_statistic_survives_unknown_enrichment_and_can_upgrade_to_exact_value() {
        let pc_state = json!({"note": {"noteId": ID, "interactInfo": {
            "likedCount": "1.7万", "commentCount": "10万+"
        }}});
        let mut pc = Engagement::from_state(&pc_state, ID);
        pc.merge(Engagement::from_state(&json!({"note": {"noteId": ID}}), ID));
        assert!(pc.stats.like_count.is_none());
        assert_eq!(pc.stats.stats_display.likes.as_deref(), Some("1.7万"));
        assert!(pc.stats.comment_count.is_none());
        assert_eq!(pc.stats.stats_display.comments.as_deref(), Some("10万+"));
        let exact = json!({"note": {"noteId": ID, "interactInfo": {"likedCount": 17432}}});
        pc.merge(Engagement::from_state(&exact, ID));
        assert_eq!(pc.stats.like_count, Some(17432));
        assert!(pc.stats.stats_display.likes.is_none());
        assert_eq!(pc.stats.stats_display.comments.as_deref(), Some("10万+"));
    }

    #[test]
    fn asset_urls_are_public_and_keep_signatures_unchanged() {
        let signed = "https://sns.example/avatar.png?sign=a%2Fb&key=Foo%2BBar";
        assert_eq!(image_url(signed).as_deref(), Some(signed));
        assert_eq!(
            image_url("//sns.example/a.png").as_deref(),
            Some("https://sns.example/a.png")
        );
        for url in [
            "file:///private/a.png",
            "http://127.0.0.1/a.png",
            "https://user:pass@sns.example/a.png",
        ] {
            assert!(image_url(url).is_none());
        }
    }

    #[test]
    fn official_emote_tokens_and_explicit_comment_pictures_keep_separate_roles() {
        let mut row = comment(1, "3");
        row["content"] = json!("表情[皱眉R][皱眉R][石化R][未收录R]🌙");
        row["pictures"] = json!([
            {"url": "https://sns.example/comment.png?sign=a%2Fb", "width": 120, "height": 160},
            {"url": "https://sns.example/comment.png?sign=a%2Fb"},
            {"url": "http://127.0.0.1/private.png"},
            {"url": "//sns.example/second.png"}
        ]);
        row["unrelated"] = json!({"url": "https://sns.example/wrong.png"});
        let got = Engagement::from_state(&mobile(vec![row]), ID)
            .top_comments()
            .pop()
            .unwrap();
        assert_eq!(
            got.emotes
                .iter()
                .map(|emote| emote.text.as_str())
                .collect::<Vec<_>>(),
            ["[皱眉R]", "[石化R]"]
        );
        assert!(got.emotes.iter().all(|emote| {
            emote
                .url
                .starts_with("https://picasso-static.xiaohongshu.com/fe-platform/")
                && !emote.large
        }));
        assert_eq!(
            got.pictures,
            [
                "https://sns.example/comment.png?sign=a%2Fb",
                "https://sns.example/second.png"
            ]
        );
        assert!(got.text.ends_with("[未收录R]🌙"));
    }

    #[test]
    fn known_zero_likes_can_keep_picture_only_comments_without_guessing_image_fields() {
        let mut picture_only = comment(1, "0");
        picture_only["content"] = json!("");
        picture_only["pictures"] = json!([{ "url": "https://sns.example/only.png" }]);
        let mut unknown_image = comment(2, "1");
        unknown_image["content"] = json!("");
        unknown_image["imageList"] =
            json!([{ "url": "https://sns.example/not-a-comment-picture.png" }]);
        let result = Engagement::from_state(&mobile(vec![picture_only, unknown_image]), ID);
        assert_eq!(result.top_comments().len(), 1);
        assert_eq!(result.top_comments()[0].like_count, 0);
    }

    #[test]
    fn missing_displayed_likes_can_use_exact_integer_but_unknown_display_cannot_use_h5_zero() {
        let mut exact = comment(1, "0");
        exact["likeViewCount"] = Value::Null;
        exact["likeCount"] = json!(13);
        let mut unknown = comment(2, "—");
        unknown["likeCount"] = json!(0);
        let result = Engagement::from_state(&mobile(vec![exact, unknown]), ID);
        assert_eq!(result.comments.len(), 1);
        assert_eq!(result.comments[0].like_count, 13);
    }
}
