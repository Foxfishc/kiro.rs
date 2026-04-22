# Changelog

## [Unreleased]

### Added
- **新增 Kiro API Key / headless 凭据支持** — 后端凭据模型、TokenManager、Provider 与 Admin API/UI 现已支持 `kiroApiKey` / `authMethod=api_key`，API Key 凭据会直接作为 Bearer token 使用并自动补充 `tokentype=API_KEY`；同时支持通过 `KIRO_API_KEY` 环境变量注入最高优先级凭据，补齐 machine_id 派生、额度查询兼容与 Admin 添加凭据表单切换 (`src/main.rs`, `src/kiro/model/credentials.rs`, `src/kiro/machine_id.rs`, `src/kiro/token_manager.rs`, `src/kiro/provider.rs`, `src/admin/types.rs`, `src/admin/service.rs`, `admin-ui/src/types/api.ts`, `admin-ui/src/components/add-credential-dialog.tsx`)

## [v1.1.23] - 2026-04-18

### Changed
- **Admin UI 支持即时配置 Prompt Cache 与凭据节流** — 全局配置弹窗新增 `credentialRpm`、`promptCacheTtlSeconds`（仅支持 5 分钟/1 小时两档）与 `promptCacheAccountingEnabled` 的前端回显与保存；Admin `/config/global` 同步暴露这三个字段，并将 Prompt Cache 改为共享运行时热更新，保存后新请求立即按最新 TTL 与记账开关生效 (`admin-ui/src/components/global-config-dialog.tsx`, `admin-ui/src/types/api.ts`, `src/admin/types.rs`, `src/admin/service.rs`, `src/anthropic/middleware.rs`, `src/anthropic/handlers.rs`, `src/anthropic/router.rs`, `src/main.rs`)

## [v1.1.22] - 2026-04-17

### Changed
- **429 限流改为凭据级冷却与自动分流** — Provider 现在会单独识别 429，解析并裁剪 `Retry-After`，将触发限流的凭据标记为 `RateLimitExceeded` 冷却；后续请求优先切换到其他可用凭据，无可切换时复用现有最短等待机制自动放慢转发速度，同时避免把 429 计入失败次数导致误禁号；补充了 `Retry-After`、默认冷却、切号、单号等待与亲和性绑定回切等回归测试 (`src/kiro/provider.rs`, `src/kiro/token_manager.rs`)
- **WebSearch 失败路径保持 cache usage 字段稳定** — 在 prompt cache accounting 开启时，MCP 失败降级路径继续返回默认的 `WebSearchCacheContext`，确保 `cache_creation_input_tokens` / `cache_read_input_tokens` 在失败响应中仍稳定存在且为 0；新增失败路径回归测试，避免 usage schema 只在异常时回退 (`src/anthropic/websearch.rs`)
- **新增 promptCacheAccountingEnabled 开关并统一三条 usage 路径** — 新增配置 `promptCacheAccountingEnabled`，默认开启；关闭后会跳过本地伪缓存读写，并且 stream / non-stream / websearch 三条 usage 路径都不再输出或扣减 cache token。关键改动覆盖 `src/model/config.rs`、`src/anthropic/middleware.rs`、`src/anthropic/handlers.rs`、`src/anthropic/stream.rs`、`src/anthropic/websearch.rs`，示例配置已同步到 `config.example.json`。

## [v1.1.21] - 2026-04-16

### 新增
- **新增 Opus 4.7 模型别名支持** — 在 `/v1/models` 中暴露 `claude-opus-4-7` 及其 thinking/agentic 变体，并补齐 Anthropic 别名到上游 Opus 4.7 模型的映射，保持现有 Opus 4.6 使用方式一致 (`src/anthropic/handlers.rs`, `src/anthropic/converter.rs`)

## [v1.1.20] - 2026-04-16

### Changed
- **上游兼容性与请求头对齐** — MCP 请求按认证类型补充 `x-amzn-kiro-profile-arn`，对齐 Token 刷新与额度查询的 `User-Agent` / `x-amz-user-agent` 生成逻辑，升级 provider UA 版本并同步默认 Kiro/Node 版本；同时保持 IDC / Builder ID 场景不发送 `profileArn` 以避免 403 (`src/kiro/provider.rs`, `src/kiro/token_manager.rs`, `src/model/config.rs`)
- **前端依赖锁文件纳管与 Anthropic 模块整洁度提升** — 取消忽略 `admin-ui/pnpm-lock.yaml` 以保证 pnpm 依赖可复现，并将 `converter` / `handlers` / `stream` 的内部上下文与 usage 参数收口，清理 clippy 基线而不改变现有行为 (`.gitignore`, `admin-ui/pnpm-lock.yaml`, `src/anthropic/converter.rs`, `src/anthropic/handlers.rs`, `src/anthropic/stream.rs`)
- **新增 Opus 4.7 模型列表与映射支持** — `/v1/models` 现已暴露 `claude-opus-4-7` 及其 `thinking` / `agentic` 变体，并在 Anthropic→Kiro 模型映射中补齐 `claude-opus-4-7` / `claude-opus-4.7` 解析与对应回归测试，保持与现有 Opus 4.6 用法一致 (`src/anthropic/handlers.rs`, `src/anthropic/converter.rs`)

## [v1.1.19] - 2026-04-04

### 修复
- **Admin 余额启动恢复与展示统一** — 启动时将磁盘余额缓存回灌到运行时缓存；缓存余额接口扩展返回 `usageLimit`/`usagePercentage`/`subscriptionTitle`，使页面刷新后卡片仍能以完整格式展示；订阅等级改为三级回退：实时余额 → 缓存余额 → 凭据快照；`invalid_grant` 的 IdC refresh 失败与 `TEMPORARILY_SUSPENDED` 额度查询失败立即禁用凭据；`get_usage_limits_for()` 成功后回写并持久化 `subscription_title`

## [v1.1.18] - 2026-04-04

### 修复
- **Admin UI 余额与订阅展示恢复** — 启动时将磁盘余额缓存回灌到运行时缓存；缓存余额接口扩展返回 `usageLimit`/`usagePercentage`/`subscriptionTitle`，使页面刷新后卡片仍能以完整格式（`余额：41.05 / 50.00 (82.1% 剩余)`）展示，而非仅显示金额；订阅等级改为三级回退：实时余额 → 缓存余额 → 凭据快照 (`src/admin/service.rs`, `src/admin/types.rs`, `src/kiro/token_manager.rs`, `admin-ui/`)
- **失效凭据立即禁用与订阅等级回写** — `invalid_grant` 的 IdC refresh 失败现在会立即禁用凭据，`TEMPORARILY_SUSPENDED` 的额度查询失败也会立即禁用凭据；同时 `get_usage_limits_for()` 成功后会回写并持久化 `subscription_title`，避免 Free 账号在订阅信息未刷新时误放行 Opus (`src/kiro/token_manager.rs`)

## [v1.1.17] - 2026-04-04

### 修复
- **CI: pnpm-workspace.yaml 添加 packages 字段** — 修复 pnpm workspace 配置缺少 packages 字段的问题

## [v1.1.16] - 2026-04-03

### Fixed
- **工具名超过 63 字符报错修复** — 新增 `shorten_tool_name()`（SHA256 确定性缩短）和 `map_tool_name()` 映射机制，对超长工具名自动缩短并在请求/响应中双向还原，避免 Kiro API 拒绝超长 MCP 工具名。影响 `converter.rs`、`stream.rs`、`handlers.rs`
- **JSON 格式 user_id 中提取 session_id** — 扩展 `extract_session_id()` 同时支持旧格式字符串 `...session_<uuid>` 和 JSON 格式 `{"session_id":"UUID"}`，修复部分客户端 conversationId 无法正确生成的问题 (`src/anthropic/converter.rs`)

### Added
- **KAM 1.8.3 新版平铺格式导入兼容** — 在现有导入对话框中新增 `normalizeKamAccount()` 转换层，自动识别 KAM 1.8.3+ 平铺格式并归一化为旧嵌套结构，复用已有预览/导入/验活流程 (`admin-ui/src/components/import-token-json-dialog.tsx`)
- **Token 刷新失败独立计数与禁用策略** — `CredentialEntry` 新增 `refresh_failure_count`，刷新失败不再污染 API 调用失败计数；达到阈值后独立禁用（`RefreshFailureLimit`），刷新成功自动解除。Admin 快照暴露 `refresh_failure_count` 和 `disabled_reason` (`src/kiro/token_manager.rs`, `src/admin/types.rs`, `src/admin/service.rs`)
- **强制刷新 Token 功能** — 新增 `POST /api/admin/credentials/:id/refresh` 接口和前端单个/批量刷新入口，支持手动触发 Token 刷新并重置失败计数 (`src/admin/handlers.rs`, `src/admin/router.rs`, `admin-ui/`)

## [v1.1.15] - 2026-04-01

### Fixed
- **减少向下游发送人为 "." 占位符** — 调整 Anthropic→Kiro 转换与压缩末尾修复：history/assistant 的 tool-only / 非文本载荷场景优先保留真实结构，不再在早期转换阶段统一补 `"."`；`tool_result.content[*].text` 空项改为优先删除，仅在删除后会变成空内容时才最小兜底，降低伪文本进入下游的概率，同时保留既有 400 防线 (`src/anthropic/converter.rs`, `src/anthropic/compressor.rs`)
- **Prompt Cache 前缀命中与归一化修复** — `cache_tracker` 改为基于前缀指纹匹配最近断点，避免不同消息形态产生误命中；同时将 `x-anthropic-billing-header` 归一化，确保计费头漂移不破坏缓存复用；`/v1/messages` 在 provisional cache 计算前先剔除空 text block，避免 tool_use-only 历史污染缓存指纹 (`src/anthropic/cache_tracker.rs`, `src/anthropic/handlers.rs`)

## [v1.1.14] - 2026-03-31

### Fixed
- **billed input_tokens 继续扣除 cache creation** — 统一 `handlers`、流式 SSE 与本地 WebSearch 三条路径的 billed input 计算，`usage.input_tokens` 现在同时扣除 `cache_creation_input_tokens` 与 `cache_read_input_tokens`，并同步更新相关断言以锁定一致语义 (`src/anthropic/handlers.rs`, `src/anthropic/stream.rs`, `src/anthropic/websearch.rs`)

## [v1.1.13] - 2026-03-25

### Fixed
- **流式与非流式 usage 口径对齐** — `/v1/messages` 流式响应补齐 `cache_creation_input_tokens` / `cache_read_input_tokens`，并将 `message_delta.usage.input_tokens` 统一为 billed input；同时增强 `tools/test_prompt_cache_usage.mjs`，支持显式 stream/non-stream 测试、SSE 解析与更详细 usage 输出 (`src/anthropic/handlers.rs`, `src/anthropic/stream.rs`, `tools/test_prompt_cache_usage.mjs`)

### Added

### Changed
- **请求访问日志统一输出实际路径** — `/v1` 的 `models`、`messages`、`count_tokens` 日志统一输出 `path` 字段并使用一致消息名，便于日志检索与区分实际访问接口 (`src/anthropic/handlers.rs`)

### Added
- **Prompt Cache 本地命中追踪** — 新增 `cache_tracker` 模块，本地按请求内容和凭据维度模拟 Prompt Caching 命中，并让 `KiroProvider` 返回实际命中的 `credential_id`，在成功请求后更新缓存状态 (`src/anthropic/cache_tracker.rs`, `src/anthropic/handlers.rs`, `src/kiro/provider.rs`, `src/token.rs`)
- **Prompt Cache TTL 配置化** — 新增 `promptCacheTtlSeconds` 配置项，替代原先 300 秒硬编码，支持通过配置文件调整本地 Prompt Cache 的 TTL (`src/model/config.rs`, `src/anthropic/middleware.rs`, `src/anthropic/router.rs`, `src/main.rs`, `config.example.json`)

### Changed
- **Anthropic 类型补齐 cache_control 字段** — 为 `SystemMessage` 和 `Tool` 增加 `cache_control` 支持，并同步修正相关测试构造，确保缓存标记可以进入本地追踪逻辑 (`src/anthropic/types.rs`, `src/anthropic/converter.rs`, `src/anthropic/websearch.rs`, `src/anthropic/handlers.rs`)

### Added
- **Kiro credit usage 透传** — 新增 `meteringEvent` 真实 payload 解析模型，并在流式 `/v1` 与非流式响应的 `usage` / `message_delta.usage` 中透传 `credit_usage`、`credit_unit`、`credit_unit_plural`，同时保持现有 `input_tokens` 与 `cache_*` 本地兼容语义不变 (`src/kiro/model/events/metering.rs`, `src/kiro/model/events/base.rs`, `src/kiro/model/events/mod.rs`, `src/anthropic/stream.rs`, `src/anthropic/handlers.rs`)
- **credit usage 回填回归测试** — 补充 `meteringEvent` 解析、流式 `message_delta.usage` 与非流式 usage 注入等单测，锁定透传行为 (`src/kiro/model/events/base.rs`, `src/anthropic/stream.rs`, `src/anthropic/handlers.rs`)

## [v1.1.12] - 2026-03-17

### Fixed
- **Usage 信息始终返回估算值** — `StreamContext` 的 `final_input_tokens` 改为始终使用本地估算值（`input_tokens`），不再使用上游 `contextUsageEvent` 计算的值；避免因服务端压缩导致返回的 token 数偏低，使客户端误判上下文大小并重复触发压缩；`context_input_tokens` 仍保留用于日志记录和 `context_usage_percentage >= 100%` 判断 (`src/anthropic/stream.rs`)

## [v1.1.11] - 2026-03-15

### Fixed
- **无凭据场景错误映射优化** — 当 `available_count() == 0`（无凭据或全部禁用）时，`call_api_with_retry` 和 `call_mcp_with_retry` 前置检查直接返回 `"没有可用的凭据"` 错误，避免进入 0 次重试的假逻辑；新增 `is_no_credentials_error()` 判定函数，在 `map_kiro_provider_error_to_response` 中映射为 503 SERVICE_UNAVAILABLE + `service_unavailable` 错误类型，替代原先的泛化 502 错误；补充单元测试验证错误识别逻辑 (`src/kiro/provider.rs`, `src/anthropic/handlers.rs`)

## [v1.1.10] - 2026-03-15

### Fixed
- **SSE 初始化不再预创建空 text block** — `generate_initial_events` 仅发送 `message_start`，不再预创建 `text=""` 的空文本块；当模型首个输出为 tool_use 且无任何 text_delta 时，避免产生空 text content block 被客户端写回 history 后触发上游 400 校验拒绝；新增 `strip_empty_text_content_blocks` 防御性函数和 `test_tool_use_only_does_not_emit_empty_text_block` 回归测试 (`src/anthropic/stream.rs`, `src/anthropic/handlers.rs`)

## [v1.1.9] - 2026-03-15

### 新增
- **Admin UI 凭据列表排序** — 支持按 ID 或余额对凭据列表进行排序
- **全局配置热更新（region / credentialRpm / compression）** — 支持在 Admin UI 中实时修改全局 Region、单凭据目标 RPM 和输入压缩配置，无需重启服务

### Fixed
- **SSE 初始化不再预创建空 text block** — `generate_initial_events` 仅发送 `message_start`，不再预创建 `text=""` 的空文本块；当模型首个输出为 tool_use 且无任何 text_delta 时，避免产生空 text content block 被客户端写回 history 后触发上游 400 校验拒绝；新增 `strip_empty_text_content_blocks` 防御性函数和 `test_tool_use_only_does_not_emit_empty_text_block` 回归测试 (`src/anthropic/stream.rs`, `src/anthropic/handlers.rs`)

## [v1.1.8] - 2026-03-15

### Added
- **Admin UI 全局代理配置热更新** — 新增 `GET/POST /api/admin/proxy` 端点，支持在 Admin UI 中查看和修改全局代理配置（proxyUrl/proxyUsername/proxyPassword），修改后运行时立即生效无需重启；`KiroProvider.global_proxy`/`default_client` 和 `MultiTokenManager.proxy` 改为 `RwLock` 实现内部可变性，更新时自动重建 HTTP Client 并清空凭据级 Client 缓存；配置变更同步持久化到 `config.json`；前端新增代理配置对话框和 Dashboard 入口卡片 (`src/kiro/provider.rs`, `src/kiro/token_manager.rs`, `src/admin/service.rs`, `src/admin/handlers.rs`, `src/admin/router.rs`, `src/main.rs`, `admin-ui/src/components/proxy-config-dialog.tsx`, `admin-ui/src/components/dashboard.tsx`)
- **全局配置热更新（credentialRpm / region / compression）** — 新增 `GET/PUT /api/admin/config/global` 端点，支持在 Admin UI 中实时修改全局 Region、单凭据目标 RPM 和输入压缩配置（enabled / maxRequestBodyBytes），无需重启服务；`AppState.compression_config` 改为 `Arc<RwLock<CompressionConfig>>` 实现运行时共享更新；`RateLimiter.config` 改为 `RwLock<RateLimitConfig>` 并新增 `update_config()` 方法；`MultiTokenManager.config` 改为 `RwLock<Config>` 并新增 `update_region()` / `update_credential_rpm()` 方法；前端新增全局配置对话框（Region、RPM、代理、压缩）替代原代理配置卡片 (`src/anthropic/middleware.rs`, `src/anthropic/router.rs`, `src/anthropic/handlers.rs`, `src/kiro/rate_limiter.rs`, `src/kiro/token_manager.rs`, `src/kiro/provider.rs`, `src/admin/types.rs`, `src/admin/service.rs`, `src/admin/handlers.rs`, `src/admin/router.rs`, `src/main.rs`, `admin-ui/src/components/global-config-dialog.tsx`, `admin-ui/src/components/dashboard.tsx`)
- **图片全局 Cap 20 张** — 所有图片（静态图 + GIF 抽帧 + history 图片）合计不超过 20 张，通过 `remaining_image_budget` 在 `process_message_content` / `merge_user_messages` / `build_history` 调用链中传递配额；currentMessage 优先消耗配额，history 使用剩余配额；GIF 抽帧时动态传递剩余 budget 限制最大帧数 (`src/anthropic/converter.rs`, `src/image.rs`)
- **上游 400 诊断日志增强** — 工具统计日志（重复检测、placeholder 计数）、`validate_tool_pairing` 异常汇总日志、`build_history` 历史结构摘要日志、图片统计日志；`is_improperly_formed_request_error` 分支增加 `kiro_request_body_bytes` 字段 (`src/anthropic/converter.rs`, `src/anthropic/handlers.rs`)
- **诊断工具增强** — `diagnose_improper_request.py` 新增图片数量超限 (`E_IMAGE_COUNT_EXCEEDS_LIMIT`)、工具名称重复 (`W_TOOL_NAME_DUPLICATE`)、history role 交替 (`W_HISTORY_ROLE_NOT_ALTERNATING`) 三项检查 (`tools/diagnose_improper_request.py`)

## [v1.1.7] - 2026-03-14

### Fixed
- **tool_result 空 text 字段兜底修复** — `repair_non_empty_content_pass` 扩展覆盖 `tool_result` content 数组中 `text` 字段为空字符串或纯空白的场景，替换为 "."，防止上游因空 text 返回 400；新增 4 个回归测试覆盖 history/current_message/纯空白/非空不修改四种情况 (`src/anthropic/compressor.rs`)

## [v1.1.6] - 2026-03-11

### Fixed
- **压缩后空 content 兜底修复** — 在输入压缩管道末尾新增统一修复 pass，将空/全空白 content 替换为 "."，覆盖空白压缩、thinking discard、tool pairing 修复后的空内容场景；新增回归测试确保 history/current/assistant 均不会产生空 text block (`src/anthropic/compressor.rs`)

## [v1.1.5] - 2026-03-02

### Fixed
- **防止孤立 tool_result 导致空消息** — `convert_user_message` 新增最终兜底逻辑，当 tool_result 被过滤为孤立块后 content 变空时插入占位符 "."，避免上游返回 400 "Improperly formed request"；新增回归测试覆盖孤立 tool_result 场景 (`src/anthropic/converter.rs`)

## [v1.1.4] - 2026-03-02

### Fixed
- **WebSearch 历史消息上下文保留** — `convert_assistant_message` 新增对 `server_tool_use`（忽略）和 `web_search_tool_result`（提取 title、url、snippet、page_age 为纯文本）的处理，修复多轮对话中搜索结果被静默丢弃导致 Kiro 丢失上下文的问题；使用纯文本格式彻底规避特殊字符破坏格式的风险；新增 2 个单元测试覆盖上述路径 (`src/anthropic/converter.rs`)

## [v1.1.3] - 2026-02-27

### Changed
- **合并三个导入对话框为一个** — 删除冗余的 `KamImportDialog` 和 `BatchImportDialog`，统一使用 `ImportTokenJsonDialog`，自动识别 KAM 嵌套格式、扁平凭据格式和 Token JSON 格式 (`admin-ui/src/components/`)
- **上游网络错误归类为瞬态错误** — `error sending request`/`connection closed`/`connection reset` 纳入 `is_transient_upstream_error`，返回 502 且不输出请求体 (`src/anthropic/handlers.rs`)
- **上游报错不再输出完整请求体** — `sensitive-logs` 模式下仅输出请求体字节数 (`src/anthropic/handlers.rs`)
- **瞬态错误匹配大小写归一化** — `is_transient_upstream_error` 统一 `.to_lowercase()` 后匹配，提取 `NETWORK_ERROR_PATTERNS` 常量消除重复 (`src/anthropic/handlers.rs`)

### Removed
- **移除负载均衡模式切换** — 删除前端按钮/API/hooks 和后端路由/handler/service/types/config 字段/token_manager 方法及测试，该设置实际未被使用 (`admin-ui/`, `src/admin/`, `src/kiro/token_manager.rs`, `src/model/config.rs`)

### Added
- **Usage 诊断日志** — 流式/缓冲流式/非流式三条路径均增加 `sensitive-logs` 保护的 usage 日志，输出 estimated/context/final input_tokens 及来源、output_tokens (`src/anthropic/stream.rs`, `src/anthropic/handlers.rs`)

## [v1.1.2] - 2026-02-27

### Added
- **Kiro Account Manager 导入** — 支持导入 KAM 导出的 JSON 凭据文件 (`admin-ui/src/components/kam-import-dialog.tsx`)
- **批量导入对话框** — 新增独立的批量导入凭据组件 (`admin-ui/src/components/batch-import-dialog.tsx`)
- **凭证 disabled 字段持久化** — 从配置文件读取 disabled 状态，支持手动禁用凭据跨重启保留 (`src/kiro/model/credentials.rs`, `src/kiro/token_manager.rs`)
- **凭据级 Region 编辑** — Admin UI 支持对已有凭据在线修改 `region` 和 `apiRegion`，点击凭据卡片内联编辑，保存后持久化 (`src/admin/`, `admin-ui/src/components/credential-card.tsx`)
- **Admin API `POST /credentials/:id/region`** — 新增 Region 修改接口，支持清除（传 null）或覆盖两个 region 字段 (`src/admin/handlers.rs`, `src/admin/router.rs`)

### Fixed
- **WebSearch SSE 事件序列修正** — 调整 server_tool_use 位置、content block index、page_age 转换、usage 统计 (`src/anthropic/websearch.rs`)
- **Token Manager 统计回写** — 立即回写统计数据，清除已删除凭据残留 (`src/kiro/token_manager.rs`)
- **HTTP 非安全地址批量导入** — 修复 admin-ui 在 HTTP 下的导入错误 (`admin-ui/src/lib/utils.ts`)
- **Docker 端口绑定优化** — 修正端口绑定和配置目录挂载 (`docker-compose.yml`)
- **移除重复 Sonnet 4.6 模型项** — 删除 `/v1/models` 中重复的 claude-sonnet-4-6 条目，避免 id 冲突 (`src/anthropic/handlers.rs`)
- **防止自动禁用状态被持久化** — `persist_credentials()` 仅持久化手动禁用，避免重启后自动禁用变为手动禁用导致无法自愈 (`src/kiro/token_manager.rs`)
- **sha256Hex digest 异常回退** — 在 `crypto.subtle.digest` 外围加 try/catch，失败时回退到纯 JS 实现 (`admin-ui/src/lib/utils.ts`)
- **parseKamJson null 输入保护** — 对 JSON null 输入增加类型检查，避免 TypeError (`admin-ui/src/components/kam-import-dialog.tsx`)
- **额度查询 region 修复** — `get_usage_limits` 改用 `effective_api_region`，凭据指定 region 时不再因走错 endpoint 而 403 (`src/kiro/token_manager.rs`)
- **批量导入丢失 apiRegion** — `TokenJsonItem` 补充 `api_region` 字段，导入 JSON 中的 `apiRegion` 不再被丢弃 (`src/admin/types.rs`, `src/admin/service.rs`)
- **API 请求使用凭据级 region** — `provider.rs` 的 `base_url`/`mcp_url`/`base_domain` 改用 `credentials.effective_api_region()`，凭据配置了 region 时不再错误地走全局 config region 导致 403 (`src/kiro/provider.rs`)
- **Region 编辑 stale state** — 点击编辑 Region 时同步最新 props 到 input，避免后台刷新后提交旧值覆盖服务端数据 (`admin-ui/src/components/credential-card.tsx`)
- **Region 值未 trim** — `set_region` 保存前对 region/apiRegion 做 trim，防止带空格的值持久化后生成无效 URL (`src/admin/service.rs`)
- **过滤超长工具名** — `convert_tools` 过滤掉 name 超过 64 字符的工具，避免上游拒绝整个请求 (`src/anthropic/converter.rs`)
- **429 错误不再输出完整请求体** — 瞬态上游错误（429/5xx）走独立分支返回 429，不触发 sensitive-logs 的请求体诊断日志 (`src/anthropic/handlers.rs`)
- **兼容旧 authRegion 配置** — `credentials.region` 增加 `#[serde(alias = "authRegion")]`，旧配置文件中的 `authRegion` 字段不再被静默忽略 (`src/kiro/model/credentials.rs`)
- **导入凭据 region 规范化** — token.json 导入路径对 region/apiRegion 做 trim + 空字符串转 None，与 `set_region` 逻辑一致 (`src/admin/service.rs`)

### Changed
- **默认 kiro_version 更新至 0.10.0** (`src/model/config.rs`)
- **Opus 模型映射调整** — opus 默认映射到 claude-opus-4.6，仅 4.5/4-5 显式映射到 claude-opus-4.5 (`src/anthropic/converter.rs`)
- **Sonnet 4.6 Model 字段补全** — 添加 context_length、max_completion_tokens、thinking 字段 (`src/anthropic/handlers.rs`)
- **Region 配置精简** — 删除 `credentials.auth_region` 和 `config.auth_region` 冗余字段；凭据的 `region` 同时用于 Token 刷新和 API 请求默认值，`api_region` 可单独覆盖 API 路由 (`src/kiro/model/credentials.rs`, `src/model/config.rs`)
- **添加凭据 UI Region 字段语义调整** — 前端"Auth Region"改为"Region"（对应 `credentials.region`），"API Region"保持，去除无意义的 `authRegion` 前端字段 (`admin-ui/`)

## [v1.1.1] - 2026-02-18

### Fixed
- **修复 Sonnet 4.6 thinking 配置变量名错误** (`src/anthropic/handlers.rs`)
  - 修正 thinking 配置覆写逻辑中的变量名拼写错误

## [v1.1.0] - 2026-02-18

### Added
- **Sonnet 4.6 模型支持** (`src/anthropic/handlers.rs`, `src/anthropic/converter.rs`, `src/anthropic/types.rs`)
  - 添加 claude-sonnet-4-6 及其 thinking/agentic 变体到模型列表
  - 更新模型映射逻辑以正确识别 Sonnet 4.6 版本
  - 为 Sonnet 4.6 启用 1M 上下文窗口和 64K 最大输出 tokens
  - 更新 thinking 配置覆写逻辑以支持 Sonnet 4.6 的 adaptive thinking

## [v1.0.21] - 2026-02-17

### Changed
- **图片处理限制放宽** (`src/model/config.rs`)
  - 单图总像素限制从 1.15M 放宽至 4M，支持更高分辨率图片直接透传
  - 图片长边限制从 1568 放宽至 4000，减少不必要的缩放压缩

## [v1.0.20] - 2026-02-17

### Fixed
- **空消息内容验证与错误分类改进** (`src/anthropic/converter.rs`, `src/anthropic/handlers.rs`)
  - 新增 `ConversionError::EmptyMessageContent` 错误类型，在请求转换阶段验证消息内容不为空
  - 在 prefill 处理后验证最后一条消息内容有效性，支持字符串和数组两种 content 格式
  - 检测空字符串、空白字符、空数组等情况，避免向上游发送无效请求
  - 修复 `is_input_too_long_error` 错误地将 "Improperly formed request" 归类为上下文过长错误
  - 新增 `is_improperly_formed_request_error` 函数专门处理格式错误，返回准确的错误信息
  - 新增 3 个测试用例验证空消息内容检测功能
- **图片文件大小压缩优化** (`src/image.rs`)
  - 新增基于文件大小的强制重新编码逻辑：即使图片尺寸符合要求，如果文件大小超过 200KB 也会重新编码降低质量
  - 修复小尺寸高质量图片（如 483x480 但 631KB）直接透传导致请求体过大的问题
  - 新增日志输出追踪大文件重新编码过程和压缩率

## [v1.0.19] - 2026-02-17

### Changed
- **自适应压缩策略优化** (`src/anthropic/handlers.rs`)
  - 请求体大小校验改为以实际序列化后的总字节数为准，不再扣除图片 base64 字节（上游存在约 5MiB 的硬性请求体大小限制，图片也必须计入）
  - 压缩层级重排：当单条 user content 已超过阈值时优先截断超长消息（第三层），再移除历史消息（第四层），避免移除历史后仍无法降到阈值内
  - 新增 `has_any_tool_results_or_tools` / `has_any_tool_uses` 预检，跳过无效的 tool 阈值降低迭代
  - 历史消息移除改为批量 drain（单轮最多 16 条），提升大上下文场景的压缩效率
- **请求体大小阈值默认上调至 4.5MiB** (`src/model/config.rs`, `config.example.json`)
  - `compression.maxRequestBodyBytes` 从 400KB 上调至 4,718,592 字节（4.5MiB），匹配上游实际限制

### Fixed
- **cargo fmt 格式化** (`src/anthropic/converter.rs`, `src/image.rs`)

## [v1.0.18] - 2026-02-17

### Added
- **GIF 动图抽帧采样与强制重编码** (`src/image.rs`, `src/anthropic/converter.rs`)
  - 新增 `process_gif_frames()` 函数，将 GIF 动图抽帧为多张静态 JPEG，避免动图 base64 体积巨大导致 upstream 400 错误
  - 采样策略：总帧数不超过 20 帧，每秒最多 5 帧，超长 GIF 自动降低采样频率均匀抽取
  - 新增 `process_image_to_format()` 函数，支持将任意图片强制重编码为指定格式
  - GIF 抽帧失败时多级回退：JPEG 重编码 → 静态 GIF 处理 → 原始数据透传
  - `process_image()` 对 GIF 格式强制重编码为静态帧，即使无需缩放也避免透传体积巨大的动图
  - `ImageProcessResult` 新增 `was_reencoded`、`original_bytes_len`、`final_bytes_len` 字段

### Changed
- **请求体大小阈值默认上调** (`src/model/config.rs`, `config.example.json`)
  - 上游存在请求体大小硬限制（实测约 5MiB 左右会触发 400），默认将 `compression.maxRequestBodyBytes` 上调至 4.5MiB 预留安全余量
- **日志分析脚本同步更新** (`tools/analyze_compression.py`, `tools/diagnose_improper_request.py`)
  - 修复 ANSI 序列污染解析，并增加“自适应二次压缩/本地超限拒绝”的统计输出

## [v1.0.17] - 2026-02-15

### Added
- **自适应压缩第四层：超长用户消息内容截断** (`src/anthropic/compressor.rs`, `src/anthropic/handlers.rs`)
  - 新增 `compress_long_messages_pass()` 函数，截断超长的 User 消息 content（保留头部，尾部附加省略标记）
  - 在 `adaptive_shrink_request_body` 的三层策略之后增加第四层兜底，解决单条消息过大（如粘贴整个文件）导致自适应压缩空转的问题
  - 动态计算截断阈值：初始为最大消息字符数的 3/4，每轮递减 3/4，最低 8192 字符
  - 日志新增 `final_message_content_max_chars` 字段便于排查

## [v1.0.16] - 2026-02-15

### Fixed
- **请求体日志仅在 upstream 报错时输出完整内容** (`src/anthropic/handlers.rs`)
  - 移除发送前的完整请求体 DEBUG 日志（`sensitive-logs` 模式下每次请求都输出几十 KB JSON），统一只输出字节大小
  - upstream 报错时在 `sensitive-logs` 模式下以 ERROR 级别输出完整请求体（截断 base64），用于诊断 400/502 等错误

## [v1.0.15] - 2026-02-15

### Added
- **Opus 4.6 1M 上下文窗口支持** (`src/anthropic/types.rs`, `src/anthropic/handlers.rs`, `src/anthropic/stream.rs`)
  - 新增 `get_context_window_size()` 函数，Opus 4.6 返回 1,000,000 tokens，其他模型返回 200,000 tokens
  - 删除硬编码 `CONTEXT_WINDOW_SIZE` 常量，改用动态计算
  - `MAX_BUDGET_TOKENS` 从 24,576 提升到 128,000
  - `Model` 结构体新增 `context_length`、`max_completion_tokens`、`thinking` 字段
- **Agentic 模型变体** (`src/anthropic/converter.rs`, `src/anthropic/handlers.rs`)
  - 新增 sonnet-agentic、opus-4.5-agentic、opus-4.6-agentic、haiku-agentic 四个模型变体
  - `map_model()` 自动剥离 `-agentic` 后缀再映射
  - Agentic 模型注入专用系统提示，引导自主工作模式
- **Thinking level 后缀** (`src/anthropic/handlers.rs`)
  - 支持 `-thinking-minimal`(512)、`-thinking-low`(1024)、`-thinking-medium`(8192)、`-thinking-high`(24576)、`-thinking-xhigh`(32768) 后缀
- **工具压缩** (`src/anthropic/tool_compression.rs` 新建)
  - 20KB 阈值两步压缩：简化 input_schema → 按比例截断 description（最短 50 字符）
- **截断检测** (`src/anthropic/truncation.rs` 新建)
  - 4 种截断类型的启发式检测（空输入、无效 JSON、缺少字段、未闭合字符串）
  - 工具 JSON 解析失败时自动检测截断并生成软失败消息

### Changed
- 工具调用仅含 tool_use 时占位符从 `" "` 改为 `"."`，提升语义清晰度

## [v1.0.14] - 2026-02-15

### Fixed
- **sensitive-logs 模式下请求体日志截断** (`src/kiro/provider.rs`, `src/anthropic/handlers.rs`)
  - 400 错误日志中的 `request_body` 字段改用 `truncate_body_for_log()` 截断（保留头尾各 1200 字符），避免输出包含大量 base64 图片数据的完整请求体
  - 工具输入 JSON 解析失败日志中的 `request_body` 字段改用 `truncate_middle()` 截断
  - 新增 `KiroProvider::truncate_body_for_log()` 函数，正确处理 UTF-8 多字节字符边界

## [v1.0.13] - 2026-02-14

### Fixed
- **请求体大小预检输出 image_bytes 归因信息** (`src/anthropic/handlers.rs`)
  - 新增 `total_image_bytes()` 函数，计算 KiroRequest 中所有图片 base64 数据的总字节数
  - 错误提示信息增加 image_bytes 和 non-image bytes 字段，便于排查请求体大小归因

## [v1.0.12] - 2026-02-14

### Fixed
- **WebSearch 仅纯搜索请求走本地处理** (`src/anthropic/websearch.rs`, `src/anthropic/handlers.rs`)
  - 新增 `should_handle_websearch_request()` 精确判断：仅当 tool_choice 强制选择 web_search、tools 仅含 web_search 单工具、或用户消息包含 `Perform a web search for the query:` 前缀时，才路由到本地 WebSearch 处理
  - 混合工具场景（web_search + 其他工具）改为剔除 web_search 后转发 upstream，避免普通对话被误当成搜索查询
  - 新增 `strip_web_search_tools()` 从 tools 列表中移除 web_search 工具
  - 搜索查询提取增加空白归一化处理

## [v1.0.11] - 2026-02-14

### Added
- **自适应二次压缩策略** (`src/anthropic/handlers.rs`)
  - 请求体超过 `max_request_body_bytes` 阈值时，自动迭代压缩：逐步降低 tool_result/tool_use_input 截断阈值，最后按轮移除最老历史消息
  - 最多迭代 32 轮，避免极端输入导致过长 CPU 消耗
  - `post_messages` 支持自适应压缩
- **压缩后 tool_use/tool_result 配对修复** (`src/anthropic/compressor.rs`)
  - 新增 `repair_tool_pairing_pass()`：历史截断后自动移除孤立的 tool_use 和 tool_result
  - 解决截断破坏跨消息 tool_use→tool_result 配对导致 upstream 返回 400 "Improperly formed request" 的问题
- **stable 版 `floor_char_boundary` 工具函数** (`src/common/utf8.rs`)
  - 新增 `common::utf8` 模块，提供 stable Rust 下的 `floor_char_boundary()` 实现
  - 统一替换项目中散落的 `str::floor_char_boundary()` nightly 调用

### Fixed
- **WebSearch 支持混合工具列表** (`src/anthropic/websearch.rs`)
  - `has_web_search_tool()` 改为只要 tools 中包含 web_search（按 name 或 type 判断）即走本地处理，不再要求 tools 仅有一个
  - `extract_search_query()` 改为取最后一条 user 消息，更符合多轮对话场景
  - 新增非流式（`stream: false`）响应支持，返回完整 JSON 而非 SSE 流

### Changed
- **迁移 `floor_char_boundary` 调用到 `common::utf8` 模块** (`src/anthropic/compressor.rs`, `src/anthropic/stream.rs`, `src/admin/service.rs`, `src/kiro/token_manager.rs`, `src/kiro/provider.rs`)
  - 移除各文件中重复的 `floor_char_boundary` 内联实现，统一使用 `crate::common::utf8::floor_char_boundary`

## [v1.0.10] - 2026-02-12

### Fixed
- **配额耗尽返回 429 而非 502** (`src/anthropic/handlers.rs`, `src/kiro/provider.rs`)
  - 所有凭据配额耗尽时返回 `429 Too Many Requests`（`rate_limit_error`），而非 `502 Bad Gateway`
  - 余额刷新时主动禁用低余额凭据（余额 < 1.0），402 分支同步清零余额缓存
- **亲和性检查不再触发限流** (`src/kiro/token_manager.rs`)
  - 亲和性检查改用 `check_rate_limit` 只读探测，消除"检查本身消耗速率配额"的恶性循环
  - 亲和性分流日志提升至 info 级别并脱敏 user_id，便于生产监控热点凭据

### Added
- **请求体大小预检** (`src/anthropic/handlers.rs`, `src/model/config.rs`)
  - 新增 `max_request_body_bytes` 配置项，序列化后拦截超大请求避免无效 upstream 往返

### Changed
- **移除无意义的 max_tokens 调整逻辑** (`src/anthropic/handlers.rs`)
  - 删除 max_tokens 超限警告日志和调整逻辑，因为该值实际不传递给 Kiro upstream

## [v1.0.9] - 2026-02-12

### Fixed
- **修复 upstream 合并丢失的功能** (`src/anthropic/stream.rs`)
  - 恢复 `stop_reason` 优先级逻辑：高优先级原因可覆盖低优先级（model_context_window_exceeded > max_tokens > tool_use > end_turn）
  - 注释掉重复的 `content_block_start` 日志，避免日志噪音
  - 修复 `contextUsageEvent` 日志格式（保留4位小数）
  - 移除冗余的 `find_char_boundary` 函数，改用标准库 `str::floor_char_boundary()`

## [v1.0.8] - 2026-02-12

### Added
- **批量导入 token.json** (`src/admin/service.rs`, `src/admin/types.rs`)
  - 新增 `import_token_json` 接口，支持批量导入官方 token.json 格式凭据
  - 自动映射 provider/authMethod 字段，支持 dry-run 预览模式
  - 去重检测：通过 refreshToken 前缀匹配避免重复导入

- **缓存余额查询接口** (`src/admin/handlers.rs`, `src/admin/router.rs`)
  - 新增 `GET /admin/credentials/balances/cached` 端点
  - 返回所有凭据的缓存余额信息（含 TTL 和缓存时间）

### Changed
- **用户亲和性支持** (`src/kiro/token_manager.rs`, `src/kiro/provider.rs`)
  - 新增 `UserAffinityManager`：用户与凭据绑定，保持会话连续性
  - `acquire_context_for_user` 方法支持按 user_id 获取绑定凭据
  - 亲和性过期时间可配置，默认 30 分钟

- **余额缓存动态 TTL** (`src/kiro/token_manager.rs`)
  - 基于使用频率动态调整缓存 TTL（高频用户更短 TTL）
  - 新增 `update_balance_cache`、`get_all_cached_balances` 方法
  - 缓存持久化到 `kiro_balance_cache.json`

- **请求压缩管道** (`src/anthropic/converter.rs`, `src/anthropic/middleware.rs`)
  - `AppState` 新增 `compression_config` 字段
  - `convert_request` 支持 `CompressionConfig` 参数
  - 图片压缩、空白压缩、上下文截断等功能集成

- **凭据级代理配置** (`src/kiro/provider.rs`)
  - KiroProvider 支持按凭据动态选择代理
  - 新增 `get_client_for_credential` 方法，缓存凭据级 client
  - API 请求和 MCP 请求均使用凭据的 `effective_proxy()`

- **API Region 分离** (`src/kiro/provider.rs`)
  - `base_url`、`mcp_url`、`base_domain` 使用 `effective_api_region()`
  - 支持 Token 刷新和 API 调用使用不同 region

- **handlers 传递 user_id** (`src/anthropic/handlers.rs`)
  - 从请求 metadata 提取 user_id 并传递给 provider
  - 支持用户亲和性功能

### Fixed
- **修复 mask_user_id UTF-8 panic** (`src/anthropic/handlers.rs`)
  - 使用 `chars()` 按字符而非字节切片，避免多字节字符导致 panic

## [v1.0.7] - 2026-02-10

### Added
- **图片 Token 估算与压缩模块** (`src/image.rs`)
  - 新增 `estimate_image_tokens()`: 从 base64 数据解析图片尺寸并估算 token 数
  - 新增 `process_image()`: 根据配置对大图进行缩放压缩
  - 实现 Anthropic 官方公式: `tokens = (width × height) / 750`
  - 支持 JPEG/PNG/GIF/WebP 格式

### Changed
- **Token 计算支持图片** (`src/token.rs`)
  - `count_all_tokens_local()` 现在处理 `type: "image"` 的 ContentBlock
  - 调用 `estimate_image_tokens()` 计算图片 token，解决之前图片 token 被计为 0 的问题
- **协议转换支持图片压缩** (`src/anthropic/converter.rs`)
  - `process_message_content()` 新增图片压缩处理，根据配置自动缩放超限图片
  - 新增 `count_images_in_request()` 统计请求中图片总数，用于判断多图模式
  - 缩放后记录日志: `图片已缩放: (原始尺寸) -> (缩放后尺寸), tokens: xxx`
- **压缩配置新增图片参数** (`src/model/config.rs`)
  - `image_max_long_edge`: 长边最大像素，默认 1568（Anthropic 推荐值）
  - `image_max_pixels_single`: 单张图片最大总像素，默认 1,150,000（约 1600 tokens）
  - `image_max_pixels_multi`: 多图模式下单张最大像素，默认 4,000,000（2000×2000）
  - `image_multi_threshold`: 触发多图限制的图片数量阈值，默认 20

### Dependencies
- 新增 `image` crate (0.25): 图片处理（支持 JPEG/PNG/GIF/WebP）
- 新增 `base64` crate (0.22): Base64 编解码

## [v1.0.6] - 2026-02-10

### Changed
- **压缩统计日志改用字节单位** (`src/anthropic/handlers.rs`)
  - 移除不准确的 token 估算（`compressed_input_tokens`、`tokens_saved`），改为直接输出字节数
  - 字段重命名：`whitespace_saved` → `whitespace_bytes_saved` 等，明确单位语义
  - 注释更新：说明字节统计用于排查 upstream 请求体大小限制

### Added
- **日志脱敏工具模块** (`src/common/redact.rs`)
  - `redact_opt_string`: Option<String> 脱敏为存在性表示
  - `mask_email`: 邮箱脱敏（保留首字符）
  - `mask_aws_account_id_in_arn`: AWS ARN 中 account id 脱敏
  - `mask_url_userinfo`: URL userinfo 脱敏
  - `mask_user_agent_machine_id`: User-Agent 中 machine_id 脱敏

## [v1.0.5] - 2026-02-09

### Changed
- **请求日志输出压缩前后 token 估算** (`src/anthropic/converter.rs`, `src/anthropic/handlers.rs`)
  - `ConversionResult` 新增 `compression_stats` 字段，将压缩统计从 converter 内部返回给调用方
  - `post_messages` 在 `convert_request()` 之前估算 input tokens，`Received` 日志追加 `estimated_input_tokens`
  - 压缩完成后输出 token 对比日志：`estimated_input_tokens`、`compressed_input_tokens`、`tokens_saved` 及各项压缩明细
  - WebSearch 分支复用已有估算值，消除重复的 `count_all_tokens` 调用
- **`count_all_tokens` 改为接受借用** (`src/token.rs`)
  - 参数从按值传递改为引用（`&str`、`&[Message]`、`&Option<Vec<T>>`），消除 handlers 中的深拷贝开销
- **历史截断统计增加字节数** (`src/anthropic/compressor.rs`)
  - `CompressionStats` 新增 `history_bytes_saved` 字段，`total_saved()` 包含历史截断字节数
  - `compress_history_pass` 返回 `(turns_removed, bytes_saved)` 元组，token 估算准确计入历史截断

## [v1.0.4] - 2026-02-09

### Changed
- **Docker 构建优化**: 引入 `cargo-chef` 实现依赖层缓存，大幅加速重复构建
  - 新增 `planner` 阶段生成依赖 recipe，`builder` 阶段先编译依赖再编译源码
  - Docker 层缓存使得仅源码变更时无需重新编译所有依赖
- **GitHub Actions Docker 构建缓存**: 启用 GHA 缓存（`cache-from/cache-to: type=gha`）
- **Cargo.toml 优化**:
  - `lto` 从 `true`（fat LTO）改为 `"thin"`，加快编译速度同时保持较好的优化效果
  - `tokio` features 从 `full` 精简为实际使用的 5 个 feature（`rt-multi-thread`, `macros`, `net`, `time`, `sync`），减小编译体积

## [v1.0.3] - 2026-02-09

### Fixed
- **opus4-6 上下文超限错误识别** (`src/anthropic/handlers.rs`)
  - 将 `Improperly formed request` 纳入输入过长错误检测，返回 Claude Code 可识别的 `400 Input is too long` 而非 `502 Bad Gateway`
- **Opus 4.6 模型 ID 移除时间日期** (`src/anthropic/handlers.rs`)
  - `claude-opus-4-6-20260206` → `claude-opus-4-6`
- **post_messages 借用冲突编译错误** (`src/anthropic/handlers.rs`)
  - 将 `override_thinking_from_model_name` 调用移至 `user_id` 提取之前，修复 E0502 借用冲突

### Added
- **模型 "thinking" 后缀支持** (`src/anthropic/handlers.rs`, `src/anthropic/converter.rs`)
  - 模型名含 `-thinking` 后缀时自动覆写 thinking 配置（opus4.6 用 adaptive + high effort，其他用 enabled）
  - 模型列表新增 sonnet/opus4.5/opus4.6/haiku 的 thinking 变体

### Docs
- **README.md 补充输入压缩配置文档**
  - 新增"输入压缩"章节，说明 5 层压缩管道机制和 10 个可配置参数

## [v1.0.2] - 2026-02-09

### Fixed
- **opus4-6 上下文超限错误识别** (`src/anthropic/handlers.rs`)
  - 将 `Improperly formed request` 纳入输入过长错误检测，返回 Claude Code 可识别的 `400 Input is too long` 而非 `502 Bad Gateway`
- **Opus 4.6 模型 ID 移除时间日期** (`src/anthropic/handlers.rs`)
  - `claude-opus-4-6-20260206` → `claude-opus-4-6`
- **post_messages 借用冲突编译错误** (`src/anthropic/handlers.rs`)
  - 将 `override_thinking_from_model_name` 调用移至 `user_id` 提取之前，修复 E0502 借用冲突

### Added
- **模型 "thinking" 后缀支持** (`src/anthropic/handlers.rs`, `src/anthropic/converter.rs`)
  - 模型名含 `-thinking` 后缀时自动覆写 thinking 配置（opus4.6 用 adaptive + high effort，其他用 enabled）
  - 模型列表新增 sonnet/opus4.5/opus4.6/haiku 的 thinking 变体

### Docs
- **README.md 补充输入压缩配置文档**
  - 新增"输入压缩"章节，说明 5 层压缩管道机制和 10 个可配置参数

## [v1.0.1] - 2026-02-08

### Fixed
- **历史截断字符统计口径修正** (`src/anthropic/compressor.rs`)
  - `max_history_chars` 从按字节 `.len()` 改为按字符 `.chars().count()`，与配置语义一致
- **remove_thinking_blocks 不再全局 trim** (`src/anthropic/compressor.rs`)
  - 移除末尾 `.trim()`，避免意外吞掉原始内容的首尾空白
- **token.json 导入注释/逻辑统一** (`src/admin/service.rs`)
  - 更新注释：`builder-id → idc`（与实际映射一致）
  - 删除 `auth_method == "builder-id"` 死分支
- **Admin UI 验活开关 added=0 卡住修复** (`admin-ui/src/components/import-token-json-dialog.tsx`)
  - `enableVerify` 开启但无新增凭据时，直接跳转 result 步骤而非卡在 preview
- **工具描述截断 max_description_chars=0 语义修正** (`src/anthropic/converter.rs`)
  - `0` 现在表示"不截断"，而非截断为空字符串

### Added
- **输入压缩管道** (`src/anthropic/compressor.rs`)
  - 新增 5 层压缩管道，规避 Kiro upstream 请求体大小限制
  - 空白压缩：连续空行(3+)→2行，行尾空格移除，保留行首缩进
  - thinking 块处理：支持 discard/truncate/keep 三种策略
  - tool_result 智能截断：按行截断保留头尾，行数不足时回退字符级截断
  - tool_use input 截断：递归截断 JSON 值中的大字符串
  - 历史截断：保留系统消息对，按轮数/字符数从前往后成对移除
  - 12 个单元测试覆盖所有压缩层和边界条件
- **CompressionConfig 配置结构体** (`src/model/config.rs`)
  - 新增 `compression` 配置字段，支持通过 JSON 配置文件调整参数
  - 10 个可配置参数：总开关、空白压缩、thinking 策略、各截断阈值、历史限制
  - 工具描述截断阈值从硬编码 10000 改为可配置（默认 4000）
- **合并 upstream 新功能**: 从 upstream/master 拉取并融合大量新特性
  - **负载均衡模式** (`src/model/config.rs`, `src/kiro/token_manager.rs`, `src/admin/`)
    - 新增 `loadBalancingMode` 配置项，支持 `priority`（默认）和 `balanced`（Least-Used）两种模式
    - Admin API 新增 `GET/PUT /config/load-balancing` 端点
    - 前端新增负载均衡模式切换开关
  - **凭据统计与持久化** (`src/kiro/token_manager.rs`)
    - 新增 `success_count`、`last_used_at` 字段，跟踪每个凭据的调用统计
    - 新增 `save_stats_debounced` 防抖持久化机制，`Drop` 时自动保存
    - 新增 `refreshTokenHash` 字段用于前端重复检测
  - **前端批量操作** (`admin-ui/src/components/dashboard.tsx`)
    - 批量导入对话框 (`BatchImportDialog`)
    - 批量验活对话框 (`BatchVerifyDialog`)
    - 分页控件、批量选择/删除/恢复/验活功能
    - 凭据卡片新增 Checkbox 选择、email 显示、订阅等级、成功次数、剩余用量等信息
  - **配置文件路径管理** (`src/model/config.rs`)
    - `Config` 新增 `config_path` 字段和 `load()`/`save()` 方法，支持配置回写
  - **前端依赖**: 新增 `@radix-ui/react-checkbox` 组件

### Changed
- `convert_request()` 签名新增 `&CompressionConfig` 参数 (`src/anthropic/converter.rs`)
- `convert_tools()` 描述截断阈值参数化 (`src/anthropic/converter.rs`)
- `AppState` 新增 `compression_config` 字段 (`src/anthropic/middleware.rs`)
- `create_router_with_provider()` 新增 `CompressionConfig` 参数 (`src/anthropic/router.rs`)
- 重构 README.md 配置文档，提升新用户上手体验
  - 明确配置文件默认路径：当前工作目录（或通过 `-c`/`--config` 和 `--credentials` 参数指定）
  - 添加 JSON 注释警告：移除所有带 `//` 注释的示例，提供可直接复制的配置
  - 修正字段必填性：仅 `apiKey` 为必填，其他字段均有默认值
  - 新增命令行参数说明表格（`-c`, `--credentials`, `-h`, `-V`）
  - 补充遗漏的 `credentialRpm` 字段说明（凭据级 RPM 限流）
  - 使用表格形式展示配置字段，标注必填/可选和默认值
- 优化 debug 日志中请求体的输出长度 (`src/anthropic/handlers.rs`)
  - 新增 `truncate_middle()` 函数：截断字符串中间部分，保留头尾各 1200 字符
  - 正确处理 UTF-8 多字节字符边界，不会截断中文
  - 仅在启用 `sensitive-logs` feature 时生效，减少日志噪音

### Fixed
- **[P0] API Key 日志泄露修复** (`src/main.rs`)
  - info 级别不再打印 API Key 前半段，仅显示末 4 位和长度
  - 完整前缀仅在 `sensitive-logs` feature 的 debug 级别输出
- **[P2] 占位工具大小写变体重复插入** (`src/anthropic/converter.rs`)
  - `collect_history_tool_names` 改为小写去重，避免 `read`/`Read` 等变体重复
  - 占位工具 push 后同步更新 `existing_tool_names` 集合
- **[P1] 统计与缓存写盘非原子操作** (`src/kiro/token_manager.rs`, `src/admin/service.rs`)
  - 统计数据和余额缓存改为临时文件 + 原子重命名，防止写入中断导致文件损坏
- **[P1] stop_reason 覆盖策略可能丢失信息** (`src/anthropic/stream.rs`)
  - `set_stop_reason()` 改为基于优先级覆盖，高优先级原因可覆盖低优先级原因
- **[P2] snapshot 重复计算 SHA-256** (`src/kiro/token_manager.rs`)
  - `CredentialEntry` 新增 `refresh_token_hash` 缓存字段
  - Token 刷新时自动更新哈希，`snapshot()` 优先使用缓存避免重复计算
- **Clippy 警告修复** (`src/model/config.rs`)
  - 修复 `field_reassign_with_default` 警告，改用结构体初始化语法
- **[P2] Assistant Prefill 静默丢弃** (`src/anthropic/converter.rs`, `src/anthropic/handlers.rs`)
  - 末尾 `assistant` 消息（prefill 场景）不再返回 400 错误，改为静默丢弃并回退到最后一条 `user` 消息
  - Claude 4.x 已弃用 assistant prefill，Kiro API 也不支持，转换器在入口处截断消息列表
  - 移除 `InvalidLastMessageRole` 错误变体，`build_history` 接受预处理后的消息切片
- **[P2] 凭据回写原子性** (`src/kiro/token_manager.rs`)
  - `persist_credentials` 改为临时文件 + `rename` 原子替换
  - 新增 `resolve_symlink_target` 辅助函数：优先 `canonicalize`，失败时用 `read_link` 解析 symlink
  - 保留原文件权限，防止 umask 导致凭据文件权限放宽
  - Windows 兼容：`rename` 前先删除已存在的目标文件
  - 避免进程崩溃或并发调用导致凭据文件损坏
- 限制 `max_tokens` 最大值为 32000（Kiro upstream 限制）
  - 当用户设置超出限制的值时自动调整为 32000
  - 记录 WARN 级别日志，包含原始值和调整后的值
  - 涉及文件：`src/anthropic/handlers.rs`
- 工具输入 JSON 解析失败时的日志输出改为受 `sensitive-logs` feature 控制
  - 默认仅输出 `buffer_len` 和 `request_body_bytes`（长度信息）
  - 启用 `--features sensitive-logs` 时输出完整 `buffer` 和 `request_body`
  - 涉及文件：`src/anthropic/handlers.rs`
- 修复 Kiro upstream 请求兼容性问题
  - 空 content（仅 tool_result/image）时使用占位符避免 400
  - 规范化工具 JSON Schema、补全空 description
  - 禁用 reqwest 系统代理探测（仅支持显式 `config.proxy_url`）
  - 新增离线诊断脚本：`tools/diagnose_improper_request.py`
  - 涉及文件：`src/anthropic/converter.rs`、`src/http_client.rs`、`tools/diagnose_improper_request.py`、`.gitignore`
- 优化 400 Bad Request "输入过长" 错误的日志输出 (`src/kiro/provider.rs`)
  - 对于 `CONTENT_LENGTH_EXCEEDS_THRESHOLD` / `Input is too long` 错误，不再输出完整请求体（太占空间且无调试价值）
  - 改为记录 `request_body_bytes`（字节数）和 `estimated_input_tokens`（估算 token 数）
  - 新增 `estimate_tokens()` 函数：基于 CJK/非 CJK 字符比例估算 token 数量
    - CJK 字符（中/日/韩）: token 数 = 字符数 / 1.5
    - 其他字符（英文等）: token 数 = 字符数 / 3.5
  - 新增 `is_input_too_long()` 和 `is_cjk_char()` 辅助函数

- 新增多维度设备指纹系统 (`src/kiro/fingerprint.rs`)
  - 每个凭据生成独立的确定性指纹，模拟真实 Kiro IDE 客户端
  - 支持 10+ 维度设备信息：SDK 版本、Kiro 版本、Node.js 版本、操作系统、屏幕分辨率、CPU 核心数、时区等
  - 提供 `user_agent()` 和 `x_amz_user_agent()` 方法构建请求头
  - 参考 CLIProxyAPIPlus 实现，降低被检测风险

- 新增精细化速率限制系统 (`src/kiro/rate_limiter.rs`)
  - 每日请求限制（默认 500 次/天）
  - 请求间隔控制（1-2 秒 + 30% 抖动）
  - 指数退避策略（30s → 5min，倍数 1.5）
  - 暂停检测（关键词匹配：suspended, banned, quota exceeded 等）

- 新增独立冷却管理模块 (`src/kiro/cooldown.rs`)
  - 分类冷却原因（7 种类型：速率限制、账户暂停、配额耗尽、Token 刷新失败等）
  - 差异化冷却时长：短冷却（1-5 分钟）vs 长冷却（1-24 小时）
  - 递增冷却机制（连续触发时延长冷却时间）
  - 自动清理过期冷却

- 新增后台 Token 刷新模块 (`src/kiro/background_refresh.rs`)
  - 独立后台任务定期检查即将过期的 Token
  - 支持批量并发刷新（信号量控制）
  - 可配置检查间隔、批处理大小、并发数
  - 优雅关闭机制

- `MultiTokenManager` 新增增强方法
  - `get_fingerprint()`: 获取凭据的设备指纹
  - `is_credential_available()`: 综合检查凭据可用性（未禁用、未冷却、未超速率限制）
  - `set_credential_cooldown()` / `clear_credential_cooldown()`: 冷却管理
  - `get_expiring_credential_ids()`: 获取即将过期的凭据列表
  - `start_background_refresh()`: 启动后台 Token 刷新任务
  - `refresh_token_for_credential()`: 带优雅降级的 Token 刷新
  - `record_api_success()` / `record_api_failure()`: 更新速率限制器状态
- `CredentialEntry` 结构体新增 `fingerprint` 字段，每个凭据独立生成设备指纹

- 修复 IDC 凭据 `fetch_profile_arn` 在某些 region 返回 `UnknownOperationException` 的问题
  - 新增 `ListAvailableCustomizations` API 作为 `ListProfiles` 的回退方案
  - 支持多 region 尝试：先尝试用户配置的 region，失败则回退到 `us-east-1`
  - 涉及文件：`src/kiro/token_manager.rs`

- 修复 `start_background_refresh` 后台刷新器生命周期问题（Codex Review P1）
  - 问题：`refresher` 作为局部变量在函数返回后被 drop，导致后台任务立即停止
  - 解决：方法现在返回 `Arc<BackgroundRefresher>`，调用方需保持引用以维持任务运行
  - 涉及文件：`src/kiro/token_manager.rs`

- 修复 `calculate_backoff` 退避时间可能超过配置上限的问题（Codex Review P2）
  - 问题：添加抖动后未再次进行上限约束，可能导致实际等待时间超过 `backoff_max_ms`
  - 解决：在添加抖动后再进行 `.min(max)` 约束
  - 涉及文件：`src/kiro/rate_limiter.rs`

- 改进 `persist_credentials` 并发写入安全性（Codex Review P1）
  - 问题：在锁外执行文件写入可能导致并发写入时旧快照覆盖新数据
  - 解决：在持有 entries 锁的情况下完成序列化，确保快照一致性
  - 涉及文件：`src/kiro/token_manager.rs`
- 修复 IDC 凭据返回 403 "The bearer token included in the request is invalid" 的问题
  - 根本原因：`profile_arn` 只从第一个凭据获取并存储在全局 `AppState` 中，当使用 IDC 凭据时，Bearer Token 来自 IDC 凭据，但 `profile_arn` 来自第一个凭据（可能是 Social 类型），导致 Token 和 profile_arn 不匹配
  - 解决方案 1：在 `call_api_with_retry` 中动态注入当前凭据的 `profile_arn`，确保 Token 和 profile_arn 始终匹配
  - 解决方案 2：IDC Token 刷新后自动调用 `ListProfiles` API 获取 `profileArn`（IDC 的 OIDC 刷新不返回此字段）
  - 新增 `inject_profile_arn()` 辅助方法，解析请求体 JSON 并覆盖 `profileArn` 字段
  - 新增 `fetch_profile_arn()` 方法，通过 CodeWhisperer ListProfiles API 获取 profileArn
  - 涉及文件：`src/kiro/provider.rs`, `src/kiro/token_manager.rs`
- 新增批量导入 token.json 功能
  - 后端：新增 `POST /api/admin/credentials/import-token-json` 端点
  - 支持解析官方 token.json 格式（含 `provider`、`refreshToken`、`clientId`、`clientSecret` 等字段）
  - 按 `provider` 字段自动映射 `authMethod`（BuilderId → idc, IdC → idc, Social → social）
  - 支持 dry-run 预览模式，返回详细的导入结果（成功/跳过/无效）
  - 通过 refreshToken 前缀匹配自动去重，避免重复导入
  - 前端：新增"导入 token.json"对话框组件
  - 支持拖放上传 JSON 文件或直接粘贴 JSON 内容
  - 三步流程：输入 → 预览 → 结果
  - 涉及文件：
    - `src/admin/types.rs`（新增 `TokenJsonItem`、`ImportTokenJsonRequest`、`ImportTokenJsonResponse` 等类型）
    - `src/admin/service.rs`（新增 `import_token_json()` 方法）
    - `src/admin/handlers.rs`（新增 `import_token_json` handler）
    - `src/admin/router.rs`（添加路由）
    - `src/kiro/token_manager.rs`（新增 `has_refresh_token_prefix()` 方法）
    - `admin-ui/src/types/api.ts`（新增导入相关类型）
    - `admin-ui/src/api/credentials.ts`（新增 `importTokenJson()` 函数）
    - `admin-ui/src/hooks/use-credentials.ts`（新增 `useImportTokenJson()` hook）
    - `admin-ui/src/components/import-token-json-dialog.tsx`（新建）
    - `admin-ui/src/components/dashboard.tsx`（添加导入按钮）

- 修复字符串切片在多字节字符中间切割导致 panic 的风险（DoS 漏洞）
  - `generate_fingerprint()` 和 `has_refresh_token_prefix()` 使用 `floor_char_boundary()` 安全截断
  - 涉及文件：`src/admin/service.rs`, `src/kiro/token_manager.rs`
- 修复日志截断在多字节字符中间切割导致 panic 的问题
  - `truncate_for_log()` 使用 `floor_char_boundary()` 安全截断 UTF-8 字符串
  - 删除 `stream.rs` 中冗余的 `find_char_boundary()` 函数，直接使用标准库方法
  - 涉及文件：`src/kiro/provider.rs`, `src/anthropic/stream.rs`
- 移除历史消息中孤立的 tool_use（无对应 tool_result）
  - Kiro API 要求 tool_use 必须有配对的 tool_result，否则返回 400 Bad Request
  - 新增 `remove_orphaned_tool_uses()` 函数清理孤立的 tool_use
  - 涉及文件：`src/anthropic/converter.rs`
  - 将 `interval()` 改为 `interval_at(Instant::now() + ping_period, ping_period)`
  - 现在首个 ping 会在 25 秒后触发，与 `/v1/messages` 行为一致
  - 涉及文件：`src/anthropic/handlers.rs`
- 修复 Clippy `collapsible_if` 警告
  - 使用 let-chains 语法合并嵌套 if 语句
  - 涉及文件：`src/anthropic/stream.rs`

- 增强 400 Bad Request 错误日志，记录完整请求信息
  - 移除请求体截断限制，记录完整的 `request_body`
  - 新增 `request_url` 和 `request_headers` 字段
  - 新增 `format_headers_for_log()` 辅助函数，对 Authorization 头进行脱敏处理
  - 删除不再使用的 `truncate_for_log()` 函数（YAGNI 原则）
  - 涉及文件：`src/kiro/provider.rs`
- 改进凭据选择算法：同优先级内实现负载均衡
  - 第一优先级：使用次数最少
  - 第二优先级：余额最多（使用次数相同时）
  - 第三优先级：轮询选择（使用次数和余额完全相同时，避免总选第一个）
  - 新增 `selection_rr` 原子计数器用于轮询抖动
  - 新增 `select_best_candidate_id()` 方法实现三级排序逻辑
  - 涉及文件：`src/kiro/token_manager.rs`

- 修复测试代码使用 `serde_json::json!` 构造 Tool 对象导致的类型不匹配问题
  - 改用 `Tool` 结构体直接构造，确保类型安全
  - 涉及文件：`src/anthropic/websearch.rs`
- 修复 `select_best_candidate_id()` 中 NaN 余额处理问题
  - 在评分阶段将 NaN/Infinity 余额归一化为 0.0
  - 避免 NaN 被 `total_cmp` 视为最大值导致错误的凭据选择
  - 避免 NaN 导致 `scored` 被完全过滤后除零 panic
  - 涉及文件：`src/kiro/token_manager.rs`
- 新增 `system` 字段格式兼容性支持（`src/anthropic/types.rs`）
  - 支持字符串格式：`"system": "You are a helpful assistant"`（new-api 等网关添加的系统提示词）
  - 支持数组格式：`"system": [{"type": "text", "text": "..."}]`（Claude Code 原生格式）
  - 自动将字符串格式转换为单元素数组，保持内部处理一致性
  - 新增 6 个单元测试验证格式兼容性
- 新增请求体大小限制：50MB（`DefaultBodyLimit::max(50 * 1024 * 1024)`）
  - 涉及文件：`src/anthropic/router.rs`
- 调整全局禁用恢复时间：`GLOBAL_DISABLE_RECOVERY_MINUTES` 从 10 分钟降至 5 分钟
  - 加快模型暂时不可用后的自动恢复速度
- 调整总重试次数硬上限：`MAX_TOTAL_RETRIES` 从 5 降至 3
  - 进一步减少无效重试开销，加快故障转移速度
- 余额初始化改为顺序查询，每次间隔 0.5 秒避免触发限流
  - 从并发查询改为顺序查询（`initialize_balances()`）
  - 移除 30 秒整体超时机制
  - 涉及文件：`src/kiro/token_manager.rs`

- 修复 assistant 消息仅包含 tool_use 时 content 为空导致 Kiro API 报错的问题
  - 当 text_content 为空且存在 tool_uses 时，使用 "OK" 作为占位符
  - 涉及文件：`src/anthropic/converter.rs`
- 修复 `MODEL_TEMPORARILY_UNAVAILABLE` 错误检测逻辑未实际调用的问题
  - 在 `call_mcp()` 和 `call_api()` 中添加错误检测和熔断触发逻辑
  - 移除 `report_model_unavailable()` 和 `disable_all_credentials()` 的 `#[allow(dead_code)]` 标记
  - 现在当检测到该错误时会正确触发全局熔断机制

- 新增 WebSearch 工具支持（`src/anthropic/websearch.rs`）
  - 实现 Anthropic WebSearch 请求到 Kiro MCP 的转换
  - 支持 SSE 流式响应，生成完整的搜索结果事件序列
  - 自动检测纯 WebSearch 请求（tools 仅包含 web_search）并路由到专用处理器
- 新增 MCP API 调用支持（`src/kiro/provider.rs`）
  - 新增 `call_mcp()` 方法，支持 WebSearch 等工具调用
  - 新增 `mcp_url()` 和 `build_mcp_headers()` 方法
  - 完整的重试和故障转移逻辑
- 新增凭据级 `region` 字段，用于 OIDC token 刷新时指定 endpoint 区域
  - 未配置时回退到 config.json 的全局 region
  - API 调用仍使用 config.json 的 region
- 新增凭据级 `machineId` 字段，支持每个凭据使用独立的机器码
  - 支持 64 字符十六进制和 UUID 格式（自动标准化）
  - 未配置时回退到 config.json 的 machineId，都未配置时由 refreshToken 派生
  - 启动时自动补全并持久化到配置文件
- 新增 GitHub Actions Docker 构建工作流（`.github/workflows/docker-build.yaml`）
  - 支持 linux/amd64 和 linux/arm64 双架构
  - 推送到 GitHub Container Registry
- 版本号升级至 2026.1.5
- TLS 库从 native-tls 切换至 rustls（reqwest 依赖调整）
- `authMethod` 自动推断：未指定时根据是否有 clientId/clientSecret 自动判断为 idc 或 social
- 移除 web_search/websearch 工具过滤（`is_unsupported_tool` 现在返回 false）

- 修复 machineId 格式兼容性问题，支持 UUID 格式自动转换为 64 字符十六进制
### Removed
- 移除 `current_id` 概念（后端和前端）
  - 后端：移除 `MultiTokenManager.current_id` 字段和相关方法（`switch_to_next`、`select_highest_priority`、`select_by_balance`、`credentials`）
  - 后端：移除 `ManagerSnapshot.current_id` 字段
  - 后端：移除 `CredentialStatusItem.is_current` 字段
  - 前端：移除 `CredentialsStatusResponse.currentId` 和 `CredentialStatusItem.isCurrent`
  - 原因：多用户并发访问时，"当前凭据"概念无意义，凭据选择由 `acquire_context_for_user()` 动态决定

- 新增启动时余额初始化功能
  - `initialize_balances()`: 启动时并发查询所有凭据余额并更新缓存
  - 整体超时 30 秒，避免阻塞启动流程
  - 初始化失败或超时时输出警告日志
- 改进凭据选择算法：从单一"使用次数最少"改为两级排序
  - 第一优先级：使用次数最少
  - 第二优先级：余额最多（使用次数相同时）
  - 未初始化余额的凭据会被降级处理，避免被优先选中
- 移除前端"当前活跃"凭据展示
  - 前端：移除凭据卡片的"当前"高亮和 Badge
  - 前端：移除 Dashboard 中的"当前活跃"统计卡片
  - 统计卡片布局从 3 列调整为 2 列

- 新增 `sensitive-logs` feature flag，显式启用才允许打印潜在敏感信息（仅用于排障）
  - 默认关闭：Kiro 请求体只输出长度，凭证只输出摘要信息
  - 启用方式：`cargo build --features sensitive-logs`

- 修复 SSE 流 ping 保活首次立即触发的问题
  - 使用 `interval_at(Instant::now() + ping_period, ping_period)` 延迟首次触发
  - 避免连接建立后立即发送无意义的 ping 事件
- 改进服务启动错误处理
  - 绑定监听地址失败时输出错误日志并退出（exit code 1）
  - HTTP 服务异常退出时输出错误日志并退出（exit code 1）

- 修复合并 upstream 后 `CredentialEntry` 结构体字段缺失导致的编译错误
  - 添加 `disable_reason: Option<DisableReason>` 字段（公共 API 展示用）
  - 添加 `auto_heal_reason: Option<AutoHealReason>` 字段（内部自愈逻辑用）
- 修复禁用原因字段不同步问题
  - `report_failure()`: 禁用时同步设置两个字段
  - `set_disabled()`: 启用/禁用时同步设置/清除两个字段
  - `reset_and_enable()`: 重置时同步清除两个字段
  - 自愈循环：重新启用凭据时同步清除 `disable_reason`
  - `mark_insufficient_balance()`: 清除 `auto_heal_reason` 防止被自愈循环错误恢复
- 重命名内部字段以提高可读性
  - `DisabledReason` → `AutoHealReason`（自愈原因枚举）
  - `disabled_reason` → `auto_heal_reason`（自愈原因字段）
- 日志中的 `user_id` 现在会进行掩码处理，保护用户隐私
  - 长度 > 25：保留前13后8字符（如 `user_f516339a***897ac7`）
  - 长度 13-25：保留前4后4字符
  - 长度 ≤ 12：完全掩码为 `***`

- 新增缓存余额查询 API（`GET /credentials/balances/cached`）
  - 后端：`CachedBalanceInfo` 结构体、`get_all_cached_balances()` 方法
  - 前端：凭据卡片直接显示缓存余额和更新时间
  - 30 秒自动轮询更新，缓存超过 1 分钟时点击强制刷新
- 新增 Bonus 用量包支持（`src/kiro/model/usage_limits.rs`）
  - 新增 `Bonus` 结构体，支持 GIFT 类型的额外用量包
  - 新增 `Bonus::is_active()` 方法，按状态/过期时间判断是否激活
  - `usage_limit()` 和 `current_usage()` 现在会合并基础额度、免费试用额度和所有激活的 bonuses
- 新增 Kiro Web Portal API 模块（`src/kiro/web_portal.rs`）
  - 支持 CBOR 协议与 app.kiro.dev 通信
  - 实现 `get_user_info()` 和 `get_user_usage_and_limits()` API
  - 新增 `aggregate_account_info()` 聚合账号信息（套餐/用量/邮箱等）
- Admin UI 前端增强
  - 新增数字格式化工具（`admin-ui/src/lib/format.ts`）：K/M/B 显示、Token 对格式化、过期时间格式化
  - 新增统计相关 API 和 Hooks：`getCredentialStats`, `resetCredentialStats`, `resetAllStats`
  - 新增账号信息 API：`getCredentialAccountInfo`, `useCredentialAccountInfo`
  - 扩展 `CredentialStatusItem` 添加统计字段（调用次数、Token 用量、最后调用时间等）
  - 新增完整的账号信息类型定义（`AccountAggregateInfo`, `CreditsUsageSummary` 等）
- 新增 `serde_cbor` 依赖用于 CBOR 编解码

- 修复手动查询余额后列表页面不显示缓存余额的问题
  - `get_balance()` 成功后调用 `update_balance_cache()` 更新缓存
  - 现在点击"查看余额"后，列表页面会正确显示缓存的余额值
- 修复关闭余额弹窗后卡片不更新缓存余额的问题
  - 弹窗关闭时调用 `queryClient.invalidateQueries({ queryKey: ['cached-balances'] })`
  - 确保卡片和弹窗使用的两个独立数据源保持同步
- 增强 Token 刷新日志，添加凭证 ID 追踪
  - 新增 `refresh_token_with_id()` 函数支持传入凭证 ID
  - 日志现在包含 `credential_id` 字段，便于多凭据环境下的问题排查
- 调整重试策略：单凭据最大重试次数 3→2，单请求最大重试次数 9→5
  - `MAX_RETRIES_PER_CREDENTIAL`: 3 → 2
  - `MAX_TOTAL_RETRIES`: 9 → 5
  - `MAX_FAILURES_PER_CREDENTIAL`: 3 → 2
  - 减少无效凭据的重试开销，加快故障转移速度

- 新增用户亲和性绑定功能：连续对话优先使用同一凭据（基于 `metadata.user_id`）
  - 新增 `src/kiro/affinity.rs` 模块，实现 `UserAffinityManager`
  - 新增 `acquire_context_for_user()` 方法支持亲和性查询
  - 亲和性绑定 TTL 为 30 分钟
- 新增余额感知故障转移：凭据失效时自动切换到余额最高的可用凭据
- 新增动态余额缓存 TTL 策略：
  - 高频渠道（10分钟内 ≥20 次调用）：10 分钟刷新
  - 低频渠道：30 分钟刷新
  - 低余额渠道（余额 < 1.0）：24 小时刷新
- 新增 `record_usage()` 方法自动记录凭据使用频率
- 新增负载均衡：无亲和性绑定时优先分配到使用频率最低的凭据
- 新增 `DisableReason` 枚举，区分凭据禁用原因（失败次数、余额不足、模型不可用、手动禁用）
- 成功请求时自动重置 `MODEL_TEMPORARILY_UNAVAILABLE` 计数器，避免跨时间累计触发
- 新增 `MODEL_TEMPORARILY_UNAVAILABLE` 错误检测和全局禁用机制
  - 当该 500 错误发生 2 次时，自动禁用所有凭据
  - 5 分钟后自动恢复（余额不足的凭据除外）
- `CredentialEntrySnapshot` 新增 `disable_reason` 字段，支持查询禁用原因
- 新增自动余额刷新：成功请求后自动在后台刷新余额缓存（基于动态 TTL 策略）
  - 新增 `spawn_balance_refresh()` 方法，使用 `tokio::spawn` 异步刷新
  - 新增 `should_refresh_balance()` 方法，根据 TTL 判断是否需要刷新