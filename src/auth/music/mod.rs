//! 音乐平台共享扫码任务；平台 HTTP 协议与宿主账号装配分别维护。

mod controller;
mod model;

pub(crate) use controller::MusicLogin;
pub(crate) use model::{MusicCredentials, MusicPlatform, QrChallenge, QrImage, QrPoll, QrProtocol};

#[cfg(test)]
mod tests;
