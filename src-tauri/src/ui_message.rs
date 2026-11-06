//! Stable presentation codes at the desktop boundary; diagnostics stay verbatim.
use serde::Serialize;
use serde_json::{Map, Value};

#[derive(Debug, Clone, Serialize)]
pub struct UiMessage {
    pub code: String,
    pub args: Map<String, Value>,
    pub detail: Option<String>,
    /// English fallback for old clients and diagnostics. Never used as a key.
    pub message: String,
}

impl UiMessage {
    pub fn app(code: &str, message: impl Into<String>) -> Self {
        Self { code: code.into(), args: Map::new(), detail: None, message: message.into() }
    }

    pub fn with_args(mut self, args: Value) -> Self {
        if let Value::Object(args) = args { self.args = args; }
        self
    }

    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }
}

impl From<String> for UiMessage {
    fn from(detail: String) -> Self {
        Self::app("native.operationFailed", detail.clone()).with_detail(detail)
    }
}

impl From<&str> for UiMessage {
    fn from(detail: &str) -> Self { detail.to_owned().into() }
}

impl std::fmt::Display for UiMessage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.write_str(&self.message) }
}

impl std::error::Error for UiMessage {}

pub fn app(code: &str, message: impl Into<String>) -> UiMessage { UiMessage::app(code, message) }

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_codes_preserve_interpolation_values_and_legacy_diagnostics() {
        let message = app("native.speakerMerged", "Merged a into b").with_args(serde_json::json!({ "from": "Speaker 1", "name": "Анна", "count": 21 }));
        let json = serde_json::to_value(&message).unwrap();
        assert_eq!(json["code"], "native.speakerMerged");
        assert_eq!(json["args"]["count"], 21);
        assert_eq!(json["args"]["name"], "Анна");
        assert_eq!(message.to_string(), "Merged a into b");
        let external: UiMessage = "Provider: EACCES <file>".into();
        assert_eq!(external.code, "native.operationFailed");
        assert_eq!(external.detail.as_deref(), Some("Provider: EACCES <file>"));
    }
}
