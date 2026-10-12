//! 媒体下载：候选模型、下载执行与卡片优先的后台调度。

pub(crate) mod gallery;
pub(crate) mod images;
mod manager;
pub mod model;
pub mod task;

pub use manager::{DownloadManager, DownloadReservation};
pub use model::{DownloadCandidate, Downloaded, MediaKind, SkipReason};
