use bpi_rs::models::VideoQuality;
use serde::{Deserialize, Serialize};
use url::Url;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Quality {
    P1080,
    P720,
    P480,
    P360,
}
impl Quality {
    pub(crate) fn bpi(self) -> VideoQuality {
        match self {
            Self::P1080 => VideoQuality::P1080,
            Self::P720 => VideoQuality::P720,
            Self::P480 => VideoQuality::P480,
            Self::P360 => VideoQuality::P360,
        }
    }

    pub fn id(self) -> u64 {
        self.bpi().as_u32() as u64
    }
}

pub fn choose_quality<'a>(streams: &'a [DashStream], wanted: Quality) -> Option<&'a DashStream> {
    streams
        .iter()
        .filter(|stream| {
            stream.id <= wanted.id()
                && quality_label(stream.id).is_some()
                && !stream_urls(stream).is_empty()
        })
        .max_by_key(|stream| (stream.id, stream.bandwidth))
}

pub fn stream_urls(stream: &DashStream) -> Vec<String> {
    media_urls(&stream.base_url, &stream.backup_url)
}

pub(crate) fn quality_label(id: u64) -> Option<&'static str> {
    match id {
        6 => Some("240P"),
        16 => Some("360P"),
        32 => Some("480P"),
        64 | 74 => Some("720P"),
        80 | 112 | 116 => Some("1080P"),
        _ => None,
    }
}

pub(crate) fn media_urls(base_url: &str, backup_urls: &[String]) -> Vec<String> {
    std::iter::once(base_url)
        .chain(backup_urls.iter().map(String::as_str))
        .filter_map(|raw| {
            let raw = if raw.starts_with("//") {
                format!("https:{raw}")
            } else {
                raw.to_string()
            };
            let url = Url::parse(&raw).ok()?;
            crate::resolver::url_policy::validate_public_url(&url).ok()?;
            Some(raw)
        })
        .fold(Vec::new(), |mut out, url| {
            if !out.contains(&url) {
                out.push(url);
            }
            out
        })
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct DashStream {
    pub id: u64,
    pub base_url: String,
    pub backup_url: Vec<String>,
    pub bandwidth: u64,
}

/// 上游在同一个 stream 对象里同时下发 `baseUrl` 与 `base_url`（备用地址同理）。
/// 用 `#[serde(alias)]` 声明这两个同义键会被判定为字段重复而整体解析失败，
/// 因此逐键取值，并合并两个键下的备用地址（`media_urls` 负责去重）。
impl<'de> Deserialize<'de> for DashStream {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = serde_json::Map::deserialize(deserializer)?;
        Ok(Self {
            id: raw
                .get("id")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or_default(),
            base_url: json_string(&raw, "baseUrl")
                .or_else(|| json_string(&raw, "base_url"))
                .unwrap_or_default(),
            backup_url: merged_backup_urls(&raw),
            bandwidth: raw
                .get("bandwidth")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or_default(),
        })
    }
}
/// DASH 清单。上游对缺失音轨返回 `flac: {"display": true, "audio": null}`，
/// `flac.audio` 是单对象而 `dolby.audio` 是数组，因此全部字段都必须可空。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DashManifest {
    #[serde(default)]
    pub video: Vec<DashStream>,
    #[serde(default)]
    pub audio: Option<Vec<DashStream>>,
    #[serde(default)]
    pub flac: Option<DashLossless>,
    #[serde(default)]
    pub dolby: Option<DashDolby>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DashLossless {
    #[serde(default)]
    pub audio: Option<DashStream>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DashDolby {
    #[serde(default)]
    pub audio: Option<Vec<DashStream>>,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct DurlStream {
    pub url: String,
    pub backup_url: Vec<String>,
    pub length: Option<u64>,
}

/// 目前 durl 只下发 `backup_url`，但 DASH 已出现同时下发两个同义键的情况，
/// 因此与 `DashStream` 采用同一套逐键合并策略。
impl<'de> Deserialize<'de> for DurlStream {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = serde_json::Map::deserialize(deserializer)?;
        Ok(Self {
            url: json_string(&raw, "url").unwrap_or_default(),
            backup_url: merged_backup_urls(&raw),
            length: raw.get("length").and_then(serde_json::Value::as_u64),
        })
    }
}

fn json_string(raw: &serde_json::Map<String, serde_json::Value>, key: &str) -> Option<String> {
    raw.get(key)
        .and_then(|value| value.as_str())
        .map(str::to_owned)
}

fn json_string_array(raw: &serde_json::Map<String, serde_json::Value>, key: &str) -> Vec<String> {
    raw.get(key)
        .and_then(|value| value.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// 合并 `backupUrl` 与 `backup_url` 两个同义键；重复地址由 `media_urls` 去重。
fn merged_backup_urls(raw: &serde_json::Map<String, serde_json::Value>) -> Vec<String> {
    let mut urls = json_string_array(raw, "backupUrl");
    urls.extend(json_string_array(raw, "backup_url"));
    urls
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PlayUrl {
    /// DASH 的顶层 quality 不是被选中轨道的清晰度，只用于 durl。
    #[serde(default)]
    pub quality: Option<u64>,
    #[serde(default)]
    pub timelength: Option<u64>,
    #[serde(default, deserialize_with = "deserialize_preview")]
    pub is_preview: bool,
    #[serde(default)]
    pub dash: Option<DashManifest>,
    #[serde(default)]
    pub durl: Option<Vec<DurlStream>>,
}

impl PlayUrl {
    pub fn dash_videos(&self) -> Vec<DashStream> {
        self.dash
            .as_ref()
            .map(|dash| dash.video.clone())
            .unwrap_or_default()
    }

    /// 主音轨取 `dash.audio`，缺失时回退到 Dolby 或无损音轨。
    pub fn dash_audios(&self) -> Vec<DashStream> {
        let Some(dash) = self.dash.as_ref() else {
            return Vec::new();
        };
        let mut audios = dash.audio.clone().unwrap_or_default();
        if audios.is_empty()
            && let Some(dolby) = dash.dolby.as_ref().and_then(|dolby| dolby.audio.clone())
        {
            audios = dolby;
        }
        if audios.is_empty()
            && let Some(flac) = dash.flac.as_ref().and_then(|flac| flac.audio.clone())
        {
            audios = vec![flac];
        }
        audios
    }

    pub fn progressive(&self) -> Option<(&str, &[String], Option<u64>)> {
        let items = self.durl.as_ref()?;
        // 下载器把 URL 列表视作同一媒体的备用来源，不能把多段 durl 的第一段
        // 当作完整视频，也不能把试看流当作正式内容。
        if self.is_preview || items.len() != 1 {
            return None;
        }
        let stream = &items[0];
        if let (Some(length), Some(total)) = (stream.length, self.timelength)
            && length < total
        {
            return None;
        }
        Some((stream.url.as_str(), &stream.backup_url, self.quality))
    }

    pub fn complete_progressive(&self) -> Option<(&str, &[String], u64)> {
        let (url, backups, quality) = self.progressive()?;
        let total = self.timelength.filter(|total| *total > 0)?;
        let length = self.durl.as_ref()?.first()?.length?;
        (length >= total).then_some((url, backups, quality?))
    }
}

fn deserialize_preview<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<bool, D::Error> {
    match serde_json::Value::deserialize(deserializer)? {
        serde_json::Value::Null => Ok(false),
        serde_json::Value::Bool(value) => Ok(value),
        serde_json::Value::Number(value) => value
            .as_u64()
            .map(|value| value != 0)
            .ok_or_else(|| serde::de::Error::custom("invalid preview flag")),
        _ => Err(serde::de::Error::custom("invalid preview flag")),
    }
}
