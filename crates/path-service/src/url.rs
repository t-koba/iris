use std::collections::HashMap;

pub const DEFAULT_PCLOUD_API_BASE: &str = "https://api.pcloud.com";
const EUROPE_PCLOUD_API_BASE: &str = "https://eapi.pcloud.com";
const INVALID_PCLOUD_API_BASE: &str = "pCloud API base must be the HTTPS US or Europe API origin";

/// Accepts only pCloud's documented US and Europe HTTPS API origins.
pub fn canonical_pcloud_api_base(api_base: &str) -> Result<&'static str, &'static str> {
    match api_base.to_ascii_lowercase().as_str() {
        "https://api.pcloud.com" | "https://api.pcloud.com/" => Ok(DEFAULT_PCLOUD_API_BASE),
        "https://eapi.pcloud.com" | "https://eapi.pcloud.com/" => Ok(EUROPE_PCLOUD_API_BASE),
        _ => Err(INVALID_PCLOUD_API_BASE),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PCloudTokenResponse {
    pub access_token: String,
    pub account_id: String,
    pub api_base: String,
}

pub fn short_config_hash(parts: &[&str]) -> String {
    use sha2::{Digest, Sha256};

    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update(part.as_bytes());
        hasher.update([0]);
    }
    hasher
        .finalize()
        .iter()
        .take(8)
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

pub fn url_encode_component(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            b' ' => out.push_str("%20"),
            _ => out.push_str(&format!("%{:02X}", byte)),
        }
    }
    out
}

pub fn parse_pcloud_callback(callback_url: &str) -> Result<PCloudTokenResponse, String> {
    let fragment = callback_url
        .split_once('#')
        .map(|(_, fragment)| fragment)
        .or_else(|| callback_url.split_once('?').map(|(_, query)| query))
        .ok_or_else(|| "pCloud callback URL has no fragment or query".to_string())?;
    let params = parse_url_encoded_pairs(fragment);
    let access_token = params
        .get("access_token")
        .cloned()
        .ok_or_else(|| "pCloud callback has no access_token".to_string())?;
    let account_id = params
        .get("uid")
        .cloned()
        .or_else(|| params.get("userid").cloned())
        .ok_or_else(|| "pCloud callback has no uid".to_string())?;
    let hostname = params
        .get("hostname")
        .cloned()
        .unwrap_or_else(|| "api.pcloud.com".to_string());
    let api_base = if hostname.starts_with("http://") || hostname.starts_with("https://") {
        hostname
    } else {
        format!("https://{}", hostname.trim_end_matches('/'))
    };
    let api_base = canonical_pcloud_api_base(&api_base)?.to_string();

    Ok(PCloudTokenResponse {
        access_token,
        account_id,
        api_base,
    })
}

fn parse_url_encoded_pairs(encoded: &str) -> HashMap<String, String> {
    encoded
        .split('&')
        .filter_map(|pair| {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            if key.is_empty() {
                return None;
            }
            Some((percent_decode(key), percent_decode(value)))
        })
        .collect()
}

pub fn percent_decode(value: &str) -> String {
    let mut out = Vec::with_capacity(value.len());
    let bytes = value.as_bytes();
    let mut index = 0usize;
    while index < bytes.len() {
        match bytes[index] {
            b'%' if index + 2 < bytes.len() => {
                if let Ok(hex) = u8::from_str_radix(&value[index + 1..index + 3], 16) {
                    out.push(hex);
                    index += 3;
                    continue;
                }
                out.push(bytes[index]);
            }
            b'+' => out.push(b' '),
            byte => out.push(byte),
        }
        index += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

#[cfg(test)]
mod tests {
    use super::{
        canonical_pcloud_api_base, parse_pcloud_callback, percent_decode, short_config_hash,
        url_encode_component,
    };

    #[test]
    fn parse_pcloud_callback_fragment_extracts_token_uid_and_host() {
        let parsed = parse_pcloud_callback(
            "http://127.0.0.1/iris-pcloud#access_token=token123&uid=42&hostname=eapi.pcloud.com",
        )
        .expect("callback should parse");
        assert_eq!(parsed.access_token, "token123");
        assert_eq!(parsed.account_id, "42");
        assert_eq!(parsed.api_base, "https://eapi.pcloud.com");
    }

    #[test]
    fn pcloud_api_base_rejects_untrusted_origin_variants() {
        assert_eq!(
            canonical_pcloud_api_base("HTTPS://API.PCLOUD.COM/"),
            Ok("https://api.pcloud.com")
        );
        assert_eq!(
            canonical_pcloud_api_base("https://eapi.pcloud.com"),
            Ok("https://eapi.pcloud.com")
        );
        for untrusted in [
            "http://api.pcloud.com",
            "https://api.pcloud.com.evil.test",
            "https://user@api.pcloud.com",
            "https://api.pcloud.com:443",
            "https://api.pcloud.com/path",
            "https://api.pcloud.com?next=evil",
            "https://api.pcloud.com#fragment",
            "https://attacker.test",
        ] {
            assert!(
                canonical_pcloud_api_base(untrusted).is_err(),
                "accepted untrusted API base: {untrusted}"
            );
        }
    }

    #[test]
    fn parse_pcloud_callback_rejects_untrusted_api_origins() {
        let error = parse_pcloud_callback(
            "http://127.0.0.1/callback#access_token=synthetic&uid=1&hostname=https%3A%2F%2Fattacker.test",
        )
        .expect_err("untrusted callback origin must be rejected");

        assert_eq!(
            error,
            "pCloud API base must be the HTTPS US or Europe API origin"
        );
    }

    #[test]
    fn percent_decode_handles_hex_and_plus() {
        assert_eq!(percent_decode("hello%20world"), "hello world");
        assert_eq!(percent_decode("hello+world"), "hello world");
    }

    #[test]
    fn url_encode_component_keeps_safe_ascii() {
        assert_eq!(url_encode_component("alpha-_.~"), "alpha-_.~");
        assert_eq!(url_encode_component("a b"), "a%20b");
    }

    #[test]
    fn short_config_hash_is_stable_for_same_input() {
        let lhs = short_config_hash(&["a", "b", "c"]);
        let rhs = short_config_hash(&["a", "b", "c"]);
        assert_eq!(lhs, rhs);
        assert_eq!(lhs.len(), 16);
    }
}
