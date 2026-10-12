//! Offline controller fixtures: no platform requests, database connections or real chats.

use super::*;
use std::{collections::VecDeque, sync::atomic::AtomicUsize};

use nuono_plugin::{
    Bot, Event, NuonoError, NuonoResult, RuntimeActionSink, RuntimeBotRoute,
    api::waiter::WaitManager,
    core::{RuntimeActionRouter, event_bus::EventBus, onebot_client::ApiClient},
    permission::{PermissionSettings, PermissionStore},
};
use serde_json::{Value, json};

use crate::{auth::store::MemoryCredentialStore, config::ResolverConfig, resolver::ParserManager};

const MANUAL_COOKIE: &str = "SESSDATA=fixture-manual-session; bili_jct=fixture-manual-csrf; DedeUserID=100002; buvid3=fixture-manual-device";
const FAKE_QR_KEY: &str = "fixture-controller-qr-key-0123456789";
const OPERATOR: &str = "000800000000000000000088";

fn credentials(uid: &str, session: &str) -> Credentials {
    Credentials::from_pairs(
        vec![
            ("SESSDATA".to_owned(), session.to_owned()),
            ("bili_jct".to_owned(), "fixture-controller-csrf".to_owned()),
            ("DedeUserID".to_owned(), uid.to_owned()),
        ],
        "fixture-controller-refresh".to_owned(),
    )
    .unwrap()
}

#[derive(Clone, Default)]
struct PermissionFixture {
    fail: Arc<AtomicBool>,
}

#[async_trait]
impl PermissionStore for PermissionFixture {
    async fn permission_settings(&self) -> std::result::Result<PermissionSettings, String> {
        if self.fail.load(Ordering::Acquire) {
            return Err("fixture permission store unavailable".to_owned());
        }
        Ok(PermissionSettings {
            masters: Some(json!([])),
            superusers: Some(json!([OPERATOR])),
            banned_users: Some(json!(["banned-operator"])),
        })
    }
}

#[derive(Default)]
struct FakeProtocol {
    polls: AsyncMutex<VecDeque<PollOutcome>>,
    generated: AtomicUsize,
    polled: AtomicUsize,
    block_generate: AtomicBool,
    generate_gate: tokio::sync::Notify,
}

#[async_trait]
impl LoginProtocol for FakeProtocol {
    async fn generate(&self) -> std::result::Result<QrCode, LoginError> {
        self.generated.fetch_add(1, Ordering::AcqRel);
        if self.block_generate.load(Ordering::Acquire) {
            self.generate_gate.notified().await;
        }
        QrCode::new(
            format!("https://passport.bilibili.com/qrcode/h5/login?oauthKey={FAKE_QR_KEY}"),
            FAKE_QR_KEY.to_owned(),
        )
    }

    async fn poll(&self, _qr: &QrCode) -> std::result::Result<PollOutcome, LoginError> {
        self.polled.fetch_add(1, Ordering::AcqRel);
        Ok(self
            .polls
            .lock()
            .await
            .pop_front()
            .unwrap_or(PollOutcome::WaitingForScan))
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
        panic!("login messages must wait for a routed receipt");
    }

    async fn call_action(&self, action: Value) -> NuonoResult<Value> {
        self.calls.lock().unwrap().push(action);
        if self.fail.load(Ordering::Acquire) {
            Err(NuonoError::MessageSend("fixture send rejected".into()))
        } else {
            Ok(json!({"message_id":"fixture-confirmed"}))
        }
    }
}

struct Rig {
    service: Arc<BilibiliLogin>,
    state: Arc<Mutex<RuntimeState>>,
    store: Arc<MemoryCredentialStore>,
    protocol: Arc<FakeProtocol>,
    permissions: Arc<PermissionFixture>,
    tasks: Arc<PluginTaskScope>,
    route: RuntimeBotRoute,
    bot: Arc<Bot>,
    sink: Arc<RecordingSink>,
    unrelated_sink: Arc<RecordingSink>,
}

impl Rig {
    async fn new(manual_cookie: &str, saved: Option<&Credentials>) -> Self {
        let tasks = Arc::new(PluginTaskScope::new("linkshownn-login-controller-test"));
        let config = ResolverConfig {
            bili_cookie: manual_cookie.to_owned(),
            ..ResolverConfig::default()
        };
        let managed_cookie = saved.map(Credentials::cookie_header);
        let parser =
            ParserManager::with_bili_session(config.clone(), managed_cookie.as_deref()).unwrap();
        let state = Arc::new(Mutex::new(RuntimeState {
            config,
            parser: Some(parser),
            ..RuntimeState::default()
        }));
        let store = Arc::new(MemoryCredentialStore::default());
        if let Some(saved) = saved {
            store
                .write(BILIBILI_SESSION_KEY, &saved.encode().unwrap())
                .await
                .unwrap();
        }
        let permissions = Arc::new(PermissionFixture::default());
        let protocol = Arc::new(FakeProtocol::default());
        let service = Arc::new(BilibiliLogin {
            state: Arc::downgrade(&state),
            tasks: tasks.clone(),
            permissions: Arc::new(PermissionManager::new(permissions.as_ref().clone())),
            store: store.clone(),
            protocol: protocol.clone(),
            control: AsyncMutex::new(Control {
                pending: None,
                user_id: saved.map(|account| account.user_id().to_owned()),
            }),
            closed: AtomicBool::new(false),
        });
        let route = RuntimeBotRoute::new("onebot", "login-instance", "00011");
        let sink = Arc::new(RecordingSink::default());
        let unrelated_sink = Arc::new(RecordingSink::default());
        let router = Arc::new(RuntimeActionRouter::new());
        router.register(route.clone(), sink.clone());
        router.register(
            RuntimeBotRoute::new("onebot", "other-instance", "00011"),
            unrelated_sink.clone(),
        );
        let bot = Arc::new(
            Bot::new(
                999,
                Arc::new(EventBus::new()),
                Arc::new(WaitManager::new()),
                Arc::new(ApiClient::new()),
                None,
                Some(unrelated_sink.clone()),
            )
            .with_action_router(router),
        );
        Self {
            service,
            state,
            store,
            protocol,
            permissions,
            tasks,
            route,
            bot,
            sink,
            unrelated_sink,
        }
    }

    fn event(&self, kind: &str, user_id: &str) -> Value {
        json!({
            "post_type":"message", "message_type":kind, "user_id":user_id,
            "group_id":"fixture-group", "route":self.route, "message_id":"0007",
            "sender":{"role":"member"}, "raw_message":"urlB站登录",
        })
    }

    fn target(&self) -> ReplyTarget {
        ReplyTarget::new(
            self.bot.clone(),
            &Event::new(&self.event("private", OPERATOR)),
        )
        .unwrap()
    }

    fn active_cookie(&self) -> Option<String> {
        self.state
            .lock()
            .unwrap()
            .parser
            .as_ref()
            .unwrap()
            .bili_cookie_for_test()
            .map(str::to_owned)
    }

    async fn pending(&self) -> (Uuid, watch::Receiver<bool>) {
        let id = Uuid::new_v4();
        let (cancel, receiver) = watch::channel(false);
        self.service.control.lock().await.pending = Some(Pending { id, cancel });
        (id, receiver)
    }

    async fn finish_task(&self) {
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if self.service.control.lock().await.pending.is_none() {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("fixture login task did not finish");
    }

    fn sent(&self) -> Vec<Value> {
        assert!(self.unrelated_sink.calls.lock().unwrap().is_empty());
        self.sink.calls.lock().unwrap().clone()
    }
}

#[tokio::test]
async fn persisted_success_immediately_replaces_the_parser_and_preserves_manual_config() {
    let rig = Rig::new(MANUAL_COOKIE, None).await;
    let account = credentials("100003", "fixture-new-managed-session");
    let (id, _) = rig.pending().await;
    assert!(rig.service.commit(id, &account).await.unwrap());
    assert_eq!(rig.active_cookie(), Some(account.cookie_header()));
    assert_eq!(rig.state.lock().unwrap().config.bili_cookie, MANUAL_COOKIE);
    let store: Arc<dyn CredentialStore> = rig.store.clone();
    assert_eq!(
        BilibiliLogin::read_saved(&store).await.unwrap(),
        Some(account)
    );
    assert!(rig.service.status().await.contains("UID：100003"));
    assert!(
        !rig.service
            .status()
            .await
            .contains("fixture-new-managed-session")
    );
}

#[tokio::test]
async fn failed_storage_write_keeps_the_previous_parser_and_account() {
    let old = credentials("100004", "fixture-old-managed-session");
    let rig = Rig::new(MANUAL_COOKIE, Some(&old)).await;
    let replacement = credentials("100005", "fixture-replacement-session");
    let (id, _) = rig.pending().await;
    rig.store.fail_writes(true);
    let error = rig.service.commit(id, &replacement).await.unwrap_err();
    assert!(!format!("{error:?}").contains("fixture-replacement-session"));
    assert_eq!(rig.active_cookie(), Some(old.cookie_header()));
    assert_eq!(rig.state.lock().unwrap().config.bili_cookie, MANUAL_COOKIE);
    let store: Arc<dyn CredentialStore> = rig.store.clone();
    assert_eq!(BilibiliLogin::read_saved(&store).await.unwrap(), Some(old));
    assert!(rig.service.status().await.contains("UID：100004"));
    assert!(!rig.service.status().await.contains("100005"));
}

#[tokio::test]
async fn cancelled_and_stale_sessions_cannot_write_or_clear_a_newer_pending_session() {
    let rig = Rig::new(MANUAL_COOKIE, None).await;
    let account = credentials("100006", "fixture-cancelled-session");
    let (old_id, receiver) = rig.pending().await;
    assert!(rig.service.cancel().await);
    assert!(*receiver.borrow());
    assert!(!rig.service.cancel().await);
    assert!(!rig.service.commit(old_id, &account).await.unwrap());
    let (new_id, _) = rig.pending().await;
    assert!(!rig.service.commit(old_id, &account).await.unwrap());
    rig.service.finish(old_id).await;
    assert_eq!(
        rig.service
            .control
            .lock()
            .await
            .pending
            .as_ref()
            .unwrap()
            .id,
        new_id
    );
    assert!(
        rig.store
            .read(BILIBILI_SESSION_KEY)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(rig.active_cookie().as_deref(), Some(MANUAL_COOKIE));
    rig.service.finish(new_id).await;
    assert!(rig.service.control.lock().await.pending.is_none());
}

#[tokio::test]
async fn clear_restores_the_manual_cookie_and_old_pending_cannot_resurrect_credentials() {
    let old = credentials("100007", "fixture-old-managed-session");
    let rig = Rig::new(MANUAL_COOKIE, Some(&old)).await;
    rig.store
        .write("another_platform", "fixture-other-session")
        .await
        .unwrap();
    let (id, receiver) = rig.pending().await;
    rig.service.clear().await.unwrap();
    assert!(*receiver.borrow());
    assert_eq!(rig.active_cookie().as_deref(), Some(MANUAL_COOKIE));
    assert_eq!(rig.state.lock().unwrap().config.bili_cookie, MANUAL_COOKIE);
    assert!(
        rig.store
            .read(BILIBILI_SESSION_KEY)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        rig.store.read("another_platform").await.unwrap().as_deref(),
        Some("fixture-other-session")
    );
    assert!(!rig.service.commit(id, &old).await.unwrap());
    assert!(rig.service.status().await.starts_with("未保存"));
}

#[tokio::test]
async fn clear_without_manual_cookie_restores_anonymous_requests() {
    let old = credentials("100008", "fixture-old-session");
    let rig = Rig::new("", Some(&old)).await;
    rig.service.clear().await.unwrap();
    assert_eq!(rig.active_cookie(), None);
    assert!(rig.state.lock().unwrap().config.bili_cookie.is_empty());
    assert!(
        rig.store
            .read(BILIBILI_SESSION_KEY)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn failed_clear_keeps_old_account_but_cancels_pending_login() {
    let old = credentials("100009", "fixture-old-session");
    let rig = Rig::new(MANUAL_COOKIE, Some(&old)).await;
    let (id, receiver) = rig.pending().await;
    rig.store.fail_removes(true);
    assert!(rig.service.clear().await.is_err());
    assert!(*receiver.borrow());
    assert_eq!(rig.active_cookie(), Some(old.cookie_header()));
    assert!(rig.service.status().await.contains("UID：100009"));
    assert!(
        !rig.service
            .commit(id, &credentials("100010", "fixture-new-session"))
            .await
            .unwrap()
    );
    let store: Arc<dyn CredentialStore> = rig.store.clone();
    assert_eq!(BilibiliLogin::read_saved(&store).await.unwrap(), Some(old));
}

#[tokio::test]
async fn failed_parser_rebuild_during_clear_still_cancels_pending_login() {
    let old = credentials("100015", "fixture-existing-session");
    let rig = Rig::new(MANUAL_COOKIE, Some(&old)).await;
    let (id, receiver) = rig.pending().await;
    rig.state.lock().unwrap().config.proxy = "http://[".to_owned();
    assert!(rig.service.clear().await.is_err());
    assert!(*receiver.borrow());
    assert_eq!(rig.active_cookie(), Some(old.cookie_header()));
    assert!(
        !rig.service
            .commit(id, &credentials("100016", "fixture-unused-session"))
            .await
            .unwrap()
    );
    let port: Arc<dyn CredentialStore> = rig.store.clone();
    assert_eq!(BilibiliLogin::read_saved(&port).await.unwrap(), Some(old));
}

struct BlockingStore {
    inner: Arc<MemoryCredentialStore>,
    entered: tokio::sync::Notify,
    release: tokio::sync::Semaphore,
}

impl BlockingStore {
    fn install(rig: &mut Rig) -> Arc<Self> {
        let store = Arc::new(Self {
            inner: rig.store.clone(),
            entered: tokio::sync::Notify::new(),
            release: tokio::sync::Semaphore::new(0),
        });
        Arc::get_mut(&mut rig.service).unwrap().store = store.clone();
        store
    }

    async fn wait_for_write(&self) {
        tokio::time::timeout(Duration::from_secs(2), self.entered.notified())
            .await
            .unwrap();
    }
}

#[async_trait]
impl CredentialStore for BlockingStore {
    async fn read(&self, key: &str) -> Result<Option<String>> {
        self.inner.read(key).await
    }

    async fn write(&self, key: &str, value: &str) -> Result<()> {
        self.entered.notify_one();
        self.release.acquire().await.unwrap().forget();
        self.inner.write(key, value).await
    }

    async fn remove(&self, key: &str) -> Result<()> {
        self.inner.remove(key).await
    }
}

#[tokio::test]
async fn clear_waits_for_in_flight_commit_then_removes_it_without_resurrection() {
    let mut rig = Rig::new(MANUAL_COOKIE, None).await;
    let store = BlockingStore::install(&mut rig);
    let account = credentials("100017", "fixture-inflight-session");
    let (id, receiver) = rig.pending().await;
    let service = rig.service.clone();
    let commit_account = account.clone();
    let commit = tokio::spawn(async move { service.commit(id, &commit_account).await });
    store.wait_for_write().await;
    assert!(rig.service.control.try_lock().is_err());
    assert_eq!(rig.active_cookie().as_deref(), Some(MANUAL_COOKIE));
    assert!(
        rig.store
            .read(BILIBILI_SESSION_KEY)
            .await
            .unwrap()
            .is_none()
    );
    let service = rig.service.clone();
    let clear = tokio::spawn(async move { service.clear().await });
    tokio::task::yield_now().await;
    assert!(!clear.is_finished());
    store.release.add_permits(1);
    assert!(commit.await.unwrap().unwrap());
    clear.await.unwrap().unwrap();
    assert!(*receiver.borrow());
    assert!(
        rig.store
            .read(BILIBILI_SESSION_KEY)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(rig.active_cookie().as_deref(), Some(MANUAL_COOKIE));
    assert!(!rig.service.commit(id, &account).await.unwrap());
}

#[tokio::test]
async fn shutdown_waits_for_accepted_commit_and_blocks_all_subsequent_commits() {
    let mut rig = Rig::new(MANUAL_COOKIE, None).await;
    let store = BlockingStore::install(&mut rig);
    let account = credentials("100018", "fixture-inflight-session");
    let (id, receiver) = rig.pending().await;
    let service = rig.service.clone();
    let commit_account = account.clone();
    let commit = tokio::spawn(async move { service.commit(id, &commit_account).await });
    store.wait_for_write().await;
    let service = rig.service.clone();
    let shutdown = tokio::spawn(async move { service.shutdown().await });
    tokio::task::yield_now().await;
    assert!(!shutdown.is_finished());
    assert!(!rig.service.closed.load(Ordering::Acquire));
    store.release.add_permits(1);
    assert!(commit.await.unwrap().unwrap());
    shutdown.await.unwrap();
    assert!(*receiver.borrow());
    assert!(rig.service.closed.load(Ordering::Acquire));
    assert_eq!(rig.active_cookie(), Some(account.cookie_header()));
    let port: Arc<dyn CredentialStore> = rig.store.clone();
    assert_eq!(
        BilibiliLogin::read_saved(&port).await.unwrap(),
        Some(account.clone())
    );
    assert!(!rig.service.commit(id, &account).await.unwrap());
    assert!(rig.service.start(rig.target()).await.is_err());
}

#[tokio::test]
async fn shutdown_rejects_new_login_clear_and_pending_commits() {
    let rig = Rig::new(MANUAL_COOKIE, None).await;
    let (id, receiver) = rig.pending().await;
    rig.service.shutdown().await;
    assert!(*receiver.borrow());
    assert!(rig.service.closed.load(Ordering::Acquire));
    assert!(rig.service.start(rig.target()).await.is_err());
    assert!(rig.service.clear().await.is_err());
    assert!(
        !rig.service
            .commit(id, &credentials("100011", "fixture-disabled-session"))
            .await
            .unwrap()
    );
    assert!(
        rig.store
            .read(BILIBILI_SESSION_KEY)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(rig.active_cookie().as_deref(), Some(MANUAL_COOKIE));
}

#[tokio::test]
async fn stored_credentials_are_validated_and_storage_failure_does_not_become_anonymous() {
    let store = Arc::new(MemoryCredentialStore::default());
    let port: Arc<dyn CredentialStore> = store.clone();
    assert!(BilibiliLogin::read_saved(&port).await.unwrap().is_none());
    store
        .write(BILIBILI_SESSION_KEY, "invalid-fixture-credential")
        .await
        .unwrap();
    let error = BilibiliLogin::read_saved(&port).await.unwrap_err();
    assert!(!format!("{error:?}").contains("invalid-fixture-credential"));
    store.fail_reads(true);
    assert!(BilibiliLogin::read_saved(&port).await.is_err());
}

#[tokio::test]
async fn authorization_requires_explicit_private_route_and_real_superuser_permission() {
    let rig = Rig::new("", None).await;
    let private = rig.event("private", OPERATOR);
    assert!(rig.service.authorize(&Event::new(&private)).await.is_ok());
    for value in [
        rig.event("group", OPERATOR),
        rig.event("channel", OPERATOR),
        rig.event("private", "ordinary-user"),
        rig.event("private", "banned-operator"),
        json!({"route":rig.route,"user_id":OPERATOR}),
        json!({"message_type":"private","user_id":OPERATOR}),
    ] {
        assert!(rig.service.authorize(&Event::new(&value)).await.is_err());
    }
    let mut ordinary_group_owner = rig.event("private", "ordinary-owner");
    ordinary_group_owner["sender"]["role"] = json!("owner");
    assert!(
        rig.service
            .authorize(&Event::new(&ordinary_group_owner))
            .await
            .is_err()
    );
    rig.permissions.fail.store(true, Ordering::Release);
    let error = rig
        .service
        .authorize(&Event::new(&private))
        .await
        .unwrap_err();
    assert_eq!(error.to_string(), "无法验证超管权限，登录操作未执行");
    assert!(
        !error
            .to_string()
            .contains("fixture permission store unavailable")
    );
}

#[test]
fn superuser_guard_rejects_group_authority_and_banned_users() {
    assert!(ensure_superuser(PermissionLevel::Master).is_ok());
    assert!(ensure_superuser(PermissionLevel::Superuser).is_ok());
    for level in [
        PermissionLevel::GroupOwner,
        PermissionLevel::GroupAdmin,
        PermissionLevel::Member,
        PermissionLevel::Banned,
    ] {
        assert!(ensure_superuser(level).is_err());
    }
}

#[tokio::test]
async fn completed_fake_login_sends_qr_then_success_to_the_exact_private_route() {
    let rig = Rig::new(MANUAL_COOKIE, None).await;
    let account = credentials("100012", "fixture-routed-login-session");
    rig.protocol
        .polls
        .lock()
        .await
        .push_back(PollOutcome::Success(account.clone()));
    rig.service.start(rig.target()).await.unwrap();
    rig.finish_task().await;
    assert_eq!(rig.protocol.generated.load(Ordering::Acquire), 1);
    assert_eq!(rig.protocol.polled.load(Ordering::Acquire), 1);
    assert_eq!(rig.active_cookie(), Some(account.cookie_header()));
    let calls = rig.sent();
    assert_eq!(calls.len(), 2);
    for call in &calls {
        assert_eq!(call["action"], "send_msg");
        assert_eq!(call["params"]["message_type"], "private");
        assert_eq!(call["params"]["user_id"], OPERATOR);
        assert!(!call.to_string().contains(FAKE_QR_KEY));
        assert!(!call.to_string().contains("fixture-routed-login-session"));
        assert!(!call.to_string().contains("fixture-controller-refresh"));
    }
    let first = calls[0]["params"]["message"].as_array().unwrap();
    assert_eq!(first.len(), 2);
    assert_eq!(first[0]["type"], "text");
    assert!(
        first[0]["data"]["text"]
            .as_str()
            .unwrap()
            .contains("扫码并确认")
    );
    assert_eq!(first[1]["type"], "image");
    assert!(
        first[1]["data"]["file"]
            .as_str()
            .unwrap()
            .starts_with("base64://")
    );
    assert!(calls[1].to_string().contains("UID：100012"));
    rig.tasks.shutdown().await;
}

#[tokio::test]
async fn failed_qr_send_prevents_polling_persistence_and_parser_replacement() {
    let rig = Rig::new(MANUAL_COOKIE, None).await;
    rig.sink.fail.store(true, Ordering::Release);
    rig.protocol
        .polls
        .lock()
        .await
        .push_back(PollOutcome::Success(credentials(
            "100013",
            "fixture-unused-session",
        )));
    rig.service.start(rig.target()).await.unwrap();
    rig.finish_task().await;
    assert_eq!(rig.protocol.generated.load(Ordering::Acquire), 1);
    assert_eq!(rig.protocol.polled.load(Ordering::Acquire), 0);
    assert!(
        rig.store
            .read(BILIBILI_SESSION_KEY)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(rig.active_cookie().as_deref(), Some(MANUAL_COOKIE));
    assert!(
        rig.sent()
            .iter()
            .all(|call| !call.to_string().contains("fixture-unused-session"))
    );
    rig.tasks.shutdown().await;
}

#[tokio::test]
async fn pending_generate_can_be_cancelled_and_duplicate_start_is_rejected() {
    let rig = Rig::new(MANUAL_COOKIE, None).await;
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
    assert!(
        rig.store
            .read(BILIBILI_SESSION_KEY)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(rig.active_cookie().as_deref(), Some(MANUAL_COOKIE));
}

#[tokio::test]
async fn expiry_does_not_overwrite_a_previously_saved_account() {
    let old = credentials("100014", "fixture-old-session");
    let rig = Rig::new(MANUAL_COOKIE, Some(&old)).await;
    rig.protocol
        .polls
        .lock()
        .await
        .push_back(PollOutcome::Expired);
    rig.service.start(rig.target()).await.unwrap();
    rig.finish_task().await;
    assert_eq!(rig.active_cookie(), Some(old.cookie_header()));
    let port: Arc<dyn CredentialStore> = rig.store.clone();
    assert_eq!(BilibiliLogin::read_saved(&port).await.unwrap(), Some(old));
    assert!(
        rig.sent()
            .last()
            .unwrap()
            .to_string()
            .contains("二维码已过期")
    );
    rig.tasks.shutdown().await;
}
