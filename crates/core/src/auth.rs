//! master key 校验：常量时间比较，不依赖 http crate。

use subtle::ConstantTimeEq;

const BEARER_PREFIX: &str = "bearer ";

/// 客户端凭据比较。长度不等时也走完整比较路径（对短侧补零），避免提前返回。
pub fn verify_master_key(expected: &str, presented: Option<&str>) -> bool {
    let presented = presented.unwrap_or("");
    let expected = expected.as_bytes();
    let presented = presented.as_bytes();

    let max_len = expected.len().max(presented.len());
    let mut diff = 0u8;
    for i in 0..max_len {
        let a = expected.get(i).copied().unwrap_or(0);
        let b = presented.get(i).copied().unwrap_or(0);
        diff |= a ^ b;
    }

    let bytes_eq = diff.ct_eq(&0u8);
    let len_eq = (expected.len() as u64).ct_eq(&(presented.len() as u64));
    (bytes_eq & len_eq).into()
}

/// 从两种头里取客户端出示的凭据。
///
/// `Authorization` 的 `Bearer ` 前缀大小写不敏感；同时提供时优先 `Authorization`，
/// 但它不是 Bearer 形态时回退到 `x-api-key`。空白凭据视为未提供。
pub fn extract_presented<'a>(
    authorization: Option<&'a str>,
    x_api_key: Option<&'a str>,
) -> Option<&'a str> {
    if let Some(value) = authorization {
        let value = value.trim();
        if value.len() >= BEARER_PREFIX.len()
            && value[..BEARER_PREFIX.len()].eq_ignore_ascii_case(BEARER_PREFIX)
        {
            let token = value[BEARER_PREFIX.len()..].trim();
            if !token.is_empty() {
                return Some(token);
            }
        }
    }

    x_api_key.map(str::trim).filter(|token| !token.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verify_master_key_matches() {
        assert!(verify_master_key("secret", Some("secret")));
        assert!(!verify_master_key("secret", Some("secreT")));
        assert!(!verify_master_key("secret", Some("secre")));
        assert!(!verify_master_key("secret", Some("secrets")));
        assert!(!verify_master_key("secret", None));
        assert!(!verify_master_key("secret", Some("")));
        assert!(verify_master_key("", Some("")));
    }

    #[test]
    fn verify_master_key_handles_non_ascii() {
        assert!(verify_master_key("密钥", Some("密钥")));
        assert!(!verify_master_key("密钥", Some("密钥 ")));
    }

    #[test]
    fn extract_presented_bearer() {
        assert_eq!(extract_presented(Some("Bearer abc"), None), Some("abc"));
        assert_eq!(extract_presented(Some("bearer   abc  "), None), Some("abc"));
        assert_eq!(extract_presented(Some("BEARER abc"), None), Some("abc"));
    }

    #[test]
    fn extract_presented_prefers_authorization() {
        assert_eq!(
            extract_presented(Some("Bearer from-header"), Some("from-x-api-key")),
            Some("from-header")
        );
    }

    #[test]
    fn extract_presented_falls_back_to_x_api_key() {
        assert_eq!(extract_presented(None, Some(" key ")), Some("key"));
        assert_eq!(
            extract_presented(Some("Basic dXNlcjpwYXNz"), Some("key")),
            Some("key")
        );
        assert_eq!(extract_presented(Some("Bearer "), Some("key")), Some("key"));
        assert_eq!(extract_presented(None, Some("   ")), None);
        assert_eq!(extract_presented(None, None), None);
    }

    #[test]
    fn extract_presented_rejects_short_authorization() {
        assert_eq!(extract_presented(Some("Beare"), Some("key")), Some("key"));
        assert_eq!(extract_presented(Some(""), None), None);
    }
}
