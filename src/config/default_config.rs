//! 默认配置声明：类型、默认值、说明、约束和网页控件均在这里维护。
//! 网页按下面的分组和字段声明顺序显示，无需手写 order。

use nuono_plugin::{
    ConfigFieldOption,
    serde::{Deserialize, Serialize},
};

use crate::resolver::Platform;

#[derive(Debug, Clone, Serialize, Deserialize, nuono_plugin::ConfigSchema)]
#[serde(crate = "nuono_plugin::serde")]
pub struct ResolverConfig {
    // 通用配置
    #[config(
        default = true,
        label = "图文卡片",
        description = "将作者、正文和图片合成一张图片；生成失败时回退为原图文消息",
        section = "通用配置",
        widget = "switch"
    )]
    pub render_cards: bool,

    #[config(
        default = true,
        label = "评论展示",
        description = "展示可获取评论的头像、正文、表情和配图；关闭后仍保留作品评论数统计",
        section = "通用配置",
        widget = "switch"
    )]
    pub show_comments: bool,

    #[config(
        default = true,
        label = "解析小程序卡片",
        description = "从 QQ 小程序卡片提取链接",
        section = "通用配置",
        widget = "switch"
    )]
    pub enable_parse_miniapp: bool,

    #[config(
        default = "",
        label = "代理地址",
        description = "HTTP/HTTPS 代理；留空时 HTTP/API 直连",
        section = "通用配置"
    )]
    pub proxy: String,

    #[config(
        default = Vec::new(),
        label = "禁用解析器",
        description = "选择不处理链接的平台",
        section = "通用配置",
        options = resolver_options(),
        editor = "multiselect",
        encoding = "json_string",
        allow_custom,
    )]
    pub disabled_resolvers: Vec<String>,

    #[config(
        default = 5,
        label = "用户冷却",
        description = "连续解析最小间隔",
        section = "通用配置",
        min = 0,
        max = 60,
        unit = "秒"
    )]
    pub user_cooldown: u64,

    #[config(
        default = 30,
        label = "最大并发解析",
        description = "全局网络解析并发",
        section = "通用配置",
        min = 1,
        max = 100,
        unit = "个"
    )]
    pub concurrent_limit: usize,

    // 媒体下载
    #[config(
        default = false,
        label = "后台下载媒体",
        description = "默认关闭；开启后额外下载并发送视频或音频，图文卡片不受影响",
        section = "媒体下载",
        widget = "switch"
    )]
    pub enable_media_download: bool,

    #[config(
        default = "",
        label = "媒体输出目录",
        description = "留空自动选宿主 OneBot 读得到的目录（macOS 为 ~/Downloads/LinkShowNN）；OneBot 在容器或别的机器上时，填一个它也能读到的绝对路径",
        section = "媒体下载",
        visible_when(key = "enable_media_download", value = true)
    )]
    pub media_output_dir: String,

    #[config(
        default = 49,
        label = "媒体最大体积",
        description = "下载媒体体积上限，-1 不限制",
        section = "媒体下载",
        min = -1,
        max = 500,
        unit = "MB",
        visible_when(key = "enable_media_download", value = true),
    )]
    pub video_max_mb: i64,

    #[config(
        default = 480,
        label = "视频最大时长",
        description = "超过时长不下载",
        section = "媒体下载",
        min = 1,
        max = 3600,
        unit = "秒",
        visible_when(key = "enable_media_download", value = true)
    )]
    pub video_duration_maximum: u64,

    #[config(
        default = 3,
        label = "最大并发下载",
        description = "后台媒体下载并发",
        section = "媒体下载",
        min = 1,
        max = 10,
        unit = "个",
        visible_when(key = "enable_media_download", value = true)
    )]
    pub download_concurrent_limit: usize,

    #[config(
        default = 30,
        label = "下载队列上限",
        description = "队列满时只发解析结果",
        section = "媒体下载",
        min = 1,
        max = 200,
        unit = "个",
        visible_when(key = "enable_media_download", value = true)
    )]
    pub download_queue_limit: usize,

    // 内容过滤
    #[config(
        default = Vec::new(),
        label = "黑名单用户",
        description = "不处理这些用户发送的链接，每项填写一个用户 ID",
        section = "内容过滤",
        editor = "tags",
        encoding = "json_string",
    )]
    pub blacklist_users: Vec<String>,

    #[config(
        default = Vec::new(),
        label = "黑名单群组",
        description = "不处理这些群组中的链接，每项填写一个群号",
        section = "内容过滤",
        editor = "tags",
        encoding = "json_string",
    )]
    pub blacklist_groups: Vec<String>,

    #[config(
        default = Vec::new(),
        label = "屏蔽关键词",
        description = "消息含任一关键词时跳过解析",
        section = "内容过滤",
        editor = "tags",
        encoding = "json_string",
    )]
    pub blocked_keywords: Vec<String>,

    #[config(
        default = false,
        label = "屏蔽词区分大小写",
        description = "默认不区分大小写",
        section = "内容过滤",
        widget = "switch"
    )]
    pub blocked_case_sensitive: bool,

    // B站
    #[config(
        default = "",
        label = "B站 Cookie",
        description = "可选手动 Cookie；数据库中的扫码登录凭据优先",
        section = "B站",
        widget = "password"
    )]
    pub bili_cookie: String,

    #[config(
        default = "720P",
        label = "B站画质",
        description = "首选视频清晰度；按实际可获取档位回退，匿名访问会提示本次可用画质",
        section = "B站",
        widget = "select",
        options = ["1080P", "720P", "480P", "360P"],
    )]
    pub bili_quality: String,

    #[config(
        default = true,
        label = "启用直播订阅",
        description = "轮询 B 站直播状态并推送",
        section = "B站",
        widget = "switch"
    )]
    pub live_enabled: bool,

    #[config(
        default = 60,
        label = "直播轮询间隔",
        description = "最低 60 秒",
        section = "B站",
        min = 60,
        max = 3600,
        unit = "秒",
        visible_when(key = "live_enabled", value = true)
    )]
    pub live_poll_interval: u64,

    #[config(
        default = 10,
        label = "直播订阅上限",
        description = "单实例最大订阅数",
        section = "B站",
        min = 1,
        max = 50,
        unit = "个",
        visible_when(key = "live_enabled", value = true)
    )]
    pub live_max_subscriptions: usize,

    // 抖音
    #[config(
        default = "",
        label = "抖音 Cookie",
        description = "可选网页 Cookie",
        section = "抖音",
        widget = "password"
    )]
    pub douyin_cookie: String,

    #[config(
        default = "1080P",
        label = "抖音画质",
        description = "首选视频清晰度；平台可能返回其他档位，卡片区分请求画质与已知实际画质",
        section = "抖音",
        widget = "select",
        options = ["1080P", "720P", "480P", "360P"],
    )]
    pub douyin_quality: String,

    // 小红书
    #[config(
        default = "",
        label = "小红书 Cookie",
        description = "可选网页 Cookie",
        section = "小红书",
        widget = "password"
    )]
    pub xhs_cookie: String,

    #[config(
        default = "1080P",
        label = "小红书画质",
        description = "从作品提供的兼容播放源选择清晰度；缺少该档时显示实际可用画质",
        section = "小红书",
        widget = "select",
        options = ["1080P", "720P", "480P", "360P"],
    )]
    pub xhs_quality: String,

    // 网易云音乐
    #[config(
        default = "",
        label = "网易云备用 API",
        description = "可选 NeteaseCloudMusicApi 地址；默认直接请求平台，歌曲信息获取失败时才尝试此地址",
        section = "网易云音乐"
    )]
    pub ncm_api_host: String,

    // Twitter/X
    #[config(
        default = false,
        label = "启用 Twitter/X",
        description = "通过配置的接口获取链接信息",
        section = "Twitter/X",
        widget = "switch"
    )]
    pub twitter_enabled: bool,

    #[config(
        default = "https://publish.twitter.com/oembed",
        label = "Twitter 接口地址",
        description = "兼容 OEmbed 的接口地址",
        section = "Twitter/X",
        visible_when(key = "twitter_enabled", value = true)
    )]
    pub twitter_api_endpoint: String,
}

pub(super) fn resolver_options() -> Vec<ConfigFieldOption> {
    Platform::all()
        .iter()
        .map(|platform| ConfigFieldOption {
            label: platform.display_name().into(),
            value: platform.as_str().into(),
        })
        .collect()
}
