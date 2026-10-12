use super::{QqMusicQrClient, model::*};
use crate::auth::music::{QrImage, QrPoll, QrProtocol};
use serde_json::json;
use std::time::Duration;

#[test]
fn parses_only_data_callbacks_without_executing_scripts() {
    let args = callback_args("ptuiCB('66','0','','0','等待扫描','');").unwrap();
    assert_eq!(args[0], "66");
    assert_eq!(
        callback_args("ptuiCB('66','0','','0','二维码未失效。', '')").unwrap()[0],
        "66"
    );
    assert!(callback_args("ptuiCB('66''0')").is_err());
    assert!(callback_args("ptuiCB('66'); alert('x')").is_err());
    assert!(callback_args("evil('66');").is_err());
    assert!(callback_args("ptuiCB('66'+evil());").is_err());
    assert_eq!(
        callback_args(r"ptuiCB('0','0','https://a/?x=1\x26y=2','0','it\'s fine','');").unwrap()[4],
        "it's fine"
    );
}

#[test]
fn callback_rebuild_allows_only_fixed_https_login_endpoint() {
    let url =
        "https://ssl.ptlogin2.graph.qq.com/check_sig?uin=12345&ptsigx=sample&service=ptqrlogin";
    assert_eq!(
        completion_params(url).unwrap(),
        ("12345".into(), "sample".into())
    );
    for bad in [
        url.replace("https://", "http://"),
        url.replace("ssl.ptlogin2.graph.qq.com", "evil.test"),
        format!("{url}&uin=54321"),
    ] {
        assert!(completion_params(&bad).is_err());
    }
    let callback = "https://y.qq.com/portal/wx_redirect.html?code=sample&state=nonce&login_type=1";
    assert_eq!(authorization_code(callback, "nonce").unwrap(), "sample");
    assert!(authorization_code(callback, "other").is_err());
    assert!(authorization_code(&callback.replace("y.qq.com", "evil.test"), "nonce").is_err());
}

#[test]
fn login_requires_explicit_success_and_music_credentials() {
    let data = json!({"code":0,"req":{"code":0,"data":{"musicid":12345,"musickey":"sample_token","keyExpiresIn":86400,"musickeyCreateTime":1_800_000_000}}});
    let credentials = credentials_from_response(&serde_json::to_vec(&data).unwrap()).unwrap();
    assert_eq!(credentials.user_id(), "12345");
    assert_eq!(credentials.expires_at(), Some(1_800_086_400));
    assert!(
        credentials
            .cookie_header()
            .contains("qqmusic_key=sample_token")
    );
    let mut denied = data.clone();
    denied["req"]["code"] = 20279.into();
    assert!(credentials_from_response(&serde_json::to_vec(&denied).unwrap()).is_err());
    denied = data;
    denied["req"]["data"]["musickey"] = "unsafe;key".into();
    assert!(credentials_from_response(&serde_json::to_vec(&denied).unwrap()).is_err());
    assert!(
        single_cookie(
            &[("qrsig".into(), "a".into()), ("qrsig".into(), "b".into())],
            "qrsig"
        )
        .is_err()
    );
}

#[tokio::test]
#[ignore = "匿名官方接口 smoke；只验证生成及未扫码状态，不登录账号"]
async fn qqmusic_anonymous_qr_generate_and_wait() {
    let client = QqMusicQrClient::new(None).unwrap();
    let qr = client.generate().await.unwrap();
    assert!(matches!(qr.image(), QrImage::Bytes(_)));
    assert!(qr.expires_in() > Duration::from_secs(150));
    assert!(matches!(
        client.poll(&qr).await.unwrap(),
        QrPoll::WaitingForScan
    ));
}
