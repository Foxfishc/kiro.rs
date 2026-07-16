//! Admin API 类型定义

use serde::{Deserialize, Serialize};

// ============ 凭据状态 ============

/// 所有凭据状态响应
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CredentialsStatusResponse {
    /// 凭据总数
    pub total: usize,
    /// 可用凭据数量（未禁用）
    pub available: usize,
    /// 各凭据状态列表
    pub credentials: Vec<CredentialStatusItem>,
}

/// 单个凭据的状态信息
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CredentialStatusItem {
    /// 凭据唯一 ID
    pub id: u64,
    /// 优先级（数字越小优先级越高）
    pub priority: u32,
    /// 是否被禁用
    pub disabled: bool,
    /// 连续失败次数
    pub failure_count: u32,
    /// Token 刷新连续失败次数
    pub refresh_failure_count: u32,
    /// 禁用原因
    pub disabled_reason: Option<String>,
    /// Token 过期时间（RFC3339 格式）
    pub expires_at: Option<String>,
    /// 认证方式
    pub auth_method: Option<String>,
    /// 是否有 Profile ARN
    pub has_profile_arn: bool,
    /// refreshToken 的 SHA-256 哈希（用于前端重复检测）
    pub refresh_token_hash: Option<String>,
    /// 用户邮箱（用于前端显示）
    pub email: Option<String>,
    /// 已持久化的订阅等级（页面刷新后可直接展示）
    pub subscription_title: Option<String>,
    /// API 调用成功次数
    pub success_count: u64,
    /// 最后一次 API 调用时间（RFC3339 格式）
    pub last_used_at: Option<String>,
    /// 凭据级 Region（用于 Token 刷新）
    pub region: Option<String>,
    /// 凭据级 API Region（单独覆盖 API 请求）
    pub api_region: Option<String>,
    /// 凭据显式配置的 endpoint（None 表示回退到 defaultEndpoint）
    pub endpoint: Option<String>,
    /// 最终生效的 endpoint 名称
    pub effective_endpoint: String,
    /// Web Portal Idp 标识（默认推断为 Google）
    pub idp: Option<String>,
    /// 凭据级代理 URL（None 表示回退到全局代理；"direct" 表示显式直连）
    pub proxy_url: Option<String>,
    /// 凭据级代理认证用户名
    pub proxy_username: Option<String>,
    /// 是否设置了凭据级代理密码（不返回明文）
    pub has_proxy_password: bool,
    /// 最近一次已知的超额开关状态（None 表示未知）
    pub overage_enabled: Option<bool>,
    /// 是否正在执行后台开启超额任务
    pub overage_enabling: bool,
    /// 最近一次开启超额失败原因
    pub overage_last_error: Option<String>,
}

// ============ 操作请求 ============

/// 启用/禁用凭据请求
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetDisabledRequest {
    /// 是否禁用
    pub disabled: bool,
}

/// 修改优先级请求
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetPriorityRequest {
    /// 新优先级值
    pub priority: u32,
}

/// 修改 Region 请求
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetRegionRequest {
    /// 凭据级 Region（用于 Token 刷新），空字符串表示清除
    pub region: Option<String>,
    /// 凭据级 API Region（单独覆盖 API 请求），空字符串表示清除
    pub api_region: Option<String>,
}

/// 修改 endpoint 请求
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetEndpointRequest {
    /// endpoint 名称，空字符串或 null 表示回退到 defaultEndpoint
    pub endpoint: Option<String>,
}

/// 修改凭据级 Web Portal Idp 请求
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetIdpRequest {
    /// idp 名称（如 "Google"），空字符串或 null 表示清除并回退到默认
    pub idp: Option<String>,
}

// ============ Kiro 托管门户登录（SSO）============

/// 启动 Kiro SSO 登录请求（region 可选，默认 us-east-1）
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StartKiroSsoRequest {
    #[serde(default)]
    pub region: Option<String>,
}

/// 启动 AWS IAM Identity Center（Enterprise SSO）直连登录请求
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StartKiroIdcRequest {
    /// IAM Identity Center 的 Start URL（形如 https://d-xxxx.awsapps.com/start）
    pub start_url: String,
    /// 登录 region（可选，默认 us-east-1）
    #[serde(default)]
    pub region: Option<String>,
}

/// 启动 Kiro SSO 登录响应
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StartKiroSsoResponse {
    pub session_id: String,
    pub sign_in_url: String,
    /// 建议轮询间隔（秒）
    pub interval: u64,
}

/// 轮询 / 取消 Kiro SSO 登录请求
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KiroSsoSessionRequest {
    pub session_id: String,
}

/// 手动提交 IAM Identity Center 回调 URL 请求（无 SSH 隧道场景）
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubmitKiroIdcCallbackRequest {
    pub session_id: String,
    /// 浏览器地址栏里的完整回调 URL（含 code + state），也兼容只粘贴 query 串
    pub callback_url: String,
}

/// 轮询 Kiro SSO 登录响应
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PollKiroSsoResponse {
    pub success: bool,
    pub completed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub credential_id: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auth_method: Option<String>,
}

/// 修改凭据级代理请求
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetCredentialProxyRequest {
    /// 代理 URL（http/https/socks5），空字符串表示清除回退到全局；
    /// 特殊值 "direct" 表示显式直连。
    pub proxy_url: Option<String>,
    /// 代理用户名（可选）
    pub proxy_username: Option<String>,
    /// 代理密码（可选；不会被返回到响应）
    pub proxy_password: Option<String>,
}

/// 单凭据 overage 状态响应
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OverageStatusResponse {
    pub id: u64,
    pub enabled: Option<bool>,
    pub enabling: bool,
    pub last_error: Option<String>,
    pub has_profile_arn: bool,
    pub auth_method: Option<String>,
}

/// 添加凭据请求
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AddCredentialRequest {
    /// 刷新令牌（OAuth 凭据必填，API Key 凭据不需要）
    pub refresh_token: Option<String>,

    /// Kiro API Key（API Key 凭据必填）
    pub kiro_api_key: Option<String>,

    /// 认证方式（可选，默认 social）
    #[serde(default = "default_auth_method")]
    pub auth_method: String,

    /// OIDC Client ID（IdC 认证需要）
    pub client_id: Option<String>,

    /// OIDC Client Secret（IdC 认证需要）
    pub client_secret: Option<String>,

    /// 优先级（可选，默认 0）
    #[serde(default)]
    pub priority: u32,

    /// 凭据级 Region 配置（用于 Token 刷新）
    /// 未配置时回退到 config.json 的全局 region
    pub region: Option<String>,

    /// 凭据级 API Region（用于 API 调用）
    pub api_region: Option<String>,

    /// 凭据级 Machine ID（可选，64 位字符串）
    /// 未配置时回退到 config.json 的 machineId
    pub machine_id: Option<String>,

    /// 凭据级 endpoint（未配置时回退到 config.defaultEndpoint；当前已注册端点由服务端校验）
    pub endpoint: Option<String>,

    /// 用户邮箱（可选，用于前端显示）
    pub email: Option<String>,

    /// 凭据级代理 URL
    pub proxy_url: Option<String>,

    /// 凭据级代理用户名
    pub proxy_username: Option<String>,

    /// 凭据级代理密码
    pub proxy_password: Option<String>,
}

fn default_auth_method() -> String {
    "social".to_string()
}

/// 添加凭据成功响应
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AddCredentialResponse {
    pub success: bool,
    pub message: String,
    /// 新添加的凭据 ID
    pub credential_id: u64,
    /// 用户邮箱（如果获取成功）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
}

// ============ 余额查询 ============

/// 余额查询响应
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BalanceResponse {
    /// 凭据 ID
    pub id: u64,
    /// 订阅类型
    pub subscription_title: Option<String>,
    /// 当前使用量
    pub current_usage: f64,
    /// 使用限额
    pub usage_limit: f64,
    /// 剩余额度
    pub remaining: f64,
    /// 使用百分比
    pub usage_percentage: f64,
    /// 下次重置时间（Unix 时间戳）
    pub next_reset_at: Option<f64>,
    /// 当前查询到的超额状态
    pub overage_enabled: bool,
    /// 上游返回/兜底后的超额额度上限（未开启时为 0）
    pub overage_cap: f64,
}

/// 缓存余额信息
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CachedBalanceItem {
    /// 凭据 ID
    pub id: u64,
    /// 缓存的剩余额度
    pub remaining: f64,
    /// 使用限额
    pub usage_limit: f64,
    /// 使用百分比
    pub usage_percentage: f64,
    /// 订阅类型
    pub subscription_title: Option<String>,
    /// 缓存时间（Unix 毫秒时间戳）
    pub cached_at: u64,
    /// 缓存存活时间（秒），缓存过期时间 = cached_at + ttl_secs * 1000
    pub ttl_secs: u64,
    /// 缓存快照里的超额开关状态
    pub overage_enabled: bool,
    /// 缓存快照里的超额额度上限（未开启时为 0）
    pub overage_cap: f64,
}

/// 所有凭据的缓存余额响应
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CachedBalancesResponse {
    /// 各凭据的缓存余额列表
    pub balances: Vec<CachedBalanceItem>,
}

// ============ 负载均衡配置 ============

// ============ 全局代理配置 ============

/// 全局代理配置响应
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProxyConfigResponse {
    pub proxy_url: Option<String>,
    pub has_credentials: bool,
}

/// 更新全局代理配置请求
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateProxyConfigRequest {
    pub proxy_url: Option<String>,
    pub proxy_username: Option<String>,
    pub proxy_password: Option<String>,
}

// ============ 通用响应 ============

/// 操作成功响应
#[derive(Debug, Serialize)]
pub struct SuccessResponse {
    pub success: bool,
    pub message: String,
}

impl SuccessResponse {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            success: true,
            message: message.into(),
        }
    }
}

// ============ 批量导入 token.json ============

/// 账号管理器原生导出格式里的嵌套 credentials 块。
/// 例如 chaogei/Kiro-account-manager 导出的 `accounts[].credentials`。
#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct NestedCredentials {
    #[serde(alias = "access_token")]
    pub access_token: Option<String>,
    #[serde(alias = "refresh_token")]
    pub refresh_token: Option<String>,
    #[serde(alias = "client_id")]
    pub client_id: Option<String>,
    #[serde(alias = "client_secret")]
    pub client_secret: Option<String>,
    pub region: Option<String>,
    #[serde(alias = "api_region")]
    pub api_region: Option<String>,
    #[serde(alias = "auth_method")]
    pub auth_method: Option<String>,
    #[serde(alias = "profile_arn")]
    pub profile_arn: Option<String>,
    /// api_key 凭据（CLIProxyAPI 导出用 snake_case kiro_api_key）
    #[serde(alias = "kiro_api_key")]
    pub kiro_api_key: Option<String>,
    /// external_idp（Azure AD）刷新所需
    #[serde(alias = "token_endpoint")]
    pub token_endpoint: Option<String>,
    #[serde(alias = "issuer_url")]
    pub issuer_url: Option<String>,
    pub scopes: Option<String>,
    pub provider: Option<String>,
}

/// 导入项：同时兼容两种 JSON 形态。
///
/// 1. **扁平格式**（kiro-rs 原生）：`refreshToken`/`clientId`/... 都在顶层。
/// 2. **账号管理器原生嵌套格式**：refresh/client 等在 `credentials` 子对象里，
///    `machineId`/`email`/`idp` 在顶层（见用户导出的 kiro-accounts-*.json）。
///
/// 解析后统一通过下面的访问器取值：顶层扁平字段优先，缺失时回退到嵌套 `credentials`。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenJsonItem {
    pub provider: Option<String>,
    #[serde(alias = "refresh_token")]
    pub refresh_token: Option<String>,
    /// api_key 凭据（顶层扁平；CLIProxyAPI snake_case 用 kiro_api_key）
    #[serde(alias = "kiro_api_key")]
    pub kiro_api_key: Option<String>,
    #[serde(alias = "client_id")]
    pub client_id: Option<String>,
    #[serde(alias = "client_secret")]
    pub client_secret: Option<String>,
    #[serde(alias = "auth_method")]
    pub auth_method: Option<String>,
    #[serde(default)]
    pub priority: u32,
    pub region: Option<String>,
    pub api_region: Option<String>,
    pub machine_id: Option<String>,
    /// 顶层 profileArn（Kiro 企业 harvest bundle 带真实 ARN，须原样保留避免再次探测）
    pub profile_arn: Option<String>,
    /// external_idp（Azure AD）刷新所需字段（顶层扁平）
    pub token_endpoint: Option<String>,
    pub issuer_url: Option<String>,
    pub scopes: Option<String>,
    /// 顶层 email（账号管理器原生格式带）
    pub email: Option<String>,
    /// 顶层 idp（如 "BuilderId"），用于回退推断 authMethod
    pub idp: Option<String>,
    /// 嵌套凭据块（账号管理器原生格式）
    pub credentials: Option<NestedCredentials>,
}

impl TokenJsonItem {
    fn nested(&self) -> Option<&NestedCredentials> {
        self.credentials.as_ref()
    }

    /// refreshToken：顶层优先，回退嵌套。
    pub fn resolved_refresh_token(&self) -> Option<String> {
        self.refresh_token
            .clone()
            .or_else(|| self.nested().and_then(|c| c.refresh_token.clone()))
            .filter(|s| !s.is_empty())
    }

    /// kiroApiKey：顶层优先，回退嵌套（api_key 账号凭据）。
    pub fn resolved_kiro_api_key(&self) -> Option<String> {
        self.kiro_api_key
            .clone()
            .or_else(|| self.nested().and_then(|c| c.kiro_api_key.clone()))
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }

    pub fn resolved_client_id(&self) -> Option<String> {
        self.client_id
            .clone()
            .or_else(|| self.nested().and_then(|c| c.client_id.clone()))
            .filter(|s| !s.is_empty())
    }

    pub fn resolved_client_secret(&self) -> Option<String> {
        self.client_secret
            .clone()
            .or_else(|| self.nested().and_then(|c| c.client_secret.clone()))
            .filter(|s| !s.is_empty())
    }

    pub fn resolved_region(&self) -> Option<String> {
        self.region
            .clone()
            .or_else(|| self.nested().and_then(|c| c.region.clone()))
            .filter(|s| !s.is_empty())
    }

    pub fn resolved_auth_method(&self) -> Option<String> {
        self.auth_method
            .clone()
            .or_else(|| self.nested().and_then(|c| c.auth_method.clone()))
            .filter(|s| !s.is_empty())
    }

    pub fn resolved_profile_arn(&self) -> Option<String> {
        self.profile_arn
            .clone()
            .or_else(|| self.nested().and_then(|c| c.profile_arn.clone()))
            .filter(|s| !s.is_empty())
    }

    /// apiRegion：顶层优先（harvest bundle 里 profile 所在区可能不同于 auth region）
    pub fn resolved_api_region(&self) -> Option<String> {
        self.api_region
            .clone()
            .or_else(|| self.nested().and_then(|c| c.api_region.clone()))
            .filter(|s| !s.is_empty())
    }

    /// external_idp 刷新所需：tokenEndpoint（顶层优先，回退嵌套）
    pub fn resolved_token_endpoint(&self) -> Option<String> {
        self.token_endpoint
            .clone()
            .or_else(|| self.nested().and_then(|c| c.token_endpoint.clone()))
            .filter(|s| !s.is_empty())
    }

    pub fn resolved_issuer_url(&self) -> Option<String> {
        self.issuer_url
            .clone()
            .or_else(|| self.nested().and_then(|c| c.issuer_url.clone()))
            .filter(|s| !s.is_empty())
    }

    pub fn resolved_scopes(&self) -> Option<String> {
        self.scopes
            .clone()
            .or_else(|| self.nested().and_then(|c| c.scopes.clone()))
            .filter(|s| !s.is_empty())
    }

    pub fn resolved_provider(&self) -> Option<String> {
        self.provider
            .clone()
            .or_else(|| self.nested().and_then(|c| c.provider.clone()))
            .filter(|s| !s.is_empty())
    }

    pub fn resolved_machine_id(&self) -> Option<String> {
        self.machine_id.clone().filter(|s| !s.is_empty())
    }

    pub fn resolved_email(&self) -> Option<String> {
        self.email.clone().filter(|s| !s.is_empty())
    }

    /// 回退推断用的 provider/idp 提示（authMethod/provider 都缺时用 idp）。
    pub fn idp_hint(&self) -> Option<&str> {
        self.idp.as_deref()
    }
}

/// 批量导出凭据请求
#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ExportCredentialsRequest {
    /// 要导出的凭据 id 列表；为空/省略则导出全部
    #[serde(default)]
    pub ids: Vec<u64>,
}

/// 批量导入请求
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportTokenJsonRequest {
    #[serde(default = "default_dry_run")]
    pub dry_run: bool,
    pub items: ImportItems,
}

fn default_dry_run() -> bool {
    true
}

/// 导入项（支持单个或数组）
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum ImportItems {
    Single(TokenJsonItem),
    Multiple(Vec<TokenJsonItem>),
}

impl ImportItems {
    pub fn into_vec(self) -> Vec<TokenJsonItem> {
        match self {
            ImportItems::Single(item) => vec![item],
            ImportItems::Multiple(items) => items,
        }
    }
}

/// 批量导入响应
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportTokenJsonResponse {
    pub summary: ImportSummary,
    pub items: Vec<ImportItemResult>,
}

/// 导入汇总
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportSummary {
    pub parsed: usize,
    pub added: usize,
    pub skipped: usize,
    pub invalid: usize,
}

/// 单项导入结果
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportItemResult {
    pub index: usize,
    pub fingerprint: String,
    pub action: ImportAction,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub credential_id: Option<u64>,
}

/// 导入动作
#[derive(Debug, Serialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ImportAction {
    Added,
    Skipped,
    Invalid,
}

/// 错误响应
#[derive(Debug, Serialize)]
pub struct AdminErrorResponse {
    pub error: AdminError,
}

#[derive(Debug, Serialize)]
pub struct AdminError {
    #[serde(rename = "type")]
    pub error_type: String,
    pub message: String,
}

impl AdminErrorResponse {
    pub fn new(error_type: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            error: AdminError {
                error_type: error_type.into(),
                message: message.into(),
            },
        }
    }

    pub fn invalid_request(message: impl Into<String>) -> Self {
        Self::new("invalid_request", message)
    }

    pub fn authentication_error() -> Self {
        Self::new("authentication_error", "Invalid or missing admin API key")
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new("not_found", message)
    }

    pub fn api_error(message: impl Into<String>) -> Self {
        Self::new("api_error", message)
    }

    pub fn internal_error(message: impl Into<String>) -> Self {
        Self::new("internal_error", message)
    }
}

// ============ 全局配置 ============

/// 全局配置响应
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GlobalConfigResponse {
    /// AWS Region
    pub region: String,
    /// 单凭据目标请求速率（RPM），None 表示无限制
    pub credential_rpm: Option<u32>,
    /// Prompt Cache TTL（秒）
    pub prompt_cache_ttl_seconds: u64,
    /// 是否启用本地 Prompt Cache usage 记账
    pub prompt_cache_accounting_enabled: bool,
    /// 默认端点名称（凭据未显式指定 endpoint 时使用）
    pub default_endpoint: String,
    /// 压缩配置
    pub compression: CompressionConfigResponse,
}

/// 压缩配置响应
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CompressionConfigResponse {
    pub enabled: bool,
    pub whitespace_compression: bool,
    pub thinking_strategy: String,
    pub tool_result_max_chars: usize,
    pub tool_result_head_lines: usize,
    pub tool_result_tail_lines: usize,
    pub tool_use_input_max_chars: usize,
    pub tool_description_max_chars: usize,
    pub max_history_turns: usize,
    pub max_history_chars: usize,
    pub max_request_body_bytes: usize,
}

/// 更新全局配置请求
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateGlobalConfigRequest {
    /// AWS Region（可选）
    pub region: Option<String>,
    /// 单凭据目标请求速率（RPM，可选）
    pub credential_rpm: Option<Option<u32>>,
    /// Prompt Cache TTL（秒，可选，仅支持 300 或 3600）
    pub prompt_cache_ttl_seconds: Option<u64>,
    /// 是否启用本地 Prompt Cache usage 记账（可选）
    pub prompt_cache_accounting_enabled: Option<bool>,
    /// 默认端点名称（可选）
    pub default_endpoint: Option<String>,
    /// 压缩配置（可选）
    pub compression: Option<UpdateCompressionConfigRequest>,
}

/// 更新压缩配置请求
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateCompressionConfigRequest {
    pub enabled: Option<bool>,
    pub whitespace_compression: Option<bool>,
    pub thinking_strategy: Option<String>,
    pub tool_result_max_chars: Option<usize>,
    pub tool_result_head_lines: Option<usize>,
    pub tool_result_tail_lines: Option<usize>,
    pub tool_use_input_max_chars: Option<usize>,
    pub tool_description_max_chars: Option<usize>,
    pub max_history_turns: Option<usize>,
    pub max_history_chars: Option<usize>,
    pub max_request_body_bytes: Option<usize>,
}
