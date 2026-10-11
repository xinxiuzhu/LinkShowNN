//! 站点标识与元数据：配置名字、显示名、页面域名和媒体 Referer。
//! URL 的匹配与校验规则由 [`crate::resolver::url_policy`] 使用这些声明实现。

/// `Default` 取 Bilibili，与媒体 Referer 的历史兜底站点保持一致。
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Default, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "lowercase")]
pub enum Platform {
    #[default]
    Bilibili,
    Douyin,
    Kuaishou,
    Xiaohongshu,
    Twitter,
    Ncm,
    Kurobbs,
    QqChannel,
}

impl Platform {
    pub fn display_name(self) -> &'static str {
        match self {
            Self::Bilibili => "哔哩哔哩",
            Self::Douyin => "抖音",
            Self::Kuaishou => "快手",
            Self::Xiaohongshu => "小红书",
            Self::Twitter => "Twitter/X",
            Self::Ncm => "网易云音乐",
            Self::Kurobbs => "库街区",
            Self::QqChannel => "腾讯频道",
        }
    }

    /// 页面域名含短链域名；HTTP 逐跳验证，浏览器验证起始与最终页面。
    pub fn page_hosts(self) -> &'static [&'static str] {
        match self {
            Self::Bilibili => &["b23.tv", "bili2233.cn", "bilibili.com"],
            Self::Douyin => &["douyin.com", "iesdouyin.com"],
            Self::Kuaishou => &["kuaishou.com", "chenzhongtech.com"],
            Self::Xiaohongshu => &["xhslink.com", "xhslink.cn", "xiaohongshu.com"],
            Self::Twitter => &["twitter.com", "x.com"],
            Self::Ncm => &["163cn.tv", "music.163.com"],
            Self::Kurobbs => &["kurobbs.com"],
            Self::QqChannel => &["pd.qq.com"],
        }
    }

    /// 配置项和日志里的稳定名字，改动会破坏用户已有的 `disabled_resolvers`。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Bilibili => "bilibili",
            Self::Douyin => "douyin",
            Self::Kuaishou => "kuaishou",
            Self::Xiaohongshu => "xiaohongshu",
            Self::Twitter => "twitter",
            Self::Ncm => "ncm",
            Self::Kurobbs => "kurobbs",
            Self::QqChannel => "qqchannel",
        }
    }

    /// 媒体 CDN 普遍校验 Referer 归属站点，跨站会被 403。
    ///
    /// 用主站地址而非分享页 host：抖音分享页在 `iesdouyin.com`，但它的 CDN
    /// 只认 `douyin.com`。
    ///
    /// 故意不写 `_` 兜底：新增平台时漏填会编译失败，而不是运行时才 403。
    pub fn media_referer(self) -> &'static str {
        match self {
            Self::Bilibili => "https://www.bilibili.com/",
            Self::Douyin => "https://www.douyin.com/",
            Self::Kuaishou => "https://www.kuaishou.com/",
            Self::Xiaohongshu => "https://www.xiaohongshu.com/",
            Self::Twitter => "https://twitter.com/",
            Self::Ncm => "https://music.163.com/",
            Self::Kurobbs => "https://www.kurobbs.com/",
            Self::QqChannel => "https://pd.qq.com/",
        }
    }

    /// URL 匹配的尝试顺序。
    ///
    /// Bilibili 排最后：它的 host 集合最宽，先试更专的平台可避免误判。
    pub fn all() -> &'static [Self] {
        &[
            Self::Xiaohongshu,
            Self::Douyin,
            Self::Kuaishou,
            Self::Ncm,
            Self::Twitter,
            Self::Kurobbs,
            Self::QqChannel,
            Self::Bilibili,
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::Platform;

    #[test]
    fn all_platforms_are_listed_exactly_once() {
        // `all()` 是 URL 匹配的唯一入口，漏一个平台就等于该平台整体失效。
        let mut names = Platform::all()
            .iter()
            .map(|platform| platform.as_str())
            .collect::<Vec<_>>();
        names.sort_unstable();
        let total = names.len();
        names.dedup();
        assert_eq!(names.len(), total, "all() 里有重复平台");
        assert_eq!(total, 8, "新增平台后请同步 all()");
    }
}
