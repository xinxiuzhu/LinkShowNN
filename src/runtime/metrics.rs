//! 运行统计：`/url状态` 的数据来源。
//!
//! 之前这些计数器散在插件状态结构里，递增语句混在业务分支中间。收进本模块后
//! 每次计数都是一次有名字的调用，排查下载链路时能对照到具体环节。

use crate::download::SkipReason;

#[derive(Debug, Clone, Default)]
pub struct Metrics {
    pub attempts: u64,
    pub successes: u64,
    pub failures: u64,
    pub blocked: u64,
    pub candidates: u64,
    pub queued: u64,
    pub completed: u64,
    pub failed: u64,
    pub skipped: u64,
    pub skipped_duration: u64,
    pub skipped_stream: u64,
    /// 最近一次下载错误，用于在状态里直接看到失败原因而不必翻日志。
    pub last_error: Option<String>,
}

impl Metrics {
    pub fn attempt(&mut self) {
        self.attempts += 1;
    }

    pub fn success(&mut self) {
        self.successes += 1;
    }

    pub fn failure(&mut self) {
        self.failures += 1;
    }

    pub fn block(&mut self) {
        self.blocked += 1;
    }

    pub fn candidate(&mut self) {
        self.candidates += 1;
    }

    pub fn queue(&mut self) {
        self.queued += 1;
    }

    pub fn complete(&mut self) {
        self.completed += 1;
    }

    /// 记录失败并保留原因。错误文本可能很长，截断后再存。
    pub fn fail(&mut self, error: impl std::fmt::Display) {
        self.failed += 1;
        let text = error.to_string();
        self.last_error = Some(text.chars().take(200).collect());
    }

    /// 记录一次跳过。有明确原因时同时累加分类计数。
    pub fn skip(&mut self, reason: Option<&SkipReason>) {
        self.skipped += 1;
        match reason {
            Some(SkipReason::DurationExceeded { .. }) => self.skipped_duration += 1,
            Some(SkipReason::NoPlayableStream) => self.skipped_stream += 1,
            None => {}
        }
    }

    /// `/url状态` 的展示文本。
    pub fn report(&self, media_download: bool, concurrent_limit: usize, cooldown: u64) -> String {
        let mut text = format!(
            "LinkShowNN 状态\n\
             解析尝试: {} 成功: {} 失败: {} 屏蔽: {}\n\
             媒体下载: {}\n\
             下载候选: {} 入队: {} 完成: {} 失败: {} 跳过: {}（时长: {}，流: {}）\n\
             并发上限: {concurrent_limit} 冷却: {cooldown}秒",
            self.attempts,
            self.successes,
            self.failures,
            self.blocked,
            if media_download { "开启" } else { "关闭" },
            self.candidates,
            self.queued,
            self.completed,
            self.failed,
            self.skipped,
            self.skipped_duration,
            self.skipped_stream,
        );
        if let Some(error) = &self.last_error {
            text.push_str(&format!("\n最近下载错误: {error}"));
        }
        text
    }
}

#[cfg(test)]
mod tests {
    use super::Metrics;
    use crate::download::SkipReason;

    #[test]
    fn skip_classifies_by_reason() {
        let mut metrics = Metrics::default();
        metrics.skip(Some(&SkipReason::NoPlayableStream));
        metrics.skip(Some(&SkipReason::DurationExceeded {
            duration: 900,
            maximum: 480,
        }));
        metrics.skip(None);
        assert_eq!(metrics.skipped, 3);
        assert_eq!(metrics.skipped_stream, 1);
        assert_eq!(metrics.skipped_duration, 1);
    }

    #[test]
    fn report_omits_error_line_until_a_failure_happens() {
        let mut metrics = Metrics::default();
        assert!(!metrics.report(true, 30, 5).contains("最近下载错误"));
        metrics.fail("non-standard URL port is not allowed");
        let report = metrics.report(true, 30, 5);
        assert!(report.contains("最近下载错误: non-standard URL port is not allowed"));
        assert_eq!(metrics.failed, 1);
    }

    #[test]
    fn long_error_text_is_truncated() {
        let mut metrics = Metrics::default();
        metrics.fail("错".repeat(500));
        assert_eq!(metrics.last_error.as_ref().unwrap().chars().count(), 200);
    }
}
