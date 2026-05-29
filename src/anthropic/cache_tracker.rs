use std::collections::{BTreeMap, HashMap};
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use sha2::{Digest, Sha256};

use crate::token::{
    count_message_content_tokens, count_system_message_tokens, count_tool_definition_tokens,
};

use super::types::{CacheControl, Message, MessagesRequest};

const DEFAULT_CACHE_TTL: Duration = Duration::from_secs(300);
const ONE_HOUR_CACHE_TTL: Duration = Duration::from_secs(3600);
const PREFIX_LOOKBACK_LIMIT: usize = 64;

// 命中上限：与 Anthropic 真实行为对齐，最新内容总有不可缓存的结构开销，
// 永远不可能 100% 命中。这里钳制到 95%，让客户端看到的 input_tokens 至少
// 保留 5% 真实重算量，避免出现 cache_read = total 的奇景。
const MAX_CACHE_RATIO_NUM: i64 = 95;
const MAX_CACHE_RATIO_DEN: i64 = 100;

// 最小可缓存 token 阈值：低于此阈值的 prompt 不进入缓存（与 Anthropic 官方
// minimum cacheable prompt 行为对齐）。Opus 系列阈值更高。
const DEFAULT_MIN_CACHEABLE_TOKENS: i32 = 1024;
const OPUS_MIN_CACHEABLE_TOKENS: i32 = 4096;

fn min_cacheable_tokens(model: &str) -> i32 {
    if model.to_lowercase().contains("opus") {
        OPUS_MIN_CACHEABLE_TOKENS
    } else {
        DEFAULT_MIN_CACHEABLE_TOKENS
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct CacheResult {
    pub cache_read_input_tokens: i32,
    pub cache_creation_input_tokens: i32,
    pub cache_creation_5m_input_tokens: i32,
    pub cache_creation_1h_input_tokens: i32,
}

#[derive(Debug, Clone)]
pub struct CacheProfile {
    total_input_tokens: i32,
    blocks: Vec<CacheBlock>,
    breakpoints: Vec<CacheBreakpoint>,
    /// 请求模型，用于决定 min_cacheable_tokens 阈值
    model: String,
}

#[derive(Debug, Clone)]
struct CacheBlock {
    prefix_fingerprint: [u8; 32],
    cumulative_tokens: i32,
}

#[derive(Debug, Clone)]
struct CacheBreakpoint {
    block_index: usize,
    ttl: Duration,
}

#[derive(Debug, Clone)]
struct CacheEntry {
    token_count: i32,
    ttl: Duration,
    expires_at: Instant,
}

struct CachedCheckpointStore {
    by_credential: HashMap<u64, HashMap<[u8; 32], CacheEntry>>,
}

pub struct CacheTracker {
    entries: Mutex<CachedCheckpointStore>,
    max_supported_ttl: Duration,
}

impl CacheTracker {
    pub fn new(max_supported_ttl: Duration) -> Self {
        Self {
            entries: Mutex::new(CachedCheckpointStore {
                by_credential: HashMap::new(),
            }),
            max_supported_ttl,
        }
    }

    pub fn build_profile(
        &self,
        payload: &MessagesRequest,
        total_input_tokens: i32,
    ) -> CacheProfile {
        let flattened = flatten_cacheable_blocks(payload);

        // 与 prompt 内容无关但会影响官方缓存可复用性的固定配置。
        // 仅哈希 model；tool_choice 不影响缓存指纹，切换不致使缓存失效。
        let request_prelude = canonicalize_json(serde_json::json!({
            "model": payload.model,
        }));
        let prelude_bytes = serde_json::to_vec(&request_prelude).unwrap_or_default();
        let mut prefix_hasher = Sha256::new();
        prefix_hasher.update((prelude_bytes.len() as u64).to_be_bytes());
        prefix_hasher.update(&prelude_bytes);

        let mut blocks = Vec::with_capacity(flattened.len());
        let mut breakpoints = Vec::new();
        let mut cumulative_tokens = 0i32;

        let mut active_ttl: Option<Duration> = None;
        let mut seen_explicit_breakpoint = false;
        let mut seen_breakpoints: std::collections::BTreeSet<usize> =
            std::collections::BTreeSet::new();

        for (index, block) in flattened.into_iter().enumerate() {
            cumulative_tokens = cumulative_tokens.saturating_add(block.tokens);

            let block_bytes = serde_json::to_vec(&block.value).unwrap_or_default();
            let block_hash: [u8; 32] = Sha256::digest(&block_bytes).into();

            let mut next_prefix_hasher = prefix_hasher.clone();
            next_prefix_hasher.update(block_hash);
            let prefix_fingerprint: [u8; 32] = next_prefix_hasher.finalize().into();
            prefix_hasher = Sha256::new();
            prefix_hasher.update(prefix_fingerprint);

            blocks.push(CacheBlock {
                prefix_fingerprint,
                cumulative_tokens,
            });

            // 显式 cache_control 断点：遇到 breakpoint_ttl 即置 seen_explicit_breakpoint
            if let Some(ttl) = block.breakpoint_ttl {
                let ttl = ttl.min(self.max_supported_ttl);
                active_ttl = Some(ttl);
                seen_explicit_breakpoint = true;
                if seen_breakpoints.insert(index) {
                    breakpoints.push(CacheBreakpoint {
                        block_index: index,
                        ttl,
                    });
                }
            }

            // 隐式 message_end 派生断点：仅当已出现过显式 cache_control 后才启用
            if seen_explicit_breakpoint
                && block.is_message_end
                && block.message_index.is_some()
                && let Some(ttl) = active_ttl
                && seen_breakpoints.insert(index)
            {
                breakpoints.push(CacheBreakpoint {
                    block_index: index,
                    ttl,
                });
            }
        }

        // total_input_tokens 保留为 count_all_tokens 的原始值，
        // 不再做任何 cumulative_tokens 抬升：命中量取 entry.token_count，
        // 低于阈值不计入缓存，非缓存余量由 usage 拼装层的 max(0) 兜底。

        CacheProfile {
            total_input_tokens: total_input_tokens.max(0),
            blocks,
            breakpoints,
            model: payload.model.clone(),
        }
    }

    pub fn compute(&self, credential_id: u64, profile: &CacheProfile) -> CacheResult {
        let Some(last_breakpoint) = profile.last_cacheable_breakpoint() else {
            return CacheResult::default();
        };
        let last_breakpoint_tokens = last_breakpoint
            .cumulative_tokens
            .min(profile.total_input_tokens);

        let now = Instant::now();
        let mut entries = self.entries.lock();
        prune_expired(&mut entries.by_credential, now);

        let min_tokens = min_cacheable_tokens(&profile.model);

        let Some(credential_entries) = entries.by_credential.get_mut(&credential_id) else {
            // 首次请求，无缓存条目
            tracing::debug!(credential_id, "首次请求，无缓存条目");
            let creation = if last_breakpoint_tokens >= min_tokens {
                last_breakpoint_tokens
            } else {
                0
            };
            let (cache_5m, cache_1h) = if creation > 0 {
                compute_ttl_breakdown(profile, 0)
            } else {
                (0, 0)
            };
            return CacheResult {
                cache_read_input_tokens: 0,
                cache_creation_input_tokens: creation,
                cache_creation_5m_input_tokens: cache_5m,
                cache_creation_1h_input_tokens: cache_1h,
            };
        };

        tracing::debug!(
            credential_id,
            entry_count = credential_entries.len(),
            "查找缓存匹配"
        );

        let mut matched_tokens = 0;

        let cacheable_breakpoints = profile.cacheable_breakpoints();
        let candidate_breakpoints: Vec<_> = cacheable_breakpoints
            .iter()
            .rev()
            .take(PREFIX_LOOKBACK_LIMIT)
            .copied()
            .collect();

        'outer: for breakpoint in candidate_breakpoints {
            // 跳过低于最小可缓存阈值的断点
            if breakpoint.cumulative_tokens < min_tokens {
                continue;
            }
            let candidate = &profile.blocks[breakpoint.block_index];
            if let Some(entry) = credential_entries.get(&candidate.prefix_fingerprint) {
                if entry.expires_at <= now {
                    continue;
                }
                // 命中量取 entry 中存储的 token_count（首次写入时单调增长的 cumulative），
                // 而非当前请求的 breakpoint.cumulative_tokens。这样 hit 量反映的是
                // 历史上写过的最大前缀，不受当前 total_input_tokens 波动影响。
                //
                // 不在命中时刷新 expires_at：Anthropic 真实 TTL 从首次写入计算，
                // 命中不延期。
                matched_tokens = entry.token_count.min(profile.total_input_tokens);
                break 'outer;
            }
        }

        // MAX_CACHE_RATIO 钳制：永远不允许 read 超过 total 的 95%，
        // 确保客户端看到的 input_tokens 至少有 5% 真实重算量。
        let cap =
            (profile.total_input_tokens as i64 * MAX_CACHE_RATIO_NUM / MAX_CACHE_RATIO_DEN) as i32;
        matched_tokens = matched_tokens.min(cap);

        let new_tokens = last_breakpoint_tokens.saturating_sub(matched_tokens).max(0);
        let (cache_5m, cache_1h) = compute_ttl_breakdown(profile, matched_tokens);

        tracing::debug!(
            credential_id,
            matched_tokens,
            new_tokens,
            cache_5m,
            cache_1h,
            "缓存计算结果"
        );

        CacheResult {
            cache_read_input_tokens: matched_tokens.max(0),
            cache_creation_input_tokens: new_tokens,
            cache_creation_5m_input_tokens: cache_5m,
            cache_creation_1h_input_tokens: cache_1h,
        }
    }

    pub fn update(&self, credential_id: u64, profile: &CacheProfile) {
        let now = Instant::now();
        let mut entries = self.entries.lock();
        prune_expired(&mut entries.by_credential, now);

        let credential_entries = entries.by_credential.entry(credential_id).or_default();
        let min_tokens = min_cacheable_tokens(&profile.model);

        for breakpoint in profile.cacheable_breakpoints() {
            if breakpoint.cumulative_tokens < min_tokens {
                continue;
            }
            let block = &profile.blocks[breakpoint.block_index];
            let next_expiry = now + breakpoint.ttl;

            match credential_entries.get_mut(&block.prefix_fingerprint) {
                Some(existing) => {
                    // 不刷新 expires_at：Anthropic 真实 prompt cache TTL 从首次写入起算，
                    // 重写已存在的 prefix 不会续 TTL。
                    // 仅单调增长 token_count；不更新 ttl，不续命。
                    existing.token_count = existing.token_count.max(block.cumulative_tokens);
                }
                None => {
                    credential_entries.insert(
                        block.prefix_fingerprint,
                        CacheEntry {
                            token_count: block.cumulative_tokens,
                            ttl: breakpoint.ttl,
                            expires_at: next_expiry,
                        },
                    );
                }
            }
        }
    }
}

/// 按断点逐个累加 5m/1h delta。profile 中的每个 cacheable 断点按其 TTL
/// 将 cumulative_tokens 中超过 matched_tokens 的部分计入对应桶。
fn compute_ttl_breakdown(profile: &CacheProfile, matched_tokens: i32) -> (i32, i32) {
    let mut cache_5m = 0;
    let mut cache_1h = 0;
    let mut previous = matched_tokens;
    for bp in profile.cacheable_breakpoints() {
        let current = bp.cumulative_tokens.min(profile.total_input_tokens);
        if current <= previous {
            continue;
        }
        let delta = current - previous;
        if bp.ttl >= ONE_HOUR_CACHE_TTL {
            cache_1h += delta;
        } else {
            cache_5m += delta;
        }
        previous = current;
    }
    (cache_5m, cache_1h)
}

impl CacheProfile {
    #[cfg(test)]
    pub fn total_input_tokens(&self) -> i32 {
        self.total_input_tokens
    }

    fn cacheable_breakpoints(&self) -> Vec<ResolvedBreakpoint> {
        self.breakpoints
            .iter()
            .filter_map(|breakpoint| {
                let block = self.blocks.get(breakpoint.block_index)?;

                Some(ResolvedBreakpoint {
                    block_index: breakpoint.block_index,
                    cumulative_tokens: block.cumulative_tokens,
                    ttl: breakpoint.ttl,
                })
            })
            .collect()
    }

    fn last_cacheable_breakpoint(&self) -> Option<ResolvedBreakpoint> {
        self.cacheable_breakpoints().into_iter().last()
    }
}

#[derive(Debug, Clone, Copy)]
struct ResolvedBreakpoint {
    block_index: usize,
    cumulative_tokens: i32,
    ttl: Duration,
}

#[derive(Debug)]
struct PendingBlock {
    value: serde_json::Value,
    tokens: i32,
    breakpoint_ttl: Option<Duration>,
    message_index: Option<usize>,
    is_message_end: bool,
}

fn flatten_cacheable_blocks(payload: &MessagesRequest) -> Vec<PendingBlock> {
    let mut blocks = Vec::new();

    if let Some(tools) = &payload.tools {
        for (tool_index, tool) in tools.iter().enumerate() {
            let mut value = serde_json::to_value(tool).unwrap_or(serde_json::Value::Null);
            let breakpoint_ttl = extract_cache_ttl(&value);
            strip_cache_control(&mut value);

            blocks.push(PendingBlock {
                value: canonicalize_json(serde_json::json!({
                    "kind": "tool",
                    "tool_index": tool_index,
                    "tool": value,
                })),
                tokens: count_tool_definition_tokens(tool) as i32,
                breakpoint_ttl,
                message_index: None,
                is_message_end: false,
            });
        }
    }

    if let Some(system) = &payload.system {
        for (system_index, block) in system.iter().enumerate() {
            let mut value = serde_json::to_value(block).unwrap_or(serde_json::Value::Null);
            let breakpoint_ttl = extract_cache_ttl(&value);
            strip_cache_control(&mut value);
            canonicalize_system_block_for_cache(&mut value);

            blocks.push(PendingBlock {
                value: canonicalize_json(serde_json::json!({
                    "kind": "system",
                    "system_index": system_index,
                    "block": value,
                })),
                tokens: count_system_message_tokens(block) as i32,
                breakpoint_ttl,
                message_index: None,
                is_message_end: false,
            });
        }
    }

    for (message_index, message) in payload.messages.iter().enumerate() {
        blocks.extend(flatten_message_blocks(message_index, message));
    }

    blocks
}

fn canonicalize_system_block_for_cache(value: &mut serde_json::Value) {
    let Some(obj) = value.as_object_mut() else {
        return;
    };

    let is_text_block = obj
        .get("type")
        .and_then(|v| v.as_str())
        .map(|t| t == "text")
        .unwrap_or(true);
    if !is_text_block {
        return;
    }

    let Some(text) = obj.get("text").and_then(|v| v.as_str()) else {
        return;
    };
    if !text.starts_with("x-anthropic-billing-header:") {
        return;
    }

    obj.insert(
        "text".to_string(),
        serde_json::Value::String("__anthropic_billing_header__".to_string()),
    );
}

fn flatten_message_blocks(message_index: usize, message: &Message) -> Vec<PendingBlock> {
    match &message.content {
        serde_json::Value::String(text) => vec![build_message_block(
            message_index,
            &message.role,
            0,
            serde_json::json!({
                "type": "text",
                "text": text,
            }),
            None,
            true,
        )],
        serde_json::Value::Array(blocks) => {
            let last_block_index = blocks.len().saturating_sub(1);
            blocks
                .iter()
                .enumerate()
                .map(|(block_index, block)| {
                    let breakpoint_ttl = extract_cache_ttl(block);
                    let mut normalized = block.clone();
                    strip_cache_control(&mut normalized);
                    build_message_block(
                        message_index,
                        &message.role,
                        block_index,
                        normalized,
                        breakpoint_ttl,
                        block_index == last_block_index,
                    )
                })
                .collect()
        }
        other => vec![build_message_block(
            message_index,
            &message.role,
            0,
            other.clone(),
            None,
            true,
        )],
    }
}

fn build_message_block(
    message_index: usize,
    role: &str,
    block_index: usize,
    block: serde_json::Value,
    breakpoint_ttl: Option<Duration>,
    is_message_end: bool,
) -> PendingBlock {
    PendingBlock {
        tokens: count_message_content_tokens(&block) as i32,
        value: canonicalize_json(serde_json::json!({
            "kind": "message",
            "message_index": message_index,
            "role": role,
            "block_index": block_index,
            "block": block,
        })),
        breakpoint_ttl,
        message_index: Some(message_index),
        is_message_end,
    }
}

fn extract_cache_ttl(value: &serde_json::Value) -> Option<Duration> {
    let cache_control = value.get("cache_control")?;
    let cache_control: CacheControl = serde_json::from_value(cache_control.clone()).ok()?;
    if cache_control.cache_type != "ephemeral" {
        return None;
    }

    Some(match cache_control.ttl.as_deref() {
        Some("1h") => ONE_HOUR_CACHE_TTL,
        _ => DEFAULT_CACHE_TTL,
    })
}

fn strip_cache_control(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Array(arr) => {
            for item in arr {
                strip_cache_control(item);
            }
        }
        serde_json::Value::Object(map) => {
            map.remove("cache_control");
            for item in map.values_mut() {
                strip_cache_control(item);
            }
        }
        _ => {}
    }
}

fn prune_expired(entries: &mut HashMap<u64, HashMap<[u8; 32], CacheEntry>>, now: Instant) {
    entries.retain(|_, credential_entries| {
        credential_entries.retain(|_, entry| entry.expires_at > now);
        !credential_entries.is_empty()
    });
}

fn canonicalize_json(value: serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Array(arr) => {
            serde_json::Value::Array(arr.into_iter().map(canonicalize_json).collect())
        }
        serde_json::Value::Object(map) => {
            let ordered: BTreeMap<_, _> = map
                .into_iter()
                .map(|(key, value)| (key, canonicalize_json(value)))
                .collect();

            let mut out = serde_json::Map::new();
            for (key, value) in ordered {
                out.insert(key, value);
            }
            serde_json::Value::Object(out)
        }
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::anthropic::types::{SystemMessage, Tool};
    use crate::token;

    fn build_request(messages: Vec<Message>) -> MessagesRequest {
        MessagesRequest {
            model: "claude-sonnet-4-6".to_string(),
            max_tokens: 1024,
            messages,
            stream: false,
            system: Some(vec![SystemMessage {
                block_type: None,
                text: "system".to_string(),
                cache_control: None,
            }]),
            tools: Some(vec![Tool {
                tool_type: None,
                name: "echo".to_string(),
                description: "echo".to_string(),
                input_schema: Default::default(),
                max_uses: None,
                cache_control: None,
            }]),
            tool_choice: None,
            thinking: None,
            output_config: None,
            metadata: None,
        }
    }

    fn build_request_with_system(
        messages: Vec<Message>,
        system: Vec<SystemMessage>,
    ) -> MessagesRequest {
        let mut request = build_request(messages);
        request.system = Some(system);
        request
    }

    fn msg(role: &str, content: serde_json::Value) -> Message {
        Message {
            role: role.to_string(),
            content,
        }
    }

    fn cache_text(text: &str) -> serde_json::Value {
        serde_json::json!([{
            "type": "text",
            "text": text,
            "cache_control": { "type": "ephemeral" }
        }])
    }

    fn long_cacheable_text() -> String {
        std::iter::repeat_n("cacheable prompt chunk", 256)
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn medium_turn_text(label: &str) -> String {
        format!(
            "{} {}",
            label,
            std::iter::repeat_n("conversation growth chunk", 80)
                .collect::<Vec<_>>()
                .join(" ")
        )
    }

    fn estimate_input_tokens(request: &MessagesRequest) -> i32 {
        token::count_all_tokens(
            request.model.clone(),
            request.system.clone(),
            request.messages.clone(),
            request.tools.clone(),
        ) as i32
    }

    #[test]
    fn attribution_header_drift_does_not_break_cache_hit() {
        let tracker = CacheTracker::new(Duration::from_secs(3600));
        let system1 = vec![
            SystemMessage {
                block_type: Some("text".to_string()),
                text:
                    "x-anthropic-billing-header: cc_version=2.1.87.1; cc_entrypoint=cli; cch=aaaaa;"
                        .to_string(),
                cache_control: None,
            },
            SystemMessage {
                block_type: Some("text".to_string()),
                text: long_cacheable_text(),
                cache_control: Some(CacheControl {
                    cache_type: "ephemeral".to_string(),
                    ttl: None,
                }),
            },
        ];
        let system2 = vec![
            SystemMessage {
                block_type: Some("text".to_string()),
                text: "x-anthropic-billing-header: cc_version=2.1.87.222222222222222222; cc_entrypoint=cli; cch=bbbbb; extra_padding=xyzxyzxyzxyz;".to_string(),
                cache_control: None,
            },
            SystemMessage {
                block_type: Some("text".to_string()),
                text: long_cacheable_text(),
                cache_control: Some(CacheControl {
                    cache_type: "ephemeral".to_string(),
                    ttl: None,
                }),
            },
        ];

        let req1 =
            build_request_with_system(vec![msg("user", serde_json::json!("hello"))], system1);
        let total1 = estimate_input_tokens(&req1);
        let profile1 = tracker.build_profile(&req1, total1);
        tracker.update(1, &profile1);

        let req2 =
            build_request_with_system(vec![msg("user", serde_json::json!("hello"))], system2);
        let total2 = estimate_input_tokens(&req2);
        let profile2 = tracker.build_profile(&req2, total2);
        let result = tracker.compute(1, &profile2);

        assert!(total1 != total2);
        // billing header 差异被 canonicalize 消除，system 指纹相同 → 命中
        assert!(result.cache_read_input_tokens > 0);
        // MAX_CACHE_RATIO 可能裁减命中量，不做 exact equality
        assert!(result.cache_read_input_tokens <= profile2.total_input_tokens());
        // MAX_CACHE_RATIO 裁减后可能产生少量 creation，仅检查非负
        assert!(result.cache_creation_input_tokens >= 0);
    }

    #[test]
    fn normal_system_text_change_still_misses() {
        // 仅 system 有显式 cache_control。当 system 文本变化时，system 断点的
        // fingerprint 改变，其后所有断点失效 → 零命中，全部 creation。
        let tracker = CacheTracker::new(Duration::from_secs(3600));
        let system1 = vec![SystemMessage {
            block_type: Some("text".to_string()),
            text: long_cacheable_text(),
            cache_control: Some(CacheControl {
                cache_type: "ephemeral".to_string(),
                ttl: None,
            }),
        }];
        let system2 = vec![SystemMessage {
            block_type: Some("text".to_string()),
            text: format!("{} extra", long_cacheable_text()),
            cache_control: Some(CacheControl {
                cache_type: "ephemeral".to_string(),
                ttl: None,
            }),
        }];

        let req1 =
            build_request_with_system(vec![msg("user", serde_json::json!("hello"))], system1);
        let total1 = estimate_input_tokens(&req1);
        let profile1 = tracker.build_profile(&req1, total1);
        tracker.update(1, &profile1);

        let req2 =
            build_request_with_system(vec![msg("user", serde_json::json!("hello"))], system2);
        let total2 = estimate_input_tokens(&req2);
        let profile2 = tracker.build_profile(&req2, total2);
        let result = tracker.compute(1, &profile2);

        // system 指纹变了 + tools 没有 breakpoint → 之前无 entry 能匹配
        assert_eq!(result.cache_read_input_tokens, 0);
        assert!(result.cache_creation_input_tokens > 0);
    }

    #[test]
    fn explicit_breakpoint_without_hit_creates_prefix_only() {
        let tracker = CacheTracker::new(Duration::from_secs(3600));
        let req = build_request(vec![msg("user", cache_text(&long_cacheable_text()))]);
        let total = estimate_input_tokens(&req);
        let profile = tracker.build_profile(&req, total);
        let result = tracker.compute(1, &profile);

        assert_eq!(result.cache_read_input_tokens, 0);
        assert_eq!(
            result.cache_creation_input_tokens,
            profile
                .last_cacheable_breakpoint()
                .map(|bp| bp.cumulative_tokens)
                .unwrap_or(0)
        );
    }

    #[test]
    fn same_content_with_shape_drift_does_not_false_hit() {
        // 消息形态从 array（带显式 cache_control）变为 string（无 breakpoint），
        // 并追加一条 assistant 消息。breakpoint 位置和内容都变了 → 不应命中。
        let tracker = CacheTracker::new(Duration::from_secs(3600));
        let req1 = build_request(vec![msg("user", cache_text(&long_cacheable_text()))]);
        let total1 = estimate_input_tokens(&req1);
        let profile1 = tracker.build_profile(&req1, total1);
        tracker.update(1, &profile1);

        let req2 = build_request(vec![
            msg("user", serde_json::json!(long_cacheable_text())),
            msg(
                "assistant",
                serde_json::json!([{
                    "type": "text",
                    "text": "ok",
                    "cache_control": { "type": "ephemeral" }
                }]),
            ),
        ]);
        let total2 = estimate_input_tokens(&req2);
        let profile2 = tracker.build_profile(&req2, total2);
        let result = tracker.compute(1, &profile2);

        // req1 的 breakpoint 在 user[0]，req2 的 breakpoint 在 assistant。
        // breakpoint 偏移 → 指纹链断裂。
        assert_eq!(result.cache_read_input_tokens, 0);
        assert!(result.cache_creation_input_tokens > 0);
    }

    #[test]
    fn same_length_retry_with_same_breakpoint_is_hit() {
        let tracker = CacheTracker::new(Duration::from_secs(3600));
        let req1 = build_request(vec![msg("user", cache_text(&long_cacheable_text()))]);
        let total1 = estimate_input_tokens(&req1);
        let profile1 = tracker.build_profile(&req1, total1);
        tracker.update(1, &profile1);

        let req2 = build_request(vec![msg("user", cache_text(&long_cacheable_text()))]);
        let total2 = estimate_input_tokens(&req2);
        let profile2 = tracker.build_profile(&req2, total2);
        let result = tracker.compute(1, &profile2);

        // 完全相同请求 → 命中，但 MAX_CACHE_RATIO 可能裁减，不做 exact equality
        assert!(result.cache_read_input_tokens > 0);
        assert!(result.cache_read_input_tokens <= profile2.total_input_tokens());
        // MAX_CACHE_RATIO 裁减后可能产生少量 creation，仅检查非负
        assert!(result.cache_creation_input_tokens >= 0);
    }

    #[test]
    fn prefix_match_with_appended_turn_reads_previous_prefix_cache() {
        let tracker = CacheTracker::new(Duration::from_secs(3600));
        let req1 = build_request(vec![
            msg("user", cache_text(&long_cacheable_text())),
            msg("assistant", serde_json::json!("R1")),
            msg("user", serde_json::json!("Follow-up")),
            msg("assistant", serde_json::json!("R2")),
        ]);
        let total1 = estimate_input_tokens(&req1);
        let profile1 = tracker.build_profile(&req1, total1);
        tracker.update(1, &profile1);

        let req2 = build_request(vec![
            msg("user", cache_text(&long_cacheable_text())),
            msg("assistant", serde_json::json!("R1")),
            msg("user", serde_json::json!("Follow-up")),
            msg("assistant", serde_json::json!("R2")),
            msg("user", serde_json::json!("New feedback")),
            msg("assistant", serde_json::json!("R3")),
        ]);
        let total2 = estimate_input_tokens(&req2);
        let profile2 = tracker.build_profile(&req2, total2);
        let result = tracker.compute(1, &profile2);

        // profile2 中某个早期断点的 fingerprint 能在 profile1 里找到，
        // 命中 token 数取自 profile2 自己的断点累加值。
        // 由于 build_profile 把最后断点的 cumulative_tokens 抬到 total_input_tokens
        // （兜入 wrapper overhead），所以期望命中至少要比「裸 profile1 cumulative」大。
        assert!(result.cache_read_input_tokens > 0);
        assert!(result.cache_read_input_tokens < profile2.total_input_tokens());
        assert!(result.cache_creation_input_tokens > 0);
    }

    #[test]
    fn prefix_lookback_limits_to_recent_window() {
        let tracker = CacheTracker::new(Duration::from_secs(3600));
        let mut messages = Vec::new();
        for i in 0..18 {
            messages.push(msg(
                "user",
                cache_text(&format!("{}-{i}", long_cacheable_text())),
            ));
            messages.push(msg("assistant", serde_json::json!(format!("reply-{i}"))));
        }
        let req = build_request(messages);
        let total = estimate_input_tokens(&req);
        let profile = tracker.build_profile(&req, total);
        assert!(profile.cacheable_breakpoints().len() >= 15);
    }

    #[test]
    fn message_end_after_anchor_creates_additional_breakpoint() {
        let req = build_request(vec![
            msg("user", cache_text(&long_cacheable_text())),
            msg("assistant", serde_json::json!("R1")),
        ]);
        let tracker = CacheTracker::new(Duration::from_secs(3600));
        let profile = tracker.build_profile(&req, estimate_input_tokens(&req));
        let breakpoints = profile.cacheable_breakpoints();
        assert!(breakpoints.len() >= 2);
    }

    #[test]
    fn multi_turn_history_extends_cacheable_prefix() {
        let tracker = CacheTracker::new(Duration::from_secs(3600));
        let long = long_cacheable_text();

        let req1 = build_request(vec![msg("user", cache_text(&long))]);
        let total1 = estimate_input_tokens(&req1);
        let profile1 = tracker.build_profile(&req1, total1);
        let result1 = tracker.compute(1, &profile1);
        assert!(result1.cache_creation_input_tokens > 0);
        // 首请求 cache_creation 计入除零看，应有正值。
        assert!(result1.cache_creation_input_tokens > 0);
        assert!(result1.cache_creation_input_tokens <= total1);
        tracker.update(1, &profile1);

        let req2 = build_request(vec![
            msg("user", cache_text(&long)),
            msg("assistant", serde_json::json!(medium_turn_text("R1"))),
            msg("user", serde_json::json!(medium_turn_text("R2"))),
        ]);
        let total2 = estimate_input_tokens(&req2);
        let profile2 = tracker.build_profile(&req2, total2);
        let result2 = tracker.compute(1, &profile2);
        // req2 含 req1 的 prefix（同样的 user[0]/cache_text），所以应有命中。
        assert!(result2.cache_read_input_tokens > 0);
        // 但命中量必须仅限 req1 的「自然 prefix 内容」，不能把 req1 的 wrapper overhead
        // 也算成 read —— 否则新 turn 的 wrapper 会被错误地从 creation 里减掉，导致亏损。
        // 近似守恒：read + creation 不应超过 total2
        assert!(
            result2.cache_read_input_tokens + result2.cache_creation_input_tokens <= total2,
            "read + creation 不应超过 total2"
        );
        let _consumed = result2.cache_read_input_tokens + result2.cache_creation_input_tokens;
        // 命中量不超过 req1 自然累加（wrapper 不归 read）。
        let req1_natural_tokens = profile1
            .last_cacheable_breakpoint()
            .map(|bp| {
                // 还原 build_profile 抬高前的自然累加：取倒数第二个 bp 或重算。
                // 此处用简单上界：req1 的 message content + system + tools content。
                bp.cumulative_tokens
            })
            .unwrap_or(0);
        assert!(result2.cache_read_input_tokens <= req1_natural_tokens);
        tracker.update(1, &profile2);

        let req3 = build_request(vec![
            msg("user", cache_text(&long)),
            msg("assistant", serde_json::json!(medium_turn_text("R1"))),
            msg("user", serde_json::json!(medium_turn_text("R2"))),
            msg("assistant", serde_json::json!(medium_turn_text("R2A"))),
            msg("user", serde_json::json!(medium_turn_text("R3"))),
        ]);
        let total3 = estimate_input_tokens(&req3);
        let profile3 = tracker.build_profile(&req3, total3);
        let result3 = tracker.compute(1, &profile3);
        // req3 包含 req2 的 prefix，read 应大于 req2 的 read。
        assert!(result3.cache_read_input_tokens > result2.cache_read_input_tokens);
        // 近似守恒：read + creation 不应超过 total3
        let consumed3 = result3.cache_read_input_tokens + result3.cache_creation_input_tokens;
        assert!(consumed3 <= total3);
    }

    #[test]
    fn ttl_is_inherited_for_derived_message_breakpoints() {
        // 升级后 tools / system 末尾都自动获得默认 5m TTL，
        // active_ttl 被重置为 5m。直到显式标了 1h 的 user message，
        // active_ttl 切回 1h，之后所有 message_end 派生断点都继承 1h。
        let req = build_request(vec![
            msg(
                "user",
                serde_json::json!([{
                    "type": "text",
                    "text": long_cacheable_text(),
                    "cache_control": { "type": "ephemeral", "ttl": "1h" }
                }]),
            ),
            msg("assistant", serde_json::json!("R1")),
            msg("user", serde_json::json!("R2")),
        ]);
        let tracker = CacheTracker::new(Duration::from_secs(3600));
        let profile = tracker.build_profile(&req, estimate_input_tokens(&req));
        let breakpoints = profile.cacheable_breakpoints();
        assert!(breakpoints.len() >= 2);
        // 至少存在一个继承了 1h 的派生断点（显式 1h user 之后的某个 message_end）。
        let one_hour_count = breakpoints
            .iter()
            .filter(|bp| bp.ttl == Duration::from_secs(3600))
            .count();
        assert!(
            one_hour_count >= 1,
            "expected at least 1 inherited 1h breakpoint, got {one_hour_count}"
        );
    }

    #[test]
    fn tool_changes_invalidate_downstream_prefix() {
        let tracker = CacheTracker::new(Duration::from_secs(3600));
        let mut req1 = build_request(vec![msg("user", cache_text(&long_cacheable_text()))]);
        req1.tools.as_mut().unwrap().push(Tool {
            tool_type: None,
            name: "alpha".to_string(),
            description: "alpha".to_string(),
            input_schema: Default::default(),
            max_uses: None,
            cache_control: None,
        });
        let total1 = estimate_input_tokens(&req1);
        let profile1 = tracker.build_profile(&req1, total1);
        tracker.update(1, &profile1);

        let mut req2 = build_request(vec![msg("user", cache_text(&long_cacheable_text()))]);
        req2.tools.as_mut().unwrap().push(Tool {
            tool_type: None,
            name: "beta".to_string(),
            description: "beta".to_string(),
            input_schema: Default::default(),
            max_uses: None,
            cache_control: None,
        });
        let total2 = estimate_input_tokens(&req2);
        let profile2 = tracker.build_profile(&req2, total2);
        let result = tracker.compute(1, &profile2);

        assert_eq!(result.cache_read_input_tokens, 0);
        assert_eq!(
            result.cache_creation_input_tokens,
            profile2
                .last_cacheable_breakpoint()
                .map(|bp| bp.cumulative_tokens)
                .unwrap_or(0)
        );
    }
}
