//! 消息投递：构造图文和媒体消息，保存请求的回复目标与上下文。

mod gallery;
mod messages;
mod scoped;
mod target;

pub(crate) use gallery::{GalleryDelivery, MAX_GALLERY_IMAGES, gallery_text};
#[cfg(test)]
pub use messages::image_reply;
pub use messages::{card_message, image_message, media_message, reply_message};
pub(crate) use scoped::{PreparedReply, send_scoped};
pub(crate) use target::ReplyTarget;
