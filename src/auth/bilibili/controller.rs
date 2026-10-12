//! 单账号扫码会话。任务可取消；凭据持久化成功后才替换解析器。

use std::sync::{
    Arc, Mutex, Weak,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

use anyhow::{Result, anyhow, ensure};
use async_trait::async_trait;
use nuono_plugin::permission::{PermissionLevel, PermissionManager};
use nuono_plugin::{ImageSource, MessageSegment, PluginTaskScope, SendableMessage};
use tokio::sync::{Mutex as AsyncMutex, watch};
use uuid::Uuid;

use super::{BilibiliQrClient, Credentials, LoginError, PollOutcome, QrCode};
use crate::auth::{
    qr_image, sessions,
    store::{BILIBILI_SESSION_KEY, CredentialStore},
};
use crate::{delivery::ReplyTarget, runtime::state::RuntimeState};

#[async_trait]
trait LoginProtocol: Send + Sync {
    async fn generate(&self) -> std::result::Result<QrCode, LoginError>;
    async fn poll(&self, qr: &QrCode) -> std::result::Result<PollOutcome, LoginError>;
}

#[async_trait]
impl LoginProtocol for BilibiliQrClient {
    async fn generate(&self) -> std::result::Result<QrCode, LoginError> {
        self.generate().await
    }
    async fn poll(&self, qr: &QrCode) -> std::result::Result<PollOutcome, LoginError> {
        self.poll(qr).await
    }
}

struct Pending {
    id: Uuid,
    cancel: watch::Sender<bool>,
}

#[derive(Default)]
struct Control {
    pending: Option<Pending>,
    user_id: Option<String>,
}

pub(crate) struct BilibiliLogin {
    state: Weak<Mutex<RuntimeState>>,
    tasks: Arc<PluginTaskScope>,
    permissions: Arc<PermissionManager>,
    store: Arc<dyn CredentialStore>,
    protocol: Arc<dyn LoginProtocol>,
    // 串行化申请、取消、清除与成功写入，旧轮询不能覆盖更新后的会话。
    control: AsyncMutex<Control>,
    closed: AtomicBool,
}

impl BilibiliLogin {
    pub(crate) async fn read_saved(
        store: &Arc<dyn CredentialStore>,
    ) -> Result<Option<Credentials>> {
        store
            .read(BILIBILI_SESSION_KEY)
            .await?
            .map(|value| Credentials::decode(&value).map_err(Into::into))
            .transpose()
    }

    pub(crate) fn new(
        state: &Arc<Mutex<RuntimeState>>,
        tasks: Arc<PluginTaskScope>,
        permissions: Arc<PermissionManager>,
        store: Arc<dyn CredentialStore>,
        saved: Option<&Credentials>,
        proxy: &str,
    ) -> Result<Arc<Self>> {
        Ok(Arc::new(Self {
            state: Arc::downgrade(state),
            tasks,
            permissions,
            store,
            protocol: Arc::new(BilibiliQrClient::new(Some(proxy))?),
            control: AsyncMutex::new(Control {
                pending: None,
                user_id: saved.map(|account| account.user_id().to_owned()),
            }),
            closed: AtomicBool::new(false),
        }))
    }

    pub(crate) async fn authorize(&self, event: &nuono_plugin::Event<'_>) -> Result<()> {
        ensure_private(event)?;
        let level = self
            .permissions
            .try_get_user_level(event.raw())
            .await
            .map_err(|_| anyhow!("无法验证超管权限，登录操作未执行"))?;
        ensure_superuser(level)
    }

    pub(crate) async fn start(self: &Arc<Self>, target: ReplyTarget) -> Result<()> {
        let mut control = self.control.lock().await;
        ensure!(
            !self.closed.load(Ordering::Acquire) && !self.tasks.is_closed(),
            "插件已停用"
        );
        ensure!(
            control.pending.is_none(),
            "已有 B 站扫码任务，请先发送 urlB站取消登录"
        );
        let id = Uuid::new_v4();
        let (cancel, cancelled) = watch::channel(false);
        control.pending = Some(Pending { id, cancel });
        let service = self.clone();
        if !self.tasks.try_spawn("bilibili-qr-login", async move {
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
        let challenge = tokio::select! {
            biased;
            _ = cancelled.changed() => None,
            result = self.challenge(&target) => Some(result),
        };
        let message = match challenge {
            None => None,
            Some(Err(error)) => Some(format!("B 站扫码登录未完成：{error}")),
            Some(Ok(credentials)) => match self.commit(id, &credentials).await {
                Ok(true) => Some(format!(
                    "B 站登录凭据已保存（UID：{}），解析立即生效。",
                    credentials.user_id()
                )),
                Ok(false) => None,
                // 不回显数据库、解析器或底层传输错误。
                Err(_) => Some("B 站登录凭据保存或应用失败，请查询登录状态后重试。".to_owned()),
            },
        };
        if let Some(message) = message {
            let _ = tokio::select! {
                biased;
                _ = cancelled.changed() => Ok(()),
                result = target.send_confirmed(message.into()) => result,
            };
        }
        self.finish(id).await;
    }

    async fn challenge(&self, target: &ReplyTarget) -> Result<Credentials> {
        let qr = self.protocol.generate().await?;
        // 不保存二维码文件；URL 只用于绘制，不进入文字消息或日志。
        let url = qr.url().to_owned();
        let png = self
            .tasks
            .render("bilibili-login-qr", move || qr_image::render(&url))
            .await
            .map_err(|_| anyhow!("登录二维码生成失败"))?;
        let image = ImageSource::bytes(png)
            .into_segment()
            .await
            .map_err(|_| anyhow!("登录二维码编码失败"))?;
        let remaining = qr.expires_in();
        tokio::time::timeout(remaining, async {
            let message: SendableMessage = vec![
                MessageSegment::text("请用哔哩哔哩 App 扫码并确认登录。二维码约 3 分钟有效；此账号将供 LinkShowNN 解析使用。取消：urlB站取消登录。"),
                image,
            ].into();
            target.send_confirmed(message).await.map_err(|_| anyhow!("登录二维码发送失败"))?;
            let mut scanned = false;
            loop {
                match self.protocol.poll(&qr).await? {
                    PollOutcome::WaitingForScan => {},
                    PollOutcome::WaitingForConfirmation if !scanned => {
                        scanned = true;
                        target.send_confirmed("已扫码，请在 B 站 App 中确认登录。".into()).await
                            .map_err(|_| anyhow!("登录提示发送失败"))?;
                    },
                    PollOutcome::WaitingForConfirmation => {},
                    PollOutcome::Expired => return Err(anyhow!("二维码已过期，请重新发送 urlB站登录")),
                    PollOutcome::Success(credentials) => return Ok(credentials),
                }
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        }).await.map_err(|_| anyhow!("二维码已过期，请重新发送 urlB站登录"))?
    }

    async fn commit(&self, id: Uuid, credentials: &Credentials) -> Result<bool> {
        let mut control = self.control.lock().await;
        if !self.is_current(&control, id) {
            return Ok(false);
        }
        // 保留强引用直到发布，不能在数据库提交后丢失运行状态。
        let state = self.state.upgrade().ok_or_else(|| anyhow!("插件已卸载"))?;
        sessions::apply_bili(&state, &self.store, Some(credentials)).await?;
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
        // 即使清除失败，也取消二维码，防止旧轮询随后重新写入。
        if let Some(pending) = control.pending.take() {
            let _ = pending.cancel.send(true);
        }
        let state = self.state.upgrade().ok_or_else(|| anyhow!("插件已卸载"))?;
        sessions::apply_bili(&state, &self.store, None).await?;
        control.user_id = None;
        Ok(())
    }

    pub(crate) async fn status(&self) -> String {
        let control = self.control.lock().await;
        let mut text = match &control.user_id {
            Some(uid) => {
                format!("已保存 B 站扫码登录凭据（UID：{uid}）。有效性以平台请求结果为准。")
            }
            None => "未保存 B 站扫码登录凭据；解析使用手动 Cookie 或匿名访问。".to_owned(),
        };
        if control.pending.is_some() {
            text.push_str("\n有一项扫码登录正在进行。");
        }
        text
    }
}

fn ensure_private(event: &nuono_plugin::Event<'_>) -> Result<()> {
    ensure!(
        event.raw()["message_type"] == "private",
        "B 站登录命令仅限超管私聊使用"
    );
    let target = event
        .message_target()
        .map_err(|_| anyhow!("登录消息缺少有效私聊路由"))?;
    ensure!(
        matches!(target.kind, nuono_plugin::RuntimeTargetKind::Private { .. }),
        "B 站登录命令仅限超管私聊使用"
    );
    Ok(())
}

fn ensure_superuser(level: PermissionLevel) -> Result<()> {
    ensure!(
        level.has_permission(PermissionLevel::Superuser),
        "B 站登录命令需要 Nuono 超管权限"
    );
    Ok(())
}

#[cfg(test)]
mod tests;
