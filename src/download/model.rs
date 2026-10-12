//! 下载意图：平台声明「这个能下」，下载层决定怎么下。
//!
//! 平台不碰并发、临时文件和 FFmpeg，只描述地址、文件名和访问该 CDN 需要的
//! Referer。

use std::path::PathBuf;

/// 下载产物：媒体本体，加上尽力而为的本地缩略图。
#[derive(Debug, Clone)]
pub struct Downloaded {
    pub path: PathBuf,
    pub thumb: Option<PathBuf>,
}

/// 下载媒体的业务种类；消息协议编码由投递模块负责。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaKind {
    Video,
    Audio,
}

/// 一次待下载的媒体。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DownloadCandidate {
    pub url: String,
    /// 同一媒体的备用源；下载失败才按顺序尝试，最多保留两条。
    pub fallback_urls: Vec<String>,
    /// DASH 分流时的音轨地址，存在则需要与视频合并。
    pub audio_url: Option<String>,
    pub filename: String,
    pub kind: MediaKind,
    pub cover_url: Option<String>,
    /// 媒体 CDN 会拒绝跨站 Referer，所以候选自带其 URL 所属来源站点。
    pub referer: String,
}

impl DownloadCandidate {
    /// 单流视频（progressive MP4）。
    pub fn video(
        url: impl Into<String>,
        filename: impl Into<String>,
        referer: impl Into<String>,
    ) -> Self {
        Self {
            url: url.into(),
            fallback_urls: Vec::new(),
            audio_url: None,
            filename: filename.into(),
            kind: MediaKind::Video,
            cover_url: None,
            referer: referer.into(),
        }
    }

    /// 单流音频。
    pub fn audio(
        url: impl Into<String>,
        filename: impl Into<String>,
        referer: impl Into<String>,
    ) -> Self {
        Self {
            kind: MediaKind::Audio,
            ..Self::video(url, filename, referer)
        }
    }

    /// 声明音轨分离，下载后需要封装成单文件。
    pub fn with_audio(mut self, audio_url: impl Into<String>) -> Self {
        self.audio_url = Some(audio_url.into());
        self
    }

    /// 备用地址不改变媒体类型、音轨或来源站点；空白与重复地址不占尝试预算。
    pub fn with_fallback_urls(mut self, urls: impl IntoIterator<Item = String>) -> Self {
        self.fallback_urls.clear();
        for url in urls {
            let url = url.trim();
            if url.is_empty()
                || url == self.url.trim()
                || self.fallback_urls.iter().any(|existing| existing == url)
            {
                continue;
            }
            self.fallback_urls.push(url.to_owned());
            if self.fallback_urls.len() == 2 {
                break;
            }
        }
        self
    }

    /// 空白封面按无封面处理，免得平台侧各写一遍 trim 判断。
    pub fn with_cover(mut self, cover_url: Option<String>) -> Self {
        self.cover_url = cover_url.filter(|url| !url.trim().is_empty());
        self
    }
}

/// 有媒体但没下载的原因。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkipReason {
    DurationExceeded { duration: u64, maximum: u64 },
    NoPlayableStream,
}

impl SkipReason {
    pub fn message(&self) -> String {
        match self {
            Self::DurationExceeded { duration, maximum } => {
                format!("视频时长 {duration} 秒超过上限 {maximum} 秒，已跳过下载，仅发送信息卡片。")
            }
            Self::NoPlayableStream => {
                "未获取到可用的音视频流，已跳过下载，仅发送信息卡片。".to_string()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{DownloadCandidate, SkipReason};

    #[test]
    fn blank_cover_is_treated_as_absent_and_audio_drives_mux() {
        let candidate =
            DownloadCandidate::video("https://cdn.example/v.m4s", "v.mp4", "https://e/")
                .with_cover(Some("   ".into()));
        assert!(candidate.cover_url.is_none());
        // 下载器按 `audio_url` 是否存在决定要不要走 FFmpeg 合并。
        assert!(candidate.audio_url.is_none());
        assert_eq!(
            candidate
                .with_audio("https://cdn.example/a.m4s")
                .audio_url
                .as_deref(),
            Some("https://cdn.example/a.m4s")
        );
    }

    #[test]
    fn skip_reason_messages_mention_the_limit() {
        let message = SkipReason::DurationExceeded {
            duration: 900,
            maximum: 480,
        }
        .message();
        assert!(message.contains("900"));
        assert!(message.contains("480"));
    }

    #[test]
    fn fallback_sources_are_bounded_unique_and_exclude_the_primary() {
        let candidate =
            DownloadCandidate::video("https://cdn.example/main.mp4", "v.mp4", "https://e/")
                .with_fallback_urls([
                    "  ".to_owned(),
                    "https://cdn.example/main.mp4".to_owned(),
                    " https://cdn.example/backup.mp4 ".to_owned(),
                    "https://cdn.example/backup.mp4".to_owned(),
                    "https://cdn.example/last.mp4".to_owned(),
                    "https://cdn.example/unused.mp4".to_owned(),
                ]);
        assert_eq!(
            candidate.fallback_urls,
            [
                "https://cdn.example/backup.mp4",
                "https://cdn.example/last.mp4",
            ]
        );
        assert!(
            DownloadCandidate::audio("https://cdn.example/song.mp3", "a.mp3", "https://e/")
                .fallback_urls
                .is_empty()
        );
    }
}
