//! 配置端口测试只模拟持久化和版本号，不触碰宿主数据库或发送平台消息。

use super::*;

use nuono_plugin::{PluginConfigSnapshot, PluginConfigUpdatePort, PluginConfigUpdates};
use serde_json::Value;

#[derive(Clone, Copy, Default)]
enum SaveFailure {
    #[default]
    None,
    Write,
    Conflict,
}

struct StoredConfig {
    snapshot: PluginConfigSnapshot,
    reads: usize,
    patches: Vec<(Value, String)>,
    failure: SaveFailure,
}

struct ConfigPort(Mutex<StoredConfig>);

#[async_trait]
impl PluginConfigUpdatePort for ConfigPort {
    async fn read_latest(&self) -> Result<PluginConfigSnapshot> {
        let mut stored = self.0.lock().unwrap();
        stored.reads += 1;
        Ok(stored.snapshot.clone())
    }

    async fn update_patch(
        &self,
        patch: Value,
        expected_revision: &str,
    ) -> Result<PluginConfigSnapshot> {
        let mut stored = self.0.lock().unwrap();
        stored
            .patches
            .push((patch.clone(), expected_revision.to_owned()));
        match stored.failure {
            SaveFailure::Write => anyhow::bail!("fixture config write failed"),
            SaveFailure::Conflict => {
                // 模拟读取版本后，网页或手改又保存了一次配置。
                stored.snapshot.values["show_comments"] = json!(false);
                stored.snapshot.values["video_max_mb"] = json!(42);
                stored.snapshot.revision = "newer-web-save".into();
            }
            SaveFailure::None => {}
        }
        anyhow::ensure!(
            stored.snapshot.revision == expected_revision,
            "fixture config revision conflict"
        );
        stored
            .snapshot
            .values
            .as_object_mut()
            .unwrap()
            .extend(patch.as_object().unwrap().clone());
        stored.snapshot.revision = "saved-by-command".into();
        Ok(stored.snapshot.clone())
    }
}

fn fixture(active: bool, saved: bool) -> (Arc<Mutex<RuntimeState>>, Arc<ConfigPort>) {
    let port = Arc::new(ConfigPort(Mutex::new(StoredConfig {
        snapshot: PluginConfigSnapshot {
            values: json!({
                "show_comments": saved,
                "render_cards": true,
                "video_max_mb": 100,
                "unknown_legacy_key": {"keep": [1, 2, 3]},
            }),
            revision: "latest-file-revision".into(),
        },
        reads: 0,
        patches: Vec::new(),
        failure: SaveFailure::None,
    })));
    let mut state = RuntimeState::default();
    state.config.show_comments = active;
    state.config_updates = PluginConfigUpdates::from_port(port.clone());
    (Arc::new(Mutex::new(state)), port)
}

#[tokio::test]
async fn comment_commands_patch_only_the_switch_with_a_fresh_revision_and_wait_for_host_refresh() {
    for (action, enabled) in [
        (CommentsAction::Enable, true),
        (CommentsAction::Disable, false),
    ] {
        let (state, port) = fixture(!enabled, !enabled);
        {
            // 宿主尚未轮询的外部修改必须保留，命令应读取这个新版本。
            let mut stored = port.0.lock().unwrap();
            stored.snapshot.values["video_max_mb"] = json!(77);
            stored.snapshot.revision = "unpolled-external-edit".into();
        }
        let before = port.0.lock().unwrap().snapshot.clone();
        let message = CommentsHandler {
            state: state.clone(),
            action,
        }
        .response()
        .await
        .unwrap();
        assert!(message.contains(&format!("已保存：{}评论展示（全局）", label(enabled))));
        assert!(message.contains("后续解析"));
        assert!(message.contains("评论数统计仍保留"));
        let stored = port.0.lock().unwrap();
        assert_eq!(stored.reads, 1);
        assert_eq!(
            stored.patches,
            vec![(json!({"show_comments": enabled}), before.revision)]
        );
        let mut expected = before.values;
        expected["show_comments"] = json!(enabled);
        assert_eq!(stored.snapshot.values, expected);
        assert_eq!(stored.snapshot.revision, "saved-by-command");
        assert_eq!(
            state.lock().unwrap().config.show_comments,
            !enabled,
            "保存不能绕过宿主回调，直接修改正在使用的运行配置"
        );
    }
}

#[tokio::test]
async fn status_reads_saved_configuration_and_reports_a_pending_runtime_refresh() {
    for (active, saved) in [(true, false), (false, true)] {
        let (state, port) = fixture(active, saved);
        let message = CommentsHandler {
            state,
            action: CommentsAction::Status,
        }
        .response()
        .await
        .unwrap();
        assert!(message.contains(&format!("评论展示（全局）：{}。", label(saved))));
        assert!(message.contains(&format!("当前运行：{}，等待配置刷新", label(active))));
        let stored = port.0.lock().unwrap();
        assert_eq!(stored.reads, 1);
        assert!(stored.patches.is_empty());
    }
    let (state, port) = fixture(false, false);
    let message = CommentsHandler {
        state,
        action: CommentsAction::Status,
    }
    .response()
    .await
    .unwrap();
    assert!(message.contains("评论展示（全局）：关闭"));
    assert!(!message.contains("等待配置刷新"));
    assert!(port.0.lock().unwrap().patches.is_empty());
}

#[tokio::test]
async fn save_failure_does_not_publish_runtime_values_or_claim_success() {
    let (state, port) = fixture(false, false);
    port.0.lock().unwrap().failure = SaveFailure::Write;
    let before = port.0.lock().unwrap().snapshot.clone();
    let error = CommentsHandler {
        state: state.clone(),
        action: CommentsAction::Enable,
    }
    .response()
    .await
    .unwrap_err();
    assert!(error.to_string().contains("write failed"));
    assert_eq!(port.0.lock().unwrap().snapshot, before);
    assert!(!state.lock().unwrap().config.show_comments);
}

#[tokio::test]
async fn a_concurrent_file_revision_rejects_the_command_without_overwriting_the_external_save() {
    let (state, port) = fixture(false, true);
    port.0.lock().unwrap().failure = SaveFailure::Conflict;
    let error = CommentsHandler {
        state: state.clone(),
        action: CommentsAction::Enable,
    }
    .response()
    .await
    .unwrap_err();
    assert!(error.to_string().contains("revision conflict"));
    let stored = port.0.lock().unwrap();
    assert_eq!(stored.snapshot.revision, "newer-web-save");
    assert_eq!(stored.snapshot.values["show_comments"], false);
    assert_eq!(stored.snapshot.values["video_max_mb"], 42);
    assert_eq!(stored.patches.len(), 1, "不能静默重试覆盖新配置");
    assert!(!state.lock().unwrap().config.show_comments);
}

#[tokio::test]
async fn missing_persistence_and_invalid_saved_status_are_explicit_errors() {
    let state = Arc::new(Mutex::new(RuntimeState::default()));
    for action in [
        CommentsAction::Enable,
        CommentsAction::Disable,
        CommentsAction::Status,
    ] {
        assert!(
            CommentsHandler {
                state: state.clone(),
                action,
            }
            .response()
            .await
            .is_err()
        );
    }
    let (state, port) = fixture(true, true);
    port.0.lock().unwrap().snapshot.values["show_comments"] = json!("invalid");
    let error = CommentsHandler {
        state,
        action: CommentsAction::Status,
    }
    .response()
    .await
    .unwrap_err();
    assert!(error.to_string().contains("不是布尔值"));
    assert!(port.0.lock().unwrap().patches.is_empty());
}

#[test]
fn registered_comment_commands_use_the_existing_prefix_and_host_permissions() {
    let plugin = crate::plugin::LinkShowNN::default();
    let registered = plugin.commands();
    let config = plugin.service_config();
    assert_eq!(config.prefix, ["url"]);
    for (name, permission) in [
        ("开启评论", PermissionLevel::SuperUser),
        ("关闭评论", PermissionLevel::SuperUser),
        ("评论状态", PermissionLevel::GroupAdmin),
    ] {
        let matching: Vec<_> = registered.iter().filter(|c| c.name == name).collect();
        assert_eq!(matching.len(), 1);
        let command = matching[0];
        assert_eq!(command.permission, permission);
        assert!(command.block);
        assert!(command.message_handler.is_some());
        assert!(!command.description.is_empty());
    }
}
