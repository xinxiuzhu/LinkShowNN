//! Content-transfer tests use local fixtures; the simulated peer never reads host paths.

use super::*;

fn video() -> DownloadCandidate {
    DownloadCandidate::video(
        "https://cdn.example/video.mp4",
        "video.mp4",
        "https://example/",
    )
}

fn bytes(source: &Value) -> Vec<u8> {
    STANDARD
        .decode(source.as_str().unwrap().strip_prefix("base64://").unwrap())
        .unwrap()
}

#[tokio::test]
async fn remote_video_and_thumbnail_contain_bytes_after_host_files_are_removed() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("video.mp4");
    let thumb = directory.path().join("thumb.jpg");
    let media_bytes = b"\x00\x00\x00\x18ftypmp42video-fixture";
    let thumb_bytes = b"\xff\xd8\xffimage-fixture";
    tokio::fs::write(&path, media_bytes).await.unwrap();
    tokio::fs::write(&thumb, thumb_bytes).await.unwrap();
    let message = media_message(&video(), &path, Some(&thumb), "base64")
        .await
        .unwrap();
    tokio::fs::remove_file(path).await.unwrap();
    tokio::fs::remove_file(thumb).await.unwrap();

    let received = message.to_json();
    assert_eq!(received[0]["type"], "video");
    assert_eq!(bytes(&received[0]["data"]["file"]), media_bytes);
    assert_eq!(bytes(&received[0]["data"]["thumb"]), thumb_bytes);
    assert!(received[0]["data"].get("cover").is_none());
}

#[tokio::test]
async fn mp3_uses_record_with_content_and_never_reads_a_thumbnail() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("song.mp3");
    tokio::fs::write(&path, b"ID3audio-fixture").await.unwrap();
    let candidate = DownloadCandidate::audio(
        "https://cdn.example/song.mp3",
        "song.mp3",
        "https://example/",
    );
    let received = media_message(
        &candidate,
        &path,
        Some(Path::new("/nonexistent-thumb")),
        "base64",
    )
    .await
    .unwrap()
    .to_json();
    assert_eq!(received[0]["type"], "record");
    assert_eq!(bytes(&received[0]["data"]["file"]), b"ID3audio-fixture");
    assert!(received[0]["data"].get("thumb").is_none());
}

#[tokio::test]
async fn incremental_base64_preserves_chunk_boundaries_and_padding() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("source");
    for length in [1, 2, 3, 48 * 1024 + 1, 48 * 1024 + 2, 96 * 1024 + 3] {
        let original = (0..length)
            .map(|index| (index % 251) as u8)
            .collect::<Vec<_>>();
        tokio::fs::write(&path, &original).await.unwrap();
        let (encoded, total) = inline_file(&path, length as u64).await.unwrap();
        assert_eq!(total, length as u64);
        assert_eq!(encoded, format!("base64://{}", STANDARD.encode(original)));
    }
}

#[tokio::test]
async fn inline_transport_enforces_combined_budget_and_rejects_empty_or_missing_files() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("video");
    let thumb = directory.path().join("thumb");
    tokio::fs::write(&path, b"12345").await.unwrap();
    tokio::fs::write(&thumb, b"1234").await.unwrap();
    assert!(
        media_message_with_budget(&video(), &path, None, "base64", 4)
            .await
            .is_err()
    );
    assert!(
        media_message_with_budget(&video(), &path, Some(&thumb), "base64", 8)
            .await
            .is_err()
    );
    assert!(
        media_message_with_budget(&video(), &path, Some(&thumb), "base64", 9)
            .await
            .is_ok()
    );
    tokio::fs::write(&path, []).await.unwrap();
    assert!(
        media_message(&video(), &path, None, "base64")
            .await
            .is_err()
    );
    assert!(
        media_message(&video(), directory.path(), None, "base64")
            .await
            .is_err()
    );
    tokio::fs::remove_file(path.clone()).await.unwrap();
    assert!(
        media_message(&video(), &path, None, "base64")
            .await
            .is_err()
    );
    assert!(
        media_message(&video(), &path, None, "invalid")
            .await
            .is_err()
    );
}

#[tokio::test]
async fn shared_directory_transport_uses_valid_file_uris_and_omits_absent_thumb() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("视频 #1.mp4");
    let thumb = directory.path().join("封面 #1.jpg");
    let encoded = media_message(&video(), &path, Some(&thumb), "local")
        .await
        .unwrap()
        .to_json();
    for (key, expected) in [("file", &path), ("thumb", &thumb)] {
        let source = encoded[0]["data"][key].as_str().unwrap();
        assert_eq!(
            url::Url::parse(source).unwrap().to_file_path().unwrap(),
            expected.as_path()
        );
        assert!(!source.contains('#'));
    }
    assert!(
        media_message(&video(), Path::new("relative.mp4"), None, "local")
            .await
            .is_err()
    );
    let encoded = media_message(&video(), &path, None, "local")
        .await
        .unwrap()
        .to_json();
    assert!(encoded[0]["data"].get("thumb").is_none());
}

#[tokio::test]
#[ignore = "manual local media preparation; no platform sends"]
async fn downloaded_media_round_trip() {
    let path = std::path::PathBuf::from(
        std::env::var("LINKSHOWNN_MEDIA_TEST_PATH").expect("set a local downloaded video path"),
    );
    let thumb = path.with_extension("thumb.jpg");
    let thumb_path = thumb.is_file().then_some(thumb.as_path());
    let prepared = media_message(&video(), &path, thumb_path, "base64")
        .await
        .unwrap()
        .to_json();
    let decoded = bytes(&prepared[0]["data"]["file"]);
    assert_eq!(decoded, tokio::fs::read(&path).await.unwrap());
    let thumb_bytes = if let Some(thumb) = thumb_path {
        let decoded = bytes(&prepared[0]["data"]["thumb"]);
        assert_eq!(decoded, tokio::fs::read(thumb).await.unwrap());
        decoded.len()
    } else {
        0
    };
    println!(
        "local media prepared without platform sends: video_bytes={}, thumbnail_bytes={}, transport=base64",
        decoded.len(),
        thumb_bytes
    );
}
