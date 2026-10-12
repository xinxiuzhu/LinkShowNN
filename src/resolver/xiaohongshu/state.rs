//! 小红书静态状态提取与请求笔记身份校验。

use std::sync::OnceLock;

use anyhow::{Context, Result, ensure};
use regex::Regex;
use serde_json::Value;
use url::Url;

pub(super) fn note_id(url: &Url) -> Option<String> {
    let segments = url.path().trim_matches('/').split('/').collect::<Vec<_>>();
    let id = match segments.as_slice() {
        ["explore", id] | ["discovery", "item", id] => id,
        _ => return None,
    };
    canonical_id(id)
}

fn canonical_id(value: &str) -> Option<String> {
    (value.len() == 24 && value.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .then(|| value.to_ascii_lowercase())
}

pub(super) fn parse_inline(html: &str) -> Result<Value> {
    static SCRIPT: OnceLock<Regex> = OnceLock::new();
    static ASSIGNMENT: OnceLock<Regex> = OnceLock::new();
    let scripts = SCRIPT.get_or_init(|| {
        Regex::new(r"(?is)<script\b[^>]*>(.*?)</script\s*>").expect("static script regex")
    });
    let assignment = ASSIGNMENT.get_or_init(|| {
        Regex::new(r"window\s*\.\s*__INITIAL_STATE__\s*=").expect("static assignment regex")
    });
    for script in scripts.captures_iter(html) {
        let body = script.get(1).context("小红书状态脚本为空")?.as_str();
        let Some(found) = assignment.find(body) else {
            continue;
        };
        let normalized = replace_bare_undefined(&body[found.end()..]);
        let value = serde_json::Deserializer::from_str(&normalized)
            .into_iter::<Value>()
            .next()
            .context("小红书笔记状态为空")?
            .context("小红书笔记状态不是可识别的 JSON 数据")?;
        ensure!(value.is_object(), "小红书笔记状态不是对象");
        return Ok(value);
    }
    anyhow::bail!("小红书分享页未返回笔记状态")
}

/// 只处理字符串之外的 JS undefined，不改写正文、URL 或转义引号。
fn replace_bare_undefined(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let mut quote = None;
    let mut escaped = false;
    let mut offset = 0;
    while offset < bytes.len() {
        let byte = bytes[offset];
        if let Some(current_quote) = quote {
            output.push(byte);
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == current_quote {
                quote = None;
            }
            offset += 1;
            continue;
        }
        if matches!(byte, b'"' | b'\'') {
            quote = Some(byte);
        } else if bytes[offset..].starts_with(b"undefined")
            && (offset == 0 || !identifier_byte(bytes[offset - 1]))
            && bytes
                .get(offset + 9)
                .is_none_or(|byte| !identifier_byte(*byte))
        {
            output.extend_from_slice(b"null");
            offset += 9;
            continue;
        }
        output.push(byte);
        offset += 1;
    }
    String::from_utf8(output).expect("only ASCII tokens in valid UTF-8 were changed")
}

fn identifier_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'$') || !byte.is_ascii()
}

pub(super) fn requested_note<'a>(state: &'a Value, expected: &str) -> Result<&'a Value> {
    if let Some(first) = state.pointer("/note/firstNoteId").and_then(Value::as_str) {
        ensure!(
            canonical_id(first).as_deref() == Some(expected),
            "小红书主笔记 ID 与请求不匹配"
        );
    }
    if let Some(entry) = state
        .pointer("/note/noteDetailMap")
        .and_then(|map| map.get(expected))
    {
        let note = entry
            .get("note")
            .filter(|note| note.is_object())
            .context("小红书状态中没有请求笔记的详情")?;
        validate_identity(note, expected, true)?;
        return Ok(note);
    }
    for pointer in [
        "/noteData/data/noteData",
        "/noteData/note",
        "/note/note",
        "/note",
    ] {
        if let Some(note) = state.pointer(pointer).filter(|note| note.is_object())
            && validate_identity(note, expected, false).is_ok()
        {
            return Ok(note);
        }
    }
    anyhow::bail!("小红书状态未包含与请求 ID 匹配的笔记")
}

fn validate_identity(note: &Value, expected: &str, map_key_bound: bool) -> Result<()> {
    let mut has_id = false;
    for key in ["noteId", "note_id", "id"] {
        if let Some(value) = note.get(key).filter(|value| !value.is_null()) {
            let id = value
                .as_str()
                .and_then(canonical_id)
                .context("小红书笔记 ID 无效")?;
            ensure!(id == expected, "小红书笔记 ID 与请求不匹配");
            has_id = true;
        }
    }
    ensure!(has_id || map_key_bound, "小红书笔记缺少可校验的 ID");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOTE_ID: &str = "67fa70820000000009021c5b";
    const OTHER_ID: &str = "67fa70820000000009021c5c";

    #[test]
    fn permalink_ids_are_strict_and_queries_are_not_part_of_the_id() {
        for path in [
            format!("explore/{NOTE_ID}"),
            format!("discovery/item/{NOTE_ID}"),
        ] {
            let url = Url::parse(&format!(
                "https://www.xiaohongshu.com/{path}/?xsec_token=fixture"
            ))
            .unwrap();
            assert_eq!(note_id(&url).as_deref(), Some(NOTE_ID));
        }
        for path in [
            "explore/123",
            "user/profile/67fa70820000000009021c5b",
            "explore/67fa70820000000009021c5b/other",
        ] {
            assert!(
                note_id(&Url::parse(&format!("https://www.xiaohongshu.com/{path}")).unwrap())
                    .is_none()
            );
        }
    }

    #[test]
    fn replaces_bare_undefined_without_changing_text_or_escaped_quotes() {
        let html = r#"<script>window.__INITIAL_STATE__ = {"note":{"noteId":"67fa70820000000009021c5b","title":"undefined 保持原样","desc":"包含 \"undefined\" 和反斜线 \\undefined","extra":undefined,"items":[undefined,null]}}; window.after=true;</script>"#;
        let state = parse_inline(html).unwrap();
        assert_eq!(state["note"]["title"], "undefined 保持原样");
        assert_eq!(
            state["note"]["desc"],
            "包含 \"undefined\" 和反斜线 \\undefined"
        );
        assert!(state["note"]["extra"].is_null());
        assert_eq!(state["note"]["items"], serde_json::json!([null, null]));
    }

    #[test]
    fn javascript_expressions_are_not_executed_or_treated_as_state() {
        for html in [
            "<script>window.__INITIAL_STATE__ = fetch('/secret')</script>",
            "<script>window.__INITIAL_STATE__ = undefined</script>",
            "<script>window.__INITIAL_STATE__ = {\"note\":undefined_name}</script>",
        ] {
            assert!(parse_inline(html).is_err());
        }
    }

    #[test]
    fn selects_the_requested_pc_map_entry_instead_of_recommendations() {
        let state = serde_json::json!({"note": {
            "firstNoteId": NOTE_ID,
            "noteDetailMap": {
                (OTHER_ID): {"note": {"noteId": OTHER_ID, "title": "推荐内容", "imageList": [1]}},
                (NOTE_ID): {"note": {"noteId": NOTE_ID, "title": "请求内容", "imageList": [2]}}
            }
        }});
        assert_eq!(
            requested_note(&state, NOTE_ID).unwrap()["title"],
            "请求内容"
        );
        assert!(requested_note(&state, OTHER_ID).is_err());
    }

    #[test]
    fn pc_map_key_is_sufficient_only_when_the_inner_id_does_not_conflict() {
        let state = serde_json::json!({"note": {"noteDetailMap": {
            (NOTE_ID): {"note": {"title": "通过 map key 绑定"}}
        }}});
        assert!(requested_note(&state, NOTE_ID).is_ok());
        let conflicting = serde_json::json!({"note": {"noteDetailMap": {
            (NOTE_ID): {"note": {"noteId": OTHER_ID, "title": "错误笔记"}}
        }}});
        assert!(requested_note(&conflicting, NOTE_ID).is_err());
        assert!(requested_note(&state, OTHER_ID).is_err());
    }

    #[test]
    fn supports_bound_h5_and_older_explicit_wrappers() {
        let note = serde_json::json!({"noteId": NOTE_ID, "title": "笔记"});
        for state in [
            serde_json::json!({"noteData": {"data": {"noteData": note}}}),
            serde_json::json!({
                "note": {"noteDetailMap": {}},
                "noteData": {"data": {"noteData": note}}
            }),
            serde_json::json!({"noteData": {"note": note}}),
            serde_json::json!({"note": {"note": note}}),
            serde_json::json!({"note": note}),
        ] {
            assert_eq!(requested_note(&state, NOTE_ID).unwrap()["title"], "笔记");
            assert!(requested_note(&state, OTHER_ID).is_err());
        }
    }

    #[test]
    fn unbound_notes_and_unrelated_nested_recommendations_are_rejected() {
        for state in [
            serde_json::json!({"note": {"title": "缺 ID", "imageList": [1]}}),
            serde_json::json!({"recommendations": {"note": {"noteId": NOTE_ID}}}),
            serde_json::json!({"note": {"noteId": NOTE_ID, "note_id": OTHER_ID}}),
        ] {
            assert!(requested_note(&state, NOTE_ID).is_err());
        }
    }
}
