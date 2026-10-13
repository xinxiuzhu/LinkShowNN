use crate::{
    download::{DownloadCandidate, SkipReason},
    resolver::{Card, MediaInfo, ResolvedContent},
};

#[derive(Debug, Clone)]
pub(super) struct Playback {
    pub url: String,
    pub filename: String,
    pub quality: &'static str,
}

#[derive(Debug, Clone)]
pub(super) struct QqSong {
    pub media: MediaInfo,
    pub duration: u64,
    pub stream: Option<Playback>,
    pub notice: Option<String>,
}

impl ResolvedContent for QqSong {
    fn card(&self) -> Card {
        let duration = format!("时长：{:02}:{:02}", self.duration / 60, self.duration % 60);
        let quality = self
            .stream
            .as_ref()
            .map(|stream| format!("音质：{}", stream.quality));
        let card = self
            .media
            .card()
            .line(duration.clone())
            .line_opt(quality.clone())
            .line_opt(self.notice.clone());
        let mut metadata = card.metadata().clone();
        if let Some(poster) = &mut metadata.poster {
            // 海报与文字回退保留同一组歌曲详情，包含未取得播放授权的提示。
            let details = [
                poster.description.take(),
                Some(duration),
                quality,
                self.notice.clone(),
            ]
            .into_iter()
            .flatten()
            .filter(|line| !line.trim().is_empty())
            .collect::<Vec<_>>();
            poster.description = Some(details.join("\n"));
        }
        card.with_metadata(metadata)
    }

    fn filter_text(&self) -> String {
        self.media.filter_text()
    }

    fn download_candidate(&self, _maximum_duration: u64) -> Option<DownloadCandidate> {
        self.stream.as_ref().map(|stream| {
            DownloadCandidate::audio(
                &stream.url,
                &stream.filename,
                self.media.platform.media_referer(),
            )
            .with_cover(self.media.cover_url.clone())
        })
    }

    fn skip_reason(&self, _maximum_duration: u64) -> Option<SkipReason> {
        self.download_candidate(0)
            .is_none()
            .then_some(SkipReason::NoPlayableStream)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resolver::Platform;

    fn song() -> QqSong {
        QqSong {
            media: MediaInfo {
                title: Some("歌曲".into()),
                description: Some("专辑：专辑名称".into()),
                author: Some("歌手".into()),
                cover_url: Some("https://y.gtimg.cn/cover.jpg".into()),
                source_url: "https://y.qq.com/n/ryqq/songDetail/000song".into(),
                platform: Platform::QqMusic,
                ..Default::default()
            },
            duration: 351,
            stream: None,
            notice: None,
        }
    }

    #[test]
    fn playable_song_poster_and_text_keep_album_duration_and_audio_quality() {
        let mut song = song();
        song.stream = Some(Playback {
            url: "https://isure.stream.qqmusic.qq.com/song.mp3".into(),
            filename: "qqmusic_song.mp3".into(),
            quality: "MP3 128 kbps",
        });
        let card = song.card();
        let poster = card.metadata().poster.as_ref().unwrap();
        assert_eq!(
            poster.description.as_deref(),
            Some("专辑：专辑名称\n时长：05:51\n音质：MP3 128 kbps")
        );
        assert_eq!(
            card.text(),
            "歌曲\n专辑：专辑名称\nhttps://y.qq.com/n/ryqq/songDetail/000song\n时长：05:51\n音质：MP3 128 kbps"
        );
        assert_eq!(poster.cover_url, song.media.cover_url);
        assert!(card.metadata().video_cover_url.is_none());
        assert!(!card.is_gallery());
        // 视频时长限制不限制歌曲；已有音源也不会产生跳过说明。
        assert!(song.download_candidate(1).is_some());
        assert!(song.skip_reason(1).is_none());
    }

    #[test]
    fn unavailable_song_poster_keeps_authorization_notice() {
        let mut song = song();
        song.notice = Some("未取得播放授权；可能需要登录或会员权益。".into());
        let card = song.card();
        assert_eq!(
            card.metadata()
                .poster
                .as_ref()
                .unwrap()
                .description
                .as_deref(),
            Some("专辑：专辑名称\n时长：05:51\n未取得播放授权；可能需要登录或会员权益。")
        );
        assert!(card.text().ends_with(song.notice.as_deref().unwrap()));
        assert!(song.download_candidate(0).is_none());
        assert_eq!(song.skip_reason(0), Some(SkipReason::NoPlayableStream));
        assert!(card.metadata().avatar_url.is_none());
        assert!(card.metadata().published_at.is_none());
    }

    #[test]
    fn song_without_cover_retains_its_text_fallback() {
        let mut song = song();
        song.media.cover_url = None;
        song.notice = Some("QQ音乐播放地址暂不可用，已保留歌曲信息。".into());
        let card = song.card();
        assert!(card.metadata().poster.is_none());
        assert_eq!(
            card.text(),
            "歌曲\n专辑：专辑名称\nhttps://y.qq.com/n/ryqq/songDetail/000song\n时长：05:51\nQQ音乐播放地址暂不可用，已保留歌曲信息。"
        );
    }
}
