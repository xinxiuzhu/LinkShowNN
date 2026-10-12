//! 链接解析：共享契约与路由在入口层，各平台业务在自己的目录内。

pub mod adapter;
pub mod bilibili;
pub mod card;
pub mod content;
pub mod douyin;
pub mod fetch;
pub mod kuaishou;
pub mod kurobbs;
mod manager;
pub mod media;
pub mod ncm;
pub mod page;
pub mod platform;
pub mod qq_channel;
pub mod qqmusic;
pub mod twitter;
pub mod url_policy;
pub mod xiaohongshu;

pub use card::{Card, CardComment, CardCommentEmote, CardMetadata, CardPoster, CardPublicDetails};
pub use content::{Resolved, ResolvedContent};
pub use manager::ParserManager;
pub(crate) use manager::SessionCookies;
pub use media::MediaInfo;
pub use platform::Platform;
