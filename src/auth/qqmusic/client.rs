use std::time::Duration;

use anyhow::{Result, anyhow, ensure};
use reqwest::{Client, RequestBuilder, header};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::auth::music::{MusicCredentials, QrChallenge, QrImage, QrPoll, QrProtocol};

use super::{
    model::{
        authorization_code, callback_args, completion_params, credentials_from_response, hash33,
        oauth_cookies, single_cookie, unix_millis, valid_token,
    },
    transport::read_response,
};

const QR_ENDPOINT: &str = "https://ssl.ptlogin2.qq.com/ptqrshow";
const POLL_ENDPOINT: &str = "https://ssl.ptlogin2.qq.com/ptqrlogin";
const CHECK_ENDPOINT: &str = "https://ssl.ptlogin2.graph.qq.com/check_sig";
const AUTHORIZE_ENDPOINT: &str = "https://graph.qq.com/oauth2.0/authorize";
const MUSIC_ENDPOINT: &str = "https://u.y.qq.com/cgi-bin/musicu.fcg";
const MUSIC_REDIRECT: &str =
    "https://y.qq.com/portal/wx_redirect.html?login_type=1&surl=https://y.qq.com/";
const TIMEOUT: Duration = Duration::from_secs(12);
const USER_AGENT: &str =
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 Safari/537.36";

#[derive(Clone)]
pub(crate) struct QqMusicQrClient {
    http: Client,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ChallengeState {
    qrsig: String,
    oauth_state: String,
}

impl QqMusicQrClient {
    pub(crate) fn new(proxy: Option<&str>) -> Result<Self> {
        let mut builder = Client::builder()
            .no_proxy()
            .timeout(TIMEOUT)
            .connect_timeout(Duration::from_secs(5))
            .redirect(reqwest::redirect::Policy::none());
        if let Some(proxy) = proxy.filter(|p| !p.trim().is_empty()) {
            ensure!(proxy.len() <= 2048, "QQ音乐登录代理无效");
            builder = builder.proxy(
                reqwest::Proxy::all(proxy.trim()).map_err(|_| anyhow!("QQ音乐登录代理无效"))?,
            );
        }
        Ok(Self {
            http: builder
                .build()
                .map_err(|_| anyhow!("QQ音乐登录客户端初始化失败"))?,
        })
    }

    fn request(&self, url: &str) -> RequestBuilder {
        self.http
            .get(url)
            .header(header::USER_AGENT, USER_AGENT)
            .header(header::REFERER, "https://xui.ptlogin2.qq.com/")
    }

    async fn authorize(
        &self,
        completion: &str,
        state: &ChallengeState,
    ) -> Result<MusicCredentials> {
        let (uin, sigx) = completion_params(completion)?;
        // 不跟随回调中的 URL：提取有限字段后向固定官方端点重建请求。
        let check = self.request(CHECK_ENDPOINT).query(&[
            ("uin", uin.as_str()),
            ("pttype", "1"),
            ("service", "ptqrlogin"),
            ("nodirect", "0"),
            ("ptsigx", sigx.as_str()),
            ("s_url", "https://graph.qq.com/oauth2.0/login_jump"),
            ("ptlang", "2052"),
            ("ptredirect", "100"),
            ("aid", "716027609"),
            ("daid", "383"),
            ("j_later", "0"),
            ("low_login_hour", "0"),
            ("regmaster", "0"),
            ("pt_login_type", "3"),
            ("pt_aid", "0"),
            ("pt_aaid", "16"),
            ("pt_light", "0"),
            ("pt_3rd_aid", "100497308"),
        ]);
        let response = read_response(check, true).await?;
        let cookies = oauth_cookies(&response.cookies)?;
        let p_skey = cookies
            .get("p_skey")
            .ok_or_else(|| anyhow!("QQ音乐授权缺少登录信息"))?;
        let cookie_header = cookies
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join("; ");
        let form = [
            ("response_type", "code".to_owned()),
            ("client_id", "100497308".to_owned()),
            ("redirect_uri", MUSIC_REDIRECT.to_owned()),
            ("scope", "get_user_info,get_app_friends".to_owned()),
            ("state", state.oauth_state.clone()),
            ("switch", String::new()),
            ("from_ptlogin", "1".to_owned()),
            ("src", "1".to_owned()),
            ("update_auth", "1".to_owned()),
            ("openapi", "1010_1030".to_owned()),
            ("g_tk", hash33(p_skey, 5381).to_string()),
            ("auth_time", unix_millis().to_string()),
            ("ui", uuid::Uuid::new_v4().to_string()),
        ];
        let form = url::form_urlencoded::Serializer::new(String::new())
            .extend_pairs(&form)
            .finish();
        let response = read_response(
            self.http
                .post(AUTHORIZE_ENDPOINT)
                .header(header::USER_AGENT, USER_AGENT)
                .header(header::REFERER, "https://graph.qq.com/")
                .header(header::COOKIE, cookie_header)
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .body(form),
            true,
        )
        .await?;
        let code = authorization_code(
            response.location.as_deref().unwrap_or(""),
            &state.oauth_state,
        )?;
        let payload = json!({"comm":{"ct":24,"cv":0,"tmeLoginType":2,"format":"json"},
            "req":{"module":"QQConnectLogin.LoginServer","method":"QQLogin","param":{"code":code}}});
        let response = read_response(
            self.http
                .post(MUSIC_ENDPOINT)
                .header(header::USER_AGENT, USER_AGENT)
                .header(header::REFERER, "https://y.qq.com/")
                .json(&payload),
            false,
        )
        .await?;
        credentials_from_response(&response.bytes)
    }
}

#[async_trait::async_trait]
impl QrProtocol for QqMusicQrClient {
    async fn generate(&self) -> Result<QrChallenge> {
        let response = read_response(
            self.request(QR_ENDPOINT)
                .query(&[
                    ("appid", "716027609"),
                    ("e", "2"),
                    ("l", "M"),
                    ("s", "3"),
                    ("d", "72"),
                    ("v", "4"),
                    ("daid", "383"),
                    ("pt_3rd_aid", "100497308"),
                ])
                .query(&[("t", uuid::Uuid::new_v4().to_string())]),
            false,
        )
        .await?;
        let qrsig = single_cookie(&response.cookies, "qrsig")?
            .ok_or_else(|| anyhow!("QQ音乐二维码缺少状态"))?;
        ensure!(valid_token(&qrsig, 1024), "QQ音乐二维码状态无效");
        let state = serde_json::to_string(&ChallengeState {
            qrsig,
            oauth_state: uuid::Uuid::new_v4().to_string(),
        })
        .map_err(|_| anyhow!("QQ音乐二维码状态无效"))?;
        QrChallenge::new(
            QrImage::Bytes(response.bytes),
            state,
            Duration::from_secs(180),
        )
    }

    async fn poll(&self, qr: &QrChallenge) -> Result<QrPoll> {
        if qr.expires_in().is_zero() {
            return Ok(QrPoll::Expired);
        }
        let state: ChallengeState =
            serde_json::from_str(qr.state()).map_err(|_| anyhow!("QQ音乐二维码状态无效"))?;
        ensure!(
            valid_token(&state.qrsig, 1024) && valid_token(&state.oauth_state, 128),
            "QQ音乐二维码状态无效"
        );
        let response = read_response(
            self.request(POLL_ENDPOINT)
                .header(header::COOKIE, format!("qrsig={}", state.qrsig))
                .query(&[
                    ("u1", "https://graph.qq.com/oauth2.0/login_jump"),
                    ("ptredirect", "0"),
                    ("h", "1"),
                    ("t", "1"),
                    ("g", "1"),
                    ("from_ui", "1"),
                    ("ptlang", "2052"),
                    ("js_ver", "20102616"),
                    ("js_type", "1"),
                    ("pt_uistyle", "40"),
                    ("aid", "716027609"),
                    ("daid", "383"),
                    ("pt_3rd_aid", "100497308"),
                    ("has_onekey", "1"),
                ])
                .query(&[
                    ("ptqrtoken", hash33(&state.qrsig, 0).to_string()),
                    ("action", format!("0-0-{}", unix_millis())),
                ]),
            false,
        )
        .await?;
        let text =
            std::str::from_utf8(&response.bytes).map_err(|_| anyhow!("QQ音乐扫码响应无效"))?;
        let args = callback_args(text)?;
        match args.first().map(String::as_str) {
            Some("66") => Ok(QrPoll::WaitingForScan),
            Some("67") => Ok(QrPoll::WaitingForConfirmation),
            Some("65") => Ok(QrPoll::Expired),
            Some("68") => Ok(QrPoll::Refused),
            Some("0") => {
                let url = args
                    .get(2)
                    .ok_or_else(|| anyhow!("QQ音乐扫码授权响应无效"))?;
                self.authorize(url, &state).await.map(QrPoll::Success)
            }
            _ => Err(anyhow!("QQ音乐扫码未成功，请重新获取二维码")),
        }
    }
}
