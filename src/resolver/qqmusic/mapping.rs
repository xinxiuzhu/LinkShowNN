use anyhow::{Context, Result, ensure};
use serde_json::Value;
use url::Url;

use super::{
    content::{Playback, QqSong},
    urls::valid_mid,
};
use crate::resolver::{MediaInfo, Platform};

pub(super) fn map_track(track: &Value, source: &Url) -> Result<QqSong> {
    let mid = track
        .get("mid")
        .and_then(Value::as_str)
        .context("QQ音乐缺少歌曲 MID")?;
    ensure!(valid_mid(mid), "QQ音乐歌曲 MID 无效");
    let title = track
        .get("name")
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .context("QQ音乐缺少歌名")?;
    let album = track.get("album");
    let album_name = album
        .and_then(|a| a.get("name"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let cover_mid = album
        .and_then(|a| a.get("pmid").or_else(|| a.get("mid")))
        .and_then(Value::as_str)
        .filter(|mid| {
            mid.len() <= 64 && mid.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_')
        });
    let artists = track
        .get("singer")
        .and_then(Value::as_array)
        .map(|singers| {
            singers
                .iter()
                .filter_map(|s| s.get("name").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join(" / ")
        })
        .unwrap_or_default();
    Ok(QqSong {
        media: MediaInfo {
            title: Some(title.into()),
            description: (!album_name.is_empty()).then(|| format!("专辑：{album_name}")),
            author: (!artists.is_empty()).then_some(artists),
            cover_url: cover_mid
                .map(|mid| format!("https://y.gtimg.cn/music/photo_new/T002R500x500M000{mid}.jpg")),
            source_url: source.to_string(),
            platform: Platform::QqMusic,
            raw: serde_json::json!({"platform":"qqmusic", "song_mid":mid}),
            ..Default::default()
        },
        duration: track
            .get("interval")
            .and_then(Value::as_u64)
            .unwrap_or_default(),
        stream: None,
        notice: None,
    })
}

/// 只接受请求歌曲的完整文件授权，不把 testfilewifi、opi30surl 或试听切片当成全曲。
pub(super) fn playback_from_response(data: &Value, mid: &str, media_mid: &str) -> Option<Playback> {
    for (prefix, extension, quality) in [
        ("M500", "mp3", "MP3 128 kbps"),
        ("C400", "m4a", "AAC 96 kbps"),
    ] {
        let expected = format!("{prefix}{media_mid}.{extension}");
        let item = data.get("midurlinfo")?.as_array()?.iter().find(|item| {
            item.get("songmid").and_then(Value::as_str) == Some(mid)
                && item.get("filename").and_then(Value::as_str) == Some(expected.as_str())
                && item.get("result").and_then(Value::as_i64) == Some(0)
        });
        let Some(item) = item else { continue };
        let Some(purl) = item
            .get("purl")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        else {
            continue;
        };
        // 固定官方 CDN，禁止把响应里的外站 URL、协议相对链接或路径穿越拼入请求。
        let (path, _) = purl.split_once('?').unwrap_or((purl, ""));
        if path != expected || purl.len() > 8192 || purl.chars().any(char::is_control) {
            continue;
        }
        return Some(Playback {
            url: format!("https://isure.stream.qqmusic.qq.com/{purl}"),
            filename: format!("qqmusic_{mid}.{extension}"),
            quality,
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resolver::ResolvedContent;

    #[test]
    fn paid_song_remains_a_useful_card_without_audio() {
        let track = serde_json::json!({"mid":"000SO0rc20bCpb","name":"Tamada","interval":341,"pay":{"pay_play":1},"album":{"name":"We Won't Be Alone","pmid":"001IBgJH2ogkJ2_2"},"singer":[{"name":"Primo Pecora"}]});
        let song = map_track(
            &track,
            &Url::parse("https://y.qq.com/n/ryqq_v2/songDetail/575658805").unwrap(),
        )
        .unwrap();
        assert!(song.card().text().contains("Tamada"));
        assert_eq!(song.media.author.as_deref(), Some("Primo Pecora"));
        assert!(
            song.media
                .cover_url
                .as_ref()
                .unwrap()
                .contains("001IBgJH2ogkJ2_2")
        );
        assert!(song.download_candidate(0).is_none());
    }

    #[test]
    fn rejects_test_audio_preview_mismatched_song_and_arbitrary_urls() {
        let mid = "000SO0rc20bCpb";
        let media_mid = "001YdZON3ouotM";
        let mut data = serde_json::json!({"testfilewifi":"M500irrelevant.mp3?vkey=x","midurlinfo":[{"songmid":mid,"filename":"M500001YdZON3ouotM.mp3","result":104003,"purl":"","opi30surl":"RS01001YdZON3ouotM.mp3?vkey=x"}]});
        assert!(playback_from_response(&data, mid, media_mid).is_none());
        data["midurlinfo"][0]["result"] = 0.into();
        for bad in [
            "https://evil.test/audio.mp3",
            "//evil.test/a",
            "../a.mp3",
            "RS01001YdZON3ouotM.mp3?vkey=x",
        ] {
            data["midurlinfo"][0]["purl"] = bad.into();
            assert!(playback_from_response(&data, mid, media_mid).is_none());
        }
        data["midurlinfo"][0]["purl"] = "M500001YdZON3ouotM.mp3?vkey=sample".into();
        assert!(playback_from_response(&data, "002d5NTY0FgLNi", media_mid).is_none());
        let stream = playback_from_response(&data, mid, media_mid).unwrap();
        assert!(
            stream
                .url
                .starts_with("https://isure.stream.qqmusic.qq.com/")
        );
        assert!(stream.filename.ends_with(".mp3"));
    }
}
