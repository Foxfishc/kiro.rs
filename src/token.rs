//! Token 计算模块
//!
//! 提供本地 token 数量估算与远程 count_tokens 回退逻辑。
//!
//! # 本地估算规则
//! - CJK 字符：约 1.5 字符/token
//! - 其他非空白字符：约 3.5 字符/token
//! - 忽略空白字符
//! - 最终四舍五入

use crate::anthropic::types::{
    CountTokensRequest, CountTokensResponse, Message, SystemMessage, Tool,
};
use crate::http_client::{ProxyConfig, build_client};
use crate::model::config::TlsBackend;
use parking_lot::RwLock;
use std::sync::OnceLock;

const TOKENS_PER_TOOL: u64 = 150;
const TOKENS_PER_MESSAGE: u64 = 4;

/// Count Tokens API 配置
#[derive(Clone, Default)]
pub struct CountTokensConfig {
    /// 外部 count_tokens API 地址
    pub api_url: Option<String>,
    /// count_tokens API 密钥
    pub api_key: Option<String>,
    /// count_tokens API 认证类型（"x-api-key" 或 "bearer"）
    pub auth_type: String,
    /// 代理配置
    pub proxy: Option<ProxyConfig>,

    pub tls_backend: TlsBackend,
}

/// 全局配置存储
static COUNT_TOKENS_CONFIG: OnceLock<CountTokensConfig> = OnceLock::new();

/// 代理配置的运行时可变存储（热更新时同步刷新）
static COUNT_TOKENS_PROXY: OnceLock<RwLock<Option<ProxyConfig>>> = OnceLock::new();

/// 初始化 count_tokens 配置
///
/// 应在应用启动时调用一次
pub fn init_config(config: CountTokensConfig) {
    let proxy = config.proxy.clone();
    let _ = COUNT_TOKENS_CONFIG.set(config);
    let _ = COUNT_TOKENS_PROXY.set(RwLock::new(proxy));
}

/// 热更新代理配置
pub fn update_proxy(proxy: Option<ProxyConfig>) {
    if let Some(lock) = COUNT_TOKENS_PROXY.get() {
        *lock.write() = proxy;
    }
}

/// 获取当前代理配置
fn get_current_proxy() -> Option<ProxyConfig> {
    COUNT_TOKENS_PROXY
        .get()
        .map(|lock| lock.read().clone())
        .unwrap_or(None)
}

/// 获取配置
fn get_config() -> Option<&'static CountTokensConfig> {
    COUNT_TOKENS_CONFIG.get()
}

fn is_cjk(c: char) -> bool {
    matches!(
        c,
        '\u{4E00}'..='\u{9FFF}'
            | '\u{3400}'..='\u{4DBF}'
            | '\u{3040}'..='\u{309F}'
            | '\u{30A0}'..='\u{30FF}'
            | '\u{AC00}'..='\u{D7AF}'
            | '\u{1100}'..='\u{11FF}'
            | '\u{3130}'..='\u{318F}'
    )
}

/// 计算文本的 token 数量
pub fn count_tokens(text: &str) -> u64 {
    if text.is_empty() {
        return 0;
    }

    let mut cjk_count = 0usize;
    let mut other_count = 0usize;

    for c in text.chars() {
        if c.is_whitespace() {
            continue;
        }

        if is_cjk(c) {
            cjk_count += 1;
        } else {
            other_count += 1;
        }
    }

    let tokens = (cjk_count as f64 / 1.5) + (other_count as f64 / 3.5);
    tokens.round() as u64
}

/// 估算请求的输入 tokens
///
/// 优先调用远程 API，失败时回退到本地计算
pub(crate) fn count_all_tokens(
    model: String,
    system: Option<Vec<SystemMessage>>,
    messages: Vec<Message>,
    tools: Option<Vec<Tool>>,
) -> u64 {
    // 检查是否配置了远程 API
    if let Some(config) = get_config()
        && config.api_url.is_some()
    {
        // 尝试调用远程 API
        let result = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(call_remote_count_tokens(
                config, model, &system, &messages, &tools,
            ))
        });

        match result {
            Ok(tokens) => {
                tracing::debug!("远程 count_tokens API 返回: {}", tokens);
                return tokens;
            }
            Err(e) => {
                tracing::warn!("远程 count_tokens API 调用失败，回退到本地计算: {}", e);
            }
        }
    }

    // 本地计算
    count_all_tokens_local(system, messages, tools)
}

/// 调用远程 count_tokens API
async fn call_remote_count_tokens(
    config: &CountTokensConfig,
    model: String,
    system: &Option<Vec<SystemMessage>>,
    messages: &[Message],
    tools: &Option<Vec<Tool>>,
) -> Result<u64, Box<dyn std::error::Error + Send + Sync>> {
    let api_url = config.api_url.as_ref().unwrap();
    let current_proxy = get_current_proxy();
    let client = build_client(current_proxy.as_ref(), 300, config.tls_backend)?;

    // 构建请求体
    let request = CountTokensRequest {
        model,
        messages: messages.to_vec(),
        system: system.clone(),
        tools: tools.clone(),
    };

    // 构建请求
    let mut req_builder = client.post(api_url);

    // 设置认证头
    if let Some(api_key) = &config.api_key {
        if config.auth_type == "bearer" {
            req_builder = req_builder.header("Authorization", format!("Bearer {}", api_key));
        } else {
            req_builder = req_builder.header("x-api-key", api_key);
        }
    }

    // 发送请求
    let response = req_builder
        .header("Content-Type", "application/json")
        .json(&request)
        .send()
        .await?;

    if !response.status().is_success() {
        return Err(format!("API 返回错误状态: {}", response.status()).into());
    }

    let result: CountTokensResponse = response.json().await?;
    Ok(result.input_tokens as u64)
}

fn estimate_messages_tokens(messages: &[Message]) -> u64 {
    // 本地兜底应估算模型看到的消息内容，而不是 API JSON 编码体；
    // 否则长历史和 tool_result 会被字段名/转义字符显著放大。
    messages
        .iter()
        .map(|message| {
            count_tokens(&message.role)
                + count_message_content_tokens(&message.content)
                + TOKENS_PER_MESSAGE
        })
        .sum()
}

fn count_serialized_value_tokens(value: &serde_json::Value) -> u64 {
    let json = serde_json::to_string(value).unwrap_or_default();
    count_tokens(&json)
}

fn estimate_content_block_tokens(obj: &serde_json::Map<String, serde_json::Value>) -> u64 {
    if let Some(text) = obj.get("text").and_then(|v| v.as_str()) {
        return count_tokens(text);
    }

    if let Some(thinking) = obj.get("thinking").and_then(|v| v.as_str()) {
        return count_tokens(thinking);
    }

    if let Some(input) = obj.get("input") {
        return count_serialized_value_tokens(input);
    }

    if let Some(content) = obj.get("content") {
        return count_message_content_tokens(content);
    }

    0
}

/// 本地计算请求的输入 tokens
fn count_all_tokens_local(
    system: Option<Vec<SystemMessage>>,
    messages: Vec<Message>,
    tools: Option<Vec<Tool>>,
) -> u64 {
    let system_tokens: u64 = system
        .unwrap_or_default()
        .iter()
        .map(count_system_message_tokens)
        .sum();
    let message_tokens = estimate_messages_tokens(&messages);
    let tool_tokens = tools
        .as_ref()
        .map(|items| items.len() as u64 * TOKENS_PER_TOOL)
        .unwrap_or(0);

    (system_tokens + message_tokens + tool_tokens).max(1)
}

/// 估算输出 tokens
pub(crate) fn estimate_output_tokens(content: &[serde_json::Value]) -> i32 {
    let total: i32 = content
        .iter()
        .map(|block| count_message_content_tokens(block) as i32)
        .sum();

    total.max(1)
}

/// 计算系统消息的 tokens
pub(crate) fn count_system_message_tokens(message: &SystemMessage) -> u64 {
    count_tokens(&message.text)
}

/// 计算工具定义的 tokens
pub(crate) fn count_tool_definition_tokens(_tool: &Tool) -> u64 {
    TOKENS_PER_TOOL
}

/// 计算消息内容的 tokens
pub(crate) fn count_message_content_tokens(value: &serde_json::Value) -> u64 {
    match value {
        serde_json::Value::Null => 0,
        serde_json::Value::String(s) => count_tokens(s),
        serde_json::Value::Array(arr) => arr.iter().map(count_message_content_tokens).sum(),
        serde_json::Value::Object(obj) => estimate_content_block_tokens(obj),
        _ => 0,
    }
}
