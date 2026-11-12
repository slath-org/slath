// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Webhook signatures: `sha256=<hex HMAC-SHA256(secret, timestamp + "." +
//! body)>`, sent as `X-Slath-Signature-256`.

use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;

/// The `X-Slath-Signature-256` value for a delivery body sent at unix time
/// `timestamp`.
#[must_use]
pub fn signature(secret: &[u8], timestamp: i64, body: &[u8]) -> String {
    use std::fmt::Write as _;
    let Ok(mut mac) = Hmac::<Sha256>::new_from_slice(secret) else {
        unreachable!("HMAC takes keys of any length");
    };
    mac.update(timestamp.to_string().as_bytes());
    mac.update(b".");
    mac.update(body);
    let tag = mac.finalize().into_bytes();
    tag.iter().fold(String::from("sha256="), |mut o, b| {
        let _ = write!(o, "{b:02x}");
        o
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signature_covers_timestamp_and_body() {
        // printf '1700000000.{"a":1}' | openssl dgst -sha256 -hmac s3cret
        assert_eq!(
            signature(b"s3cret", 1_700_000_000, br#"{"a":1}"#),
            "sha256=1698a50bc74d1ff1db85c4e0a5297c2ad9fdba245d5737cdb789e4cc6e098940"
        );
        assert_ne!(
            signature(b"s3cret", 1_700_000_001, br#"{"a":1}"#),
            signature(b"s3cret", 1_700_000_000, br#"{"a":1}"#)
        );
    }
}
