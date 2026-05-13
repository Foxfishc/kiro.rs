//! 事件基础定义
//!
//! 定义事件类型枚举、trait 和统一事件结构

use crate::kiro::parser::error::{ParseError, ParseResult};
use crate::kiro::parser::frame::Frame;

/// 事件类型枚举
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EventType {
    /// 助手响应事件
    AssistantResponse,
    /// 工具使用事件
    ToolUse,
    /// 计费事件
    Metering,
    /// 上下文使用率事件
    ContextUsage,
    /// 初始响应事件（流的第一帧，包含服务端分配的 conversationId）
    InitialResponse,
    /// 推理内容事件（thinking 模型的服务端推理流，对应 Anthropic 的 `thinking` content_block）
    ReasoningContent,
    /// 未知事件类型
    Unknown,
}

impl EventType {
    /// 从事件类型字符串解析
    pub fn from_str(s: &str) -> Self {
        match s {
            "assistantResponseEvent" => Self::AssistantResponse,
            "toolUseEvent" => Self::ToolUse,
            "meteringEvent" => Self::Metering,
            "contextUsageEvent" => Self::ContextUsage,
            "initialResponseEvent" => Self::InitialResponse,
            "reasoningContentEvent" => Self::ReasoningContent,
            _ => Self::Unknown,
        }
    }

    /// 转换为事件类型字符串
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::AssistantResponse => "assistantResponseEvent",
            Self::ToolUse => "toolUseEvent",
            Self::Metering => "meteringEvent",
            Self::ContextUsage => "contextUsageEvent",
            Self::InitialResponse => "initialResponseEvent",
            Self::ReasoningContent => "reasoningContentEvent",
            Self::Unknown => "unknown",
        }
    }
}

impl std::fmt::Display for EventType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// 事件 payload trait
///
/// 所有具体事件类型都需要实现此 trait
pub trait EventPayload: Sized {
    /// 从帧解析事件负载
    fn from_frame(frame: &Frame) -> ParseResult<Self>;
}

/// 统一事件枚举
///
/// 封装所有可能的事件类型
#[derive(Debug, Clone)]
pub enum Event {
    /// 助手响应
    AssistantResponse(super::AssistantResponseEvent),
    /// 工具使用
    ToolUse(super::ToolUseEvent),
    /// 计费
    Metering(super::MeteringEvent),
    /// 上下文使用率
    ContextUsage(super::ContextUsageEvent),
    /// 推理内容（thinking 模型的服务端推理流）
    ReasoningContent(super::ReasoningContentEvent),
    /// 初始响应（首帧，含 conversationId）
    InitialResponse {
        /// 服务端分配的 conversationId（可能为空字符串）。
        /// 预留：未来 stream handler 可用它做 server-authoritative
        /// conversationId affinity（覆盖客户端生成的 UUID）。
        #[allow(dead_code)]
        conversation_id: String,
    },
    /// 未知事件 (保留原始帧数据)
    Unknown {},
    /// 服务端错误
    Error {
        /// 错误代码
        error_code: String,
        /// 错误消息
        error_message: String,
    },
    /// 服务端异常
    Exception {
        /// 异常类型
        exception_type: String,
        /// 异常消息
        message: String,
    },
}

impl Event {
    /// 从帧解析事件
    pub fn from_frame(frame: Frame) -> ParseResult<Self> {
        let message_type = frame.message_type().unwrap_or("event");

        match message_type {
            "event" => Self::parse_event(frame),
            "error" => Self::parse_error(frame),
            "exception" => Self::parse_exception(frame),
            other => Err(ParseError::InvalidMessageType(other.to_string())),
        }
    }

    /// 解析事件类型消息
    fn parse_event(frame: Frame) -> ParseResult<Self> {
        let event_type_str = frame.event_type().unwrap_or("unknown");
        let event_type = EventType::from_str(event_type_str);

        match event_type {
            EventType::AssistantResponse => {
                let payload = super::AssistantResponseEvent::from_frame(&frame)?;
                Ok(Self::AssistantResponse(payload))
            }
            EventType::ToolUse => {
                let payload = super::ToolUseEvent::from_frame(&frame)?;
                Ok(Self::ToolUse(payload))
            }
            EventType::Metering => {
                let payload = super::MeteringEvent::from_frame(&frame)?;
                Ok(Self::Metering(payload))
            }
            EventType::ContextUsage => {
                let payload = super::ContextUsageEvent::from_frame(&frame)?;
                Ok(Self::ContextUsage(payload))
            }
            EventType::InitialResponse => {
                // Payload shape: `{"conversationId":""}` (may be populated by
                // server). We just lift the id — no dedicated payload struct
                // needed since the field is single-purpose.
                let payload_str = frame.payload_as_str();
                let conversation_id = serde_json::from_str::<serde_json::Value>(&payload_str)
                    .ok()
                    .and_then(|v| {
                        v.get("conversationId")
                            .and_then(|s| s.as_str())
                            .map(String::from)
                    })
                    .unwrap_or_default();
                Ok(Self::InitialResponse { conversation_id })
            }
            EventType::ReasoningContent => {
                let payload = super::ReasoningContentEvent::from_frame(&frame)?;
                Ok(Self::ReasoningContent(payload))
            }
            EventType::Unknown => Ok(Self::Unknown {}),
        }
    }

    /// 解析错误类型消息
    fn parse_error(frame: Frame) -> ParseResult<Self> {
        let error_code = frame
            .headers
            .error_code()
            .unwrap_or("UnknownError")
            .to_string();
        let error_message = frame.payload_as_str();

        Ok(Self::Error {
            error_code,
            error_message,
        })
    }

    /// 解析异常类型消息
    fn parse_exception(frame: Frame) -> ParseResult<Self> {
        let exception_type = frame
            .headers
            .exception_type()
            .unwrap_or("UnknownException")
            .to_string();
        let message = frame.payload_as_str();

        Ok(Self::Exception {
            exception_type,
            message,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_event_type_from_str() {
        assert_eq!(
            EventType::from_str("assistantResponseEvent"),
            EventType::AssistantResponse
        );
        assert_eq!(EventType::from_str("toolUseEvent"), EventType::ToolUse);
        assert_eq!(EventType::from_str("meteringEvent"), EventType::Metering);
        assert_eq!(
            EventType::from_str("contextUsageEvent"),
            EventType::ContextUsage
        );
        assert_eq!(EventType::from_str("unknown_type"), EventType::Unknown);
    }

    #[test]
    fn test_event_type_as_str() {
        assert_eq!(
            EventType::AssistantResponse.as_str(),
            "assistantResponseEvent"
        );
        assert_eq!(EventType::ToolUse.as_str(), "toolUseEvent");
    }

    #[test]
    fn test_parse_metering_event_payload() {
        use crate::kiro::parser::header::{HeaderValue, Headers};

        let mut headers = Headers::new();
        headers.insert(
            ":message-type".to_string(),
            HeaderValue::String("event".to_string()),
        );
        headers.insert(
            ":event-type".to_string(),
            HeaderValue::String("meteringEvent".to_string()),
        );

        let frame = Frame {
            headers,
            payload: br#"{"unit":"credit","unitPlural":"credits","usage":0.25}"#.to_vec(),
        };

        let event = Event::from_frame(frame).unwrap();
        match event {
            Event::Metering(metering) => {
                assert_eq!(metering.unit, "credit");
                assert_eq!(metering.unit_plural, "credits");
                assert_eq!(metering.usage, 0.25);
            }
            other => panic!("expected metering event, got {other:?}"),
        }
    }
}
