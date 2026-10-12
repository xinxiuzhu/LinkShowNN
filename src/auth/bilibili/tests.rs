use super::{
    BilibiliQrClient,
    client::{parse_generate, parse_poll},
    model::{Credentials, LoginError, MAX_RESPONSE_BYTES, PollOutcome},
};
use serde_json::json;

const FIXTURE_KEY: &str = "fixture-qr-key-0123456789";

fn generate_bytes(url: &str) -> Vec<u8> {
    serde_json::to_vec(&json!({"code":0,"data":{"url":url,"qrcode_key":FIXTURE_KEY}})).unwrap()
}

fn qr() -> super::QrCode {
    parse_generate(&generate_bytes(&format!(
        "https://passport.bilibili.com/qrcode/h5/login?oauthKey={FIXTURE_KEY}"
    )))
    .unwrap()
}

fn poll_bytes(code: i32) -> Vec<u8> {
    serde_json::to_vec(&json!({"code":0,"data":{
        "url":"", "refresh_token":"fixture-refresh-not-a-real-token", "timestamp":1,
        "code":code, "message":"fixture status"
    }}))
    .unwrap()
}

fn cookies() -> Vec<(String, String)> {
    [
        ("SESSDATA", "fixture-session%2Conly"),
        ("bili_jct", "fixture-csrf-only"),
        ("DedeUserID", "100001"),
        ("DedeUserID__ckMd5", "fixture-md5-only"),
        ("buvid3", "fixture-device-only"),
        ("unneeded_tracking", "discard-this-cookie"),
    ]
    .into_iter()
    .map(|(name, value)| (name.to_owned(), value.to_owned()))
    .collect()
}

#[test]
fn generate_accepts_only_matching_official_qr_links() {
    let old = qr();
    assert!(
        old.url()
            .starts_with("https://passport.bilibili.com/qrcode/h5/login")
    );
    assert!(old.expires_in().as_secs() <= 180);
    assert!(old.expires_in().as_secs() > 170);
    let modern = format!(
        "https://passport.bilibili.com/h5-app/passport/login/scan?navhide=1&qrcode_key={FIXTURE_KEY}&from=main_web"
    );
    assert!(parse_generate(&generate_bytes(&modern)).is_ok());
    let current_account_url = format!(
        "https://account.bilibili.com/h5/account-h5/auth/scan-web?navhide=1&callback=https%3A%2F%2Fwww.bilibili.com%2F&qrcode_key={FIXTURE_KEY}"
    );
    assert!(parse_generate(&generate_bytes(&current_account_url)).is_ok());
    for invalid in [
        format!("http://passport.bilibili.com/qrcode/h5/login?oauthKey={FIXTURE_KEY}"),
        format!(
            "https://passport.bilibili.com.evil.example/qrcode/h5/login?oauthKey={FIXTURE_KEY}"
        ),
        format!(
            "https://evil.example@passport.bilibili.com/qrcode/h5/login?oauthKey={FIXTURE_KEY}"
        ),
        format!("https://passport.bilibili.com:8443/qrcode/h5/login?oauthKey={FIXTURE_KEY}"),
        format!("https://passport.bilibili.com/unrelated?oauthKey={FIXTURE_KEY}"),
        "https://passport.bilibili.com/qrcode/h5/login?oauthKey=different-fixture-key".to_owned(),
        format!(
            "https://passport.bilibili.com/qrcode/h5/login?oauthKey={FIXTURE_KEY}&oauthKey={FIXTURE_KEY}"
        ),
        format!("https://passport.bilibili.com/qrcode/h5/login?oauthKey={FIXTURE_KEY}#fragment"),
        format!("https://account.bilibili.com/qrcode/h5/login?oauthKey={FIXTURE_KEY}"),
        format!("https://account.bilibili.com/h5-app/passport/login/scan?qrcode_key={FIXTURE_KEY}"),
        format!(
            "https://passport.bilibili.com/h5/account-h5/auth/scan-web?qrcode_key={FIXTURE_KEY}"
        ),
        format!("https://www.bilibili.com/h5/account-h5/auth/scan-web?qrcode_key={FIXTURE_KEY}"),
        format!(
            "https://account.bilibili.com.evil.example/h5/account-h5/auth/scan-web?qrcode_key={FIXTURE_KEY}"
        ),
    ] {
        assert_eq!(
            parse_generate(&generate_bytes(&invalid)).unwrap_err(),
            LoginError::InvalidQrCode
        );
    }
}

#[test]
fn generated_requests_are_anonymous_and_use_fixed_official_endpoints() {
    let client = BilibiliQrClient::new(None).unwrap();
    let generate = client.generate_request().build().unwrap();
    let poll = client.poll_request(&qr()).build().unwrap();
    for request in [&generate, &poll] {
        assert_eq!(request.url().scheme(), "https");
        assert_eq!(request.url().host_str(), Some("passport.bilibili.com"));
        assert!(!request.headers().contains_key(reqwest_012::header::COOKIE));
        assert!(
            !request
                .headers()
                .contains_key(reqwest_012::header::AUTHORIZATION)
        );
    }
    assert_eq!(
        generate.url().path(),
        "/x/passport-login/web/qrcode/generate"
    );
    assert!(generate.url().query().is_none());
    assert_eq!(poll.url().path(), "/x/passport-login/web/qrcode/poll");
    assert_eq!(
        poll.url().query_pairs().collect::<Vec<_>>(),
        vec![("qrcode_key".into(), FIXTURE_KEY.into())]
    );
}

#[test]
fn polling_distinguishes_every_login_step() {
    for (code, expected) in [
        (86101, PollOutcome::WaitingForScan),
        (86090, PollOutcome::WaitingForConfirmation),
        (86038, PollOutcome::Expired),
    ] {
        assert_eq!(parse_poll(&poll_bytes(code), Vec::new()).unwrap(), expected);
    }
    assert_eq!(
        parse_poll(&poll_bytes(99999), Vec::new()).unwrap_err(),
        LoginError::Api(99999)
    );
}

#[test]
fn success_uses_only_required_response_header_cookies() {
    let PollOutcome::Success(credentials) = parse_poll(&poll_bytes(0), cookies()).unwrap() else {
        panic!("expected success")
    };
    let header = credentials.cookie_header();
    assert!(header.contains("SESSDATA=fixture-session%2Conly"));
    assert!(header.contains("bili_jct=fixture-csrf-only"));
    assert!(header.contains("DedeUserID=100001"));
    assert!(!header.contains("unneeded_tracking"));
    assert_eq!(credentials.user_id(), "100001");
    assert_eq!(
        credentials.refresh_token(),
        "fixture-refresh-not-a-real-token"
    );
    assert_eq!(
        Credentials::decode(&credentials.encode().unwrap()).unwrap(),
        credentials
    );
    assert!(credentials.encode().unwrap().len() < 16 * 1024);
}

#[test]
fn successful_payload_cannot_inject_cookies_or_login_redirects() {
    let bytes = serde_json::to_vec(&json!({"code":0,"data":{
        "url":"https://evil.example/?SESSDATA=fixture-secret", "refresh_token":"", "timestamp":1,
        "code":0, "message":"OK", "cookies":cookies()
    }}))
    .unwrap();
    assert_eq!(
        parse_poll(&bytes, Vec::new()).unwrap_err(),
        LoginError::InvalidCredentials
    );
    let mut partial = cookies();
    partial.retain(|(name, _)| name != "SESSDATA");
    assert_eq!(
        parse_poll(&poll_bytes(0), partial).unwrap_err(),
        LoginError::InvalidCredentials
    );
}

#[test]
fn credentials_reject_duplicate_cookies_header_injection_and_invalid_user_ids() {
    let mut duplicate = cookies();
    duplicate.push(("SESSDATA".to_owned(), "another-fixture-session".to_owned()));
    assert_eq!(
        Credentials::from_pairs(duplicate, String::new()).unwrap_err(),
        LoginError::InvalidCredentials
    );
    for invalid in [
        "",
        "bad;new=value",
        "bad\r\nHeader:value",
        "bad value",
        "bad,split",
        "bad\\quoted",
    ] {
        let mut pairs = cookies();
        pairs[0].1 = invalid.to_owned();
        assert_eq!(
            Credentials::from_pairs(pairs, String::new()).unwrap_err(),
            LoginError::InvalidCredentials
        );
    }
    for invalid in ["0", "not-a-user", "18446744073709551616"] {
        let mut pairs = cookies();
        pairs[2].1 = invalid.to_owned();
        assert_eq!(
            Credentials::from_pairs(pairs, String::new()).unwrap_err(),
            LoginError::InvalidCredentials
        );
    }
    let mut pairs = cookies();
    pairs[0].1 = "x".repeat(4097);
    assert_eq!(
        Credentials::from_pairs(pairs, String::new()).unwrap_err(),
        LoginError::InvalidCredentials
    );
    assert_eq!(
        Credentials::from_pairs(cookies(), "x".repeat(4097)).unwrap_err(),
        LoginError::InvalidCredentials
    );
}

#[test]
fn errors_and_debug_output_do_not_expose_protocol_secrets() {
    let credentials =
        Credentials::from_pairs(cookies(), "fixture-refresh-not-a-real-token".to_owned()).unwrap();
    let debug = format!("{:?} {:?}", qr(), PollOutcome::Success(credentials));
    for secret in [
        FIXTURE_KEY,
        "fixture-session%2Conly",
        "fixture-csrf-only",
        "fixture-refresh-not-a-real-token",
        "100001",
    ] {
        assert!(!debug.contains(secret));
    }
    let failure = br#"{"code":-400,"message":"private-key-and-cookie","data":null}"#;
    let error = parse_generate(failure).unwrap_err();
    assert_eq!(error, LoginError::Api(-400));
    assert!(!format!("{error} {error:?}").contains("private-key-and-cookie"));
}

#[test]
fn protocol_payloads_require_explicit_success_and_bounded_valid_json() {
    for bytes in [
        b"not json".as_slice(),
        b"{}".as_slice(),
        br#"{"code":0}"#.as_slice(),
        br#"{"code":"0","data":{}}"#.as_slice(),
    ] {
        assert_eq!(
            parse_generate(bytes).unwrap_err(),
            LoginError::InvalidResponse
        );
    }
    assert_eq!(
        parse_generate(&vec![b' '; MAX_RESPONSE_BYTES + 1]).unwrap_err(),
        LoginError::ResponseTooLarge
    );
    assert_eq!(
        parse_poll(br#"{"code":-101,"data":{}}"#, cookies()).unwrap_err(),
        LoginError::Api(-101)
    );
}

#[test]
fn stored_credentials_require_version_validity_and_size_bounds() {
    let credentials = Credentials::from_pairs(cookies(), String::new()).unwrap();
    let mut value: serde_json::Value =
        serde_json::from_str(&credentials.encode().unwrap()).unwrap();
    value["version"] = 2.into();
    assert_eq!(
        Credentials::decode(&value.to_string()).unwrap_err(),
        LoginError::InvalidCredentials
    );
    assert_eq!(
        Credentials::decode(&"x".repeat(16 * 1024 + 1)).unwrap_err(),
        LoginError::InvalidCredentials
    );
    assert_eq!(
        Credentials::decode("not json fixture-secret").unwrap_err(),
        LoginError::InvalidCredentials
    );
}

async fn local_http_response(
    response_bytes: Vec<u8>,
) -> Result<(Vec<u8>, Vec<(String, String)>), LoginError> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = [0u8; 4096];
        let _ = socket.read(&mut request).await;
        let _ = socket.write_all(&response_bytes).await;
    });
    let client = reqwest_012::Client::builder()
        .no_proxy()
        .redirect(reqwest_012::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(2))
        .build()
        .unwrap();
    let result =
        super::client::request_bytes(client.get(format!("http://{address}/fixture"))).await;
    server.await.unwrap();
    result
}

#[tokio::test]
async fn bounded_http_reader_preserves_set_cookies_without_following_redirects() {
    let body = poll_bytes(0);
    let mut response = format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nSet-Cookie: SESSDATA=fixture-session%2Conly; Path=/; HttpOnly\r\nSet-Cookie: bili_jct=fixture-csrf-only; Path=/\r\nSet-Cookie: DedeUserID=100001; Path=/\r\nConnection: close\r\n\r\n",
        body.len()
    ).into_bytes();
    response.extend_from_slice(&body);
    let (bytes, pairs) = local_http_response(response).await.unwrap();
    let PollOutcome::Success(credentials) = parse_poll(&bytes, pairs).unwrap() else {
        panic!("expected fixture login success")
    };
    assert_eq!(credentials.user_id(), "100001");
    assert!(
        credentials
            .cookie_header()
            .contains("fixture-session%2Conly")
    );
    let redirect = b"HTTP/1.1 302 Found\r\nLocation: https://evil.example/?private-fixture-key\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec();
    assert_eq!(
        local_http_response(redirect).await.unwrap_err(),
        LoginError::HttpStatus(302)
    );
}

#[tokio::test]
async fn bounded_http_reader_rejects_large_declared_or_chunked_bodies() {
    let declared = format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        MAX_RESPONSE_BYTES + 1
    )
    .into_bytes();
    assert_eq!(
        local_http_response(declared).await.unwrap_err(),
        LoginError::ResponseTooLarge
    );
    let mut chunked = format!(
        "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n{:x}\r\n",
        MAX_RESPONSE_BYTES + 1
    )
    .into_bytes();
    chunked.extend(vec![b'x'; MAX_RESPONSE_BYTES + 1]);
    chunked.extend_from_slice(b"\r\n0\r\n\r\n");
    assert_eq!(
        local_http_response(chunked).await.unwrap_err(),
        LoginError::ResponseTooLarge
    );
}

#[tokio::test]
async fn bounded_http_reader_rejects_excessive_cookie_headers() {
    let headers = (0..33)
        .map(|index| format!("Set-Cookie: fixture{index}=value; Path=/\r\n"))
        .collect::<String>();
    let response =
        format!("HTTP/1.1 200 OK\r\n{headers}Content-Length: 0\r\nConnection: close\r\n\r\n")
            .into_bytes();
    assert_eq!(
        local_http_response(response).await.unwrap_err(),
        LoginError::ResponseTooLarge
    );
}

/// Opt in explicitly; never scans the challenge, sends a chat message or persists an account.
#[tokio::test]
#[ignore = "explicit anonymous Bilibili network smoke; no account login or credential persistence"]
async fn live_anonymous_qr_login_smoke() {
    let proxy = std::env::var("LINKSHOWNN_QR_TEST_PROXY").ok();
    let client = BilibiliQrClient::new(proxy.as_deref())
        .unwrap_or_else(|_| panic!("anonymous QR client setup failed"));
    let qr = client
        .generate()
        .await
        .unwrap_or_else(|error| panic!("anonymous QR generation failed: {error}"));
    let png = crate::auth::qr_image::render(qr.url())
        .unwrap_or_else(|_| panic!("anonymous QR image rendering failed"));
    assert!(
        png.starts_with(b"\x89PNG\r\n\x1a\n"),
        "anonymous QR image is invalid"
    );
    let outcome = client
        .poll(&qr)
        .await
        .unwrap_or_else(|error| panic!("anonymous QR polling failed: {error}"));
    assert!(
        matches!(outcome, PollOutcome::WaitingForScan),
        "anonymous QR initial status is unexpected"
    );
}
