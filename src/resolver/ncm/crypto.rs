//! 官方 eapi 请求封装；只加密请求参数，不解密或修改媒体。
//!
//! 协议参考维护者原始实现：
//! https://github.com/neteasecloudmusicapienhanced/api-enhanced/blob/main/util/crypto.js

use aes::Aes128;
use cipher::{BlockEncrypt, KeyInit};
use serde_json::Value;

pub(super) fn encode(path: &str, body: &Value) -> String {
    let text = body.to_string();
    let digest = format!(
        "{:x}",
        md5::compute(format!("nobody{path}use{text}md5forencrypt"))
    );
    let mut bytes = format!("{path}-36cd479b6b5-{text}-36cd479b6b5-{digest}").into_bytes();
    let padding = 16 - bytes.len() % 16;
    bytes.resize(bytes.len() + padding, padding as u8);
    let cipher = Aes128::new_from_slice(b"e82ckenh8dichen8").expect("fixed AES key length");
    for block in bytes.chunks_exact_mut(16) {
        cipher.encrypt_block(block.into());
    }
    bytes.iter().map(|byte| format!("{byte:02X}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eapi_matches_independent_openssl_protocol_vector() {
        // Generated independently with OpenSSL AES-128-ECB/PKCS7, not this encoder.
        assert_eq!(
            encode(
                "/api/song/enhance/player/url/v1",
                &serde_json::json!({"ids":"[109125]","level":"standard"})
            ),
            "FA90B329E9614F79E79598F37DC2EDB487F00D1BC4C9B24CD57E6C318B9073569338432CD7D98D1A3626E997A2C53121359F199609F3518813FB5A18BAFF68CEB83D00D24CECA2C01F229A64E4D80CBB8D6B45F0B448BA8EFFC0D4F07964D6EFFDA128BA8595CB5B662FA2494BC33D22E164B2889528090FA30FD0F72E5CE80F"
        );
    }
}
