//! 默认配置、用户值解析与 Dashboard 字段声明。
//!
//! 插件加载时读取一次完整配置；保存新配置后重新加载插件或重启服务生效。

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use nuono_plugin::{ConfigFieldOption, PluginConfigField};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::resolver::Platform;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ResolverConfig {
    pub proxy: String,
    pub render_cards: bool,
    pub obscura_enabled: bool,
    pub obscura_timeout_seconds: u64,
    pub obscura_settle_ms: u64,
    pub video_max_mb: i64,
    pub video_duration_maximum: u64,
    pub user_cooldown: u64,
    pub concurrent_limit: usize,
    pub download_concurrent_limit: usize,
    pub download_queue_limit: usize,
    pub disabled_resolvers: Vec<String>,
    pub blacklist_users: Vec<String>,
    pub blacklist_groups: Vec<String>,
    pub blocked_keywords: Vec<String>,
    pub blocked_case_sensitive: bool,
    pub bili_cookie: String,
    pub bili_quality: String,
    pub douyin_cookie: String,
    pub douyin_anonymous_browser: bool,
    pub douyin_browser_path: String,
    pub xhs_cookie: String,
    pub ncm_api_host: String,
    pub twitter_enabled: bool,
    pub twitter_api_endpoint: String,
    pub enable_parse_miniapp: bool,
    pub enable_media_download: bool,
    pub media_output_dir: String,
    pub live_enabled: bool,
    pub live_poll_interval: u64,
    pub live_max_subscriptions: usize,
}
impl Default for ResolverConfig {
    fn default() -> Self {
        Self {
            proxy: String::new(),
            render_cards: true,
            obscura_enabled: true,
            obscura_timeout_seconds: 30,
            obscura_settle_ms: 1000,
            video_max_mb: 49,
            video_duration_maximum: 480,
            user_cooldown: 5,
            concurrent_limit: 30,
            download_concurrent_limit: 3,
            download_queue_limit: 30,
            disabled_resolvers: Vec::new(),
            blacklist_users: Vec::new(),
            blacklist_groups: Vec::new(),
            blocked_keywords: Vec::new(),
            blocked_case_sensitive: false,
            bili_cookie: String::new(),
            bili_quality: "720P".into(),
            douyin_cookie: String::new(),
            douyin_anonymous_browser: true,
            douyin_browser_path: String::new(),
            xhs_cookie: String::new(),
            ncm_api_host: "http://127.0.0.1:3000".into(),
            twitter_enabled: false,
            twitter_api_endpoint: "https://publish.twitter.com/oembed".into(),
            enable_parse_miniapp: true,
            enable_media_download: false,
            media_output_dir: String::new(),
            live_enabled: true,
            live_poll_interval: 60,
            live_max_subscriptions: 10,
        }
    }
}
impl ResolverConfig {
    pub fn from_value(mut value: Value) -> Result<Self> {
        if value.is_null() {
            return Ok(Self::default());
        }
        if let Some(object) = value.as_object_mut() {
            for key in [
                "disabled_resolvers",
                "blacklist_users",
                "blacklist_groups",
                "blocked_keywords",
            ] {
                let Some(Value::String(raw)) = object.get(key).cloned() else {
                    continue;
                };
                let parsed = serde_json::from_str::<Vec<String>>(&raw)
                    .with_context(|| format!("invalid JSON array in {key}"))?;
                object.insert(key.to_string(), serde_json::to_value(parsed)?);
            }
        }
        serde_json::from_value(value).context("invalid URL resolver config")
    }
    #[cfg(test)]
    pub fn defaults_json() -> Value {
        serde_json::to_value(Self::default()).unwrap_or_else(|_| serde_json::json!({}))
    }

    /// 媒体落盘目录。配置优先，留空则退到宿主 OneBot 读得到的位置。
    ///
    /// 插件自己的 data 目录不是安全默认值：macOS 上 QQ 跑在 App Sandbox 里，
    /// 除了 `~/Downloads` 这类有内建例外的目录，其余绝对路径都会被 EPERM 拒绝，
    /// 表现为下载成功但 `send_msg` 报 `EPERM: operation not permitted`。
    pub fn resolve_media_dir(&self, data_dir: &Path) -> PathBuf {
        match self.media_output_dir.trim() {
            "" => sandbox_readable_dir().unwrap_or_else(|| data_dir.join("media")),
            custom => PathBuf::from(custom),
        }
    }
}

/// 宿主进程沙箱化时仍可读的目录，找不到就返回 `None` 由调用方回退。
///
/// 只在 macOS 生效：Linux 和 Windows 的 OneBot 实现通常不受这类文件系统限制，
/// 沿用插件 data 目录即可。
fn sandbox_readable_dir() -> Option<PathBuf> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    let home = std::env::var_os("HOME")?;
    let candidate = Path::new(&home).join("Downloads");
    candidate.is_dir().then(|| candidate.join("LinkShowNN"))
}

impl ResolverConfig {
    pub fn fields() -> Vec<PluginConfigField> {
        let section = |field: PluginConfigField, name: &str| {
            let order = match field.key.as_str() {
                "enable_media_download"
                | "proxy"
                | "user_cooldown"
                | "disabled_resolvers"
                | "blacklist_users"
                | "live_enabled"
                | "twitter_enabled" => 10,
                "media_output_dir"
                | "enable_parse_miniapp"
                | "concurrent_limit"
                | "blacklist_groups"
                | "bili_quality"
                | "twitter_api_endpoint"
                | "live_poll_interval" => 20,
                "video_max_mb" | "blocked_keywords" | "live_max_subscriptions" => 30,
                "video_duration_maximum" | "blocked_case_sensitive" => 40,
                "obscura_timeout_seconds" => 20,
                "obscura_settle_ms" => 30,
                "download_concurrent_limit" => 50,
                "download_queue_limit" => 60,
                _ => 10,
            };
            field.with_section(name).with_order(
                order
                    + match name {
                        "通用" => 100,
                        "限制" => 200,
                        "过滤" => 300,
                        "平台" => 400,
                        "下载" => 500,
                        "直播订阅" => 600,
                        "Bilibili" => 700,
                        "抖音" => 800,
                        "小红书" => 900,
                        "网易云" => 1000,
                        "Twitter/X" => 1100,
                        "网页解析" => 1200,
                        _ => 1200,
                    },
            )
        };
        vec![
            section(
                PluginConfigField::switch(
                    "render_cards", "图文卡片", "将作者、正文和图片合成一张图片；生成失败时回退为原图文消息", true,
                ),
                "通用",
            ),
            section(
                PluginConfigField::text("proxy", "代理地址", "HTTP/HTTPS 代理；留空时 HTTP/API 直连，Obscura 使用环境代理", ""),
                "通用",
            ),
            section(
                PluginConfigField::switch(
                    "obscura_enabled",
                    "使用 Obscura",
                    "执行小红书、快手、QQ 频道分享页的 JavaScript；关闭可对照原 HTTP 解析。抖音匿名浏览器单独设置",
                    true,
                ),
                "网页解析",
            ),
            section(
                PluginConfigField::number(
                    "obscura_timeout_seconds", "网页解析超时", "包含浏览器排队和页面执行时间", 30,
                )
                .with_min(1)
                .with_max(120)
                .with_unit("秒")
                .with_visible_when("obscura_enabled", true),
                "网页解析",
            ),
            section(
                PluginConfigField::number(
                    "obscura_settle_ms", "页面加载后等待", "等待异步脚本填充数据；应小于网页解析超时", 1000,
                )
                .with_min(0)
                .with_max(10000)
                .with_unit("毫秒")
                .with_visible_when("obscura_enabled", true),
                "网页解析",
            ),
            section(
                PluginConfigField::number(
                    "video_max_mb",
                    "媒体最大体积",
                    "下载媒体体积上限，-1 不限制",
                    49,
                )
                .with_min(-1)
                .with_max(500)
                .with_unit("MB")
                .with_visible_when("enable_media_download", true),
                "下载",
            ),
            section(
                PluginConfigField::number(
                    "video_duration_maximum",
                    "视频最大时长",
                    "超过时长不下载",
                    480,
                )
                .with_min(1)
                .with_max(3600)
                .with_unit("秒")
                .with_visible_when("enable_media_download", true),
                "下载",
            ),
            section(
                PluginConfigField::number("user_cooldown", "用户冷却", "连续解析最小间隔", 5)
                    .with_min(0)
                    .with_max(60)
                    .with_unit("秒"),
                "限制",
            ),
            section(
                PluginConfigField::number(
                    "concurrent_limit",
                    "最大并发解析",
                    "全局网络解析并发",
                    30,
                )
                .with_min(1)
                .with_max(100)
                .with_unit("个"),
                "限制",
            ),
            section(
                PluginConfigField::number(
                    "download_concurrent_limit",
                    "最大并发下载",
                    "后台媒体下载并发",
                    3,
                )
                .with_min(1)
                .with_max(10)
                .with_unit("个")
                .with_visible_when("enable_media_download", true),
                "下载",
            ),
            section(
                PluginConfigField::number(
                    "download_queue_limit",
                    "下载队列上限",
                    "队列满时只发解析结果",
                    30,
                )
                .with_min(1)
                .with_max(200)
                .with_unit("个")
                .with_visible_when("enable_media_download", true),
                "下载",
            ),
            section(
                PluginConfigField::json_string_list(
                    "disabled_resolvers",
                    "禁用解析器",
                    "选择不处理链接的平台",
                    "[]",
                )
                .with_options(resolver_options())
                .with_custom_options()
                .with_editor("multiselect"),
                "平台",
            ),
            section(
                PluginConfigField::json_string_list(
                    "blacklist_users",
                    "黑名单用户",
                    "不处理这些用户发送的链接，每项填写一个用户 ID",
                    "[]",
                ),
                "过滤",
            ),
            section(
                PluginConfigField::json_string_list(
                    "blacklist_groups",
                    "黑名单群组",
                    "不处理这些群组中的链接，每项填写一个群号",
                    "[]",
                ),
                "过滤",
            ),
            section(
                PluginConfigField::json_string_list(
                    "blocked_keywords",
                    "屏蔽关键词",
                    "消息含任一关键词时跳过解析",
                    "[]",
                ),
                "过滤",
            ),
            section(
                PluginConfigField::switch(
                    "blocked_case_sensitive",
                    "屏蔽词区分大小写",
                    "默认不区分大小写",
                    false,
                ),
                "过滤",
            ),
            section(
                PluginConfigField::password("bili_cookie", "B站 Cookie", "可选认证 Cookie"),
                "Bilibili",
            ),
            section(
                PluginConfigField::select(
                    "bili_quality",
                    "B站画质",
                    "DASH 首选画质",
                    "720P",
                    vec!["1080P", "720P", "480P", "360P"]
                        .into_iter()
                        .map(|v| ConfigFieldOption {
                            label: v.into(),
                            value: v.into(),
                        })
                        .collect(),
                ),
                "Bilibili",
            ),
            section(
                PluginConfigField::password("douyin_cookie", "抖音 Cookie", "可选网页 Cookie"),
                "抖音",
            ),
            section(
                PluginConfigField::switch(
                    "douyin_anonymous_browser", "抖音匿名浏览器", "接口无法读取作品时，使用本机 Chrome/Edge 独立临时会话读取公开文案、封面和作者；无需登录。目前支持 macOS/Linux", true,
                ),
                "抖音",
            ),
            section(
                PluginConfigField::text(
                    "douyin_browser_path", "浏览器程序路径", "留空自动查找本机 Chrome/Edge/Chromium；不会自动下载安装或读取已有浏览器资料", "",
                ).with_visible_when("douyin_anonymous_browser", true),
                "抖音",
            ),
            section(
                PluginConfigField::password("xhs_cookie", "小红书 Cookie", "可选网页 Cookie"),
                "小红书",
            ),
            section(
                PluginConfigField::text(
                    "ncm_api_host",
                    "网易云 API",
                    "NeteaseCloudMusicApi 地址",
                    "http://127.0.0.1:3000",
                ),
                "网易云",
            ),
            section(
                PluginConfigField::switch(
                    "twitter_enabled",
                    "启用 Twitter/X",
                    "通过配置的接口获取链接信息",
                    false,
                ),
                "Twitter/X",
            ),
            section(
                PluginConfigField::text(
                    "twitter_api_endpoint",
                    "Twitter 接口地址",
                    "兼容 OEmbed 的接口地址",
                    "https://publish.twitter.com/oembed",
                )
                .with_visible_when("twitter_enabled", true),
                "Twitter/X",
            ),
            section(
                PluginConfigField::switch(
                    "enable_parse_miniapp",
                    "解析小程序卡片",
                    "从 QQ 小程序卡片提取链接",
                    true,
                ),
                "通用",
            ),
            section(
                PluginConfigField::switch(
                    "enable_media_download",
                    "后台下载媒体",
                    "默认关闭；开启后额外下载并发送视频或音频，图文卡片不受影响",
                    false,
                ),
                "下载",
            ),
            section(
                PluginConfigField::text(
                    "media_output_dir",
                    "媒体输出目录",
                    "留空自动选宿主 OneBot 读得到的目录（macOS 为 ~/Downloads/LinkShowNN）；OneBot 在容器或别的机器上时，填一个它也能读到的绝对路径",
                    "",
                )
                .with_visible_when("enable_media_download", true),
                "下载",
            ),
            section(
                PluginConfigField::switch(
                    "live_enabled",
                    "启用直播订阅",
                    "轮询 B 站直播状态并推送",
                    true,
                ),
                "直播订阅",
            ),
            section(
                PluginConfigField::number(
                    "live_poll_interval",
                    "直播轮询间隔",
                    "最低 60 秒",
                    60,
                )
                .with_min(60)
                .with_max(3600)
                .with_unit("秒")
                .with_visible_when("live_enabled", true),
                "直播订阅",
            ),
            section(
                PluginConfigField::number(
                    "live_max_subscriptions",
                    "直播订阅上限",
                    "单实例最大订阅数",
                    10,
                )
                .with_min(1)
                .with_max(50)
                .with_unit("个")
                .with_visible_when("live_enabled", true),
                "直播订阅",
            ),
        ]
    }
}

fn resolver_options() -> Vec<ConfigFieldOption> {
    Platform::all()
        .iter()
        .map(|platform| ConfigFieldOption {
            label: platform.display_name().into(),
            value: platform.as_str().into(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{Path, Platform, ResolverConfig, resolver_options, sandbox_readable_dir};
    use serde_json::{Value, json};

    #[test]
    fn schema_defaults_round_trip_without_changing_array_storage() {
        let defaults = Value::Object(
            ResolverConfig::fields()
                .into_iter()
                .map(|field| (field.key, field.default))
                .collect(),
        );
        assert_eq!(
            defaults.as_object().unwrap().len(),
            ResolverConfig::defaults_json().as_object().unwrap().len()
        );
        for key in [
            "disabled_resolvers",
            "blacklist_users",
            "blacklist_groups",
            "blocked_keywords",
        ] {
            assert!(defaults[key].is_string());
        }
        let parsed = ResolverConfig::from_value(defaults).unwrap();
        assert_eq!(
            serde_json::to_value(parsed).unwrap(),
            ResolverConfig::defaults_json()
        );
    }

    #[test]
    fn disabled_resolver_choices_follow_the_registered_platforms() {
        let options = resolver_options();
        assert_eq!(
            options
                .iter()
                .map(|option| option.value.as_str())
                .collect::<Vec<_>>(),
            Platform::all()
                .iter()
                .map(|platform| platform.as_str())
                .collect::<Vec<_>>(),
        );
        let field = ResolverConfig::fields()
            .into_iter()
            .find(|field| field.key == "disabled_resolvers")
            .unwrap();
        assert_eq!(field.ui.editor.as_deref(), Some("multiselect"));
        assert_eq!(field.ui.encoding.as_deref(), Some("json_string"));
        assert!(field.ui.allow_custom);
        assert_eq!(field.options.as_ref().unwrap().len(), options.len());
        let config = ResolverConfig::from_value(json!({
            "disabled_resolvers": serde_json::to_string(&options.iter().map(|option| &option.value).collect::<Vec<_>>()).unwrap(),
            "blacklist_users": "[\"1001\",\"1002\"]",
            "blacklist_groups": "[\"2001\"]",
            "blocked_keywords": "[\"中文关键词\",\"spoiler\"]",
        })).unwrap();
        let native_value = serde_json::to_value(config).unwrap();
        assert_eq!(native_value["blacklist_users"], json!(["1001", "1002"]));
        assert_eq!(
            native_value["blocked_keywords"],
            json!(["中文关键词", "spoiler"])
        );
        let reparsed = ResolverConfig::from_value(native_value.clone()).unwrap();
        assert_eq!(serde_json::to_value(reparsed).unwrap(), native_value);
    }

    #[test]
    fn accepts_legacy_json_string_arrays() {
        let config = ResolverConfig::from_value(json!({
            "disabled_resolvers": "[\"twitter\"]",
            "blacklist_users": "[\"1001\"]",
            "blacklist_groups": "[]",
            "blocked_keywords": "[\"spoiler\"]"
        }))
        .unwrap();

        assert_eq!(config.disabled_resolvers, vec!["twitter"]);
        assert_eq!(config.blacklist_users, vec!["1001"]);
        assert_eq!(config.blacklist_groups, Vec::<String>::new());
        assert_eq!(config.blocked_keywords, vec!["spoiler"]);
    }

    #[test]
    fn retains_legacy_resolver_names_without_rewriting_their_case() {
        let config = ResolverConfig::from_value(json!({
            "disabled_resolvers": "[\"Twitter\",\"future_platform\"]"
        }))
        .unwrap();
        assert_eq!(
            config.disabled_resolvers,
            vec!["Twitter", "future_platform"]
        );
    }

    #[test]
    fn media_output_dir_defaults_to_empty_and_accepts_override() {
        assert_eq!(ResolverConfig::default().media_output_dir, "");

        let config = ResolverConfig::from_value(json!({
            "media_output_dir": "/tmp/linkshownn-media"
        }))
        .unwrap();

        assert_eq!(config.media_output_dir, "/tmp/linkshownn-media");
    }

    #[test]
    fn configured_media_dir_wins_over_sandbox_fallback() {
        let config = ResolverConfig {
            media_output_dir: "  /srv/onebot/shared  ".into(),
            ..ResolverConfig::default()
        };
        assert_eq!(
            config.resolve_media_dir(Path::new("/plugin/data")),
            Path::new("/srv/onebot/shared")
        );
    }

    #[test]
    fn empty_media_dir_avoids_plugin_data_dir_when_sandbox_dir_exists() {
        // macOS 上 QQ 的沙箱读不到插件 data 目录，所以留空时不能退到那里。
        let resolved = ResolverConfig::default().resolve_media_dir(Path::new("/plugin/data"));
        match sandbox_readable_dir() {
            Some(expected) => assert_eq!(resolved, expected),
            None => assert_eq!(resolved, Path::new("/plugin/data/media")),
        }
    }
}
