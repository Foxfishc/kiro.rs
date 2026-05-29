# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## 项目概述

kiro-rs 是一个用 Rust 编写的 Anthropic Claude API 兼容代理服务，将 Anthropic API 请求转换为 Kiro API 请求。支持多凭据管理、自动故障转移、流式响应和 Web 管理界面。

**技术栈**: Rust (Axum 0.8 + Tokio) + React 18 + TypeScript + Tailwind CSS

## 常用命令

```bash
# 使用 Makefile（推荐）
make help          # 查看所有可用命令
make release       # 构建前端 + 后端（release）
make dev           # 开发运行（自动构建前端，启用 sensitive-logs）
make check         # 格式化 + lint + 测试
make ui            # 仅构建前端
make ui-dev        # 前端开发服务器

# 手动构建（必须先构建前端）
cd admin-ui && pnpm install && pnpm build
cargo build --release

# 开发运行
cargo run -- -c config.json --credentials credentials.json

# 测试
cargo test
cargo test <test_name>           # 运行单个测试

# 代码检查
cargo fmt          # 格式化
cargo clippy       # lint

# 启用敏感日志构建（排障用，输出 token 用量等诊断信息）
cargo run --features sensitive-logs -- -c config.json --credentials credentials.json

# 前端开发
cd admin-ui
pnpm install
pnpm dev           # 开发服务器
pnpm build         # 生产构建

# 排障工具
python tools/test_400_improperly_formed.py  # 测试上游 400 错误场景
```

## 请求处理流程

```
POST /v1/messages (Anthropic 格式)
  → auth_middleware: 验证 x-api-key / Bearer token（subtle 常量时间比较）
  → post_messages handler:
      1. 判断 WebSearch 触发条件，决定本地处理或剔除后转发
      2. converter::convert_request() 转换为 Kiro 请求格式
      3. provider.call_api() 发送请求（含重试和故障转移）
      4. stream.rs 解析 AWS Event Stream → 转换为 Anthropic SSE 格式返回
```

## 核心设计模式

1. **Provider Pattern** - `kiro/provider.rs`: 统一的 API 提供者接口，处理请求转发和重试。支持凭据级代理（每个凭据可配独立 HTTP/SOCKS5 代理，缓存对应 HTTP Client 避免重复创建）
2. **Multi-Token Manager** - `kiro/token_manager.rs`: 多凭据管理，按优先级故障转移，后台异步刷新 Token（支持 Social 和 IdC 两种认证方式）。余额缓存动态 TTL：高频用户 10 分钟、低频用户 30 分钟、低余额用户 24 小时，过期时异步刷新不阻塞请求
3. **Protocol Converter** - `anthropic/converter.rs`: Anthropic ↔ Kiro 双向协议转换，包括模型映射（sonnet/opus/haiku → Kiro 模型 ID）、JSON Schema 规范化（修复 MCP 工具的 `required: null` / `properties: null`）、工具占位符生成、图片格式转换
4. **Event Stream Parser** - `kiro/parser/`: AWS Event Stream 二进制协议解析（header + payload + CRC32C 校验）
5. **Streaming Response** - `anthropic/stream.rs`: 使用 `StreamContext` 实时将 Kiro 事件转换为 Anthropic SSE，最终 usage 采用本地估算口径并透传上游 `meteringEvent` 诊断信息
6. **Input Compressor** - `anthropic/compressor.rs`: 多层压缩管道（空白压缩 → thinking 截断 → tool_result 截断 → tool_use input 截断 → 历史截断），自动修复 tool_use/tool_result 配对以避免上游 400 错误
7. **Image Processor** - `image.rs`: 图片处理（缩放、GIF 抽帧、token 计算）。GIF 抽帧策略：最多 20 帧、最多 5fps、按时长自适应采样间隔，输出为 JPEG 静态帧序列
8. **Affinity & Load Balance** - `kiro/affinity.rs`: 凭据亲和性 + 负载均衡。retry 链路使用 `exclude_ids` 强制跳过上次失败凭据（避免撞回同一个失败凭据）；新凭据 `recent_usage` 用现有凭据中位数作 baseline，防止「雷暴」（瞬间被全量打到）
9. **Background Refresh** - `kiro/background_refresh.rs`: 周期性刷新余额缓存（10 分钟周期），余额不足主动禁用凭据
10. **Cache Tracker** - `anthropic/cache_tracker.rs`: 跟踪 Anthropic prompt cache 命中率。`expires_at` 从首次写入算（命中和后续 update 都不刷新），TTL 与 Anthropic 真实行为对齐。仅 `model` 进入 prefix prelude；隐式断点须在出现显式 `cache_control` 之后启用；命中应用 0.95 上限与 Opus 4096 / 其他 1024 最小可缓存阈值。
11. **Rate Limiter** - `kiro/rate_limiter.rs`: 凭据级本地限流。`credentialRpm: 0` 真禁用所有本地限流检查（不再回落到默认值）
12. **Web Portal & Overage** - `kiro/web_portal.rs` + `kiro/overage.rs`: 通过 Web Portal 与 `GetUserUsageAndLimits` 同步 overage 状态；余额展示/缓存/自动禁用使用「基础额度 + 超额额度」的有效额度

## 共享状态

```rust
AppState {
    api_key: String,                          // Anthropic API 认证密钥
    kiro_provider: Option<Arc<KiroProvider>>,  // 核心 API 提供者（Arc 线程安全共享）
    profile_arn: Option<String>,               // AWS Profile ARN
    compression_config: CompressionConfig,     // 输入压缩配置
}
```

通过 Axum `State` extractor 注入到所有 handler 中。

## 凭据故障转移与冷却

- 凭据按 `priority` 字段排序，优先使用高优先级凭据
- 请求失败时 `report_failure()` 触发故障转移到下一个可用凭据
- 冷却分类管理：`FailureLimit` / `InsufficientBalance` / `ModelUnavailable` / `QuotaExceeded`
- `MODEL_TEMPORARILY_UNAVAILABLE` 触发全局熔断，禁用所有凭据
- Retry 链路通过 `exclude_ids` 跳过上次失败凭据（参见 `affinity.rs`）
- KIRO_API_KEY 环境变量启动时会作为最高优先级 runtime-only 凭据插入

## API 端点

**代理端点**:
- `GET /v1/models` - 获取可用模型列表
- `POST /v1/messages` - 创建消息（Anthropic 格式）
- `POST /v1/messages/count_tokens` - Token 计数

**Admin API** (需配置 `adminApiKey`):
- 凭据 CRUD、状态监控、余额查询

## 重要注意事项

1. **构建顺序**: 必须先构建前端 `admin-ui`，再编译 Rust 后端（静态文件通过 `rust-embed` 嵌入，derive 宏为 `#[derive(Embed)]`）
2. **凭据格式**: 支持单凭据（向后兼容）和多凭据（数组格式，支持 priority 字段）
3. **重试策略**: 单凭据最多重试 2 次，单请求最多重试 3 次
4. **WebSearch 工具**: 仅当请求明确触发 WebSearch（`tool_choice` 强制 / 仅提供 `web_search` 单工具 / 消息前缀匹配）时走本地 WebSearch；否则从 `tools` 中剔除 `web_search` 后转发上游（避免误路由）
5. **安全**: 使用 `subtle` 库进行常量时间比较防止时序攻击；Admin API Key 空字符串视为未配置
6. **Prefill 处理**: Claude 4.x 已弃用 assistant prefill，末尾 assistant 消息被静默丢弃
7. **sensitive-logs 特性**: 编译时 feature flag，启用后输出 token 用量诊断日志和请求体大小（默认关闭，仅用于排障）
8. **网络错误分类**: 连接关闭/重置、发送失败等网络错误被归类为瞬态上游错误，返回 502（不记录请求体）
9. **Rust edition**: 项目使用 Rust 2024 edition
10. **图片处理**: GIF 会被抽帧并重编码为 JPEG 静态帧序列（最多 20 帧、最多 5fps），以降低请求体大小并提升内容识别效果。图片缩放规则：长边超过 4000px 或总像素超过 400 万时等比缩放
11. **输入压缩**: 当请求体接近上游限制（约 5MB）时，自动执行多层压缩（空白压缩 → thinking 截断 → tool_result 截断 → tool_use input 截断 → 历史截断），并自动修复 tool_use/tool_result 配对以避免上游 400 错误
12. **上游 400 排障**: 若遇到 `Improperly formed request` 错误，参考 `docs/troubleshooting/400-improperly-formed-request.md` 和 `tools/test_400_improperly_formed.py` 进行诊断
13. **TLS 后端**: 默认 `rustls`，遇到 token 刷新失败/`error request` 等问题可通过 `config.json` 的 `tlsBackend` 切回 `native-tls`
14. **代理优先级**: 凭据级代理 > 全局代理 > 无代理；凭据可显式设 `direct` 强制直连覆盖全局代理
15. **Thinking 模型路由**: `claude-opus-4-8-thinking` / `claude-opus-4-7-thinking` / `claude-opus-4-6-thinking` 走 adaptive thinking；客户端选普通模型但请求带 `thinking` 参数也会启用思考（无效预算默认 high）
16. **代码风格**: 项目内日志、注释、文档均使用中文（与 fork 上游保持一致）。新增代码请保持中文注释/日志风格
