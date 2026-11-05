//! 网关自己产生的错误。上游响应不走这里，原样透传。

use serde_json::{json, Value};

/// 九种稳定错误类别，对应设计文档 §8。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    AuthError,
    /// 客户端 API key 有效，但 scope 不含请求的 provider。
    ScopeDenied,
    ProviderNotFound,
    ProviderRequired,
    ApiMismatch,
    ModelNotFound,
    InvalidRequest,
    /// provider 引用的凭据还没写进凭据文件。
    CredentialNotConfigured,
    UpstreamError,
}

impl ErrorKind {
    /// 错误信封里的 `type`。
    pub fn r#type(&self) -> &'static str {
        match self {
            ErrorKind::AuthError => "auth_error",
            ErrorKind::ScopeDenied => "scope_denied",
            ErrorKind::ProviderNotFound => "provider_not_found",
            ErrorKind::ProviderRequired => "provider_required",
            ErrorKind::ApiMismatch => "api_mismatch",
            ErrorKind::ModelNotFound => "model_not_found",
            ErrorKind::InvalidRequest => "invalid_request",
            ErrorKind::CredentialNotConfigured => "credential_not_configured",
            ErrorKind::UpstreamError => "upstream_error",
        }
    }

    /// HTTP 状态码。
    pub fn status(&self) -> u16 {
        match self {
            ErrorKind::AuthError => 401,
            ErrorKind::ScopeDenied => 403,
            ErrorKind::ProviderNotFound
            | ErrorKind::ProviderRequired
            | ErrorKind::ApiMismatch
            | ErrorKind::ModelNotFound => 404,
            ErrorKind::InvalidRequest => 400,
            ErrorKind::CredentialNotConfigured => 502,
            ErrorKind::UpstreamError => 502,
        }
    }

    /// 稳定机器码，当前与 `type` 同形。
    pub fn code(&self) -> &'static str {
        self.r#type()
    }
}

#[derive(Debug, Clone)]
pub struct GatewayError {
    pub kind: ErrorKind,
    pub message: String,
    /// 仅 `upstream_error` 可能带上游状态码。
    pub upstream_status: Option<u16>,
}

impl GatewayError {
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            upstream_status: None,
        }
    }

    pub fn upstream(msg: impl Into<String>, status: Option<u16>) -> Self {
        Self {
            kind: ErrorKind::UpstreamError,
            message: msg.into(),
            upstream_status: status,
        }
    }

    pub fn status(&self) -> u16 {
        self.kind.status()
    }

    pub fn r#type(&self) -> &'static str {
        self.kind.r#type()
    }

    /// `{"error":{"message":..,"type":..,"code":..}}`
    pub fn to_json(&self) -> Value {
        json!({
            "error": {
                "message": self.message,
                "type": self.kind.r#type(),
                "code": self.kind.code(),
            }
        })
    }
}

impl std::fmt::Display for GatewayError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.kind.r#type(), self.message)
    }
}

impl std::error::Error for GatewayError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_maps_to_status_and_type() {
        let table = [
            (ErrorKind::AuthError, 401, "auth_error"),
            (ErrorKind::ScopeDenied, 403, "scope_denied"),
            (ErrorKind::ProviderNotFound, 404, "provider_not_found"),
            (ErrorKind::ProviderRequired, 404, "provider_required"),
            (ErrorKind::ApiMismatch, 404, "api_mismatch"),
            (ErrorKind::ModelNotFound, 404, "model_not_found"),
            (ErrorKind::InvalidRequest, 400, "invalid_request"),
            (
                ErrorKind::CredentialNotConfigured,
                502,
                "credential_not_configured",
            ),
            (ErrorKind::UpstreamError, 502, "upstream_error"),
        ];
        for (kind, status, name) in table {
            assert_eq!(kind.status(), status);
            assert_eq!(kind.r#type(), name);
            assert_eq!(kind.code(), name);
        }
    }

    #[test]
    fn envelope_shape() {
        let err = GatewayError::new(ErrorKind::ModelNotFound, "模型 `x` 不在列表中");
        let json = err.to_json();
        let error = json.get("error").unwrap().as_object().unwrap();
        let mut keys: Vec<_> = error.keys().cloned().collect();
        keys.sort();
        assert_eq!(keys, vec!["code", "message", "type"]);
        assert_eq!(error["type"], "model_not_found");
        assert_eq!(error["code"], "model_not_found");
        assert_eq!(error["message"], "模型 `x` 不在列表中");
        assert_eq!(json.as_object().unwrap().len(), 1);
    }

    #[test]
    fn upstream_helper_keeps_status() {
        let err = GatewayError::upstream("上游不可达", Some(503));
        assert_eq!(err.kind, ErrorKind::UpstreamError);
        assert_eq!(err.status(), 502);
        assert_eq!(err.upstream_status, Some(503));
        assert_eq!(err.to_string(), "upstream_error: 上游不可达");
    }
}
