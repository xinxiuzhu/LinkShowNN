//! 与平台无关的 HTML/JSON 提取工具。
//!
//! 各平台适配器共用这些函数，平台特有的字段知识留在各自模块里。

use regex::Regex;
use serde_json::Value;
use url::Url;

use crate::resolver::{MediaInfo, Platform};

/// Open Graph 元数据是所有分享页的公共底座，平台适配器在此基础上增强。
pub fn page_metadata(html: &str, source: &Url, platform: Platform) -> MediaInfo {
    MediaInfo {
        title: meta_content(html, "og:title").or_else(|| html_title(html)),
        description: meta_content(html, "og:description"),
        cover_url: meta_content(html, "og:image"),
        video_url: meta_content(html, "og:video").or_else(|| meta_content(html, "og:video:url")),
        source_url: source.to_string(),
        platform,
        raw: serde_json::json!({"platform": platform.as_str(), "html_fallback": true}),
        ..Default::default()
    }
}

pub fn meta_content(html: &str, property: &str) -> Option<String> {
    let escaped = regex::escape(property);
    let patterns = [
        format!(r#"(?is)<meta[^>]+property=["']{escaped}["'][^>]+content=["'](.*?)["']"#),
        format!(r#"(?is)<meta[^>]+content=["'](.*?)["'][^>]+property=["']{escaped}["']"#),
    ];
    patterns.into_iter().find_map(|pattern| {
        Regex::new(&pattern)
            .ok()?
            .captures(html)?
            .get(1)
            .map(|value| decode_html(value.as_str()))
    })
}

pub fn html_title(html: &str) -> Option<String> {
    Regex::new(r"(?is)<title[^>]*>(.*?)</title>")
        .ok()?
        .captures(html)?
        .get(1)
        .map(|value| decode_html(value.as_str()).trim().to_string())
        .filter(|value| !value.is_empty())
}

/// 抓取内联 SSR 状态，例如 `window.INIT_STATE = {...}</script>`。
pub fn captured_json(html: &str, pattern: &str) -> Option<Value> {
    let raw = Regex::new(pattern)
        .ok()?
        .captures(html)?
        .get(1)?
        .as_str()
        .trim();
    serde_json::from_str(raw).ok()
}

pub fn find_object_with_key<'a>(value: &'a Value, key: &str) -> Option<&'a Value> {
    match value {
        Value::Object(map) => {
            if map.contains_key(key) {
                return Some(value);
            }
            map.values()
                .find_map(|value| find_object_with_key(value, key))
        }
        Value::Array(values) => values
            .iter()
            .find_map(|value| find_object_with_key(value, key)),
        _ => None,
    }
}

pub fn find_string(value: &Value, keys: &[&str]) -> Option<String> {
    match value {
        Value::Object(map) => {
            for key in keys {
                if let Some(text) = map.get(*key).and_then(Value::as_str)
                    && !text.trim().is_empty()
                {
                    return Some(text.to_string());
                }
            }
            map.values().find_map(|value| find_string(value, keys))
        }
        Value::Array(values) => values.iter().find_map(|value| find_string(value, keys)),
        _ => None,
    }
}

pub fn collect_strings(value: &Value, keys: &[&str], limit: usize) -> Vec<String> {
    fn visit(value: &Value, keys: &[&str], limit: usize, output: &mut Vec<String>) {
        if output.len() >= limit {
            return;
        }
        match value {
            Value::Object(map) => {
                for key in keys {
                    if let Some(Value::String(text)) = map.get(*key)
                        && !text.trim().is_empty()
                        && !output.contains(text)
                    {
                        output.push(text.clone());
                    }
                }
                for value in map.values() {
                    visit(value, keys, limit, output);
                }
            }
            Value::Array(values) => {
                for value in values {
                    visit(value, keys, limit, output);
                }
            }
            _ => {}
        }
    }
    let mut output = Vec::new();
    visit(value, keys, limit, &mut output);
    output
}

pub fn first_nested_url(value: Option<&Value>) -> Option<String> {
    value.and_then(|value| find_string(value, &["url", "src"]))
}

pub fn strip_html(value: &str) -> String {
    Regex::new(r"(?is)<[^>]+>")
        .map(|regex| decode_html(&regex.replace_all(value, " ")))
        .unwrap_or_else(|_| value.to_string())
}

pub fn decode_html(value: &str) -> String {
    value
        .replace("&amp;", "&")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_metadata_records_platform_for_download_referer() {
        let media = page_metadata(
            r#"<meta property="og:title" content="频道视频">"#,
            &Url::parse("https://pd.qq.com/s/1").unwrap(),
            Platform::QqChannel,
        );
        assert_eq!(media.title.as_deref(), Some("频道视频"));
        assert_eq!(media.platform, Platform::QqChannel);
    }

    #[test]
    fn html_title_falls_back_when_open_graph_missing() {
        let media = page_metadata(
            "<title> 标题 </title>",
            &Url::parse("https://pd.qq.com/s/1").unwrap(),
            Platform::QqChannel,
        );
        assert_eq!(media.title.as_deref(), Some("标题"));
    }
}
