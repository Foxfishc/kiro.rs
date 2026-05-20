//! Anthropic → Kiro 协议转换器
//!
//! 负责将 Anthropic API 请求格式转换为 Kiro API 请求格式

use std::collections::HashMap;

use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::image::{process_gif_frames, process_image, process_image_to_format};
use crate::kiro::model::requests::conversation::{
    AssistantMessage, ConversationState, CurrentMessage, HistoryAssistantMessage,
    HistoryUserMessage, KiroImage, Message, UserInputMessage, UserInputMessageContext, UserMessage,
};
use crate::kiro::model::requests::tool::{
    InputSchema, Tool as KiroTool, ToolResult, ToolSpecification, ToolUseEntry,
};

use super::types::{ContentBlock, MessagesRequest, Tool as AnthropicTool};
use crate::anthropic::compressor::CompressionStats;
use crate::model::config::CompressionConfig;

/// 单请求图片总数上限（所有消息合计，含 GIF 抽帧后的帧数）
const MAX_TOTAL_IMAGES: usize = 20;

/// kiro-cli 2.3.0 wire-aligned system-injection ack (Q_LOG_LEVEL=trace
/// 2026-05-12 capture, history[1].assistantResponseMessage.content). Must
/// match byte-for-byte — it's part of the server's prefix-cache key.
const SYSTEM_ACK: &str = "I will fully incorporate this information when generating my responses, and explicitly acknowledge relevant parts of the summary when answering questions.";

/// Wrap a system prompt for history injection the same way kiro-cli does:
/// empty CONTEXT ENTRY block + a blank line + `Follow this instruction: <text>`.
/// kiro-cli 2.3.0 wire literal: `"--- CONTEXT ENTRY BEGIN ---\n--- CONTEXT
/// ENTRY END ---\n\nFollow this instruction: <text>"`.
///
/// 防御性：若 `text` 已含 `--- CONTEXT ENTRY BEGIN ---` 或 `Follow this instruction:`
/// 前缀（例如用户直接把上一轮 wire 透传，或上游 caller 已经 wrap 过），不重复包装，
/// 避免 `Follow this instruction: Follow this instruction:` 双层破坏 prefix cache key。
fn wrap_system_for_history(text: &str) -> String {
    let trimmed = text.trim_start();
    if trimmed.starts_with("--- CONTEXT ENTRY BEGIN ---")
        || trimmed.starts_with("Follow this instruction:")
    {
        return text.to_string();
    }
    format!(
        "--- CONTEXT ENTRY BEGIN ---\n--- CONTEXT ENTRY END ---\n\nFollow this instruction: {text}"
    )
}

/// 规范化 JSON Schema，修复 MCP 工具定义中常见的类型问题
///
/// Claude Code / MCP 工具定义偶尔会出现 `required: null`、`properties: null` 等，
/// 导致上游返回 400 "Improperly formed request"（参见
/// `docs/troubleshooting/400-improperly-formed-request.md` TC-07）。
///
/// **Round 6 灰度结论**（2026-05-13）: `$schema` 和 `additionalProperties` 这两
/// 个字段的"缺失补默认"逻辑被证实**没有任何**避免 400 的实际作用 —— 实测三种
/// schema 形态（正常 / 空 properties / 缺 type）裁剪后上游均 200。troubleshooting
/// 文档归因的 400 触发点都跟这两个字段无关。
///
/// 因此改为：`$schema` 和 `additionalProperties` **缺失时不补**（与 kiro-cli 2.3.0
/// wire 字节对齐，每工具节约 ~80B 并消除 prefix-cache key 偏移）；仅在字段存在
/// 但形态非法时纠正。`type` / `properties` / `required` 三个字段是 TC-07 真正治理
/// 的目标，**保留**缺失补默认。
fn normalize_json_schema(schema: serde_json::Value) -> serde_json::Value {
    let serde_json::Value::Object(mut obj) = schema else {
        // 整体非 object → 兜底空 schema。不补 $schema/additionalProperties。
        return serde_json::json!({
            "type": "object",
            "properties": {},
            "required": [],
        });
    };

    // $schema：缺失不补；存在但非合法非空字符串才纠正。
    if let Some(v) = obj.get("$schema")
        && v.as_str().is_none_or(|s| s.is_empty())
    {
        obj.insert(
            "$schema".to_string(),
            serde_json::Value::String("http://json-schema.org/draft-07/schema#".to_string()),
        );
    }

    // type（必须是字符串）—— 这是 MCP 异常的常见目标，保留缺失补默认。
    if obj
        .get("type")
        .and_then(|v| v.as_str())
        .is_none_or(|s| s.is_empty())
    {
        obj.insert(
            "type".to_string(),
            serde_json::Value::String("object".to_string()),
        );
    }

    // properties（必须是 object）—— 保留 null/缺失兜底。
    match obj.get("properties") {
        Some(serde_json::Value::Object(_)) => {}
        _ => {
            obj.insert(
                "properties".to_string(),
                serde_json::Value::Object(serde_json::Map::new()),
            );
        }
    }

    // required（必须是 string 数组）—— 保留 null/异常元素兜底。
    let required = match obj.remove("required") {
        Some(serde_json::Value::Array(arr)) => serde_json::Value::Array(
            arr.into_iter()
                .filter_map(|v| v.as_str().map(|s| serde_json::Value::String(s.to_string())))
                .collect(),
        ),
        _ => serde_json::Value::Array(Vec::new()),
    };
    obj.insert("required".to_string(), required);

    // additionalProperties：缺失不补；存在但非 bool/object 才纠正为 true。
    if let Some(v) = obj.get("additionalProperties")
        && !v.is_boolean()
        && !v.is_object()
    {
        obj.insert(
            "additionalProperties".to_string(),
            serde_json::Value::Bool(true),
        );
    }

    serde_json::Value::Object(obj)
}

/// 追加到 Write 工具 description 末尾的内容
const WRITE_TOOL_DESCRIPTION_SUFFIX: &str = "- IMPORTANT: If the content to write exceeds 150 lines, you MUST only write the first 50 lines using this tool, then use `Edit` tool to append the remaining content in chunks of no more than 50 lines each. If needed, leave a unique placeholder to help append content. Do NOT attempt to write all content at once.";

/// 追加到 Edit 工具 description 末尾的内容
const EDIT_TOOL_DESCRIPTION_SUFFIX: &str = "- IMPORTANT: If the `new_string` content exceeds 50 lines, you MUST split it into multiple Edit calls, each replacing no more than 50 lines at a time. If used to append content, leave a unique placeholder to help append content. On the final chunk, do NOT include the placeholder.";

/// 追加到系统提示词的分块写入策略
const SYSTEM_CHUNKED_POLICY: &str = "\
When the Write or Edit tool has content size limits, always comply silently. \
Never suggest bypassing these limits via alternative tools. \
Never ask the user whether to switch approaches. \
Complete all chunked operations without commentary.";

/// Agentic 模型专用系统提示
///
/// 指导模型在 agentic 模式下的行为：持续工作、自主决策、减少确认
const KIRO_AGENTIC_SYSTEM_PROMPT: &str = "\
You are an autonomous coding agent. Follow these principles:\n\
1. Work continuously until the task is fully complete.\n\
2. Use tools proactively without asking for permission.\n\
3. When encountering errors, debug and fix them autonomously.\n\
4. Break complex tasks into steps and execute them sequentially.\n\
5. Verify your work by reading files after writing them.\n\
6. Never ask the user for confirmation mid-task — just proceed.\n\
7. If a tool call fails, try alternative approaches before giving up.\n\
8. Prefer making changes directly over explaining what you would do.";

fn non_empty_content_or_space(content: String, has_non_text_payload: bool) -> String {
    // 尽量保留真实结构，不在早期转换阶段为非文本载荷主动补 "."。
    // 含非文本载荷时保留原始文本，最终是否需要兜底由调用方决定。
    if has_non_text_payload {
        return content;
    }
    content
}

/// 统计单个消息内容中的图片数量
fn count_images_in_content(content: &serde_json::Value) -> usize {
    match content {
        serde_json::Value::Array(arr) => arr
            .iter()
            .filter(|item| item.get("type").and_then(|v| v.as_str()) == Some("image"))
            .count(),
        _ => 0,
    }
}

/// Kiro 上游使用的规范模型 ID
const KIRO_MODEL_SONNET_4_5: &str = "claude-sonnet-4.5";
const KIRO_MODEL_SONNET_4_6: &str = "claude-sonnet-4.6";
const KIRO_MODEL_OPUS_4_5: &str = "claude-opus-4.5";
const KIRO_MODEL_OPUS_4_6: &str = "claude-opus-4.6";
const KIRO_MODEL_OPUS_4_7: &str = "claude-opus-4.7";
const KIRO_MODEL_HAIKU_4_5: &str = "claude-haiku-4.5";

fn normalize_model_name(model: &str) -> String {
    let model = model.to_lowercase();
    let model = model.strip_suffix("-thinking").unwrap_or(&model);
    let model = model.strip_suffix("-agentic").unwrap_or(model);
    model.to_string()
}

/// 模型映射：将 Anthropic 模型名映射到 Kiro 模型 ID
///
/// 映射规则：
/// - sonnet 且包含 4.6/4-6 → claude-sonnet-4.6，否则 → claude-sonnet-4.5
/// - opus 且包含 4.5/4-5 → claude-opus-4.5，包含 4.7/4-7 → claude-opus-4.7，否则 → claude-opus-4.6
/// - 所有 haiku → claude-haiku-4.5
/// - `-thinking` / `-agentic` 后缀会被剥离后再映射
pub fn map_model(model: &str) -> Option<String> {
    let normalized_model = normalize_model_name(model);

    if normalized_model.contains("sonnet") {
        if normalized_model.contains("4-6") || normalized_model.contains("4.6") {
            Some(KIRO_MODEL_SONNET_4_6.to_string())
        } else {
            Some(KIRO_MODEL_SONNET_4_5.to_string())
        }
    } else if normalized_model.contains("opus") {
        if normalized_model.contains("4-5") || normalized_model.contains("4.5") {
            Some(KIRO_MODEL_OPUS_4_5.to_string())
        } else if normalized_model.contains("4-7") || normalized_model.contains("4.7") {
            Some(KIRO_MODEL_OPUS_4_7.to_string())
        } else {
            Some(KIRO_MODEL_OPUS_4_6.to_string())
        }
    } else if normalized_model.contains("haiku") {
        Some(KIRO_MODEL_HAIKU_4_5.to_string())
    } else {
        None
    }
}

/// 判断模型名是否为 agentic 变体
pub fn is_agentic_model(model: &str) -> bool {
    model.to_lowercase().ends_with("-agentic")
}

/// 转换结果
#[derive(Debug)]
pub struct ConversionResult {
    /// 转换后的 Kiro 请求
    pub conversation_state: ConversationState,
    /// 压缩统计信息（仅在启用压缩时有值）
    pub compression_stats: Option<CompressionStats>,
    /// 工具名称映射（短名称 → 原始名称），仅当存在超长工具名时非空
    pub tool_name_map: HashMap<String, String>,
}

/// 转换错误
#[derive(Debug)]
pub enum ConversionError {
    UnsupportedModel(String),
    EmptyMessages,
    EmptyMessageContent,
}

impl std::fmt::Display for ConversionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConversionError::UnsupportedModel(model) => write!(f, "模型不支持: {}", model),
            ConversionError::EmptyMessages => write!(f, "消息列表为空"),
            ConversionError::EmptyMessageContent => write!(f, "消息内容为空"),
        }
    }
}

impl std::error::Error for ConversionError {}

/// 从 metadata.user_id 中提取 session UUID
///
/// 支持两种格式：
/// 1. 字符串格式: user_xxx_account__session_0b4445e1-f5be-49e1-87ce-62bbc28ad705
/// 2. JSON 格式: {"device_id":"...","account_uuid":"...","session_id":"UUID"}
///
/// 提取 session UUID 作为 conversationId
fn extract_session_id(user_id: &str) -> Option<String> {
    // 先尝试 JSON 格式解析
    if let Ok(json) = serde_json::from_str::<serde_json::Value>(user_id)
        && let Some(session_id) = json.get("session_id").and_then(|v| v.as_str())
        && is_valid_uuid(session_id)
    {
        return Some(session_id.to_string());
    }

    // 再尝试字符串格式：查找 "session_" 后面的内容
    if let Some(pos) = user_id.find("session_") {
        let session_part = &user_id[pos + 8..]; // "session_" 长度为 8
        // session_part 应该是 UUID 格式: xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx
        // 验证是否是有效的 UUID 格式（36 字符，包含 4 个连字符）
        if session_part.len() >= 36 {
            let uuid_str = &session_part[..36];
            if is_valid_uuid(uuid_str) {
                return Some(uuid_str.to_string());
            }
        }
    }
    None
}

/// 简单验证 UUID 格式（36 字符，包含 4 个连字符）
fn is_valid_uuid(s: &str) -> bool {
    s.len() == 36 && s.chars().filter(|c| *c == '-').count() == 4
}

/// 用**首条 user message** SHA-256 指纹派生跨 turn 稳定的 conversation_id。
///
/// **Round 7 重要更新（2026-05-13）**: 之前实现哈希 `messages[..len-1]`，但这个
/// 切片在每轮多轮里都在膨胀（turn N 切 [0..N-1]，turn N+1 切 [0..N+1]）
/// → SHA-256 必然不同 → conversation_id 每轮变。Round 7 cache 长程实测员
/// 抓了 8 轮 capture，conversationId 8 个不同值，**证实 fingerprint 每轮变**。
///
/// **但 cache 仍然命中**（同轮实测 13× credits/token 折扣）—— 因为 Bedrock prompt
/// cache 真实机制基于**请求 body 字节前缀哈希**（system + history 序列化），
/// **不依赖** conversationId。conversationId 在 Bedrock 看来只是上游遥测聚合
/// 字段，跨 turn 变化不影响 cache 命中。
///
/// 既然 cache 真因不在这里，fingerprint 的价值降级为"减少上游遥测 conv_id 噪声"。
/// 改为只哈希**首条 message** 的 role + content 文本：
///   - 跨 turn 字节级稳定（首条不变）
///   - 不含 thinking signature 这类客户端可能漂移的字段
///   - 不同会话有不同 fingerprint（首条 user 通常不同）
///
/// 返回 None: messages 空，让上层走 v4。
fn messages_history_fingerprint(messages: &[crate::anthropic::types::Message]) -> Option<String> {
    let first = messages.first()?;
    let mut hasher = Sha256::new();
    hasher.update(first.role.as_bytes());
    hasher.update(b"\x1f");
    // 优先取字符串形态（最常见、字节最稳定）；array 形态退而求其次走 JSON 序列化。
    // 注意：这里只哈希**首条** message —— Anthropic SDK 把 assistant 写回 history
    // 时不会回去改首条 user message，所以这是会话级稳定 anchor。
    if let Some(s) = first.content.as_str() {
        hasher.update(s.as_bytes());
    } else {
        let s = serde_json::to_string(&first.content).unwrap_or_default();
        hasher.update(s.as_bytes());
    }
    let digest = hasher.finalize();
    let bytes: [u8; 16] = digest[..16].try_into().ok()?;
    Some(Uuid::new_v5(&Uuid::NAMESPACE_DNS, &bytes).to_string())
}

/// Kiro API 工具名称最大长度限制
const TOOL_NAME_MAX_LEN: usize = 63;

/// 生成确定性短名称：截断前缀 + "_" + 8 位 SHA256 hex
fn shorten_tool_name(name: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(name.as_bytes());
    let hash_hex = format!("{:x}", hasher.finalize());
    let hash_suffix = &hash_hex[..8];
    let prefix_max = TOOL_NAME_MAX_LEN - 1 - 8;
    let prefix = match name.char_indices().nth(prefix_max) {
        Some((idx, _)) => &name[..idx],
        None => name,
    };
    format!("{}_{}", prefix, hash_suffix)
}

/// 如果名称超长则缩短，并记录映射（short → original）
fn map_tool_name(name: &str, tool_name_map: &mut HashMap<String, String>) -> String {
    if name.len() <= TOOL_NAME_MAX_LEN {
        return name.to_string();
    }
    let short = shorten_tool_name(name);
    tool_name_map.insert(short.clone(), name.to_string());
    short
}

/// 收集历史消息中使用的所有工具名称（小写去重）
///
/// 返回去重后的工具名称列表，保留原始大小写（首次出现的形式），
/// 但通过小写比较避免 `read` / `Read` 这类变体重复。
fn collect_history_tool_names(history: &[Message]) -> Vec<String> {
    let mut seen_lowercase = std::collections::HashSet::new();
    let mut tool_names = Vec::new();

    for msg in history {
        if let Message::Assistant(assistant_msg) = msg
            && let Some(ref tool_uses) = assistant_msg.assistant_response_message.tool_uses
        {
            for tool_use in tool_uses {
                if seen_lowercase.insert(tool_use.name.to_lowercase()) {
                    tool_names.push(tool_use.name.clone());
                }
            }
        }
    }

    tool_names
}

/// 为历史中使用但不在 tools 列表中的工具创建占位符定义
/// Kiro API 要求：历史消息中引用的工具必须在 currentMessage.tools 中有定义
///
/// **Round 7 (2026-05-13)**: 保持与 `normalize_json_schema` 一致 ——
/// 不主动写入 `$schema` 与 `additionalProperties` 字段（Round 6 已确认
/// kiro-cli 2.3.0 wire 不发这两个字段；强加会偏移 prefix-cache key、
/// 每工具浪费 ~80B）。
fn create_placeholder_tool(name: &str) -> KiroTool {
    KiroTool {
        tool_specification: ToolSpecification {
            name: name.to_string(),
            description: "Tool used in conversation history".to_string(),
            input_schema: InputSchema::from_json(serde_json::json!({
                "type": "object",
                "properties": {},
                "required": []
            })),
        },
    }
}

/// 将 Anthropic 请求转换为 Kiro 请求
pub fn convert_request(
    req: &MessagesRequest,
    compression_config: &CompressionConfig,
) -> Result<ConversionResult, ConversionError> {
    // 1. 映射模型
    let model_id = map_model(&req.model)
        .ok_or_else(|| ConversionError::UnsupportedModel(req.model.clone()))?;

    // 2. 检查消息列表
    if req.messages.is_empty() {
        return Err(ConversionError::EmptyMessages);
    }

    // 2.5. 预处理 prefill：如果末尾是 assistant，静默丢弃并截断到最后一条 user
    // 原因：Claude 4.x 已弃用 assistant prefill，Kiro API 也不支持
    let messages: &[_] = if req
        .messages
        .last()
        .map(|m| m.role != "user")
        .unwrap_or(false)
    {
        tracing::info!("检测到末尾 assistant 消息（prefill），静默丢弃，回退到最后一条 user 消息");
        let last_user_idx = req
            .messages
            .iter()
            .rposition(|m| m.role == "user")
            .ok_or(ConversionError::EmptyMessages)?;
        &req.messages[..=last_user_idx]
    } else {
        &req.messages
    };

    // 2.6. 验证最后一条消息内容不为空
    // 检查最后一条消息（经过 prefill 处理后）是否有有效内容
    let last_message = messages.last().unwrap();
    let has_valid_content = match &last_message.content {
        serde_json::Value::String(s) => !s.trim().is_empty(),
        serde_json::Value::Array(arr) => arr.iter().any(|item| {
            if let Ok(block) = serde_json::from_value::<ContentBlock>(item.clone()) {
                match block.block_type.as_str() {
                    "text" => block.text.as_ref().is_some_and(|t| !t.trim().is_empty()),
                    "image" | "tool_use" | "tool_result" => true,
                    _ => false,
                }
            } else {
                false
            }
        }),
        _ => false,
    };
    if !has_valid_content {
        tracing::warn!("最后一条消息内容为空（仅包含空白文本或无内容）");
        return Err(ConversionError::EmptyMessageContent);
    }

    // 3. 生成会话 ID 和代理 ID
    //
    // 优先级:
    //   (a) metadata.user_id 含 session UUID → 直接取
    //   (b) 否则用 history 前缀（除最后一条 user）SHA-256 指纹做 fallback。
    //       proxy 是无状态的，但同一会话的"已经发生过的对话"对每次请求都稳定；
    //       同一上下文 → 同一 conversation_id → 同一 v5 派生的 agentContinuationId。
    //   (c) 单轮且无 metadata 时（history 为空），退化为 Uuid::v4。
    //
    // 重要：旧实现在 (a) 失败时直接 Uuid::v4，导致 Anthropic SDK 默认配置
    // （不传 metadata.user_id）下每请求一个新 conversation_id，
    // patch #6 (agentContinuationId v5 派生) 完全空操作 → 多轮 prefix cache 失效。
    let conversation_id = req
        .metadata
        .as_ref()
        .and_then(|m| m.user_id.as_ref())
        .and_then(|user_id| extract_session_id(user_id))
        .or_else(|| messages_history_fingerprint(messages))
        .unwrap_or_else(|| Uuid::new_v4().to_string());
    // kiro-cli 2.3.0 multi-turn capture (gar-body-1.json & gar-body-3.json)
    // shows the same `agentContinuationId` across every turn of one session —
    // it's a session-stable ID, not per-request. We derive it deterministically
    // from `conversation_id` so repeat requests under the same session reuse it.
    let agent_continuation_id =
        Uuid::new_v5(&Uuid::NAMESPACE_DNS, conversation_id.as_bytes()).to_string();

    // 4. 确定触发类型
    let chat_trigger_type = determine_chat_trigger_type(req);

    // 4.5. 统计图片总数（用于决定压缩策略，基于截断后的 messages）
    let total_image_count: usize = messages
        .iter()
        .map(|msg| count_images_in_content(&msg.content))
        .sum();

    // 4.6. 初始化图片配额（所有消息合计不超过 MAX_TOTAL_IMAGES）
    let mut remaining_image_budget = MAX_TOTAL_IMAGES;

    // 5. 处理最后一条消息作为 current_message（经过 prefill 预处理，末尾必为 user）
    // 先处理 currentMessage 以优先保留当前用户输入的图片
    let last_message = messages.last().unwrap();
    let (text_content, images, tool_results) = process_message_content(
        &last_message.content,
        compression_config,
        total_image_count,
        &mut remaining_image_budget,
    )?;

    // 6. 转换工具定义（超长名称自动缩短并记录映射）
    let mut tool_name_map = HashMap::new();
    let mut tools = convert_tools(
        &req.tools,
        compression_config.tool_description_max_chars,
        &mut tool_name_map,
    );

    // 7. 构建历史消息（需要先构建，以便收集历史中使用的工具）
    // history 使用 currentMessage 消耗后的剩余图片配额
    let mut history = build_history(
        req,
        messages,
        BuildHistoryContext {
            model_id: &model_id,
            compression_config,
            total_image_count,
            is_agentic: is_agentic_model(&req.model),
            remaining_image_budget: &mut remaining_image_budget,
            tool_name_map: &mut tool_name_map,
        },
    )?;

    // 8. 验证并过滤 tool_use/tool_result 配对
    // 移除孤立的 tool_result（没有对应的 tool_use）
    // 同时返回孤立的 tool_use_id 集合，用于后续清理
    let (validated_tool_results, orphaned_tool_use_ids) =
        validate_tool_pairing(&history, &tool_results);

    // 9. 从历史中移除孤立的 tool_use（Kiro API 要求 tool_use 必须有对应的 tool_result）
    remove_orphaned_tool_uses(&mut history, &orphaned_tool_use_ids);

    // 10. 收集历史中使用的工具名称，为缺失的工具生成占位符定义
    // Kiro API 要求：历史消息中引用的工具必须在 tools 列表中有定义
    // 注意：Kiro 匹配工具名称时忽略大小写，所以这里也需要忽略大小写比较
    let history_tool_names = collect_history_tool_names(&history);
    let mut existing_tool_names: std::collections::HashSet<_> = tools
        .iter()
        .map(|t| t.tool_specification.name.to_lowercase())
        .collect();

    for tool_name in history_tool_names {
        let lower = tool_name.to_lowercase();
        if !existing_tool_names.contains(&lower) {
            tools.push(create_placeholder_tool(&tool_name));
            existing_tool_names.insert(lower);
        }
    }

    // 10.5. 工具压缩：在所有工具（含 placeholder）就绪后执行
    tools = super::tool_compression::compress_tools_if_needed(&tools);

    // 10.6. 工具统计诊断日志
    {
        let original_tool_count = req.tools.as_ref().map(|t| t.len()).unwrap_or(0);
        let placeholder_count = tools.len().saturating_sub(original_tool_count);

        // 大小写不敏感的重复检测
        let mut name_counts: std::collections::HashMap<String, usize> =
            std::collections::HashMap::new();
        for t in &tools {
            *name_counts
                .entry(t.tool_specification.name.to_lowercase())
                .or_insert(0) += 1;
        }
        let duplicates: Vec<_> = name_counts
            .iter()
            .filter(|(_, count)| **count > 1)
            .map(|(name, count)| format!("{}(x{})", name, count))
            .collect();

        if !duplicates.is_empty() {
            tracing::warn!(
                tool_count = tools.len(),
                duplicates = ?duplicates,
                "检测到重复工具名称（大小写不敏感）"
            );
        }
        tracing::info!(
            tool_count = tools.len(),
            placeholder_count = placeholder_count,
            "工具定义统计"
        );
    }

    // 11. 构建 UserInputMessageContext
    let mut context = UserInputMessageContext::new();
    if !tools.is_empty() {
        context = context.with_tools(tools);
    }
    let has_tool_results = !validated_tool_results.is_empty();
    if has_tool_results {
        context = context.with_tool_results(validated_tool_results);
    }

    // 12. 构建当前消息
    // 保留文本内容，即使有工具结果也不丢弃用户文本
    let content = non_empty_content_or_space(text_content, !images.is_empty() || has_tool_results);
    // current_message 是请求主体，必须保留；若文本为空且无非文本载荷，最终兜底
    let content = if content.trim().is_empty() && images.is_empty() && !has_tool_results {
        tracing::warn!("currentMessage content 为空，已使用占位符修复");
        ".".to_string()
    } else {
        content
    };

    let mut user_input = UserInputMessage::new(content, &model_id)
        .with_context(context)
        .with_origin("AI_EDITOR");

    if !images.is_empty() {
        user_input = user_input.with_images(images);
    }

    let current_message = CurrentMessage::new(user_input);

    // 12.5. 图片统计日志
    {
        let actual_image_count = MAX_TOTAL_IMAGES - remaining_image_budget;
        if actual_image_count > 0 || total_image_count > 0 {
            tracing::info!(
                source_image_count = total_image_count,
                actual_image_count = actual_image_count,
                images_dropped = total_image_count.saturating_sub(actual_image_count),
                budget_remaining = remaining_image_budget,
                "图片统计"
            );
        }
    }

    // 13. 构建 ConversationState
    let mut conversation_state = ConversationState::new(conversation_id)
        .with_agent_continuation_id(agent_continuation_id)
        .with_agent_task_type("vibe")
        .with_chat_trigger_type(chat_trigger_type)
        .with_current_message(current_message)
        .with_history(history);

    // 14. 执行输入压缩
    let compression_stats = if compression_config.enabled {
        let stats = super::compressor::compress(&mut conversation_state, compression_config);
        if stats.total_saved() > 0 || stats.history_turns_removed > 0 {
            Some(stats)
        } else {
            None
        }
    } else {
        None
    };

    if !tool_name_map.is_empty() {
        tracing::info!("工具名称映射: {} 个超长名称已缩短", tool_name_map.len());
    }

    Ok(ConversionResult {
        conversation_state,
        compression_stats,
        tool_name_map,
    })
}

/// 确定聊天触发类型
/// "AUTO" 模式可能会导致 400 Bad Request 错误
fn determine_chat_trigger_type(_req: &MessagesRequest) -> String {
    "MANUAL".to_string()
}

/// 处理消息内容，提取文本、图片和工具结果
fn process_message_content(
    content: &serde_json::Value,
    compression_config: &CompressionConfig,
    total_image_count: usize,
    remaining_image_budget: &mut usize,
) -> Result<(String, Vec<KiroImage>, Vec<ToolResult>), ConversionError> {
    let mut text_parts = Vec::new();
    let mut images = Vec::new();
    let mut tool_results = Vec::new();

    match content {
        serde_json::Value::String(s) => {
            text_parts.push(s.clone());
        }
        serde_json::Value::Array(arr) => {
            for item in arr {
                if let Ok(block) = serde_json::from_value::<ContentBlock>(item.clone()) {
                    match block.block_type.as_str() {
                        "text" => {
                            if let Some(text) = block.text {
                                text_parts.push(text);
                            }
                        }
                        "image" => {
                            if let Some(source) = block.source
                                && let Some(format) = get_image_format(&source.media_type)
                            {
                                // GIF：抽帧为多张静态图，避免动图 base64 体积巨大导致上游 400
                                if format.eq_ignore_ascii_case("gif") {
                                    if *remaining_image_budget == 0 {
                                        tracing::warn!("图片配额已用尽，跳过 GIF");
                                        continue;
                                    }
                                    match process_gif_frames(
                                        &source.data,
                                        compression_config,
                                        total_image_count,
                                        *remaining_image_budget,
                                    ) {
                                        Ok(gif) => {
                                            let total_final_bytes: usize =
                                                gif.frames.iter().map(|f| f.final_bytes_len).sum();
                                            tracing::info!(
                                                duration_ms = gif.duration_ms,
                                                source_frames = gif.source_frames,
                                                sampled_frames = gif.frames.len(),
                                                sampling_interval_ms = gif.sampling_interval_ms,
                                                output_format = gif.output_format,
                                                original_bytes_len =
                                                    gif.frames[0].original_bytes_len,
                                                total_final_bytes = total_final_bytes,
                                                "GIF 已抽帧并重编码"
                                            );

                                            let frame_count = gif.frames.len();
                                            for f in gif.frames {
                                                images.push(KiroImage::from_base64(
                                                    gif.output_format,
                                                    f.data,
                                                ));
                                            }
                                            *remaining_image_budget =
                                                remaining_image_budget.saturating_sub(frame_count);
                                        }
                                        Err(e) => {
                                            tracing::warn!(
                                                "GIF 抽帧失败，回退为静态图（可能丢失动图信息）: {}",
                                                e
                                            );
                                            if *remaining_image_budget == 0 {
                                                tracing::warn!("图片配额已用尽，跳过 GIF 回退");
                                                continue;
                                            }
                                            match process_image_to_format(
                                                &source.data,
                                                "jpeg",
                                                compression_config,
                                                total_image_count,
                                            ) {
                                                Ok(result) => {
                                                    images.push(KiroImage::from_base64(
                                                        "jpeg",
                                                        result.data,
                                                    ));
                                                    *remaining_image_budget -= 1;
                                                }
                                                Err(e2) => {
                                                    tracing::warn!(
                                                        "GIF 回退重编码失败，尝试静态 GIF: {}",
                                                        e2
                                                    );
                                                    match process_image(
                                                        &source.data,
                                                        &format,
                                                        compression_config,
                                                        total_image_count,
                                                    ) {
                                                        Ok(result) => {
                                                            images.push(KiroImage::from_base64(
                                                                format,
                                                                result.data,
                                                            ));
                                                            *remaining_image_budget -= 1;
                                                        }
                                                        Err(e3) => {
                                                            tracing::warn!(
                                                                "图片处理失败，使用原始数据: {}",
                                                                e3
                                                            );
                                                            images.push(KiroImage::from_base64(
                                                                format,
                                                                source.data,
                                                            ));
                                                            *remaining_image_budget -= 1;
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                } else {
                                    // 处理静态图片（可能缩放）
                                    if *remaining_image_budget == 0 {
                                        tracing::warn!("图片配额已用尽，跳过静态图片");
                                        continue;
                                    }
                                    match process_image(
                                        &source.data,
                                        &format,
                                        compression_config,
                                        total_image_count,
                                    ) {
                                        Ok(result) => {
                                            if result.was_resized {
                                                tracing::info!(
                                                    "图片已缩放: {:?} -> {:?}, tokens: {}",
                                                    result.original_size,
                                                    result.final_size,
                                                    result.tokens
                                                );
                                            }
                                            images
                                                .push(KiroImage::from_base64(format, result.data));
                                            *remaining_image_budget -= 1;
                                        }
                                        Err(e) => {
                                            tracing::warn!("图片处理失败，使用原始数据: {}", e);
                                            images
                                                .push(KiroImage::from_base64(format, source.data));
                                            *remaining_image_budget -= 1;
                                        }
                                    }
                                }
                            }
                        }
                        "tool_result" => {
                            if let Some(tool_use_id) = block.tool_use_id {
                                let result_content = extract_tool_result_content(&block.content);
                                let is_error = block.is_error.unwrap_or(false);

                                let mut result = if is_error {
                                    ToolResult::error(&tool_use_id, result_content)
                                } else {
                                    ToolResult::success(&tool_use_id, result_content)
                                };
                                result.status =
                                    Some(if is_error { "error" } else { "success" }.to_string());

                                tool_results.push(result);
                            }
                        }
                        "tool_use" => {
                            // tool_use 在 assistant 消息中处理，这里忽略
                        }
                        _ => {}
                    }
                }
            }
        }
        _ => {}
    }

    Ok((text_parts.join("\n"), images, tool_results))
}

/// 从 media_type 获取图片格式
fn get_image_format(media_type: &str) -> Option<String> {
    match media_type {
        "image/jpeg" => Some("jpeg".to_string()),
        "image/png" => Some("png".to_string()),
        "image/gif" => Some("gif".to_string()),
        "image/webp" => Some("webp".to_string()),
        _ => None,
    }
}

/// 提取工具结果内容
fn extract_tool_result_content(content: &Option<serde_json::Value>) -> String {
    match content {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(serde_json::Value::Array(arr)) => {
            let mut parts = Vec::new();
            for item in arr {
                if let Some(text) = item.get("text").and_then(|v| v.as_str()) {
                    parts.push(text.to_string());
                }
            }
            parts.join("\n")
        }
        Some(v) => v.to_string(),
        None => String::new(),
    }
}

/// 验证并过滤 tool_use/tool_result 配对
///
/// 收集所有 tool_use_id，验证 tool_result 是否匹配
/// 静默跳过孤立的 tool_use 和 tool_result，输出警告日志
///
/// # Arguments
/// * `history` - 历史消息引用
/// * `tool_results` - 当前消息中的 tool_result 列表
///
/// # Returns
/// 元组：(经过验证和过滤后的 tool_result 列表, 孤立的 tool_use_id 集合)
fn validate_tool_pairing(
    history: &[Message],
    tool_results: &[ToolResult],
) -> (Vec<ToolResult>, std::collections::HashSet<String>) {
    use std::collections::HashSet;

    // 1. 收集所有历史中的 tool_use_id
    let mut all_tool_use_ids: HashSet<String> = HashSet::new();
    // 2. 收集历史中已经有 tool_result 的 tool_use_id
    let mut history_tool_result_ids: HashSet<String> = HashSet::new();

    for msg in history {
        match msg {
            Message::Assistant(assistant_msg) => {
                if let Some(ref tool_uses) = assistant_msg.assistant_response_message.tool_uses {
                    for tool_use in tool_uses {
                        all_tool_use_ids.insert(tool_use.tool_use_id.clone());
                    }
                }
            }
            Message::User(user_msg) => {
                // 收集历史 user 消息中的 tool_results
                for result in &user_msg
                    .user_input_message
                    .user_input_message_context
                    .tool_results
                {
                    history_tool_result_ids.insert(result.tool_use_id.clone());
                }
            }
        }
    }

    // 3. 计算真正未配对的 tool_use_ids（排除历史中已配对的）
    let mut unpaired_tool_use_ids: HashSet<String> = all_tool_use_ids
        .difference(&history_tool_result_ids)
        .cloned()
        .collect();

    // 4. 过滤并验证当前消息的 tool_results
    let mut filtered_results = Vec::new();

    for result in tool_results {
        if unpaired_tool_use_ids.contains(&result.tool_use_id) {
            // 配对成功
            filtered_results.push(result.clone());
            unpaired_tool_use_ids.remove(&result.tool_use_id);
        } else if all_tool_use_ids.contains(&result.tool_use_id) {
            // tool_use 存在但已经在历史中配对过了，这是重复的 tool_result
            tracing::warn!(
                "跳过重复的 tool_result：该 tool_use 已在历史中配对，tool_use_id={}",
                result.tool_use_id
            );
        } else {
            // 孤立 tool_result - 找不到对应的 tool_use
            tracing::warn!(
                "跳过孤立的 tool_result：找不到对应的 tool_use，tool_use_id={}",
                result.tool_use_id
            );
        }
    }

    // 5. 检测真正孤立的 tool_use（有 tool_use 但在历史和当前消息中都没有 tool_result）
    for orphaned_id in &unpaired_tool_use_ids {
        tracing::warn!(
            "检测到孤立的 tool_use：找不到对应的 tool_result，将从历史中移除，tool_use_id={}",
            orphaned_id
        );
    }

    // 配对验证汇总日志（仅在有异常时输出）
    if !unpaired_tool_use_ids.is_empty() || filtered_results.len() != tool_results.len() {
        tracing::warn!(
            total_tool_use_ids = all_tool_use_ids.len(),
            history_tool_result_ids = history_tool_result_ids.len(),
            orphaned_tool_use_count = unpaired_tool_use_ids.len(),
            orphaned_tool_use_ids = ?unpaired_tool_use_ids,
            input_tool_results = tool_results.len(),
            output_tool_results = filtered_results.len(),
            "tool_use/tool_result 配对验证完成（有异常）"
        );
    }

    (filtered_results, unpaired_tool_use_ids)
}

/// 从历史消息中移除孤立的 tool_use
///
/// Kiro API 要求每个 tool_use 必须有对应的 tool_result，否则返回 400 Bad Request。
/// 此函数遍历历史中的 assistant 消息，移除没有对应 tool_result 的 tool_use。
///
/// # Arguments
/// * `history` - 可变的历史消息列表
/// * `orphaned_ids` - 需要移除的孤立 tool_use_id 集合
fn remove_orphaned_tool_uses(
    history: &mut [Message],
    orphaned_ids: &std::collections::HashSet<String>,
) {
    if orphaned_ids.is_empty() {
        return;
    }

    for msg in history.iter_mut() {
        if let Message::Assistant(assistant_msg) = msg
            && let Some(ref mut tool_uses) = assistant_msg.assistant_response_message.tool_uses
        {
            let original_len = tool_uses.len();
            tool_uses.retain(|tu| !orphaned_ids.contains(&tu.tool_use_id));

            // 如果移除后为空，设置为 None
            if tool_uses.is_empty() {
                assistant_msg.assistant_response_message.tool_uses = None;
            } else if tool_uses.len() != original_len {
                tracing::debug!(
                    "从 assistant 消息中移除了 {} 个孤立的 tool_use",
                    original_len - tool_uses.len()
                );
            }
        }
    }
}

/// 转换工具定义
///
/// # 不支持的工具类型
///
/// 以下工具类型会被自动过滤（Kiro API 当前不支持）：
/// - `web_search_*`: Anthropic 的 Web 搜索工具（如 `web_search_20250305`）
///
/// **TODO**: 如果 Kiro API 未来支持 web_search，需要：
/// 1. 移除下方的 `filter` 过滤逻辑
/// 2. 添加 web_search 工具的转换逻辑（可能需要特殊处理 `max_uses` 等字段）
/// 3. 更新相关测试用例
fn convert_tools(
    tools: &Option<Vec<AnthropicTool>>,
    max_description_chars: usize,
    tool_name_map: &mut HashMap<String, String>,
) -> Vec<KiroTool> {
    let Some(tools) = tools else {
        return Vec::new();
    };

    tools
        .iter()
        .filter(|t| {
            // 过滤掉 web_search 类型的工具（Kiro API 当前不支持）
            // 工具类型格式: "web_search_20250305"
            let dominated = t
                .tool_type
                .as_ref()
                .is_some_and(|ty| ty.starts_with("web_search"));
            if dominated {
                tracing::debug!("过滤不支持的工具: name={}, type={:?}", t.name, t.tool_type);
            }

            !dominated
        })
        .map(|t| {
            let mut description = if t.description.trim().is_empty() {
                format!("Tool: {}", t.name)
            } else {
                t.description.clone()
            };

            // 对 Write/Edit 工具追加自定义描述后缀
            let suffix = match t.name.as_str() {
                "Write" => WRITE_TOOL_DESCRIPTION_SUFFIX,
                "Edit" => EDIT_TOOL_DESCRIPTION_SUFFIX,
                _ => "",
            };
            if !suffix.is_empty() {
                description.push('\n');
                description.push_str(suffix);
            }

            // 限制描述长度（0=不截断；安全截断 UTF-8，单次遍历）
            let description = if max_description_chars > 0 {
                match description.char_indices().nth(max_description_chars) {
                    Some((idx, _)) => description[..idx].to_string(),
                    None => description,
                }
            } else {
                description
            };

            let schema = normalize_json_schema(serde_json::json!(t.input_schema));

            KiroTool {
                tool_specification: ToolSpecification {
                    name: map_tool_name(&t.name, tool_name_map),
                    description,
                    input_schema: InputSchema::from_json(schema),
                },
            }
        })
        .collect()
}

/// 生成thinking标签前缀
fn generate_thinking_prefix(req: &MessagesRequest) -> Option<String> {
    if let Some(t) = &req.thinking {
        if t.thinking_type == "enabled" {
            return Some(format!(
                "<thinking_mode>enabled</thinking_mode><max_thinking_length>{}</max_thinking_length>",
                t.budget_tokens
            ));
        } else if t.thinking_type == "adaptive" {
            let raw_effort = req
                .output_config
                .as_ref()
                .map(|c| c.effort.as_str())
                .unwrap_or("high");
            // 白名单归一化：仅接受 low/medium/high，非法值回退 high
            let effort = match raw_effort {
                "low" | "medium" | "high" => raw_effort,
                _ => {
                    tracing::warn!("未知的 thinking effort 值 '{}', 回退为 'high'", raw_effort);
                    "high"
                }
            };
            return Some(format!(
                "<thinking_mode>adaptive</thinking_mode><thinking_effort>{}</thinking_effort>",
                effort
            ));
        }
    }
    None
}

/// 检查内容是否已包含thinking标签
fn has_thinking_tags(content: &str) -> bool {
    content.contains("<thinking_mode>") || content.contains("<max_thinking_length>")
}

/// 检查请求的工具列表中是否包含 Write 或 Edit 工具
fn has_write_or_edit_tool(req: &MessagesRequest) -> bool {
    req.tools
        .as_ref()
        .is_some_and(|tools| tools.iter().any(|t| t.name == "Write" || t.name == "Edit"))
}

struct BuildHistoryContext<'a> {
    model_id: &'a str,
    compression_config: &'a CompressionConfig,
    total_image_count: usize,
    is_agentic: bool,
    remaining_image_budget: &'a mut usize,
    tool_name_map: &'a mut HashMap<String, String>,
}

/// 构建历史消息
/// `messages` 参数是经过 prefill 预处理后的消息切片（末尾必为 user）
fn build_history(
    req: &MessagesRequest,
    messages: &[super::types::Message],
    ctx: BuildHistoryContext<'_>,
) -> Result<Vec<Message>, ConversionError> {
    let BuildHistoryContext {
        model_id,
        compression_config,
        total_image_count,
        is_agentic,
        remaining_image_budget,
        tool_name_map,
    } = ctx;
    let mut history = Vec::new();

    // 生成thinking前缀（如果需要）
    let thinking_prefix = generate_thinking_prefix(req);

    // 仅在请求包含 Write/Edit 工具时注入分块写入策略
    let should_inject_chunked_policy = has_write_or_edit_tool(req);

    // 1. 处理系统消息
    if let Some(ref system) = req.system {
        let system_content: String = system
            .iter()
            .map(|s| s.text.clone())
            .collect::<Vec<_>>()
            .join("\n");

        if !system_content.is_empty() {
            // 仅在存在 Write/Edit 工具时追加分块写入策略到系统消息
            let system_content = if should_inject_chunked_policy {
                format!("{}\n{}", system_content, SYSTEM_CHUNKED_POLICY)
            } else {
                system_content
            };

            // 系统消息作为 user + assistant 配对。
            //
            // kiro-cli 2.3.0 wire format (Q_LOG_LEVEL=trace 2026-05-12 capture):
            // system 注入是一个空的 CONTEXT ENTRY 包装 + 换行 + `Follow this
            // instruction: <text>`，对应的 assistant ack 是一段固定的长句。
            // 这段文案是上游 prefix-cache key 的稳定前缀，文案不一致会让每轮
            // 重算 system 部分，破坏 cache 命中。
            //
            // thinking_prefix（`<thinking_mode>...</thinking_mode>` 元控制标签）
            // 是给模型自己的元配置，不应当被包进 "Follow this instruction:"
            // 后面（语义错位 — 模型会把它当成用户指令的一部分）。把它放在
            // wrap 之外的最前面，与 system 指令分离。
            let wrapped_system = wrap_system_for_history(&system_content);
            let history_content = match (&thinking_prefix, has_thinking_tags(&system_content)) {
                (Some(prefix), false) => format!("{prefix}\n{wrapped_system}"),
                _ => wrapped_system,
            };
            let user_msg = HistoryUserMessage::new(history_content, model_id);
            history.push(Message::User(user_msg));

            let assistant_msg = HistoryAssistantMessage::new(SYSTEM_ACK);
            history.push(Message::Assistant(assistant_msg));
        }
    } else if thinking_prefix.is_some() || should_inject_chunked_policy {
        // 没有系统消息但需要注入 thinking 配置或分块写入策略。
        // 注：此分支没有用户原 system，仅注入控制标签；不走 CONTEXT ENTRY 包装
        // （CONTEXT ENTRY 是给"用户实际要求"用的，元控制标签不应混入）。
        let mut parts = Vec::new();
        if let Some(ref prefix) = thinking_prefix {
            parts.push(prefix.clone());
        }
        if should_inject_chunked_policy {
            // 分块写入策略是协议级行为指令；包成 Follow this instruction: 比裸
            // 注入更接近 kiro-cli 风格的多 system 处理。
            parts.push(wrap_system_for_history(SYSTEM_CHUNKED_POLICY));
        }
        let content = parts.join("\n");

        let user_msg = HistoryUserMessage::new(content, model_id);
        history.push(Message::User(user_msg));

        let assistant_msg = HistoryAssistantMessage::new(SYSTEM_ACK);
        history.push(Message::Assistant(assistant_msg));
    }

    // Agentic 模型：追加专用系统提示
    if is_agentic {
        let user_msg = HistoryUserMessage::new(KIRO_AGENTIC_SYSTEM_PROMPT, model_id);
        history.push(Message::User(user_msg));

        let assistant_msg =
            HistoryAssistantMessage::new("I will work autonomously following these principles.");
        history.push(Message::Assistant(assistant_msg));
    }

    // 2. 处理常规消息历史
    // 最后一条消息作为 currentMessage，不加入历史
    let history_end_index = messages.len().saturating_sub(1);

    // 收集并配对消息
    let mut user_buffer: Vec<&super::types::Message> = Vec::new();
    let mut assistant_buffer: Vec<&super::types::Message> = Vec::new();

    for msg in messages.iter().take(history_end_index) {
        if msg.role == "user" {
            // 先处理累积的 assistant 消息
            if !assistant_buffer.is_empty() {
                let merged = merge_assistant_messages(&assistant_buffer, tool_name_map)?;
                history.push(Message::Assistant(merged));
                assistant_buffer.clear();
            }
            user_buffer.push(msg);
        } else if msg.role == "assistant" {
            // 先处理累积的 user 消息
            if !user_buffer.is_empty() {
                let merged_user = merge_user_messages(
                    &user_buffer,
                    model_id,
                    compression_config,
                    total_image_count,
                    remaining_image_budget,
                )?;
                history.push(Message::User(merged_user));
                user_buffer.clear();
            }
            // 累积 assistant 消息（支持连续多条）
            assistant_buffer.push(msg);
        }
    }

    // 处理末尾累积的 assistant 消息
    if !assistant_buffer.is_empty() {
        let merged = merge_assistant_messages(&assistant_buffer, tool_name_map)?;
        history.push(Message::Assistant(merged));
    }

    // 处理结尾的孤立 user 消息
    if !user_buffer.is_empty() {
        let merged_user = merge_user_messages(
            &user_buffer,
            model_id,
            compression_config,
            total_image_count,
            remaining_image_budget,
        )?;
        history.push(Message::User(merged_user));

        // 自动配对一个 "OK" 的 assistant 响应
        let auto_assistant = HistoryAssistantMessage::new("OK");
        history.push(Message::Assistant(auto_assistant));
    }

    // 历史结构摘要日志
    {
        let mut user_count = 0usize;
        let mut assistant_count = 0usize;
        let mut synthetic_count = 0usize;
        let mut image_count_in_history = 0usize;

        // system 消息产生的合成配对
        if req
            .system
            .as_ref()
            .is_some_and(|s| s.iter().any(|b| !b.text.is_empty()))
        {
            synthetic_count += 2;
        }

        for msg in &history {
            match msg {
                Message::User(u) => {
                    user_count += 1;
                    image_count_in_history += u.user_input_message.images.len();
                }
                Message::Assistant(_) => assistant_count += 1,
            }
        }

        tracing::info!(
            history_len = history.len(),
            user_messages = user_count,
            assistant_messages = assistant_count,
            synthetic_pairs = synthetic_count,
            images_in_history = image_count_in_history,
            "历史消息结构摘要"
        );
    }

    Ok(history)
}

fn merge_user_messages(
    messages: &[&super::types::Message],
    model_id: &str,
    compression_config: &CompressionConfig,
    total_image_count: usize,
    remaining_image_budget: &mut usize,
) -> Result<HistoryUserMessage, ConversionError> {
    let mut content_parts = Vec::new();
    let mut all_images = Vec::new();
    let mut all_tool_results = Vec::new();

    for msg in messages {
        let (text, images, tool_results) = process_message_content(
            &msg.content,
            compression_config,
            total_image_count,
            remaining_image_budget,
        )?;
        if !text.is_empty() {
            content_parts.push(text);
        }
        all_images.extend(images);
        all_tool_results.extend(tool_results);
    }

    let content = non_empty_content_or_space(
        content_parts.join("\n"),
        !all_images.is_empty() || !all_tool_results.is_empty(),
    );
    // 历史 user 消息尽量避免主动补 "."：
    // 若只有非文本载荷，则保留空字符串让真实结构(images/tool_results)表达语义；
    // 仅纯文本且被压成空白时才做最终兜底，避免空 content 直发下游。
    let content =
        if content.trim().is_empty() && all_images.is_empty() && all_tool_results.is_empty() {
            ".".to_string()
        } else {
            content
        };
    // 保留文本内容，即使有工具结果也不丢弃用户文本
    let mut user_msg = UserMessage::new(&content, model_id);

    if !all_images.is_empty() {
        user_msg = user_msg.with_images(all_images);
    }

    if !all_tool_results.is_empty() {
        let mut ctx = UserInputMessageContext::new();
        ctx = ctx.with_tool_results(all_tool_results);
        user_msg = user_msg.with_context(ctx);
    }

    Ok(HistoryUserMessage {
        user_input_message: user_msg,
    })
}

/// 转换 assistant 消息
fn convert_assistant_message(
    msg: &super::types::Message,
    tool_name_map: &mut HashMap<String, String>,
) -> Result<HistoryAssistantMessage, ConversionError> {
    let mut thinking_content = String::new();
    let mut text_content = String::new();
    let mut tool_uses = Vec::new();

    match &msg.content {
        serde_json::Value::String(s) => {
            text_content = s.clone();
        }
        serde_json::Value::Array(arr) => {
            for item in arr {
                if let Ok(block) = serde_json::from_value::<ContentBlock>(item.clone()) {
                    match block.block_type.as_str() {
                        "thinking" => {
                            if let Some(thinking) = block.thinking {
                                thinking_content.push_str(&thinking);
                            }
                        }
                        "text" => {
                            if let Some(text) = block.text {
                                text_content.push_str(&text);
                            }
                        }
                        "tool_use" => {
                            if let (Some(id), Some(name)) = (block.id, block.name) {
                                let input = block.input.unwrap_or(serde_json::json!({}));
                                let mapped_name = map_tool_name(&name, tool_name_map);
                                tool_uses
                                    .push(ToolUseEntry::new(id, mapped_name).with_input(input));
                            }
                        }
                        // WebSearch 相关块：server_tool_use 忽略，
                        // web_search_tool_result 提取 title/url/snippet/page_age 保留到对话历史
                        "server_tool_use" => {}
                        "web_search_tool_result" => {
                            // 注意：历史文本长度由 compressor.rs 的压缩流程统一管理，
                            // 此处无需额外截断。实际效果需运行时验证。
                            //
                            // encrypted_content 字段：本项目中由 websearch.rs 写入，
                            // 存储的是可读 snippet；若上游改为原生 Anthropic WebSearch 响应，
                            // 该字段语义可能不同，届时需重新评估是否保留。
                            if let Some(serde_json::Value::Array(results)) = block.content {
                                for result in &results {
                                    if result.get("type").and_then(|t| t.as_str())
                                        == Some("web_search_result")
                                    {
                                        let title = result
                                            .get("title")
                                            .and_then(|t| t.as_str())
                                            .unwrap_or("");
                                        let url = result
                                            .get("url")
                                            .and_then(|u| u.as_str())
                                            .unwrap_or("");
                                        if !url.is_empty() {
                                            // 历史上下文仅供 Kiro 模型理解，使用纯文本格式
                                            // 避免 title/url 含特殊字符时 Markdown 语法被破坏
                                            let snippet = result
                                                .get("encrypted_content")
                                                .and_then(|s| s.as_str())
                                                .unwrap_or("");
                                            let page_age = result
                                                .get("page_age")
                                                .and_then(|s| s.as_str())
                                                .unwrap_or("");
                                            // 剥除控制字符（换行等），保持单行
                                            let clean_title: String =
                                                title.chars().filter(|c| !c.is_control()).collect();
                                            let clean_snippet: String = snippet
                                                .chars()
                                                .filter(|c| !c.is_control())
                                                .collect();
                                            if clean_title.is_empty() {
                                                text_content.push_str(&format!("{}\n", url));
                                            } else {
                                                text_content.push_str(&format!(
                                                    "{}: {}\n",
                                                    clean_title, url
                                                ));
                                            }
                                            if !page_age.is_empty() {
                                                text_content
                                                    .push_str(&format!("Date: {}\n", page_age));
                                            }
                                            if !clean_snippet.is_empty() {
                                                text_content
                                                    .push_str(&format!("{}\n", clean_snippet));
                                            }
                                            text_content.push('\n');
                                        }
                                    }
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
        _ => {}
    }

    // 组合 thinking 和 text 内容
    // 格式: <thinking>思考内容</thinking>\n\ntext内容
    let final_content = if !thinking_content.is_empty() {
        if !text_content.is_empty() {
            format!(
                "<thinking>{}</thinking>\n\n{}",
                thinking_content, text_content
            )
        } else {
            format!("<thinking>{}</thinking>", thinking_content)
        }
    } else {
        text_content
    };

    let mut assistant = AssistantMessage::new(final_content);
    if !tool_uses.is_empty() {
        // kiro-cli 2.3.0 wire: every toolUse turn carries a `messageId` UUID
        // (gar-body-3.json line 38). Plain-text turns omit it. We mint a fresh
        // UUID — server-side it appears to be opaque/ID-only.
        assistant.message_id = Some(Uuid::new_v4().to_string());
        assistant = assistant.with_tool_uses(tool_uses);
    }

    Ok(HistoryAssistantMessage {
        assistant_response_message: assistant,
    })
}

/// 合并多个连续的 assistant 消息为一条
/// 用于处理网络不稳定时产生的连续 assistant 消息（Issue #79）
fn merge_assistant_messages(
    messages: &[&super::types::Message],
    tool_name_map: &mut HashMap<String, String>,
) -> Result<HistoryAssistantMessage, ConversionError> {
    assert!(!messages.is_empty());
    if messages.len() == 1 {
        return convert_assistant_message(messages[0], tool_name_map);
    }

    let mut all_tool_uses: Vec<ToolUseEntry> = Vec::new();
    let mut content_parts: Vec<String> = Vec::new();

    for msg in messages {
        let converted = convert_assistant_message(msg, tool_name_map)?;
        let am = converted.assistant_response_message;
        if !am.content.trim().is_empty() {
            content_parts.push(am.content);
        }
        if let Some(tus) = am.tool_uses {
            all_tool_uses.extend(tus);
        }
    }

    let content = if content_parts.is_empty() {
        String::new()
    } else {
        content_parts.join("\n\n")
    };

    let mut assistant = AssistantMessage::new(content);
    if !all_tool_uses.is_empty() {
        assistant.message_id = Some(Uuid::new_v4().to_string());
        assistant = assistant.with_tool_uses(all_tool_uses);
    }
    Ok(HistoryAssistantMessage {
        assistant_response_message: assistant,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::config::CompressionConfig;

    #[test]
    fn test_map_model_sonnet() {
        assert_eq!(
            map_model("claude-sonnet-4-20250514").unwrap(),
            KIRO_MODEL_SONNET_4_5
        );
        assert_eq!(
            map_model("claude-3-5-sonnet-20241022").unwrap(),
            KIRO_MODEL_SONNET_4_5
        );
        assert_eq!(
            map_model("claude-sonnet-4-6").unwrap(),
            KIRO_MODEL_SONNET_4_6
        );
        assert_eq!(
            map_model("claude-sonnet-4.6").unwrap(),
            KIRO_MODEL_SONNET_4_6
        );
    }

    #[test]
    fn test_map_model_opus() {
        assert_eq!(
            map_model("claude-opus-4-20250514").unwrap(),
            KIRO_MODEL_OPUS_4_6
        );
        assert_eq!(
            map_model("claude-opus-4-20260206").unwrap(),
            KIRO_MODEL_OPUS_4_6
        );
        assert_eq!(
            map_model("claude-opus-4-5-20250514").unwrap(),
            KIRO_MODEL_OPUS_4_5
        );
        assert_eq!(map_model("claude-opus-4.5").unwrap(), KIRO_MODEL_OPUS_4_5);
        assert_eq!(map_model("claude-opus-4-6").unwrap(), KIRO_MODEL_OPUS_4_6);
        assert_eq!(map_model("claude-opus-4-7").unwrap(), KIRO_MODEL_OPUS_4_7);
        assert_eq!(map_model("claude-opus-4.7").unwrap(), KIRO_MODEL_OPUS_4_7);
    }

    #[test]
    fn test_map_model_haiku() {
        assert_eq!(
            map_model("claude-haiku-4-20250514").unwrap(),
            KIRO_MODEL_HAIKU_4_5
        );
        assert_eq!(
            map_model("claude-haiku-4-5-20251001").unwrap(),
            KIRO_MODEL_HAIKU_4_5
        );
    }

    #[test]
    fn test_map_model_unsupported() {
        assert!(map_model("gpt-4").is_none());
    }

    #[test]
    fn test_map_model_thinking_suffixes() {
        assert_eq!(
            map_model("claude-sonnet-4-5-20250929-thinking"),
            Some(KIRO_MODEL_SONNET_4_5.to_string())
        );
        assert_eq!(
            map_model("claude-sonnet-4-6-thinking"),
            Some(KIRO_MODEL_SONNET_4_6.to_string())
        );
        assert_eq!(
            map_model("claude-opus-4-5-20251101-thinking"),
            Some(KIRO_MODEL_OPUS_4_5.to_string())
        );
        assert_eq!(
            map_model("claude-opus-4-6-thinking"),
            Some(KIRO_MODEL_OPUS_4_6.to_string())
        );
        assert_eq!(
            map_model("claude-opus-4-7-thinking"),
            Some(KIRO_MODEL_OPUS_4_7.to_string())
        );
        assert_eq!(
            map_model("claude-haiku-4-5-20251001-thinking"),
            Some(KIRO_MODEL_HAIKU_4_5.to_string())
        );
    }

    #[test]
    fn test_map_model_agentic_suffixes() {
        assert_eq!(
            map_model("claude-sonnet-4-6-agentic"),
            Some(KIRO_MODEL_SONNET_4_6.to_string())
        );
        assert_eq!(
            map_model("claude-sonnet-4-5-20250929-agentic"),
            Some(KIRO_MODEL_SONNET_4_5.to_string())
        );
        assert_eq!(
            map_model("claude-opus-4-6-agentic"),
            Some(KIRO_MODEL_OPUS_4_6.to_string())
        );
        assert_eq!(
            map_model("claude-opus-4-7-agentic"),
            Some(KIRO_MODEL_OPUS_4_7.to_string())
        );
        assert_eq!(
            map_model("claude-opus-4-5-20251101-agentic"),
            Some(KIRO_MODEL_OPUS_4_5.to_string())
        );
        assert_eq!(
            map_model("claude-haiku-4-5-20251001-agentic"),
            Some(KIRO_MODEL_HAIKU_4_5.to_string())
        );
    }

    #[test]
    fn test_map_model_versioned_entries_from_models_endpoint() {
        let supported_models = [
            ("claude-sonnet-4-6", KIRO_MODEL_SONNET_4_6),
            ("claude-sonnet-4-6-thinking", KIRO_MODEL_SONNET_4_6),
            ("claude-sonnet-4-6-agentic", KIRO_MODEL_SONNET_4_6),
            ("claude-sonnet-4-5-20250929", KIRO_MODEL_SONNET_4_5),
            ("claude-sonnet-4-5-20250929-thinking", KIRO_MODEL_SONNET_4_5),
            ("claude-sonnet-4-5-20250929-agentic", KIRO_MODEL_SONNET_4_5),
            ("claude-opus-4-5-20251101", KIRO_MODEL_OPUS_4_5),
            ("claude-opus-4-5-20251101-thinking", KIRO_MODEL_OPUS_4_5),
            ("claude-opus-4-5-20251101-agentic", KIRO_MODEL_OPUS_4_5),
            ("claude-opus-4-6", KIRO_MODEL_OPUS_4_6),
            ("claude-opus-4-6-thinking", KIRO_MODEL_OPUS_4_6),
            ("claude-opus-4-6-agentic", KIRO_MODEL_OPUS_4_6),
            ("claude-opus-4-7", KIRO_MODEL_OPUS_4_7),
            ("claude-opus-4-7-thinking", KIRO_MODEL_OPUS_4_7),
            ("claude-opus-4-7-agentic", KIRO_MODEL_OPUS_4_7),
            ("claude-haiku-4-5-20251001", KIRO_MODEL_HAIKU_4_5),
            ("claude-haiku-4-5-20251001-thinking", KIRO_MODEL_HAIKU_4_5),
            ("claude-haiku-4-5-20251001-agentic", KIRO_MODEL_HAIKU_4_5),
        ];

        for (input, expected) in supported_models {
            assert_eq!(map_model(input), Some(expected.to_string()), "{input}");
        }
    }

    #[test]
    fn test_determine_chat_trigger_type() {
        // 无工具时返回 MANUAL
        let req = MessagesRequest {
            model: "claude-sonnet-4".to_string(),
            max_tokens: 1024,
            messages: vec![],
            stream: false,
            system: None,
            tools: None,
            tool_choice: None,
            thinking: None,
            output_config: None,
            metadata: None,
        };
        assert_eq!(determine_chat_trigger_type(&req), "MANUAL");
    }

    #[test]
    fn test_collect_history_tool_names() {
        use crate::kiro::model::requests::tool::ToolUseEntry;

        // 创建包含工具使用的历史消息
        let mut assistant_msg = AssistantMessage::new("I'll read the file.");
        assistant_msg = assistant_msg.with_tool_uses(vec![
            ToolUseEntry::new("tool-1", "read")
                .with_input(serde_json::json!({"path": "/test.txt"})),
            ToolUseEntry::new("tool-2", "write")
                .with_input(serde_json::json!({"path": "/out.txt"})),
        ]);

        let history = vec![
            Message::User(HistoryUserMessage::new(
                "Read the file",
                "claude-sonnet-4.5",
            )),
            Message::Assistant(HistoryAssistantMessage {
                assistant_response_message: assistant_msg,
            }),
        ];

        let tool_names = collect_history_tool_names(&history);
        assert_eq!(tool_names.len(), 2);
        assert!(tool_names.contains(&"read".to_string()));
        assert!(tool_names.contains(&"write".to_string()));
    }

    #[test]
    fn test_create_placeholder_tool() {
        let tool = create_placeholder_tool("my_custom_tool");

        assert_eq!(tool.tool_specification.name, "my_custom_tool");
        assert!(!tool.tool_specification.description.is_empty());

        // 验证 JSON 序列化正确
        let json = serde_json::to_string(&tool).unwrap();
        assert!(json.contains("\"name\":\"my_custom_tool\""));
    }

    #[test]
    fn test_history_tools_added_to_tools_list() {
        use super::super::types::Message as AnthropicMessage;

        // 创建一个请求，历史中有工具使用，但 tools 列表为空
        let req = MessagesRequest {
            model: "claude-sonnet-4".to_string(),
            max_tokens: 1024,
            messages: vec![
                AnthropicMessage {
                    role: "user".to_string(),
                    content: serde_json::json!("Read the file"),
                },
                AnthropicMessage {
                    role: "assistant".to_string(),
                    content: serde_json::json!([
                        {"type": "text", "text": "I'll read the file."},
                        {"type": "tool_use", "id": "tool-1", "name": "read", "input": {"path": "/test.txt"}}
                    ]),
                },
                AnthropicMessage {
                    role: "user".to_string(),
                    content: serde_json::json!([
                        {"type": "tool_result", "tool_use_id": "tool-1", "content": "file content"}
                    ]),
                },
            ],
            stream: false,
            system: None,
            tools: None, // 没有提供工具定义
            tool_choice: None,
            thinking: None,
            output_config: None,
            metadata: None,
        };

        let result = convert_request(&req, &CompressionConfig::default()).unwrap();

        // 验证 tools 列表中包含了历史中使用的工具的占位符定义
        let tools = &result
            .conversation_state
            .current_message
            .user_input_message
            .user_input_message_context
            .tools;

        assert!(!tools.is_empty(), "tools 列表不应为空");
        assert!(
            tools.iter().any(|t| t.tool_specification.name == "read"),
            "tools 列表应包含 'read' 工具的占位符定义"
        );
    }

    /// Round 4 regression: agentContinuationId must be deterministic from
    /// conversationId — multi-turn captures of kiro-cli 2.3.0 show the same
    /// continuation UUID across every turn of one session. The proxy is
    /// stateless so we derive it via UUID v5 on conversation_id.
    #[test]
    fn test_wrap_system_for_history_matches_kiro_cli_format() {
        // Verify the wire-aligned wrapper format.
        let wrapped = wrap_system_for_history("You are terse.");
        assert_eq!(
            wrapped,
            "--- CONTEXT ENTRY BEGIN ---\n--- CONTEXT ENTRY END ---\n\nFollow this instruction: You are terse."
        );
    }

    /// Round 5 regression: wrap_system_for_history must be idempotent when the
    /// input already starts with the wrapper or `Follow this instruction:`.
    /// Pre-fix double-wrapping broke the prefix-cache key.
    #[test]
    fn test_wrap_system_for_history_idempotent() {
        let already_wrapped =
            "--- CONTEXT ENTRY BEGIN ---\n--- CONTEXT ENTRY END ---\n\nFollow this instruction: hi";
        assert_eq!(wrap_system_for_history(already_wrapped), already_wrapped);
        let with_follow_prefix = "Follow this instruction: hi";
        assert_eq!(
            wrap_system_for_history(with_follow_prefix),
            with_follow_prefix
        );
        // Whitespace at the head must not break detection.
        let leading_ws = "  Follow this instruction: hi";
        assert_eq!(wrap_system_for_history(leading_ws), leading_ws);
    }

    /// Round 7 regression: messages_history_fingerprint is keyed on the
    /// **first message only** (post-Round 7 redesign). Same first message →
    /// same fingerprint across ALL turns of the same session, even as
    /// history grows. Different first message → different fingerprint.
    #[test]
    fn test_messages_history_fingerprint_stability() {
        use crate::anthropic::types::Message;
        let mk = |role: &str, content: &str| Message {
            role: role.to_string(),
            content: serde_json::Value::String(content.to_string()),
        };
        // Empty messages → None (let caller fall back to v4).
        let empty: Vec<Message> = vec![];
        assert!(messages_history_fingerprint(&empty).is_none());

        // Single message → returns Some (Round 7: first-message keying).
        let single = vec![mk("user", "hi")];
        let fp_single = messages_history_fingerprint(&single).unwrap();

        // Turn 2 of same session (first message unchanged, new turns appended)
        // → same fingerprint as turn 1.
        let turn2 = vec![mk("user", "hi"), mk("assistant", "yo"), mk("user", "again")];
        let fp_turn2 = messages_history_fingerprint(&turn2).unwrap();
        assert_eq!(
            fp_single, fp_turn2,
            "first-message anchor must persist across turns"
        );

        // Turn 5 of same session — history grew further, fingerprint unchanged.
        let turn5 = vec![
            mk("user", "hi"),
            mk("assistant", "yo"),
            mk("user", "again"),
            mk("assistant", "ok"),
            mk("user", "more"),
            mk("assistant", "done"),
            mk("user", "current"),
        ];
        let fp_turn5 = messages_history_fingerprint(&turn5).unwrap();
        assert_eq!(
            fp_single, fp_turn5,
            "same first-message → same fingerprint regardless of history length"
        );

        // Different session (different first message) → different fingerprint.
        let other = vec![mk("user", "different first message")];
        let fp_other = messages_history_fingerprint(&other).unwrap();
        assert_ne!(fp_single, fp_other);
    }

    #[test]
    fn test_agent_continuation_id_deterministic_from_conversation_id() {
        let conv = "11111111-2222-3333-4444-555555555555";
        let id1 = uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_DNS, conv.as_bytes()).to_string();
        let id2 = uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_DNS, conv.as_bytes()).to_string();
        assert_eq!(id1, id2, "same conversation_id → same agentContinuationId");
        let id_other =
            uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_DNS, b"different-conv").to_string();
        assert_ne!(id1, id_other, "different conversation_id → different id");
    }

    #[test]
    fn test_extract_session_id_valid() {
        // 测试有效的 user_id 格式
        let user_id = "user_0dede55c6dcc4a11a30bbb5e7f22e6fdf86cdeba3820019cc27612af4e1243cd_account__session_8bb5523b-ec7c-4540-a9ca-beb6d79f1552";
        let session_id = extract_session_id(user_id);
        assert_eq!(
            session_id,
            Some("8bb5523b-ec7c-4540-a9ca-beb6d79f1552".to_string())
        );
    }

    #[test]
    fn test_extract_session_id_no_session() {
        // 测试没有 session 的 user_id
        let user_id = "user_0dede55c6dcc4a11a30bbb5e7f22e6fdf86cdeba3820019cc27612af4e1243cd";
        let session_id = extract_session_id(user_id);
        assert_eq!(session_id, None);
    }

    #[test]
    fn test_extract_session_id_invalid_uuid() {
        // 测试无效的 UUID 格式
        let user_id = "user_xxx_session_invalid-uuid";
        let session_id = extract_session_id(user_id);
        assert_eq!(session_id, None);
    }

    #[test]
    fn test_extract_session_id_json_format() {
        // 测试 JSON 格式的 user_id
        let user_id = r#"{"device_id":"0dede55c6dcc4a11a30bbb5e7f22e6fdf86cdeba3820019cc27612af4e1243cd","account_uuid":"","session_id":"8bb5523b-ec7c-4540-a9ca-beb6d79f1552"}"#;
        let session_id = extract_session_id(user_id);
        assert_eq!(
            session_id,
            Some("8bb5523b-ec7c-4540-a9ca-beb6d79f1552".to_string())
        );
    }

    #[test]
    fn test_extract_session_id_json_invalid_session() {
        // 测试 JSON 格式但 session_id 不是有效 UUID
        let user_id = r#"{"device_id":"abc","session_id":"not-a-uuid"}"#;
        let session_id = extract_session_id(user_id);
        assert_eq!(session_id, None);
    }

    #[test]
    fn test_convert_request_with_session_metadata() {
        use super::super::types::{Message as AnthropicMessage, Metadata};

        // 测试带有 metadata 的请求，应该使用 session UUID 作为 conversationId
        let req = MessagesRequest {
            model: "claude-sonnet-4".to_string(),
            max_tokens: 1024,
            messages: vec![AnthropicMessage {
                role: "user".to_string(),
                content: serde_json::json!("Hello"),
            }],
            stream: false,
            system: None,
            tools: None,
            tool_choice: None,
            thinking: None,
            output_config: None,
            metadata: Some(Metadata {
                user_id: Some(
                    "user_0dede55c6dcc4a11a30bbb5e7f22e6fdf86cdeba3820019cc27612af4e1243cd_account__session_a0662283-7fd3-4399-a7eb-52b9a717ae88".to_string(),
                ),
            }),
        };

        let result = convert_request(&req, &CompressionConfig::default()).unwrap();
        assert_eq!(
            result.conversation_state.conversation_id,
            "a0662283-7fd3-4399-a7eb-52b9a717ae88"
        );
    }

    #[test]
    fn test_convert_request_without_metadata() {
        use super::super::types::Message as AnthropicMessage;

        // 测试没有 metadata 的请求，应该生成新的 UUID
        let req = MessagesRequest {
            model: "claude-sonnet-4".to_string(),
            max_tokens: 1024,
            messages: vec![AnthropicMessage {
                role: "user".to_string(),
                content: serde_json::json!("Hello"),
            }],
            stream: false,
            system: None,
            tools: None,
            tool_choice: None,
            thinking: None,
            output_config: None,
            metadata: None,
        };

        let result = convert_request(&req, &CompressionConfig::default()).unwrap();
        // 验证生成的是有效的 UUID 格式
        assert_eq!(result.conversation_state.conversation_id.len(), 36);
        assert_eq!(
            result
                .conversation_state
                .conversation_id
                .chars()
                .filter(|c| *c == '-')
                .count(),
            4
        );
    }

    #[test]
    fn test_validate_tool_pairing_orphaned_result() {
        // 测试孤立的 tool_result 被过滤
        // 历史中没有 tool_use，但 tool_results 中有 tool_result
        let history = vec![
            Message::User(HistoryUserMessage::new("Hello", "claude-sonnet-4.5")),
            Message::Assistant(HistoryAssistantMessage::new("Hi there!")),
        ];

        let tool_results = vec![ToolResult::success("orphan-123", "some result")];

        let (filtered, _) = validate_tool_pairing(&history, &tool_results);

        // 孤立的 tool_result 应该被过滤掉
        assert!(filtered.is_empty(), "孤立的 tool_result 应该被过滤");
    }

    #[test]
    fn test_validate_tool_pairing_orphaned_use() {
        use crate::kiro::model::requests::tool::ToolUseEntry;

        // 测试孤立的 tool_use（有 tool_use 但没有对应的 tool_result）
        let mut assistant_msg = AssistantMessage::new("I'll read the file.");
        assistant_msg = assistant_msg.with_tool_uses(vec![
            ToolUseEntry::new("tool-orphan", "read")
                .with_input(serde_json::json!({"path": "/test.txt"})),
        ]);

        let history = vec![
            Message::User(HistoryUserMessage::new(
                "Read the file",
                "claude-sonnet-4.5",
            )),
            Message::Assistant(HistoryAssistantMessage {
                assistant_response_message: assistant_msg,
            }),
        ];

        // 没有 tool_result
        let tool_results: Vec<ToolResult> = vec![];

        let (filtered, orphaned) = validate_tool_pairing(&history, &tool_results);

        // 结果应该为空（因为没有 tool_result）
        // 同时应该返回孤立的 tool_use_id
        assert!(filtered.is_empty());
        assert!(orphaned.contains("tool-orphan"));
    }

    #[test]
    fn test_validate_tool_pairing_valid() {
        use crate::kiro::model::requests::tool::ToolUseEntry;

        // 测试正常配对的情况
        let mut assistant_msg = AssistantMessage::new("I'll read the file.");
        assistant_msg = assistant_msg.with_tool_uses(vec![
            ToolUseEntry::new("tool-1", "read")
                .with_input(serde_json::json!({"path": "/test.txt"})),
        ]);

        let history = vec![
            Message::User(HistoryUserMessage::new(
                "Read the file",
                "claude-sonnet-4.5",
            )),
            Message::Assistant(HistoryAssistantMessage {
                assistant_response_message: assistant_msg,
            }),
        ];

        let tool_results = vec![ToolResult::success("tool-1", "file content")];

        let (filtered, orphaned) = validate_tool_pairing(&history, &tool_results);

        // 配对成功，应该保留，无孤立
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].tool_use_id, "tool-1");
        assert!(orphaned.is_empty());
    }

    #[test]
    fn test_validate_tool_pairing_mixed() {
        use crate::kiro::model::requests::tool::ToolUseEntry;

        // 测试混合情况：部分配对成功，部分孤立
        let mut assistant_msg = AssistantMessage::new("I'll use two tools.");
        assistant_msg = assistant_msg.with_tool_uses(vec![
            ToolUseEntry::new("tool-1", "read").with_input(serde_json::json!({})),
            ToolUseEntry::new("tool-2", "write").with_input(serde_json::json!({})),
        ]);

        let history = vec![
            Message::User(HistoryUserMessage::new("Do something", "claude-sonnet-4.5")),
            Message::Assistant(HistoryAssistantMessage {
                assistant_response_message: assistant_msg,
            }),
        ];

        // tool_results: tool-1 配对，tool-3 孤立
        let tool_results = vec![
            ToolResult::success("tool-1", "result 1"),
            ToolResult::success("tool-3", "orphan result"), // 孤立
        ];

        let (filtered, orphaned) = validate_tool_pairing(&history, &tool_results);

        // 只有 tool-1 应该保留
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].tool_use_id, "tool-1");
        // tool-2 是孤立的 tool_use（无 result），tool-3 是孤立的 tool_result
        assert!(orphaned.contains("tool-2"));
    }

    #[test]
    fn test_validate_tool_pairing_history_already_paired() {
        use crate::kiro::model::requests::tool::ToolUseEntry;

        // 测试历史中已配对的 tool_use 不应该被报告为孤立
        // 场景：多轮对话中，之前的 tool_use 已经在历史中有对应的 tool_result
        let mut assistant_msg1 = AssistantMessage::new("I'll read the file.");
        assistant_msg1 = assistant_msg1.with_tool_uses(vec![
            ToolUseEntry::new("tool-1", "read")
                .with_input(serde_json::json!({"path": "/test.txt"})),
        ]);

        // 构建历史中的 user 消息，包含 tool_result
        let mut user_msg_with_result = UserMessage::new("", "claude-sonnet-4.5");
        let mut ctx = UserInputMessageContext::new();
        ctx = ctx.with_tool_results(vec![ToolResult::success("tool-1", "file content")]);
        user_msg_with_result = user_msg_with_result.with_context(ctx);

        let history = vec![
            // 第一轮：用户请求
            Message::User(HistoryUserMessage::new(
                "Read the file",
                "claude-sonnet-4.5",
            )),
            // 第一轮：assistant 使用工具
            Message::Assistant(HistoryAssistantMessage {
                assistant_response_message: assistant_msg1,
            }),
            // 第二轮：用户返回工具结果（历史中已配对）
            Message::User(HistoryUserMessage {
                user_input_message: user_msg_with_result,
            }),
            // 第二轮：assistant 响应
            Message::Assistant(HistoryAssistantMessage::new("The file contains...")),
        ];

        // 当前消息没有 tool_results（用户只是继续对话）
        let tool_results: Vec<ToolResult> = vec![];

        let (filtered, orphaned) = validate_tool_pairing(&history, &tool_results);

        // 结果应该为空，且不应该有孤立 tool_use
        // 因为 tool-1 已经在历史中配对了
        assert!(filtered.is_empty());
        assert!(orphaned.is_empty());
    }

    #[test]
    fn test_validate_tool_pairing_duplicate_result() {
        use crate::kiro::model::requests::tool::ToolUseEntry;

        // 测试重复的 tool_result（历史中已配对，当前消息又发送了相同的 tool_result）
        let mut assistant_msg = AssistantMessage::new("I'll read the file.");
        assistant_msg = assistant_msg.with_tool_uses(vec![
            ToolUseEntry::new("tool-1", "read")
                .with_input(serde_json::json!({"path": "/test.txt"})),
        ]);

        // 历史中已有 tool_result
        let mut user_msg_with_result = UserMessage::new("", "claude-sonnet-4.5");
        let mut ctx = UserInputMessageContext::new();
        ctx = ctx.with_tool_results(vec![ToolResult::success("tool-1", "file content")]);
        user_msg_with_result = user_msg_with_result.with_context(ctx);

        let history = vec![
            Message::User(HistoryUserMessage::new(
                "Read the file",
                "claude-sonnet-4.5",
            )),
            Message::Assistant(HistoryAssistantMessage {
                assistant_response_message: assistant_msg,
            }),
            Message::User(HistoryUserMessage {
                user_input_message: user_msg_with_result,
            }),
            Message::Assistant(HistoryAssistantMessage::new("Done")),
        ];

        // 当前消息又发送了相同的 tool_result（重复）
        let tool_results = vec![ToolResult::success("tool-1", "file content again")];

        let (filtered, _) = validate_tool_pairing(&history, &tool_results);

        // 重复的 tool_result 应该被过滤掉
        assert!(filtered.is_empty(), "重复的 tool_result 应该被过滤");
    }

    #[test]
    fn test_convert_assistant_message_tool_use_only() {
        use super::super::types::Message as AnthropicMessage;

        // 测试仅包含 tool_use 的 assistant 消息（无 text 块）
        // 转换阶段应保留结构化 tool_uses，不主动补 "." 占位符
        let msg = AnthropicMessage {
            role: "assistant".to_string(),
            content: serde_json::json!([
                {"type": "tool_use", "id": "toolu_01ABC", "name": "read_file", "input": {"path": "/test.txt"}}
            ]),
        };

        let result = convert_assistant_message(&msg, &mut HashMap::new()).expect("应该成功转换");

        assert!(
            result.assistant_response_message.content.is_empty(),
            "仅 tool_use 时转换阶段不应主动补 '.'"
        );

        // 验证 tool_uses 被正确保留
        let tool_uses = result
            .assistant_response_message
            .tool_uses
            .expect("应该有 tool_uses");
        assert_eq!(tool_uses.len(), 1);
        assert_eq!(tool_uses[0].tool_use_id, "toolu_01ABC");
        assert_eq!(tool_uses[0].name, "read_file");
    }

    #[test]
    fn test_convert_assistant_message_with_text_and_tool_use() {
        use super::super::types::Message as AnthropicMessage;

        // 测试同时包含 text 和 tool_use 的 assistant 消息
        let msg = AnthropicMessage {
            role: "assistant".to_string(),
            content: serde_json::json!([
                {"type": "text", "text": "Let me read that file for you."},
                {"type": "tool_use", "id": "toolu_02XYZ", "name": "read_file", "input": {"path": "/data.json"}}
            ]),
        };

        let result = convert_assistant_message(&msg, &mut HashMap::new()).expect("应该成功转换");

        // 验证 content 使用原始文本（不是占位符）
        assert_eq!(
            result.assistant_response_message.content,
            "Let me read that file for you."
        );

        // 验证 tool_uses 被正确保留
        let tool_uses = result
            .assistant_response_message
            .tool_uses
            .expect("应该有 tool_uses");
        assert_eq!(tool_uses.len(), 1);
        assert_eq!(tool_uses[0].tool_use_id, "toolu_02XYZ");
    }

    #[test]
    fn test_convert_assistant_message_web_search_tool_result() {
        use super::super::types::Message as AnthropicMessage;

        // 测试 web_search_tool_result 块被提取为纯文本（title、url、snippet、page_age）保留到历史
        let msg = AnthropicMessage {
            role: "assistant".to_string(),
            content: serde_json::json!([
                {"type": "server_tool_use", "id": "srvtoolu_01ABC", "name": "web_search", "input": {"query": "rust async"}},
                {
                    "type": "web_search_tool_result",
                    "content": [
                        {
                            "type": "web_search_result",
                            "title": "Async in Rust",
                            "url": "https://rust-lang.org/async",
                            "encrypted_content": "Rust async/await guide.",
                            "page_age": "January 1, 2025"
                        },
                        {"type": "web_search_result", "title": "", "url": "https://example.com/no-title",
                         "encrypted_content": "", "page_age": null}
                    ]
                }
            ]),
        };

        let result = convert_assistant_message(&msg, &mut HashMap::new()).expect("应该成功转换");
        let content = &result.assistant_response_message.content;

        assert!(
            content.contains("Async in Rust: https://rust-lang.org/async"),
            "有 title 时应输出 'title: url'"
        );
        assert!(content.contains("Date: January 1, 2025"), "page_age 应保留");
        assert!(
            content.contains("Rust async/await guide."),
            "snippet 应保留"
        );
        assert!(
            content.contains("https://example.com/no-title"),
            "title 为空时应输出纯 URL"
        );
        assert!(
            !content.contains("srvtoolu_01ABC"),
            "server_tool_use 应被忽略"
        );
    }

    #[test]
    fn test_convert_assistant_message_web_search_result_control_chars() {
        use super::super::types::Message as AnthropicMessage;

        // 测试 title 含控制字符时被过滤
        let msg = AnthropicMessage {
            role: "assistant".to_string(),
            content: serde_json::json!([
                {
                    "type": "web_search_tool_result",
                    "content": [
                        {"type": "web_search_result", "title": "Title\nWith\tControl", "url": "https://example.com"}
                    ]
                }
            ]),
        };

        let result = convert_assistant_message(&msg, &mut HashMap::new()).expect("应该成功转换");
        let content = &result.assistant_response_message.content;

        // 控制字符被过滤，不应出现换行和 tab
        assert!(!content.contains('\t'), "tab 字符应被过滤");
        // 内容应包含 URL
        assert!(content.contains("https://example.com"), "URL 应保留");
    }

    #[test]
    fn test_convert_tools_filters_web_search() {
        use super::super::types::Tool as AnthropicTool;
        use std::collections::HashMap;

        // 测试 web_search 工具被过滤
        // Kiro API 当前不支持 web_search，需要自动过滤
        let tools = vec![
            // web_search 工具（应被过滤）
            AnthropicTool {
                tool_type: Some("web_search_20250305".to_string()),
                name: "web_search".to_string(),
                description: String::new(),
                input_schema: HashMap::new(),
                max_uses: Some(8),
                cache_control: None,
            },
            // 普通工具（应保留）
            AnthropicTool {
                tool_type: None,
                name: "read_file".to_string(),
                description: "Read a file from disk".to_string(),
                input_schema: {
                    let mut schema = HashMap::new();
                    schema.insert("type".to_string(), serde_json::json!("object"));
                    schema
                },
                max_uses: None,
                cache_control: None,
            },
        ];

        let converted = convert_tools(&Some(tools), 4000, &mut HashMap::new());

        // 应该只有 1 个工具（web_search 被过滤）
        assert_eq!(converted.len(), 1, "web_search 应该被过滤");
        assert_eq!(
            converted[0].tool_specification.name, "read_file",
            "只应保留 read_file 工具"
        );
    }

    #[test]
    fn test_convert_tools_filters_all_web_search_variants() {
        use super::super::types::Tool as AnthropicTool;
        use std::collections::HashMap;

        // 测试所有 web_search 变体都被过滤
        let tools = vec![
            AnthropicTool {
                tool_type: Some("web_search_20250305".to_string()),
                name: "web_search".to_string(),
                description: String::new(),
                input_schema: HashMap::new(),
                max_uses: Some(8),
                cache_control: None,
            },
            AnthropicTool {
                tool_type: Some("web_search_20260101".to_string()), // 假设的未来版本
                name: "web_search".to_string(),
                description: String::new(),
                input_schema: HashMap::new(),
                max_uses: Some(10),
                cache_control: None,
            },
        ];

        let converted = convert_tools(&Some(tools), 4000, &mut HashMap::new());

        // 所有 web_search 工具都应被过滤
        assert!(converted.is_empty(), "所有 web_search 变体都应被过滤");
    }

    #[test]
    fn test_convert_tools_fills_empty_description_and_normalizes_schema() {
        use super::super::types::{Message as AnthropicMessage, Tool as AnthropicTool};
        use std::collections::HashMap;

        let mut input_schema = HashMap::new();
        input_schema.insert("type".to_string(), serde_json::json!("object"));

        let req = MessagesRequest {
            model: "claude-sonnet-4".to_string(),
            max_tokens: 128,
            messages: vec![AnthropicMessage {
                role: "user".to_string(),
                content: serde_json::json!("hi"),
            }],
            stream: false,
            system: None,
            tools: Some(vec![AnthropicTool {
                tool_type: None,
                name: "mcp__ida-pro-mcp__patch_address_assembles".to_string(),
                description: "".to_string(), // 上游可能拒绝空 description
                input_schema,                // 故意不带 $schema 等字段
                max_uses: None,
                cache_control: None,
            }]),
            tool_choice: None,
            thinking: None,
            output_config: None,
            metadata: None,
        };

        let result = convert_request(&req, &CompressionConfig::default()).unwrap();
        let tools = &result
            .conversation_state
            .current_message
            .user_input_message
            .user_input_message_context
            .tools;

        let tool = tools
            .iter()
            .find(|t| t.tool_specification.name == "mcp__ida-pro-mcp__patch_address_assembles")
            .expect("转换后应包含该工具");

        assert!(
            !tool.tool_specification.description.trim().is_empty(),
            "转换后的工具描述不应为空"
        );
        // Round 6: `$schema` is no longer auto-injected when missing from user
        // input (wire alignment with kiro-cli 2.3.0). It stays absent here.
        assert!(
            tool.tool_specification.input_schema.json["$schema"].is_null(),
            "$schema 缺失时不应自动注入"
        );
        // `type` IS still backfilled — it's a real MCP-anomaly target (TC-07).
        assert_eq!(tool.tool_specification.input_schema.json["type"], "object");
    }

    #[test]
    fn test_current_message_content_is_non_empty_when_only_tool_result() {
        use super::super::types::Message as AnthropicMessage;

        // 构造典型 tool_use -> tool_result 链路，最后一条为 tool_result user 消息
        let req = MessagesRequest {
            model: "claude-sonnet-4".to_string(),
            max_tokens: 128,
            messages: vec![
                AnthropicMessage {
                    role: "user".to_string(),
                    content: serde_json::json!("do it"),
                },
                AnthropicMessage {
                    role: "assistant".to_string(),
                    content: serde_json::json!([
                        {"type": "tool_use", "id": "tool-1", "name": "read", "input": {"path": "/tmp/a"}}
                    ]),
                },
                AnthropicMessage {
                    role: "user".to_string(),
                    content: serde_json::json!([
                        {"type": "tool_result", "tool_use_id": "tool-1", "content": "ok"}
                    ]),
                },
            ],
            stream: false,
            system: None,
            tools: None,
            tool_choice: None,
            thinking: None,
            output_config: None,
            metadata: None,
        };

        let result = convert_request(&req, &CompressionConfig::default()).unwrap();
        let content = &result
            .conversation_state
            .current_message
            .user_input_message
            .content;

        assert!(
            content.is_empty(),
            "仅有效 tool_result 的 current user 消息不应在早期转换阶段补 '.'"
        );
    }

    #[test]
    fn test_history_user_message_content_is_non_empty_when_only_tool_result() {
        use super::super::types::Message as AnthropicMessage;

        // 让 tool_result 进入 history：tool_result 后紧跟 assistant，然后用户继续提问
        let req = MessagesRequest {
            model: "claude-sonnet-4".to_string(),
            max_tokens: 128,
            messages: vec![
                AnthropicMessage {
                    role: "user".to_string(),
                    content: serde_json::json!("do it"),
                },
                AnthropicMessage {
                    role: "assistant".to_string(),
                    content: serde_json::json!([
                        {"type": "tool_use", "id": "tool-1", "name": "read", "input": {"path": "/tmp/a"}}
                    ]),
                },
                AnthropicMessage {
                    role: "user".to_string(),
                    content: serde_json::json!([
                        {"type": "tool_result", "tool_use_id": "tool-1", "content": "ok"}
                    ]),
                },
                AnthropicMessage {
                    role: "assistant".to_string(),
                    content: serde_json::json!("done"),
                },
                AnthropicMessage {
                    role: "user".to_string(),
                    content: serde_json::json!("next"),
                },
            ],
            stream: false,
            system: None,
            tools: None,
            tool_choice: None,
            thinking: None,
            output_config: None,
            metadata: None,
        };

        let result = convert_request(&req, &CompressionConfig::default()).unwrap();

        let mut found = false;
        for msg in &result.conversation_state.history {
            let Message::User(user_msg) = msg else {
                continue;
            };
            let ctx = &user_msg.user_input_message.user_input_message_context;
            if ctx.tool_results.is_empty() {
                continue;
            }
            found = true;
            assert!(
                user_msg.user_input_message.content.is_empty(),
                "history 中仅含有效 tool_result 的 user 消息不应在早期转换阶段补 '.'"
            );
        }
        assert!(found, "测试数据应在 history 中包含 tool_results");
    }

    #[test]
    fn test_current_message_content_is_non_empty_when_tool_result_filtered_as_orphan() {
        use super::super::types::{Message as AnthropicMessage, Tool as AnthropicTool};
        use std::collections::HashMap;

        // 场景：当前消息仅有 tool_result，但 tool_use_id 与历史不匹配（会被过滤为孤立结果）
        // 过滤后当前消息无文本/无 tool_result，必须仍保留非空 content 占位符，避免上游 400。
        let req = MessagesRequest {
            model: "claude-sonnet-4".to_string(),
            max_tokens: 128,
            messages: vec![
                AnthropicMessage {
                    role: "user".to_string(),
                    content: serde_json::json!("请读取配置"),
                },
                AnthropicMessage {
                    role: "assistant".to_string(),
                    content: serde_json::json!([
                        {"type": "tool_use", "id": "tooluse_valid_1", "name": "read_file", "input": {"path": "/tmp/a"}}
                    ]),
                },
                AnthropicMessage {
                    role: "user".to_string(),
                    content: serde_json::json!([
                        {"type": "tool_result", "tool_use_id": "toolu_orphan_1", "content": "ok"}
                    ]),
                },
            ],
            stream: false,
            system: None,
            tools: Some(vec![AnthropicTool {
                tool_type: None,
                name: "read_file".to_string(),
                description: "read".to_string(),
                input_schema: HashMap::new(),
                max_uses: None,
                cache_control: None,
            }]),
            tool_choice: None,
            thinking: None,
            output_config: None,
            metadata: None,
        };

        let result = convert_request(&req, &CompressionConfig::default()).unwrap();

        // 孤立 tool_result 会被过滤
        assert!(
            result
                .conversation_state
                .current_message
                .user_input_message
                .user_input_message_context
                .tool_results
                .is_empty(),
            "孤立 tool_result 应被过滤"
        );

        // 过滤后 content 仍应为非空占位符，避免上游拒绝请求
        assert_eq!(
            result
                .conversation_state
                .current_message
                .user_input_message
                .content,
            "."
        );
    }

    #[test]
    fn test_remove_orphaned_tool_uses() {
        use crate::kiro::model::requests::tool::ToolUseEntry;

        // 测试从历史中移除孤立的 tool_use
        let mut assistant_msg = AssistantMessage::new("I'll use multiple tools.");
        assistant_msg = assistant_msg.with_tool_uses(vec![
            ToolUseEntry::new("tool-1", "read").with_input(serde_json::json!({})),
            ToolUseEntry::new("tool-2", "write").with_input(serde_json::json!({})),
            ToolUseEntry::new("tool-3", "delete").with_input(serde_json::json!({})),
        ]);

        let mut history = vec![
            Message::User(HistoryUserMessage::new("Do something", "claude-sonnet-4.5")),
            Message::Assistant(HistoryAssistantMessage {
                assistant_response_message: assistant_msg,
            }),
        ];

        // 移除 tool-1 和 tool-3
        let mut orphaned = std::collections::HashSet::new();
        orphaned.insert("tool-1".to_string());
        orphaned.insert("tool-3".to_string());

        remove_orphaned_tool_uses(&mut history, &orphaned);

        // 验证只剩下 tool-2
        if let Message::Assistant(ref assistant_msg) = history[1] {
            let tool_uses = assistant_msg
                .assistant_response_message
                .tool_uses
                .as_ref()
                .expect("应该还有 tool_uses");
            assert_eq!(tool_uses.len(), 1);
            assert_eq!(tool_uses[0].tool_use_id, "tool-2");
        } else {
            panic!("应该是 Assistant 消息");
        }
    }

    #[test]
    fn test_remove_orphaned_tool_uses_all_removed() {
        use crate::kiro::model::requests::tool::ToolUseEntry;

        // 测试移除所有 tool_use 后，tool_uses 变为 None
        let mut assistant_msg = AssistantMessage::new("I'll use a tool.");
        assistant_msg = assistant_msg.with_tool_uses(vec![
            ToolUseEntry::new("tool-1", "read").with_input(serde_json::json!({})),
        ]);

        let mut history = vec![
            Message::User(HistoryUserMessage::new("Do something", "claude-sonnet-4.5")),
            Message::Assistant(HistoryAssistantMessage {
                assistant_response_message: assistant_msg,
            }),
        ];

        let mut orphaned = std::collections::HashSet::new();
        orphaned.insert("tool-1".to_string());

        remove_orphaned_tool_uses(&mut history, &orphaned);

        // 验证 tool_uses 变为 None
        if let Message::Assistant(ref assistant_msg) = history[1] {
            assert!(
                assistant_msg.assistant_response_message.tool_uses.is_none(),
                "移除所有 tool_use 后应为 None"
            );
        } else {
            panic!("应该是 Assistant 消息");
        }
    }

    #[test]
    fn test_normalize_json_schema_coerces_field_types() {
        let input = serde_json::json!({
            "$schema": null,
            "type": null,
            "properties": null,
            "required": null,
            "additionalProperties": null,
        });

        let normalized = normalize_json_schema(input);

        assert_eq!(
            normalized.get("$schema").and_then(|v| v.as_str()),
            Some("http://json-schema.org/draft-07/schema#")
        );
        assert_eq!(
            normalized.get("type").and_then(|v| v.as_str()),
            Some("object")
        );
        assert!(normalized.get("properties").is_some_and(|v| v.is_object()));
        assert!(normalized.get("required").is_some_and(|v| v.is_array()));
        assert!(
            normalized
                .get("additionalProperties")
                .is_some_and(|v| v.is_boolean())
        );
    }

    /// Round 7 regression: image content blocks convert to KiroImage with the
    /// correct format + bytes wire shape. Round 4 wire 字节级对比员实测金准是
    /// `{"format":"png","source":{"bytes":"..."}}`，四种 media_type 都该支持。
    #[test]
    fn test_image_wire_shape_matches_kiro_cli_format() {
        use super::super::types::Message as AnthropicMessage;
        // Minimal PNG header (4 bytes is plenty for a wire-shape test).
        let b64 = "iVBORw0KGgo=";
        for (media_type, expected_format) in &[
            ("image/png", "png"),
            ("image/jpeg", "jpeg"),
            ("image/gif", "gif"),
            ("image/webp", "webp"),
        ] {
            let req = MessagesRequest {
                model: "claude-sonnet-4".to_string(),
                max_tokens: 32,
                messages: vec![AnthropicMessage {
                    role: "user".to_string(),
                    content: serde_json::json!([
                        {
                            "type": "image",
                            "source": {"type": "base64", "media_type": media_type, "data": b64}
                        },
                        {"type": "text", "text": "describe"},
                    ]),
                }],
                stream: false,
                system: None,
                tools: None,
                tool_choice: None,
                thinking: None,
                output_config: None,
                metadata: None,
            };
            let result = convert_request(&req, &CompressionConfig::default()).unwrap();
            let imgs = &result
                .conversation_state
                .current_message
                .user_input_message
                .images;
            assert_eq!(
                imgs.len(),
                1,
                "media_type {} should yield 1 image",
                media_type
            );
            assert_eq!(
                imgs[0].format, *expected_format,
                "format mapping for {}",
                media_type
            );
            assert!(
                !imgs[0].source.bytes.is_empty(),
                "source.bytes must be populated"
            );
        }
    }

    /// Round 6 regression: `$schema` and `additionalProperties` are now wire-aligned
    /// with kiro-cli 2.3.0 — when missing from user-provided schema, they stay
    /// missing (not auto-injected). This shaves ~80B per tool off the prefix-cache
    /// key. The other three fields (type / properties / required) still backfill.
    #[test]
    fn test_normalize_json_schema_does_not_inject_missing_schema_field() {
        let input = serde_json::json!({
            "type": "object",
            "properties": {"x": {"type": "string"}},
        });
        let normalized = normalize_json_schema(input);
        assert!(
            normalized.get("$schema").is_none(),
            "missing $schema must NOT be auto-injected (wire alignment with kiro-cli)"
        );
        assert!(
            normalized.get("additionalProperties").is_none(),
            "missing additionalProperties must NOT be auto-injected"
        );
        // But type/properties/required still ensured present (these treat MCP anomalies).
        assert_eq!(
            normalized.get("type").and_then(|v| v.as_str()),
            Some("object")
        );
        assert!(normalized.get("properties").is_some());
        assert!(normalized.get("required").is_some());
    }

    #[test]
    fn test_normalize_json_schema_filters_required_non_strings() {
        let input = serde_json::json!({
            "type": "object",
            "properties": {},
            "required": ["a", 1, null, {"x": 1}],
        });

        let normalized = normalize_json_schema(input);
        let required = normalized
            .get("required")
            .and_then(|v| v.as_array())
            .expect("required 应该是数组");

        assert_eq!(required, &vec![serde_json::Value::String("a".to_string())]);
    }

    #[test]
    fn test_chunked_policy_injected_only_with_write_edit_tools() {
        use super::super::types::{
            Message as AnthropicMessage, SystemMessage, Tool as AnthropicTool,
        };
        use std::collections::HashMap;

        let system = vec![SystemMessage {
            text: "You are a helpful assistant.".to_string(),
            block_type: None,
            cache_control: None,
        }];

        // 无工具 → 不注入 chunked policy
        let req_no_tools = MessagesRequest {
            model: "claude-sonnet-4".to_string(),
            max_tokens: 1024,
            messages: vec![AnthropicMessage {
                role: "user".to_string(),
                content: serde_json::json!("hello"),
            }],
            stream: false,
            system: Some(system.clone()),
            tools: None,
            tool_choice: None,
            thinking: None,
            output_config: None,
            metadata: None,
        };

        let result = convert_request(&req_no_tools, &CompressionConfig::default()).unwrap();
        let first_user = &result.conversation_state.history[0];
        match first_user {
            Message::User(u) => {
                assert!(
                    !u.user_input_message.content.contains("chunked operations"),
                    "无工具时不应注入 chunked policy"
                );
            }
            _ => panic!("history[0] 应该是 User 消息"),
        }

        // 有 Write 工具 → 注入 chunked policy
        let req_with_write = MessagesRequest {
            model: "claude-sonnet-4".to_string(),
            max_tokens: 1024,
            messages: vec![AnthropicMessage {
                role: "user".to_string(),
                content: serde_json::json!("hello"),
            }],
            stream: false,
            system: Some(system.clone()),
            tools: Some(vec![AnthropicTool {
                tool_type: None,
                name: "Write".to_string(),
                description: "Write a file".to_string(),
                input_schema: HashMap::new(),
                max_uses: None,
                cache_control: None,
            }]),
            tool_choice: None,
            thinking: None,
            output_config: None,
            metadata: None,
        };

        let result = convert_request(&req_with_write, &CompressionConfig::default()).unwrap();
        let first_user = &result.conversation_state.history[0];
        match first_user {
            Message::User(u) => {
                assert!(
                    u.user_input_message.content.contains("chunked operations"),
                    "有 Write 工具时应注入 chunked policy"
                );
            }
            _ => panic!("history[0] 应该是 User 消息"),
        }

        // system: None + 有 Edit 工具 → 也应注入 chunked policy
        let req_no_system_with_edit = MessagesRequest {
            model: "claude-sonnet-4".to_string(),
            max_tokens: 1024,
            messages: vec![AnthropicMessage {
                role: "user".to_string(),
                content: serde_json::json!("hello"),
            }],
            stream: false,
            system: None,
            tools: Some(vec![AnthropicTool {
                tool_type: None,
                name: "Edit".to_string(),
                description: "Edit a file".to_string(),
                input_schema: HashMap::new(),
                max_uses: None,
                cache_control: None,
            }]),
            tool_choice: None,
            thinking: None,
            output_config: None,
            metadata: None,
        };

        let result =
            convert_request(&req_no_system_with_edit, &CompressionConfig::default()).unwrap();
        let first_user = &result.conversation_state.history[0];
        match first_user {
            Message::User(u) => {
                assert!(
                    u.user_input_message.content.contains("chunked operations"),
                    "system: None + 有 Edit 工具时也应注入 chunked policy"
                );
            }
            _ => panic!("history[0] 应该是 User 消息"),
        }
    }

    #[test]
    fn test_effort_whitelist_fallback() {
        use super::super::types::{Message as AnthropicMessage, OutputConfig, Thinking};

        // 合法值 "low"
        let req = MessagesRequest {
            model: "claude-sonnet-4".to_string(),
            max_tokens: 1024,
            messages: vec![AnthropicMessage {
                role: "user".to_string(),
                content: serde_json::json!("hello"),
            }],
            stream: false,
            system: None,
            tools: None,
            tool_choice: None,
            thinking: Some(Thinking {
                thinking_type: "adaptive".to_string(),
                budget_tokens: 0,
            }),
            output_config: Some(OutputConfig {
                effort: "low".to_string(),
            }),
            metadata: None,
        };

        let prefix = generate_thinking_prefix(&req).unwrap();
        assert!(prefix.contains("<thinking_effort>low</thinking_effort>"));

        // 非法值 → 回退 "high"
        let req_invalid = MessagesRequest {
            model: "claude-sonnet-4".to_string(),
            max_tokens: 1024,
            messages: vec![AnthropicMessage {
                role: "user".to_string(),
                content: serde_json::json!("hello"),
            }],
            stream: false,
            system: None,
            tools: None,
            tool_choice: None,
            thinking: Some(Thinking {
                thinking_type: "adaptive".to_string(),
                budget_tokens: 0,
            }),
            output_config: Some(OutputConfig {
                effort: "ultra".to_string(),
            }),
            metadata: None,
        };

        let prefix = generate_thinking_prefix(&req_invalid).unwrap();
        assert!(
            prefix.contains("<thinking_effort>high</thinking_effort>"),
            "非法 effort 值应回退为 high，实际: {}",
            prefix
        );
    }

    #[test]
    fn test_collect_history_tool_names_deduplicates_case_variants() {
        use crate::kiro::model::requests::tool::ToolUseEntry;

        // 历史中同时出现 "read" 和 "Read"（大小写变体），应只保留首次出现的形式
        let mut msg1 = AssistantMessage::new("reading...");
        msg1 = msg1.with_tool_uses(vec![
            ToolUseEntry::new("t-1", "read").with_input(serde_json::json!({"path": "/a.txt"})),
        ]);

        let mut msg2 = AssistantMessage::new("reading again...");
        msg2 = msg2.with_tool_uses(vec![
            ToolUseEntry::new("t-2", "Read").with_input(serde_json::json!({"path": "/b.txt"})),
        ]);

        let history = vec![
            Message::User(HistoryUserMessage::new("go", "claude-sonnet-4.5")),
            Message::Assistant(HistoryAssistantMessage {
                assistant_response_message: msg1,
            }),
            Message::User(HistoryUserMessage::new("ok", "claude-sonnet-4.5")),
            Message::Assistant(HistoryAssistantMessage {
                assistant_response_message: msg2,
            }),
        ];

        let tool_names = collect_history_tool_names(&history);
        // 只应有 1 个条目（首次出现的 "read"）
        assert_eq!(
            tool_names.len(),
            1,
            "大小写变体应被去重，实际: {:?}",
            tool_names
        );
        assert_eq!(tool_names[0], "read");
    }

    #[test]
    fn test_convert_request_handles_assistant_prefill() {
        // 末尾 assistant 消息（prefill）应被静默丢弃，使用最后一条 user 消息作为 current_message
        use super::super::types::Message as AnthropicMessage;

        let req = MessagesRequest {
            model: "claude-sonnet-4".to_string(),
            max_tokens: 1024,
            messages: vec![
                AnthropicMessage {
                    role: "user".to_string(),
                    content: serde_json::json!("Hello"),
                },
                AnthropicMessage {
                    role: "assistant".to_string(),
                    content: serde_json::json!("Hi there"),
                },
            ],
            stream: false,
            system: None,
            tools: None,
            tool_choice: None,
            thinking: None,
            output_config: None,
            metadata: None,
        };

        let result = convert_request(&req, &CompressionConfig::default());
        assert!(result.is_ok(), "prefill 场景不应报错: {:?}", result.err());
        let state = result.unwrap().conversation_state;
        assert_eq!(
            state.current_message.user_input_message.content, "Hello",
            "current_message 应为最后一条 user 消息的内容"
        );
    }

    #[test]
    fn test_convert_request_prefill_no_user_message() {
        // 只有 assistant 消息、没有 user 消息时应返回 EmptyMessages 错误
        use super::super::types::Message as AnthropicMessage;

        let req = MessagesRequest {
            model: "claude-sonnet-4".to_string(),
            max_tokens: 1024,
            messages: vec![AnthropicMessage {
                role: "assistant".to_string(),
                content: serde_json::json!("Hi there"),
            }],
            stream: false,
            system: None,
            tools: None,
            tool_choice: None,
            thinking: None,
            output_config: None,
            metadata: None,
        };

        let err = convert_request(&req, &CompressionConfig::default()).unwrap_err();
        assert!(
            matches!(err, ConversionError::EmptyMessages),
            "只有 assistant 消息时应返回 EmptyMessages，实际: {:?}",
            err
        );
    }

    #[test]
    fn test_convert_request_empty_message_content() {
        // 测试空消息内容应返回 EmptyMessageContent 错误
        use super::super::types::Message as AnthropicMessage;

        let req = MessagesRequest {
            model: "claude-sonnet-4".to_string(),
            max_tokens: 1024,
            messages: vec![AnthropicMessage {
                role: "user".to_string(),
                content: serde_json::json!(""),
            }],
            stream: false,
            system: None,
            tools: None,
            tool_choice: None,
            thinking: None,
            output_config: None,
            metadata: None,
        };

        let err = convert_request(&req, &CompressionConfig::default()).unwrap_err();
        assert!(
            matches!(err, ConversionError::EmptyMessageContent),
            "空消息内容应返回 EmptyMessageContent，实际: {:?}",
            err
        );
    }

    #[test]
    fn test_convert_request_empty_text_block() {
        // 测试仅包含空白文本块的消息应返回 EmptyMessageContent 错误
        use super::super::types::Message as AnthropicMessage;

        let req = MessagesRequest {
            model: "claude-sonnet-4".to_string(),
            max_tokens: 1024,
            messages: vec![AnthropicMessage {
                role: "user".to_string(),
                content: serde_json::json!([
                    {"type": "text", "text": "   "},
                    {"type": "text", "text": "\n\t"}
                ]),
            }],
            stream: false,
            system: None,
            tools: None,
            tool_choice: None,
            thinking: None,
            output_config: None,
            metadata: None,
        };

        let err = convert_request(&req, &CompressionConfig::default()).unwrap_err();
        assert!(
            matches!(err, ConversionError::EmptyMessageContent),
            "仅包含空白文本的消息应返回 EmptyMessageContent，实际: {:?}",
            err
        );
    }

    #[test]
    fn test_convert_request_prefill_with_empty_user_message() {
        // 测试 prefill 场景下，如果回退后的 user 消息为空，应返回 EmptyMessageContent 错误
        use super::super::types::Message as AnthropicMessage;

        let req = MessagesRequest {
            model: "claude-sonnet-4".to_string(),
            max_tokens: 1024,
            messages: vec![
                AnthropicMessage {
                    role: "user".to_string(),
                    content: serde_json::json!(""),
                },
                AnthropicMessage {
                    role: "assistant".to_string(),
                    content: serde_json::json!("Hi there"),
                },
            ],
            stream: false,
            system: None,
            tools: None,
            tool_choice: None,
            thinking: None,
            output_config: None,
            metadata: None,
        };

        let err = convert_request(&req, &CompressionConfig::default()).unwrap_err();
        assert!(
            matches!(err, ConversionError::EmptyMessageContent),
            "prefill 回退后的空 user 消息应返回 EmptyMessageContent，实际: {:?}",
            err
        );
    }

    #[test]
    fn test_merge_consecutive_assistant_messages() {
        // 测试连续 assistant 消息被正确合并（Issue #79）
        use super::super::types::Message as AnthropicMessage;

        // 第一条 assistant 消息：只有 thinking 和空 text
        let msg1 = AnthropicMessage {
            role: "assistant".to_string(),
            content: serde_json::json!([
                {"type": "thinking", "thinking": "Let me think about this..."},
                {"type": "text", "text": " "}
            ]),
        };

        // 第二条 assistant 消息：有 thinking、text 和 tool_use
        let msg2 = AnthropicMessage {
            role: "assistant".to_string(),
            content: serde_json::json!([
                {"type": "thinking", "thinking": "I should read the file."},
                {"type": "text", "text": "Let me read that file."},
                {"type": "tool_use", "id": "toolu_01ABC", "name": "read_file", "input": {"path": "/test.txt"}}
            ]),
        };

        let messages: Vec<&AnthropicMessage> = vec![&msg1, &msg2];
        let result = merge_assistant_messages(&messages, &mut HashMap::new()).expect("合并应成功");

        // 验证 thinking 和 text 内容被合并
        let content = &result.assistant_response_message.content;
        assert!(content.contains("<thinking>"), "应包含 thinking 标签");
        assert!(
            content.contains("Let me read that file"),
            "应包含第二条消息的 text 内容"
        );

        // 验证 tool_use 被保留
        let tool_uses = result
            .assistant_response_message
            .tool_uses
            .expect("应有 tool_uses");
        assert_eq!(tool_uses.len(), 1);
        assert_eq!(tool_uses[0].tool_use_id, "toolu_01ABC");
        assert_eq!(tool_uses[0].name, "read_file");
    }

    #[test]
    fn test_consecutive_assistant_with_tool_use_result_pairing() {
        // 测试 Issue #79 的完整场景：连续 assistant 消息后跟 tool_result
        use super::super::types::Message as AnthropicMessage;

        let req = MessagesRequest {
            model: "claude-sonnet-4".to_string(),
            max_tokens: 1024,
            messages: vec![
                // 用户请求
                AnthropicMessage {
                    role: "user".to_string(),
                    content: serde_json::json!("Read the config file"),
                },
                // 第一条 assistant 消息（部分响应，网络中断）
                AnthropicMessage {
                    role: "assistant".to_string(),
                    content: serde_json::json!([
                        {"type": "thinking", "thinking": "I need to read the file..."},
                        {"type": "text", "text": " "}
                    ]),
                },
                // 第二条 assistant 消息（重新生成，包含 tool_use）
                AnthropicMessage {
                    role: "assistant".to_string(),
                    content: serde_json::json!([
                        {"type": "thinking", "thinking": "Let me read the config."},
                        {"type": "text", "text": "I'll read the config file for you."},
                        {"type": "tool_use", "id": "toolu_01XYZ", "name": "read_file", "input": {"path": "/config.json"}}
                    ]),
                },
                // 用户返回 tool_result
                AnthropicMessage {
                    role: "user".to_string(),
                    content: serde_json::json!([
                        {"type": "tool_result", "tool_use_id": "toolu_01XYZ", "content": "{\"key\": \"value\"}"}
                    ]),
                },
            ],
            stream: false,
            system: None,
            tools: None,
            tool_choice: None,
            thinking: None,
            output_config: None,
            metadata: None,
        };

        // 转换应成功，不应因 tool_use/tool_result 配对失败而报错
        let result = convert_request(&req, &CompressionConfig::default());
        assert!(
            result.is_ok(),
            "连续 assistant 消息场景不应报错: {:?}",
            result.err()
        );

        let state = result.unwrap().conversation_state;

        // 验证历史中的 assistant 消息包含 tool_use
        let mut found_tool_use = false;
        for msg in &state.history {
            if let Message::Assistant(assistant_msg) = msg
                && let Some(ref tool_uses) = assistant_msg.assistant_response_message.tool_uses
                && tool_uses.iter().any(|t| t.tool_use_id == "toolu_01XYZ")
            {
                found_tool_use = true;
                break;
            }
        }
        assert!(found_tool_use, "合并后的 assistant 消息应包含 tool_use");

        // 验证 current_message 包含 tool_result
        let tool_results = &state
            .current_message
            .user_input_message
            .user_input_message_context
            .tool_results;
        assert!(
            tool_results.iter().any(|t| t.tool_use_id == "toolu_01XYZ"),
            "current_message 应包含对应的 tool_result"
        );
    }

    #[test]
    fn test_merge_assistant_messages_multiple_tool_uses() {
        // 测试合并多条 assistant 消息时，所有 tool_use 都被保留
        use super::super::types::Message as AnthropicMessage;

        let msg1 = AnthropicMessage {
            role: "assistant".to_string(),
            content: serde_json::json!([
                {"type": "text", "text": "First action"},
                {"type": "tool_use", "id": "tool-1", "name": "read", "input": {"path": "/a.txt"}}
            ]),
        };

        let msg2 = AnthropicMessage {
            role: "assistant".to_string(),
            content: serde_json::json!([
                {"type": "text", "text": "Second action"},
                {"type": "tool_use", "id": "tool-2", "name": "write", "input": {"path": "/b.txt"}}
            ]),
        };

        let messages: Vec<&AnthropicMessage> = vec![&msg1, &msg2];
        let result = merge_assistant_messages(&messages, &mut HashMap::new()).expect("合并应成功");

        let tool_uses = result
            .assistant_response_message
            .tool_uses
            .expect("应有 tool_uses");

        assert_eq!(tool_uses.len(), 2, "应保留所有 tool_use");
        assert!(tool_uses.iter().any(|t| t.tool_use_id == "tool-1"));
        assert!(tool_uses.iter().any(|t| t.tool_use_id == "tool-2"));
    }

    #[test]
    fn test_merge_assistant_messages_only_tool_use() {
        // 测试合并后只有 tool_use 时，不主动补占位符
        use super::super::types::Message as AnthropicMessage;

        let msg1 = AnthropicMessage {
            role: "assistant".to_string(),
            content: serde_json::json!([
                {"type": "text", "text": " "}  // 空白 text
            ]),
        };

        let msg2 = AnthropicMessage {
            role: "assistant".to_string(),
            content: serde_json::json!([
                {"type": "tool_use", "id": "tool-1", "name": "read", "input": {}}
            ]),
        };

        let messages: Vec<&AnthropicMessage> = vec![&msg1, &msg2];
        let result = merge_assistant_messages(&messages, &mut HashMap::new()).expect("合并应成功");

        assert!(
            result.assistant_response_message.content.is_empty(),
            "仅 tool_use 时合并阶段不应主动补 '.'"
        );
    }
}
