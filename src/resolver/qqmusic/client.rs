use std::time::Duration;

use anyhow::{Context, Result, bail, ensure};
use reqwest::{Client, RequestBuilder, header};
use serde_json::{Value, json};
use url::Url;

use super::{
    content::Playback,
    mapping::playback_from_response,
    urls::{SongId, song_id, valid_mid},
};
use crate::resolver::{
    Platform,
    url_policy::{resolve_redirect, validate_platform_url},
};

const ENDPOINT: &str = "https://u.y.qq.com/cgi-bin/musicu.fcg";
const MAX_API_BYTES: usize = 2 * 1024 * 1024;
const TIMEOUT: Duration = Duration::from_secs(15);
pub(super) const USER_AGENT: &str =
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 Safari/537.36";

pub(super) struct QqMusicClient {
    http: Client,
    cookie: String,
}

impl QqMusicClient {
    pub(super) fn new(http: Client, cookie: String) -> Self {
        Self { http, cookie }
    }

    pub(super) fn logged_in(&self) -> bool {
        cookie_value(&self.cookie, "qqmusic_key").is_some()
            && cookie_value(&self.cookie, "qqmusic_uin")
                .or_else(|| cookie_value(&self.cookie, "uin"))
                .is_some_and(|id| id.parse::<u64>().is_ok_and(|id| id > 0))
    }

    pub(super) async fn expand(&self, source: &Url) -> Result<Url> {
        let mut current = source.clone();
        for redirects in 0..=5 {
            validate_platform_url(&current, Platform::QqMusic)?;
            if song_id(&current).is_some() {
                return Ok(current);
            }
            // 短链展开不发送音乐账号凭据。
            let response = self
                .http
                .get(current.clone())
                .timeout(TIMEOUT)
                .header(header::USER_AGENT, USER_AGENT)
                .send()
                .await
                .map_err(|_| anyhow::anyhow!("QQ音乐短链请求失败"))?;
            if response.status().is_redirection() {
                ensure!(redirects < 5, "QQ音乐短链重定向过多");
                let location = response
                    .headers()
                    .get(header::LOCATION)
                    .and_then(|h| h.to_str().ok())
                    .context("QQ音乐短链未返回地址")?;
                current = resolve_redirect(&current, location, Platform::QqMusic)?;
            } else {
                bail!("QQ音乐链接未指向可识别的歌曲");
            }
        }
        bail!("QQ音乐短链重定向过多")
    }

    pub(super) async fn detail(&self, id: &SongId) -> Result<Value> {
        let param = match id {
            SongId::Id(id) => json!({"song_id":id}),
            SongId::Mid(mid) => json!({"song_mid":mid}),
        };
        let data = self
            .rpc("music.pf_song_detail_svr", "get_song_detail_yqq", param)
            .await?;
        track_for_request(&data, id)
    }

    pub(super) async fn playback(&self, track: &Value) -> Result<Option<Playback>> {
        let mid = track
            .get("mid")
            .and_then(Value::as_str)
            .context("QQ音乐缺少歌曲 MID")?;
        let media_mid = track
            .get("file")
            .and_then(|f| f.get("media_mid"))
            .and_then(Value::as_str)
            .unwrap_or(mid);
        ensure!(
            valid_mid(mid) && valid_mid(media_mid),
            "QQ音乐歌曲 MID 无效"
        );
        let uin = self.uin();
        let filenames = [
            format!("M500{media_mid}.mp3"),
            format!("C400{media_mid}.m4a"),
        ];
        let song_type = track.get("type").and_then(Value::as_u64).unwrap_or(0);
        let data = self
            .rpc(
                "music.vkey.GetVkey",
                "UrlGetVkey",
                json!({
                    "uin":uin, "guid":(uuid::Uuid::new_v4().as_u128() % 10_000_000_000).to_string(),
                    "songmid":[mid,mid], "filename":filenames,
                    "songtype":[song_type,song_type],
                    "ctx":0, "loginflag":1, "platform":"20"
                }),
            )
            .await?;
        Ok(playback_from_response(&data, mid, media_mid))
    }

    fn uin(&self) -> &str {
        cookie_value(&self.cookie, "qqmusic_uin")
            .or_else(|| cookie_value(&self.cookie, "uin"))
            .filter(|id| id.parse::<u64>().is_ok_and(|id| id > 0))
            .unwrap_or("0")
    }

    async fn rpc(&self, module: &str, method: &str, param: Value) -> Result<Value> {
        let key = cookie_value(&self.cookie, "qqmusic_key").unwrap_or("");
        let mut comm = json!({"ct":24,"cv":0,"format":"json","uin":self.uin(),"g_tk":hash33(key,5381),"g_tk_new_20200303":hash33(key,5381)});
        if self.logged_in() {
            comm["authst"] = key.into();
            comm["qq"] = self.uin().into();
            comm["tmeLoginType"] = if key.starts_with("W_X") { 1 } else { 2 }.into();
        }
        let payload = json!({"comm":comm,"req":{"module":module,"method":method,"param":param}});
        let mut request = self
            .http
            .post(ENDPOINT)
            .timeout(TIMEOUT)
            .header(header::USER_AGENT, USER_AGENT)
            .header(header::REFERER, "https://y.qq.com/")
            .json(&payload);
        if self.logged_in() {
            request = request.header(header::COOKIE, &self.cookie);
        }
        let response: Value = serde_json::from_slice(&request_bytes(request).await?)
            .context("QQ音乐返回了无效数据")?;
        ensure!(
            response.get("code").and_then(Value::as_i64) == Some(0),
            "QQ音乐请求未成功"
        );
        let req = response.get("req").context("QQ音乐响应缺少结果")?;
        ensure!(
            req.get("code").and_then(Value::as_i64) == Some(0),
            "QQ音乐接口未成功"
        );
        req.get("data").cloned().context("QQ音乐响应缺少数据")
    }
}

fn track_for_request(data: &Value, requested: &SongId) -> Result<Value> {
    let track = data.get("track_info").context("QQ音乐未返回歌曲信息")?;
    let matches = match requested {
        SongId::Id(id) => *id > 0 && track.get("id").and_then(Value::as_u64) == Some(*id),
        SongId::Mid(mid) => {
            valid_mid(mid) && track.get("mid").and_then(Value::as_str) == Some(mid.as_str())
        }
    };
    ensure!(matches, "QQ音乐返回的歌曲与请求不一致");
    Ok(track.clone())
}

fn cookie_value<'a>(cookie: &'a str, name: &str) -> Option<&'a str> {
    cookie
        .split(';')
        .filter_map(|part| part.trim().split_once('='))
        .find(|(key, value)| *key == name && !value.is_empty())
        .map(|(_, value)| value)
}

fn hash33(value: &str, initial: u32) -> u32 {
    value.bytes().fold(initial, |hash, c| {
        hash.wrapping_mul(33).wrapping_add(u32::from(c))
    }) & 0x7fff_ffff
}

async fn request_bytes(request: RequestBuilder) -> Result<Vec<u8>> {
    let mut response = request
        .send()
        .await
        .map_err(|_| anyhow::anyhow!("QQ音乐请求失败"))?;
    ensure!(
        response.status().is_success(),
        "QQ音乐请求失败：HTTP {}",
        response.status().as_u16()
    );
    ensure!(
        response
            .content_length()
            .is_none_or(|len| len <= MAX_API_BYTES as u64),
        "QQ音乐响应过大"
    );
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| anyhow::anyhow!("QQ音乐响应读取失败"))?
    {
        ensure!(
            bytes.len().saturating_add(chunk.len()) <= MAX_API_BYTES,
            "QQ音乐响应过大"
        );
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binds_song_detail_to_requested_numeric_id_or_mid() {
        let data = json!({"track_info":{"id":607240405,"mid":"002d5NTY0FgLNi","name":"远光点"}});
        assert!(track_for_request(&data, &SongId::Id(607240405)).is_ok());
        assert!(track_for_request(&data, &SongId::Mid("002d5NTY0FgLNi".into())).is_ok());
        assert!(track_for_request(&data, &SongId::Id(575658805)).is_err());
        assert!(track_for_request(&data, &SongId::Mid("000SO0rc20bCpb".into())).is_err());
        assert!(
            track_for_request(
                &json!({"track_info":{"mid":"002d5NTY0FgLNi"}}),
                &SongId::Id(607240405)
            )
            .is_err()
        );
        assert!(
            track_for_request(
                &json!({"track_info":{"id":607240405}}),
                &SongId::Mid("002d5NTY0FgLNi".into())
            )
            .is_err()
        );
    }

    #[test]
    fn login_identity_requires_both_valid_account_and_music_key() {
        let http = Client::new();
        assert!(!QqMusicClient::new(http.clone(), "qqmusic_key=secret".into()).logged_in());
        assert!(
            !QqMusicClient::new(http.clone(), "qqmusic_uin=0; qqmusic_key=secret".into())
                .logged_in()
        );
        let client = QqMusicClient::new(http, "qqmusic_uin=12345; qqmusic_key=sample".into());
        assert!(client.logged_in());
        assert_eq!(client.uin(), "12345");
        assert_eq!(
            cookie_value("qqmusic_key=a=b; uin=7", "qqmusic_key"),
            Some("a=b")
        );
    }
}
