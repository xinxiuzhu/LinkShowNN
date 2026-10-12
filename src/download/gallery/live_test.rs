//! Opt-in live preparation, followed by an in-memory route, never a real chat.

use anyhow::{Result, ensure};
use nuono_plugin::api::waiter::WaitManager;
use nuono_plugin::core::{RuntimeActionRouter, event_bus::EventBus, onebot_client::ApiClient};
use nuono_plugin::{Bot, Event, PluginTaskScope, RuntimeActionSink, RuntimeBotRoute};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

use crate::{delivery, resolver::Card};

struct CapturedSink(Arc<Mutex<Vec<Value>>>);

#[async_trait::async_trait]
impl RuntimeActionSink for CapturedSink {
    async fn send_action(&self, _: Value) -> nuono_plugin::NuonoResult<()> {
        panic!("gallery preview must use confirmed calls");
    }

    async fn call_action(&self, action: Value) -> nuono_plugin::NuonoResult<Value> {
        if action["action"] == "get_login_info" {
            return Ok(json!({"user_id":"999999", "nickname":"LinkShowNN 测试"}));
        }
        self.0.lock().unwrap().push(action);
        Ok(json!({"message_id":"local-gallery-preview"}))
    }
}

pub(crate) async fn preview(
    card: Card,
    proxy: &str,
    tasks: Arc<PluginTaskScope>,
    output: &std::path::Path,
) -> Result<()> {
    let total = card.image_urls().count();
    let text = delivery::gallery_text(&card);
    let gallery = super::prepare(card, proxy, tasks.clone()).await?;
    let captured = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::new(CapturedSink(captured.clone()));
    let route = RuntimeBotRoute::new("onebot_v11", "local-gallery-preview", "999999");
    let router = Arc::new(RuntimeActionRouter::new());
    router.register(route.clone(), sink.clone());
    let bot = Arc::new(
        Bot::new(
            999999,
            Arc::new(EventBus::new()),
            Arc::new(WaitManager::new()),
            Arc::new(ApiClient::new()),
            None,
            Some(sink),
        )
        .with_action_router(router),
    );
    let event = json!({"route":route, "post_type":"message", "message_type":"group", "group_id":"123456", "user_id":"888888", "message_id":"local-preview"});
    let target = delivery::ReplyTarget::new(bot, &Event::new(&event))?;
    delivery::send_scoped(target, tasks, delivery::PreparedReply::Gallery(gallery)).await?;
    let mut actions = captured.lock().unwrap();
    ensure!(
        actions.len() == 1,
        "gallery preview must produce one sending action"
    );
    let mut action = actions.pop().unwrap();
    drop(actions);
    let (nodes, segments) = if total > 3 {
        ensure!(
            action["action"] == "send_group_forward_msg",
            "gallery did not use a forward"
        );
        let nodes = action["params"]["messages"].as_array_mut().unwrap();
        let count = nodes.len();
        let segments = nodes
            .iter_mut()
            .flat_map(|node| node["data"]["content"].as_array_mut().unwrap())
            .collect::<Vec<_>>();
        (count, segments)
    } else {
        ensure!(
            action["action"] == "send_msg",
            "small gallery did not use one message"
        );
        (
            1,
            action["params"]["message"]
                .as_array_mut()
                .unwrap()
                .iter_mut()
                .collect::<Vec<_>>(),
        )
    };
    let mut images = 0;
    for segment in segments {
        if segment["type"] == "image" {
            ensure!(
                segment["data"]["file"]
                    .as_str()
                    .is_some_and(|s| s.starts_with("base64://")),
                "gallery image was not prepared as SDK bytes"
            );
            images += 1;
            // Save inspectable structure, never dump large encoded media into logs.
            segment["data"]["file"] = json!("<SDK 图片字节>");
        }
    }
    ensure!(
        images == total.min(delivery::MAX_GALLERY_IMAGES),
        "live gallery lost requested pictures"
    );
    tokio::fs::create_dir_all(output).await?;
    tokio::fs::write(
        output.join("public-gallery-delivery.json"),
        serde_json::to_vec_pretty(&action)?,
    )
    .await?;
    tokio::fs::write(output.join("public-gallery-text.txt"), text).await?;
    println!(
        "gallery delivery preview: original_images={total}, sent_images={images}, nodes={nodes}, in-memory route only"
    );
    Ok(())
}
