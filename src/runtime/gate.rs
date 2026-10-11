//! 准入判断：黑名单、关键词、平台开关和用户冷却。
//!
//! 全是纯函数加一张时间表，不碰网络也不碰插件状态，因此可以直接对着规则写断言。

use std::collections::HashMap;
use std::time::{Duration, Instant};

use crate::{default_config::ResolverConfig, resolver::Platform};

/// 拒绝原因。调用方据此决定是静默丢弃还是计入屏蔽统计。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rejection {
    BlockedUser,
    BlockedGroup,
    BlockedKeyword,
    ResolverDisabled,
    Cooldown { remaining: u64 },
}

/// 检查静态规则：与时间无关的部分。
pub fn check(
    config: &ResolverConfig,
    platform: Platform,
    user_id: &str,
    group_id: Option<&str>,
    text: &str,
) -> Option<Rejection> {
    if config.blacklist_users.iter().any(|id| id == user_id) {
        return Some(Rejection::BlockedUser);
    }
    if let Some(group) = group_id
        && config.blacklist_groups.iter().any(|id| id == group)
    {
        return Some(Rejection::BlockedGroup);
    }
    if blocks_keyword(config, text) {
        return Some(Rejection::BlockedKeyword);
    }
    if config
        .disabled_resolvers
        .iter()
        .any(|name| name.eq_ignore_ascii_case(platform.as_str()))
    {
        return Some(Rejection::ResolverDisabled);
    }
    None
}

/// 关键词过滤。解析前查消息原文，解析后查卡片文本，两处共用这一份规则。
///
/// 空配置项会命中任意文本，所以先过滤掉。大小写敏感由配置决定。
pub fn blocks_keyword(config: &ResolverConfig, text: &str) -> bool {
    let lowered = (!config.blocked_case_sensitive).then(|| text.to_lowercase());
    config
        .blocked_keywords
        .iter()
        .filter(|word| !word.trim().is_empty())
        .any(|word| match &lowered {
            Some(text) => text.contains(&word.to_lowercase()),
            None => text.contains(word.as_str()),
        })
}

/// 用户级冷却。通过时写入本次时间，因此调用一次即消耗配额。
pub fn take_cooldown(
    last_calls: &mut HashMap<String, Instant>,
    user_id: &str,
    cooldown: u64,
    now: Instant,
) -> Option<Rejection> {
    if cooldown == 0 {
        return None;
    }
    let window = Duration::from_secs(cooldown);
    if let Some(last) = last_calls.get(user_id) {
        let elapsed = now.saturating_duration_since(*last);
        if elapsed < window {
            return Some(Rejection::Cooldown {
                remaining: (window - elapsed).as_secs() + 1,
            });
        }
    }
    last_calls.insert(user_id.to_string(), now);
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> ResolverConfig {
        ResolverConfig {
            blacklist_users: vec!["600".into()],
            blacklist_groups: vec!["900".into()],
            blocked_keywords: vec!["广告".into(), String::new()],
            disabled_resolvers: vec!["douyin".into()],
            ..ResolverConfig::default()
        }
    }

    #[test]
    fn blocks_listed_user_group_keyword_and_platform() {
        let config = config();
        assert_eq!(
            check(&config, Platform::Bilibili, "600", None, "x"),
            Some(Rejection::BlockedUser)
        );
        assert_eq!(
            check(&config, Platform::Bilibili, "1", Some("900"), "x"),
            Some(Rejection::BlockedGroup)
        );
        assert_eq!(
            check(&config, Platform::Bilibili, "1", None, "这是广告贴"),
            Some(Rejection::BlockedKeyword)
        );
        // 平台名比对忽略大小写，配置里写 douyin 也能挡住 Douyin。
        assert_eq!(
            check(&config, Platform::Douyin, "1", None, "x"),
            Some(Rejection::ResolverDisabled)
        );
        assert_eq!(check(&config, Platform::Bilibili, "1", None, "正常"), None);
    }

    #[test]
    fn keyword_case_sensitivity_follows_config() {
        let mut config = ResolverConfig {
            blocked_keywords: vec!["AD".into()],
            ..ResolverConfig::default()
        };
        assert!(blocks_keyword(&config, "an ad here"));
        config.blocked_case_sensitive = true;
        assert!(!blocks_keyword(&config, "an ad here"));
        assert!(blocks_keyword(&config, "an AD here"));
    }

    #[test]
    fn empty_keyword_entry_does_not_block_everything() {
        // 配置里留了个空字符串，若不过滤会让任何文本都命中。
        assert_eq!(
            check(&config(), Platform::Bilibili, "1", None, "任意"),
            None
        );
    }

    #[test]
    fn cooldown_blocks_within_window_and_passes_after() {
        let mut calls = HashMap::new();
        let start = Instant::now();
        assert_eq!(take_cooldown(&mut calls, "1", 10, start), None);
        assert!(matches!(
            take_cooldown(&mut calls, "1", 10, start + Duration::from_secs(3)),
            Some(Rejection::Cooldown { .. })
        ));
        assert_eq!(
            take_cooldown(&mut calls, "1", 10, start + Duration::from_secs(11)),
            None
        );
    }

    #[test]
    fn zero_cooldown_never_blocks_and_records_nothing() {
        let mut calls = HashMap::new();
        let now = Instant::now();
        assert_eq!(take_cooldown(&mut calls, "1", 0, now), None);
        assert_eq!(take_cooldown(&mut calls, "1", 0, now), None);
        assert!(calls.is_empty());
    }
}
