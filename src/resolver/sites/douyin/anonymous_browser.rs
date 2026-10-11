//! Anonymous, short-lived local Chromium fallback for public Douyin works.
//!
//! Each visit owns a fresh profile and a private loopback CDP endpoint. This is
//! an ordinary local browser, not the SDK's public-network-filtered HTTP client:
//! although the initial URL is fixed here, page subresources use Chromium's
//! networking policy. No existing profile, login cookie, or browser session is
//! imported. The normal browser sandbox remains enabled.

use std::{
    collections::{HashMap, VecDeque},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{Arc, LazyLock, Mutex, Weak},
    time::Duration,
};

use anyhow::{Result, anyhow, bail, ensure};
use futures_util::{SinkExt, StreamExt};
use nuono_plugin::PluginTaskScope;
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::{
    net::TcpStream,
    sync::{Notify, OwnedSemaphorePermit, Semaphore},
};
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream, connect_async_with_config,
    tungstenite::{Message, protocol::WebSocketConfig},
};
use url::Url;

use super::client::AwemeRef;
use super::engagement::Engagement;

const MAX_DOM_BYTES: usize = 4 * 1024 * 1024;
const VISIT_TIMEOUT: Duration = Duration::from_secs(40);
static ADMITTED: LazyLock<Arc<Semaphore>> = LazyLock::new(|| Arc::new(Semaphore::new(3)));
static ACTIVE: LazyLock<Arc<Semaphore>> = LazyLock::new(|| Arc::new(Semaphore::new(1)));

#[derive(Debug)]
pub(crate) struct AnonymousPage {
    pub html: String,
    pub(super) engagement: Engagement,
}

#[derive(Clone)]
pub(crate) struct AnonymousBrowser {
    proxy: String,
    configured_path: Option<PathBuf>,
    tasks: Arc<PluginTaskScope>,
    cleanup: Arc<CleanupRegistry>,
}

impl AnonymousBrowser {
    pub(crate) fn new(
        proxy: &str,
        configured_path: &str,
        tasks: Arc<PluginTaskScope>,
    ) -> Result<Self> {
        // These settings only affect this optional fallback. Validate them at
        // use time so an unsupported proxy/path cannot disable other resolvers.
        let proxy = proxy.trim().to_owned();
        let configured_path =
            (!configured_path.trim().is_empty()).then(|| PathBuf::from(configured_path.trim()));
        let cleanup = Arc::new(CleanupRegistry::default());
        // One callback per client, not per visit. Its registry only holds weak
        // owners; completed visits retain neither profiles nor quota permits.
        let on_shutdown = cleanup.clone();
        tasks.on_shutdown(move || on_shutdown.close());
        Ok(Self {
            proxy,
            configured_path,
            tasks,
            cleanup,
        })
    }

    pub(crate) async fn fetch(
        &self,
        aweme: &AwemeRef,
        include_engagement: bool,
    ) -> Result<AnonymousPage> {
        ensure!(
            cfg!(any(target_os = "macos", target_os = "linux")),
            "抖音匿名浏览器回退目前仅支持 macOS 和 Linux"
        );
        let target = canonical_target(aweme)?;
        let proxy = validate_proxy(&self.proxy)?;
        ensure!(!self.tasks.is_closed(), "插件已停用，匿名浏览器请求已取消");
        let admitted = ADMITTED
            .clone()
            .try_acquire_owned()
            .map_err(|_| anyhow!("抖音匿名浏览器队列已满"))?;
        let deadline = tokio::time::Instant::now() + VISIT_TIMEOUT;
        let work = async {
            let active = ACTIVE
                .clone()
                .acquire_owned()
                .await
                .map_err(|_| anyhow!("抖音匿名浏览器队列已关闭"))?;
            ensure!(!self.tasks.is_closed(), "插件已停用，匿名浏览器请求已取消");
            let executable = discover_browser(self.configured_path.as_deref())?;
            let profile = tempfile::Builder::new()
                .prefix("nuono-douyin-anonymous-")
                .tempdir()
                .map_err(|_| anyhow!("无法创建匿名浏览器临时目录"))?;
            ensure_private_profile(profile.path())?;
            let profile_path = profile.path().to_owned();
            let child = browser_command(&executable, profile.path(), proxy.as_deref())?
                .spawn()
                .map_err(|_| anyhow!("无法启动本机 Chrome/Edge，请检查程序路径和浏览器沙箱环境"))?;
            let owner = Arc::new(ProcessOwner::new(child, profile, vec![admitted, active]));
            self.cleanup.register(&owner)?;
            let result = visit(
                &profile_path,
                &target,
                &aweme.id,
                include_engagement,
                deadline,
            )
            .await;
            // Keep both permits until the complete process group is stopped,
            // the direct child is reaped, and its temporary profile is removed.
            owner.cleanup();
            result
        };
        let result = tokio::select! {
            biased;
            _ = self.cleanup.cancelled() => Err(anyhow!("插件已停用，匿名浏览器请求已取消")),
            result = tokio::time::timeout_at(deadline, work) => {
                result.map_err(|_| anyhow!("抖音匿名浏览器等待超时，页面可能要求验证或暂不可用"))?
            }
        };
        ensure!(!self.tasks.is_closed(), "插件已停用，匿名浏览器请求已取消");
        result
    }
}

fn canonical_target(aweme: &AwemeRef) -> Result<String> {
    ensure!(!aweme.is_note, "匿名浏览器回退目前仅处理抖音视频作品");
    ensure!(
        !aweme.id.is_empty()
            && aweme.id.len() <= 24
            && aweme.id.bytes().all(|byte| byte.is_ascii_digit()),
        "抖音作品编号无效"
    );
    Ok(format!("https://www.douyin.com/video/{}", aweme.id))
}

fn validate_proxy(raw: &str) -> Result<Option<String>> {
    if raw.trim().is_empty() {
        return Ok(None);
    }
    let url = Url::parse(raw.trim()).map_err(|_| anyhow!("匿名浏览器代理地址无效"))?;
    ensure!(
        matches!(url.scheme(), "http" | "https")
            && url.host_str().is_some()
            && url.username().is_empty()
            && url.password().is_none()
            && matches!(url.path(), "" | "/")
            && url.query().is_none()
            && url.fragment().is_none(),
        "匿名浏览器仅支持不含用户名、密码或路径参数的 HTTP/HTTPS 代理"
    );
    Ok(Some(url.to_string()))
}

fn executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path)
            .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        false
    }
}

fn discover_browser(configured: Option<&Path>) -> Result<PathBuf> {
    if let Some(path) = configured {
        ensure!(
            path.is_absolute() && executable(path),
            "配置的浏览器程序不存在或不可执行"
        );
        return std::fs::canonicalize(path).map_err(|_| anyhow!("无法读取配置的浏览器程序"));
    }
    let mut candidates: Vec<PathBuf> = Vec::new();
    #[cfg(target_os = "macos")]
    {
        let bundles = [
            "Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
            "Google Chrome.app/Contents/MacOS/Google Chrome",
            "Chromium.app/Contents/MacOS/Chromium",
        ];
        for bundle in bundles {
            candidates.push(Path::new("/Applications").join(bundle));
        }
        if let Some(home_directory) = std::env::var_os("HOME") {
            for bundle in bundles {
                candidates.push(
                    PathBuf::from(&home_directory)
                        .join("Applications")
                        .join(bundle),
                );
            }
        }
    }
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    if let Some(search_path) = std::env::var_os("PATH") {
        for directory in std::env::split_paths(&search_path).filter(|path| path.is_absolute()) {
            for name in [
                "microsoft-edge",
                "microsoft-edge-stable",
                "google-chrome",
                "google-chrome-stable",
                "chromium",
                "chromium-browser",
            ] {
                candidates.push(directory.join(name));
            }
        }
    }
    candidates
        .into_iter()
        .find(|path| executable(path))
        .and_then(|path| std::fs::canonicalize(path).ok())
        .ok_or_else(|| {
            anyhow!("未找到本机 Chrome/Edge；请安装浏览器或配置程序路径，插件不会自动安装")
        })
}

fn ensure_private_profile(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
            .map_err(|_| anyhow!("无法设置匿名浏览器临时目录权限"))?;
    }
    Ok(())
}

fn browser_command(executable: &Path, profile: &Path, proxy: Option<&str>) -> Result<Command> {
    ensure!(
        cfg!(any(target_os = "macos", target_os = "linux")),
        "抖音匿名浏览器回退目前仅支持 macOS 和 Linux"
    );
    let mut command = Command::new(executable);
    command
        .args([
            "--headless=new",
            "--remote-debugging-address=127.0.0.1",
            "--remote-debugging-port=0",
            "--no-first-run",
            "--no-default-browser-check",
            "--disable-background-networking",
            "--disable-sync",
            "--disable-extensions",
            "--disable-component-update",
            "--disable-breakpad",
            "--disable-crash-reporter",
            "--autoplay-policy=user-gesture-required",
            "--mute-audio",
        ])
        .arg(format!("--user-data-dir={}", profile.display()))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Some(proxy) = proxy {
        command.arg(format!("--proxy-server={proxy}"));
    } else {
        command.arg("--no-proxy-server");
    }
    for name in [
        "http_proxy",
        "https_proxy",
        "all_proxy",
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "ALL_PROXY",
    ] {
        command.env_remove(name);
    }
    command.arg("about:blank");
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    Ok(command)
}

#[derive(Default)]
struct CleanupRegistry {
    state: Mutex<CleanupState>,
    notify: Notify,
}

#[derive(Default)]
struct CleanupState {
    closed: bool,
    owners: Vec<Weak<ProcessOwner>>,
}

impl CleanupRegistry {
    fn register(&self, owner: &Arc<ProcessOwner>) -> Result<()> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        ensure!(!state.closed, "插件已停用，匿名浏览器请求已取消");
        state.owners.retain(|owner| owner.strong_count() != 0);
        state.owners.push(Arc::downgrade(owner));
        Ok(())
    }

    fn close(&self) {
        let owners = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            state.closed = true;
            std::mem::take(&mut state.owners)
        };
        self.notify.notify_waiters();
        for owner in owners.into_iter().filter_map(|owner| owner.upgrade()) {
            owner.cleanup();
        }
    }

    async fn cancelled(&self) {
        let notified = self.notify.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        if !self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .closed
        {
            notified.await;
        }
    }
}

struct ProcessOwner {
    resources: Mutex<Option<ProcessResources>>,
}

struct ProcessResources {
    child: Child,
    profile: TempDir,
    permits: Vec<OwnedSemaphorePermit>,
}

impl ProcessOwner {
    fn new(child: Child, profile: TempDir, permits: Vec<OwnedSemaphorePermit>) -> Self {
        Self {
            resources: Mutex::new(Some(ProcessResources {
                child,
                profile,
                permits,
            })),
        }
    }

    fn cleanup(&self) {
        // Hold the lock through reaping: a concurrent shutdown must wait for the
        // caller's cleanup, not return while another thread still owns children.
        let mut slot = self
            .resources
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(mut resources) = slot.take() {
            #[cfg(unix)]
            {
                // The group is created with PGID == this unreaped child's PID.
                // Never use a browser name or signal a group after reaping it.
                let pid = resources.child.id() as libc::pid_t;
                if pid > 0 {
                    unsafe {
                        libc::killpg(pid, libc::SIGKILL);
                    }
                }
            }
            let _ = resources.child.kill();
            let _ = resources.child.wait();
            // Remove the disposable profile before admitting another browser.
            drop(resources.profile);
            drop(resources.permits);
        }
    }
}

impl Drop for ProcessOwner {
    fn drop(&mut self) {
        self.cleanup();
    }
}

fn parse_devtools_endpoint(contents: &str) -> Result<String> {
    ensure!(contents.len() <= 1024, "浏览器调试端口文件无效");
    let mut lines = contents.lines();
    let port = lines
        .next()
        .and_then(|line| line.parse::<u16>().ok())
        .filter(|port| *port >= 1024)
        .ok_or_else(|| anyhow!("浏览器调试端口无效"))?;
    let path = lines
        .next()
        .ok_or_else(|| anyhow!("浏览器调试会话标识缺失"))?;
    let nonce = path.strip_prefix("/devtools/browser/").unwrap_or_default();
    ensure!(
        nonce.len() == 36
            && nonce.bytes().enumerate().all(|(index, byte)| {
                if matches!(index, 8 | 13 | 18 | 23) {
                    byte == b'-'
                } else {
                    byte.is_ascii_hexdigit()
                }
            })
            && lines.next().is_none(),
        "浏览器调试会话标识无效"
    );
    Ok(format!("ws://127.0.0.1:{port}{path}"))
}

async fn read_devtools_endpoint(profile: &Path) -> Result<String> {
    let path = profile.join("DevToolsActivePort");
    loop {
        match tokio::fs::symlink_metadata(&path).await {
            Ok(meta) => {
                ensure!(
                    meta.is_file() && meta.len() <= 1024,
                    "浏览器调试端口文件无效"
                );
                let data = tokio::fs::read(&path)
                    .await
                    .map_err(|_| anyhow!("无法读取浏览器调试端口"))?;
                let contents =
                    std::str::from_utf8(&data).map_err(|_| anyhow!("浏览器调试端口文件无效"))?;
                // Chromium writes both lines at startup. An empty/partial file
                // can briefly be observed before the write finishes.
                if contents.lines().count() >= 2 {
                    return parse_devtools_endpoint(contents);
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => bail!("无法读取浏览器调试端口"),
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

struct Cdp {
    socket: WebSocketStream<MaybeTlsStream<TcpStream>>,
    next_id: u64,
    capture: Option<ResponseCapture>,
}

#[derive(Clone, Copy)]
enum ResponseKind {
    Comments,
    Detail,
}

struct ResponseCapture {
    work_id: String,
    session: String,
    accepted: usize,
    pending: HashMap<String, ResponseKind>,
    ready: VecDeque<(String, ResponseKind)>,
}

impl ResponseCapture {
    fn new(work_id: &str, session: &str) -> Self {
        Self {
            work_id: work_id.into(),
            session: session.into(),
            accepted: 0,
            pending: HashMap::new(),
            ready: VecDeque::new(),
        }
    }

    fn observe(&mut self, event: &Value) {
        if event["sessionId"].as_str() != Some(&self.session) {
            return;
        }
        let params = &event["params"];
        let Some(request) = params["requestId"].as_str().filter(|id| id.len() <= 256) else {
            return;
        };
        match event["method"].as_str() {
            Some("Network.responseReceived") if self.accepted < 8 => {
                if params["response"]["status"].as_u64() != Some(200)
                    || !matches!(params["type"].as_str(), Some("XHR" | "Fetch"))
                {
                    return;
                }
                let Some(url) = params["response"]["url"]
                    .as_str()
                    .and_then(|s| Url::parse(s).ok())
                else {
                    return;
                };
                if url.scheme() != "https"
                    || !matches!(url.host_str(), Some("www.douyin.com" | "www-hj.douyin.com"))
                    || url.port_or_known_default() != Some(443)
                    || url
                        .query_pairs()
                        .filter(|(key, _)| key == "aweme_id")
                        .map(|(_, value)| value.into_owned())
                        .collect::<Vec<_>>()
                        != [self.work_id.clone()]
                {
                    return;
                }
                let kind = match url.path() {
                    "/aweme/v1/web/comment/list/" => ResponseKind::Comments,
                    "/aweme/v1/web/aweme/detail/" => ResponseKind::Detail,
                    _ => return,
                };
                self.pending.insert(request.into(), kind);
                self.accepted += 1;
            }
            Some("Network.loadingFinished") => {
                if let Some(kind) = self.pending.remove(request)
                    && params["encodedDataLength"]
                        .as_f64()
                        .is_none_or(|size| size <= 1024.0 * 1024.0)
                {
                    self.ready.push_back((request.into(), kind));
                }
            }
            Some("Network.loadingFailed") => {
                self.pending.remove(request);
            }
            _ => {}
        }
    }
}

impl Cdp {
    async fn connect(endpoint: &str) -> Result<Self> {
        let config = WebSocketConfig::default()
            .max_message_size(Some(MAX_DOM_BYTES))
            .max_frame_size(Some(MAX_DOM_BYTES))
            .max_write_buffer_size(MAX_DOM_BYTES);
        let (socket, _) = connect_async_with_config(endpoint, Some(config), false)
            .await
            .map_err(|_| anyhow!("无法连接本次匿名浏览器调试会话"))?;
        Ok(Self {
            socket,
            next_id: 0,
            capture: None,
        })
    }

    async fn send(&mut self, method: &str, params: Value, session: Option<&str>) -> Result<u64> {
        self.next_id += 1;
        let mut message = json!({"id":self.next_id,"method":method,"params":params});
        if let Some(session) = session {
            message["sessionId"] = Value::String(session.to_owned());
        }
        self.socket
            .send(Message::Text(message.to_string().into()))
            .await
            .map_err(|_| anyhow!("匿名浏览器调试会话已断开"))?;
        Ok(self.next_id)
    }

    async fn call(&mut self, method: &str, params: Value, session: Option<&str>) -> Result<Value> {
        let id = self.send(method, params, session).await?;
        loop {
            let message = self
                .socket
                .next()
                .await
                .ok_or_else(|| anyhow!("匿名浏览器调试会话已结束"))?
                .map_err(|_| anyhow!("匿名浏览器响应无效或超过大小限制"))?;
            let value: Value = match message {
                Message::Text(text) => serde_json::from_str(&text)
                    .map_err(|_| anyhow!("匿名浏览器返回了无效调试数据"))?,
                Message::Ping(data) => {
                    self.socket
                        .send(Message::Pong(data))
                        .await
                        .map_err(|_| anyhow!("匿名浏览器调试会话已断开"))?;
                    continue;
                }
                Message::Close(_) => bail!("匿名浏览器调试会话已结束"),
                Message::Pong(_) => continue,
                _ => bail!("匿名浏览器返回了不支持的调试数据"),
            };
            if let Some(capture) = &mut self.capture {
                capture.observe(&value);
            }
            if value.get("method").and_then(Value::as_str) == Some("Fetch.requestPaused") {
                if let (Some(request_id), Some(event_session)) = (
                    value.pointer("/params/requestId").and_then(Value::as_str),
                    value.get("sessionId").and_then(Value::as_str),
                ) {
                    // Media requests are never continued. Its acknowledgement
                    // is ignored while waiting for our original command ID.
                    self.send(
                        "Fetch.failRequest",
                        json!({"requestId":request_id,"errorReason":"BlockedByClient"}),
                        Some(event_session),
                    )
                    .await?;
                }
                continue;
            }
            if value.get("id").and_then(Value::as_u64) != Some(id) {
                continue;
            }
            if let Some(error) = value.get("error") {
                if navigation_context_changed(method, error) {
                    return Ok(Value::Null);
                }
                bail!("匿名浏览器不支持所需页面操作");
            }
            return Ok(value.get("result").cloned().unwrap_or(Value::Null));
        }
    }
}

fn navigation_context_changed(method: &str, error: &Value) -> bool {
    if method != "Runtime.evaluate" || error.get("code").and_then(Value::as_i64) != Some(-32000) {
        return false;
    }
    let message = error
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_ascii_lowercase();
    message.contains("execution context was destroyed")
        || message.contains("cannot find context with specified id")
        || message.contains("cannot find default execution context")
}

async fn visit(
    profile: &Path,
    target: &str,
    id: &str,
    include_engagement: bool,
    deadline: tokio::time::Instant,
) -> Result<AnonymousPage> {
    let expected = AwemeRef {
        id: id.to_owned(),
        is_note: false,
    };
    let source = Url::parse(target).map_err(|_| anyhow!("抖音作品地址无效"))?;
    let endpoint = read_devtools_endpoint(profile).await?;
    let mut cdp = Cdp::connect(&endpoint).await?;
    cdp.call(
        "Browser.setDownloadBehavior",
        json!({"behavior":"deny"}),
        None,
    )
    .await?;
    let created = cdp
        .call("Target.createTarget", json!({"url":"about:blank"}), None)
        .await?;
    let target_id = created
        .get("targetId")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("匿名浏览器未创建页面"))?;
    let attached = cdp
        .call(
            "Target.attachToTarget",
            json!({"targetId":target_id,"flatten":true}),
            None,
        )
        .await?;
    let session = attached
        .get("sessionId")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("匿名浏览器未创建页面会话"))?;
    cdp.call("Page.enable", json!({}), Some(session)).await?;
    cdp.call(
        "Network.enable",
        json!({"maxTotalBufferSize":4194304,"maxResourceBufferSize":1048576}),
        Some(session),
    )
    .await?;
    if include_engagement {
        cdp.capture = Some(ResponseCapture::new(id, session));
    }
    cdp.call("Network.setBlockedURLs", json!({"urls":["*.douyinvod.com/*","*.douyinvod.net/*","*.mp4*","*.m3u8*","*.mpd*","*.m4s*","*.mp3*"]}), Some(session)).await?;
    cdp.call(
        "Fetch.enable",
        json!({"patterns":[{"resourceType":"Media","requestStage":"Request"}]}),
        Some(session),
    )
    .await?;
    // Playback suppression is confined to media; page/browser-check scripts
    // execute unchanged. Downloads and media response bodies are also blocked.
    cdp.call("Page.addScriptToEvaluateOnNewDocument", json!({"source":"document.addEventListener('play', function(event) { if (event.target instanceof HTMLMediaElement) event.target.pause(); }, true);"}), Some(session)).await?;
    let navigation = cdp
        .call("Page.navigate", json!({"url":target}), Some(session))
        .await?;
    ensure!(
        navigation.get("errorText").is_none(),
        "抖音公开页面导航失败"
    );
    let expression = readiness_expression(target, id);
    loop {
        let result = cdp
            .call(
                "Runtime.evaluate",
                json!({"expression":expression,"returnByValue":true}),
                Some(session),
            )
            .await?;
        ensure!(
            result.get("exceptionDetails").is_none(),
            "抖音公开页面结构无法读取"
        );
        let value = result.pointer("/result/value");
        ensure!(
            value
                .and_then(|value| value.get("oversized"))
                .and_then(Value::as_bool)
                != Some(true),
            "抖音公开页面超过大小限制"
        );
        if let Some(html) = value
            .and_then(|value| value.get("html"))
            .and_then(Value::as_str)
        {
            ensure!(
                !html.is_empty() && html.len() <= MAX_DOM_BYTES,
                "抖音公开页面超过大小限制"
            );
            // Hydration can publish canonical/meta before the author JSON-LD
            // and the matching content tree. Only return a complete work the
            // same production mapper will accept, without exposing its errors.
            if super::desktop::map_desktop_page(html, &expected, &source).is_ok() {
                let mut page = AnonymousPage {
                    html: html.to_owned(),
                    engagement: Engagement::default(),
                };
                if include_engagement {
                    // Optional comments may fail or request login. Preserve the
                    // validated basic card and any public data already read.
                    let _ = tokio::time::timeout_at(
                        (tokio::time::Instant::now() + Duration::from_secs(8))
                            .min(deadline - Duration::from_millis(250)),
                        collect_engagement(&mut cdp, session, &expected, &source, &mut page),
                    )
                    .await;
                }
                return Ok(page);
            }
        }
        tokio::time::sleep(Duration::from_millis(400)).await;
    }
}

async fn collect_engagement(
    cdp: &mut Cdp,
    session: &str,
    expected: &AwemeRef,
    source: &Url,
    page: &mut AnonymousPage,
) -> Result<()> {
    let expression = readiness_expression(source.as_str(), &expected.id);
    for _ in 0..12 {
        while let Some((request, kind)) = cdp
            .capture
            .as_mut()
            .and_then(|capture| capture.ready.pop_front())
        {
            let response = tokio::time::timeout(
                Duration::from_secs(2),
                cdp.call(
                    "Network.getResponseBody",
                    json!({"requestId":request}),
                    Some(session),
                ),
            )
            .await;
            let Ok(Ok(response)) = response else {
                continue;
            };
            if response["base64Encoded"].as_bool() == Some(true) {
                continue;
            }
            let Some(body) = response["body"]
                .as_str()
                .filter(|body| body.len() <= 1024 * 1024)
            else {
                continue;
            };
            let Ok(payload) = serde_json::from_str::<Value>(body) else {
                continue;
            };
            let extra = match kind {
                ResponseKind::Comments => Engagement::from_comments(&payload, &expected.id),
                ResponseKind::Detail
                    if payload["aweme_detail"]["aweme_id"].as_str() == Some(&expected.id) =>
                {
                    Engagement::from_detail(&payload["aweme_detail"])
                }
                _ => continue,
            };
            page.engagement.merge(extra);
        }
        if let Ok(extra) = super::desktop::map_engagement(&page.html, expected, source) {
            page.engagement.merge(extra);
        }
        if page.engagement.comments.len() >= 20 {
            break;
        }
        // A bounded scroll only exposes the page's public comment rows. No
        // clicking login prompts, reactions, replies, or signing API requests.
        let scroll = format!(
            r#"(() => {{
            if (location.origin + location.pathname !== {}) return;
            const work = document.querySelector('[data-e2e="detail-video-info"][data-e2e-aweme-id="{}"]');
            const root = work?.closest('[data-e2e="video-detail"]');
            const list = root?.querySelector('[data-e2e="comment-list"]');
            const rows = list?.querySelectorAll('[data-e2e="comment-item"]');
            (rows?.length ? rows[rows.length - 1] : list)?.scrollIntoView({{block:'center'}});
        }})()"#,
            serde_json::to_string(source.as_str())?,
            expected.id
        );
        cdp.call(
            "Runtime.evaluate",
            json!({"expression":scroll,"returnByValue":true}),
            Some(session),
        )
        .await?;
        tokio::time::sleep(Duration::from_millis(500)).await;
        let snapshot = cdp
            .call(
                "Runtime.evaluate",
                json!({"expression":expression,"returnByValue":true}),
                Some(session),
            )
            .await?;
        if let Some(html) = snapshot
            .pointer("/result/value/html")
            .and_then(Value::as_str)
            .filter(|html| html.len() <= MAX_DOM_BYTES)
        {
            // Check again after scrolling/navigation before accepting a snapshot.
            if let Ok(extra) = super::desktop::map_engagement(html, expected, source) {
                page.engagement.merge(extra);
                page.html = html.to_owned();
            }
        }
    }
    Ok(())
}

fn readiness_expression(target: &str, id: &str) -> String {
    // Check both canonical identity and the hydrated work node; a generic
    // shell, recommended work, error page or captcha is not a successful fetch.
    const EXPRESSION: &str = r#"(() => {
        const expected = EXPECTED_URL;
        const id = EXPECTED_ID;
        const expectedLocation = new URL(expected);
        const currentLocation = new URL(location.href);
        if (currentLocation.origin !== expectedLocation.origin ||
            currentLocation.pathname !== expectedLocation.pathname) return {};
        const canonical = document.querySelector('link[rel="canonical"]');
        const work = document.querySelector('[data-e2e="detail-video-info"][data-e2e-aweme-id="' + id + '"]');
        const root = work?.closest('[data-e2e="video-detail"]');
        const heading = work?.querySelector('h1')?.textContent?.trim();
        const title = document.querySelector('meta[name="lark:url:video_title"]')?.content?.trim();
        const cover = document.querySelector('meta[name="lark:url:video_cover_image_url"]')?.content;
        const structured = document.querySelector('script[type="application/ld+json"]');
        const author = root?.querySelector('[data-e2e="user-info"]');
        const avatar = author?.querySelector('img');
        if (!canonical || canonical.href !== expected || !work || !heading || !title || !structured ||
            !/^https?:\/\//.test(cover || '') || !author?.textContent?.trim() ||
            !/^https?:\/\//.test(avatar?.currentSrc || avatar?.src || '')) return {};
        const html = document.documentElement.outerHTML;
        return html.length > 4194304 ? {oversized:true} : {html};
    })()"#;
    EXPRESSION
        .replace(
            "EXPECTED_URL",
            &serde_json::to_string(target).expect("serialize URL"),
        )
        .replace(
            "EXPECTED_ID",
            &serde_json::to_string(id).expect("serialize ID"),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[tokio::test]
    async fn incompatible_browser_settings_only_fail_when_fallback_is_requested() {
        let scope = Arc::new(PluginTaskScope::new("anonymous-browser-settings-test"));
        let work = AwemeRef {
            id: "7693165612066999571".into(),
            is_note: false,
        };
        let browser = AnonymousBrowser::new("socks5://127.0.0.1:7897", "", scope.clone()).unwrap();
        assert!(
            browser
                .fetch(&work, false)
                .await
                .unwrap_err()
                .to_string()
                .contains("HTTP/HTTPS")
        );
        let browser = AnonymousBrowser::new("", "./relative-browser", scope.clone()).unwrap();
        assert!(
            browser
                .fetch(&work, false)
                .await
                .unwrap_err()
                .to_string()
                .contains("浏览器程序")
        );
        scope.shutdown().await;
    }

    #[test]
    fn response_capture_only_accepts_bounded_same_work_public_json() {
        let mut capture = ResponseCapture::new("123", "session");
        let event = |host: &str, work: &str, session: &str, kind: &str, id: &str| {
            json!({
                "sessionId":session,"method":"Network.responseReceived","params":{"requestId":id,"type":kind,
                "response":{"status":200,"url":format!("https://{host}/aweme/v1/web/comment/list/?aweme_id={work}")}}
            })
        };
        for bad in [
            event("evil.douyin.com", "123", "session", "XHR", "a"),
            event("www.douyin.com", "456", "session", "XHR", "b"),
            event("www.douyin.com", "123", "other", "XHR", "c"),
            event("www.douyin.com", "123", "session", "Preflight", "d"),
        ] {
            capture.observe(&bad);
        }
        assert!(capture.pending.is_empty());
        for id in 0..12 {
            capture.observe(&event(
                "www-hj.douyin.com",
                "123",
                "session",
                "XHR",
                &id.to_string(),
            ));
        }
        assert_eq!(capture.pending.len(), 8);
        capture.observe(&json!({"sessionId":"session","method":"Network.loadingFinished","params":{"requestId":"0","encodedDataLength":1024}}));
        capture.observe(&json!({"sessionId":"session","method":"Network.loadingFinished","params":{"requestId":"1","encodedDataLength":2097152}}));
        assert_eq!(capture.ready.len(), 1);
        assert_eq!(capture.ready[0].0, "0");
        assert_eq!(capture.pending.len(), 6);
    }

    #[test]
    fn only_evaluation_context_replacement_is_retryable() {
        for message in [
            "Execution context was destroyed.",
            "Cannot find context with specified id",
            "Cannot find default execution context",
        ] {
            let error = json!({"code":-32000,"message":message});
            assert!(navigation_context_changed("Runtime.evaluate", &error));
            assert!(!navigation_context_changed("Page.navigate", &error));
        }
        assert!(!navigation_context_changed(
            "Runtime.evaluate",
            &json!({"code":-32601,"message":"Cannot find context with specified id"})
        ));
        assert!(!navigation_context_changed(
            "Runtime.evaluate",
            &json!({"code":-32000,"message":"Object reference chain is too long"})
        ));
        assert!(!navigation_context_changed(
            "Runtime.evaluate",
            &json!({"code":-32000})
        ));
    }

    #[test]
    fn request_identity_and_proxy_are_strict_and_errors_do_not_echo_inputs() {
        let valid = AwemeRef {
            id: "7693165612066999571".into(),
            is_note: false,
        };
        assert_eq!(
            canonical_target(&valid).unwrap(),
            "https://www.douyin.com/video/7693165612066999571"
        );
        for id in [
            "",
            "1?token=secret",
            "1/../2",
            "１２３",
            "1234567890123456789012345",
        ] {
            assert!(
                canonical_target(&AwemeRef {
                    id: id.into(),
                    is_note: false
                })
                .is_err()
            );
        }
        assert!(
            canonical_target(&AwemeRef {
                is_note: true,
                ..valid
            })
            .is_err()
        );
        assert_eq!(validate_proxy(" ").unwrap(), None);
        assert_eq!(
            validate_proxy("http://127.0.0.1:7897").unwrap().as_deref(),
            Some("http://127.0.0.1:7897/")
        );
        for proxy in [
            "socks5://localhost:1",
            "http://user:secret@localhost:1",
            "https://localhost/path",
            "http://localhost/?secret=1",
            "file:///tmp/proxy",
        ] {
            let error = validate_proxy(proxy).unwrap_err().to_string();
            assert!(!error.contains("secret"));
        }
    }

    #[test]
    fn private_devtools_file_can_only_name_its_own_loopback_session() {
        let nonce = "01234567-89ab-cdef-0123-456789abcdef";
        assert_eq!(
            parse_devtools_endpoint(&format!("9222\n/devtools/browser/{nonce}\n")).unwrap(),
            format!("ws://127.0.0.1:9222/devtools/browser/{nonce}")
        );
        for file in [
            "80\n/devtools/browser/01234567-89ab-cdef-0123-456789abcdef",
            "70000\n/devtools/browser/01234567-89ab-cdef-0123-456789abcdef",
            "9222\nws://external.example/devtools/browser/x",
            "9222\n/devtools/page/01234567-89ab-cdef-0123-456789abcdef",
            "9222\n/devtools/browser/../../private",
            "9222\n/devtools/browser/01234567-89ab-cdef-0123-456789abcdef?token=x",
            "9222\n/devtools/browser/01234567-89ab-cdef-0123-456789abcdef\nextra",
        ] {
            assert!(parse_devtools_endpoint(file).is_err());
        }
    }

    #[cfg(unix)]
    #[test]
    fn launch_keeps_sandbox_and_uses_only_its_disposable_profile() {
        use std::os::unix::fs::PermissionsExt;
        let profile = tempfile::tempdir().unwrap();
        ensure_private_profile(profile.path()).unwrap();
        assert_eq!(
            std::fs::metadata(profile.path())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        let command = browser_command(Path::new("/browser"), profile.path(), None).unwrap();
        let arguments: Vec<_> = command
            .get_args()
            .map(|value| value.to_string_lossy())
            .collect();
        assert!(arguments.iter().any(|value| value == "--no-proxy-server"));
        assert!(
            arguments
                .iter()
                .any(|value| value == "--remote-debugging-address=127.0.0.1")
        );
        assert!(!arguments.iter().any(|value| value.contains("no-sandbox") || value.contains("remote-allow-origins")));
        assert_eq!(arguments.last().unwrap(), "about:blank");
    }

    #[cfg(unix)]
    fn fixture_owner(permit: OwnedSemaphorePermit) -> (Arc<ProcessOwner>, u32, PathBuf) {
        use std::os::unix::process::CommandExt;
        let profile = tempfile::tempdir().unwrap();
        let path = profile.path().to_owned();
        let child = Command::new("/bin/sleep")
            .arg("30")
            .process_group(0)
            .spawn()
            .unwrap();
        let pid = child.id();
        (
            Arc::new(ProcessOwner::new(child, profile, vec![permit])),
            pid,
            path,
        )
    }

    #[cfg(unix)]
    fn assert_reaped(pid: u32, profile: &Path, quota: &Semaphore) {
        assert_eq!(unsafe { libc::kill(pid as libc::pid_t, 0) }, -1);
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ESRCH)
        );
        assert!(!profile.exists());
        assert_eq!(quota.available_permits(), 1);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn scope_shutdown_reaps_process_and_profile_before_releasing_quota() {
        let scope = Arc::new(PluginTaskScope::new("anonymous-browser-cleanup-test"));
        let browser = AnonymousBrowser::new("", "", scope.clone()).unwrap();
        let quota = Arc::new(Semaphore::new(1));
        let (owner, pid, profile) = fixture_owner(quota.clone().acquire_owned().await.unwrap());
        browser.cleanup.register(&owner).unwrap();
        scope.shutdown().await;
        assert_reaped(pid, &profile, &quota);
        owner.cleanup();
        assert!(browser.cleanup.register(&owner).is_err());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cancelled_caller_drops_owner_and_reaps_without_scope_shutdown() {
        let quota = Arc::new(Semaphore::new(1));
        let (owner, pid, profile) = fixture_owner(quota.clone().acquire_owned().await.unwrap());
        let (started, ready) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            let _owner = owner;
            let _ = started.send(());
            std::future::pending::<()>().await;
        });
        ready.await.unwrap();
        task.abort();
        let _ = task.await;
        assert_reaped(pid, &profile, &quota);
    }

    #[tokio::test]
    async fn cdp_media_interception_rejects_media_without_losing_command_response() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            let command: Value =
                serde_json::from_str(socket.next().await.unwrap().unwrap().to_text().unwrap())
                    .unwrap();
            socket.send(Message::Text(json!({"method":"Fetch.requestPaused","sessionId":"session","params":{"requestId":"media"}}).to_string().into())).await.unwrap();
            let rejected: Value =
                serde_json::from_str(socket.next().await.unwrap().unwrap().to_text().unwrap())
                    .unwrap();
            assert_eq!(rejected["method"], "Fetch.failRequest");
            assert_eq!(rejected["params"]["errorReason"], "BlockedByClient");
            socket
                .send(Message::Text(
                    json!({"id":rejected["id"],"result":{}}).to_string().into(),
                ))
                .await
                .unwrap();
            socket
                .send(Message::Text(
                    json!({"id":command["id"],"result":{"ok":true}})
                        .to_string()
                        .into(),
                ))
                .await
                .unwrap();
        });
        let endpoint = parse_devtools_endpoint(&format!(
            "{}\n/devtools/browser/01234567-89ab-cdef-0123-456789abcdef",
            address.port()
        ))
        .unwrap();
        let mut cdp = Cdp::connect(&endpoint).await.unwrap();
        let result = tokio::time::timeout(
            Duration::from_secs(3),
            cdp.call("Runtime.evaluate", json!({}), Some("session")),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(result["ok"], true);
        server.await.unwrap();
    }
}
