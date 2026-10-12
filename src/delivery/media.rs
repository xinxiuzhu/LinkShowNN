//! 音视频与缩略图传输。默认传文件内容，宿主路径只用于显式共享目录模式。

use std::path::Path;

use anyhow::{Context, Result, bail, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use nuono_plugin::SendableMessage;
use serde_json::{Map, Value};
use tokio::io::AsyncReadExt;

use crate::download::{DownloadCandidate, MediaKind};

/// 原始媒体和缩略图的合计预算；Base64 在线上约增大三分之一。
/// 下载允许 -1，但通过消息内嵌传输仍必须有内存上限。
pub(crate) const MAX_INLINE_MEDIA_BYTES: u64 = 64 * 1024 * 1024;

pub(crate) async fn media_message(
    candidate: &DownloadCandidate,
    path: &Path,
    thumb: Option<&Path>,
    transport: &str,
) -> Result<SendableMessage> {
    media_message_with_budget(candidate, path, thumb, transport, MAX_INLINE_MEDIA_BYTES).await
}

async fn media_message_with_budget(
    candidate: &DownloadCandidate,
    path: &Path,
    thumb: Option<&Path>,
    transport: &str,
    max_bytes: u64,
) -> Result<SendableMessage> {
    let thumb = (candidate.kind == MediaKind::Video)
        .then_some(thumb)
        .flatten();
    let (file, thumb) = match transport {
        "base64" => {
            let (file, bytes) = inline_file(path, max_bytes).await?;
            let thumb = match thumb {
                Some(path) => Some(inline_file(path, max_bytes - bytes).await?.0),
                None => None,
            };
            (file, thumb)
        }
        "local" => (file_uri(path)?, thumb.map(file_uri).transpose()?),
        _ => bail!("unsupported media transport"),
    };
    // Own the strings directly instead of serializing an SDK segment and then
    // reparsing it just to add OneBot's optional video thumb extension.
    let mut data = Map::new();
    data.insert("file".into(), Value::String(file));
    if let Some(thumb) = thumb {
        data.insert("thumb".into(), Value::String(thumb));
    }
    let kind = match candidate.kind {
        MediaKind::Video => "video",
        MediaKind::Audio => "record",
    };
    let mut segment = Map::new();
    segment.insert("type".into(), Value::String(kind.into()));
    segment.insert("data".into(), Value::Object(data));
    Ok(SendableMessage::Json(
        Value::Array(vec![Value::Object(segment)]).to_string(),
    ))
}

fn file_uri(path: &Path) -> Result<String> {
    url::Url::from_file_path(path)
        .map(String::from)
        .map_err(|_| anyhow::anyhow!("local media path must be absolute"))
}

/// Encode incrementally with a small read buffer; do not also keep an entire
/// raw video allocation. Check both metadata and bytes actually read, including
/// files changed between metadata and reading.
async fn inline_file(path: &Path, max_bytes: u64) -> Result<(String, u64)> {
    let file = tokio::fs::File::open(path)
        .await
        .context("failed to open media for delivery")?;
    let metadata = file.metadata().await?;
    ensure!(
        metadata.is_file(),
        "media delivery source is not a regular file"
    );
    ensure!(metadata.len() > 0, "media delivery source is empty");
    ensure!(
        metadata.len() <= max_bytes,
        "inline media exceeds 64 MiB delivery budget; use a shared directory with local transport for larger files"
    );
    let capacity = usize::try_from(metadata.len().div_ceil(3) * 4 + 9)?;
    let mut encoded = String::with_capacity(capacity);
    encoded.push_str("base64://");
    let mut file = file.take(max_bytes + 1);
    let mut buffer = [0u8; 48 * 1024 + 2];
    let mut carry = 0;
    let mut total = 0;
    loop {
        let read = file.read(&mut buffer[carry..]).await?;
        if read == 0 {
            STANDARD.encode_string(&buffer[..carry], &mut encoded);
            break;
        }
        total += read as u64;
        ensure!(
            total <= max_bytes,
            "inline media exceeds 64 MiB delivery budget; use a shared directory with local transport for larger files"
        );
        let available = carry + read;
        let complete = available / 3 * 3;
        STANDARD.encode_string(&buffer[..complete], &mut encoded);
        carry = available - complete;
        buffer.copy_within(complete..available, 0);
    }
    ensure!(total > 0, "media delivery source is empty");
    Ok((encoded, total))
}

#[cfg(test)]
mod tests;
