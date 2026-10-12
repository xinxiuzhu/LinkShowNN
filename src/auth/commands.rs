//! 登录命令保留完整事件，用宿主权限和精确私聊路由执行。

use std::sync::{Arc, Mutex};

use anyhow::{Result, anyhow};
use nuono_plugin::{Bot, Command, CommandHandler, Event, MatchData, PermissionLevel};

use crate::{
    auth::music::MusicPlatform,
    delivery::{PreparedReply, ReplyTarget, send_scoped},
    runtime::state::RuntimeState,
};

#[derive(Clone, Copy)]
enum Action {
    Login,
    Status,
    Cancel,
    Clear,
}

struct LoginHandler {
    state: Arc<Mutex<RuntimeState>>,
    action: Action,
}

#[async_trait::async_trait]
impl CommandHandler for LoginHandler {
    async fn handle(&self, bot: &Bot, event: &Event<'_>, _data: MatchData) -> Result<()> {
        let (login, tasks) = {
            let state = self.state.lock().map_err(|_| anyhow!("插件状态不可用"))?;
            (
                state
                    .bili_login
                    .clone()
                    .ok_or_else(|| anyhow!("B 站登录服务未加载"))?,
                state.tasks.clone().ok_or_else(|| anyhow!("插件未加载"))?,
            )
        };
        // 即使处理器被其它入口直接调用，也不能绕过宿主权限检查。
        login.authorize(event).await?;
        let target = ReplyTarget::new(Arc::new(bot.clone()), event)?;
        let text = match self.action {
            Action::Login => match login.start(target.clone()).await {
                Ok(()) => return Ok(()),
                Err(error) => error.to_string(),
            },
            Action::Status => login.status().await,
            Action::Cancel => if login.cancel().await {
                "B 站扫码登录已取消，已保存的账号保持不变。"
            } else {
                "当前没有 B 站扫码登录任务。"
            }
            .to_owned(),
            Action::Clear => {
                if login.clear().await.is_err() {
                    "清除 B 站扫码凭据失败，请重试。".to_owned()
                } else {
                    "已清除插件保存的 B 站扫码凭据；解析恢复手动 Cookie 或匿名访问。这不会退出你在 B 站 App 中的账号。".to_owned()
                }
            }
        };
        send_scoped(target, tasks, PreparedReply::Message(text.into())).await
    }
}

pub(crate) fn commands(state: &Arc<Mutex<RuntimeState>>) -> Vec<Command> {
    let mut commands: Vec<_> = [
        (
            "B站登录",
            "bili登录",
            "扫码登录 B 站（仅超管私聊）",
            Action::Login,
        ),
        (
            "B站登录状态",
            "bili登录状态",
            "查看 B 站扫码凭据保存状态",
            Action::Status,
        ),
        (
            "B站取消登录",
            "bili取消登录",
            "取消 B 站扫码任务",
            Action::Cancel,
        ),
        (
            "B站清除登录",
            "bili清除登录",
            "清除插件保存的 B 站扫码凭据",
            Action::Clear,
        ),
    ]
    .into_iter()
    .map(|(name, alias, description, action)| {
        Command::new(
            name,
            Arc::new(LoginHandler {
                state: state.clone(),
                action,
            }),
        )
        .with_alias(alias)
        .with_description(description)
        .with_permission(PermissionLevel::SuperUser)
        .with_block(true)
    })
    .collect();
    commands.extend(music_commands(state));
    commands
}

struct MusicLoginHandler {
    state: Arc<Mutex<RuntimeState>>,
    platform: MusicPlatform,
    action: Action,
}

#[async_trait::async_trait]
impl CommandHandler for MusicLoginHandler {
    async fn handle(&self, bot: &Bot, event: &Event<'_>, _data: MatchData) -> Result<()> {
        let (login, tasks) = {
            let state = self.state.lock().map_err(|_| anyhow!("插件状态不可用"))?;
            let login = match self.platform {
                MusicPlatform::Ncm => &state.ncm_login,
                MusicPlatform::QqMusic => &state.qqmusic_login,
            };
            (
                login.clone().ok_or_else(|| anyhow!("音乐登录服务未加载"))?,
                state.tasks.clone().ok_or_else(|| anyhow!("插件未加载"))?,
            )
        };
        login.authorize(event).await?;
        let target = ReplyTarget::new(Arc::new(bot.clone()), event)?;
        let platform = self.platform.label();
        let text = match self.action {
            Action::Login => match login.start(target.clone()).await {
                Ok(()) => return Ok(()),
                Err(error) => error.to_string(),
            },
            Action::Status => login.status().await,
            Action::Cancel => {
                if login.cancel().await {
                    format!("{platform}扫码登录已取消，已保存的账号保持不变。")
                } else {
                    format!("当前没有{platform}扫码登录任务。")
                }
            }
            Action::Clear => {
                if login.clear().await.is_err() {
                    format!("清除{platform}扫码凭据失败，请重试。")
                } else {
                    format!(
                        "已清除插件保存的{platform}扫码凭据；解析恢复匿名访问。这不会退出你在音乐 App 中的账号。"
                    )
                }
            }
        };
        send_scoped(target, tasks, PreparedReply::Message(text.into())).await
    }
}

fn music_commands(state: &Arc<Mutex<RuntimeState>>) -> Vec<Command> {
    [MusicPlatform::Ncm, MusicPlatform::QqMusic]
        .into_iter()
        .flat_map(|platform| {
            [
                ("登录", "扫码登录", Action::Login),
                ("登录状态", "查看扫码凭据保存状态", Action::Status),
                ("取消登录", "取消扫码任务", Action::Cancel),
                ("清除登录", "清除插件保存的扫码凭据", Action::Clear),
            ]
            .into_iter()
            .map(move |(suffix, description, action)| {
                let alias = match platform {
                    MusicPlatform::Ncm => "ncm",
                    MusicPlatform::QqMusic => "qqmusic",
                };
                Command::new(
                    &format!("{}{suffix}", platform.command_name()),
                    Arc::new(MusicLoginHandler {
                        state: state.clone(),
                        platform,
                        action,
                    }),
                )
                .with_alias(&format!("{alias}{suffix}"))
                .with_description(&format!("{}{description}（仅超管私聊）", platform.label()))
                .with_permission(PermissionLevel::SuperUser)
                .with_block(true)
            })
        })
        .collect()
}
