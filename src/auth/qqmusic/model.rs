use crate::auth::music::{MusicCredentials, MusicPlatform};
use anyhow::{Result, anyhow, ensure};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    time::{SystemTime, UNIX_EPOCH},
};
use url::Url;

pub(super) fn unix_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}
pub(super) fn hash33(value: &str, initial: u32) -> u32 {
    value.bytes().fold(initial, |h, c| {
        h.wrapping_mul(33).wrapping_add(u32::from(c))
    }) & 0x7fff_ffff
}
pub(super) fn valid_token(token: &str, max: usize) -> bool {
    !token.is_empty()
        && token.len() <= max
        && token
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-' | b'='))
}

pub(super) fn single_cookie(cookies: &[(String, String)], name: &str) -> Result<Option<String>> {
    let mut matches = cookies.iter().filter(|(key, _)| key == name);
    let value = matches.next().map(|(_, v)| v.clone());
    ensure!(matches.next().is_none(), "QQ音乐登录 Cookie 重复");
    Ok(value)
}

pub(super) fn oauth_cookies(pairs: &[(String, String)]) -> Result<BTreeMap<String, String>> {
    let mut cookies = BTreeMap::new();
    for name in ["p_skey", "p_uin", "skey", "uin", "pt4_token", "ptcz"] {
        if let Some(value) = single_cookie(pairs, name)? {
            ensure!(valid_token(&value, 4096), "QQ音乐授权 Cookie 无效");
            cookies.insert(name.to_owned(), value);
        }
    }
    Ok(cookies)
}

pub(super) fn callback_args(text: &str) -> Result<Vec<String>> {
    let text = text.trim();
    let body = text
        .strip_suffix(';')
        .unwrap_or(text)
        .strip_prefix("ptuiCB(")
        .and_then(|s| s.strip_suffix(')'))
        .ok_or_else(|| anyhow!("QQ音乐扫码响应无效"))?;
    // 参数只作为字符串解析，从不执行平台返回的 JavaScript。
    let regex = regex::Regex::new(r"'((?:\\.|[^'\\])*)'").expect("constant regex");
    let mut args = Vec::new();
    let mut end = 0;
    for captures in regex.captures_iter(body) {
        let full = captures.get(0).expect("match");
        ensure!(
            if args.is_empty() {
                body[end..full.start()].trim().is_empty()
            } else {
                body[end..full.start()].trim() == ","
            },
            "QQ音乐扫码响应无效"
        );
        let raw = captures.get(1).expect("argument").as_str();
        args.push(decode_js_string(raw)?);
        end = full.end();
    }
    ensure!(
        !args.is_empty() && args.len() <= 8 && body[end..].trim().is_empty(),
        "QQ音乐扫码响应无效"
    );
    Ok(args)
}

fn decode_js_string(raw: &str) -> Result<String> {
    let mut decoded = String::new();
    let mut chars = raw.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            decoded.push(c);
            continue;
        }
        let escape = chars.next().ok_or_else(|| anyhow!("QQ音乐扫码响应无效"))?;
        let c = match escape {
            '\\' | '\'' | '"' | '/' => escape,
            'x' | 'u' => {
                let mut code = 0u32;
                for _ in 0..if escape == 'x' { 2 } else { 4 } {
                    code = code * 16
                        + chars
                            .next()
                            .and_then(|c| c.to_digit(16))
                            .ok_or_else(|| anyhow!("QQ音乐扫码响应无效"))?;
                }
                char::from_u32(code).ok_or_else(|| anyhow!("QQ音乐扫码响应无效"))?
            }
            'n' => '\n',
            'r' => '\r',
            't' => '\t',
            _ => return Err(anyhow!("QQ音乐扫码响应无效")),
        };
        decoded.push(c);
    }
    Ok(decoded)
}

pub(super) fn completion_params(raw: &str) -> Result<(String, String)> {
    let url = Url::parse(raw).map_err(|_| anyhow!("QQ音乐登录回调无效"))?;
    ensure!(
        url.scheme() == "https"
            && url.host_str() == Some("ssl.ptlogin2.graph.qq.com")
            && url.path() == "/check_sig"
            && url.username().is_empty()
            && url.password().is_none()
            && url.port().is_none_or(|p| p == 443)
            && url.fragment().is_none(),
        "QQ音乐登录回调无效"
    );
    let params = url.query_pairs().collect::<Vec<_>>();
    let extract = |name: &str| -> Result<String> {
        let values = params
            .iter()
            .filter(|(key, _)| key == name)
            .collect::<Vec<_>>();
        ensure!(values.len() == 1, "QQ音乐登录回调字段无效");
        Ok(values[0].1.to_string())
    };
    let uin = extract("uin")?;
    let sigx = extract("ptsigx")?;
    ensure!(
        uin.len() <= 20 && uin.parse::<u64>().is_ok_and(|id| id > 0) && valid_token(&sigx, 1024),
        "QQ音乐登录回调字段无效"
    );
    Ok((uin, sigx))
}

pub(super) fn authorization_code(raw: &str, state: &str) -> Result<String> {
    let url = Url::parse(raw).map_err(|_| anyhow!("QQ音乐授权回调无效"))?;
    ensure!(
        url.scheme() == "https"
            && url.host_str() == Some("y.qq.com")
            && url.path() == "/portal/wx_redirect.html"
            && url.username().is_empty()
            && url.password().is_none()
            && url.port().is_none_or(|p| p == 443)
            && url.fragment().is_none(),
        "QQ音乐授权回调无效"
    );
    let params = url.query_pairs().collect::<Vec<_>>();
    let code = params
        .iter()
        .filter(|(key, _)| key == "code")
        .collect::<Vec<_>>();
    let returned_state = params
        .iter()
        .filter(|(key, _)| key == "state")
        .collect::<Vec<_>>();
    ensure!(
        code.len() == 1
            && valid_token(&code[0].1, 1024)
            && returned_state.len() == 1
            && returned_state[0].1 == state,
        "QQ音乐授权状态无效"
    );
    Ok(code[0].1.to_string())
}

pub(super) fn credentials_from_response(bytes: &[u8]) -> Result<MusicCredentials> {
    let response: Value =
        serde_json::from_slice(bytes).map_err(|_| anyhow!("QQ音乐登录响应无效"))?;
    ensure!(
        response.get("code").and_then(Value::as_i64) == Some(0),
        "QQ音乐登录未成功"
    );
    let req = response
        .get("req")
        .ok_or_else(|| anyhow!("QQ音乐登录响应无效"))?;
    ensure!(
        req.get("code").and_then(Value::as_i64) == Some(0),
        "QQ音乐登录未成功"
    );
    let data = req
        .get("data")
        .ok_or_else(|| anyhow!("QQ音乐登录响应无效"))?;
    if let Some(code) = data.get("code") {
        ensure!(code.as_i64() == Some(0), "QQ音乐账号授权未成功");
    }
    let musicid = data
        .get("musicid")
        .and_then(|v| v.as_u64().or_else(|| v.as_str()?.parse().ok()))
        .filter(|id| *id > 0)
        .ok_or_else(|| anyhow!("QQ音乐登录账号无效"))?;
    let key = data
        .get("musickey")
        .and_then(Value::as_str)
        .filter(|k| valid_token(k, 4096))
        .ok_or_else(|| anyhow!("QQ音乐登录凭据无效"))?;
    let expires_at = data
        .get("keyExpiresIn")
        .and_then(Value::as_u64)
        .filter(|ttl| *ttl > 0)
        .and_then(|ttl| {
            data.get("musickeyCreateTime")
                .and_then(Value::as_u64)
                .filter(|t| *t > 0)
                .unwrap_or_else(|| (unix_millis() / 1000) as u64)
                .checked_add(ttl)
        });
    MusicCredentials::new(
        MusicPlatform::QqMusic,
        format!(
            "qqmusic_uin={musicid}; uin={musicid}; qqmusic_key={key}; qm_keyst={key}; login_type=2"
        ),
        musicid.to_string(),
        expires_at,
    )
}
