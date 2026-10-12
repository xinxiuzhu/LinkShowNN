//! 媒体下载执行：并发控制、临时文件清理和音视频合流。

use std::{
    net::IpAddr,
    path::{Component, Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use tokio::net::lookup_host;

use anyhow::{Context, Result, bail};
use reqwest::{Client, Response, header};
use tokio::{
    fs::File,
    io::AsyncWriteExt,
    process::Command,
    sync::{OwnedSemaphorePermit, Semaphore},
};
use tracing::warn;
use url::Url;

use super::{DownloadCandidate, Downloaded, MediaKind};
use crate::resolver::url_policy::validate_public_url;

const MEDIA_ATTEMPTS_TIMEOUT: Duration = Duration::from_secs(180);
const MAX_MEDIA_SOURCES: usize = 3;

/// A size limit is terminal: another source must not circumvent the user's budget.
#[derive(Debug, thiserror::Error)]
#[error("media exceeds configured size limit")]
struct MediaSizeLimit;

#[derive(Clone)]
pub struct DownloadManager {
    client: Client,
    semaphore: Arc<Semaphore>,
    queue: Arc<Semaphore>,
    max_bytes: Option<u64>,
    data_dir: PathBuf,
}

impl DownloadManager {
    pub fn new(
        proxy: Option<&str>,
        concurrent: usize,
        queue_limit: usize,
        max_mb: i64,
        data_dir: PathBuf,
    ) -> Result<Self> {
        let max_bytes = if max_mb < 0 {
            None
        } else {
            Some(
                (max_mb as u64)
                    .checked_mul(1024 * 1024)
                    .context("configured media size limit exceeds supported bytes")?,
            )
        };
        let mut builder = Client::builder()
            .connect_timeout(std::time::Duration::from_secs(5))
            .timeout(std::time::Duration::from_secs(180))
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy();
        if let Some(proxy) = proxy.filter(|value| !value.trim().is_empty()) {
            builder = builder.proxy(reqwest::Proxy::all(proxy.trim())?);
        }
        Ok(Self {
            client: builder.build()?,
            semaphore: Arc::new(Semaphore::new(concurrent.max(1))),
            queue: Arc::new(Semaphore::new(queue_limit.max(1))),
            max_bytes,
            data_dir: absolute_dir(data_dir)?,
        })
    }

    pub fn try_reserve(&self) -> Option<DownloadReservation> {
        self.queue
            .clone()
            .try_acquire_owned()
            .ok()
            .map(|queue_permit| DownloadReservation {
                manager: self.clone(),
                queue_permit,
            })
    }

    /// 下载媒体本体，成功后再尽力取一次缩略图。
    ///
    /// 缩略图失败不往上抛：视频能发出去就是成功，只是 QQ 少一张首帧。
    async fn download_reserved(&self, candidate: &DownloadCandidate) -> Result<Downloaded> {
        let media = self.download_media(candidate).await?;
        let thumb = self.download_thumb(candidate, media.path()).await;
        // Until both awaits finish, cancellation still owns and removes every
        // file. Published paths must survive the later platform media upload.
        Ok(Downloaded {
            path: media.keep(),
            thumb: thumb.map(PendingFile::keep),
        })
    }

    /// 缩略图与视频同目录同名，只换后缀，随视频一起被清理策略覆盖。
    async fn download_thumb(
        &self,
        candidate: &DownloadCandidate,
        media: &Path,
    ) -> Option<PendingFile> {
        if candidate.kind != MediaKind::Video {
            return None;
        }
        let cover_url = candidate.cover_url.as_deref()?;
        let target = media.with_extension("thumb.jpg");
        match self
            .download_stream(cover_url, &candidate.referer, &target)
            .await
        {
            Ok(path) => Some(path),
            Err(error) => {
                warn!(error = %error, "video thumbnail download failed; sending without thumb");
                None
            }
        }
    }

    async fn download_media(&self, candidate: &DownloadCandidate) -> Result<PendingFile> {
        let _permit = self
            .semaphore
            .acquire()
            .await
            .map_err(|_| anyhow::anyhow!("media download semaphore is closed"))?;
        tokio::fs::create_dir_all(&self.data_dir).await?;
        let safe_name = unique_name(&sanitize_name(&candidate.filename)?);
        let output = self.data_dir.join(&safe_name);
        let video_path = self.data_dir.join(format!(".{safe_name}.video"));
        let video = self.download_sources(candidate, &video_path).await?;
        // Reserve a fresh output before rename/ffmpeg can overwrite it. Never
        // register an existing user file for cancellation cleanup.
        let (output, output_file) = PendingFile::create_new(output)?;
        drop(output_file);
        let Some(audio_url) = candidate.audio_url.as_deref() else {
            // A same-directory rename is short and synchronous: cancellation
            // must not race a detached filesystem job recreating the output.
            std::fs::rename(video.path(), output.path())?;
            return Ok(output);
        };
        let audio_path = self.data_dir.join(format!(".{safe_name}.audio"));
        let audio = self
            .download_stream(audio_url, &candidate.referer, &audio_path)
            .await?;
        self.mux_with_ffmpeg(video.path(), audio.path(), output.path())
            .await?;
        if let Some(max_bytes) = self.max_bytes
            && tokio::fs::metadata(output.path()).await?.len() > max_bytes
        {
            return Err(MediaSizeLimit.into());
        }
        Ok(output)
    }

    async fn download_sources(
        &self,
        candidate: &DownloadCandidate,
        path: &PathBuf,
    ) -> Result<PendingFile> {
        // All source attempts share this deadline. Each failed stream's owner
        // removes its partial file before the next source can reuse the path.
        tokio::time::timeout(MEDIA_ATTEMPTS_TIMEOUT, async {
            let mut attempted = Vec::new();
            let mut last_error = None;
            for source in std::iter::once(&candidate.url).chain(&candidate.fallback_urls) {
                let source = source.trim();
                if source.is_empty() || attempted.contains(&source) {
                    continue;
                }
                if attempted.len() == MAX_MEDIA_SOURCES {
                    break;
                }
                attempted.push(source);
                match self.download_stream(source, &candidate.referer, path).await {
                    Ok(file) => return Ok(file),
                    Err(error) if error.downcast_ref::<MediaSizeLimit>().is_some() => {
                        return Err(error);
                    }
                    Err(error) => last_error = Some(error),
                }
            }
            Err(last_error.context("media has no download source")?)
        })
        .await
        .context("media download attempts timed out")?
    }

    async fn download_stream(
        &self,
        source: &str,
        referer: &str,
        path: &PathBuf,
    ) -> Result<PendingFile> {
        let url = Url::parse(source).context("invalid media URL")?;
        validate_public_url(&url)?;
        let response = self.fetch_public(url, referer).await?;
        if let Some(max_bytes) = self.max_bytes
            && response
                .content_length()
                .is_some_and(|size| size > max_bytes)
        {
            return Err(MediaSizeLimit.into());
        }
        if response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(|value| value.split(';').next().unwrap_or_default().trim())
            .is_some_and(|mime| {
                mime.eq_ignore_ascii_case("text/html")
                    || mime.eq_ignore_ascii_case("application/json")
            })
        {
            bail!("media source returned an HTML or JSON response");
        }
        let (pending, file) = PendingFile::create_new(path.clone())?;
        // Declared after the owner so cancellation closes the file first.
        let mut file = File::from_std(file);
        let mut total = 0u64;
        let mut stream = response.bytes_stream();
        use futures_util::StreamExt;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            total = total.saturating_add(chunk.len() as u64);
            if self.max_bytes.is_some_and(|max_bytes| total > max_bytes) {
                return Err(MediaSizeLimit.into());
            }
            file.write_all(&chunk).await?;
        }
        file.flush().await?;
        Ok(pending)
    }

    async fn mux_with_ffmpeg(&self, video: &Path, audio: &Path, output: &Path) -> Result<()> {
        let ffmpeg = std::env::var_os("NUONO_FFMPEG_BIN").unwrap_or_else(|| "ffmpeg".into());
        let mut command = Command::new(ffmpeg);
        command.kill_on_drop(true);
        let result = tokio::time::timeout(
            Duration::from_secs(5 * 60),
            command
                .args(["-hide_banner", "-loglevel", "error", "-y"])
                .args(["-i"])
                .arg(video)
                .args(["-i"])
                .arg(audio)
                .args([
                    "-map",
                    "0:v:0",
                    "-map",
                    "1:a:0",
                    "-c",
                    "copy",
                    "-movflags",
                    "+faststart",
                ])
                .arg(output)
                .output(),
        )
        .await
        .context("ffmpeg timed out after 5 minutes")?
        .context("failed to start ffmpeg; install ffmpeg and ensure it is in PATH")?;
        if !result.status.success() {
            let message = String::from_utf8_lossy(&result.stderr);
            bail!("ffmpeg failed: {}", message.trim());
        }
        Ok(())
    }

    async fn fetch_public(&self, source: Url, referer: &str) -> Result<Response> {
        let mut current = source;
        for _ in 0..=5 {
            validate_public_url(&current)?;
            validate_resolved_addresses(&current).await?;
            let response = self
                .client
                .get(current.clone())
                .header(
                    header::USER_AGENT,
                    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 Chrome/131 Safari/537.36",
                )
                .header(header::REFERER, referer)
                .send()
                .await?;
            if response.status().is_redirection() {
                let location = response
                    .headers()
                    .get(header::LOCATION)
                    .and_then(|value| value.to_str().ok())
                    .context("media redirect has no Location")?;
                current = current.join(location)?;
                continue;
            }
            return Ok(response.error_for_status()?);
        }
        bail!("media URL redirected too many times")
    }
}

async fn validate_resolved_addresses(url: &Url) -> Result<()> {
    let host = url.host_str().context("media URL has no hostname")?;
    if host.trim_matches(['[', ']']).parse::<IpAddr>().is_ok() {
        return Ok(());
    }
    let port = url
        .port_or_known_default()
        .context("media URL has no port")?;
    let mut found = false;
    for address in lookup_host((host, port)).await? {
        found = true;
        let mut literal = url.clone();
        literal
            .set_host(Some(&address.ip().to_string()))
            .map_err(|_| anyhow::anyhow!("resolved media IP is invalid"))?;
        validate_public_url(&literal)?;
    }
    if !found {
        bail!("media hostname resolved to no addresses");
    }
    Ok(())
}

pub struct DownloadReservation {
    manager: DownloadManager,
    queue_permit: OwnedSemaphorePermit,
}

impl DownloadReservation {
    pub async fn download(self, candidate: &DownloadCandidate) -> Result<Downloaded> {
        let result = self.manager.download_reserved(candidate).await;
        drop(self.queue_permit);
        result
    }
}

/// Owns only a file created by this download. Drop is synchronous because the
/// runtime may be shutting down and cannot guarantee another cleanup task runs.
struct PendingFile {
    path: PathBuf,
    remove_on_drop: bool,
}

impl PendingFile {
    fn create_new(path: PathBuf) -> Result<(Self, std::fs::File)> {
        // Synchronous creation closes the cancellation window between the OS
        // creating the file and an async caller acquiring its cleanup guard.
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?;
        Ok((
            Self {
                path,
                remove_on_drop: true,
            },
            file,
        ))
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn keep(mut self) -> PathBuf {
        self.remove_on_drop = false;
        std::mem::take(&mut self.path)
    }
}

impl Drop for PendingFile {
    fn drop(&mut self) {
        if self.remove_on_drop
            && let Err(error) = std::fs::remove_file(&self.path)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            warn!(path = %self.path.display(), error = %error,
                "cancelled media file cleanup failed");
        }
    }
}

fn absolute_dir(dir: PathBuf) -> Result<PathBuf> {
    if dir.is_absolute() {
        return Ok(dir);
    }
    let home = std::env::var_os("HOME")
        .or_else(|| {
            if cfg!(target_os = "windows") {
                std::env::var_os("USERPROFILE")
            } else {
                None
            }
        })
        .map(PathBuf::from);
    if matches!(dir.components().next(), Some(Component::Normal(name)) if name.to_string_lossy().starts_with('~'))
    {
        return resolve_media_path(&dir, Path::new(""), home.as_deref());
    }
    let base = std::env::current_dir().context("failed to resolve current directory")?;
    resolve_media_path(&dir, &base, home.as_deref())
}

/// Expand only the current user's home marker and normalize without requiring
/// the output directory to exist. Explicit inputs keep tests independent of HOME.
fn resolve_media_path(dir: &Path, base: &Path, home: Option<&Path>) -> Result<PathBuf> {
    let mut parts = dir.components();
    let expanded = match parts.next() {
        Some(Component::Normal(name)) if name == "~" => {
            let home =
                home.context("media directory uses '~' but home directory is unavailable")?;
            if !home.is_absolute() {
                bail!("media home directory must be an absolute path");
            }
            home.join(parts.as_path())
        }
        Some(Component::Normal(name)) if name.to_string_lossy().starts_with('~') => {
            bail!("media directory supports '~/' but not '~user' paths");
        }
        _ => dir.to_path_buf(),
    };
    // Preserve absolute paths: removing `..` lexically changes their meaning
    // when a preceding component is a symbolic link.
    if expanded.is_absolute() {
        return Ok(expanded);
    }
    let rooted = base.join(expanded);
    let mut resolved = PathBuf::new();
    for part in rooted.components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir => {
                if !resolved.pop() {
                    bail!("media directory escapes the filesystem root");
                }
            }
            other => resolved.push(other),
        }
    }
    if !resolved.is_absolute() {
        bail!("media directory must resolve to an absolute path");
    }
    Ok(resolved)
}

fn sanitize_name(name: &str) -> Result<String> {
    let safe_name = name
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_'))
        .take(80)
        .collect::<String>();
    if safe_name.is_empty() {
        bail!("media filename is empty");
    }
    Ok(safe_name)
}

fn unique_name(name: &str) -> String {
    match name.rsplit_once('.') {
        Some((stem, extension)) => format!("{stem}.{}.{}", uuid::Uuid::new_v4(), extension),
        None => format!("{name}.{}", uuid::Uuid::new_v4()),
    }
}

#[cfg(test)]
mod tests {
    use super::{DownloadManager, PendingFile, absolute_dir, resolve_media_path, sanitize_name};
    use std::path::{Path, PathBuf};

    #[test]
    fn sanitizes_download_names() {
        assert_eq!(
            sanitize_name("../../video file?.mp4").unwrap(),
            "....videofile.mp4"
        );
        assert!(sanitize_name(" ??? ").is_err());
    }

    #[test]
    fn resolves_relative_media_dir_to_absolute_path() {
        let resolved = absolute_dir(PathBuf::from("./data/LinkShowNN/media")).unwrap();
        assert!(resolved.is_absolute());
        assert!(resolved.ends_with("data/LinkShowNN/media"));

        let absolute = PathBuf::from("/tmp/nuono-media");
        assert_eq!(absolute_dir(absolute.clone()).unwrap(), absolute);
    }

    #[test]
    fn expands_current_home_before_resolving_the_media_directory() {
        let root = tempfile::tempdir().unwrap();
        let base = root.path().join("Nuono");
        let home = root.path().join("home");
        assert_eq!(
            resolve_media_path(Path::new("~/Downloads/LinkShowNN"), &base, Some(&home)).unwrap(),
            home.join("Downloads/LinkShowNN")
        );
        assert_eq!(
            resolve_media_path(Path::new("~"), &base, Some(&home)).unwrap(),
            home
        );
    }

    #[test]
    fn rejects_missing_home_and_other_users_instead_of_creating_literal_tilde_dirs() {
        let root = tempfile::tempdir().unwrap();
        let base = root.path();
        let missing = resolve_media_path(Path::new("~/Downloads"), base, None).unwrap_err();
        assert!(
            missing
                .to_string()
                .contains("home directory is unavailable")
        );
        for input in ["~another/Downloads", "~another", "~Downloads"] {
            let error = resolve_media_path(Path::new(input), base, Some(base)).unwrap_err();
            assert!(error.to_string().contains("not '~user'"));
        }
        assert!(
            resolve_media_path(Path::new("~/Downloads"), base, Some(Path::new("home"))).is_err()
        );
    }

    #[test]
    fn preserves_absolute_paths_and_resolves_relative_directories_without_existing_paths() {
        let root = tempfile::tempdir().unwrap();
        let base = root.path().join("Nuono");
        assert_eq!(
            resolve_media_path(Path::new("./data/../media"), &base, None).unwrap(),
            base.join("media")
        );
        let absolute = root.path().join("downloads/./old/../LinkShowNN");
        assert_eq!(
            resolve_media_path(&absolute, &base, None).unwrap(),
            absolute
        );
        let filesystem_root = root.path().ancestors().last().unwrap();
        assert!(resolve_media_path(Path::new(".."), filesystem_root, None).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn absolute_and_home_paths_preserve_parent_components_after_symlinks() {
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("target/child");
        std::fs::create_dir_all(&target).unwrap();
        let link = root.path().join("link");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let configured = link.join("../media");
        let resolved = resolve_media_path(&configured, Path::new(""), None).unwrap();
        assert_eq!(resolved, configured);
        std::fs::create_dir_all(&resolved).unwrap();
        assert!(root.path().join("target/media").is_dir());
        assert!(!root.path().join("media").exists());
        let home_resolved =
            resolve_media_path(Path::new("~/../other"), Path::new(""), Some(&link)).unwrap();
        std::fs::create_dir_all(home_resolved).unwrap();
        assert!(root.path().join("target/other").is_dir());
        assert!(!root.path().join("other").exists());
    }

    #[test]
    fn media_size_configuration_keeps_negative_unlimited_and_rejects_overflow() {
        let directory = tempfile::tempdir().unwrap();
        for (limit, expected) in [(-1, None), (0, Some(0)), (1, Some(1024 * 1024))] {
            let manager =
                DownloadManager::new(None, 1, 1, limit, directory.path().to_owned()).unwrap();
            assert_eq!(manager.max_bytes, expected);
        }
        let error = DownloadManager::new(None, 1, 1, i64::MAX, directory.path().to_owned())
            .err()
            .unwrap();
        assert!(
            error
                .to_string()
                .contains("size limit exceeds supported bytes")
        );
    }

    #[tokio::test]
    async fn cancelling_media_work_removes_unpublished_files() {
        let dir = tempfile::tempdir().unwrap();
        let paths: Vec<_> = [
            ".request.video",
            ".request.audio",
            "request.mp4",
            "request.thumb.jpg",
        ]
        .into_iter()
        .map(|name| dir.path().join(name))
        .collect();
        let task_paths = paths.clone();
        let (started, ready) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            let _pending: Vec<_> = task_paths
                .into_iter()
                .map(|path| {
                    let (pending, file) = PendingFile::create_new(path).unwrap();
                    drop(file);
                    pending
                })
                .collect();
            started.send(()).unwrap();
            std::future::pending::<()>().await;
        });
        ready.await.unwrap();
        assert!(paths.iter().all(|path| path.exists()));
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        assert!(paths.iter().all(|path| !path.exists()));
    }

    #[test]
    fn published_media_survives_while_existing_files_are_never_claimed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("published.mp4");
        let (pending, file) = PendingFile::create_new(path.clone()).unwrap();
        drop(file);
        assert_eq!(pending.keep(), path);
        std::fs::write(&path, b"already owned").unwrap();
        assert!(PendingFile::create_new(path.clone()).is_err());
        assert_eq!(std::fs::read(path).unwrap(), b"already owned");
    }
}

#[cfg(test)]
#[path = "transport_tests.rs"]
mod transport_tests;
