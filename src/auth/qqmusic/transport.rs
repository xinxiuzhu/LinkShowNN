use anyhow::{Result, anyhow, ensure};
use reqwest::{RequestBuilder, header};
const MAX_BYTES: usize = 1024 * 1024;

pub(super) struct HttpResponse {
    pub(super) bytes: Vec<u8>,
    pub(super) cookies: Vec<(String, String)>,
    pub(super) location: Option<String>,
}

pub(super) async fn read_response(
    request: RequestBuilder,
    allow_redirect: bool,
) -> Result<HttpResponse> {
    let mut response = request
        .send()
        .await
        .map_err(|_| anyhow!("QQ音乐登录请求失败"))?;
    ensure!(
        response.status().is_success() || allow_redirect && response.status().is_redirection(),
        "QQ音乐登录返回 HTTP {}",
        response.status().as_u16()
    );
    ensure!(
        response
            .content_length()
            .is_none_or(|n| n <= MAX_BYTES as u64),
        "QQ音乐登录响应过大"
    );
    let cookies = response
        .headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .map(|h| h.to_str().map_err(|_| anyhow!("QQ音乐登录 Cookie 无效")))
        .collect::<Result<Vec<_>>>()?;
    ensure!(
        cookies.len() <= 32 && cookies.iter().map(|c| c.len()).sum::<usize>() <= 16 * 1024,
        "QQ音乐登录 Cookie 过大"
    );
    let cookies = cookies
        .into_iter()
        .filter_map(|c| {
            c.split(';')
                .next()?
                .split_once('=')
                .map(|(k, v)| (k.trim().to_owned(), v.to_owned()))
        })
        .collect();
    let location = response
        .headers()
        .get(header::LOCATION)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    ensure!(
        location.as_ref().is_none_or(|s| s.len() <= 4096),
        "QQ音乐登录跳转地址无效"
    );
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| anyhow!("QQ音乐登录响应读取失败"))?
    {
        ensure!(
            bytes.len().saturating_add(chunk.len()) <= MAX_BYTES,
            "QQ音乐登录响应过大"
        );
        bytes.extend_from_slice(&chunk);
    }
    Ok(HttpResponse {
        bytes,
        cookies,
        location,
    })
}
