use crate::resolver::card::{CardPublicDetails, CardStatsDisplay};
use crate::resolver::{CardComment, CardMetadata, CardPoster};

use super::*;

#[test]
fn global_comment_switch_hides_gallery_comments_and_retains_comment_count() {
    let card = Card::new().line("图集作品").with_metadata(CardMetadata {
        poster: Some(CardPoster {
            comment_count: Some(44),
            ..Default::default()
        }),
        comments: vec![CardComment {
            author: "评论者".into(),
            text: "这条评论不展示".into(),
            ..Default::default()
        }],
        ..Default::default()
    });
    assert!(gallery_text(&card).contains("评论者"));
    let hidden = gallery_text(&card.with_comments_visible(false));
    assert!(hidden.contains("评论 44"));
    assert!(!hidden.contains("评论者"));
    assert!(!hidden.contains("这条评论不展示"));
}

#[test]
fn gallery_text_shows_only_known_statistics_and_public_author_details() {
    let card = Card::new()
        .line("图集标题")
        .line("作品正文")
        .line("https://example.test/gallery")
        .with_metadata(CardMetadata {
            author: Some("测试作者".into()),
            published_at: Some(0),
            source_url: Some("https://example.test/gallery".into()),
            poster: Some(CardPoster {
                like_count: Some(100),
                comment_count: Some(0),
                stats_display: CardStatsDisplay {
                    favorites: Some("2.3万".into()),
                    ..Default::default()
                },
                ..Default::default()
            }),
            public_details: CardPublicDetails {
                author_followers_display: Some("29.2万".into()),
                author_likes_display: Some("752.0万".into()),
                suggested_search: Some("动态背景图".into()),
            },
            ..Default::default()
        });
    let text = gallery_text(&card);
    assert!(text.contains("图集标题\n作品正文"));
    assert!(text.contains("作者：测试作者"));
    assert!(text.contains("1970-01-01 08:00:00"));
    assert!(text.contains("点赞 100 · 评论 0 · 收藏 2.3万"));
    assert!(!text.contains("分享"));
    assert!(text.contains("粉丝：29.2万"));
    assert!(text.contains("获赞：752.0万"));
    assert!(text.contains("大家都在搜：动态背景图"));
    assert_eq!(text.matches("https://example.test/gallery").count(), 1);
    assert!(text.ends_with("https://example.test/gallery"));
}

#[test]
fn gallery_text_selects_highest_three_comments_without_extra_image_urls() {
    let card = Card::new().line("作品").with_metadata(CardMetadata {
        comments: vec![
            CardComment {
                author: "低赞评论".into(),
                text: "低赞正文".into(),
                like_count: 1,
                ..Default::default()
            },
            CardComment {
                author: "最高评论".into(),
                text: "最高正文[表情包]".into(),
                like_count: 99,
                pictures: vec!["https://cdn.example/attached.png".into()],
                avatar_url: Some("https://cdn.example/avatar.png".into()),
                ..Default::default()
            },
            CardComment {
                author: "第二评论".into(),
                text: "第二正文".into(),
                like_count: 9,
                ..Default::default()
            },
            CardComment {
                author: "第三评论".into(),
                text: "第三正文".into(),
                like_count: 8,
                like_count_display: Some("8".into()),
                ..Default::default()
            },
        ],
        ..Default::default()
    });
    let text = gallery_text(&card);
    assert!(!text.contains("低赞"));
    assert!(text.find("最高评论").unwrap() < text.find("第二评论").unwrap());
    assert!(text.find("第二评论").unwrap() < text.find("第三评论").unwrap());
    assert!(text.contains("最高正文[表情包] [含 1 张配图]"));
    assert!(!text.contains("https://cdn.example/"));
    assert!(!text.contains("可见评论"));
    assert!(!text.contains("点赞前三"));
}

#[test]
fn oversized_text_is_bounded_and_retains_original_link() {
    let source = "https://example.test/gallery?complete=original";
    let card = Card::new()
        .line("汉".repeat(8000))
        .with_metadata(CardMetadata {
            source_url: Some(source.into()),
            comments: (0..3)
                .map(|index| CardComment {
                    author: format!("评论者{index}"),
                    text: "长".repeat(8000),
                    like_count: index,
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        });
    let text = gallery_text(&card);
    assert!(text.chars().count() <= MAX_TEXT_CHARS);
    assert!(text.ends_with(source));
    assert!(text.contains('…'));
    assert!(text.contains("评论者"));
}

#[test]
fn absent_optional_metadata_is_not_filled_with_fake_zeroes_or_times() {
    let card = Card::new().line("作品");
    assert_eq!(gallery_text(&card), "作品");
}

#[test]
fn long_text_cannot_hide_forward_limit_or_direct_fallback_notices() {
    let source = "https://example.test/gallery";
    for unsupported in [false, true] {
        let delivery = GalleryDelivery::new(
            format!("{}\n{source}", "文".repeat(6000)),
            (0..8)
                .map(|_| nuono_plugin::MessageSegment::image("base64://bounded-fixture"))
                .collect(),
            49,
            source,
        )
        .unwrap();
        let encoded = delivery.into_direct(unsupported).to_json();
        let text = encoded[0]["data"]["text"].as_str().unwrap();
        assert!(text.chars().count() <= MAX_TEXT_CHARS);
        assert!(text.contains("图集共 49 张"));
        assert!(text.contains("最多展示前 8 张"));
        assert_eq!(text.contains("当前平台不支持合并转发"), unsupported);
        assert!(text.ends_with(source));
    }
}
