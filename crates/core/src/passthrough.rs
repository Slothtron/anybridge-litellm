//! 透传相关的纯逻辑：`{tail}` 白名单与请求体 `model` 提取。

use crate::domain::ApiKind;
use crate::error::{ErrorKind, GatewayError};

/// 判断 `{tail}`（`/llmproxy/{provider}/v1/` 之后的部分，已去掉前导 `/`）
/// 是否在该 `api` 的白名单内。
///
/// 空 tail、空段（含 `//` 与首尾 `/`）、`.`/`..` 段、含 `?` 或 `#` 的 tail 一律拒绝。
pub fn allows(api: ApiKind, tail: &str) -> bool {
    if tail.is_empty() || tail.contains('?') || tail.contains('#') {
        return false;
    }
    let segments: Vec<&str> = tail.split('/').collect();
    if segments
        .iter()
        .any(|s| s.is_empty() || *s == "." || *s == "..")
    {
        return false;
    }

    // `models` 本身也允许（server 层对它走网关生成，不透传）。
    // 不提供 `models/{id}`：模型详情由 `/models` 列表承载。
    match api {
        ApiKind::Completions => matches!(segments.as_slice(), ["chat", "completions"] | ["models"]),
        ApiKind::Anthropic => matches!(
            segments.as_slice(),
            ["messages"] | ["messages", "count_tokens"] | ["models"]
        ),
        ApiKind::Responses => matches!(segments.as_slice(), ["responses"] | ["models"]),
    }
}

/// 从请求体解析 `model`。只读，不重新序列化。
pub fn extract_model(body: &[u8]) -> Result<String, GatewayError> {
    let value: serde_json::Value = serde_json::from_slice(body).map_err(|e| {
        GatewayError::new(
            ErrorKind::InvalidRequest,
            format!("请求体不是合法 JSON：{e}"),
        )
    })?;
    match value.get("model") {
        Some(serde_json::Value::String(model)) => Ok(model.clone()),
        Some(_) => Err(GatewayError::new(
            ErrorKind::InvalidRequest,
            "请求体的 model 字段必须是字符串",
        )),
        None => Err(GatewayError::new(
            ErrorKind::InvalidRequest,
            "请求体缺少 model 字段",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completions_whitelist() {
        for tail in ["chat/completions", "models"] {
            assert!(allows(ApiKind::Completions, tail), "{tail} 应被允许");
        }
        for tail in [
            "",
            "completions",
            "chat",
            "chat/completions/x",
            "chat/completions/",
            "messages",
            "responses",
            "models/",
            "models/gpt-4o",
            "models/a/b",
            "models/..",
            "..",
            "models/../x",
            "/chat/completions",
            "chat//completions",
            "chat/completions?x=1",
            "models#x",
        ] {
            assert!(!allows(ApiKind::Completions, tail), "{tail:?} 应被拒绝");
        }
    }

    #[test]
    fn anthropic_whitelist() {
        for tail in ["messages", "messages/count_tokens", "models"] {
            assert!(allows(ApiKind::Anthropic, tail), "{tail} 应被允许");
        }
        for tail in [
            "chat/completions",
            "responses",
            "messages/count_tokens/x",
            "messages/",
            "models/claude-x",
            "models/a/b",
        ] {
            assert!(!allows(ApiKind::Anthropic, tail), "{tail:?} 应被拒绝");
        }
    }

    #[test]
    fn responses_whitelist() {
        for tail in ["responses", "models"] {
            assert!(allows(ApiKind::Responses, tail), "{tail} 应被允许");
        }
        for tail in [
            "chat/completions",
            "messages",
            "responses/x",
            "models/",
            "models/gpt-5",
            "models/a/b",
        ] {
            assert!(!allows(ApiKind::Responses, tail), "{tail:?} 应被拒绝");
        }
    }

    #[test]
    fn extract_model_success() {
        assert_eq!(
            extract_model(br#"{"model":"gpt-4o","messages":[]}"#).unwrap(),
            "gpt-4o"
        );
        // 键序与空白不影响，只读 model
        assert_eq!(
            extract_model(b"{ \"messages\" : [], \"model\" : \"a b\" }").unwrap(),
            "a b"
        );
        // 空字符串是合法字符串，由查表阶段报 model_not_found
        assert_eq!(extract_model(br#"{"model":""}"#).unwrap(), "");
    }

    #[test]
    fn extract_model_missing() {
        let err = extract_model(br#"{"messages":[]}"#).unwrap_err();
        assert_eq!(err.kind, ErrorKind::InvalidRequest);
        assert_eq!(err.status(), 400);
        assert!(err.message.contains("model"), "{}", err.message);
    }

    #[test]
    fn extract_model_non_string() {
        for body in [br#"{"model":123}"#.as_slice(), br#"{"model":null}"#] {
            let err = extract_model(body).unwrap_err();
            assert_eq!(err.kind, ErrorKind::InvalidRequest);
            assert!(err.message.contains("字符串"), "{}", err.message);
        }
    }

    #[test]
    fn extract_model_invalid_json() {
        for body in [b"".as_slice(), b"not json", b"{\"model\":"] {
            let err = extract_model(body).unwrap_err();
            assert_eq!(err.kind, ErrorKind::InvalidRequest);
            assert_eq!(err.status(), 400);
        }
    }

    #[test]
    fn extract_model_non_object_json() {
        let err = extract_model(br#"["model"]"#).unwrap_err();
        assert_eq!(err.kind, ErrorKind::InvalidRequest);
    }
}
