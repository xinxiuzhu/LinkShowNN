use super::*;
use crate::{auth::store::MemoryCredentialStore, config::ResolverConfig};
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Default)]
struct BlockingStore {
    memory: MemoryCredentialStore,
    entered: tokio::sync::Notify,
    release: tokio::sync::Notify,
    writes: AtomicUsize,
}

#[async_trait::async_trait]
impl CredentialStore for BlockingStore {
    async fn read(&self, key: &str) -> Result<Option<String>> {
        self.memory.read(key).await
    }
    async fn write(&self, key: &str, value: &str) -> Result<()> {
        self.writes.fetch_add(1, Ordering::SeqCst);
        if key == MusicPlatform::Ncm.session_key() {
            self.entered.notify_one();
            self.release.notified().await;
        }
        self.memory.write(key, value).await
    }
    async fn remove(&self, key: &str) -> Result<()> {
        self.memory.remove(key).await
    }
}

fn rig() -> (
    Arc<Mutex<RuntimeState>>,
    Arc<MemoryCredentialStore>,
    Arc<dyn CredentialStore>,
) {
    let config = ResolverConfig::default();
    let state = Arc::new(Mutex::new(RuntimeState {
        parser: Some(ParserManager::new(config.clone()).unwrap()),
        config,
        ..RuntimeState::default()
    }));
    let memory = Arc::new(MemoryCredentialStore::default());
    (state, memory.clone(), memory)
}

fn music(platform: MusicPlatform, value: &str) -> MusicCredentials {
    let cookie = match platform {
        MusicPlatform::Ncm => format!("MUSIC_U={value}; __csrf=fixture-csrf"),
        MusicPlatform::QqMusic => format!("qqmusic_key={value}; qqmusic_uin=123; login_type=2"),
    };
    MusicCredentials::new(platform, cookie, "123".into(), None).unwrap()
}

#[tokio::test]
async fn concurrent_music_updates_and_bili_clear_preserve_other_platforms() {
    let (state, store, port) = rig();
    let ncm = music(MusicPlatform::Ncm, "fixture-ncm");
    let qq = music(MusicPlatform::QqMusic, "fixture-qq");
    let (one, two) = tokio::join!(
        apply_music(&state, &port, MusicPlatform::Ncm, Some(&ncm)),
        apply_music(&state, &port, MusicPlatform::QqMusic, Some(&qq)),
    );
    one.unwrap();
    two.unwrap();
    let bili = Credentials::decode(&serde_json::json!({
        "version":1,
        "cookies":[["SESSDATA","fixture-bili"],["bili_jct","fixture-csrf"],["DedeUserID","456"]],
        "refresh_token":"fixture-refresh"
    }).to_string()).unwrap();
    apply_bili(&state, &port, Some(&bili)).await.unwrap();
    let sessions = state
        .lock()
        .unwrap()
        .parser
        .as_ref()
        .unwrap()
        .session_cookies();
    assert_eq!(sessions.ncm.as_deref(), Some(ncm.cookie_header()));
    assert_eq!(sessions.qqmusic.as_deref(), Some(qq.cookie_header()));
    assert_eq!(sessions.bili, Some(bili.cookie_header()));
    apply_bili(&state, &port, None).await.unwrap();
    apply_music(&state, &port, MusicPlatform::Ncm, None)
        .await
        .unwrap();
    let sessions = state
        .lock()
        .unwrap()
        .parser
        .as_ref()
        .unwrap()
        .session_cookies();
    assert!(sessions.bili.is_none());
    assert!(sessions.ncm.is_none());
    assert_eq!(sessions.qqmusic.as_deref(), Some(qq.cookie_header()));
    assert!(store.read(BILIBILI_SESSION_KEY).await.unwrap().is_none());
    assert!(
        store
            .read(MusicPlatform::Ncm.session_key())
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .read(MusicPlatform::QqMusic.session_key())
            .await
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn rejected_storage_and_parser_candidates_keep_the_saved_snapshot() {
    let (state, store, port) = rig();
    let old = music(MusicPlatform::Ncm, "fixture-old");
    let new = music(MusicPlatform::Ncm, "fixture-new");
    apply_music(&state, &port, MusicPlatform::Ncm, Some(&old))
        .await
        .unwrap();
    store.fail_writes(true);
    assert!(
        apply_music(&state, &port, MusicPlatform::Ncm, Some(&new))
            .await
            .is_err()
    );
    store.fail_writes(false);
    store.fail_removes(true);
    assert!(
        apply_music(&state, &port, MusicPlatform::Ncm, None)
            .await
            .is_err()
    );
    store.fail_removes(false);
    state.lock().unwrap().config.proxy = "not a proxy://".into();
    assert!(
        apply_music(&state, &port, MusicPlatform::Ncm, Some(&new))
            .await
            .is_err()
    );
    let sessions = state
        .lock()
        .unwrap()
        .parser
        .as_ref()
        .unwrap()
        .session_cookies();
    assert_eq!(sessions.ncm.as_deref(), Some(old.cookie_header()));
    assert_eq!(
        store.read(MusicPlatform::Ncm.session_key()).await.unwrap(),
        Some(old.encode().unwrap())
    );
}

#[tokio::test]
async fn a_session_for_another_platform_cannot_be_saved() {
    let (state, store, port) = rig();
    let qq = music(MusicPlatform::QqMusic, "fixture-qq");
    assert!(
        apply_music(&state, &port, MusicPlatform::Ncm, Some(&qq))
            .await
            .is_err()
    );
    assert!(
        store
            .read(MusicPlatform::Ncm.session_key())
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        state
            .lock()
            .unwrap()
            .parser
            .as_ref()
            .unwrap()
            .session_cookies()
            .ncm
            .is_none()
    );
}

#[tokio::test]
async fn overlapping_platform_commits_wait_for_storage_and_keep_both_sessions() {
    let (state, _, _) = rig();
    let store = Arc::new(BlockingStore::default());
    let port: Arc<dyn CredentialStore> = store.clone();
    let ncm = music(MusicPlatform::Ncm, "fixture-overlapping-ncm");
    let qq = music(MusicPlatform::QqMusic, "fixture-overlapping-qq");
    let ncm_state = state.clone();
    let ncm_port = port.clone();
    let ncm_saved = ncm.clone();
    let first = tokio::spawn(async move {
        apply_music(&ncm_state, &ncm_port, MusicPlatform::Ncm, Some(&ncm_saved)).await
    });
    store.entered.notified().await;
    let (started, start) = tokio::sync::oneshot::channel();
    let qq_state = state.clone();
    let qq_saved = qq.clone();
    let second = tokio::spawn(async move {
        let _ = started.send(());
        apply_music(&qq_state, &port, MusicPlatform::QqMusic, Some(&qq_saved)).await
    });
    start.await.unwrap();
    tokio::task::yield_now().await;
    assert_eq!(store.writes.load(Ordering::SeqCst), 1);
    assert!(!second.is_finished());
    store.release.notify_one();
    first.await.unwrap().unwrap();
    second.await.unwrap().unwrap();
    let sessions = state
        .lock()
        .unwrap()
        .parser
        .as_ref()
        .unwrap()
        .session_cookies();
    assert_eq!(sessions.ncm.as_deref(), Some(ncm.cookie_header()));
    assert_eq!(sessions.qqmusic.as_deref(), Some(qq.cookie_header()));
}
