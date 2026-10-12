//! 可取消、有界、只在确认回执后轮询的音乐扫码登录任务。

use std::{
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use anyhow::{Result, anyhow, ensure};
use nuono_plugin::{
    ImageSource, MessageSegment, PluginTaskScope, SendableMessage,
    permission::{PermissionLevel, PermissionManager},
};
use tokio::sync::{Mutex as AsyncMutex, watch};
use uuid::Uuid;

use super::{MusicCredentials, MusicPlatform, QrImage, QrPoll, QrProtocol};
use crate::{
    auth::{qr_image, store::CredentialStore},
    delivery::ReplyTarget,
    runtime::state::RuntimeState,
};

struct Pending {
    id: Uuid,
    cancel: watch::Sender<bool>,
}

#[derive(Default)]
struct Control {
    pending: Option<Pending>,
    user_id: Option<String>,
}

pub(crate) struct MusicLogin {
    platform: MusicPlatform,
    state: Weak<Mutex<RuntimeState>>,
    tasks: Arc<PluginTaskScope>,
    permissions: Arc<PermissionManager>,
    store: Arc<dyn CredentialStore>,
    protocol: Arc<dyn QrProtocol>,
    control: AsyncMutex<Control>,
    closed: AtomicBool,
}

enum AttemptError {
    Unavailable,
    Expired,
    Refused,
}

impl MusicLogin {
    pub(crate) async fn read_saved(
        platform: MusicPlatform,
        store: &Arc<dyn CredentialStore>,
    ) -> Result<Option<MusicCredentials>> {
        store
            .read(platform.session_key())
            .await?
            .map(|encoded| MusicCredentials::decode(platform, &encoded))
            .transpose()
    }

    pub(crate) fn new(
        platform: MusicPlatform,
        state: &Arc<Mutex<RuntimeState>>,
        tasks: Arc<PluginTaskScope>,
        permissions: Arc<PermissionManager>,
        store: Arc<dyn CredentialStore>,
        protocol: Arc<dyn QrProtocol>,
        saved: Option<&MusicCredentials>,
    ) -> Arc<Self> {
        Arc::new(Self {
            platform,
            state: Arc::downgrade(state),
            tasks,
            permissions,
            store,
            protocol,
            control: AsyncMutex::new(Control {
                pending: None,
                user_id: saved
                    .filter(|session| session.platform() == platform)
                    .map(|session| session.user_id().to_owned()),
            }),
            closed: AtomicBool::new(false),
        })
    }

    pub(crate) async fn authorize(&self, event: &nuono_plugin::Event<'_>) -> Result<()> {
        ensure!(
            event.raw()["message_type"] == "private",
            "音乐登录命令仅限超管私聊使用"
        );
        let target = event
            .message_target()
            .map_err(|_| anyhow!("登录消息缺少有效私聊路由"))?;
        ensure!(
            matches!(target.kind, nuono_plugin::RuntimeTargetKind::Private { .. }),
            "音乐登录命令仅限超管私聊使用"
        );
        let level = self
            .permissions
            .try_get_user_level(event.raw())
            .await
            .map_err(|_| anyhow!("无法验证超管权限，登录操作未执行"))?;
        ensure!(
            level.has_permission(PermissionLevel::Superuser),
            "音乐登录命令需要 Nuono 超管权限"
        );
        Ok(())
    }

    pub(crate) async fn start(self: &Arc<Self>, target: ReplyTarget) -> Result<()> {
        let mut control = self.control.lock().await;
        ensure!(
            !self.closed.load(Ordering::Acquire) && !self.tasks.is_closed(),
            "插件已停用"
        );
        ensure!(
            control.pending.is_none(),
            "已有{}扫码任务，请先发送 url{}取消登录",
            self.platform.label(),
            self.platform.command_name()
        );
        let id = Uuid::new_v4();
        let (cancel, cancelled) = watch::channel(false);
        control.pending = Some(Pending { id, cancel });
        let service = self.clone();
        if !self.tasks.try_spawn(self.platform.task_name(), async move {
            service.run(id, target, cancelled).await;
        }) {
            control.pending = None;
            return Err(anyhow!("插件已停用，登录任务未启动"));
        }
        Ok(())
    }

    async fn run(
        self: Arc<Self>,
        id: Uuid,
        target: ReplyTarget,
        mut cancelled: watch::Receiver<bool>,
    ) {
        let result = tokio::select! { biased; _ = cancelled.changed() => None, result = self.challenge(&target) => Some(result), };
        let message = match result {
            None => None,
            Some(Err(AttemptError::Expired)) => Some(format!(
                "{}登录二维码已过期，请重新发送 url{}登录。",
                self.platform.label(),
                self.platform.command_name()
            )),
            Some(Err(AttemptError::Refused)) => Some(format!(
                "{}登录已被取消或拒绝，已保存的账号保持不变。",
                self.platform.label()
            )),
            // Never expose a platform HTTP error, its source chain, URL or response body.
            Some(Err(AttemptError::Unavailable)) => Some(format!(
                "{}扫码登录未完成，请稍后重试。",
                self.platform.label()
            )),
            Some(Ok(credentials)) => match self.commit(id, &credentials).await {
                Ok(true) => Some(format!(
                    "{}登录凭据已保存（账号：{}），解析立即生效。会员权限以平台返回结果为准。",
                    self.platform.label(),
                    credentials.user_id()
                )),
                Ok(false) => None,
                Err(_) => Some(format!(
                    "{}登录凭据保存或应用失败，请查询登录状态后重试。",
                    self.platform.label()
                )),
            },
        };
        if let Some(message) = message {
            let _ = tokio::select! { biased; _ = cancelled.changed() => Ok(()), result = target.send_confirmed(message.into()) => result, };
        }
        self.finish(id).await;
    }

    async fn challenge(
        &self,
        target: &ReplyTarget,
    ) -> std::result::Result<MusicCredentials, AttemptError> {
        // A misbehaving protocol cannot occupy a pending slot indefinitely before returning a QR.
        let qr = tokio::time::timeout(Duration::from_secs(30), self.protocol.generate())
            .await
            .map_err(|_| AttemptError::Unavailable)?
            .map_err(|_| AttemptError::Unavailable)?;
        let remaining = qr.expires_in();
        tokio::time::timeout(remaining, async {
            let png = match qr.image() {
                QrImage::Url(url) => {
                    let url = url.clone();
                    self.tasks.render("music-login-qr", move || qr_image::render(&url)).await
                        .map_err(|_| AttemptError::Unavailable)?
                }
                QrImage::Bytes(bytes) => bytes.clone(),
            };
            let image = ImageSource::bytes(png).into_segment().await.map_err(|_| AttemptError::Unavailable)?;
            let text = format!("请用{} App 扫码并确认登录。此账号将供 LinkShowNN 解析使用；会员曲目仍取决于账号权限。取消：url{}取消登录。", self.platform.label(), self.platform.command_name());
            let message: SendableMessage = vec![MessageSegment::text(text), image].into();
            target.send_confirmed(message).await.map_err(|_| AttemptError::Unavailable)?;
            let mut scanned = false;
            loop {
                if qr.expires_in().is_zero() { return Err(AttemptError::Expired); }
                match self.protocol.poll(&qr).await.map_err(|_| AttemptError::Unavailable)? {
                    QrPoll::WaitingForScan => {},
                    QrPoll::WaitingForConfirmation if !scanned => {
                        scanned = true;
                        target.send_confirmed(format!("已扫码，请在{} App 中确认登录。", self.platform.label()).into()).await
                            .map_err(|_| AttemptError::Unavailable)?;
                    }
                    QrPoll::WaitingForConfirmation => {},
                    QrPoll::Expired => return Err(AttemptError::Expired),
                    QrPoll::Refused => return Err(AttemptError::Refused),
                    QrPoll::Success(credentials) if credentials.platform() == self.platform => return Ok(credentials),
                    QrPoll::Success(_) => return Err(AttemptError::Unavailable),
                }
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        }).await.map_err(|_| AttemptError::Expired)?
    }

    async fn commit(&self, id: Uuid, credentials: &MusicCredentials) -> Result<bool> {
        let mut control = self.control.lock().await;
        if !self.is_current(&control, id) {
            return Ok(false);
        }
        ensure!(
            credentials.platform() == self.platform,
            "音乐登录凭据平台不匹配"
        );
        let state = self.state.upgrade().ok_or_else(|| anyhow!("插件已卸载"))?;
        crate::auth::sessions::apply_music(&state, &self.store, self.platform, Some(credentials))
            .await?;
        control.user_id = Some(credentials.user_id().to_owned());
        Ok(true)
    }

    fn is_current(&self, control: &Control, id: Uuid) -> bool {
        !self.closed.load(Ordering::Acquire)
            && !self.tasks.is_closed()
            && control
                .pending
                .as_ref()
                .is_some_and(|pending| pending.id == id)
    }

    async fn finish(&self, id: Uuid) {
        let mut control = self.control.lock().await;
        if control
            .pending
            .as_ref()
            .is_some_and(|pending| pending.id == id)
        {
            control.pending = None;
        }
    }

    pub(crate) async fn cancel(&self) -> bool {
        let mut control = self.control.lock().await;
        if let Some(pending) = control.pending.take() {
            let _ = pending.cancel.send(true);
            true
        } else {
            false
        }
    }

    pub(crate) async fn shutdown(&self) {
        let mut control = self.control.lock().await;
        self.closed.store(true, Ordering::Release);
        if let Some(pending) = control.pending.take() {
            let _ = pending.cancel.send(true);
        }
    }

    pub(crate) async fn clear(&self) -> Result<()> {
        let mut control = self.control.lock().await;
        ensure!(
            !self.closed.load(Ordering::Acquire) && !self.tasks.is_closed(),
            "插件已停用"
        );
        if let Some(pending) = control.pending.take() {
            let _ = pending.cancel.send(true);
        }
        let state = self.state.upgrade().ok_or_else(|| anyhow!("插件已卸载"))?;
        crate::auth::sessions::apply_music(&state, &self.store, self.platform, None).await?;
        control.user_id = None;
        Ok(())
    }

    pub(crate) async fn status(&self) -> String {
        let control = self.control.lock().await;
        let mut text = match &control.user_id {
            Some(uid) => format!(
                "已保存{}扫码登录凭据（账号：{uid}）。有效性及会员权限以平台请求结果为准。",
                self.platform.label()
            ),
            None => format!(
                "未保存{}扫码登录凭据；解析使用手动 Cookie 或匿名访问。",
                self.platform.label()
            ),
        };
        if control.pending.is_some() {
            text.push_str("\n有一项扫码登录正在进行。");
        }
        text
    }
}

#[cfg(test)]
#[path = "controller_tests.rs"]
mod tests;
