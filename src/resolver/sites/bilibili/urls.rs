use super::error::Error;
use regex::Regex;
use std::sync::OnceLock;
use url::Url;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    Video {
        bvid: Option<String>,
        aid: Option<u64>,
        page: u32,
    },
    Bangumi {
        ep_id: Option<u64>,
        season_id: Option<u64>,
        media_id: Option<u64>,
        page: u32,
    },
    Live {
        room_id: u64,
    },
    Opus {
        id: u64,
    },
    Article {
        id: u64,
    },
    Favorite {
        id: u64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BilibiliUrl {
    pub normalized: Url,
    pub target: Target,
}

fn page(url: &Url) -> u32 {
    url.query_pairs()
        .find_map(|(k, v)| {
            (k.eq_ignore_ascii_case("p"))
                .then(|| v.parse().ok())
                .flatten()
        })
        .or_else(|| {
            url.fragment().and_then(|fragment| {
                Regex::new(r"(?:^|[#&])p(?:age)?=(\d+)")
                    .ok()?
                    .captures(fragment)?
                    .get(1)?
                    .as_str()
                    .parse()
                    .ok()
            })
        })
        .unwrap_or(1)
}

fn captures<'a>(regex: &'a Regex, text: &'a str) -> Option<regex::Captures<'a>> {
    regex.captures(text)
}

pub fn parse_url(raw: &str) -> Result<Option<BilibiliUrl>, Error> {
    let text = raw
        .trim()
        .trim_end_matches(|c: char| ",.;:!?)]}>，。；：！？）】》」』".contains(c));
    let mut normalized = Url::parse(text).map_err(|e| Error::InvalidUrl(e.to_string()))?;
    if normalized.username() != ""
        || normalized.password().is_some()
        || !matches!(normalized.port(), None | Some(80) | Some(443))
    {
        return Ok(None);
    }
    let selected_page = page(&normalized);
    let host = normalized
        .host_str()
        .unwrap_or_default()
        .trim_end_matches('.')
        .to_ascii_lowercase();
    let allowed = host == "b23.tv"
        || host.ends_with(".b23.tv")
        || host == "bili2233.cn"
        || host.ends_with(".bili2233.cn")
        || host == "bilibili.com"
        || host.ends_with(".bilibili.com");
    if !allowed || !matches!(normalized.scheme(), "http" | "https") {
        return Ok(None);
    }
    normalized.set_fragment(None);
    let text = normalized.as_str();
    static RE_BV: OnceLock<Regex> = OnceLock::new();
    static RE_AV: OnceLock<Regex> = OnceLock::new();
    static RE_EP: OnceLock<Regex> = OnceLock::new();
    static RE_SS: OnceLock<Regex> = OnceLock::new();
    static RE_MD: OnceLock<Regex> = OnceLock::new();
    static RE_LIVE: OnceLock<Regex> = OnceLock::new();
    static RE_OPUS: OnceLock<Regex> = OnceLock::new();
    static RE_CV: OnceLock<Regex> = OnceLock::new();
    static RE_FAV: OnceLock<Regex> = OnceLock::new();
    let target = if let Some(c) = captures(
        RE_BV.get_or_init(|| Regex::new(r"(?i)\b(BV[1-9A-HJ-NP-Za-km-z]{10})\b").unwrap()),
        text,
    ) {
        Target::Video {
            bvid: Some(c[1].to_string()),
            aid: None,
            page: selected_page,
        }
    } else if let Some(c) = captures(
        RE_AV.get_or_init(|| Regex::new(r"(?i)\bav(\d{1,12})\b").unwrap()),
        text,
    ) {
        Target::Video {
            bvid: None,
            aid: c[1].parse().ok(),
            page: selected_page,
        }
    } else if let Some(c) = captures(
        RE_EP.get_or_init(|| Regex::new(r"(?i)\bep(\d+)\b").unwrap()),
        text,
    ) {
        Target::Bangumi {
            ep_id: c[1].parse().ok(),
            season_id: None,
            media_id: None,
            page: selected_page,
        }
    } else if let Some(c) = captures(
        RE_SS.get_or_init(|| Regex::new(r"(?i)\bss(\d+)\b").unwrap()),
        text,
    ) {
        Target::Bangumi {
            ep_id: None,
            season_id: c[1].parse().ok(),
            media_id: None,
            page: selected_page,
        }
    } else if let Some(c) = captures(
        RE_MD.get_or_init(|| Regex::new(r"(?i)\bmd(\d+)\b").unwrap()),
        text,
    ) {
        Target::Bangumi {
            ep_id: None,
            season_id: None,
            media_id: c[1].parse().ok(),
            page: selected_page,
        }
    } else if let Some(c) = captures(
        RE_LIVE.get_or_init(|| Regex::new(r"(?i)live\.bilibili\.com/(?:blanc/)?(\d+)").unwrap()),
        text,
    ) {
        Target::Live {
            room_id: c[1].parse().unwrap_or_default(),
        }
    } else if let Some(c) = captures(
        RE_OPUS.get_or_init(|| {
            Regex::new(r"(?i)(?:t|m)\.bilibili\.com/(?:opus/)?(\d+)|www\.bilibili\.com/opus/(\d+)")
                .unwrap()
        }),
        text,
    ) {
        Target::Opus {
            id: c
                .get(1)
                .or_else(|| c.get(2))
                .and_then(|m| m.as_str().parse().ok())
                .unwrap_or_default(),
        }
    } else if let Some(c) = captures(
        RE_CV.get_or_init(|| Regex::new(r"(?i)(?:/read/(?:cv|mobile/)|/read/cv)(\d+)").unwrap()),
        text,
    ) {
        Target::Article {
            id: c[1].parse().unwrap_or_default(),
        }
    } else if let Some(c) = captures(
        RE_FAV.get_or_init(|| Regex::new(r"(?i)(?:[?&]fid=|/favlist/)(\d+)").unwrap()),
        text,
    ) {
        Target::Favorite {
            id: c[1].parse().unwrap_or_default(),
        }
    } else {
        return Ok(None);
    };
    Ok(Some(BilibiliUrl { normalized, target }))
}
