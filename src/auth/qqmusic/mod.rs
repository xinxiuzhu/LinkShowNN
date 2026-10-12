//! QQ音乐 QQ 账号扫码：只访问平台自己的 HTTPS 登录与授权端点。

mod client;
mod model;
mod transport;

pub(crate) use client::QqMusicQrClient;

#[cfg(test)]
mod tests;
