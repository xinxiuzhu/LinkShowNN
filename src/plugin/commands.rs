//! 帮助、运行状态及命令注册。

use std::sync::{Arc, Mutex};

use anyhow::Result;
use nuono_plugin::prelude::*;

use crate::runtime::state::RuntimeState;

use super::LinkShowNN;

struct InfoHandler {
    state: Arc<Mutex<RuntimeState>>,
    help: bool,
}

#[async_trait]
impl TriggerHandler for InfoHandler {
    async fn handle(&self, bot: &Bot, _msg: &Message, _data: MatchData) -> Result<()> {
        if self.help {
            bot.send(
                "LinkShowNN 支持：B站、抖音、快手、小红书、Twitter/X、网易云音乐、QQ音乐、库街区、腾讯频道。\n直接发送链接即可自动解析。\n管理员命令：url状态。\n超管私聊扫码：urlB站登录 / url网易云登录 / urlQQ音乐登录。\n各平台还有登录状态、取消登录和清除登录指令。",
            )
            .await?;
            return Ok(());
        }
        let snapshot = {
            let state = self
                .state
                .lock()
                .map_err(|_| anyhow::anyhow!("resolver state lock poisoned"))?;
            let mut report = state.metrics.report(
                state.config.enable_media_download,
                state.config.concurrent_limit,
                state.config.user_cooldown,
            );
            report.push_str("\n分享页引擎: HTTP");
            report.push_str("\n抖音: API / 公开静态分享页");
            report.push_str(if state.config.render_cards {
                "\n解析回复: 图文卡片"
            } else {
                "\n解析回复: 原图文消息"
            });
            report
        };
        bot.send(snapshot).await?;
        Ok(())
    }
}

impl LinkShowNN {
    pub(super) fn registered_commands(&self) -> Vec<Command> {
        let mut commands = vec![
            Command::from_message(
                "帮助",
                Arc::new(InfoHandler {
                    state: self.state.clone(),
                    help: true,
                }),
            )
            .with_alias("help")
            .with_description("查看 LinkShowNN 帮助")
            .with_block(true),
            Command::from_message(
                "状态",
                Arc::new(InfoHandler {
                    state: self.state.clone(),
                    help: false,
                }),
            )
            .with_alias("status")
            .with_description("查看 LinkShowNN 运行统计")
            .with_permission(PermissionLevel::GroupAdmin)
            .with_block(true),
        ];
        commands.extend(crate::auth::commands(&self.state));
        commands
    }
}
