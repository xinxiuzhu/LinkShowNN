//! 订阅文件读写和订阅集合；不持有宿主任务或轮询状态。

use std::{collections::BTreeMap, path::PathBuf, sync::Arc};

use anyhow::Result;
use tokio::{fs, sync::RwLock};

use super::subscription::LiveSubscription;

#[derive(Clone)]
pub struct LiveStore {
    path: PathBuf,
    subscriptions: Arc<RwLock<BTreeMap<String, LiveSubscription>>>,
}

impl LiveStore {
    pub async fn load(path: PathBuf) -> Result<Self> {
        let subscriptions = if let Ok(data) = fs::read(&path).await {
            serde_json::from_slice(&data).unwrap_or_default()
        } else {
            BTreeMap::new()
        };
        Ok(Self {
            path,
            subscriptions: Arc::new(RwLock::new(subscriptions)),
        })
    }

    pub async fn save(&self) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).await?;
        }
        let value = self.subscriptions.read().await.clone();
        fs::write(&self.path, serde_json::to_vec_pretty(&value)?).await?;
        Ok(())
    }

    pub async fn subscribe(
        &self,
        key: String,
        subscription: LiveSubscription,
        limit: usize,
    ) -> Result<bool> {
        let mut values = self.subscriptions.write().await;
        if !values.contains_key(&key) && values.len() >= limit {
            return Ok(false);
        }
        values.insert(key, subscription);
        drop(values);
        self.save().await?;
        Ok(true)
    }

    pub async fn unsubscribe(&self, key: &str) -> Result<bool> {
        let removed = self.subscriptions.write().await.remove(key).is_some();
        if removed {
            self.save().await?;
        }
        Ok(removed)
    }

    pub async fn list(&self) -> Vec<LiveSubscription> {
        self.subscriptions.read().await.values().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::live::subscription::SubscriptionRoute;

    #[tokio::test]
    async fn subscription_persists_structured_route() {
        let path = std::env::temp_dir().join(format!("nuono-live-{}.json", uuid::Uuid::new_v4()));
        let store = LiveStore::load(path.clone()).await.unwrap();
        let route = SubscriptionRoute {
            platform: "telegram".into(),
            instance_id: "main".into(),
            bot_self_id: "bot".into(),
            target_type: "group".into(),
            target_id: "chat".into(),
            target_parent: None,
        };
        assert!(
            store
                .subscribe(
                    "telegram:main:chat".into(),
                    LiveSubscription {
                        room_id: 1,
                        route: route.clone()
                    },
                    10
                )
                .await
                .unwrap()
        );
        let restored = LiveStore::load(path.clone()).await.unwrap();
        assert_eq!(restored.list().await[0].route, route);
        let _ = fs::remove_file(path).await;
    }
}
