//! 评论展示开关共用宿主配置文件与网页配置，运行资源由宿主热更新。

use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use nuono_plugin::prelude::*;
use serde_json::json;
use tracing::warn;

use crate::runtime::state::{self, RuntimeState};

#[derive(Clone, Copy)]
enum CommentsAction {
    Enable,
    Disable,
    Status,
}

struct CommentsHandler {
    state: Arc<Mutex<RuntimeState>>,
    action: CommentsAction,
}

impl CommentsHandler {
    async fn response(&self) -> Result<String> {
        let (updates, active) = state::with(&self.state, |state| {
            (state.config_updates.clone(), state.config.show_comments)
        })?;
        let current = updates.read_latest().await?;
        if matches!(self.action, CommentsAction::Status) {
            let saved = current.values["show_comments"]
                .as_bool()
                .context("评论展示配置不是布尔值")?;
            let mut text = format!("评论展示（全局）：{}。", label(saved));
            if saved != active {
                text.push_str(&format!("当前运行：{}，等待配置刷新。", label(active)));
            }
            text.push_str("\n关闭评论展示后，作品评论数统计仍保留。");
            return Ok(text);
        }
        let enabled = matches!(self.action, CommentsAction::Enable);
        updates
            .update_patch(json!({"show_comments": enabled}), &current.revision)
            .await?;
        // Never await on_config_changed here: the host delivers it after this
        // command releases its plugin callback lock.
        Ok(format!(
            "已保存：{}评论展示（全局）。约 1 秒后用于后续解析，作品评论数统计仍保留。",
            if enabled { "开启" } else { "关闭" }
        ))
    }
}

fn label(enabled: bool) -> &'static str {
    if enabled { "开启" } else { "关闭" }
}

#[async_trait]
impl TriggerHandler for CommentsHandler {
    async fn handle(&self, bot: &Bot, _msg: &Message, data: MatchData) -> Result<()> {
        if !data.args.trim().is_empty() {
            bot.send("用法：url开启评论 / url关闭评论 / url评论状态。")
                .await?;
            return Ok(());
        }
        let text = match self.response().await {
            Ok(text) => text,
            Err(error) => {
                warn!(error = %error, "评论展示配置指令失败");
                "评论设置读取或保存失败，请稍后重试或检查网页配置。".into()
            }
        };
        bot.send(text).await?;
        Ok(())
    }
}

pub(crate) fn commands(state: &Arc<Mutex<RuntimeState>>) -> Vec<Command> {
    [
        (
            "开启评论",
            "开启解析评论展示（全局）",
            CommentsAction::Enable,
            PermissionLevel::SuperUser,
        ),
        (
            "关闭评论",
            "关闭解析评论展示（全局）",
            CommentsAction::Disable,
            PermissionLevel::SuperUser,
        ),
        (
            "评论状态",
            "查看解析评论展示开关",
            CommentsAction::Status,
            PermissionLevel::GroupAdmin,
        ),
    ]
    .into_iter()
    .map(|(name, description, action, permission)| {
        Command::from_message(
            name,
            Arc::new(CommentsHandler {
                state: state.clone(),
                action,
            }),
        )
        .with_description(description)
        .with_permission(permission)
        .with_block(true)
    })
    .collect()
}

#[cfg(test)]
mod tests;
