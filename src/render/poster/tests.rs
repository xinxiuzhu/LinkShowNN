use super::layout::layout;
use super::*;
use crate::render::icons::{StatKind, statistics};
use crate::render::{
    format::{compact_count, count_text},
    image::{ImageInfo, encode_png},
    limits::MAX_HEIGHT,
    text::{FONT, clip_font, width, wrap_font},
};
use crate::resolver::card::{CardComment, CardMetadata};
use crate::resolver::{CardPoster, CardPublicDetails, Platform};
use ::image::metadata::Orientation;

fn comment(id: u64, text: &str) -> CardComment {
    CardComment {
        id: id.to_string(),
        author: format!("评论者{id}"),
        author_id: Some(id.to_string()),
        avatar_url: Some(format!("comment-avatar-{id}")),
        text: text.into(),
        like_count: 100 / id.max(1),
        published_at: None,
        ..Default::default()
    }
}

fn fixture(poster: CardPoster) -> (Card, HashMap<String, Vec<u8>>) {
    let card = Card::new()
        .with_metadata(CardMetadata {
            platform: Some(Platform::Bilibili),
            author: Some("作者名字".into()),
            avatar_url: Some("avatar".into()),
            video_cover_url: Some("cover".into()),
            poster: Some(poster),
            ..Default::default()
        })
        .image("cover");
    let assets = HashMap::from([
        (
            "cover".into(),
            encode_png(&RgbImage::from_pixel(160, 90, Rgb([20, 170, 240]))).unwrap(),
        ),
        (
            "avatar".into(),
            encode_png(&RgbImage::from_pixel(20, 20, Rgb([250, 20, 20]))).unwrap(),
        ),
    ]);
    (card, assets)
}

#[test]
fn poster_uses_video_cover_not_avatar_and_has_transparent_corners() {
    let (card, assets) = fixture(CardPoster {
        title: "视频标题".into(),
        ..Default::default()
    });
    let png = crate::render::compose(&card, &assets).unwrap();
    let output = ::image::load_from_memory(&png).unwrap().into_rgba8();
    assert_eq!(output.width(), 800);
    assert!(output.height() <= MAX_HEIGHT);
    assert_eq!(output.get_pixel(0, 0)[3], 0);
    assert_eq!(output.get_pixel(799, output.height() - 1)[3], 0);
    let cover = output.get_pixel(400, 100);
    assert!(cover[2] > cover[0] * 3, "top must use the blue video cover");
    let plan = layout(&card, image_info(&assets["cover"]).unwrap()).unwrap();
    let avatar = output.get_pixel(MARGIN + 48, plan.author_y + 48);
    assert!(avatar[0] > avatar[2] * 3, "author must use the red avatar");
}

fn check_music_album_poster(platform: Platform, source: &str, original: &str, preview: &str) {
    use crate::resolver::{MediaInfo, ResolvedContent};
    use qrcode::{Color, EcLevel, QrCode};

    let cover_url = "https://cdn.example.com/music-album.png";
    let description =
        "专辑：风与回声\n歌曲时长：03:42\n仅试听：00:30；完整播放需要账号具备相应权益";
    let card = MediaInfo {
        title: Some("风中的旋律".into()),
        description: Some(description.into()),
        author: Some("模拟歌手".into()),
        cover_url: Some(cover_url.into()),
        audio_url: Some("https://cdn.example.com/music-preview.mp3".into()),
        source_url: source.into(),
        platform,
        ..Default::default()
    }
    .card()
    .with_original_url(original);
    let assets = HashMap::from([(
        cover_url.into(),
        encode_png(&RgbImage::from_pixel(160, 160, Rgb([24, 170, 240]))).unwrap(),
    )]);

    let metadata = card.metadata();
    let poster = metadata
        .poster
        .as_ref()
        .expect("music cover creates a poster");
    assert_eq!(poster.cover_url.as_deref(), Some(cover_url));
    assert_eq!(card.poster_cover_url(), Some(cover_url));
    assert!(metadata.video_cover_url.is_none());
    assert!(!card.is_gallery());
    assert_eq!(metadata.author.as_deref(), Some("模拟歌手"));
    assert_eq!(poster.description.as_deref(), Some(description));
    assert!(card.text().contains(description));

    let plan = layout(&card, image_info(&assets[cover_url]).unwrap()).unwrap();
    assert_eq!(plan.author, "模拟歌手");
    assert_eq!(plan.title.concat(), "风中的旋律");
    let rendered_description = plan.description.concat();
    for text in ["风与回声", "03:42", "仅试听", "00:30", "相应权益"] {
        assert!(
            rendered_description.contains(text),
            "{platform:?}: missing {text}"
        );
    }
    assert!(
        plan.stats.is_empty(),
        "music must not invent video statistics"
    );

    let png = crate::render::compose(&card, &assets).unwrap();
    let output = ::image::load_from_memory(&png).unwrap().into_rgba8();
    assert_eq!(output.width(), WIDTH);
    assert_eq!(output.height(), plan.height);
    assert!(output.height() <= MAX_HEIGHT);
    assert_eq!(output.get_pixel(0, 0)[3], 0);
    assert_eq!(output.get_pixel(WIDTH - 1, output.height() - 1)[3], 0);
    let top = output.get_pixel(WIDTH / 2, 120);
    let bottom = output.get_pixel(WIDTH / 2, output.height() - 90);
    assert_eq!(top[3], 255);
    assert!(
        u32::from(top[2]) > u32::from(top[0]) * 3,
        "album cover stays blue"
    );
    assert!(
        top.0[..3]
            .iter()
            .map(|channel| u32::from(*channel))
            .sum::<u32>()
            > bottom.0[..3]
                .iter()
                .map(|channel| u32::from(*channel))
                .sum::<u32>()
                * 2,
        "the lower poster must fade to a darker background"
    );

    // Check the QR bits in the final PNG against the original share URL's encoded
    // payload. The canonical song link differs and omits the sharing parameters.
    assert_ne!(source, original);
    let qr = plan.link_qr.as_ref().expect("music original link has a QR");
    let expected = QrCode::with_error_correction_level(original.as_bytes(), EcLevel::H).unwrap();
    let modules = expected.width() as u32;
    let module_pixels = qr.size() / (modules + 8);
    assert!(module_pixels >= 2);
    assert_eq!(qr.size(), module_pixels * (modules + 8));
    let qr_x = WIDTH - MARGIN - qr.size();
    for y in 0..modules {
        for x in 0..modules {
            let pixel = output.get_pixel(
                qr_x + (x + 4) * module_pixels + module_pixels / 2,
                plan.author_y + (y + 4) * module_pixels + module_pixels / 2,
            );
            let is_ink = pixel.0[..3].iter().all(|channel| *channel > 180);
            assert_eq!(
                is_ink,
                expected[(x as usize, y as usize)] == Color::Dark,
                "{platform:?}: original link QR module ({x}, {y})"
            );
        }
    }

    if let Some(directory) = std::env::var_os("NUONO_CARD_PREVIEW_DIR") {
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(std::path::Path::new(&directory).join(preview), png).unwrap();
    }
}

#[test]
fn netease_music_album_uses_poster_style_without_becoming_video_or_gallery() {
    check_music_album_poster(
        Platform::Ncm,
        "https://music.163.com/song?id=109125",
        "https://music.163.com/song?id=109125&uct2=fixture%2Bshare",
        "linkshownn-netease-music-preview.png",
    );
}

#[test]
fn qqmusic_album_uses_poster_style_without_becoming_video_or_gallery() {
    check_music_album_poster(
        Platform::QqMusic,
        "https://y.qq.com/n/ryqq/songDetail/003fixtureSong",
        "https://c6.y.qq.com/base/fcgi-bin/u?__=fixtureShare",
        "linkshownn-qqmusic-preview.png",
    );
}

#[test]
fn original_link_qr_keeps_author_and_comment_areas_separate() {
    let (card, assets) = fixture(CardPoster {
        title: "二维码卡片".into(),
        author_id: Some("36323155278858571".into()),
        ..Default::default()
    });
    let mut metadata = card.metadata().clone();
    metadata.author = Some("很长的作者昵称很长的作者昵称很长的作者昵称".into());
    metadata.source_url = Some("https://www.xiaohongshu.com/explore/6ac6146a000000001b021019?xsec_token=full_signed_parameter".into());
    metadata.original_url = Some("https://xhslink.cn/o/6Ji5ENKJVty".into());
    metadata.comments = vec![comment(1, "评论仍位于作者与二维码下方")];
    let card = card.with_metadata(metadata);
    let plan = layout(&card, image_info(&assets["cover"]).unwrap()).unwrap();
    let qr = plan.link_qr.as_ref().unwrap();
    let x = WIDTH - MARGIN - qr.size();
    let author_x = MARGIN + POSTER_AVATAR + 32;
    assert!(author_x + text_width(&plan.author, 38.0).ceil() as u32 + 24 <= x);
    assert!(author_x + text_width(plan.author_id.as_ref().unwrap(), 26.0).ceil() as u32 + 24 <= x);
    assert!(plan.comments_y >= plan.author_y + qr.size() + 30 + 48);
    let png = render(&card, &assets).unwrap();
    let pixels = ::image::load_from_memory(&png).unwrap().into_rgb8();
    assert_ne!(pixels.get_pixel(x, plan.author_y).0, [255; 3]);
    if let Some(directory) = std::env::var_os("NUONO_CARD_PREVIEW_DIR") {
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(
            std::path::Path::new(&directory).join("linkshownn-link-qr-preview.png"),
            png,
        )
        .unwrap();
    }
}

#[test]
fn missing_counts_are_omitted_but_real_zero_is_retained() {
    for platform in [None, Some(Platform::Bilibili), Some(Platform::Douyin)] {
        assert!(statistics(&CardPoster::default(), platform).is_empty());
    }
    let stats = statistics(
        &CardPoster {
            coin_count: Some(0),
            like_count: Some(42_321),
            ..Default::default()
        },
        Some(Platform::Bilibili),
    );
    assert_eq!(
        stats,
        [
            (StatKind::Like, "4.2万".into()),
            (StatKind::Coin, "0".into())
        ]
    );
    assert_eq!(compact_count(2_003), "2,003");
    assert_eq!(compact_count(662_100), "66.2万");
    assert_eq!(compact_count(41_804), "4.2万");
    assert_eq!(compact_count(99_999), "10万");
    assert_eq!(compact_count(250_000_000), "2.5亿");
}

#[test]
fn abbreviated_web_counts_render_without_inventing_exact_numbers() {
    use crate::resolver::card::CardStatsDisplay;
    let (card, assets) = fixture(CardPoster {
        title: "抖音作品".into(),
        stats_display: CardStatsDisplay {
            likes: Some("20.9万".into()),
            comments: Some("1.2万".into()),
            favorites: Some("1.4万".into()),
            shares: Some("5.2万".into()),
        },
        ..Default::default()
    });
    let mut metadata = card.metadata().clone();
    metadata.platform = Some(Platform::Douyin);
    let card = card.with_metadata(metadata);
    let plan = layout(&card, image_info(&assets["cover"]).unwrap()).unwrap();
    assert_eq!(
        plan.stats,
        [
            (StatKind::Like, "20.9万".into()),
            (StatKind::Comment, "1.2万".into()),
            (StatKind::Favorite, "1.4万".into()),
            (StatKind::Share, "5.2万".into())
        ]
    );
    assert!(plan.stat_labels);
    assert!(!plan.details.join(" ").contains("评论"));
    assert_eq!(
        plan.stats
            .iter()
            .map(|(kind, _)| kind.label())
            .collect::<Vec<_>>(),
        ["点赞", "评论", "收藏", "分享"]
    );
    assert!(plan.author_y >= plan.stats_y + ICON_SIZE + 7 + 22 + 32);
    let png = crate::render::compose(&card, &assets).unwrap();
    assert_eq!(
        ::image::load_from_memory(&png).unwrap().height(),
        plan.height
    );
    if let Some(directory) = std::env::var_os("NUONO_CARD_PREVIEW_DIR") {
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(
            std::path::Path::new(&directory).join("linkshownn-douyin-statistics-preview.png"),
            png,
        )
        .unwrap();
    }
    assert_eq!(
        count_text(Some(1234), &Some("1.2万".into())).as_deref(),
        Some("1,234")
    );
}

#[test]
fn xiaohongshu_uses_likes_comments_favorites_shares_and_shared_comment_layout() {
    let (card, assets) = fixture(CardPoster {
        title: "小红书公开视频".into(),
        like_count: Some(1728),
        comment_count: Some(44),
        favorite_count: Some(208),
        share_count: Some(211),
        ..Default::default()
    });
    let mut metadata = card.metadata().clone();
    metadata.platform = Some(Platform::Xiaohongshu);
    metadata.comments = vec![comment(1, "公开评论[皱眉R]")];
    let card = card.with_metadata(metadata);
    let plan = layout(&card, image_info(&assets["cover"]).unwrap()).unwrap();
    assert_eq!(
        plan.stats,
        [
            (StatKind::Like, "1,728".into()),
            (StatKind::Comment, "44".into()),
            (StatKind::Favorite, "208".into()),
            (StatKind::Share, "211".into()),
        ]
    );
    assert!(plan.stat_labels);
    assert!(!plan.details.join(" ").contains("评论"));
    assert_eq!(plan.comments.len(), 1);
    let png = crate::render::compose(&card, &assets).unwrap();
    assert_eq!(
        ::image::load_from_memory(&png).unwrap().height(),
        plan.height
    );
}

#[test]
fn playback_quality_and_anonymous_notice_are_readable_with_comments_disabled() {
    let (card, assets) = fixture(CardPoster {
        title: "清晰度与评论开关".into(),
        video_quality: Some("720P".into()),
        quality_notice: Some("未登录，本次可获取720P；可登录后再尝试更高画质".into()),
        comment_count: Some(44),
        ..Default::default()
    });
    let mut metadata = card.metadata().clone();
    metadata.comments = vec![comment(1, "不展示的评论")];
    let hidden = card.with_metadata(metadata).with_comments_visible(false);
    let plan = layout(&hidden, image_info(&assets["cover"]).unwrap()).unwrap();
    let details = plan.details.join("");
    assert!(details.contains("清晰度 720P"));
    assert!(details.contains("未登录"));
    assert!(details.contains("44评论"));
    assert!(plan.comments.is_empty());
    assert!(render(&hidden, &assets).is_ok());
}

#[test]
fn bilibili_keeps_coin_cell_and_comments_in_details() {
    let (card, assets) = fixture(CardPoster {
        like_count: Some(1),
        coin_count: Some(2),
        favorite_count: Some(3),
        share_count: Some(4),
        comment_count: Some(5),
        ..Default::default()
    });
    let plan = layout(&card, image_info(&assets["cover"]).unwrap()).unwrap();
    assert_eq!(
        plan.stats,
        [
            (StatKind::Like, "1".into()),
            (StatKind::Coin, "2".into()),
            (StatKind::Favorite, "3".into()),
            (StatKind::Share, "4".into()),
        ]
    );
    assert!(!plan.stat_labels);
    assert_eq!(plan.author_y, plan.stats_y + 96);
    assert_eq!(plan.details, ["5评论"]);
    assert!(plan.author_details.is_empty());
    assert_eq!(plan.comments_y, plan.author_y + POSTER_AVATAR + 48);
}

#[test]
fn douyin_zero_comments_is_visible_and_unknown_cells_are_omitted() {
    let (card, assets) = fixture(CardPoster {
        comment_count: Some(0),
        coin_count: Some(999),
        ..Default::default()
    });
    let mut metadata = card.metadata().clone();
    metadata.platform = Some(Platform::Douyin);
    let card = card.with_metadata(metadata);
    let plan = layout(&card, image_info(&assets["cover"]).unwrap()).unwrap();
    assert_eq!(plan.stats, [(StatKind::Comment, "0".into())]);
    assert!(plan.details.is_empty());
    assert_eq!(plan.author_y, plan.stats_y + 118);

    let mut metadata = card.metadata().clone();
    metadata.poster = Some(CardPoster::default());
    let empty = card.with_metadata(metadata);
    let plan = layout(&empty, image_info(&assets["cover"]).unwrap()).unwrap();
    assert!(plan.stats.is_empty());
    assert_eq!(plan.author_y, plan.stats_y + 8);
}

#[test]
fn public_author_counts_and_search_are_optional_and_keep_comments_below_them() {
    let (card, assets) = fixture(CardPoster {
        title: "公开作者信息".into(),
        ..Default::default()
    });
    let cover = image_info(&assets["cover"]).unwrap();
    let baseline = layout(&card, cover).unwrap();
    let mut metadata = card.metadata().clone();
    metadata.platform = Some(Platform::Douyin);
    metadata.public_details = CardPublicDetails {
        author_followers_display: Some("29.2万".into()),
        author_likes_display: Some("752.0万".into()),
        suggested_search: Some("苹果折叠屏".into()),
    };
    metadata.comments = vec![comment(1, "评论在作者信息之后")];
    let card = card.with_metadata(metadata);
    let plan = layout(&card, cover).unwrap();
    assert_eq!(
        plan.author_details,
        ["粉丝 29.2万   获赞 752.0万", "大家都在搜：苹果折叠屏"]
    );
    assert!(plan.author_details_y > plan.author_y + POSTER_AVATAR);
    assert!(plan.comments_y >= plan.author_details_y + 2 * 28 + 48);
    assert!(plan.comments[0].y > plan.comments_y);
    assert!(plan.height <= MAX_HEIGHT);
    let png = crate::render::compose(&card, &assets).unwrap();
    assert_eq!(
        ::image::load_from_memory(&png).unwrap().height(),
        plan.height
    );

    let mut metadata = card.metadata().clone();
    metadata.public_details = CardPublicDetails {
        author_followers_display: Some(" ".into()),
        author_likes_display: None,
        suggested_search: Some("\n".into()),
    };
    metadata.comments.clear();
    let empty = card.with_metadata(metadata);
    let plan = layout(&empty, cover).unwrap();
    assert!(plan.author_details.is_empty());
    assert_eq!(plan.comments_y, baseline.comments_y);
    assert_eq!(plan.height, baseline.height);
}

#[test]
fn public_details_are_bounded_without_losing_all_comment_rows() {
    let (card, _) = fixture(CardPoster {
        title: "长标题".repeat(1000),
        description: Some("长简介".repeat(1000)),
        ..Default::default()
    });
    let mut metadata = card.metadata().clone();
    metadata.platform = Some(Platform::Douyin);
    metadata.public_details = CardPublicDetails {
        author_followers_display: Some("123456789".repeat(1000)),
        author_likes_display: Some("987654321".repeat(1000)),
        suggested_search: Some("长搜索推荐".repeat(1000)),
    };
    metadata.comments = (1..=3)
        .map(|id| comment(id, &"长评论".repeat(1000)))
        .collect();
    let card = card.with_metadata(metadata);
    let plan = layout(
        &card,
        ImageInfo {
            width: 90,
            height: 160,
            orientation: Orientation::NoTransforms,
        },
    )
    .unwrap();
    assert_eq!(plan.author_details.len(), 3);
    assert!(plan.author_details.last().unwrap().ends_with('…'));
    assert!(
        plan.author_details
            .iter()
            .all(|line| text_width(line, DETAIL_SIZE) <= TEXT_WIDTH as f32)
    );
    assert_eq!(plan.comments.len(), 3);
    assert!(plan.height <= MAX_HEIGHT);
    assert!(plan.comments.last().unwrap().y < plan.footer_y);
}

#[test]
fn portrait_cover_is_contained_and_exif_rotation_changes_its_layout() {
    let (card, _) = fixture(CardPoster::default());
    let portrait = ImageInfo {
        width: 90,
        height: 160,
        orientation: Orientation::NoTransforms,
    };
    let plan = layout(&card, portrait).unwrap();
    assert_eq!(plan.cover_height, 1400);
    assert!(plan.cover_width <= WIDTH);
    assert!((plan.cover_width as f32 / plan.cover_height as f32 - 90.0 / 160.0).abs() < 0.001);
    assert!(plan.title_y > plan.cover_height);
    let landscape = layout(
        &card,
        ImageInfo {
            orientation: Orientation::Rotate90,
            ..portrait
        },
    )
    .unwrap();
    assert_eq!((landscape.cover_width, landscape.cover_height), (800, 450));
}

#[test]
fn long_title_author_ids_and_description_stay_inside_layout() {
    let (card, assets) = fixture(CardPoster {
        title: "很长的标题 ABCDEFGH ".repeat(1000),
        description: Some("正文说明".repeat(4000)),
        content_id: Some("BV0123456789".repeat(100)),
        author_id: Some("0123456789".repeat(100)),
        ..Default::default()
    });
    let mut metadata = card.metadata().clone();
    metadata.author = Some("非常长的作者名称".repeat(100));
    let card = card.with_metadata(metadata);
    let plan = layout(&card, image_info(&assets["cover"]).unwrap()).unwrap();
    assert_eq!(plan.title.len(), 4);
    assert!(
        plan.title
            .iter()
            .all(|line| width(&BOLD, line, TITLE_SIZE) <= TEXT_WIDTH as f32)
    );
    assert!(plan.title.last().unwrap().ends_with('…'));
    assert!(plan.description.len() <= 6);
    assert!(
        plan.description
            .iter()
            .all(|line| text_width(line, DESCRIPTION_SIZE) <= TEXT_WIDTH as f32)
    );
    let author_width = WIDTH - MARGIN * 2 - POSTER_AVATAR - 32;
    assert!(width(&BOLD, &plan.author, 38.0) <= author_width as f32);
    assert!(text_width(plan.author_id.as_deref().unwrap(), 26.0) <= author_width as f32);
    assert!(text_width(plan.content_id.as_deref().unwrap(), DETAIL_SIZE) <= TEXT_WIDTH as f32);
    assert!(!plan.content_id_inline);
    assert!(plan.author_y + POSTER_AVATAR < plan.footer_y);
    assert!(plan.height <= MAX_HEIGHT);
}

#[test]
fn title_and_nickname_truncation_preserve_complete_emoji_sequences() {
    for sequence in ["👩🏽‍💻", "👨‍👩‍👧‍👦", "🇨🇳", "❤️", "1️⃣"]
    {
        let source = sequence.repeat(100);
        let nickname_width = width(&BOLD, &format!("{sequence}…"), 38.0).ceil() as u32;
        let nickname = clip_font(&BOLD, &source, 38.0, nickname_width);
        assert!(nickname.ends_with('…'), "{sequence}: {nickname}");
        let visible = nickname.strip_suffix('…').unwrap();
        assert!(!visible.is_empty(), "one complete emoji must fit");
        assert!(
            visible.replace(sequence, "").is_empty(),
            "{sequence}: {nickname}"
        );
        assert!(width(&BOLD, &nickname, 38.0) <= nickname_width as f32);

        let title_width = width(&BOLD, &sequence.repeat(2), TITLE_SIZE).ceil() as u32;
        let title = wrap_font(&BOLD, &source, TITLE_SIZE, title_width, 4);
        assert_eq!(title.len(), 4);
        assert!(title.last().unwrap().ends_with('…'));
        for line in title {
            let visible = line.strip_suffix('…').unwrap_or(&line);
            assert!(
                visible.replace(sequence, "").is_empty(),
                "{sequence}: {line}"
            );
            assert!(width(&BOLD, &line, TITLE_SIZE) <= title_width as f32);
        }
        assert_eq!(
            avatar_initial(&format!(" \n{sequence}昵称"), "评"),
            sequence
        );
    }
    assert_eq!(avatar_initial(" \n", "评"), "评");
}

#[test]
fn emoji_metrics_scale_with_title_and_nickname_sizes() {
    let sample = "鸣潮 🌙 👩🏽‍💻 🇨🇳 ❤️";
    let small = width(&BOLD, sample, 22.0);
    let large = width(&BOLD, sample, 44.0);
    assert!(small > 0.0);
    assert!((large - small * 2.0).abs() < 0.01);
    for size in [19.0, DESCRIPTION_SIZE, 38.0, TITLE_SIZE] {
        let emoji = width(&BOLD, "🌙", size);
        assert!(emoji >= size * 0.8 && emoji <= size * 1.5);
        assert!((emoji - width(&FONT, "🌙", size)).abs() < 0.01);
    }
}

#[test]
fn poster_plan_preserves_moon_description_and_emoji_authors() {
    let description = "🌙 共鸣者档案前瞻 | 莉莉\n这个味道，嗯……希望这份心意能传达给你。";
    let (card, assets) = fixture(CardPoster {
        title: "莉莉 🌙 👩🏽‍💻 🇨🇳 ❤️".into(),
        description: Some(description.into()),
        ..Default::default()
    });
    let mut metadata = card.metadata().clone();
    metadata.author = Some("👩🏽‍💻 鸣潮".into());
    metadata.comments = vec![CardComment {
        author: "🇨🇳 测试者".into(),
        ..comment(1, "🌙 今晚见")
    }];
    let card = card.with_metadata(metadata);
    let plan = layout(&card, image_info(&assets["cover"]).unwrap()).unwrap();
    assert_eq!(plan.title.concat(), "莉莉 🌙 👩🏽‍💻 🇨🇳 ❤️");
    assert_eq!(plan.author, "👩🏽‍💻 鸣潮");
    assert_eq!(plan.comments[0].author, "🇨🇳 测试者");
    assert!(plan.description.concat().contains('🌙'));
    assert!(!plan.description.concat().contains('□'));
    assert!(
        plan.description
            .iter()
            .all(|line| text_width(line, DESCRIPTION_SIZE) <= TEXT_WIDTH as f32)
    );
    assert!(plan.height <= MAX_HEIGHT);
}

#[test]
fn bad_cover_fails_and_bad_avatar_uses_placeholder() {
    let (card, mut assets) = fixture(CardPoster::default());
    assets.insert("avatar".into(), vec![1, 2, 3]);
    assert!(render(&card, &assets).is_ok());
    assets.insert("cover".into(), vec![1, 2, 3]);
    assert!(render(&card, &assets).is_err());
    assets.remove("cover");
    assert!(render(&card, &assets).is_err());
}

#[test]
fn comments_append_below_author_and_optional_avatars_do_not_break_poster() {
    let (card, mut assets) = fixture(CardPoster::default());
    let cover = image_info(&assets["cover"]).unwrap();
    let original_plan = layout(&card, cover).unwrap();
    let mut metadata = card.metadata().clone();
    metadata.comments = vec![
        comment(1, "第一条评论"),
        comment(2, "头像损坏仍然保留评论"),
        comment(3, "没有头像也可以展示"),
        comment(4, "不展示第四条"),
    ];
    metadata.comments[2].like_count = 0;
    let card = card.with_metadata(metadata);
    assets.insert(
        "comment-avatar-1".into(),
        encode_png(&RgbImage::from_pixel(20, 20, Rgb([10, 240, 20]))).unwrap(),
    );
    assets.insert("comment-avatar-2".into(), vec![1, 2, 3]);
    let plan = layout(&card, image_info(&assets["cover"]).unwrap()).unwrap();
    assert_eq!(plan.comments.len(), 3);
    assert_eq!(plan.comments[2].likes, "0");
    assert_eq!(original_plan.author_y, plan.author_y);
    assert_eq!(original_plan.title_y, plan.title_y);
    assert!(plan.comments_y > plan.author_y + POSTER_AVATAR);
    assert!(plan.comments[0].y > plan.comments_y);
    assert!(plan.height > original_plan.height);
    let png = crate::render::compose(&card, &assets).unwrap();
    let output = ::image::load_from_memory(&png).unwrap().into_rgba8();
    let avatar = output.get_pixel(
        MARGIN + COMMENT_AVATAR / 2,
        plan.comments[0].y + COMMENT_AVATAR / 2,
    );
    assert!(
        avatar[1] > 200 && avatar[0] < 30,
        "comment uses its own avatar"
    );
    assert_eq!(output.height(), plan.height);

    let mut metadata = card.metadata().clone();
    metadata.comments = vec![comment(1, " \n ")];
    let empty = card.with_metadata(metadata);
    let empty_plan = layout(&empty, image_info(&assets["cover"]).unwrap()).unwrap();
    assert!(empty_plan.comments.is_empty());
    assert_eq!(empty_plan.height, original_plan.height);
}

#[test]
fn long_comments_fit_three_lines_and_keep_footer_inside_tall_poster() {
    let (card, _) = fixture(CardPoster {
        title: "很长的标题".repeat(1000),
        description: Some("正文说明".repeat(1000)),
        ..Default::default()
    });
    let mut metadata = card.metadata().clone();
    metadata.comments = (1..=3)
        .map(|id| {
            let mut value = comment(id, &"评论长文 English words ".repeat(1000));
            value.author = "很长的昵称".repeat(1000);
            value.like_count = u64::MAX;
            value
        })
        .collect();
    let card = card.with_metadata(metadata);
    let plan = layout(
        &card,
        ImageInfo {
            width: 90,
            height: 160,
            orientation: Orientation::NoTransforms,
        },
    )
    .unwrap();
    assert_eq!(plan.comments.len(), 3);
    for comment in &plan.comments {
        assert_eq!(comment.body.lines.len(), 3);
        assert!(comment.body.lines.last().unwrap().text().ends_with('…'));
        assert!(
            comment
                .body
                .lines
                .iter()
                .all(|line| { line.width <= COMMENT_TEXT_WIDTH as f32 })
        );
        let author_and_likes = text_width(&comment.author, 19.0)
            + text_width(&comment.likes, 17.0)
            + COMMENT_LIKE_SIZE as f32
            + 32.0;
        assert!(author_and_likes <= COMMENT_TEXT_WIDTH as f32);
        assert!(comment.y + 36 + 3 * COMMENT_LEADING < plan.footer_y);
    }
    assert!(plan.height <= MAX_HEIGHT);
}

#[test]
fn inline_emotes_and_pictures_keep_all_three_comments_within_canvas() {
    use crate::resolver::card::CardCommentEmote;

    let (card, mut assets) = fixture(CardPoster {
        title: "长标题".repeat(200),
        description: Some("详细说明".repeat(200)),
        like_count: Some(10),
        content_id: Some("稿件编号".repeat(100)),
        ..Default::default()
    });
    assets.insert(
        "emote".into(),
        encode_png(&RgbImage::from_pixel(100, 100, Rgb([20, 240, 30]))).unwrap(),
    );
    assets.insert(
        "picture".into(),
        encode_png(&RgbImage::from_pixel(400, 300, Rgb([20, 30, 240]))).unwrap(),
    );
    let mut metadata = card.metadata().clone();
    metadata.comments = (1..=3)
        .map(|id| CardComment {
            emotes: vec![CardCommentEmote {
                text: "[MyGO]".into(),
                url: "emote".into(),
                large: true,
            }],
            pictures: vec!["picture".into(); 3],
            ..comment(id, &"评论[MyGO]".repeat(200))
        })
        .collect();
    let card = card.with_metadata(metadata);
    let media = comments::Media::prepare(&card.metadata().comments, &assets);
    let plan = layout_with_media(
        &card,
        ImageInfo {
            width: 90,
            height: 160,
            orientation: Orientation::NoTransforms,
        },
        media,
    )
    .unwrap();
    assert_eq!(plan.comments.len(), 3);
    assert!(plan.height <= MAX_HEIGHT);
    for comment in &plan.comments {
        assert_eq!(comment.body.lines.len(), 3);
        assert!(
            comment
                .body
                .lines
                .iter()
                .all(|line| line.height > COMMENT_LEADING)
        );
        assert!(comment.y + 36 + comment.body.height() < plan.footer_y);
    }
    for comments in plan.comments.windows(2) {
        assert!(comments[0].y + 36 + comments[0].body.height() < comments[1].y);
    }
}

#[test]
fn picture_only_comment_and_failed_picture_are_visible() {
    let (card, mut assets) = fixture(CardPoster::default());
    let mut metadata = card.metadata().clone();
    metadata.comments = vec![CardComment {
        pictures: vec!["comment-picture".into()],
        ..comment(1, "")
    }];
    let card = card.with_metadata(metadata);
    assets.insert(
        "comment-picture".into(),
        encode_png(&RgbImage::from_pixel(100, 80, Rgb([20, 240, 30]))).unwrap(),
    );
    for valid in [true, false] {
        if !valid {
            assets.insert("comment-picture".into(), vec![1, 2, 3]);
        }
        let media = comments::Media::prepare(&card.metadata().comments, &assets);
        let plan = layout_with_media(&card, image_info(&assets["cover"]).unwrap(), media).unwrap();
        assert_eq!(plan.comments.len(), 1);
        assert!(plan.comments[0].body.lines.is_empty());
        assert!(plan.comments[0].body.height() >= 26);
        assert!(render(&card, &assets).is_ok());
    }
}

#[test]
#[ignore = "requires a local public metadata/cover fixture; performs no network requests"]
fn real_video_poster_preview() {
    let directory =
        std::env::var_os("NUONO_POSTER_FIXTURE_DIR").expect("set NUONO_POSTER_FIXTURE_DIR");
    let directory = std::path::Path::new(&directory);
    let value: serde_json::Value =
        serde_json::from_slice(&std::fs::read(directory.join("view.json")).unwrap()).unwrap();
    let value = value.get("data").unwrap_or(&value);
    let stat = &value["stat"];
    let owner = &value["owner"];
    let comments_path = directory.join("comments.json");
    let comments = if comments_path.exists() {
        let value = serde_json::from_slice(&std::fs::read(comments_path).unwrap()).unwrap();
        crate::resolver::bilibili::comments::map_comments(&value)
    } else {
        Vec::new()
    };
    let card = Card::new()
        .with_metadata(CardMetadata {
            platform: Some(Platform::Bilibili),
            author: owner["name"].as_str().map(str::to_owned),
            avatar_url: Some("avatar".into()),
            video_cover_url: Some("cover".into()),
            published_at: value["pubdate"].as_i64(),
            comments,
            poster: Some(CardPoster {
                cover_url: Some("cover".into()),
                title: value["title"].as_str().unwrap_or("").into(),
                description: value["desc"].as_str().map(str::to_owned),
                content_id: value["bvid"].as_str().map(str::to_owned),
                author_id: owner["mid"].as_u64().map(|id| id.to_string()),
                view_count: stat["view"].as_u64(),
                danmaku_count: stat["danmaku"].as_u64(),
                comment_count: stat["reply"].as_u64(),
                like_count: stat["like"].as_u64(),
                coin_count: stat["coin"].as_u64(),
                favorite_count: stat["favorite"].as_u64(),
                share_count: stat["share"].as_u64(),
                stats_display: Default::default(),
                video_quality: None,
                quality_notice: None,
            }),
            ..Default::default()
        })
        .image("cover");
    let mut assets = HashMap::from([
        (
            "cover".into(),
            std::fs::read(directory.join("cover.jpg")).unwrap(),
        ),
        (
            "avatar".into(),
            std::fs::read(directory.join("avatar.jpg")).unwrap(),
        ),
    ]);
    for comment in &card.metadata().comments {
        if let Some(url) = &comment.avatar_url
            && let Ok(bytes) =
                std::fs::read(directory.join(format!("comments-avatar-{}.jpg", comment.id)))
        {
            assets.insert(url.clone(), bytes);
        }
        for (index, emote) in comment.emotes.iter().enumerate() {
            if let Ok(bytes) =
                std::fs::read(directory.join(format!("comment-emote-{}-{index}.bin", comment.id)))
            {
                assets.insert(emote.url.clone(), bytes);
            }
        }
        for (index, url) in comment.pictures.iter().enumerate() {
            if let Ok(bytes) =
                std::fs::read(directory.join(format!("comment-picture-{}-{index}.bin", comment.id)))
            {
                assets.insert(url.clone(), bytes);
            }
        }
    }
    let output = std::env::var_os("NUONO_CARD_PREVIEW_DIR").expect("set NUONO_CARD_PREVIEW_DIR");
    std::fs::create_dir_all(&output).unwrap();
    std::fs::write(
        std::path::Path::new(&output).join("linkshownn-poster-preview.png"),
        crate::render::compose(&card, &assets).unwrap(),
    )
    .unwrap();
}
