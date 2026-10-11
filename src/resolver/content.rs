//! 解析结果的行为契约。
//!
//! 各站点保留自己的结果类型，通过同一接口提供展示、过滤和下载信息。

use std::sync::Arc;

use super::Card;
use crate::download::{DownloadCandidate, SkipReason};

/// 一份已解析的内容能回答的三个问题：怎么展示、要不要下载、拿什么做关键词过滤。
///
/// 实现者是各平台自己的结果类型（如 `BiliVideoContent`、`MediaInfo`），它们
/// 保留各自的原生字段，不必迁就其他平台。
pub trait ResolvedContent: Send + Sync + std::fmt::Debug {
    /// 展示卡片。纯函数，便于离线断言排版。
    fn card(&self) -> Card;

    /// 供黑名单关键词匹配的文本。默认取卡片正文，避免每个平台重复实现。
    fn filter_text(&self) -> String {
        self.card().text()
    }

    /// 可下载的媒体。`maximum_duration` 为 0 表示不限时长。
    ///
    /// 默认没有可下载媒体，纯图文平台无需实现。
    fn download_candidate(&self, _maximum_duration: u64) -> Option<DownloadCandidate> {
        None
    }

    /// 有媒体但被规则挡下时的原因，用于回执里说明为何只发卡片。
    ///
    /// 只在 [`Self::download_candidate`] 返回 `None` 时才被询问。
    fn skip_reason(&self, _maximum_duration: u64) -> Option<SkipReason> {
        None
    }
}

/// 装配层持有的解析结果句柄。
///
/// 共享句柄允许克隆结果而无需复制平台响应；消费者只访问行为契约。
#[derive(Debug, Clone)]
pub struct Resolved(Arc<dyn ResolvedContent>);

impl Resolved {
    pub fn new(content: impl ResolvedContent + 'static) -> Self {
        Self(Arc::new(content))
    }

    pub fn card(&self) -> Card {
        self.0.card()
    }

    pub fn filter_text(&self) -> String {
        self.0.filter_text()
    }

    pub fn download_candidate(&self, maximum_duration: u64) -> Option<DownloadCandidate> {
        self.0.download_candidate(maximum_duration)
    }

    /// 跳过原因只在确实没有候选时才有意义，这里统一保证该前置条件。
    pub fn skip_reason(&self, maximum_duration: u64) -> Option<SkipReason> {
        if self.download_candidate(maximum_duration).is_some() {
            return None;
        }
        self.0.skip_reason(maximum_duration)
    }
}

#[cfg(test)]
mod tests {
    use super::{Resolved, ResolvedContent};
    use crate::download::{DownloadCandidate, SkipReason};
    use crate::resolver::Card;

    #[derive(Debug)]
    struct Article;

    impl ResolvedContent for Article {
        fn card(&self) -> Card {
            Card::new().line("标题").line("正文")
        }
    }

    #[derive(Debug)]
    struct Clip;

    impl ResolvedContent for Clip {
        fn card(&self) -> Card {
            Card::new().line("片子")
        }

        fn download_candidate(&self, maximum_duration: u64) -> Option<DownloadCandidate> {
            (maximum_duration == 0 || maximum_duration >= 60).then(|| {
                DownloadCandidate::video(
                    "https://cdn.example/v.mp4",
                    "clip.mp4",
                    "https://example/",
                )
            })
        }

        fn skip_reason(&self, _maximum_duration: u64) -> Option<SkipReason> {
            Some(SkipReason::NoPlayableStream)
        }
    }

    #[test]
    fn text_only_content_has_no_download_or_skip() {
        let resolved = Resolved::new(Article);
        assert_eq!(resolved.filter_text(), "标题\n正文");
        assert!(resolved.download_candidate(0).is_none());
        assert!(resolved.skip_reason(0).is_none());
    }

    #[test]
    fn skip_reason_is_suppressed_when_candidate_exists() {
        let resolved = Resolved::new(Clip);
        assert!(resolved.download_candidate(0).is_some());
        assert!(resolved.skip_reason(0).is_none());
        assert_eq!(resolved.skip_reason(30), Some(SkipReason::NoPlayableStream));
    }
}
