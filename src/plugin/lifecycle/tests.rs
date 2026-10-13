//! Hot configuration changes use in-memory sessions and task scopes only.
//! No host, runtime database, QR client, or real platform request is loaded.

use super::*;
use std::{
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

use serde_json::{Value, json};
use tokio::sync::oneshot;

struct Fixture {
    plugin: LinkShowNN,
    tasks: Arc<PluginTaskScope>,
    directory: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let config = ResolverConfig {
            render_cards: false,
            live_enabled: false,
            concurrent_limit: 2,
            download_concurrent_limit: 1,
            download_queue_limit: 1,
            media_output_dir: directory.path().join("old-media").display().to_string(),
            ..ResolverConfig::default()
        };
        let tasks = Arc::new(PluginTaskScope::new("hot-config-fixture"));
        let downloads = DownloadManager::new(
            None,
            config.download_concurrent_limit,
            config.download_queue_limit,
            config.video_max_mb,
            config.resolve_media_dir(directory.path()),
        )
        .unwrap();
        let state = Arc::new(std::sync::Mutex::new(crate::runtime::state::RuntimeState {
            parser: Some(ParserManager::new(config.clone()).unwrap()),
            tasks: Some(tasks.clone()),
            downloads: Some(downloads),
            media_delivery_slots: Some(Arc::new(Semaphore::new(1))),
            config,
            ..Default::default()
        }));
        let plugin = LinkShowNN {
            state,
            limiter: Arc::new(Semaphore::new(2)),
            data_dir: Some(directory.path().to_path_buf()),
        };
        Self {
            plugin,
            tasks,
            directory,
        }
    }

    fn config(&self) -> Value {
        serde_json::to_value(&self.plugin.state.lock().unwrap().config).unwrap()
    }

    async fn cleanup(mut self) {
        self.plugin.on_unload().await.unwrap();
        assert!(self.tasks.is_closed());
    }
}

async fn bounded<T>(future: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(3), future)
        .await
        .expect("hot configuration fixture stalled")
}

struct DropSignal(Arc<AtomicBool>);

impl Drop for DropSignal {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

#[tokio::test]
async fn published_config_rebuilds_resources_without_interrupting_the_task_scope() {
    let mut fixture = Fixture::new();
    let dropped = Arc::new(AtomicBool::new(false));
    let marker = DropSignal(dropped.clone());
    let (started, running) = oneshot::channel();
    assert!(fixture.tasks.try_spawn("ongoing-fixture-task", async move {
        let _marker = marker;
        let _ = started.send(());
        std::future::pending::<()>().await;
    }));
    bounded(running).await.unwrap();
    let (old_download, slots) = {
        let state = fixture.plugin.state.lock().unwrap();
        (
            state.downloads.as_ref().unwrap().clone(),
            state.media_delivery_slots.clone().unwrap(),
        )
    };
    let old_reservation = old_download.try_reserve().unwrap();
    let occupied_slot = slots.acquire().await.unwrap();
    let old_limiter = fixture.plugin.limiter.clone();
    let media_dir = fixture.directory.path().join("new-media");
    let mut updated = fixture.config();
    for (key, value) in [
        ("enable_media_download", json!(true)),
        ("video_duration_maximum", json!(60)),
        ("video_max_mb", json!(1)),
        ("media_transport", json!("local")),
        ("media_output_dir", json!(media_dir)),
        ("concurrent_limit", json!(7)),
        ("download_concurrent_limit", json!(2)),
        ("download_queue_limit", json!(3)),
        ("render_cards", json!(true)),
        ("show_comments", json!(false)),
        ("bili_quality", json!("480P")),
    ] {
        updated[key] = value;
    }
    fixture.plugin.on_config_changed(&updated).await.unwrap();
    let (config, downloads) = {
        let state = fixture.plugin.state.lock().unwrap();
        assert!(Arc::ptr_eq(state.tasks.as_ref().unwrap(), &fixture.tasks));
        assert!(Arc::ptr_eq(
            state.media_delivery_slots.as_ref().unwrap(),
            &slots
        ));
        assert!(
            state
                .renderer
                .as_ref()
                .is_some_and(|renderer| !renderer.is_closed())
        );
        (
            state.config.clone(),
            state.downloads.as_ref().unwrap().clone(),
        )
    };
    assert!(config.enable_media_download);
    assert_eq!(config.video_duration_maximum, 60);
    assert_eq!(config.video_max_mb, 1);
    assert_eq!(config.media_transport, "local");
    assert_eq!(
        config.resolve_media_dir(fixture.directory.path()),
        media_dir
    );
    assert!(!config.show_comments);
    assert_eq!(config.bili_quality, "480P");
    assert_eq!(config.download_concurrent_limit, 2);
    assert_eq!(fixture.plugin.limiter.available_permits(), 7);
    assert!(!Arc::ptr_eq(&old_limiter, &fixture.plugin.limiter));
    assert!(
        !old_limiter.is_closed(),
        "accepted requests may finish using their old snapshot"
    );
    let reservations = (0..3)
        .map(|_| downloads.try_reserve().expect("new queue permits new work"))
        .collect::<Vec<_>>();
    assert!(downloads.try_reserve().is_none());
    assert!(
        old_download.try_reserve().is_none(),
        "old in-flight download remains owned"
    );
    assert_eq!(
        slots.available_permits(),
        0,
        "config must not reopen media delivery slots"
    );
    assert!(!fixture.tasks.is_closed());
    assert!(
        !dropped.load(Ordering::SeqCst),
        "hot update must not shut down ongoing work"
    );
    drop((reservations, old_reservation, occupied_slot));
    fixture.cleanup().await;
    assert!(dropped.load(Ordering::SeqCst));
}

#[tokio::test]
async fn rejected_schema_and_client_candidates_keep_the_previous_runtime_snapshot() {
    let mut fixture = Fixture::new();
    let previous = fixture.config();
    let old_limiter = fixture.plugin.limiter.clone();
    let old_download = fixture
        .plugin
        .state
        .lock()
        .unwrap()
        .downloads
        .as_ref()
        .unwrap()
        .clone();
    let reservation = old_download.try_reserve().unwrap();
    for failure in [json!({"concurrent_limit": 0}), json!({"proxy": "http://["})] {
        let mut updated = previous.clone();
        updated["enable_media_download"] = json!(true);
        updated["download_queue_limit"] = json!(3);
        for (key, value) in failure.as_object().unwrap() {
            updated[key] = value.clone();
        }
        assert!(fixture.plugin.on_config_changed(&updated).await.is_err());
        assert_eq!(fixture.config(), previous);
        assert!(Arc::ptr_eq(&fixture.plugin.limiter, &old_limiter));
        let state = fixture.plugin.state.lock().unwrap();
        assert!(state.renderer.is_none());
        assert!(state.downloads.as_ref().unwrap().try_reserve().is_none());
        assert!(Arc::ptr_eq(state.tasks.as_ref().unwrap(), &fixture.tasks));
        assert!(!fixture.tasks.is_closed());
    }
    drop(reservation);
    fixture.cleanup().await;
}

#[tokio::test]
async fn update_waits_for_account_mutations_and_preserves_the_latest_sessions() {
    let fixture = Fixture::new();
    let previous = fixture.config();
    let mut updated = previous.clone();
    updated["show_comments"] = json!(false);
    updated["video_duration_maximum"] = json!(120);
    let state = fixture.plugin.state.clone();
    let mutations = state.lock().unwrap().auth_mutations.clone();
    let mutation = mutations.lock().await;
    let (started, entered) = oneshot::channel();
    let Fixture {
        mut plugin,
        tasks,
        directory,
    } = fixture;
    let callback = tokio::spawn(async move {
        let _ = started.send(());
        let result = plugin.on_config_changed(&updated).await;
        (plugin, result)
    });
    bounded(entered).await.unwrap();
    assert_eq!(
        serde_json::to_value(&state.lock().unwrap().config).unwrap(),
        previous
    );
    // Model an already-running login publishing while it owns the shared lock.
    // These are fixture strings, never persisted or sent to any service.
    let sessions = SessionCookies {
        bili: Some("SESSDATA=fixture-latest-bili; bili_jct=fixture-csrf".into()),
        ncm: Some("MUSIC_U=fixture-latest-ncm".into()),
        qqmusic: Some("qqmusic_key=fixture-latest-qq; qqmusic_uin=123".into()),
    };
    state.lock().unwrap().parser = Some(
        ParserManager::with_sessions(ResolverConfig::load(previous).unwrap(), sessions.clone())
            .unwrap(),
    );
    drop(mutation);
    let (plugin, result) = bounded(callback).await.unwrap();
    result.unwrap();
    let current = state
        .lock()
        .unwrap()
        .parser
        .as_ref()
        .unwrap()
        .session_cookies();
    assert_eq!(current.bili, sessions.bili);
    assert_eq!(current.ncm, sessions.ncm);
    assert_eq!(current.qqmusic, sessions.qqmusic);
    assert_eq!(state.lock().unwrap().config.video_duration_maximum, 120);
    assert!(!state.lock().unwrap().config.show_comments);
    Fixture {
        plugin,
        tasks,
        directory,
    }
    .cleanup()
    .await;
}

#[tokio::test]
async fn disabling_live_stops_only_its_worker_and_keeps_other_tasks_available() {
    let mut fixture = Fixture::new();
    fixture.plugin.state.lock().unwrap().config.live_enabled = true;
    let poller = fixture.plugin.state.lock().unwrap().live_poller.clone();
    let store = LiveStore::load(fixture.directory.path().join("fixture-subscriptions.json"))
        .await
        .unwrap();
    let dropped = Arc::new(AtomicBool::new(false));
    let marker = DropSignal(dropped.clone());
    let (started, entered) = oneshot::channel();
    let mut first = Some((marker, started));
    poller
        .start_polling(&fixture.tasks, store, Duration::from_secs(60), move |_| {
            let first = first.take();
            async move {
                let (_marker, started) = first.unwrap();
                let _ = started.send(());
                std::future::pending::<()>().await;
            }
        })
        .await;
    bounded(entered).await.unwrap();
    let mut updated = fixture.config();
    updated["live_enabled"] = json!(false);
    bounded(fixture.plugin.on_config_changed(&updated))
        .await
        .unwrap();
    assert!(dropped.load(Ordering::SeqCst));
    assert!(!fixture.tasks.is_closed());
    let (done, finished) = oneshot::channel();
    assert!(fixture.tasks.try_spawn("still-available", async move {
        let _ = done.send(());
    }));
    bounded(finished).await.unwrap();
    fixture.cleanup().await;
}
