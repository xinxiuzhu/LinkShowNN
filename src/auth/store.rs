//! Platform sessions stored in Nuono's private plugin secrets.

use anyhow::{Result, anyhow};
use async_trait::async_trait;
use nuono_plugin::PluginSecretStore;
use std::sync::Arc;

pub(crate) const BILIBILI_SESSION_KEY: &str = "bili_qr_session";

#[async_trait]
pub(crate) trait CredentialStore: Send + Sync {
    async fn read(&self, key: &str) -> Result<Option<String>>;
    async fn write(&self, key: &str, value: &str) -> Result<()>;
    async fn remove(&self, key: &str) -> Result<()>;
}

pub(crate) fn database_store(secrets: PluginSecretStore) -> Arc<dyn CredentialStore> {
    Arc::new(DatabaseCredentialStore { secrets })
}

// Deliberately has no Debug implementation: the stored session is private.
struct DatabaseCredentialStore {
    secrets: PluginSecretStore,
}

#[async_trait]
impl CredentialStore for DatabaseCredentialStore {
    async fn read(&self, key: &str) -> Result<Option<String>> {
        self.secrets
            .get_private(key)
            .await
            .map_err(|_| anyhow!("cannot read platform credentials"))
    }

    async fn write(&self, key: &str, value: &str) -> Result<()> {
        self.secrets
            .set_private(key, value)
            .await
            .map_err(|_| anyhow!("cannot store platform credentials"))
    }

    async fn remove(&self, key: &str) -> Result<()> {
        self.secrets
            .delete_private(key)
            .await
            .map_err(|_| anyhow!("cannot remove platform credentials"))
    }
}

#[cfg(test)]
#[derive(Default)]
pub(crate) struct MemoryCredentialStore {
    values: tokio::sync::Mutex<std::collections::BTreeMap<String, String>>,
    read_failure: std::sync::atomic::AtomicBool,
    write_failure: std::sync::atomic::AtomicBool,
    remove_failure: std::sync::atomic::AtomicBool,
}

#[cfg(test)]
impl MemoryCredentialStore {
    pub(crate) fn fail_reads(&self, enabled: bool) {
        self.read_failure
            .store(enabled, std::sync::atomic::Ordering::SeqCst);
    }

    pub(crate) fn fail_writes(&self, enabled: bool) {
        self.write_failure
            .store(enabled, std::sync::atomic::Ordering::SeqCst);
    }

    pub(crate) fn fail_removes(&self, enabled: bool) {
        self.remove_failure
            .store(enabled, std::sync::atomic::Ordering::SeqCst);
    }
}

#[cfg(test)]
#[async_trait]
impl CredentialStore for MemoryCredentialStore {
    async fn read(&self, key: &str) -> Result<Option<String>> {
        if self.read_failure.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(anyhow!("cannot read platform credentials"));
        }
        Ok(self.values.lock().await.get(key).cloned())
    }

    async fn write(&self, key: &str, value: &str) -> Result<()> {
        if self.write_failure.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(anyhow!("cannot store platform credentials"));
        }
        let mut values = self.values.lock().await;
        if value.is_empty() {
            values.remove(key);
        } else {
            values.insert(key.to_owned(), value.to_owned());
        }
        Ok(())
    }

    async fn remove(&self, key: &str) -> Result<()> {
        if self
            .remove_failure
            .load(std::sync::atomic::Ordering::SeqCst)
        {
            return Err(anyhow!("cannot remove platform credentials"));
        }
        self.values.lock().await.remove(key);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn fixture_keeps_platform_entries_separate() {
        let store = MemoryCredentialStore::default();
        assert!(store.read(BILIBILI_SESSION_KEY).await.unwrap().is_none());
        store
            .write(BILIBILI_SESSION_KEY, "fake-bili")
            .await
            .unwrap();
        store.write("another_platform", "fake-other").await.unwrap();
        store.remove(BILIBILI_SESSION_KEY).await.unwrap();
        assert!(store.read(BILIBILI_SESSION_KEY).await.unwrap().is_none());
        assert_eq!(
            store.read("another_platform").await.unwrap().as_deref(),
            Some("fake-other")
        );
        store.write("another_platform", "").await.unwrap();
        assert!(store.read("another_platform").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn failed_mutations_preserve_the_previous_session() {
        let store = MemoryCredentialStore::default();
        store.write(BILIBILI_SESSION_KEY, "fake-old").await.unwrap();
        store.fail_writes(true);
        let write_error = store
            .write(BILIBILI_SESSION_KEY, "private-fake-key")
            .await
            .unwrap_err();
        assert_eq!(write_error.to_string(), "cannot store platform credentials");
        assert!(!format!("{write_error:?}").contains("private-fake-key"));
        store.fail_removes(true);
        let remove_error = store.remove(BILIBILI_SESSION_KEY).await.unwrap_err();
        assert_eq!(
            remove_error.to_string(),
            "cannot remove platform credentials"
        );
        assert_eq!(
            store.read(BILIBILI_SESSION_KEY).await.unwrap().as_deref(),
            Some("fake-old")
        );
    }

    #[tokio::test]
    async fn failed_reads_do_not_report_a_missing_session_or_expose_the_key() {
        let store = MemoryCredentialStore::default();
        store.fail_reads(true);
        let error = store.read("private-fake-key").await.unwrap_err();
        assert_eq!(error.to_string(), "cannot read platform credentials");
        assert!(!format!("{error:?}").contains("private-fake-key"));
        store.fail_reads(false);
        assert!(store.read("private-fake-key").await.unwrap().is_none());
    }
}
