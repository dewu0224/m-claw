//! Feishu webhook signature verification.
//!
//! Verifies the `X-Lark-Signature` header against the request body
//! using HMAC-SHA256: `base64(hmac_sha256(timestamp + body, app_secret))`.

use base64::Engine;
use hmac::{Hmac, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

/// Verify a Feishu webhook signature.
///
/// # Parameters
/// - `timestamp`: value of the `X-Lark-Request-Timestamp` header.
/// - `signature`: value of the `X-Lark-Signature` header.
/// - `body`: raw request body bytes.
/// - `app_secret`: the application's app_secret.
///
/// # Returns
/// `true` if the signature matches.
pub fn verify_signature(
    timestamp: &str,
    signature: &str,
    body: &[u8],
    app_secret: &str,
) -> bool {
    let Ok(mut mac) = HmacSha256::new_from_slice(app_secret.as_bytes()) else {
        return false;
    };
    // The sign string is: timestamp + "\n" + body
    mac.update(timestamp.as_bytes());
    mac.update(b"\n");
    mac.update(body);

    let result = mac.finalize();
    let expected = base64::engine::general_purpose::STANDARD.encode(result.into_bytes());
    constant_time_eq(expected.as_bytes(), signature.as_bytes())
}

/// Constant-time comparison to avoid timing attacks.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_signature() {
        let secret = "my_app_secret";
        let timestamp = "1234567890";
        let body = b"hello world";

        // Compute the expected signature
        let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(timestamp.as_bytes());
        mac.update(b"\n");
        mac.update(body);
        let sig = base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes());

        assert!(verify_signature(timestamp, &sig, body, secret));
    }

    #[test]
    fn invalid_signature() {
        assert!(!verify_signature("123", "badsig", b"body", "secret"));
    }

    #[test]
    fn wrong_secret() {
        let timestamp = "999";
        let body = b"data";
        let mut mac = HmacSha256::new_from_slice(b"correct_secret").unwrap();
        mac.update(timestamp.as_bytes());
        mac.update(b"\n");
        mac.update(body);
        let sig = base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes());

        assert!(!verify_signature(timestamp, &sig, body, "wrong_secret"));
    }

    #[test]
    fn empty_body() {
        let secret = "s";
        let timestamp = "0";
        let body = b"";
        let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(timestamp.as_bytes());
        mac.update(b"\n");
        mac.update(body);
        let sig = base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes());

        assert!(verify_signature(timestamp, &sig, body, secret));
    }
}
