//! 控制器离线回归：虚拟平台、私聊回执与私有凭据存储。

use super::*;
use std::{collections::VecDeque, sync::atomic::AtomicUsize};

use async_trait::async_trait;
use nuono_plugin::{
    Bot, Event, NuonoError, NuonoResult, RuntimeActionSink, RuntimeBotRoute,
    api::waiter::WaitManager,
    core::{RuntimeActionRouter, event_bus::EventBus, onebot_client::ApiClient},
    permission::{PermissionSettings, PermissionStore},
};
use serde_json::{Value, json};

use crate::{
    auth::{music::QrChallenge, store::MemoryCredentialStore},
    config::ResolverConfig,
    resolver::{ParserManager, SessionCookies},
};

const OPERATOR: &str = "000800000000000000000088";
const FAKE_QR_KEY: &str = "fixture-music-qr-private-key";
const FAKE_COOKIE: &str = "fixture-music-private-cookie";

fn credentials(platform: MusicPlatform, uid: &str) -> MusicCredentials {
    let name = match platform {
        MusicPlatform::Ncm => "MUSIC_U",
        MusicPlatform::QqMusic => "qqmusic_key",
    };
    MusicCredentials::new(platform, format!("{name}={FAKE_COOKIE}"), uid.into(), None).unwrap()
}

#[derive(Clone, Default)]
struct PermissionFixture {
    fail: Arc<AtomicBool>,
}

#[async_trait]
impl PermissionStore for PermissionFixture {
    async fn permission_settings(&self) -> std::result::Result<PermissionSettings, String> {
        if self.fail.load(Ordering::Acquire) {
            return Err("fixture permission failure with private token".into());
        }
        Ok(PermissionSettings {
            masters: Some(json!([])),
            superusers: Some(json!([OPERATOR])),
            banned_users: Some(json!(["banned-operator"])),
        })
    }
}

struct FakeProtocol {
    polls: AsyncMutex<VecDeque<QrPoll>>,
    generated: AtomicUsize,
    polled: AtomicUsize,
    block_generate: AtomicBool,
    fail_generate: AtomicBool,
    gate: tokio::sync::Notify,
    ttl: Duration,
}

impl Default for FakeProtocol {
    fn default() -> Self {
        Self {
            polls: AsyncMutex::new(VecDeque::new()),
            generated: AtomicUsize::new(0),
            polled: AtomicUsize::new(0),
            block_generate: AtomicBool::new(false),
            fail_generate: AtomicBool::new(false),
            gate: tokio::sync::Notify::new(),
            ttl: Duration::from_secs(60),
        }
    }
}

#[async_trait]
impl QrProtocol for FakeProtocol {
    async fn generate(&self) -> Result<QrChallenge> {
        self.generated.fetch_add(1, Ordering::AcqRel);
        if self.block_generate.load(Ordering::Acquire) {
            self.gate.notified().await;
        }
        if self.fail_generate.load(Ordering::Acquire) {
            return Err(anyhow!(
                "transport https://example.invalid?secret={FAKE_QR_KEY}; Cookie={FAKE_COOKIE}"
            ));
        }
        QrChallenge::new(
            QrImage::Url(format!("https://example.invalid/qr?key={FAKE_QR_KEY}")),
            FAKE_QR_KEY.into(),
            self.ttl,
        )
    }

    async fn poll(&self, _qr: &QrChallenge) -> Result<QrPoll> {
        self.polled.fetch_add(1, Ordering::AcqRel);
        Ok(self
            .polls
            .lock()
            .await
            .pop_front()
            .unwrap_or(QrPoll::WaitingForScan))
    }
}

#[derive(Default)]
struct RecordingSink {
    calls: Mutex<Vec<Value>>,
    fail: AtomicBool,
}

#[async_trait]
impl RuntimeActionSink for RecordingSink {
    async fn send_action(&self, _action: Value) -> NuonoResult<()> {
        panic!("QR messages must wait for confirmed receipts");
    }
    async fn call_action(&self, action: Value) -> NuonoResult<Value> {
        self.calls.lock().unwrap().push(action);
        if self.fail.load(Ordering::Acquire) {
            Err(NuonoError::MessageSend("fixture send failure".into()))
        } else {
            Ok(json!({"message_id":"fixture-confirmed"}))
        }
    }
}

struct Rig {
    service: Arc<MusicLogin>,
    state: Arc<Mutex<RuntimeState>>,
    store: Arc<MemoryCredentialStore>,
    protocol: Arc<FakeProtocol>,
    permission: Arc<PermissionFixture>,
    tasks: Arc<PluginTaskScope>,
    route: RuntimeBotRoute,
    bot: Arc<Bot>,
    sink: Arc<RecordingSink>,
    other_sink: Arc<RecordingSink>,
}

impl Rig {
    async fn new(platform: MusicPlatform, saved: Option<&MusicCredentials>) -> Self {
        Self::with_protocol(platform, saved, Arc::new(FakeProtocol::default())).await
    }

    async fn with_protocol(
        platform: MusicPlatform,
        saved: Option<&MusicCredentials>,
        protocol: Arc<FakeProtocol>,
    ) -> Self {
        let tasks = Arc::new(PluginTaskScope::new(
            "linkshownn-music-login-controller-test",
        ));
        let config = ResolverConfig::default();
        let mut sessions = SessionCookies::default();
        if let Some(saved) = saved {
            match platform {
                MusicPlatform::Ncm => sessions.ncm = Some(saved.cookie_header().to_owned()),
                MusicPlatform::QqMusic => sessions.qqmusic = Some(saved.cookie_header().to_owned()),
            }
        }
        let parser = ParserManager::with_sessions(config.clone(), sessions).unwrap();
        let state = Arc::new(Mutex::new(RuntimeState {
            config,
            parser: Some(parser),
            ..RuntimeState::default()
        }));
        let store = Arc::new(MemoryCredentialStore::default());
        if let Some(saved) = saved {
            store
                .write(platform.session_key(), &saved.encode().unwrap())
                .await
                .unwrap();
        }
        let permission = Arc::new(PermissionFixture::default());
        let service = MusicLogin::new(
            platform,
            &state,
            tasks.clone(),
            Arc::new(PermissionManager::new(permission.as_ref().clone())),
            store.clone(),
            protocol.clone(),
            saved,
        );
        let route = RuntimeBotRoute::new("onebot", "music-login-instance", "00011");
        let sink = Arc::new(RecordingSink::default());
        let other_sink = Arc::new(RecordingSink::default());
        let router = Arc::new(RuntimeActionRouter::new());
        router.register(route.clone(), sink.clone());
        router.register(
            RuntimeBotRoute::new("onebot", "other-instance", "00011"),
            other_sink.clone(),
        );
        let bot = Arc::new(
            Bot::new(
                999,
                Arc::new(EventBus::new()),
                Arc::new(WaitManager::new()),
                Arc::new(ApiClient::new()),
                None,
                Some(other_sink.clone()),
            )
            .with_action_router(router),
        );
        Self {
            service,
            state,
            store,
            protocol,
            permission,
            tasks,
            route,
            bot,
            sink,
            other_sink,
        }
    }

    fn event(&self, kind: &str, uid: &str) -> Value {
        json!({"post_type":"message", "message_type":kind, "user_id":uid, "group_id":"fixture-group", "route":self.route, "message_id":"0007", "raw_message":"url网易云登录", "sender":{"role":"member"}})
    }
    fn target(&self) -> ReplyTarget {
        ReplyTarget::new(
            self.bot.clone(),
            &Event::new(&self.event("private", OPERATOR)),
        )
        .unwrap()
    }
    async fn pending(&self) -> (Uuid, watch::Receiver<bool>) {
        let id = Uuid::new_v4();
        let (cancel, receiver) = watch::channel(false);
        self.service.control.lock().await.pending = Some(Pending { id, cancel });
        (id, receiver)
    }
    async fn finish_task(&self) {
        tokio::time::timeout(Duration::from_secs(10), async {
            while self.service.control.lock().await.pending.is_some() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("fixture music task did not finish");
    }
    fn sent(&self) -> Vec<Value> {
        assert!(self.other_sink.calls.lock().unwrap().is_empty());
        self.sink.calls.lock().unwrap().clone()
    }

    fn active_cookie(&self) -> Option<String> {
        let sessions = self
            .state
            .lock()
            .unwrap()
            .parser
            .as_ref()
            .unwrap()
            .session_cookies();
        match self.service.platform {
            MusicPlatform::Ncm => sessions.ncm,
            MusicPlatform::QqMusic => sessions.qqmusic,
        }
    }
}

#[tokio::test]
async fn successful_qr_login_persists_then_reports_only_account_to_exact_private_route() {
    for platform in [MusicPlatform::Ncm, MusicPlatform::QqMusic] {
        let rig = Rig::new(platform, None).await;
        let account = credentials(platform, "100003");
        rig.protocol
            .polls
            .lock()
            .await
            .push_back(QrPoll::Success(account.clone()));
        rig.service.start(rig.target()).await.unwrap();
        rig.finish_task().await;
        assert_eq!(rig.protocol.generated.load(Ordering::Acquire), 1);
        assert_eq!(rig.protocol.polled.load(Ordering::Acquire), 1);
        let store: Arc<dyn CredentialStore> = rig.store.clone();
        assert!(
            MusicLogin::read_saved(platform, &store)
                .await
                .unwrap()
                .unwrap()
                == account
        );
        assert_eq!(
            rig.active_cookie().as_deref(),
            Some(account.cookie_header())
        );
        assert!(rig.service.status().await.contains("账号：100003"));
        let calls = rig.sent();
        assert_eq!(calls.len(), 2);
        for call in &calls {
            assert_eq!(call["action"], "send_msg");
            assert_eq!(call["params"]["message_type"], "private");
            assert_eq!(call["params"]["user_id"], OPERATOR);
            assert!(!call.to_string().contains(FAKE_QR_KEY));
            assert!(!call.to_string().contains(FAKE_COOKIE));
        }
        let first = calls[0]["params"]["message"].as_array().unwrap();
        assert_eq!(first.len(), 2);
        assert_eq!(first[1]["type"], "image");
        assert!(
            first[0]["data"]["text"]
                .as_str()
                .unwrap()
                .contains("供 LinkShowNN 解析使用")
        );
        assert!(calls[1].to_string().contains("账号：100003"));
        rig.tasks.shutdown().await;
    }
}

#[tokio::test]
async fn failed_qr_receipt_prevents_poll_and_credentials_from_being_saved() {
    let rig = Rig::new(MusicPlatform::Ncm, None).await;
    rig.sink.fail.store(true, Ordering::Release);
    rig.protocol
        .polls
        .lock()
        .await
        .push_back(QrPoll::Success(credentials(MusicPlatform::Ncm, "100004")));
    rig.service.start(rig.target()).await.unwrap();
    rig.finish_task().await;
    assert_eq!(rig.protocol.polled.load(Ordering::Acquire), 0);
    assert!(
        rig.store
            .read(MusicPlatform::Ncm.session_key())
            .await
            .unwrap()
            .is_none()
    );
    assert!(rig.service.status().await.starts_with("未保存"));
    rig.tasks.shutdown().await;
}

#[tokio::test]
async fn cancelled_and_stale_success_cannot_write_or_finish_a_newer_session() {
    let rig = Rig::new(MusicPlatform::QqMusic, None).await;
    let account = credentials(MusicPlatform::QqMusic, "100005");
    let (old, cancelled) = rig.pending().await;
    assert!(rig.service.cancel().await);
    assert!(*cancelled.borrow());
    assert!(!rig.service.commit(old, &account).await.unwrap());
    let (new, _) = rig.pending().await;
    assert!(!rig.service.commit(old, &account).await.unwrap());
    rig.service.finish(old).await;
    assert_eq!(
        rig.service
            .control
            .lock()
            .await
            .pending
            .as_ref()
            .unwrap()
            .id,
        new
    );
    assert!(
        rig.store
            .read(MusicPlatform::QqMusic.session_key())
            .await
            .unwrap()
            .is_none()
    );
    rig.service.cancel().await;
    rig.tasks.shutdown().await;
}

#[tokio::test]
async fn pending_generation_can_be_cancelled_and_rejects_duplicate_start() {
    let rig = Rig::new(MusicPlatform::Ncm, None).await;
    rig.protocol.block_generate.store(true, Ordering::Release);
    rig.service.start(rig.target()).await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        while rig.protocol.generated.load(Ordering::Acquire) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(rig.service.start(rig.target()).await.is_err());
    assert!(rig.service.cancel().await);
    rig.tasks.shutdown().await;
    assert_eq!(rig.protocol.polled.load(Ordering::Acquire), 0);
    assert!(rig.sent().is_empty());
}

#[tokio::test]
async fn failed_persistence_preserves_old_account_and_saved_value() {
    let old = credentials(MusicPlatform::Ncm, "100006");
    let rig = Rig::new(MusicPlatform::Ncm, Some(&old)).await;
    let (id, _) = rig.pending().await;
    rig.store.fail_writes(true);
    let error = rig
        .service
        .commit(id, &credentials(MusicPlatform::Ncm, "100007"))
        .await
        .unwrap_err();
    assert!(!format!("{error:?}").contains(FAKE_COOKIE));
    assert!(rig.service.status().await.contains("100006"));
    assert!(!rig.service.status().await.contains("100007"));
    let store: Arc<dyn CredentialStore> = rig.store.clone();
    assert!(
        MusicLogin::read_saved(MusicPlatform::Ncm, &store)
            .await
            .unwrap()
            .unwrap()
            == old
    );
    assert_eq!(rig.active_cookie().as_deref(), Some(old.cookie_header()));
    rig.service.cancel().await;
    rig.tasks.shutdown().await;
}

#[tokio::test]
async fn clear_cancels_old_pending_even_when_secret_deletion_fails() {
    let old = credentials(MusicPlatform::QqMusic, "100008");
    let rig = Rig::new(MusicPlatform::QqMusic, Some(&old)).await;
    let (id, cancelled) = rig.pending().await;
    rig.store.fail_removes(true);
    assert!(rig.service.clear().await.is_err());
    assert!(*cancelled.borrow());
    assert!(!rig.service.commit(id, &old).await.unwrap());
    assert!(rig.service.status().await.contains("100008"));
    assert!(
        rig.store
            .read(MusicPlatform::QqMusic.session_key())
            .await
            .unwrap()
            .is_some()
    );
    assert_eq!(rig.active_cookie().as_deref(), Some(old.cookie_header()));
    rig.tasks.shutdown().await;
}

#[tokio::test]
async fn successful_clear_removes_only_this_platform_and_stale_poll_cannot_restore_it() {
    let old = credentials(MusicPlatform::Ncm, "100013");
    let rig = Rig::new(MusicPlatform::Ncm, Some(&old)).await;
    rig.store
        .write(
            MusicPlatform::QqMusic.session_key(),
            "fixture-other-platform",
        )
        .await
        .unwrap();
    let (id, cancelled) = rig.pending().await;
    rig.service.clear().await.unwrap();
    assert!(*cancelled.borrow());
    assert_eq!(rig.active_cookie(), None);
    assert!(rig.service.status().await.starts_with("未保存"));
    assert!(
        rig.store
            .read(MusicPlatform::Ncm.session_key())
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        rig.store
            .read(MusicPlatform::QqMusic.session_key())
            .await
            .unwrap()
            .as_deref(),
        Some("fixture-other-platform")
    );
    assert!(!rig.service.commit(id, &old).await.unwrap());
    rig.tasks.shutdown().await;
}

#[tokio::test]
async fn failed_candidate_rebuild_never_writes_and_clear_still_cancels_pending() {
    let old = credentials(MusicPlatform::Ncm, "100014");
    let rig = Rig::new(MusicPlatform::Ncm, Some(&old)).await;
    let (id, cancelled) = rig.pending().await;
    rig.state.lock().unwrap().config.proxy = "http://[".into();
    assert!(
        rig.service
            .commit(id, &credentials(MusicPlatform::Ncm, "100015"))
            .await
            .is_err()
    );
    assert_eq!(rig.active_cookie().as_deref(), Some(old.cookie_header()));
    let store: Arc<dyn CredentialStore> = rig.store.clone();
    assert!(
        MusicLogin::read_saved(MusicPlatform::Ncm, &store)
            .await
            .unwrap()
            .unwrap()
            == old
    );
    assert!(rig.service.clear().await.is_err());
    assert!(*cancelled.borrow());
    assert!(!rig.service.commit(id, &old).await.unwrap());
    rig.tasks.shutdown().await;
}

#[tokio::test]
async fn expiry_refusal_wrong_platform_or_protocol_errors_never_replace_saved_account() {
    for outcome in [
        QrPoll::Expired,
        QrPoll::Refused,
        QrPoll::Success(credentials(MusicPlatform::QqMusic, "100009")),
    ] {
        let old = credentials(MusicPlatform::Ncm, "100010");
        let rig = Rig::new(MusicPlatform::Ncm, Some(&old)).await;
        rig.protocol.polls.lock().await.push_back(outcome);
        rig.service.start(rig.target()).await.unwrap();
        rig.finish_task().await;
        assert!(rig.service.status().await.contains("100010"));
        let store: Arc<dyn CredentialStore> = rig.store.clone();
        assert!(
            MusicLogin::read_saved(MusicPlatform::Ncm, &store)
                .await
                .unwrap()
                .unwrap()
                == old
        );
        rig.tasks.shutdown().await;
    }
    let rig = Rig::new(MusicPlatform::Ncm, None).await;
    rig.protocol.fail_generate.store(true, Ordering::Release);
    rig.service.start(rig.target()).await.unwrap();
    rig.finish_task().await;
    assert_eq!(rig.protocol.polled.load(Ordering::Acquire), 0);
    for call in rig.sent() {
        assert!(!call.to_string().contains(FAKE_QR_KEY));
        assert!(!call.to_string().contains(FAKE_COOKIE));
    }
    rig.tasks.shutdown().await;
}

#[tokio::test]
async fn total_challenge_deadline_clears_pending_and_shutdown_rejects_later_mutations() {
    let protocol = Arc::new(FakeProtocol {
        ttl: Duration::from_millis(20),
        ..FakeProtocol::default()
    });
    let rig = Rig::with_protocol(MusicPlatform::Ncm, None, protocol).await;
    rig.service.start(rig.target()).await.unwrap();
    rig.finish_task().await;
    assert!(rig.service.control.lock().await.pending.is_none());
    assert!(
        rig.store
            .read(MusicPlatform::Ncm.session_key())
            .await
            .unwrap()
            .is_none()
    );
    let (id, receiver) = rig.pending().await;
    rig.service.shutdown().await;
    assert!(*receiver.borrow());
    assert!(rig.service.start(rig.target()).await.is_err());
    assert!(rig.service.clear().await.is_err());
    assert!(
        !rig.service
            .commit(id, &credentials(MusicPlatform::Ncm, "100011"))
            .await
            .unwrap()
    );
    rig.tasks.shutdown().await;
}

#[tokio::test]
async fn all_login_actions_require_real_superuser_and_an_explicit_private_route() {
    let rig = Rig::new(MusicPlatform::Ncm, None).await;
    let private = rig.event("private", OPERATOR);
    assert!(rig.service.authorize(&Event::new(&private)).await.is_ok());
    for event in [
        rig.event("group", OPERATOR),
        rig.event("channel", OPERATOR),
        rig.event("private", "ordinary-user"),
        rig.event("private", "banned-operator"),
        json!({"route":rig.route,"user_id":OPERATOR}),
        json!({"message_type":"private","user_id":OPERATOR}),
    ] {
        assert!(rig.service.authorize(&Event::new(&event)).await.is_err());
    }
    let mut owner = rig.event("private", "ordinary-owner");
    owner["sender"]["role"] = json!("owner");
    assert!(rig.service.authorize(&Event::new(&owner)).await.is_err());
    rig.permission.fail.store(true, Ordering::Release);
    let error = rig
        .service
        .authorize(&Event::new(&private))
        .await
        .unwrap_err();
    assert_eq!(error.to_string(), "无法验证超管权限，登录操作未执行");
    assert!(!format!("{error:?}").contains("private token"));
    rig.tasks.shutdown().await;
}

#[tokio::test]
async fn read_saved_rejects_invalid_or_other_platform_data_and_does_not_swallow_storage_errors() {
    let store = Arc::new(MemoryCredentialStore::default());
    let port: Arc<dyn CredentialStore> = store.clone();
    assert!(
        MusicLogin::read_saved(MusicPlatform::Ncm, &port)
            .await
            .unwrap()
            .is_none()
    );
    store
        .write(
            MusicPlatform::Ncm.session_key(),
            &credentials(MusicPlatform::QqMusic, "100012")
                .encode()
                .unwrap(),
        )
        .await
        .unwrap();
    let error = MusicLogin::read_saved(MusicPlatform::Ncm, &port)
        .await
        .err()
        .unwrap();
    assert!(!format!("{error:?}").contains(FAKE_COOKIE));
    store.fail_reads(true);
    assert!(
        MusicLogin::read_saved(MusicPlatform::Ncm, &port)
            .await
            .is_err()
    );
}
