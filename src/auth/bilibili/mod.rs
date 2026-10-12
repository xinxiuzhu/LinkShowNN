//! B 站官方 Web 扫码登录协议。与解析器的既有 Cookie 会话相互独立。

mod client;
mod controller;
mod model;

pub(crate) use client::BilibiliQrClient;
pub(crate) use controller::BilibiliLogin;
pub(crate) use model::{Credentials, LoginError, PollOutcome, QrCode};

#[cfg(test)]
mod tests;
