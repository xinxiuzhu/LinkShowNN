//! 纯离线模型测试，不使用账号、数据库或平台接口。

use std::time::Duration;

use image::{ExtendedColorType, ImageEncoder, codecs::png::PngEncoder};
use serde_json::json;

use super::*;

const SECRET: &str = "fixture-music-private-token";

fn ncm() -> MusicCredentials {
    MusicCredentials::new(
        MusicPlatform::Ncm,
        format!("MUSIC_U={SECRET}; __csrf=fixture-csrf"),
        "123456".into(),
        None,
    )
    .unwrap()
}

#[test]
fn credentials_roundtrip_is_versioned_and_platform_specific() {
    let account = ncm();
    let encoded = account.encode().unwrap();
    assert!(MusicCredentials::decode(MusicPlatform::Ncm, &encoded).unwrap() == account);
    assert!(MusicCredentials::decode(MusicPlatform::QqMusic, &encoded).is_err());
    let mut stored: serde_json::Value = serde_json::from_str(&encoded).unwrap();
    stored["version"] = json!(2);
    assert!(MusicCredentials::decode(MusicPlatform::Ncm, &stored.to_string()).is_err());
    stored["version"] = json!(1);
    stored["unexpected"] = json!(SECRET);
    let error = MusicCredentials::decode(MusicPlatform::Ncm, &stored.to_string())
        .err()
        .unwrap();
    assert!(!format!("{error:?}").contains(SECRET));
    assert!(MusicCredentials::decode(MusicPlatform::Ncm, &"x".repeat(17 * 1024)).is_err());
}

#[test]
fn cookie_normalization_keeps_equals_in_values_and_accepts_final_semicolon() {
    let account = MusicCredentials::new(
        MusicPlatform::QqMusic,
        format!("  qqmusic_key={SECRET}==; qqmusic_uin=123456; "),
        "123456".into(),
        Some(2_000_000_000),
    )
    .unwrap();
    assert_eq!(
        account.cookie_header(),
        format!("qqmusic_key={SECRET}==; qqmusic_uin=123456")
    );
    assert_eq!(account.user_id(), "123456");
    assert_eq!(account.expires_at(), Some(2_000_000_000));
}

#[test]
fn cookie_injection_duplicates_missing_required_tokens_and_size_are_rejected() {
    for cookie in [
        format!("MUSIC_U={SECRET}\r\nAuthorization: injected"),
        format!("MUSIC_U={SECRET}; MUSIC_U=duplicate"),
        format!("MUSIC_U={SECRET}; malformed"),
        format!("MUSIC_U={SECRET}; bad name=value"),
        format!("MUSIC_U={SECRET}; unsafe=value with space"),
        format!("MUSIC_U={SECRET}; unsafe=\"quoted\""),
        format!("MUSIC_U={SECRET};; another=value"),
        format!("MUSIC_U={SECRET}; x="),
        format!("MUSIC_U={}", "x".repeat(4097)),
        format!("OTHER={SECRET}"),
        format!(
            "MUSIC_U={SECRET}; {}",
            (0..65)
                .map(|index| format!("k{index}=v"))
                .collect::<Vec<_>>()
                .join("; ")
        ),
    ] {
        let error = MusicCredentials::new(MusicPlatform::Ncm, cookie, "123456".into(), None)
            .err()
            .unwrap();
        assert!(!format!("{error:?}").contains(SECRET));
    }
    assert!(
        MusicCredentials::new(
            MusicPlatform::QqMusic,
            format!("MUSIC_U={SECRET}"),
            "123456".into(),
            None
        )
        .is_err()
    );
}

#[test]
fn account_id_and_expiration_have_bounded_valid_ranges() {
    for uid in [
        "",
        "0",
        "-1",
        "o123456",
        "12 34",
        "18446744073709551616",
        "123456789012345678901",
    ] {
        assert!(
            MusicCredentials::new(
                MusicPlatform::Ncm,
                format!("MUSIC_U={SECRET}"),
                uid.into(),
                None
            )
            .is_err()
        );
    }
    for expiry in [0, 253_402_300_800, u64::MAX] {
        assert!(
            MusicCredentials::new(
                MusicPlatform::Ncm,
                format!("MUSIC_U={SECRET}"),
                "123456".into(),
                Some(expiry)
            )
            .is_err()
        );
    }
    // Persisted expired sessions remain inspectable; the platform determines usability.
    assert!(
        MusicCredentials::new(
            MusicPlatform::Ncm,
            format!("MUSIC_U={SECRET}"),
            "123456".into(),
            Some(1)
        )
        .is_ok()
    );
}

#[test]
fn challenge_accepts_https_url_or_bounded_qr_png_without_exposing_state() {
    let url = format!("https://example.invalid/qr?fixture_key={SECRET}");
    let qr = QrChallenge::new(
        QrImage::Url(url.clone()),
        SECRET.into(),
        Duration::from_secs(60),
    )
    .unwrap();
    assert_eq!(qr.state(), SECRET);
    assert!(qr.expires_in() <= Duration::from_secs(60));
    let png = crate::auth::qr_image::render(&url).unwrap();
    assert!(QrChallenge::new(QrImage::Bytes(png), SECRET.into(), Duration::from_secs(60)).is_ok());
}

#[test]
fn challenge_rejects_insecure_urls_bad_images_and_unbounded_state_or_lifetime() {
    for url in [
        "http://example.invalid/qr",
        "https://user:pass@example.invalid/qr",
        "https://example.invalid:444/qr",
        "https://example.invalid/qr#fragment",
        "https://example.invalid/qr\n",
    ] {
        let error = QrChallenge::new(
            QrImage::Url(url.into()),
            SECRET.into(),
            Duration::from_secs(60),
        )
        .err()
        .unwrap();
        assert!(!format!("{error:?}").contains(SECRET));
    }
    for state in [
        String::new(),
        "x".repeat(16 * 1024 + 1),
        format!("{SECRET}\n"),
    ] {
        assert!(
            QrChallenge::new(
                QrImage::Url("https://example.invalid/qr".into()),
                state,
                Duration::from_secs(60)
            )
            .is_err()
        );
    }
    for ttl in [Duration::ZERO, Duration::from_secs(901)] {
        assert!(
            QrChallenge::new(
                QrImage::Url("https://example.invalid/qr".into()),
                SECRET.into(),
                ttl
            )
            .is_err()
        );
    }
    assert!(
        QrChallenge::new(
            QrImage::Bytes(vec![1, 2, 3]),
            SECRET.into(),
            Duration::from_secs(60)
        )
        .is_err()
    );
    assert!(
        QrChallenge::new(
            QrImage::Bytes(vec![0; 1024 * 1024 + 1]),
            SECRET.into(),
            Duration::from_secs(60)
        )
        .is_err()
    );
    let mut oversized_png = Vec::new();
    PngEncoder::new(&mut oversized_png)
        .write_image(&vec![255; 2049], 2049, 1, ExtendedColorType::L8)
        .unwrap();
    assert!(
        QrChallenge::new(
            QrImage::Bytes(oversized_png),
            SECRET.into(),
            Duration::from_secs(60)
        )
        .is_err()
    );
}
