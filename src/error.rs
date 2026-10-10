use std::{fmt, time::Duration};

use serde::{Deserialize, de::DeserializeOwned};
use serde_json::Value;

pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Server-side API error preserving the original code, message, and HTTP status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiError {
    pub code: String,
    pub message: String,
    pub display_message: Option<String>,
    pub http_status: u16,
}

/// Known server error categories. Unrecognized codes remain available in [`ApiError`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ApiErrorKind {
    AccessTokenInvalid,
    RefreshTokenInvalid,
    SignatureInvalid,
    DeviceLimit,
    NotFound,
    AlreadyExists,
    InvalidRapidProof,
    BadRequest,
    FileTypeFolder,
    VideoPreviewNotFound,
    FeatureDisabled,
    InRecycleBin,
    ShareNotAllowed,
    Other,
}

impl ApiError {
    pub fn kind(&self) -> ApiErrorKind {
        match self.code.as_str() {
            "AccessTokenInvalid" | "AccessTokenExpired" => ApiErrorKind::AccessTokenInvalid,
            "InvalidParameter.RefreshToken" => ApiErrorKind::RefreshTokenInvalid,
            "DeviceSessionSignatureInvalid" => ApiErrorKind::SignatureInvalid,
            "UserDeviceOffline" => ApiErrorKind::DeviceLimit,
            "NotFound.File" | "NotFound.FileId" | "NotFound.View" => ApiErrorKind::NotFound,
            "AlreadyExist.File" => ApiErrorKind::AlreadyExists,
            "InvalidRapidProof" => ApiErrorKind::InvalidRapidProof,
            "BadRequest" => ApiErrorKind::BadRequest,
            "InvalidResource.FileTypeFolder" => ApiErrorKind::FileTypeFolder,
            "NotFound.VideoPreviewInfo" => ApiErrorKind::VideoPreviewNotFound,
            "FeatureTemporaryDisabled" => ApiErrorKind::FeatureDisabled,
            "ForbiddenFileInTheRecycleBin" => ApiErrorKind::InRecycleBin,
            "FileShareNotAllowed" => ApiErrorKind::ShareNotAllowed,
            _ => ApiErrorKind::Other,
        }
    }

    /// Prefer a nonempty `display_message`, falling back to `message`.
    pub fn user_message(&self) -> &str {
        match &self.display_message {
            Some(m) if !m.is_empty() => m,
            _ => &self.message,
        }
    }

    /// Treat a nonempty string `code` as an error unless it is a recognized success code.
    pub(crate) fn from_body(http_status: u16, body: &[u8]) -> Option<Self> {
        #[derive(Deserialize)]
        struct Raw {
            #[serde(default)]
            code: Value,
            #[serde(default)]
            message: Value,
            #[serde(default)]
            display_message: Value,
        }
        let raw: Raw = serde_json::from_slice(body).ok()?;
        Self::from_parts(http_status, &raw.code, &raw.message, &raw.display_message)
    }

    pub(crate) fn from_value(http_status: u16, body: &Value) -> Option<Self> {
        let get = |k: &str| body.get(k).cloned().unwrap_or(Value::Null);
        Self::from_parts(http_status, &get("code"), &get("message"), &get("display_message"))
    }

    fn from_parts(http_status: u16, code: &Value, message: &Value, display: &Value) -> Option<Self> {
        let code = code.as_str().filter(|c| !c.is_empty() && !is_success_code(c))?;
        let text = |v: &Value| match v {
            Value::String(s) => s.clone(),
            Value::Null => String::new(),
            other => other.to_string(),
        };
        Some(Self {
            code: code.to_owned(),
            message: text(message),
            display_message: display.as_str().map(str::to_owned),
            http_status,
        })
    }
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} (http {}): {}", self.code, self.http_status, self.user_message())
    }
}

impl std::error::Error for ApiError {}

#[derive(Debug)]
#[non_exhaustive]
pub enum Error {
    Network(reqwest::Error),
    Http {
        status: u16,
        body: String,
    },
    Api(ApiError),
    RateLimited {
        status: u16,
        retry_after: Option<Duration>,
    },
    Decode {
        source: serde_json::Error,
        body: String,
    },
    /// A decoded response lacks required success fields. `shape` contains only
    /// whitelisted field names/types and fixed notice classifications, not values.
    UnexpectedResponse {
        operation: &'static str,
        shape: String,
    },
    Io(std::io::Error),
    Crypto(String),
    InvalidInput(String),
    /// No usable refresh token is available in the token store.
    NotLoggedIn,
    /// Client-side lookup failed, for example while resolving a path.
    NotFound(String),
    /// The download URL points to a blocked resource.
    Blocked,
    Integrity {
        expected: String,
        actual: String,
    },
}

impl Error {
    pub fn api(&self) -> Option<&ApiError> {
        match self {
            Self::Api(e) => Some(e),
            _ => None,
        }
    }

    pub fn api_kind(&self) -> Option<ApiErrorKind> {
        self.api().map(ApiError::kind)
    }

    pub fn is_not_found(&self) -> bool {
        matches!(self, Self::NotFound(_)) || self.api_kind() == Some(ApiErrorKind::NotFound)
    }

    pub fn is_already_exists(&self) -> bool {
        self.api_kind() == Some(ApiErrorKind::AlreadyExists)
    }

    /// Whether login is required because the refresh token is missing or invalid.
    pub fn needs_relogin(&self) -> bool {
        matches!(self, Self::NotLoggedIn) || self.api_kind() == Some(ApiErrorKind::RefreshTokenInvalid)
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Network(e) => write!(f, "network: {e}"),
            Self::Http { status, body } => write!(f, "http {status}: {body}"),
            Self::Api(e) => write!(f, "api {e}"),
            Self::RateLimited { status, retry_after } => {
                write!(f, "rate limited (http {status})")?;
                if let Some(d) = retry_after {
                    write!(f, ", retry after {}s", d.as_secs())?;
                }
                Ok(())
            }
            Self::Decode { source, body } => write!(f, "decode: {source}; body: {body}"),
            Self::UnexpectedResponse { operation, shape } => {
                write!(f, "unexpected {operation} response: {shape}")
            }
            Self::Io(e) => write!(f, "io: {e}"),
            Self::Crypto(m) => write!(f, "crypto: {m}"),
            Self::InvalidInput(m) => write!(f, "invalid input: {m}"),
            Self::NotLoggedIn => f.write_str("no refresh token available, login required"),
            Self::NotFound(what) => write!(f, "not found: {what}"),
            Self::Blocked => f.write_str("resource is blocked by the server"),
            Self::Integrity { expected, actual } => {
                write!(f, "integrity check failed: expected {expected}, got {actual}")
            }
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Network(e) => Some(e),
            Self::Api(e) => Some(e),
            Self::Decode { source, .. } => Some(source),
            Self::Io(e) => Some(e),
            _ => None,
        }
    }
}

pub(crate) fn unexpected_response(operation: &'static str, body: &Value) -> Error {
    fn kind(value: Option<&Value>) -> &'static str {
        match value {
            None => "missing",
            Some(Value::Null) => "null",
            Some(Value::Bool(true)) => "true",
            Some(Value::Bool(false)) => "false",
            Some(Value::Number(_)) => "number",
            Some(Value::String(s)) if s.is_empty() => "empty-string",
            Some(Value::String(_)) => "string",
            Some(Value::Array(_)) => "array",
            Some(Value::Object(_)) => "object",
        }
    }
    let keys = [
        "code",
        "message",
        "msg",
        "success",
        "result",
        "data",
        "share_id",
        "share_url",
        "shareId",
        "shareUrl",
        "bottleId",
        "bottleName",
        "createBottleLimit",
        "createBottleUsed",
        "fishBottleLimit",
        "fishBottleUsed",
        "error",
        "error_description",
        "error_message",
        "error_code",
        "error_msg",
        "errorCode",
        "errorMessage",
        "errorMsg",
        "errCode",
        "errMsg",
        "resultCode",
        "resultMsg",
        "resultMessage",
        "statusCode",
        "statusMessage",
        "status",
        "text",
        "body",
        "description",
        "display_message",
        "reason",
        "title",
        "tips",
    ];
    let mut fields = vec![
        format!("root={}", kind(Some(body))),
        format!("share_id={}", kind(body.get("share_id"))),
    ];
    for (prefix, object) in [
        ("", Some(body)),
        ("data.", body.get("data")),
        ("error.", body.get("error")),
    ] {
        for key in keys {
            if let Some(value) = object.and_then(|v| v.get(key)) {
                fields.push(format!("{prefix}{key}={}", kind(Some(value))));
            }
        }
        if let Some(object) = object.and_then(Value::as_object) {
            let unknown = object.keys().filter(|key| !keys.contains(&key.as_str())).count();
            fields.push(format!("{prefix}unknown_fields={unknown}"));
        }
    }
    let upgrade_notice = [Some(body), body.get("data"), body.get("error")]
        .into_iter()
        .flatten()
        .any(|object| {
            [
                "message",
                "msg",
                "display_message",
                "error",
                "error_description",
                "error_message",
                "error_msg",
                "errorMessage",
                "errorMsg",
                "errMsg",
                "resultMsg",
                "resultMessage",
                "statusMessage",
                "description",
                "reason",
                "tips",
            ]
            .into_iter()
            .any(|key| {
                object.get(key).and_then(Value::as_str).is_some_and(|message| {
                    message.contains("\u{5347}\u{7ea7}") || message.to_ascii_lowercase().contains("upgrade")
                })
            })
        });
    if upgrade_notice {
        fields.push("notice=upgrade_required".into());
    }
    Error::UnexpectedResponse {
        operation,
        shape: fields.join("; "),
    }
}

impl From<reqwest::Error> for Error {
    fn from(e: reqwest::Error) -> Self {
        Self::Network(e)
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<ApiError> for Error {
    fn from(e: ApiError) -> Self {
        Self::Api(e)
    }
}

/// Live testing confirmed that albums_info returns `"code":"200"` on success.
fn is_success_code(code: &str) -> bool {
    code.parse::<u16>().is_ok_and(|n| n == 0 || (200..300).contains(&n))
}

fn snippet(body: &[u8]) -> String {
    const MAX: usize = 512;
    let text = String::from_utf8_lossy(body);
    match text.char_indices().nth(MAX) {
        Some((i, _)) => format!("{}…", &text[..i]),
        None => text.into_owned(),
    }
}

/// Classify the HTTP status and response body as a single result.
pub(crate) fn check_response(status: u16, body: &[u8], retry_after: Option<Duration>) -> Result<()> {
    if status == 429 {
        return Err(Error::RateLimited { status, retry_after });
    }
    if let Some(e) = ApiError::from_body(status, body) {
        return Err(Error::Api(e));
    }
    if !(200..300).contains(&status) {
        return Err(Error::Http {
            status,
            body: snippet(body),
        });
    }
    Ok(())
}

/// Decode an empty response body as `null`.
pub(crate) fn decode<R: DeserializeOwned>(body: &[u8]) -> Result<R> {
    let input = if body.iter().all(u8::is_ascii_whitespace) {
        b"null".as_slice()
    } else {
        body
    };
    serde_json::from_slice(input).map_err(|source| Error::Decode {
        source,
        body: snippet(body),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unexpected_response_classifies_notices_without_exposing_names_or_values() {
        for body in [
            serde_json::json!({"error": "\u{8bf7}\u{5347}\u{7ea7}", "private-name": "private-value"}),
            serde_json::json!({"error": {"message": "Please upgrade; private-value"}}),
        ] {
            let text = unexpected_response("create_share_link", &body).to_string();
            assert!(text.contains("notice=upgrade_required"));
            assert!(text.contains("share_id=missing"));
            assert!(!text.contains("private-name"));
            assert!(!text.contains("private-value"));
        }
    }

    #[test]
    fn parses_api_error() {
        let body = br#"{"code":"AccessTokenInvalid","message":"AccessToken is invalid."}"#;
        let e = ApiError::from_body(401, body).unwrap();
        assert_eq!(e.kind(), ApiErrorKind::AccessTokenInvalid);
        assert_eq!(e.http_status, 401);
        assert_eq!(e.user_message(), "AccessToken is invalid.");
    }

    #[test]
    fn prefers_display_message() {
        let body = r#"{"code":"NotFound.File","message":"m","display_message":"文件不存在"}"#;
        let e = ApiError::from_body(404, body.as_bytes()).unwrap();
        assert_eq!(e.kind(), ApiErrorKind::NotFound);
        assert_eq!(e.user_message(), "文件不存在");
    }

    #[test]
    fn empty_or_missing_code_is_success() {
        assert!(ApiError::from_body(200, br#"{"code":"","data":{}}"#).is_none());
        assert!(ApiError::from_body(200, br#"{"items":[]}"#).is_none());
        assert!(ApiError::from_body(200, br#"{"code":0}"#).is_none());
        assert!(ApiError::from_body(200, br#"{"code":"200","message":"success"}"#).is_none());
        assert!(ApiError::from_body(200, br#"{"code":"0"}"#).is_none());
        assert!(ApiError::from_body(200, br#"{"code":"500"}"#).is_some());
        assert!(ApiError::from_body(502, b"Bad Gateway").is_none());
    }

    #[test]
    fn check_response_classification() {
        assert!(check_response(200, b"{}", None).is_ok());
        assert!(check_response(204, b"", None).is_ok());
        assert!(matches!(
            check_response(429, b"", None),
            Err(Error::RateLimited { status: 429, .. })
        ));
        assert!(matches!(
            check_response(502, b"Bad Gateway", None),
            Err(Error::Http { status: 502, ref body }) if body == "Bad Gateway"
        ));
        let err = check_response(200, br#"{"code":"AlreadyExist.File","message":"x"}"#, None).unwrap_err();
        assert!(err.is_already_exists());
    }

    #[test]
    fn decode_empty_body() {
        let v: serde::de::IgnoredAny = decode(b"").unwrap();
        let _ = v;
        let o: Option<u32> = decode(b"  ").unwrap();
        assert_eq!(o, None);
        assert!(matches!(decode::<u32>(b"oops"), Err(Error::Decode { .. })));
    }

    #[test]
    fn known_codes() {
        let kind = |code: &str| {
            ApiError {
                code: code.into(),
                message: String::new(),
                display_message: None,
                http_status: 400,
            }
            .kind()
        };
        assert_eq!(kind("AccessTokenExpired"), ApiErrorKind::AccessTokenInvalid);
        assert_eq!(kind("InvalidParameter.RefreshToken"), ApiErrorKind::RefreshTokenInvalid);
        assert_eq!(kind("DeviceSessionSignatureInvalid"), ApiErrorKind::SignatureInvalid);
        assert_eq!(kind("UserDeviceOffline"), ApiErrorKind::DeviceLimit);
        assert_eq!(kind("NotFound.FileId"), ApiErrorKind::NotFound);
        assert_eq!(kind("Something.New"), ApiErrorKind::Other);
    }
}
