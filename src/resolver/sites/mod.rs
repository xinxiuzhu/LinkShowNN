//! 站点解析接口。
//!
//! 每个适配器持有自己的请求依赖与加载时配置；路由与装配集中在
//! [`crate::resolver::ParserManager`]。通用分享页取页、Open Graph 兜底由
//! [`PageAdapter`] 复用，站点只补自身字段。

pub mod bilibili;
pub mod douyin;
pub mod kuaishou;
pub mod kurobbs;
pub mod ncm;
pub mod qq_channel;
pub mod twitter;
pub mod xiaohongshu;

use anyhow::Result;
use url::Url;

use crate::{
    resolver::fetch::{FetchedPage, PageFetcher},
    resolver::page::page_metadata,
    resolver::{MediaInfo, Platform, Resolved},
};

/// 把已识别的 URL 变成解析结果，依赖由适配器自身管理。
#[async_trait::async_trait]
pub trait PlatformAdapter: Send + Sync {
    fn platform(&self) -> Platform;

    async fn resolve(&self, url: &Url) -> Result<Resolved>;
}

/// 走「抓分享页 + 解析执行后状态或内联状态」这条路的站点。
///
/// 默认实现完成取页、Open Graph 兜底和 Cookie 附加；字段提取保持纯函数，
/// 可使用离线 HTML 与运行时状态验证。
pub trait PageAdapter: Send + Sync {
    fn platform(&self) -> Platform;

    fn fetcher(&self) -> &PageFetcher;

    fn page_cookie(&self) -> Option<&str> {
        None
    }

    fn enrich(&self, page: &FetchedPage, media: &mut MediaInfo);
}

#[async_trait::async_trait]
impl<T: PageAdapter> PlatformAdapter for T {
    fn platform(&self) -> Platform {
        PageAdapter::platform(self)
    }

    async fn resolve(&self, url: &Url) -> Result<Resolved> {
        let platform = PageAdapter::platform(self);
        let page = self
            .fetcher()
            .get_page(url, platform, self.page_cookie())
            .await?;
        let mut media = page_metadata(&page.html, &page.url, platform);
        self.enrich(&page, &mut media);
        Ok(Resolved::new(media))
    }
}
