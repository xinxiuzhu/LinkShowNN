//! 登录业务：平台协议、扫码会话和宿主数据库私有凭据。

mod bilibili;
mod commands;
pub(crate) mod music;
pub(crate) mod ncm;
pub(crate) mod qqmusic;
mod qr_image;
mod sessions;
mod store;

pub(crate) use bilibili::BilibiliLogin;
pub(crate) use bilibili::Credentials;
pub(crate) use commands::commands;
pub(crate) use store::database_store;
