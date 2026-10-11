use bpi_rs::models::VideoQuality;
use serde::{Deserialize, Serialize};

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
    let order = [Quality::P1080, Quality::P720, Quality::P480, Quality::P360];
    let start = order.iter().position(|q| *q == wanted).unwrap_or(1);
    order[start..]
        .iter()
        .find_map(|quality| streams.iter().find(|stream| stream.id == quality.id()))
        .or_else(|| streams.iter().max_by_key(|stream| stream.bandwidth))
}

pub fn stream_urls(stream: &DashStream) -> Vec<String> {
    std::iter::once(stream.base_url.clone())
        .chain(stream.backup_url.iter().cloned())
        .filter(|url| !url.is_empty())
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
/// 因此逐键取值，并合并两个键下的备用地址（`unique_urls` 负责去重）。
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
}

/// 目前 durl 只下发 `backup_url`，但 DASH 已出现同时下发两个同义键的情况，
/// 因此与 `DashStream` 采用同一套逐键合并策略。
impl<'de> Deserialize<'de> for DurlStream {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = serde_json::Map::deserialize(deserializer)?;
        Ok(Self {
            url: json_string(&raw, "url").unwrap_or_default(),
            backup_url: merged_backup_urls(&raw),
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

/// 合并 `backupUrl` 与 `backup_url` 两个同义键；重复地址由 `unique_urls` 去重。
fn merged_backup_urls(raw: &serde_json::Map<String, serde_json::Value>) -> Vec<String> {
    let mut urls = json_string_array(raw, "backupUrl");
    urls.extend(json_string_array(raw, "backup_url"));
    urls
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PlayUrl {
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

    pub fn progressive(&self) -> Option<(&str, &[String])> {
        self.durl
            .as_ref()
            .and_then(|items| items.first())
            .map(|stream| (stream.url.as_str(), stream.backup_url.as_slice()))
    }
}
