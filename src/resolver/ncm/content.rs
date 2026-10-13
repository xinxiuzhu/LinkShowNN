use serde_json::Value;
use url::Url;

use crate::{
    download::{DownloadCandidate, SkipReason},
    resolver::{Card, MediaInfo, Platform, ResolvedContent},
};

#[derive(Debug)]
pub(super) struct NcmSong {
    pub(super) media: MediaInfo,
    filename: String,
}

impl ResolvedContent for NcmSong {
    fn card(&self) -> Card {
        self.media.card()
    }
    fn filter_text(&self) -> String {
        self.media.filter_text()
    }
    fn download_candidate(&self, _maximum_duration: u64) -> Option<DownloadCandidate> {
        self.media
            .audio_url
            .as_ref()
            .map(|url| DownloadCandidate::audio(url, &self.filename, Platform::Ncm.media_referer()))
    }

    fn skip_reason(&self, _maximum_duration: u64) -> Option<SkipReason> {
        self.download_candidate(0)
            .is_none()
            .then_some(SkipReason::NoPlayableStream)
    }
}

pub(super) fn map_song(song: &Value, audio: Option<&Value>, source: &Url) -> NcmSong {
    let artists = song
        .get("ar")
        .or_else(|| song.get("artists"))
        .and_then(Value::as_array);
    let author = artists
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.get("name").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join(" / ")
        })
        .filter(|name| !name.is_empty());
    let album = song.get("al").or_else(|| song.get("album"));
    let id = song.get("id").and_then(Value::as_u64).unwrap_or(0);
    let playable = audio
        .and_then(|body| body.get("data"))
        .and_then(Value::as_array)
        .and_then(|items| {
            items
                .iter()
                .find(|item| item.get("id").and_then(Value::as_u64) == Some(id))
        });
    let audio_url = playable
        .and_then(|item| item.get("url"))
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .filter(|value| {
            Url::parse(value)
                .ok()
                .is_some_and(|url| crate::resolver::url_policy::validate_public_url(&url).is_ok())
        })
        .map(str::to_owned);
    let mut details = Vec::new();
    if let Some(name) = album
        .and_then(|album| album.get("name"))
        .and_then(Value::as_str)
    {
        details.push(format!("专辑：{name}"));
    }
    let song_duration = song
        .get("dt")
        .or_else(|| song.get("duration"))
        .and_then(Value::as_u64);
    if let Some(duration) = song_duration {
        details.push(format!("歌曲时长：{}", duration_text(duration)));
    }
    let audio_duration = playable
        .and_then(|item| item.get("time"))
        .and_then(Value::as_u64);
    let trial = playable
        .and_then(|item| item.get("freeTrialInfo"))
        .filter(|value| !value.is_null() && value.as_str() != Some("null"));
    // 某些播放接口未提供试听标记。只在已取得音源且其有效时长超过一秒、
    // 不超过原曲一半、缺口也超过一秒时保守标注片段，避免误判编码尾差。
    let shortened = audio_url.is_some()
        && audio_duration
            .zip(song_duration)
            .is_some_and(|(audio, song)| {
                audio > 1000 && audio <= song / 2 && song.saturating_sub(audio) > 1000
            });
    if let Some(trial) = trial {
        let duration = trial
            .get("end")
            .and_then(Value::as_u64)
            .zip(trial.get("start").and_then(Value::as_u64))
            .and_then(|(end, start)| end.checked_sub(start))
            .and_then(|seconds| seconds.checked_mul(1000))
            .or(audio_duration);
        details.push(match duration {
            Some(duration) => format!(
                "仅试听：{}；完整播放需要账号具备相应权益",
                duration_text(duration)
            ),
            None => "仅试听；完整播放需要账号具备相应权益".to_owned(),
        });
    } else if shortened {
        details.push(format!(
            "音源片段：{}（可能为试听）；与歌曲完整时长不符",
            duration_text(audio_duration.expect("shortened source requires duration"))
        ));
    } else if audio_url.is_none() {
        details.push("暂未取得可用音源；可能需要登录、会员或歌曲播放权益".to_owned());
    }
    let extension = playable
        .and_then(|item| item.get("type"))
        .and_then(Value::as_str)
        .filter(|value| matches!(*value, "mp3" | "flac" | "m4a" | "aac" | "ogg" | "wav"))
        .unwrap_or("mp3");
    NcmSong {
        media: MediaInfo {
            title: song.get("name").and_then(Value::as_str).map(str::to_owned),
            description: (!details.is_empty()).then(|| details.join("\n")),
            cover_url: album
                .and_then(|album| album.get("picUrl"))
                .and_then(Value::as_str)
                .map(str::to_owned),
            audio_url,
            author,
            source_url: source.to_string(),
            platform: Platform::Ncm,
            // 只保存公开歌曲元信息，不保留音源临时签名或登录状态。
            raw: serde_json::json!({"platform": "ncm", "song": song}),
            ..Default::default()
        },
        filename: format!(
            "ncm_{id}{}.{extension}",
            if trial.is_some() || shortened {
                "_preview"
            } else {
                ""
            }
        ),
    }
}

fn duration_text(milliseconds: u64) -> String {
    let seconds = milliseconds / 1000;
    format!("{:02}:{:02}", seconds / 60, seconds % 60)
}
