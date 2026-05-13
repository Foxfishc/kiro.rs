//! 推理内容事件
//!
//! 处理 reasoningContentEvent — Q Developer 在 thinking 模式下推送的服务端
//! 推理内容流。对应 Anthropic 的 `thinking` content_block + thinking_delta /
//! signature_delta SSE 事件。
//!
//! payload 形态（实测）：
//!   `{ "text": "..." }`                     — 增量文本
//!   `{ "text": "...", "signature": "..." }` — 末尾的服务端签名（多轮 cache 必需）
//!   `{ "redactedContent": "..." }`          — 服务端打码的不可读内容（可丢）

use serde::{Deserialize, Serialize};

use crate::kiro::parser::error::ParseResult;
use crate::kiro::parser::frame::Frame;

use super::base::EventPayload;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct ReasoningContentEvent {
    /// 增量推理文本
    #[serde(default)]
    pub text: String,
    /// 服务端签名（仅在 reasoning 块结束时出现，客户端必须在下一轮原样回放以维持 cache）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
    /// 打码内容（不可读，可忽略）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub redacted_content: Option<String>,
}

impl EventPayload for ReasoningContentEvent {
    fn from_frame(frame: &Frame) -> ParseResult<Self> {
        frame.payload_as_json()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_deserialize_text_only() {
        let json = r#"{"text":"用户需要..."}"#;
        let ev: ReasoningContentEvent = serde_json::from_str(json).unwrap();
        assert_eq!(ev.text, "用户需要...");
        assert!(ev.signature.is_none());
    }

    #[test]
    fn test_deserialize_with_signature() {
        let json = r#"{"text":"final","signature":"sig-abc"}"#;
        let ev: ReasoningContentEvent = serde_json::from_str(json).unwrap();
        assert_eq!(ev.signature.as_deref(), Some("sig-abc"));
    }

    #[test]
    fn test_deserialize_redacted() {
        let json = r#"{"redactedContent":"opaque-blob"}"#;
        let ev: ReasoningContentEvent = serde_json::from_str(json).unwrap();
        assert_eq!(ev.redacted_content.as_deref(), Some("opaque-blob"));
        assert_eq!(ev.text, "");
    }
}
