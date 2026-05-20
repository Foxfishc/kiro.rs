//! 流式响应处理模块
//!
//! 实现 Kiro → Anthropic 流式响应转换和 SSE 状态管理

use std::collections::HashMap;

use serde_json::json;
use uuid::Uuid;

use crate::common::utf8::floor_char_boundary;
use crate::kiro::model::events::{Event, MeteringEvent};

/// 需要跳过的包裹字符
///
/// 当 thinking 标签被这些字符包裹时，认为是在引用标签而非真正的标签：
/// - 反引号 (`)：行内代码
/// - 双引号 (")：字符串
/// - 单引号 (')：字符串
const QUOTE_CHARS: &[u8] = b"`\"'\\#!@$%^&*()-_=+[]{};:<>,.?/";

/// 检查指定位置的字符是否是引用字符
fn is_quote_char(buffer: &str, pos: usize) -> bool {
    buffer
        .as_bytes()
        .get(pos)
        .map(|c| QUOTE_CHARS.contains(c))
        .unwrap_or(false)
}

/// 查找真正的 thinking 结束标签（不被引用字符包裹，且后面有双换行符）
///
/// 当模型在思考过程中提到 `</thinking>` 时，通常会用反引号、引号等包裹，
/// 或者在同一行有其他内容（如"关于 </thinking> 标签"）。
/// 这个函数会跳过这些情况，只返回真正的结束标签位置。
///
/// 跳过的情况：
/// - 被引用字符包裹（反引号、引号等）
/// - 后面没有双换行符（真正的结束标签后面会有 `\n\n`）
/// - 标签在缓冲区末尾（流式处理时需要等待更多内容）
///
/// # 参数
/// - `buffer`: 要搜索的字符串
///
/// # 返回值
/// - `Some(pos)`: 真正的结束标签的起始位置
/// - `None`: 没有找到真正的结束标签
fn find_real_thinking_end_tag(buffer: &str) -> Option<usize> {
    const TAG: &str = "</thinking>";
    let mut search_start = 0;

    while let Some(pos) = buffer[search_start..].find(TAG) {
        let absolute_pos = search_start + pos;

        // 检查前面是否有引用字符
        let has_quote_before = absolute_pos > 0 && is_quote_char(buffer, absolute_pos - 1);

        // 检查后面是否有引用字符
        let after_pos = absolute_pos + TAG.len();
        let has_quote_after = is_quote_char(buffer, after_pos);

        // 如果被引用字符包裹，跳过
        if has_quote_before || has_quote_after {
            search_start = absolute_pos + 1;
            continue;
        }

        // 检查后面的内容
        let after_content = &buffer[after_pos..];

        // 如果标签后面内容不足以判断是否有双换行符，等待更多内容
        if after_content.len() < 2 {
            return None;
        }

        // 真正的 thinking 结束标签后面会有双换行符 `\n\n`
        if after_content.starts_with("\n\n") {
            return Some(absolute_pos);
        }

        // 不是双换行符，跳过继续搜索
        search_start = absolute_pos + 1;
    }

    None
}

/// 查找缓冲区末尾的 thinking 结束标签（允许末尾只有空白字符）
///
/// 用于“边界事件”场景：例如 thinking 结束后立刻进入 tool_use，或流结束，
/// 此时 `</thinking>` 后面可能没有 `\n\n`，但结束标签依然应被识别并过滤。
///
/// 约束：只有当 `</thinking>` 之后全部都是空白字符时才认为是结束标签，
/// 以避免在 thinking 内容中提到 `</thinking>`（非结束标签）时误判。
fn find_real_thinking_end_tag_at_buffer_end(buffer: &str) -> Option<usize> {
    const TAG: &str = "</thinking>";
    let mut search_start = 0;

    while let Some(pos) = buffer[search_start..].find(TAG) {
        let absolute_pos = search_start + pos;

        // 检查前面是否有引用字符
        let has_quote_before = absolute_pos > 0 && is_quote_char(buffer, absolute_pos - 1);

        // 检查后面是否有引用字符
        let after_pos = absolute_pos + TAG.len();
        let has_quote_after = is_quote_char(buffer, after_pos);

        if has_quote_before || has_quote_after {
            search_start = absolute_pos + 1;
            continue;
        }

        // 只有当标签后面全部是空白字符时才认定为结束标签
        if buffer[after_pos..].trim().is_empty() {
            return Some(absolute_pos);
        }

        search_start = absolute_pos + 1;
    }

    None
}

/// 查找真正的 thinking 开始标签（不被引用字符包裹）
///
/// 与 `find_real_thinking_end_tag` 类似，跳过被引用字符包裹的开始标签。
fn find_real_thinking_start_tag(buffer: &str) -> Option<usize> {
    const TAG: &str = "<thinking>";
    let mut search_start = 0;

    while let Some(pos) = buffer[search_start..].find(TAG) {
        let absolute_pos = search_start + pos;

        // 检查前面是否有引用字符
        let has_quote_before = absolute_pos > 0 && is_quote_char(buffer, absolute_pos - 1);

        // 检查后面是否有引用字符
        let after_pos = absolute_pos + TAG.len();
        let has_quote_after = is_quote_char(buffer, after_pos);

        // 如果不被引用字符包裹，则是真正的开始标签
        if !has_quote_before && !has_quote_after {
            return Some(absolute_pos);
        }

        // 继续搜索下一个匹配
        search_start = absolute_pos + 1;
    }

    None
}

/// SSE 事件
#[derive(Debug, Clone)]
pub struct SseEvent {
    pub event: String,
    pub data: serde_json::Value,
}

impl SseEvent {
    pub fn new(event: impl Into<String>, data: serde_json::Value) -> Self {
        Self {
            event: event.into(),
            data,
        }
    }

    /// 格式化为 SSE 字符串
    pub fn to_sse_string(&self) -> String {
        format!(
            "event: {}\ndata: {}\n\n",
            self.event,
            serde_json::to_string(&self.data).unwrap_or_default()
        )
    }
}

/// 内容块状态
#[derive(Debug, Clone)]
struct BlockState {
    block_type: String,
    started: bool,
    stopped: bool,
}

impl BlockState {
    fn new(block_type: impl Into<String>) -> Self {
        Self {
            block_type: block_type.into(),
            started: false,
            stopped: false,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct FinalUsage<'a> {
    input_tokens: i32,
    output_tokens: i32,
    cache_usage: Option<CacheUsageBreakdown>,
    metering: Option<&'a MeteringEvent>,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct CacheUsageBreakdown {
    pub cache_creation_input_tokens: i32,
    pub cache_read_input_tokens: i32,
    pub cache_creation_5m_input_tokens: i32,
    pub cache_creation_1h_input_tokens: i32,
}

/// SSE 状态管理器
///
/// 确保 SSE 事件序列符合 Claude API 规范：
/// 1. message_start 只能出现一次
/// 2. content_block 必须先 start 再 delta 再 stop
/// 3. message_delta 只能出现一次，且在所有 content_block_stop 之后
/// 4. message_stop 在最后
#[derive(Debug)]
pub struct SseStateManager {
    /// message_start 是否已发送
    message_started: bool,
    /// message_delta 是否已发送
    message_delta_sent: bool,
    /// 活跃的内容块状态
    active_blocks: HashMap<i32, BlockState>,
    /// 消息是否已结束
    message_ended: bool,
    /// 下一个块索引
    next_block_index: i32,
    /// 当前 stop_reason
    stop_reason: Option<String>,
    /// 是否有工具调用
    has_tool_use: bool,
}

impl Default for SseStateManager {
    fn default() -> Self {
        Self::new()
    }
}

impl SseStateManager {
    pub fn new() -> Self {
        Self {
            message_started: false,
            message_delta_sent: false,
            active_blocks: HashMap::new(),
            message_ended: false,
            next_block_index: 0,
            stop_reason: None,
            has_tool_use: false,
        }
    }

    /// 判断指定块是否处于可接收 delta 的打开状态
    fn is_block_open_of_type(&self, index: i32, expected_type: &str) -> bool {
        self.active_blocks
            .get(&index)
            .is_some_and(|b| b.started && !b.stopped && b.block_type == expected_type)
    }

    /// 获取下一个块索引
    pub fn next_block_index(&mut self) -> i32 {
        let index = self.next_block_index;
        self.next_block_index += 1;
        index
    }

    /// 记录工具调用
    pub fn set_has_tool_use(&mut self, has: bool) {
        self.has_tool_use = has;
    }

    /// stop_reason 优先级（索引越小优先级越高）
    ///
    /// `error` 是 Anthropic 非标准但被 Anthropic SDK 客户端识别的 stop reason：
    /// 上游流中断、Smithy exception 等异常路径走这条，让客户端知道这一轮 assistant
    /// 内容是异常截断的，而不是被当成 end_turn 写回 history 污染后续对话。
    const STOP_REASON_PRIORITY: &'static [&'static str] = &[
        "error",
        "model_context_window_exceeded",
        "max_tokens",
        "tool_use",
        "end_turn",
    ];

    /// 获取 stop_reason 的优先级（越小越高，未知原因返回 usize::MAX）
    fn stop_reason_priority(reason: &str) -> usize {
        Self::STOP_REASON_PRIORITY
            .iter()
            .position(|&r| r == reason)
            .unwrap_or(usize::MAX)
    }

    /// 设置 stop_reason（高优先级原因可覆盖低优先级原因）
    ///
    /// 优先级从高到低：model_context_window_exceeded > max_tokens > tool_use > end_turn
    pub fn set_stop_reason(&mut self, reason: impl Into<String>) {
        let reason = reason.into();
        let new_priority = Self::stop_reason_priority(&reason);
        let should_set = match &self.stop_reason {
            None => true,
            Some(current) => new_priority < Self::stop_reason_priority(current),
        };
        if should_set {
            self.stop_reason = Some(reason);
        }
    }

    /// 检查是否存在非 thinking 类型的内容块（如 text 或 tool_use）
    fn has_non_thinking_blocks(&self) -> bool {
        self.active_blocks
            .values()
            .any(|b| b.block_type != "thinking")
    }

    /// 获取最终的 stop_reason
    pub fn get_stop_reason(&self) -> String {
        if let Some(ref reason) = self.stop_reason {
            reason.clone()
        } else if self.has_tool_use {
            "tool_use".to_string()
        } else {
            "end_turn".to_string()
        }
    }

    /// 处理 message_start 事件
    pub fn handle_message_start(&mut self, event: serde_json::Value) -> Option<SseEvent> {
        if self.message_started {
            tracing::debug!("跳过重复的 message_start 事件");
            return None;
        }
        self.message_started = true;
        Some(SseEvent::new("message_start", event))
    }

    /// 处理 content_block_start 事件
    pub fn handle_content_block_start(
        &mut self,
        index: i32,
        block_type: &str,
        data: serde_json::Value,
    ) -> Vec<SseEvent> {
        let mut events = Vec::new();

        // 如果是 tool_use 块，先关闭之前的文本块
        if block_type == "tool_use" {
            self.has_tool_use = true;
            for (block_index, block) in self.active_blocks.iter_mut() {
                if block.block_type == "text" && block.started && !block.stopped {
                    // 自动发送 content_block_stop 关闭文本块
                    events.push(SseEvent::new(
                        "content_block_stop",
                        json!({
                            "type": "content_block_stop",
                            "index": block_index
                        }),
                    ));
                    block.stopped = true;
                }
            }
        }

        // 检查块是否已存在
        if let Some(block) = self.active_blocks.get_mut(&index) {
            if block.started {
                tracing::trace!("块 {} 已启动，跳过重复的 content_block_start", index);
                return events;
            }
            block.started = true;
        } else {
            let mut block = BlockState::new(block_type);
            block.started = true;
            self.active_blocks.insert(index, block);
        }

        events.push(SseEvent::new("content_block_start", data));
        events
    }

    /// 处理 content_block_delta 事件
    pub fn handle_content_block_delta(
        &mut self,
        index: i32,
        data: serde_json::Value,
    ) -> Option<SseEvent> {
        // 确保块已启动
        if let Some(block) = self.active_blocks.get(&index) {
            if !block.started || block.stopped {
                tracing::warn!(
                    "块 {} 状态异常: started={}, stopped={}",
                    index,
                    block.started,
                    block.stopped
                );
                return None;
            }
        } else {
            // 块不存在，可能需要先创建
            tracing::warn!("收到未知块 {} 的 delta 事件", index);
            return None;
        }

        Some(SseEvent::new("content_block_delta", data))
    }

    /// 处理 content_block_stop 事件
    pub fn handle_content_block_stop(&mut self, index: i32) -> Option<SseEvent> {
        if let Some(block) = self.active_blocks.get_mut(&index) {
            if block.stopped {
                tracing::debug!("块 {} 已停止，跳过重复的 content_block_stop", index);
                return None;
            }
            block.stopped = true;
            return Some(SseEvent::new(
                "content_block_stop",
                json!({
                    "type": "content_block_stop",
                    "index": index
                }),
            ));
        }
        None
    }

    /// 生成最终事件序列
    pub fn generate_final_events(&mut self, usage: FinalUsage<'_>) -> Vec<SseEvent> {
        let mut events = Vec::new();

        // 关闭所有未关闭的块
        for (index, block) in self.active_blocks.iter_mut() {
            if block.started && !block.stopped {
                events.push(SseEvent::new(
                    "content_block_stop",
                    json!({
                        "type": "content_block_stop",
                        "index": index
                    }),
                ));
                block.stopped = true;
            }
        }

        // 发送 message_delta
        if !self.message_delta_sent {
            self.message_delta_sent = true;
            let mut usage_json = json!({
                "input_tokens": usage.input_tokens,
                "output_tokens": usage.output_tokens,
            });
            if let Some(cache_usage) = usage.cache_usage {
                usage_json["cache_creation_input_tokens"] =
                    json!(cache_usage.cache_creation_input_tokens);
                usage_json["cache_read_input_tokens"] = json!(cache_usage.cache_read_input_tokens);
                usage_json["cache_creation"] = json!({
                    "ephemeral_5m_input_tokens": cache_usage.cache_creation_5m_input_tokens,
                    "ephemeral_1h_input_tokens": cache_usage.cache_creation_1h_input_tokens
                });
            }
            if let Some(metering) = usage.metering {
                usage_json["credit_usage"] = json!(metering.usage);
                usage_json["credit_unit"] = json!(metering.unit);
                usage_json["credit_unit_plural"] = json!(metering.unit_plural);
            }
            events.push(SseEvent::new(
                "message_delta",
                json!({
                    "type": "message_delta",
                    "delta": {
                        "stop_reason": self.get_stop_reason(),
                        "stop_sequence": null
                    },
                    "usage": usage_json
                }),
            ));
        }

        // 发送 message_stop
        if !self.message_ended {
            self.message_ended = true;
            events.push(SseEvent::new(
                "message_stop",
                json!({ "type": "message_stop" }),
            ));
        }

        events
    }
}

/// 流处理上下文
pub struct StreamContext {
    /// SSE 状态管理器
    pub state_manager: SseStateManager,
    /// 请求的模型名称
    pub model: String,
    /// 消息 ID
    pub message_id: String,
    /// 输入 tokens（估算值）
    pub input_tokens: i32,
    /// cache usage 统计（可选）
    pub cache_usage: Option<CacheUsageBreakdown>,
    /// 从 contextUsageEvent 计算的实际输入 tokens
    pub context_input_tokens: Option<i32>,
    /// 输出 tokens 累计
    pub output_tokens: i32,
    /// 工具块索引映射 (tool_id -> block_index)
    pub tool_block_indices: HashMap<String, i32>,
    /// 工具名称反向映射（短名称 → 原始名称），用于响应时还原
    pub tool_name_map: HashMap<String, String>,
    /// thinking 是否启用
    pub thinking_enabled: bool,
    /// thinking 内容缓冲区
    pub thinking_buffer: String,
    /// 是否在 thinking 块内
    pub in_thinking_block: bool,
    /// thinking 块是否已提取完成
    pub thinking_extracted: bool,
    /// thinking 块索引
    pub thinking_block_index: Option<i32>,
    /// Q 上游 reasoningContentEvent 通过独立事件流推 thinking 内容（与老的
    /// 嵌入 `<thinking>` 标签格式互斥）；此字段标记 reasoning 块是否打开。
    pub reasoning_block_open: bool,
    /// reasoningContentEvent 末尾 payload 可能带 signature，需要在 content_block_stop
    /// 之前 emit signature_delta；缓存它直到关闭 thinking 块的时机。
    pub pending_reasoning_signature: Option<String>,
    /// 文本块索引（按需动态分配）
    pub text_block_index: Option<i32>,
    /// 上游 meteringEvent 透传的 credit usage，仅用于最终 usage 统计，不生成独立 SSE 事件
    pub metering: Option<MeteringEvent>,
    /// 是否需要剥离 thinking 内容开头的换行符
    /// 模型输出 `<thinking>\n` 时，`\n` 可能与标签在同一 chunk 或下一 chunk
    strip_thinking_leading_newline: bool,
}

impl StreamContext {
    /// 创建启用thinking的StreamContext
    pub fn new_with_thinking(
        model: impl Into<String>,
        input_tokens: i32,
        cache_usage: Option<CacheUsageBreakdown>,
        thinking_enabled: bool,
        tool_name_map: HashMap<String, String>,
    ) -> Self {
        Self {
            state_manager: SseStateManager::new(),
            model: model.into(),
            message_id: format!("msg_{}", Uuid::new_v4().to_string().replace('-', "")),
            input_tokens,
            cache_usage,
            context_input_tokens: None,
            output_tokens: 0,
            tool_block_indices: HashMap::new(),
            tool_name_map,
            thinking_enabled,
            thinking_buffer: String::new(),
            in_thinking_block: false,
            thinking_extracted: false,
            thinking_block_index: None,
            reasoning_block_open: false,
            pending_reasoning_signature: None,
            text_block_index: None,
            metering: None,
            strip_thinking_leading_newline: false,
        }
    }

    /// 生成 message_start 事件
    pub fn create_message_start_event(&self) -> serde_json::Value {
        let billed_input_tokens = self
            .cache_usage
            .map(|cache_usage| {
                billed_input_tokens(
                    self.input_tokens,
                    cache_usage.cache_creation_input_tokens,
                    cache_usage.cache_read_input_tokens,
                )
            })
            .unwrap_or(self.input_tokens);
        let mut usage = json!({
            "input_tokens": billed_input_tokens,
            "output_tokens": 1,
        });
        if let Some(cache_usage) = self.cache_usage {
            usage["cache_creation_input_tokens"] = json!(cache_usage.cache_creation_input_tokens);
            usage["cache_read_input_tokens"] = json!(cache_usage.cache_read_input_tokens);
        }
        json!({
            "type": "message_start",
            "message": {
                "id": self.message_id,
                "type": "message",
                "role": "assistant",
                "content": [],
                "model": self.model,
                "stop_reason": null,
                "stop_sequence": null,
                "usage": usage
            }
        })
    }

    /// 生成初始事件序列（仅 message_start）
    ///
    /// 注意：不再在初始化阶段创建空 text block。
    /// 否则当模型首个输出为 tool_use（且没有任何 text_delta）时，
    /// 会产生一个空的 text content block（text=""），客户端写回 history 后会触发上游校验拒绝。
    pub fn generate_initial_events(&mut self) -> Vec<SseEvent> {
        let mut events = Vec::new();

        // message_start
        let msg_start = self.create_message_start_event();
        if let Some(event) = self.state_manager.handle_message_start(msg_start) {
            events.push(event);
        }

        events
    }

    /// 处理 Kiro 事件并转换为 Anthropic SSE 事件
    pub fn process_kiro_event(&mut self, event: &Event) -> Vec<SseEvent> {
        match event {
            Event::InitialResponse { conversation_id } => {
                // 服务端首帧含 server-authoritative conversationId。当前
                // 我们已经用客户端派生 ID（converter.rs::convert_request），
                // 这里只用作可观测性 — 把上游 ID 落到 trace 便于关联日志。
                if !conversation_id.is_empty() {
                    tracing::debug!(
                        server_conversation_id = %conversation_id,
                        "收到 initialResponseEvent"
                    );
                }
                Vec::new()
            }
            Event::ReasoningContent(reasoning) => self.process_reasoning_content(reasoning),
            Event::AssistantResponse(resp) => {
                // 切换到 text 流时关闭 reasoning 块（先 signature_delta 再 stop）
                let mut events = self.close_reasoning_if_open();
                events.extend(self.process_assistant_response(&resp.content));
                events
            }
            Event::ToolUse(tool_use) => {
                let mut events = self.close_reasoning_if_open();
                events.extend(self.process_tool_use(tool_use));
                events
            }
            Event::ContextUsage(context_usage) => {
                // 从上下文使用百分比计算实际的 input_tokens
                let context_window = super::types::get_context_window_size(&self.model) as f64;
                let actual_input_tokens =
                    (context_usage.context_usage_percentage * context_window / 100.0) as i32;
                self.context_input_tokens = Some(actual_input_tokens);
                // 上下文使用量达到 100% 时，设置 stop_reason 为 model_context_window_exceeded
                if context_usage.context_usage_percentage >= 100.0 {
                    self.state_manager
                        .set_stop_reason("model_context_window_exceeded");
                }
                tracing::debug!(
                    "收到 contextUsageEvent: {:.4}%, 计算 input_tokens: {} (context_window: {})",
                    context_usage.context_usage_percentage,
                    actual_input_tokens,
                    context_window as i32
                );
                Vec::new()
            }
            Event::Metering(metering) => {
                self.metering = Some(metering.clone());
                tracing::debug!(
                    usage = metering.usage,
                    unit = %metering.unit,
                    unit_plural = %metering.unit_plural,
                    "收到 meteringEvent"
                );
                Vec::new()
            }
            Event::Error {
                error_code,
                error_message,
            } => {
                tracing::error!("收到错误事件: {} - {}", error_code, error_message);
                // 上游业务错误 → 转 Anthropic SSE error event + 标记 stop_reason
                // 让客户端能感知（throttling、content filter 等）。
                self.state_manager.set_stop_reason("error");
                vec![SseEvent::new(
                    "error",
                    json!({
                        "type": "error",
                        "error": {
                            "type": error_code,
                            "message": error_message,
                        }
                    }),
                )]
            }
            Event::Exception {
                exception_type,
                message,
            } => {
                // 处理 ContentLengthExceededException
                if exception_type == "ContentLengthExceededException" {
                    self.state_manager.set_stop_reason("max_tokens");
                    tracing::warn!("收到异常事件: {} - {}", exception_type, message);
                    return Vec::new();
                }
                tracing::warn!("收到异常事件: {} - {}", exception_type, message);
                // 其他 exception → 也转 SSE error event 并标记 stop_reason=error
                self.state_manager.set_stop_reason("error");
                vec![SseEvent::new(
                    "error",
                    json!({
                        "type": "error",
                        "error": {
                            "type": exception_type,
                            "message": message,
                        }
                    }),
                )]
            }
            _ => Vec::new(),
        }
    }

    /// 处理 reasoningContentEvent — Q 上游对 thinking 模型推送的独立推理流。
    ///
    /// 与老式嵌入 `<thinking>` 标签的 assistantResponseEvent 路径**互斥**：
    /// 实际生产中只会走一条路径（新模型走 reasoningContentEvent，旧 assistant
    /// 文本流走 process_content_with_thinking）。
    ///
    /// Anthropic SSE 协议要求：
    ///   content_block_start (thinking) → thinking_delta* → signature_delta → content_block_stop
    fn process_reasoning_content(
        &mut self,
        reasoning: &crate::kiro::model::events::ReasoningContentEvent,
    ) -> Vec<SseEvent> {
        let mut events = Vec::new();

        // 首次进入 reasoning：分配块索引、emit content_block_start
        if !self.reasoning_block_open {
            let index = self.state_manager.next_block_index();
            self.thinking_block_index = Some(index);
            self.reasoning_block_open = true;
            events.extend(self.state_manager.handle_content_block_start(
                index,
                "thinking",
                json!({
                    "type": "content_block_start",
                    "index": index,
                    "content_block": { "type": "thinking", "thinking": "" }
                }),
            ));
        }

        let Some(index) = self.thinking_block_index else {
            return events;
        };

        if !reasoning.text.is_empty() {
            self.output_tokens += estimate_tokens(&reasoning.text);
            if let Some(delta_event) = self.state_manager.handle_content_block_delta(
                index,
                json!({
                    "type": "content_block_delta",
                    "index": index,
                    "delta": { "type": "thinking_delta", "thinking": reasoning.text }
                }),
            ) {
                events.push(delta_event);
            }
        }

        // 暂存 signature，在关闭块时一并发出（Anthropic 规范要求 signature_delta
        // 紧接在 content_block_stop 之前）
        if let Some(sig) = &reasoning.signature {
            self.pending_reasoning_signature = Some(sig.clone());
        }

        events
    }

    /// 切换到 text/tool_use 流之前关闭 reasoning 块：
    /// emit signature_delta（若 pending）→ content_block_stop。
    fn close_reasoning_if_open(&mut self) -> Vec<SseEvent> {
        if !self.reasoning_block_open {
            return Vec::new();
        }
        let Some(index) = self.thinking_block_index else {
            self.reasoning_block_open = false;
            return Vec::new();
        };

        let mut events = Vec::new();

        // signature_delta：优先用上游 reasoning event 真实 signature。
        // Anthropic 规范要求字段存在，没有就发空串：
        //   - Round 6 实测：客户端 SDK 接受空串、Kiro 上游接受 200
        //   - 但下一轮 history 回写时上游可能拒（"thinking-cache token 不合法"）
        //   - 真实 reasoningContentEvent 通常带 signature，若未带 → log 警告便于追踪
        let signature = match self.pending_reasoning_signature.take() {
            Some(sig) if !sig.is_empty() => sig,
            _ => {
                tracing::warn!(
                    "reasoning 块关闭时无 signature（上游未在 reasoningContentEvent 中提供），\
                     使用空串占位。下一轮 history 回写该 thinking 块可能被上游拒。"
                );
                String::new()
            }
        };
        if let Some(delta_event) = self.state_manager.handle_content_block_delta(
            index,
            json!({
                "type": "content_block_delta",
                "index": index,
                "delta": { "type": "signature_delta", "signature": signature }
            }),
        ) {
            events.push(delta_event);
        }

        if let Some(stop_event) = self.state_manager.handle_content_block_stop(index) {
            events.push(stop_event);
        }
        self.reasoning_block_open = false;
        // 清掉 thinking_block_index，避免老路径 process_content_with_thinking 误用同一索引
        // （Anthropic 协议允许多 thinking 块，但 index 必须不同；不清空可能让两条路径共用 index）
        self.thinking_block_index = None;
        events
    }

    /// 处理助手响应事件
    fn process_assistant_response(&mut self, content: &str) -> Vec<SseEvent> {
        if content.is_empty() {
            return Vec::new();
        }

        // 估算 tokens
        self.output_tokens += estimate_tokens(content);

        // 如果启用了thinking，需要处理thinking块
        if self.thinking_enabled {
            return self.process_content_with_thinking(content);
        }

        // 非 thinking 模式同样复用统一的 text_delta 发送逻辑，
        // 以便在 tool_use 自动关闭文本块后能够自愈重建新的文本块，避免“吞字”。
        self.create_text_delta_events(content)
    }

    /// 处理包含thinking块的内容
    fn process_content_with_thinking(&mut self, content: &str) -> Vec<SseEvent> {
        let mut events = Vec::new();

        // 将内容添加到缓冲区进行处理
        self.thinking_buffer.push_str(content);

        loop {
            if !self.in_thinking_block && !self.thinking_extracted {
                // 查找 <thinking> 开始标签（跳过被反引号包裹的）
                if let Some(start_pos) = find_real_thinking_start_tag(&self.thinking_buffer) {
                    // 发送 <thinking> 之前的内容作为 text_delta
                    // 注意：如果前面只是空白字符（如 adaptive 模式返回的 \n\n），则跳过，
                    // 避免在 thinking 块之前产生无意义的 text 块导致客户端解析失败
                    let before_thinking = self.thinking_buffer[..start_pos].to_string();
                    if !before_thinking.is_empty() && !before_thinking.trim().is_empty() {
                        events.extend(self.create_text_delta_events(&before_thinking));
                    }

                    // 进入 thinking 块
                    self.in_thinking_block = true;
                    self.strip_thinking_leading_newline = true;
                    self.thinking_buffer =
                        self.thinking_buffer[start_pos + "<thinking>".len()..].to_string();

                    // 创建 thinking 块的 content_block_start 事件
                    let thinking_index = self.state_manager.next_block_index();
                    self.thinking_block_index = Some(thinking_index);
                    let start_events = self.state_manager.handle_content_block_start(
                        thinking_index,
                        "thinking",
                        json!({
                            "type": "content_block_start",
                            "index": thinking_index,
                            "content_block": {
                                "type": "thinking",
                                "thinking": ""
                            }
                        }),
                    );
                    events.extend(start_events);
                } else {
                    // 没有找到 <thinking>，检查是否可能是部分标签
                    // 保留可能是部分标签的内容
                    let target_len = self
                        .thinking_buffer
                        .len()
                        .saturating_sub("<thinking>".len());
                    let safe_len = floor_char_boundary(&self.thinking_buffer, target_len);
                    if safe_len > 0 {
                        let safe_content = self.thinking_buffer[..safe_len].to_string();
                        // 如果 thinking 尚未提取，且安全内容只是空白字符，
                        // 则不发送为 text_delta，继续保留在缓冲区等待更多内容。
                        // 这避免了 4.6 模型中 <thinking> 标签跨事件分割时，
                        // 前导空白（如 "\n\n"）被错误地创建为 text 块，
                        // 导致 text 块先于 thinking 块出现的问题。
                        if !safe_content.is_empty() && !safe_content.trim().is_empty() {
                            events.extend(self.create_text_delta_events(&safe_content));
                            self.thinking_buffer = self.thinking_buffer[safe_len..].to_string();
                        }
                    }
                    break;
                }
            } else if self.in_thinking_block {
                // 剥离 <thinking> 标签后紧跟的换行符（可能跨 chunk）
                if self.strip_thinking_leading_newline {
                    if self.thinking_buffer.starts_with('\n') {
                        self.thinking_buffer = self.thinking_buffer[1..].to_string();
                        self.strip_thinking_leading_newline = false;
                    } else if !self.thinking_buffer.is_empty() {
                        // buffer 非空但不以 \n 开头，不再需要剥离
                        self.strip_thinking_leading_newline = false;
                    }
                    // buffer 为空时保留标志，等待下一个 chunk
                }

                // 在 thinking 块内，查找 </thinking> 结束标签（跳过被反引号包裹的）
                if let Some(end_pos) = find_real_thinking_end_tag(&self.thinking_buffer) {
                    // 提取 thinking 内容
                    let thinking_content = self.thinking_buffer[..end_pos].to_string();
                    if let Some(thinking_index) = self.thinking_block_index
                        && !thinking_content.is_empty()
                    {
                        events.push(
                            self.create_thinking_delta_event(thinking_index, &thinking_content),
                        );
                    }

                    // 结束 thinking 块
                    self.in_thinking_block = false;
                    self.thinking_extracted = true;

                    // 发送空的 thinking_delta 事件，然后发送 content_block_stop 事件
                    if let Some(thinking_index) = self.thinking_block_index {
                        // 先发送空的 thinking_delta
                        events.push(self.create_thinking_delta_event(thinking_index, ""));
                        // 在 content_block_stop 之前发 signature_delta（Anthropic 规范）
                        events.push(self.create_thinking_signature_event(thinking_index));
                        // 再发送 content_block_stop
                        if let Some(stop_event) =
                            self.state_manager.handle_content_block_stop(thinking_index)
                        {
                            events.push(stop_event);
                        }
                    }

                    // 剥离 `</thinking>\n\n`（find_real_thinking_end_tag 已确认 \n\n 存在）
                    self.thinking_buffer =
                        self.thinking_buffer[end_pos + "</thinking>\n\n".len()..].to_string();
                } else {
                    // 没有找到结束标签，发送当前缓冲区内容作为 thinking_delta。
                    // 保留末尾可能是部分 `</thinking>\n\n` 的内容：
                    // find_real_thinking_end_tag 要求标签后有 `\n\n` 才返回 Some，
                    // 因此保留区必须覆盖 `</thinking>\n\n` 的完整长度（13 字节），
                    // 否则当 `</thinking>` 已在 buffer 但 `\n\n` 尚未到达时，
                    // 标签的前几个字符会被错误地作为 thinking_delta 发出。
                    let target_len = self
                        .thinking_buffer
                        .len()
                        .saturating_sub("</thinking>\n\n".len());
                    let safe_len = floor_char_boundary(&self.thinking_buffer, target_len);
                    if safe_len > 0 {
                        let safe_content = self.thinking_buffer[..safe_len].to_string();
                        if let Some(thinking_index) = self.thinking_block_index
                            && !safe_content.is_empty()
                        {
                            events.push(
                                self.create_thinking_delta_event(thinking_index, &safe_content),
                            );
                        }
                        self.thinking_buffer = self.thinking_buffer[safe_len..].to_string();
                    }
                    break;
                }
            } else {
                // thinking 已提取完成，剩余内容作为 text_delta
                if !self.thinking_buffer.is_empty() {
                    let remaining = self.thinking_buffer.clone();
                    self.thinking_buffer.clear();
                    events.extend(self.create_text_delta_events(&remaining));
                }
                break;
            }
        }

        events
    }

    /// 创建 text_delta 事件
    ///
    /// 如果文本块尚未创建，会先创建文本块。
    /// 当发生 tool_use 时，状态机会自动关闭当前文本块；后续文本会自动创建新的文本块继续输出。
    ///
    /// 返回值包含可能的 content_block_start 事件和 content_block_delta 事件。
    fn create_text_delta_events(&mut self, text: &str) -> Vec<SseEvent> {
        let mut events = Vec::new();

        // 如果当前 text_block_index 指向的块已经被关闭（例如 tool_use 开始时自动 stop），
        // 则丢弃该索引并创建新的文本块继续输出，避免 delta 被状态机拒绝导致"吞字"。
        if let Some(idx) = self.text_block_index
            && !self.state_manager.is_block_open_of_type(idx, "text")
        {
            self.text_block_index = None;
        }

        // 获取或创建文本块索引
        let text_index = if let Some(idx) = self.text_block_index {
            idx
        } else {
            // 文本块尚未创建，需要先创建
            let idx = self.state_manager.next_block_index();
            self.text_block_index = Some(idx);

            // 发送 content_block_start 事件
            let start_events = self.state_manager.handle_content_block_start(
                idx,
                "text",
                json!({
                    "type": "content_block_start",
                    "index": idx,
                    "content_block": {
                        "type": "text",
                        "text": ""
                    }
                }),
            );
            events.extend(start_events);
            idx
        };

        // 发送 content_block_delta 事件
        if let Some(delta_event) = self.state_manager.handle_content_block_delta(
            text_index,
            json!({
                "type": "content_block_delta",
                "index": text_index,
                "delta": {
                    "type": "text_delta",
                    "text": text
                }
            }),
        ) {
            events.push(delta_event);
        }

        events
    }

    /// 创建 thinking_delta 事件
    fn create_thinking_delta_event(&self, index: i32, thinking: &str) -> SseEvent {
        SseEvent::new(
            "content_block_delta",
            json!({
                "type": "content_block_delta",
                "index": index,
                "delta": {
                    "type": "thinking_delta",
                    "thinking": thinking
                }
            }),
        )
    }

    /// 创建 signature_delta 事件（thinking 块关闭前发送）。
    ///
    /// Anthropic Messages 规范要求 `thinking` content block 在 `content_block_stop`
    /// 之前发一个 `signature_delta`，客户端 SDK 会把它原样写回 history。
    ///
    /// **Round 6 实测结论**（2026-05-13）：
    /// - 空字符串 `""`：Anthropic SDK 接受、Kiro 上游接受（HTTP 200）、客户端 schema 校验通过
    /// - 缺字段：上游 HTTP 502 — 所以字段**必须存在**
    /// - 任意伪造签名（含 SHA-256(thinking) / stub 占位）：Kiro 上游不识别为合法
    ///   thinking-cache token，`cache_read_input_tokens` 仍为 0。
    ///
    /// 因此空字符串是最优解：满足 schema、零 CPU 开销、与伪造方案功能等价。
    /// 真正的 thinking-level cache 需要 Kiro 上游回传合法 signature 才能启用，
    /// 这是上游未实现的特性，不是 proxy 能补的。
    fn create_thinking_signature_event(&self, index: i32) -> SseEvent {
        SseEvent::new(
            "content_block_delta",
            json!({
                "type": "content_block_delta",
                "index": index,
                "delta": {
                    "type": "signature_delta",
                    "signature": ""
                }
            }),
        )
    }

    /// 处理工具使用事件
    fn process_tool_use(
        &mut self,
        tool_use: &crate::kiro::model::events::ToolUseEvent,
    ) -> Vec<SseEvent> {
        let mut events = Vec::new();

        self.state_manager.set_has_tool_use(true);

        // tool_use 必须发生在 thinking 结束之后。
        // 但当 `</thinking>` 后面没有 `\n\n`（例如紧跟 tool_use 或流结束）时，
        // thinking 结束标签会滞留在 thinking_buffer，导致后续 flush 时把 `</thinking>` 当作内容输出。
        // 这里在开始 tool_use block 前做一次"边界场景"的结束标签识别与过滤。
        if self.thinking_enabled
            && self.in_thinking_block
            && let Some(end_pos) = find_real_thinking_end_tag_at_buffer_end(&self.thinking_buffer)
        {
            let thinking_content = self.thinking_buffer[..end_pos].to_string();
            if let Some(thinking_index) = self.thinking_block_index
                && !thinking_content.is_empty()
            {
                events.push(self.create_thinking_delta_event(thinking_index, &thinking_content));
            }

            // 结束 thinking 块
            self.in_thinking_block = false;
            self.thinking_extracted = true;

            if let Some(thinking_index) = self.thinking_block_index {
                // 先发送空的 thinking_delta
                events.push(self.create_thinking_delta_event(thinking_index, ""));
                // 在 content_block_stop 之前发 signature_delta（Anthropic 规范）
                events.push(self.create_thinking_signature_event(thinking_index));
                // 再发送 content_block_stop
                if let Some(stop_event) =
                    self.state_manager.handle_content_block_stop(thinking_index)
                {
                    events.push(stop_event);
                }
            }

            // 把结束标签后的内容当作普通文本（通常为空或空白）
            let after_pos = end_pos + "</thinking>".len();
            let remaining = self.thinking_buffer[after_pos..].trim_start().to_string();
            self.thinking_buffer.clear();
            if !remaining.is_empty() {
                events.extend(self.create_text_delta_events(&remaining));
            }
        }

        // thinking 模式下，process_content_with_thinking 可能会为了探测 `<thinking>` 而暂存一小段尾部文本。
        // 如果此时直接开始 tool_use，状态机会自动关闭 text block，导致这段"待输出文本"看起来被 tool_use 吞掉。
        // 约束：只在尚未进入 thinking block、且 thinking 尚未被提取时，将缓冲区当作普通文本 flush。
        if self.thinking_enabled
            && !self.in_thinking_block
            && !self.thinking_extracted
            && !self.thinking_buffer.is_empty()
        {
            let buffered = std::mem::take(&mut self.thinking_buffer);
            events.extend(self.create_text_delta_events(&buffered));
        }

        // 获取或分配块索引
        let block_index = if let Some(&idx) = self.tool_block_indices.get(&tool_use.tool_use_id) {
            idx
        } else {
            let idx = self.state_manager.next_block_index();
            self.tool_block_indices
                .insert(tool_use.tool_use_id.clone(), idx);
            idx
        };

        // 还原工具名称（如果有映射）
        let original_name = self
            .tool_name_map
            .get(&tool_use.name)
            .cloned()
            .unwrap_or_else(|| tool_use.name.clone());

        // 发送 content_block_start
        let start_events = self.state_manager.handle_content_block_start(
            block_index,
            "tool_use",
            json!({
                "type": "content_block_start",
                "index": block_index,
                "content_block": {
                    "type": "tool_use",
                    "id": tool_use.tool_use_id,
                    "name": original_name,
                    "input": {}
                }
            }),
        );
        events.extend(start_events);

        // 发送参数增量 (ToolUseEvent.input 是 String 类型)
        if !tool_use.input.is_empty() {
            self.output_tokens += (tool_use.input.len() as i32 + 3) / 4; // 估算 token

            if let Some(delta_event) = self.state_manager.handle_content_block_delta(
                block_index,
                json!({
                    "type": "content_block_delta",
                    "index": block_index,
                    "delta": {
                        "type": "input_json_delta",
                        "partial_json": tool_use.input
                    }
                }),
            ) {
                events.push(delta_event);
            }
        }

        // 如果是完整的工具调用（stop=true），发送 content_block_stop
        if tool_use.stop
            && let Some(stop_event) = self.state_manager.handle_content_block_stop(block_index)
        {
            events.push(stop_event);
        }

        events
    }

    /// 生成最终事件序列
    pub fn generate_final_events(&mut self) -> Vec<SseEvent> {
        let mut events = Vec::new();

        // 如果只有 reasoning 内容（没有后续 text/tool_use），在流结束前关闭 reasoning 块
        events.extend(self.close_reasoning_if_open());

        // Flush thinking_buffer 中的剩余内容
        if self.thinking_enabled && !self.thinking_buffer.is_empty() {
            if self.in_thinking_block {
                // 末尾可能残留 `</thinking>`（例如紧跟 tool_use 或流结束），需要在 flush 时过滤掉结束标签。
                if let Some(end_pos) =
                    find_real_thinking_end_tag_at_buffer_end(&self.thinking_buffer)
                {
                    let thinking_content = self.thinking_buffer[..end_pos].to_string();
                    if let Some(thinking_index) = self.thinking_block_index
                        && !thinking_content.is_empty()
                    {
                        events.push(
                            self.create_thinking_delta_event(thinking_index, &thinking_content),
                        );
                    }

                    // 关闭 thinking 块：先发送空的 thinking_delta + signature_delta，再发送 content_block_stop
                    if let Some(thinking_index) = self.thinking_block_index {
                        events.push(self.create_thinking_delta_event(thinking_index, ""));
                        events.push(self.create_thinking_signature_event(thinking_index));
                        if let Some(stop_event) =
                            self.state_manager.handle_content_block_stop(thinking_index)
                        {
                            events.push(stop_event);
                        }
                    }

                    // 把结束标签后的内容当作普通文本（通常为空或空白）
                    let after_pos = end_pos + "</thinking>".len();
                    let remaining = self.thinking_buffer[after_pos..].trim_start().to_string();
                    self.thinking_buffer.clear();
                    self.in_thinking_block = false;
                    self.thinking_extracted = true;
                    if !remaining.is_empty() {
                        events.extend(self.create_text_delta_events(&remaining));
                    }
                } else {
                    // 如果还在 thinking 块内，发送剩余内容作为 thinking_delta
                    if let Some(thinking_index) = self.thinking_block_index {
                        events.push(
                            self.create_thinking_delta_event(thinking_index, &self.thinking_buffer),
                        );
                    }
                    // 关闭 thinking 块：先发送空的 thinking_delta，再发送 content_block_stop
                    if let Some(thinking_index) = self.thinking_block_index {
                        // 先发送空的 thinking_delta
                        events.push(self.create_thinking_delta_event(thinking_index, ""));
                        // 在 content_block_stop 之前发 signature_delta（Anthropic 规范）
                        events.push(self.create_thinking_signature_event(thinking_index));
                        // 再发送 content_block_stop
                        if let Some(stop_event) =
                            self.state_manager.handle_content_block_stop(thinking_index)
                        {
                            events.push(stop_event);
                        }
                    }
                }
            } else {
                // 否则发送剩余内容作为 text_delta
                let buffer_content = self.thinking_buffer.clone();
                events.extend(self.create_text_delta_events(&buffer_content));
            }
            self.thinking_buffer.clear();
        }

        // 如果整个流中只产生了 thinking 块，没有 text 也没有 tool_use，
        // 则设置 stop_reason 为 max_tokens（表示模型耗尽了 token 预算在思考上），
        // 并补发一套完整的 text 事件（内容为一个空格），确保 content 数组中有 text 块
        if self.thinking_enabled
            && self.thinking_block_index.is_some()
            && !self.state_manager.has_non_thinking_blocks()
        {
            self.state_manager.set_stop_reason("max_tokens");
            events.extend(self.create_text_delta_events(" "));
        }

        // 始终基于本地估算输入与 cache 统计来生成 usage，
        // 避免因服务端压缩导致上游 token 统计偏低，使客户端误判上下文大小。
        // credit usage 则仅透传上游 meteringEvent，不影响本地 input/cache usage 语义。
        let final_input_tokens = self.input_tokens;
        let billed_input_tokens = self
            .cache_usage
            .map(|cache_usage| {
                billed_input_tokens(
                    final_input_tokens,
                    cache_usage.cache_creation_input_tokens,
                    cache_usage.cache_read_input_tokens,
                )
            })
            .unwrap_or(final_input_tokens);

        #[cfg(feature = "sensitive-logs")]
        tracing::info!(
            estimated_input_tokens = self.input_tokens,
            context_input_tokens = ?self.context_input_tokens,
            final_input_tokens,
            output_tokens = self.output_tokens,
            "StreamContext usage: final_input_tokens={} (估算值), billed_input_tokens={}, context_input_tokens={} (上游值), output_tokens={}",
            final_input_tokens,
            billed_input_tokens,
            self.context_input_tokens.map_or("N/A".to_string(), |v| v.to_string()),
            self.output_tokens
        );

        // 生成最终事件
        events.extend(self.state_manager.generate_final_events(FinalUsage {
            input_tokens: billed_input_tokens,
            output_tokens: self.output_tokens,
            cache_usage: self.cache_usage,
            metering: self.metering.as_ref(),
        }));
        events
    }
}

/// 将总输入 token 转为 Anthropic usage 的 input_tokens 口径（剔除 cache 读写）
fn billed_input_tokens(
    input_tokens: i32,
    cache_creation_input_tokens: i32,
    cache_read_input_tokens: i32,
) -> i32 {
    input_tokens
        .saturating_sub(cache_creation_input_tokens)
        .saturating_sub(cache_read_input_tokens)
        .max(0)
}

/// 简单的 token 估算
fn estimate_tokens(text: &str) -> i32 {
    let chars: Vec<char> = text.chars().collect();
    let mut chinese_count = 0;
    let mut other_count = 0;

    for c in &chars {
        if *c >= '\u{4E00}' && *c <= '\u{9FFF}' {
            chinese_count += 1;
        } else {
            other_count += 1;
        }
    }

    // 中文约 1.5 字符/token，英文约 4 字符/token
    let chinese_tokens = (chinese_count * 2 + 2) / 3;
    let other_tokens = (other_count + 3) / 4;

    (chinese_tokens + other_tokens).max(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn zero_cache_usage() -> Option<CacheUsageBreakdown> {
        Some(CacheUsageBreakdown {
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
            cache_creation_5m_input_tokens: 0,
            cache_creation_1h_input_tokens: 0,
        })
    }

    fn cache_usage(
        cache_creation_input_tokens: i32,
        cache_read_input_tokens: i32,
        cache_creation_5m_input_tokens: i32,
        cache_creation_1h_input_tokens: i32,
    ) -> Option<CacheUsageBreakdown> {
        Some(CacheUsageBreakdown {
            cache_creation_input_tokens,
            cache_read_input_tokens,
            cache_creation_5m_input_tokens,
            cache_creation_1h_input_tokens,
        })
    }

    /// Round 7 regression: "error" must be the highest-priority stop_reason,
    /// covering end_turn / tool_use / max_tokens / model_context_window_exceeded.
    /// 上游流中断 (handlers.rs Some(Err(e)) 分支) 与 Event::Error/Exception 都靠
    /// 这个优先级覆盖默认 end_turn —— Round 5 patch #11-13 的回归保护。
    #[test]
    fn test_stop_reason_priority_error_wins_over_end_turn() {
        let mut mgr = SseStateManager::new();
        // 先设了 end_turn，再设 error 应该覆盖。
        mgr.set_stop_reason("end_turn");
        mgr.set_stop_reason("error");
        // 模拟 final delta 抽出最终 stop_reason。
        let final_reason = mgr.stop_reason.clone();
        assert_eq!(final_reason.as_deref(), Some("error"));
    }

    #[test]
    fn test_stop_reason_priority_error_wins_over_tool_use_and_max_tokens() {
        for first_set in &["tool_use", "max_tokens", "model_context_window_exceeded"] {
            let mut mgr = SseStateManager::new();
            mgr.set_stop_reason(*first_set);
            mgr.set_stop_reason("error");
            assert_eq!(
                mgr.stop_reason.as_deref(),
                Some("error"),
                "error must override {}",
                first_set
            );
        }
    }

    /// Round 7 regression: Event::Error gets converted to an Anthropic SSE error
    /// event AND set stop_reason="error" — Round 5 patch #13.
    #[test]
    fn test_event_error_emits_sse_error_event() {
        let mut ctx =
            StreamContext::new_with_thinking("claude-sonnet-4", 0, None, false, HashMap::new());
        let events = ctx.process_kiro_event(&Event::Error {
            error_code: "ThrottlingException".to_string(),
            error_message: "Rate limited".to_string(),
        });
        assert_eq!(
            events.len(),
            1,
            "Event::Error should emit one SSE error event"
        );
        assert_eq!(events[0].event, "error");
        let data = &events[0].data;
        assert_eq!(data["type"], "error");
        assert_eq!(data["error"]["type"], "ThrottlingException");
        assert_eq!(data["error"]["message"], "Rate limited");
        assert_eq!(ctx.state_manager.stop_reason.as_deref(), Some("error"));
    }

    /// Round 7 regression: Event::Exception (non ContentLengthExceededException)
    /// also flows through the new "error" SSE path.
    #[test]
    fn test_event_exception_emits_sse_error_event() {
        let mut ctx =
            StreamContext::new_with_thinking("claude-sonnet-4", 0, None, false, HashMap::new());
        let events = ctx.process_kiro_event(&Event::Exception {
            exception_type: "InternalServerException".to_string(),
            message: "boom".to_string(),
        });
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event, "error");
        assert_eq!(events[0].data["error"]["type"], "InternalServerException");
        assert_eq!(ctx.state_manager.stop_reason.as_deref(), Some("error"));
    }

    /// Round 7 regression: ContentLengthExceededException stays on its
    /// "max_tokens" override (NOT mapped to "error"), preserving Round 5's
    /// special-case for the more semantically-precise stop_reason.
    #[test]
    fn test_event_content_length_exceeded_keeps_max_tokens() {
        let mut ctx =
            StreamContext::new_with_thinking("claude-sonnet-4", 0, None, false, HashMap::new());
        let events = ctx.process_kiro_event(&Event::Exception {
            exception_type: "ContentLengthExceededException".to_string(),
            message: "too long".to_string(),
        });
        assert!(
            events.is_empty(),
            "ContentLengthExceededException emits no SSE event"
        );
        assert_eq!(ctx.state_manager.stop_reason.as_deref(), Some("max_tokens"));
    }

    #[test]
    fn test_sse_event_format() {
        let event = SseEvent::new("message_start", json!({"type": "message_start"}));
        let sse_str = event.to_sse_string();

        assert!(sse_str.starts_with("event: message_start\n"));
        assert!(sse_str.contains("data: "));
        assert!(sse_str.ends_with("\n\n"));
    }

    #[test]
    fn test_sse_state_manager_message_start() {
        let mut manager = SseStateManager::new();

        // 第一次应该成功
        let event = manager.handle_message_start(json!({"type": "message_start"}));
        assert!(event.is_some());

        // 第二次应该被跳过
        let event = manager.handle_message_start(json!({"type": "message_start"}));
        assert!(event.is_none());
    }

    #[test]
    fn test_sse_state_manager_block_lifecycle() {
        let mut manager = SseStateManager::new();

        // 创建块
        let events = manager.handle_content_block_start(0, "text", json!({}));
        assert_eq!(events.len(), 1);

        // delta
        let event = manager.handle_content_block_delta(0, json!({}));
        assert!(event.is_some());

        // stop
        let event = manager.handle_content_block_stop(0);
        assert!(event.is_some());

        // 重复 stop 应该被跳过
        let event = manager.handle_content_block_stop(0);
        assert!(event.is_none());
    }

    #[test]
    fn test_stream_context_includes_metering_in_message_delta_usage() {
        let mut ctx = StreamContext::new_with_thinking(
            "test-model",
            123,
            zero_cache_usage(),
            false,
            HashMap::new(),
        );
        let initial_events = ctx.generate_initial_events();
        let mut all_events = initial_events;
        ctx.process_kiro_event(&Event::Metering(MeteringEvent {
            unit: "credit".to_string(),
            unit_plural: "credits".to_string(),
            usage: 0.75,
        }));

        all_events.extend(ctx.generate_final_events());
        let message_start_usage = all_events
            .iter()
            .find(|e| e.event == "message_start")
            .map(|e| e.data["message"]["usage"].clone())
            .expect("message_start should exist");

        let message_delta_usage = all_events
            .iter()
            .find(|e| e.event == "message_delta")
            .map(|e| e.data["usage"].clone())
            .expect("message_delta should exist");

        assert_eq!(message_start_usage["input_tokens"], json!(123));
        assert_eq!(message_start_usage["cache_creation_input_tokens"], json!(0));
        assert_eq!(message_start_usage["cache_read_input_tokens"], json!(0));
        assert_eq!(message_delta_usage["input_tokens"], json!(123));
        assert_eq!(message_delta_usage["cache_creation_input_tokens"], json!(0));
        assert_eq!(message_delta_usage["cache_read_input_tokens"], json!(0));
        assert_eq!(message_delta_usage["credit_usage"], json!(0.75));
        assert_eq!(message_delta_usage["credit_unit"], json!("credit"));
        assert_eq!(message_delta_usage["credit_unit_plural"], json!("credits"));
    }

    #[test]
    fn test_stream_context_includes_cache_usage_fields() {
        let mut ctx = StreamContext::new_with_thinking(
            "test-model",
            321,
            cache_usage(7, 8, 0, 0),
            false,
            HashMap::new(),
        );
        let all_events = ctx.generate_initial_events();

        let message_start_usage = all_events
            .iter()
            .find(|e| e.event == "message_start")
            .map(|e| e.data["message"]["usage"].clone())
            .expect("message_start should exist");

        assert_eq!(message_start_usage["input_tokens"], json!(306));
        assert_eq!(message_start_usage["cache_creation_input_tokens"], json!(7));
        assert_eq!(message_start_usage["cache_read_input_tokens"], json!(8));
    }
    #[test]
    fn test_stream_context_uses_billed_input_tokens_when_cache_read_present() {
        let mut ctx = StreamContext::new_with_thinking(
            "test-model",
            321,
            cache_usage(7, 8, 0, 0),
            false,
            HashMap::new(),
        );
        let initial_events = ctx.generate_initial_events();
        let message_start_usage = initial_events
            .iter()
            .find(|e| e.event == "message_start")
            .map(|e| e.data["message"]["usage"].clone())
            .expect("message_start should exist");

        let final_events = ctx.generate_final_events();
        let message_delta_usage = final_events
            .iter()
            .find(|e| e.event == "message_delta")
            .map(|e| e.data["usage"].clone())
            .expect("message_delta should exist");

        assert_eq!(message_start_usage["input_tokens"], json!(306));
        assert_eq!(message_delta_usage["input_tokens"], json!(306));
        assert_eq!(message_delta_usage["cache_creation_input_tokens"], json!(7));
        assert_eq!(message_delta_usage["cache_read_input_tokens"], json!(8));
    }
    #[test]
    fn test_stream_context_omits_cache_usage_fields_when_disabled() {
        let mut ctx =
            StreamContext::new_with_thinking("test-model", 321, None, false, HashMap::new());
        let initial_events = ctx.generate_initial_events();
        let message_start_usage = initial_events
            .iter()
            .find(|e| e.event == "message_start")
            .map(|e| e.data["message"]["usage"].clone())
            .expect("message_start should exist");

        let final_events = ctx.generate_final_events();
        let message_delta_usage = final_events
            .iter()
            .find(|e| e.event == "message_delta")
            .map(|e| e.data["usage"].clone())
            .expect("message_delta should exist");

        assert_eq!(message_start_usage["input_tokens"], json!(321));
        assert!(
            message_start_usage
                .get("cache_creation_input_tokens")
                .is_none()
        );
        assert!(message_start_usage.get("cache_read_input_tokens").is_none());
        assert_eq!(message_delta_usage["input_tokens"], json!(321));
        assert!(
            message_delta_usage
                .get("cache_creation_input_tokens")
                .is_none()
        );
        assert!(message_delta_usage.get("cache_read_input_tokens").is_none());
        assert!(message_delta_usage.get("cache_creation").is_none());
    }

    #[test]
    fn test_stream_context_extracts_cache_from_metering_event() {
        let mut ctx = StreamContext::new_with_thinking(
            "test-model",
            1000,
            cache_usage(50, 800, 30, 20),
            false,
            HashMap::new(),
        );

        ctx.process_kiro_event(&Event::Metering(MeteringEvent {
            unit: "credit".to_string(),
            unit_plural: "credits".to_string(),
            usage: 0.5,
        }));

        let final_events = ctx.generate_final_events();
        let message_delta_usage = final_events
            .iter()
            .find(|e| e.event == "message_delta")
            .map(|e| e.data["usage"].clone())
            .expect("message_delta should exist");

        assert_eq!(message_delta_usage["cache_read_input_tokens"], json!(800));
        assert_eq!(
            message_delta_usage["cache_creation_input_tokens"],
            json!(50)
        );
        assert_eq!(
            message_delta_usage["cache_creation"]["ephemeral_5m_input_tokens"],
            json!(30)
        );
        assert_eq!(
            message_delta_usage["cache_creation"]["ephemeral_1h_input_tokens"],
            json!(20)
        );
        assert_eq!(message_delta_usage["input_tokens"], json!(150));
    }

    #[test]
    fn test_text_delta_after_tool_use_restarts_text_block() {
        let mut ctx = StreamContext::new_with_thinking(
            "test-model",
            1,
            zero_cache_usage(),
            false,
            HashMap::new(),
        );

        let initial_events = ctx.generate_initial_events();
        assert!(
            initial_events
                .iter()
                .all(|e| !(e.event == "content_block_start"
                    && e.data["content_block"]["type"] == "text"))
        );

        // 首次输出文本时应创建 text block
        let first_text_events = ctx.process_assistant_response("hi");
        let initial_text_index = first_text_events
            .iter()
            .find_map(|e| {
                if e.event == "content_block_start" && e.data["content_block"]["type"] == "text" {
                    e.data["index"].as_i64()
                } else {
                    None
                }
            })
            .expect("text block should start on first text delta")
            as i32;

        // tool_use 开始会自动关闭现有 text block
        let tool_events = ctx.process_tool_use(&crate::kiro::model::events::ToolUseEvent {
            name: "test_tool".to_string(),
            tool_use_id: "tool_1".to_string(),
            input: "{}".to_string(),
            stop: false,
        });
        assert!(
            tool_events.iter().any(|e| {
                e.event == "content_block_stop"
                    && e.data["index"].as_i64() == Some(initial_text_index as i64)
            }),
            "tool_use should stop the previous text block"
        );

        // 之后再来文本增量，应自动创建新的 text block 而不是往已 stop 的块里写 delta
        let text_events = ctx.process_assistant_response("hello");
        let new_text_start_index = text_events.iter().find_map(|e| {
            if e.event == "content_block_start" && e.data["content_block"]["type"] == "text" {
                e.data["index"].as_i64()
            } else {
                None
            }
        });
        assert!(
            new_text_start_index.is_some(),
            "should start a new text block"
        );
        assert_ne!(
            new_text_start_index.unwrap(),
            initial_text_index as i64,
            "new text block index should differ from the stopped one"
        );
        assert!(
            text_events.iter().any(|e| {
                e.event == "content_block_delta"
                    && e.data["delta"]["type"] == "text_delta"
                    && e.data["delta"]["text"] == "hello"
            }),
            "should emit text_delta after restarting text block"
        );
    }

    #[test]
    fn test_tool_use_only_does_not_emit_empty_text_block() {
        // tool_use-only 的流式响应不应产生空 text block（text=""），否则客户端写回 history 会触发上游校验拒绝
        let mut ctx = StreamContext::new_with_thinking(
            "test-model",
            1,
            zero_cache_usage(),
            false,
            HashMap::new(),
        );

        let mut all_events = Vec::new();
        all_events.extend(ctx.generate_initial_events());
        all_events.extend(
            ctx.process_tool_use(&crate::kiro::model::events::ToolUseEvent {
                name: "test_tool".to_string(),
                tool_use_id: "tool_1".to_string(),
                input: "{}".to_string(),
                stop: true,
            }),
        );
        all_events.extend(ctx.generate_final_events());

        assert!(
            all_events.iter().any(|e| {
                e.event == "content_block_start" && e.data["content_block"]["type"] == "tool_use"
            }),
            "should emit tool_use content_block_start"
        );
        assert!(
            all_events.iter().all(|e| {
                !(e.event == "content_block_start" && e.data["content_block"]["type"] == "text")
            }),
            "tool_use-only stream should not start a text block"
        );
    }

    #[test]
    fn test_tool_use_flushes_pending_thinking_buffer_text_before_tool_block() {
        // thinking 模式下，短文本可能被暂存在 thinking_buffer 以等待 `<thinking>` 的跨 chunk 匹配。
        // 当紧接着出现 tool_use 时，应先 flush 这段文本，再开始 tool_use block。
        let mut ctx = StreamContext::new_with_thinking(
            "test-model",
            1,
            zero_cache_usage(),
            true,
            HashMap::new(),
        );
        let _initial_events = ctx.generate_initial_events();

        // 两段短文本（各 2 个中文字符），总长度仍可能不足以满足 safe_len>0 的输出条件，
        // 因而会留在 thinking_buffer 中等待后续 chunk。
        let ev1 = ctx.process_assistant_response("有修");
        assert!(
            ev1.iter().all(|e| e.event != "content_block_delta"),
            "short prefix should be buffered under thinking mode"
        );
        let ev2 = ctx.process_assistant_response("改：");
        assert!(
            ev2.iter().all(|e| e.event != "content_block_delta"),
            "short prefix should still be buffered under thinking mode"
        );

        let events = ctx.process_tool_use(&crate::kiro::model::events::ToolUseEvent {
            name: "Write".to_string(),
            tool_use_id: "tool_1".to_string(),
            input: "{}".to_string(),
            stop: false,
        });

        let text_start_index = events.iter().find_map(|e| {
            if e.event == "content_block_start" && e.data["content_block"]["type"] == "text" {
                e.data["index"].as_i64()
            } else {
                None
            }
        });
        let pos_text_delta = events.iter().position(|e| {
            e.event == "content_block_delta" && e.data["delta"]["type"] == "text_delta"
        });
        let pos_text_stop = text_start_index.and_then(|idx| {
            events.iter().position(|e| {
                e.event == "content_block_stop" && e.data["index"].as_i64() == Some(idx)
            })
        });
        let pos_tool_start = events.iter().position(|e| {
            e.event == "content_block_start" && e.data["content_block"]["type"] == "tool_use"
        });

        assert!(
            text_start_index.is_some(),
            "should start a text block to flush buffered text"
        );
        assert!(
            pos_text_delta.is_some(),
            "should flush buffered text as text_delta"
        );
        assert!(
            pos_text_stop.is_some(),
            "should stop text block before tool_use block starts"
        );
        assert!(pos_tool_start.is_some(), "should start tool_use block");

        let pos_text_delta = pos_text_delta.unwrap();
        let pos_text_stop = pos_text_stop.unwrap();
        let pos_tool_start = pos_tool_start.unwrap();

        assert!(
            pos_text_delta < pos_text_stop && pos_text_stop < pos_tool_start,
            "ordering should be: text_delta -> text_stop -> tool_use_start"
        );

        assert!(
            events.iter().any(|e| {
                e.event == "content_block_delta"
                    && e.data["delta"]["type"] == "text_delta"
                    && e.data["delta"]["text"] == "有修改："
            }),
            "flushed text should equal the buffered prefix"
        );
    }

    #[test]
    fn test_estimate_tokens() {
        assert!(estimate_tokens("Hello") > 0);
        assert!(estimate_tokens("你好") > 0);
        assert!(estimate_tokens("Hello 你好") > 0);
    }

    #[test]
    fn test_find_real_thinking_start_tag_basic() {
        // 基本情况：正常的开始标签
        assert_eq!(find_real_thinking_start_tag("<thinking>"), Some(0));
        assert_eq!(find_real_thinking_start_tag("prefix<thinking>"), Some(6));
    }

    #[test]
    fn test_find_real_thinking_start_tag_with_backticks() {
        // 被反引号包裹的应该被跳过
        assert_eq!(find_real_thinking_start_tag("`<thinking>`"), None);
        assert_eq!(find_real_thinking_start_tag("use `<thinking>` tag"), None);

        // 先有被包裹的，后有真正的开始标签
        assert_eq!(
            find_real_thinking_start_tag("about `<thinking>` tag<thinking>content"),
            Some(22)
        );
    }

    #[test]
    fn test_find_real_thinking_start_tag_with_quotes() {
        // 被双引号包裹的应该被跳过
        assert_eq!(find_real_thinking_start_tag("\"<thinking>\""), None);
        assert_eq!(find_real_thinking_start_tag("the \"<thinking>\" tag"), None);

        // 被单引号包裹的应该被跳过
        assert_eq!(find_real_thinking_start_tag("'<thinking>'"), None);

        // 混合情况
        assert_eq!(
            find_real_thinking_start_tag("about \"<thinking>\" and '<thinking>' then<thinking>"),
            Some(40)
        );
    }

    #[test]
    fn test_find_real_thinking_end_tag_basic() {
        // 基本情况：正常的结束标签后面有双换行符
        assert_eq!(find_real_thinking_end_tag("</thinking>\n\n"), Some(0));
        assert_eq!(
            find_real_thinking_end_tag("content</thinking>\n\n"),
            Some(7)
        );
        assert_eq!(
            find_real_thinking_end_tag("some text</thinking>\n\nmore text"),
            Some(9)
        );

        // 没有双换行符的情况
        assert_eq!(find_real_thinking_end_tag("</thinking>"), None);
        assert_eq!(find_real_thinking_end_tag("</thinking>\n"), None);
        assert_eq!(find_real_thinking_end_tag("</thinking> more"), None);
    }

    #[test]
    fn test_find_real_thinking_end_tag_with_backticks() {
        // 被反引号包裹的应该被跳过
        assert_eq!(find_real_thinking_end_tag("`</thinking>`\n\n"), None);
        assert_eq!(
            find_real_thinking_end_tag("mention `</thinking>` in code\n\n"),
            None
        );

        // 只有前面有反引号
        assert_eq!(find_real_thinking_end_tag("`</thinking>\n\n"), None);

        // 只有后面有反引号
        assert_eq!(find_real_thinking_end_tag("</thinking>`\n\n"), None);
    }

    #[test]
    fn test_find_real_thinking_end_tag_with_quotes() {
        // 被双引号包裹的应该被跳过
        assert_eq!(find_real_thinking_end_tag("\"</thinking>\"\n\n"), None);
        assert_eq!(
            find_real_thinking_end_tag("the string \"</thinking>\" is a tag\n\n"),
            None
        );

        // 被单引号包裹的应该被跳过
        assert_eq!(find_real_thinking_end_tag("'</thinking>'\n\n"), None);
        assert_eq!(
            find_real_thinking_end_tag("use '</thinking>' as marker\n\n"),
            None
        );

        // 混合情况：双引号包裹后有真正的标签
        assert_eq!(
            find_real_thinking_end_tag("about \"</thinking>\" tag</thinking>\n\n"),
            Some(23)
        );

        // 混合情况：单引号包裹后有真正的标签
        assert_eq!(
            find_real_thinking_end_tag("about '</thinking>' tag</thinking>\n\n"),
            Some(23)
        );
    }

    #[test]
    fn test_find_real_thinking_end_tag_mixed() {
        // 先有被包裹的，后有真正的结束标签
        assert_eq!(
            find_real_thinking_end_tag("discussing `</thinking>` tag</thinking>\n\n"),
            Some(28)
        );

        // 多个被包裹的，最后一个是真正的
        assert_eq!(
            find_real_thinking_end_tag("`</thinking>` and `</thinking>` done</thinking>\n\n"),
            Some(36)
        );

        // 多种引用字符混合
        assert_eq!(
            find_real_thinking_end_tag(
                "`</thinking>` and \"</thinking>\" and '</thinking>' done</thinking>\n\n"
            ),
            Some(54)
        );
    }

    #[test]
    fn test_tool_use_immediately_after_thinking_filters_end_tag_and_closes_thinking_block() {
        let mut ctx = StreamContext::new_with_thinking(
            "test-model",
            1,
            zero_cache_usage(),
            true,
            HashMap::new(),
        );
        let _initial_events = ctx.generate_initial_events();

        let mut all_events = Vec::new();

        // thinking 内容以 `</thinking>` 结尾，但后面没有 `\n\n`（模拟紧跟 tool_use 的场景）
        all_events.extend(ctx.process_assistant_response("<thinking>abc</thinking>"));

        let tool_events = ctx.process_tool_use(&crate::kiro::model::events::ToolUseEvent {
            name: "Write".to_string(),
            tool_use_id: "tool_1".to_string(),
            input: "{}".to_string(),
            stop: false,
        });
        all_events.extend(tool_events);

        all_events.extend(ctx.generate_final_events());

        // 不应把 `</thinking>` 当作 thinking 内容输出
        assert!(
            all_events.iter().all(|e| {
                !(e.event == "content_block_delta"
                    && e.data["delta"]["type"] == "thinking_delta"
                    && e.data["delta"]["thinking"] == "</thinking>")
            }),
            "`</thinking>` should be filtered from output"
        );

        // thinking block 必须在 tool_use block 之前关闭
        let thinking_index = ctx
            .thinking_block_index
            .expect("thinking block index should exist");
        let pos_thinking_stop = all_events.iter().position(|e| {
            e.event == "content_block_stop"
                && e.data["index"].as_i64() == Some(thinking_index as i64)
        });
        let pos_tool_start = all_events.iter().position(|e| {
            e.event == "content_block_start" && e.data["content_block"]["type"] == "tool_use"
        });
        assert!(
            pos_thinking_stop.is_some(),
            "thinking block should be stopped"
        );
        assert!(pos_tool_start.is_some(), "tool_use block should be started");
        assert!(
            pos_thinking_stop.unwrap() < pos_tool_start.unwrap(),
            "thinking block should stop before tool_use block starts"
        );
    }

    #[test]
    fn test_final_flush_filters_standalone_thinking_end_tag() {
        let mut ctx = StreamContext::new_with_thinking(
            "test-model",
            1,
            zero_cache_usage(),
            true,
            HashMap::new(),
        );
        let _initial_events = ctx.generate_initial_events();

        let mut all_events = Vec::new();
        all_events.extend(ctx.process_assistant_response("<thinking>abc</thinking>"));
        all_events.extend(ctx.generate_final_events());

        assert!(
            all_events.iter().all(|e| {
                !(e.event == "content_block_delta"
                    && e.data["delta"]["type"] == "thinking_delta"
                    && e.data["delta"]["thinking"] == "</thinking>")
            }),
            "`</thinking>` should be filtered during final flush"
        );
    }

    #[test]
    fn test_thinking_strips_leading_newline_same_chunk() {
        // <thinking>\n 在同一个 chunk 中，\n 应被剥离
        let mut ctx = StreamContext::new_with_thinking(
            "test-model",
            1,
            zero_cache_usage(),
            true,
            HashMap::new(),
        );
        let _initial_events = ctx.generate_initial_events();

        let events = ctx.process_assistant_response("<thinking>\nHello world");

        // 找到所有 thinking_delta 事件
        let thinking_deltas: Vec<_> = events
            .iter()
            .filter(|e| {
                e.event == "content_block_delta" && e.data["delta"]["type"] == "thinking_delta"
            })
            .collect();

        // 拼接所有 thinking 内容
        let full_thinking: String = thinking_deltas
            .iter()
            .map(|e| e.data["delta"]["thinking"].as_str().unwrap_or(""))
            .collect();

        assert!(
            !full_thinking.starts_with('\n'),
            "thinking content should not start with \\n, got: {:?}",
            full_thinking
        );
    }

    #[test]
    fn test_thinking_strips_leading_newline_cross_chunk() {
        // <thinking> 在第一个 chunk 末尾，\n 在第二个 chunk 开头
        let mut ctx = StreamContext::new_with_thinking(
            "test-model",
            1,
            zero_cache_usage(),
            true,
            HashMap::new(),
        );
        let _initial_events = ctx.generate_initial_events();

        let events1 = ctx.process_assistant_response("<thinking>");
        let events2 = ctx.process_assistant_response("\nHello world");

        let mut all_events = Vec::new();
        all_events.extend(events1);
        all_events.extend(events2);

        let thinking_deltas: Vec<_> = all_events
            .iter()
            .filter(|e| {
                e.event == "content_block_delta" && e.data["delta"]["type"] == "thinking_delta"
            })
            .collect();

        let full_thinking: String = thinking_deltas
            .iter()
            .map(|e| e.data["delta"]["thinking"].as_str().unwrap_or(""))
            .collect();

        assert!(
            !full_thinking.starts_with('\n'),
            "thinking content should not start with \\n across chunks, got: {:?}",
            full_thinking
        );
    }

    #[test]
    fn test_thinking_no_strip_when_no_leading_newline() {
        // <thinking> 后直接跟内容（无 \n），内容应完整保留
        let mut ctx = StreamContext::new_with_thinking(
            "test-model",
            1,
            zero_cache_usage(),
            true,
            HashMap::new(),
        );
        let _initial_events = ctx.generate_initial_events();

        let events = ctx.process_assistant_response("<thinking>abc</thinking>\n\ntext");

        let thinking_deltas: Vec<_> = events
            .iter()
            .filter(|e| {
                e.event == "content_block_delta" && e.data["delta"]["type"] == "thinking_delta"
            })
            .collect();

        let full_thinking: String = thinking_deltas
            .iter()
            .filter(|e| {
                !e.data["delta"]["thinking"]
                    .as_str()
                    .unwrap_or("")
                    .is_empty()
            })
            .map(|e| e.data["delta"]["thinking"].as_str().unwrap_or(""))
            .collect();

        assert_eq!(full_thinking, "abc", "thinking content should be 'abc'");
    }

    #[test]
    fn test_text_after_thinking_strips_leading_newlines() {
        // `</thinking>\n\n` 后的文本不应以 \n\n 开头
        let mut ctx = StreamContext::new_with_thinking(
            "test-model",
            1,
            zero_cache_usage(),
            true,
            HashMap::new(),
        );
        let _initial_events = ctx.generate_initial_events();

        let events = ctx.process_assistant_response("<thinking>\nabc</thinking>\n\n你好");

        let text_deltas: Vec<_> = events
            .iter()
            .filter(|e| e.event == "content_block_delta" && e.data["delta"]["type"] == "text_delta")
            .collect();

        let full_text: String = text_deltas
            .iter()
            .map(|e| e.data["delta"]["text"].as_str().unwrap_or(""))
            .collect();

        assert!(
            !full_text.starts_with('\n'),
            "text after thinking should not start with \\n, got: {:?}",
            full_text
        );
        assert_eq!(full_text, "你好");
    }

    /// 辅助函数：从事件列表中提取所有 thinking_delta 的拼接内容
    fn collect_thinking_content(events: &[SseEvent]) -> String {
        events
            .iter()
            .filter(|e| {
                e.event == "content_block_delta" && e.data["delta"]["type"] == "thinking_delta"
            })
            .map(|e| e.data["delta"]["thinking"].as_str().unwrap_or(""))
            .filter(|s| !s.is_empty())
            .collect()
    }

    /// 辅助函数：从事件列表中提取所有 text_delta 的拼接内容
    fn collect_text_content(events: &[SseEvent]) -> String {
        events
            .iter()
            .filter(|e| e.event == "content_block_delta" && e.data["delta"]["type"] == "text_delta")
            .map(|e| e.data["delta"]["text"].as_str().unwrap_or(""))
            .collect()
    }

    #[test]
    fn test_end_tag_newlines_split_across_events() {
        // `</thinking>\n` 在 chunk 1，`\n` 在 chunk 2，`text` 在 chunk 3
        // 确保 `</thinking>` 不会被部分当作 thinking 内容发出
        let mut ctx = StreamContext::new_with_thinking(
            "test-model",
            1,
            zero_cache_usage(),
            true,
            HashMap::new(),
        );
        let _initial_events = ctx.generate_initial_events();

        let mut all = Vec::new();
        all.extend(ctx.process_assistant_response("<thinking>\nabc</thinking>\n"));
        all.extend(ctx.process_assistant_response("\n"));
        all.extend(ctx.process_assistant_response("你好"));
        all.extend(ctx.generate_final_events());

        let thinking = collect_thinking_content(&all);
        assert_eq!(
            thinking, "abc",
            "thinking should be 'abc', got: {:?}",
            thinking
        );

        let text = collect_text_content(&all);
        assert_eq!(text, "你好", "text should be '你好', got: {:?}", text);
    }

    #[test]
    fn test_end_tag_alone_in_chunk_then_newlines_in_next() {
        // `</thinking>` 单独在一个 chunk，`\n\ntext` 在下一个 chunk
        let mut ctx = StreamContext::new_with_thinking(
            "test-model",
            1,
            zero_cache_usage(),
            true,
            HashMap::new(),
        );
        let _initial_events = ctx.generate_initial_events();

        let mut all = Vec::new();
        all.extend(ctx.process_assistant_response("<thinking>\nabc</thinking>"));
        all.extend(ctx.process_assistant_response("\n\n你好"));
        all.extend(ctx.generate_final_events());

        let thinking = collect_thinking_content(&all);
        assert_eq!(
            thinking, "abc",
            "thinking should be 'abc', got: {:?}",
            thinking
        );

        let text = collect_text_content(&all);
        assert_eq!(text, "你好", "text should be '你好', got: {:?}", text);
    }

    #[test]
    fn test_start_tag_newline_split_across_events() {
        // `\n\n` 在 chunk 1，`<thinking>` 在 chunk 2，`\n` 在 chunk 3
        let mut ctx = StreamContext::new_with_thinking(
            "test-model",
            1,
            zero_cache_usage(),
            true,
            HashMap::new(),
        );
        let _initial_events = ctx.generate_initial_events();

        let mut all = Vec::new();
        all.extend(ctx.process_assistant_response("\n\n"));
        all.extend(ctx.process_assistant_response("<thinking>"));
        all.extend(ctx.process_assistant_response("\n"));
        all.extend(ctx.process_assistant_response("abc</thinking>\n\ntext"));
        all.extend(ctx.generate_final_events());

        let thinking = collect_thinking_content(&all);
        assert_eq!(
            thinking, "abc",
            "thinking should be 'abc', got: {:?}",
            thinking
        );

        let text = collect_text_content(&all);
        assert_eq!(text, "text", "text should be 'text', got: {:?}", text);
    }

    #[test]
    fn test_full_flow_maximally_split() {
        // 极端拆分：每个关键边界都在不同 chunk
        let mut ctx = StreamContext::new_with_thinking(
            "test-model",
            1,
            zero_cache_usage(),
            true,
            HashMap::new(),
        );
        let _initial_events = ctx.generate_initial_events();

        let mut all = Vec::new();
        // \n\n<thinking>\n 拆成多段
        all.extend(ctx.process_assistant_response("\n"));
        all.extend(ctx.process_assistant_response("\n"));
        all.extend(ctx.process_assistant_response("<thin"));
        all.extend(ctx.process_assistant_response("king>"));
        all.extend(ctx.process_assistant_response("\n"));
        all.extend(ctx.process_assistant_response("hello"));
        // </thinking>\n\n 拆成多段
        all.extend(ctx.process_assistant_response("</thi"));
        all.extend(ctx.process_assistant_response("nking>"));
        all.extend(ctx.process_assistant_response("\n"));
        all.extend(ctx.process_assistant_response("\n"));
        all.extend(ctx.process_assistant_response("world"));
        all.extend(ctx.generate_final_events());

        let thinking = collect_thinking_content(&all);
        assert_eq!(
            thinking, "hello",
            "thinking should be 'hello', got: {:?}",
            thinking
        );

        let text = collect_text_content(&all);
        assert_eq!(text, "world", "text should be 'world', got: {:?}", text);
    }

    #[test]
    fn test_thinking_only_sets_max_tokens_stop_reason() {
        // 整个流只有 thinking 块，没有 text 也没有 tool_use，stop_reason 应为 max_tokens
        let mut ctx = StreamContext::new_with_thinking(
            "test-model",
            1,
            zero_cache_usage(),
            true,
            HashMap::new(),
        );
        let _initial_events = ctx.generate_initial_events();

        let mut all_events = Vec::new();
        all_events.extend(ctx.process_assistant_response("<thinking>\nabc</thinking>"));
        all_events.extend(ctx.generate_final_events());

        let message_delta = all_events
            .iter()
            .find(|e| e.event == "message_delta")
            .expect("should have message_delta event");

        assert_eq!(
            message_delta.data["delta"]["stop_reason"], "max_tokens",
            "stop_reason should be max_tokens when only thinking is produced"
        );

        // 应补发一套完整的 text 事件（content_block_start + delta 空格 + content_block_stop）
        assert!(
            all_events.iter().any(|e| {
                e.event == "content_block_start" && e.data["content_block"]["type"] == "text"
            }),
            "should emit text content_block_start"
        );
        assert!(
            all_events.iter().any(|e| {
                e.event == "content_block_delta"
                    && e.data["delta"]["type"] == "text_delta"
                    && e.data["delta"]["text"] == " "
            }),
            "should emit text_delta with a single space"
        );
        // text block 应被 generate_final_events 自动关闭
        let text_block_index = all_events
            .iter()
            .find_map(|e| {
                if e.event == "content_block_start" && e.data["content_block"]["type"] == "text" {
                    e.data["index"].as_i64()
                } else {
                    None
                }
            })
            .expect("text block should exist");
        assert!(
            all_events.iter().any(|e| {
                e.event == "content_block_stop"
                    && e.data["index"].as_i64() == Some(text_block_index)
            }),
            "text block should be stopped"
        );
    }

    #[test]
    fn test_thinking_with_text_keeps_end_turn_stop_reason() {
        // thinking + text 的情况，stop_reason 应为 end_turn
        let mut ctx = StreamContext::new_with_thinking(
            "test-model",
            1,
            zero_cache_usage(),
            true,
            HashMap::new(),
        );
        let _initial_events = ctx.generate_initial_events();

        let mut all_events = Vec::new();
        all_events.extend(ctx.process_assistant_response("<thinking>\nabc</thinking>\n\nHello"));
        all_events.extend(ctx.generate_final_events());

        let message_delta = all_events
            .iter()
            .find(|e| e.event == "message_delta")
            .expect("should have message_delta event");

        assert_eq!(
            message_delta.data["delta"]["stop_reason"], "end_turn",
            "stop_reason should be end_turn when text is also produced"
        );
    }

    #[test]
    fn test_thinking_with_tool_use_keeps_tool_use_stop_reason() {
        // thinking + tool_use 的情况，stop_reason 应为 tool_use
        let mut ctx = StreamContext::new_with_thinking(
            "test-model",
            1,
            zero_cache_usage(),
            true,
            HashMap::new(),
        );
        let _initial_events = ctx.generate_initial_events();

        let mut all_events = Vec::new();
        all_events.extend(ctx.process_assistant_response("<thinking>\nabc</thinking>"));
        all_events.extend(
            ctx.process_tool_use(&crate::kiro::model::events::ToolUseEvent {
                name: "test_tool".to_string(),
                tool_use_id: "tool_1".to_string(),
                input: "{}".to_string(),
                stop: true,
            }),
        );
        all_events.extend(ctx.generate_final_events());

        let message_delta = all_events
            .iter()
            .find(|e| e.event == "message_delta")
            .expect("should have message_delta event");

        assert_eq!(
            message_delta.data["delta"]["stop_reason"], "tool_use",
            "stop_reason should be tool_use when tool_use is present"
        );
    }
}
