//! Token 管理模块
//!
//! 负责 Token 过期检测和刷新，支持 Social 和 IdC 认证方式
//! 支持单凭据 (TokenManager) 和多凭据 (MultiTokenManager) 管理
//!
//! ## 增强特性
//!
//! - **多维度设备指纹**: 每个凭据生成独立的设备指纹，模拟真实客户端
//! - **后台 Token 刷新**: 定期检查并预刷新即将过期的 Token
//! - **精细化速率限制**: 每日请求限制、请求间隔控制、指数退避
//! - **冷却管理**: 分类管理不同原因的冷却状态
//! - **优雅降级**: Token 刷新失败时使用现有 Token

use anyhow::bail;
use chrono::{DateTime, Duration, Utc};
use parking_lot::{Mutex, RwLock};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::time::{Duration as StdDuration, Instant};
use tokio::sync::Mutex as TokioMutex;

use crate::common::utf8::floor_char_boundary;
use crate::http_client::{ProxyConfig, build_client};
use crate::kiro::affinity::UserAffinityManager;
use crate::kiro::background_refresh::{
    BackgroundRefreshConfig, BackgroundRefresher, RefreshResult,
};
use crate::kiro::cooldown::{CooldownManager, CooldownReason};
use crate::kiro::endpoint::{
    CLI_ENDPOINT_NAME, CliEndpoint, IDE_ENDPOINT_NAME, IdeEndpoint, KiroEndpoint, RequestContext,
};
use crate::kiro::fingerprint::Fingerprint;
use crate::kiro::machine_id;
use crate::kiro::model::credentials::KiroCredentials;
use crate::kiro::model::token_refresh::{
    ExternalIdpTokenResponse, IdcRefreshRequest, IdcRefreshResponse, RefreshRequest,
    RefreshResponse,
};
use crate::kiro::model::usage_limits::{DEFAULT_OVERAGE_CAP, UsageLimitsResponse};
use crate::kiro::rate_limiter::{RateLimitConfig, RateLimiter};
use crate::model::config::Config;

/// 对 user_id 进行掩码处理，保护隐私
fn mask_user_id(user_id: Option<&str>) -> String {
    match user_id {
        Some(id) => {
            let len = id.len();
            if len > 12 {
                format!("{}***{}", &id[..4], &id[len - 4..])
            } else {
                "***".to_string()
            }
        }
        None => "None".to_string(),
    }
}

/// Token 管理器
///
/// 负责管理凭据和 Token 的自动刷新
#[allow(dead_code)]
pub struct TokenManager {
    config: Config,
    credentials: KiroCredentials,
    proxy: Option<ProxyConfig>,
}

#[allow(dead_code)]
impl TokenManager {
    /// 创建新的 TokenManager 实例
    pub fn new(config: Config, credentials: KiroCredentials, proxy: Option<ProxyConfig>) -> Self {
        Self {
            config,
            credentials,
            proxy,
        }
    }

    /// 获取凭据的引用
    pub fn credentials(&self) -> &KiroCredentials {
        &self.credentials
    }

    /// 获取配置的引用
    pub fn config(&self) -> &Config {
        &self.config
    }

    /// 确保获取有效的访问 Token
    ///
    /// 如果 Token 过期或即将过期，会自动刷新
    pub async fn ensure_valid_token(&mut self) -> anyhow::Result<String> {
        let token_missing_or_truncated = self
            .credentials
            .access_token
            .as_deref()
            .is_none_or(|t| t.trim().is_empty() || t.ends_with("...") || t.contains("..."));

        if token_missing_or_truncated
            || is_token_expired(&self.credentials)
            || is_token_expiring_soon(&self.credentials)
        {
            self.credentials =
                refresh_token(&self.credentials, &self.config, self.proxy.as_ref()).await?;

            // 刷新后再次检查 token 时间有效性
            if is_token_expired(&self.credentials) {
                anyhow::bail!("刷新后的 Token 仍然无效或已过期");
            }
        }

        self.credentials
            .access_token
            .clone()
            .ok_or_else(|| anyhow::anyhow!("没有可用的 accessToken"))
    }

    /// 获取使用额度信息
    ///
    /// 调用 getUsageLimits API 查询当前账户的使用额度
    pub async fn get_usage_limits(&mut self) -> anyhow::Result<UsageLimitsResponse> {
        let token = self.ensure_valid_token().await?;
        get_usage_limits(&self.credentials, &self.config, &token, self.proxy.as_ref()).await
    }
}

/// 检查 Token 是否在指定时间内过期
pub(crate) fn is_token_expiring_within(
    credentials: &KiroCredentials,
    minutes: i64,
) -> Option<bool> {
    credentials
        .expires_at
        .as_ref()
        .and_then(|expires_at| DateTime::parse_from_rfc3339(expires_at).ok())
        .map(|expires| expires <= Utc::now() + Duration::minutes(minutes))
}

/// 检查 Token 是否已过期（提前 5 分钟判断）
pub(crate) fn is_token_expired(credentials: &KiroCredentials) -> bool {
    if credentials.is_api_key_credential() {
        return false;
    }
    is_token_expiring_within(credentials, 5).unwrap_or(true)
}

/// 检查 Token 是否即将过期（10分钟内）
pub(crate) fn is_token_expiring_soon(credentials: &KiroCredentials) -> bool {
    if credentials.is_api_key_credential() {
        return false;
    }
    is_token_expiring_within(credentials, 10).unwrap_or(false)
}

fn sha256_hex(input: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    let result = hasher.finalize();
    format!("{:x}", result)
}

fn credential_secret_hash(credentials: &KiroCredentials) -> Option<String> {
    credentials
        .kiro_api_key
        .as_deref()
        .map(sha256_hex)
        .or_else(|| credentials.refresh_token.as_deref().map(sha256_hex))
}

fn is_invalid_grant_error(err: &anyhow::Error) -> bool {
    err.to_string().contains("invalid_grant")
}

fn is_temporarily_suspended_error(err: &anyhow::Error) -> bool {
    err.to_string().contains("TEMPORARILY_SUSPENDED")
}

/// 验证凭据的基本有效性
pub(crate) fn validate_credential_secret(credentials: &KiroCredentials) -> anyhow::Result<()> {
    if credentials.is_api_key_credential() {
        let api_key = credentials
            .kiro_api_key
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("缺少 kiroApiKey"))?;

        if api_key.trim().is_empty() {
            bail!("kiroApiKey 为空");
        }

        return Ok(());
    }

    validate_refresh_token(credentials)
}

/// 验证 refreshToken 的基本有效性
pub(crate) fn validate_refresh_token(credentials: &KiroCredentials) -> anyhow::Result<()> {
    let refresh_token = credentials
        .refresh_token
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("缺少 refreshToken"))?;

    if refresh_token.is_empty() {
        bail!("refreshToken 为空");
    }

    if refresh_token.len() < 100 || refresh_token.ends_with("...") || refresh_token.contains("...")
    {
        bail!(
            "refreshToken 已被截断（长度: {} 字符）。\n\
             这通常是 Kiro IDE 为了防止凭证被第三方工具使用而故意截断的。",
            refresh_token.len()
        );
    }

    Ok(())
}

/// 刷新 Token
pub(crate) async fn refresh_token(
    credentials: &KiroCredentials,
    config: &Config,
    proxy: Option<&ProxyConfig>,
) -> anyhow::Result<KiroCredentials> {
    // 使用凭据自身的 ID（如果有）
    let id = credentials.id.unwrap_or(0);
    refresh_token_with_id(credentials, config, proxy, id).await
}

/// 刷新 Token（带凭证 ID）
pub(crate) async fn refresh_token_with_id(
    credentials: &KiroCredentials,
    config: &Config,
    proxy: Option<&ProxyConfig>,
    _id: u64,
) -> anyhow::Result<KiroCredentials> {
    validate_refresh_token(credentials)?;

    // 根据 auth_method 选择刷新方式
    // 如果未指定 auth_method，根据是否有 clientId/clientSecret 自动判断
    let auth_method = credentials.auth_method.as_deref().unwrap_or_else(|| {
        if credentials.client_id.is_some() && credentials.client_secret.is_some() {
            "idc"
        } else {
            "social"
        }
    });

    if auth_method.eq_ignore_ascii_case("external_idp") {
        refresh_external_idp_token(credentials, config, proxy).await
    } else if auth_method.eq_ignore_ascii_case("idc")
        || auth_method.eq_ignore_ascii_case("builder-id")
        || auth_method.eq_ignore_ascii_case("iam")
    {
        refresh_idc_token(credentials, config, proxy).await
    } else {
        refresh_social_token(credentials, config, proxy).await
    }
}

/// 刷新外部 IdP（企业 SSO，如 Azure AD）Token
///
/// 通过 IdP 的 Token 端点使用 OAuth2 refresh_token 授权（public client，无 client secret）
/// 续期。原始 scopes 中的 offline_access 才能拿到 refresh token。IdP 不返回 profileArn
/// （由 ListAvailableProfiles 用 EXTERNAL_IDP token 类型单独解析），因此保持 profile_arn 不变。
///
/// Azure AD 会轮换 refresh token；有些 IdP 刷新时不返回新的 refresh token，
/// 这种情况下保留原 refresh token。
async fn refresh_external_idp_token(
    credentials: &KiroCredentials,
    config: &Config,
    proxy: Option<&ProxyConfig>,
) -> anyhow::Result<KiroCredentials> {
    tracing::info!("正在刷新外部 IdP (企业 SSO) Token...");

    let refresh_token = credentials.refresh_token.as_ref().unwrap();
    let client_id = credentials
        .client_id
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| anyhow::anyhow!("外部 IdP 刷新需要 clientId"))?;
    let token_endpoint = credentials
        .token_endpoint
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| anyhow::anyhow!("外部 IdP 刷新需要 tokenEndpoint"))?;

    // SSRF 防护：刷新时携带 refresh token，token 端点必须通过 IdP 主机白名单校验，
    // 防止把 refresh token POST 到内网或攻击者控制的主机。
    crate::kiro::sso::validate_external_idp_endpoint(token_endpoint)?;

    let client = build_client(proxy, 60, config.tls_backend)?;

    let mut form: Vec<(&str, String)> = vec![
        ("client_id", client_id.to_string()),
        ("grant_type", "refresh_token".to_string()),
        ("refresh_token", refresh_token.to_string()),
    ];
    if let Some(scopes) = credentials.scopes.as_deref().filter(|s| !s.trim().is_empty()) {
        form.push(("scope", scopes.to_string()));
    }

    let response = client
        .post(token_endpoint)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .header("Accept", "application/json")
        .form(&form)
        .send()
        .await?;

    let status = response.status();
    let body_text = response.text().await.unwrap_or_default();
    let data: ExternalIdpTokenResponse = serde_json::from_str(&body_text).unwrap_or(
        ExternalIdpTokenResponse {
            access_token: None,
            refresh_token: None,
            expires_in: None,
            error: None,
            error_description: None,
        },
    );

    let access_token = match data.access_token {
        Some(t) if !t.is_empty() && status.is_success() => t,
        _ => {
            if let Some(err) = data.error {
                bail!(
                    "外部 IdP Token 刷新失败 (status {}): {}: {}",
                    status,
                    err,
                    data.error_description.unwrap_or_default()
                );
            }
            bail!("外部 IdP Token 刷新失败 (status {}): {}", status, body_text);
        }
    };

    let mut new_credentials = credentials.clone();
    new_credentials.access_token = Some(access_token);

    // 部分 IdP 刷新时返回新的 refresh token，没有返回则保留原值
    if let Some(new_refresh_token) = data.refresh_token.filter(|t| !t.is_empty()) {
        new_credentials.refresh_token = Some(new_refresh_token);
    }

    if let Some(expires_in) = data.expires_in {
        let expires_at = Utc::now() + Duration::seconds(expires_in);
        new_credentials.expires_at = Some(expires_at.to_rfc3339());
        tracing::info!(expires_in = %expires_in, "外部 IdP Token 刷新成功");
    } else {
        tracing::info!("外部 IdP Token 刷新成功（无过期时间）");
    }

    Ok(new_credentials)
}

/// 刷新 Social Token
async fn refresh_social_token(
    credentials: &KiroCredentials,
    config: &Config,
    proxy: Option<&ProxyConfig>,
) -> anyhow::Result<KiroCredentials> {
    tracing::info!("正在刷新 Social Token...");

    let refresh_token = credentials.refresh_token.as_ref().unwrap();
    // 优先使用凭据级 region，未配置或为空时回退到 config.region
    let region = credentials
        .region
        .as_ref()
        .filter(|r| !r.trim().is_empty())
        .unwrap_or(&config.region);

    let refresh_url = format!("https://prod.{}.auth.desktop.kiro.dev/refreshToken", region);
    let refresh_domain = format!("prod.{}.auth.desktop.kiro.dev", region);
    let machine_id = machine_id::generate_from_credentials(credentials, config)
        .ok_or_else(|| anyhow::anyhow!("无法生成 machineId"))?;
    let kiro_version = &config.kiro_version;

    let client = build_client(proxy, 60, config.tls_backend)?;
    let body = RefreshRequest {
        refresh_token: refresh_token.to_string(),
    };

    let response = client
        .post(&refresh_url)
        .header("Accept", "application/json, text/plain, */*")
        .header("Content-Type", "application/json")
        .header(
            "User-Agent",
            format!("KiroIDE-{}-{}", kiro_version, machine_id),
        )
        .header("Accept-Encoding", "gzip, compress, deflate, br")
        .header("host", &refresh_domain)
        .header("Connection", "close")
        .json(&body)
        .send()
        .await?;

    let status = response.status();
    if !status.is_success() {
        let body_text = response.text().await.unwrap_or_default();
        let error_msg = match status.as_u16() {
            401 => "OAuth 凭证已过期或无效，需要重新认证",
            403 => "权限不足，无法刷新 Token",
            429 => "请求过于频繁，已被限流",
            500..=599 => "服务器错误，AWS OAuth 服务暂时不可用",
            _ => "Token 刷新失败",
        };
        bail!("{}: {} {}", error_msg, status, body_text);
    }

    let data: RefreshResponse = response.json().await?;

    let mut new_credentials = credentials.clone();
    new_credentials.access_token = Some(data.access_token);

    if let Some(new_refresh_token) = data.refresh_token {
        new_credentials.refresh_token = Some(new_refresh_token);
    }

    if let Some(profile_arn) = data.profile_arn {
        new_credentials.profile_arn = Some(profile_arn);
    }

    if let Some(expires_in) = data.expires_in {
        let expires_at = Utc::now() + Duration::seconds(expires_in);
        new_credentials.expires_at = Some(expires_at.to_rfc3339());
        tracing::info!(expires_in = %expires_in, "Social Token 刷新成功");
    } else {
        tracing::info!("Social Token 刷新成功（无过期时间）");
    }

    Ok(new_credentials)
}

/// IdC Token 刷新所需的 x-amz-user-agent header 前缀
const IDC_AMZ_USER_AGENT_PREFIX: &str = "aws-sdk-js/3.980.0";

fn build_idc_refresh_user_agents(config: &Config) -> (String, String) {
    let os_name = &config.system_version;
    let node_version = &config.node_version;

    let x_amz_user_agent = format!("{} KiroIDE", IDC_AMZ_USER_AGENT_PREFIX);
    let user_agent = format!(
        "{} ua/2.1 os/{} lang/js md/nodejs#{} api/sso-oidc#3.980.0 m/E KiroIDE",
        IDC_AMZ_USER_AGENT_PREFIX, os_name, node_version
    );

    (x_amz_user_agent, user_agent)
}

/// 刷新 IdC Token (AWS SSO OIDC)
async fn refresh_idc_token(
    credentials: &KiroCredentials,
    config: &Config,
    proxy: Option<&ProxyConfig>,
) -> anyhow::Result<KiroCredentials> {
    tracing::info!("正在刷新 IdC Token...");

    let refresh_token = credentials.refresh_token.as_ref().unwrap();
    let client_id = credentials
        .client_id
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("IdC 刷新需要 clientId"))?;
    let client_secret = credentials
        .client_secret
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("IdC 刷新需要 clientSecret"))?;

    // 优先级：凭据.auth_region > 凭据.region > config.auth_region > config.region
    let region = credentials.effective_auth_region(config);
    let refresh_url = format!("https://oidc.{}.amazonaws.com/token", region);
    let (x_amz_user_agent, user_agent) = build_idc_refresh_user_agents(config);

    let client = build_client(proxy, 60, config.tls_backend)?;
    let body = IdcRefreshRequest {
        client_id: client_id.to_string(),
        client_secret: client_secret.to_string(),
        refresh_token: refresh_token.to_string(),
        grant_type: "refresh_token".to_string(),
    };

    let response = client
        .post(&refresh_url)
        .header("content-type", "application/json")
        .header("x-amz-user-agent", &x_amz_user_agent)
        .header("user-agent", &user_agent)
        .header("host", format!("oidc.{}.amazonaws.com", region))
        .header("amz-sdk-invocation-id", uuid::Uuid::new_v4().to_string())
        .header("amz-sdk-request", "attempt=1; max=4")
        .header("Connection", "close")
        .json(&body)
        .send()
        .await?;

    let status = response.status();
    if !status.is_success() {
        let body_text = response.text().await.unwrap_or_default();
        let error_msg = match status.as_u16() {
            401 => "IdC 凭证已过期或无效，需要重新认证",
            403 => "权限不足，无法刷新 Token",
            429 => "请求过于频繁，已被限流",
            500..=599 => "服务器错误，AWS OIDC 服务暂时不可用",
            _ => "IdC Token 刷新失败",
        };
        bail!("{}: {} {}", error_msg, status, body_text);
    }

    let data: IdcRefreshResponse = response.json().await?;

    let mut new_credentials = credentials.clone();
    new_credentials.access_token = Some(data.access_token);

    if let Some(new_refresh_token) = data.refresh_token {
        new_credentials.refresh_token = Some(new_refresh_token);
    }

    if let Some(expires_in) = data.expires_in {
        let expires_at = Utc::now() + Duration::seconds(expires_in);
        new_credentials.expires_at = Some(expires_at.to_rfc3339());
        tracing::info!(expires_in = %expires_in, "IdC Token 刷新成功");
    } else {
        tracing::info!("IdC Token 刷新成功（无过期时间）");
    }

    // 注意：IDC 凭据（auth_method = "idc" 或 "builder-id"）不需要 profileArn
    // 参考 CLIProxyAPIPlus：AWS SSO OIDC 用户发送 profileArn 反而会导致 403 错误
    // 因此这里不再尝试获取 profileArn，保持为 None 即可

    Ok(new_credentials)
}

fn endpoint_for_credentials(
    credentials: &KiroCredentials,
    config: &Config,
) -> anyhow::Result<Box<dyn KiroEndpoint>> {
    match credentials.effective_endpoint_name(Some(&config.default_endpoint)) {
        IDE_ENDPOINT_NAME => Ok(Box::new(IdeEndpoint::new())),
        CLI_ENDPOINT_NAME => Ok(Box::new(CliEndpoint::new())),
        name => bail!("未知 endpoint: {}", name),
    }
}

/// 获取使用额度信息
pub(crate) async fn get_usage_limits(
    credentials: &KiroCredentials,
    config: &Config,
    token: &str,
    proxy: Option<&ProxyConfig>,
) -> anyhow::Result<UsageLimitsResponse> {
    tracing::debug!(
        endpoint = %credentials.effective_endpoint_name(Some(&config.default_endpoint)),
        "正在获取使用额度信息..."
    );

    let machine_id = machine_id::generate_from_credentials(credentials, config)
        .ok_or_else(|| anyhow::anyhow!("无法生成 machineId"))?;
    let endpoint = endpoint_for_credentials(credentials, config)?;
    let ctx = RequestContext {
        credentials,
        token,
        machine_id: &machine_id,
        config,
    };
    let usage = endpoint.usage_request_parts(&ctx)?;

    let client = build_client(proxy, 60, config.tls_backend)?;
    let mut request = client.get(&usage.url);
    for (name, value) in usage.headers {
        request = request.header(name, value);
    }

    let response = request.send().await?;

    let status = response.status();
    if !status.is_success() {
        let body_text = response.text().await.unwrap_or_default();
        let error_msg = match status.as_u16() {
            401 => "认证失败，Token 无效或已过期",
            403 => "权限不足，无法获取使用额度",
            429 => "请求过于频繁，已被限流",
            500..=599 => "服务器错误，AWS 服务暂时不可用",
            _ => "获取使用额度失败",
        };
        bail!("{}: {} {}", error_msg, status, body_text);
    }

    let body_text = response.text().await?;
    let data: UsageLimitsResponse = serde_json::from_str(&body_text).map_err(|e| {
        tracing::error!(
            "getUsageLimits JSON 解析失败: {}，原始响应: {}",
            e,
            body_text
        );
        anyhow::anyhow!("JSON 解析失败: {}", e)
    })?;
    Ok(data)
}

// ============================================================================
// IdC / Enterprise profileArn 解析（对齐 chaogei/Kiro-account-manager 参考实现）
//
// Enterprise（IAM Identity Center）账号签发的 token 需要携带一个真实的
// CodeWhisperer profileArn 才能调用数据面 / 额度端点，否则会返回
// 403 "User is not authorized to make this call."。官方 Kiro IDE 在认证后
// 通过 CodeWhisperer Runtime 的 ListAvailableProfiles 拿到可用 profile 列表，
// 用户选择后存储 ARN；反代自动取第一个 profile。
//
// 参考实现：src/main/proxy/kiroApi.ts fetchEnterpriseProfileArn()
//          src/main/kiroAuthSync.ts getEnterpriseFallbackArn()
// ============================================================================

/// IdC / Enterprise profileArn 解析结果（三态）。
///
/// 关键教训：早期实现对「空 profile 列表」也套用参考项目的兜底 ARN
/// （账号 610548660232），但那个 ARN 属于参考项目作者的租户，独立租户的
/// token 对它无权，反而触发 403 "bearer token invalid"。因此空列表必须
/// 明确判定为 NoProfile（账号无可用 profile → 验活失败），而不是兜底。
#[derive(Debug, Clone)]
pub enum IdcProfileResolution {
    /// 成功解析到真实 profileArn。
    Resolved(String),
    /// token 有效但账号没有任何可用 profile（ListAvailableProfiles 返回空列表）。
    /// 说明该 SSO 用户未被分配 Q Developer / Kiro 应用 → 该账号不可用。
    NoProfile,
    /// 网络 / 临时错误，无法判定。调用方保持 profileArn 缺失，下次再试。
    Unavailable,
}

/// 根据区域返回 CodeWhisperer Runtime 端点（ListAvailableProfiles 用）。
fn codewhisperer_endpoint(region: &str) -> String {
    if region.starts_with("eu-") {
        "https://codewhisperer.eu-central-1.amazonaws.com".to_string()
    } else {
        "https://codewhisperer.us-east-1.amazonaws.com".to_string()
    }
}

/// 判断凭据是否为 IdC / Enterprise（IAM Identity Center）类型。
fn is_idc_credential(credentials: &KiroCredentials) -> bool {
    credentials
        .auth_method
        .as_deref()
        .is_some_and(|m| m.eq_ignore_ascii_case("idc"))
}

/// 通过 CodeWhisperer Runtime 的 ListAvailableProfiles 为 Enterprise/IdC 账号解析
/// 真实 profileArn。
///
/// 返回：
/// - `Resolved(arn)`：`profiles[0].arn`，账号可用；
/// - `NoProfile`：HTTP 成功但 `profiles` 为空 → 该 SSO 用户没有可用的 Q Developer/Kiro
///   应用 profile，账号不可用（**不再**套用兜底 ARN，那会导致 403 "bearer token invalid"）；
/// - `Unavailable`：网络/构建/解析/非 2xx 等临时错误，无法判定，调用方保持缺失下次再试。
async fn fetch_idc_profile_arn(
    credentials: &KiroCredentials,
    config: &Config,
    token: &str,
    proxy: Option<&ProxyConfig>,
) -> IdcProfileResolution {
    let region = credentials.effective_api_region(config);
    let url = format!("{}/ListAvailableProfiles", codewhisperer_endpoint(region));
    let machine_id = machine_id::generate_from_credentials(credentials, config)
        .unwrap_or_else(|| "unknown".to_string());

    let client = match build_client(proxy, 30, config.tls_backend) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("ListAvailableProfiles 构建 client 失败: {}", e);
            return IdcProfileResolution::Unavailable;
        }
    };

    let x_amz_user_agent = format!(
        "aws-sdk-js/1.0.0 KiroIDE-{}-{}",
        config.kiro_version, machine_id
    );
    let user_agent = format!(
        "aws-sdk-js/1.0.0 ua/2.1 os/{} lang/js md/nodejs#{} api/codewhispererruntime#1.0.0 m/N,E KiroIDE-{}-{}",
        config.system_version, config.node_version, config.kiro_version, machine_id
    );

    let resp = client
        .post(&url)
        .header("content-type", "application/json")
        .header("Authorization", format!("Bearer {}", token))
        .header("x-amz-user-agent", x_amz_user_agent)
        .header("user-agent", user_agent)
        .header("amz-sdk-invocation-id", uuid::Uuid::new_v4().to_string())
        .header("amz-sdk-request", "attempt=1; max=1")
        .body("{}")
        .send()
        .await;

    let resp = match resp {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("ListAvailableProfiles 请求失败: {}", e);
            return IdcProfileResolution::Unavailable;
        }
    };

    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();

    if !status.is_success() {
        // 403 = 该账号 token 对 CodeWhisperer 无权限（无 Kiro/Q Developer 订阅或未分配应用）。
        // 这属于账号本身不可用 → 判定 NoProfile，让验活流程标记该凭证无效。
        if status.as_u16() == 403 {
            tracing::warn!(
                "ListAvailableProfiles 返回 403（账号未开通 Kiro/Q Developer 或未分配应用）→ 判定无可用 profile"
            );
            return IdcProfileResolution::NoProfile;
        }
        tracing::warn!(
            "ListAvailableProfiles 失败 status={} body={}",
            status,
            body.chars().take(200).collect::<String>()
        );
        return IdcProfileResolution::Unavailable;
    }

    let json: serde_json::Value = match serde_json::from_str(&body) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("ListAvailableProfiles 响应解析失败: {}", e);
            return IdcProfileResolution::Unavailable;
        }
    };

    let arn = json
        .get("profiles")
        .and_then(|p| p.as_array())
        .and_then(|arr| arr.first())
        .and_then(|p| p.get("arn"))
        .and_then(|a| a.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());

    match arn {
        Some(arn) => {
            tracing::info!("Enterprise/IdC profileArn 已解析: {}", arn);
            IdcProfileResolution::Resolved(arn)
        }
        None => {
            // HTTP 200 但 profiles 为空：账号没有任何可用 profile。
            tracing::warn!(
                "ListAvailableProfiles 返回空列表（账号无可用 Q Developer/Kiro profile）→ 判定无可用 profile"
            );
            IdcProfileResolution::NoProfile
        }
    }
}

// ============================================================================
// 多凭据 Token 管理器
// ============================================================================

/// 凭据禁用原因
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DisableReason {
    /// 连续失败次数过多
    FailureLimit,
    /// Token 刷新连续失败次数过多
    RefreshFailureLimit,
    /// 认证失败（如 invalid_grant）
    AuthenticationFailed,
    /// 账户被暂停
    AccountSuspended,
    /// 余额不足
    #[allow(dead_code)]
    InsufficientBalance,
    /// 模型临时不可用（全局禁用）
    ModelUnavailable,
    /// 手动禁用
    Manual,
    /// 额度已用尽（如 MONTHLY_REQUEST_COUNT）
    QuotaExceeded,
}

/// 单个凭据条目的状态
#[allow(dead_code)]
struct CredentialEntry {
    /// 凭据唯一 ID
    id: u64,
    /// 凭据信息
    credentials: KiroCredentials,
    /// API 调用连续失败次数
    failure_count: u32,
    /// Token 刷新连续失败次数
    refresh_failure_count: u32,
    /// 是否已禁用
    disabled: bool,
    /// 自愈原因（用于区分手动禁用 vs 自动禁用，便于自愈逻辑判断）
    auto_heal_reason: Option<AutoHealReason>,
    /// 禁用原因（公共 API 展示用）
    disable_reason: Option<DisableReason>,
    /// 设备指纹（每个凭据独立）
    fingerprint: Fingerprint,
    /// API 调用成功次数
    success_count: u64,
    /// 最后一次 API 调用时间（RFC3339 格式）
    last_used_at: Option<String>,
    /// refreshToken 的 SHA-256 哈希缓存（避免 snapshot 重复计算）
    refresh_token_hash: Option<String>,
    /// 最近一次已知的超额开关状态（GetUserUsageAndLimits 返回值缓存）
    overage_enabled: Option<bool>,
    /// 是否正在执行后台开启超额任务（防止重复开启）
    overage_enabling: bool,
    /// 最近一次开启超额失败原因
    overage_last_error: Option<String>,
}

/// 自愈原因（内部使用，用于判断是否可自动恢复）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AutoHealReason {
    /// Admin API 手动禁用（不自动恢复）
    Manual,
    /// 连续失败达到阈值后自动禁用（可自动恢复）
    TooManyFailures,
    /// 额度已用尽（如 MONTHLY_REQUEST_COUNT）
    #[allow(dead_code)]
    QuotaExceeded,
}

/// 统计数据持久化条目
#[derive(Serialize, Deserialize)]
struct StatsEntry {
    success_count: u64,
    last_used_at: Option<String>,
}

// ============================================================================
// Admin API 公开结构
// ============================================================================

/// 凭据条目快照（用于 Admin API 读取）
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CredentialEntrySnapshot {
    /// 凭据唯一 ID
    pub id: u64,
    /// 优先级
    pub priority: u32,
    /// 是否被禁用
    pub disabled: bool,
    /// 禁用原因
    pub disable_reason: Option<DisableReason>,
    /// 连续失败次数
    pub failure_count: u32,
    /// Token 刷新连续失败次数
    pub refresh_failure_count: u32,
    /// 认证方式
    pub auth_method: Option<String>,
    /// 是否有 Profile ARN
    pub has_profile_arn: bool,
    /// Token 过期时间
    pub expires_at: Option<String>,
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
    /// 最终生效的 endpoint 名称
    pub endpoint: Option<String>,
    /// Web Portal Idp 标识（默认推断为 Google）
    pub idp: Option<String>,
    /// 凭据级代理 URL（None 表示回退到全局代理）
    pub proxy_url: Option<String>,
    /// 凭据级代理用户名
    pub proxy_username: Option<String>,
    /// 凭据级代理是否设置了密码（不返回明文）
    pub has_proxy_password: bool,
    /// 最近一次已知的超额开关状态
    pub overage_enabled: Option<bool>,
    /// 是否正在执行后台开启超额任务
    pub overage_enabling: bool,
    /// 最近一次开启超额失败原因
    pub overage_last_error: Option<String>,
}

/// 凭据管理器状态快照
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ManagerSnapshot {
    /// 凭据条目列表
    pub entries: Vec<CredentialEntrySnapshot>,
    /// 总凭据数量
    pub total: usize,
    /// 可用凭据数量
    pub available: usize,
}

/// 单凭据 overage 状态快照
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OverageStatusSnapshot {
    pub id: u64,
    /// 最近一次已知的 overage 开关状态（None 表示未知，未调用过 GetUserUsageAndLimits）
    pub enabled: Option<bool>,
    /// 是否正在执行后台开启任务
    pub enabling: bool,
    /// 最近一次开启失败原因
    pub last_error: Option<String>,
    /// 是否具备 profileArn（开启 overage 必要条件）
    pub has_profile_arn: bool,
    /// 凭据的认证方式（用于前端判断是否可调用 web portal）
    pub auth_method: Option<String>,
}

/// Web Portal API 调用上下文
///
/// 与 [`CallContext`] 的区别：本上下文用于 app.kiro.dev 的 web portal 接口
/// （需要 idp + profileArn cookie），不参与负载均衡选择。
pub struct WebPortalContext {
    /// 凭据 ID（保留字段，便于上层日志关联，不被读取也保留）
    #[allow(dead_code)]
    pub id: u64,
    pub token: String,
    pub idp: String,
    pub profile_arn: Option<String>,
    pub proxy: Option<ProxyConfig>,
}

/// 缓存余额信息（用于 Admin API）
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CachedBalanceInfo {
    /// 凭据 ID
    pub id: u64,
    /// 缓存的剩余额度
    pub remaining: f64,
    /// 缓存时间（Unix 毫秒时间戳）
    pub cached_at: u64,
    /// 缓存存活时间（秒）
    pub ttl_secs: u64,
    /// 缓存的总额度（已知 overage 开启时包含超额额度）
    pub usage_limit: f64,
    /// 缓存快照里的超额开关状态
    pub overage_enabled: bool,
    /// 缓存快照里的超额额度上限（未开启时为 0）
    pub overage_cap: f64,
}

/// 余额缓存条目
struct CachedBalance {
    remaining: f64,
    usage_limit: f64,
    overage_enabled: bool,
    overage_cap: f64,
    cached_at: std::time::Instant,
    /// 是否已初始化（区分"未获取过余额"和"余额为零"）
    initialized: bool,
    /// 最近一段时间的使用次数（用于判断高频/低频）
    recent_usage: u32,
    /// 上次重置使用计数的时间
    usage_reset_at: std::time::Instant,
}

/// 高频渠道 TTL（10 分钟）
const BALANCE_TTL_HIGH_FREQ_SECS: u64 = 600;
/// 低频渠道 TTL（30 分钟）
const BALANCE_TTL_LOW_FREQ_SECS: u64 = 1800;
/// 低余额渠道 TTL（24 小时）
const BALANCE_TTL_LOW_BALANCE_SECS: u64 = 86400;
/// 高频判定阈值（10分钟内使用超过此次数视为高频）
const HIGH_FREQ_THRESHOLD: u32 = 20;
/// 使用计数重置周期（10 分钟）
const USAGE_COUNT_RESET_SECS: u64 = 600;
/// 低余额阈值
const LOW_BALANCE_THRESHOLD: f64 = 1.0;

/// 多凭据 Token 管理器
///
/// 支持多个凭据的管理，实现负载均衡 + 故障转移策略
/// 故障统计基于 API 调用结果，而非 Token 刷新结果
///
/// ## 增强特性
///
/// - **多维度设备指纹**: 每个凭据生成独立的设备指纹
/// - **后台 Token 刷新**: 定期预刷新即将过期的 Token
/// - **精细化速率限制**: 每日请求限制、请求间隔控制
/// - **冷却管理**: 分类管理不同原因的冷却状态
/// - **优雅降级**: Token 刷新失败时使用现有 Token
#[allow(dead_code)]
pub struct MultiTokenManager {
    config: RwLock<Config>,
    proxy: RwLock<Option<ProxyConfig>>,
    /// 凭据条目列表
    entries: Mutex<Vec<CredentialEntry>>,
    /// Token 刷新锁，确保同一时间只有一个刷新操作
    refresh_lock: TokioMutex<()>,
    /// 凭据文件路径（用于回写）
    credentials_path: Option<PathBuf>,
    /// 是否为多凭据格式（数组格式才回写）
    is_multiple_format: bool,
    /// MODEL_TEMPORARILY_UNAVAILABLE 错误计数
    model_unavailable_count: AtomicU32,
    /// 选择抖动计数器（用于同权重候选的轮询，避免总选第一个）
    selection_rr: AtomicU64,
    /// 全局禁用恢复时间（None 表示未被全局禁用）
    global_recovery_time: Mutex<Option<DateTime<Utc>>>,
    /// 用户亲和性管理器
    affinity: UserAffinityManager,
    /// 余额缓存（用于负载均衡和故障转移时选择最优凭据）
    balance_cache: Mutex<HashMap<u64, CachedBalance>>,
    /// 速率限制器
    rate_limiter: RateLimiter,
    /// 冷却管理器
    cooldown_manager: CooldownManager,
    /// 后台刷新器
    background_refresher: Option<Arc<BackgroundRefresher>>,
    /// 最近一次统计持久化时间（用于 debounce）
    last_stats_save_at: Mutex<Option<Instant>>,
    /// 统计数据是否有未落盘更新
    stats_dirty: AtomicBool,
}

/// 凭据可用性诊断：被禁用的凭据
#[allow(dead_code)]
#[derive(Debug, Clone)]
struct DisabledCredentialDiag {
    id: u64,
    disable_reason: Option<DisableReason>,
    failure_count: u32,
    priority: u32,
}

/// 凭据可用性诊断：处于冷却的凭据
#[allow(dead_code)]
#[derive(Debug, Clone)]
struct CooldownCredentialDiag {
    id: u64,
    reason: CooldownReason,
    remaining_ms: u64,
}

/// 凭据可用性诊断：被速率限制挡住的凭据
#[allow(dead_code)]
#[derive(Debug, Clone)]
struct RateLimitedCredentialDiag {
    id: u64,
    wait_ms: u64,
}

/// 每个凭据最大 API 调用失败次数
const MAX_FAILURES_PER_CREDENTIAL: u32 = 3;

/// MODEL_TEMPORARILY_UNAVAILABLE 触发全局禁用的阈值
const MODEL_UNAVAILABLE_THRESHOLD: u32 = 2;

/// 全局禁用恢复时间（分钟）
const GLOBAL_DISABLE_RECOVERY_MINUTES: i64 = 5;

/// 统计数据持久化防抖间隔
const STATS_SAVE_DEBOUNCE: StdDuration = StdDuration::from_secs(30);

/// 当所有可用凭据都进入冷却/速率限制时，如果最短等待时间不超过该阈值，
/// 继续短睡重试（平滑瞬时抖动）；超过则立即 bail，由上层返回 429 + Retry-After，
/// 避免 HTTP handler 挂到客户端超时。
const ALL_CREDENTIALS_COOLDOWN_BAIL_THRESHOLD: StdDuration = StdDuration::from_secs(2);

/// API 调用上下文
///
/// 绑定特定凭据的调用上下文，确保 token、credentials 和 id 的一致性
#[derive(Clone)]
pub struct CallContext {
    /// 凭据 ID（用于 report_success/report_failure）
    pub id: u64,
    /// 凭据信息（用于构建请求头）
    pub credentials: KiroCredentials,
    /// 访问 Token
    pub token: String,
}

/// 解析 symlink 目标路径
///
/// 优先使用 `canonicalize`（解析所有 symlink 并返回绝对路径）。
/// 如果失败（例如目标文件不存在），则尝试用 `read_link` 解析一层 symlink。
/// 如果都失败，返回原路径。
fn resolve_symlink_target(path: &PathBuf) -> PathBuf {
    // 优先尝试 canonicalize（目标文件存在时最可靠）
    if let Ok(real) = std::fs::canonicalize(path) {
        return real;
    }

    // canonicalize 失败（目标可能不存在），尝试 read_link 解析 symlink
    if let Ok(target) = std::fs::read_link(path) {
        // read_link 返回的可能是相对路径，需要相对于 symlink 所在目录解析
        if target.is_absolute() {
            return target;
        }
        if let Some(parent) = path.parent() {
            return parent.join(target);
        }
        return target;
    }

    // 都失败，返回原路径
    path.clone()
}

impl MultiTokenManager {
    /// 创建多凭据 Token 管理器
    ///
    /// # Arguments
    /// * `config` - 应用配置
    /// * `credentials` - 凭据列表
    /// * `proxy` - 可选的代理配置
    /// * `credentials_path` - 凭据文件路径（用于回写）
    /// * `is_multiple_format` - 是否为多凭据格式（数组格式才回写）
    pub fn new(
        config: Config,
        credentials: Vec<KiroCredentials>,
        proxy: Option<ProxyConfig>,
        credentials_path: Option<PathBuf>,
        is_multiple_format: bool,
    ) -> anyhow::Result<Self> {
        let rate_limit_config = {
            let mut cfg = RateLimitConfig::default();
            if let Some(rpm) = config.credential_rpm.filter(|&v| v > 0) {
                // RPM -> 固定间隔（ms），例如 20 RPM => 3000ms
                let interval_ms = (60_000u64 / rpm as u64).max(1);
                cfg.min_interval_ms = interval_ms;
                cfg.max_interval_ms = interval_ms;
                // 固定间隔下抖动无意义，避免反复计算造成误差
                cfg.jitter_percent = 0.0;
            }
            cfg
        };

        // 计算当前最大 ID，为没有 ID 的凭据分配新 ID
        let max_existing_id = credentials.iter().filter_map(|c| c.id).max().unwrap_or(0);
        let mut next_id = max_existing_id + 1;
        let mut has_new_ids = false;
        let mut has_new_machine_ids = false;
        let config_ref = &config;

        let entries: Vec<CredentialEntry> = credentials
            .into_iter()
            .map(|mut cred| {
                cred.canonicalize_auth_method();
                let id = cred.id.unwrap_or_else(|| {
                    let id = next_id;
                    next_id += 1;
                    cred.id = Some(id);
                    if !cred.runtime_only {
                        has_new_ids = true;
                    }
                    id
                });
                if cred.machine_id.is_none()
                    && let Some(machine_id) =
                        machine_id::generate_from_credentials(&cred, config_ref)
                {
                    cred.machine_id = Some(machine_id);
                    if !cred.runtime_only {
                        has_new_machine_ids = true;
                    }
                }
                // 为每个凭据生成独立的设备指纹
                let fingerprint_seed = cred
                    .refresh_token
                    .as_deref()
                    .or(cred.kiro_api_key.as_deref())
                    .or(cred.machine_id.as_deref())
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| format!("credential-{}", id));
                let fingerprint = Fingerprint::generate_from_seed(&fingerprint_seed);

                let refresh_token_hash = credential_secret_hash(&cred);
                CredentialEntry {
                    id,
                    credentials: cred.clone(),
                    failure_count: 0,
                    refresh_failure_count: 0,
                    disabled: cred.disabled, // 从配置文件读取 disabled 状态
                    auto_heal_reason: if cred.disabled {
                        Some(AutoHealReason::Manual)
                    } else {
                        None
                    },
                    disable_reason: if cred.disabled {
                        Some(DisableReason::Manual)
                    } else {
                        None
                    },
                    fingerprint,
                    success_count: 0,
                    last_used_at: None,
                    refresh_token_hash,
                    overage_enabled: cred.overage_enabled,
                    overage_enabling: false,
                    overage_last_error: None,
                }
            })
            .collect();

        // 检测重复 ID
        let mut seen_ids = std::collections::HashSet::new();
        let mut duplicate_ids = Vec::new();
        for entry in &entries {
            if !seen_ids.insert(entry.id) {
                duplicate_ids.push(entry.id);
            }
        }
        if !duplicate_ids.is_empty() {
            anyhow::bail!("检测到重复的凭据 ID: {:?}", duplicate_ids);
        }

        // 初始化余额缓存（为每个凭据创建初始条目，支持负载均衡）
        let now = std::time::Instant::now();
        let initial_cache: HashMap<u64, CachedBalance> = entries
            .iter()
            .map(|e| {
                (
                    e.id,
                    CachedBalance {
                        remaining: 0.0,
                        usage_limit: 0.0,
                        overage_enabled: e.overage_enabled.unwrap_or(false),
                        overage_cap: if e.overage_enabled == Some(true) {
                            DEFAULT_OVERAGE_CAP
                        } else {
                            0.0
                        },
                        cached_at: now,
                        initialized: false,
                        recent_usage: 0,
                        usage_reset_at: now,
                    },
                )
            })
            .collect();

        let manager = Self {
            config: RwLock::new(config),
            proxy: RwLock::new(proxy),
            entries: Mutex::new(entries),
            refresh_lock: TokioMutex::new(()),
            credentials_path,
            is_multiple_format,
            model_unavailable_count: AtomicU32::new(0),
            selection_rr: AtomicU64::new(0),
            global_recovery_time: Mutex::new(None),
            affinity: UserAffinityManager::new(),
            balance_cache: Mutex::new(initial_cache),
            rate_limiter: RateLimiter::new(rate_limit_config),
            cooldown_manager: CooldownManager::new(),
            background_refresher: None,
            last_stats_save_at: Mutex::new(None),
            stats_dirty: AtomicBool::new(false),
        };

        // 如果有新分配的 ID 或新生成的 machineId，立即持久化到配置文件
        if has_new_ids || has_new_machine_ids {
            if let Err(e) = manager.persist_credentials() {
                tracing::warn!("补全凭据 ID/machineId 后持久化失败: {}", e);
            } else {
                tracing::info!("已补全凭据 ID/machineId 并写回配置文件");
            }
        }

        // 加载持久化的统计数据（success_count, last_used_at）
        manager.load_stats();

        Ok(manager)
    }

    /// 获取配置的克隆
    pub fn config(&self) -> Config {
        self.config.read().clone()
    }

    /// 热更新代理配置
    pub fn update_proxy(&self, proxy: Option<ProxyConfig>) {
        *self.proxy.write() = proxy;
    }

    /// 热更新全局 Region
    pub fn update_region(&self, region: String) {
        self.config.write().region = region;
    }

    /// 热更新默认 endpoint
    pub fn update_default_endpoint(&self, default_endpoint: String) {
        self.config.write().default_endpoint = default_endpoint;
    }

    /// 热更新单凭据目标请求速率（RPM）
    pub fn update_credential_rpm(&self, rpm: Option<u32>) {
        // 更新 config 中的 credential_rpm
        self.config.write().credential_rpm = rpm;

        // 重新计算 RateLimitConfig 并应用到 rate_limiter
        let mut cfg = RateLimitConfig::default();
        if let Some(rpm) = rpm.filter(|&v| v > 0) {
            let interval_ms = (60_000u64 / rpm as u64).max(1);
            cfg.min_interval_ms = interval_ms;
            cfg.max_interval_ms = interval_ms;
            cfg.jitter_percent = 0.0;
        }
        self.rate_limiter.update_config(cfg);
    }

    /// 获取凭据总数
    pub fn total_count(&self) -> usize {
        self.entries.lock().len()
    }

    /// 获取可用凭据数量
    pub fn available_count(&self) -> usize {
        self.entries.lock().iter().filter(|e| !e.disabled).count()
    }

    /// 输出一份"为什么当前没有可用凭据"的诊断信息（用于排障）
    ///
    /// 注意：该方法只在 DEBUG 日志级别开启时执行，避免给正常路径引入额外开销。
    fn debug_log_availability_diagnostics(
        &self,
        event: &'static str,
        tried_ids: &[u64],
        min_wait: Option<std::time::Duration>,
        min_wait_detail: Option<(u64, &'static str, std::time::Duration)>,
    ) {
        if !tracing::enabled!(tracing::Level::DEBUG) {
            return;
        }

        // 先快照 entries，避免在持有 entries 锁时再去访问 rate_limiter/cooldown_manager。
        let (total, mut enabled_ids, mut disabled) = {
            let entries = self.entries.lock();
            let mut enabled_ids: Vec<u64> = Vec::with_capacity(entries.len());
            let mut disabled: Vec<DisabledCredentialDiag> = Vec::new();

            for e in entries.iter() {
                if e.disabled {
                    disabled.push(DisabledCredentialDiag {
                        id: e.id,
                        disable_reason: e.disable_reason,
                        failure_count: e.failure_count,
                        priority: e.credentials.priority,
                    });
                } else {
                    enabled_ids.push(e.id);
                }
            }

            (entries.len(), enabled_ids, disabled)
        };

        enabled_ids.sort_unstable();
        disabled.sort_by_key(|d| d.id);

        let enabled_total = enabled_ids.len();
        let disabled_total = disabled.len();

        let mut cooldowns: Vec<CooldownCredentialDiag> = Vec::new();
        let mut rate_limited: Vec<RateLimitedCredentialDiag> = Vec::new();
        let mut ready: Vec<u64> = Vec::new();

        for id in &enabled_ids {
            if let Some((reason, remaining)) = self.cooldown_manager.check_cooldown(*id) {
                cooldowns.push(CooldownCredentialDiag {
                    id: *id,
                    reason,
                    remaining_ms: remaining.as_millis() as u64,
                });
                continue;
            }

            match self.rate_limiter.check_rate_limit(*id) {
                Ok(()) => ready.push(*id),
                Err(wait) => rate_limited.push(RateLimitedCredentialDiag {
                    id: *id,
                    wait_ms: wait.as_millis() as u64,
                }),
            }
        }

        cooldowns.sort_by_key(|c| (c.remaining_ms, c.id));
        rate_limited.sort_by_key(|r| (r.wait_ms, r.id));
        ready.sort_unstable();

        // 基于诊断时刻的 check_rate_limit/check_cooldown 计算"下一次可能可用"的最短等待
        let computed_min_wait_ms = cooldowns
            .iter()
            .map(|c| c.remaining_ms)
            .chain(rate_limited.iter().map(|r| r.wait_ms))
            .min();

        let min_wait_ms = min_wait.map(|d| d.as_millis() as u64);
        let (min_wait_from_id, min_wait_source, min_wait_source_ms) = match min_wait_detail {
            Some((id, source, d)) => (Some(id), Some(source), Some(d.as_millis() as u64)),
            None => (None, None, None),
        };

        tracing::debug!(
            event = event,
            total = total,
            enabled_total = enabled_total,
            disabled_total = disabled_total,
            tried = tried_ids.len(),
            tried_ids = ?tried_ids,
            config_credential_rpm = ?self.config.read().credential_rpm,
            min_wait_ms = ?min_wait_ms,
            min_wait_from_id = ?min_wait_from_id,
            min_wait_source = ?min_wait_source,
            min_wait_source_ms = ?min_wait_source_ms,
            computed_min_wait_ms = ?computed_min_wait_ms,
            disabled = ?disabled,
            cooldowns = ?cooldowns,
            rate_limited = ?rate_limited,
            ready = ?ready,
            "凭据可用性诊断"
        );
    }

    /// 选择最佳凭据（两级排序：使用次数最少 + 余额最多；完全相同则轮询）
    fn select_best_candidate_id(&self, candidate_ids: &[u64]) -> Option<u64> {
        if candidate_ids.is_empty() {
            return None;
        }

        let rr = self.selection_rr.fetch_add(1, Ordering::Relaxed) as usize;
        let cache = self.balance_cache.lock();

        let mut scored: Vec<(u64, u32, f64)> = Vec::with_capacity(candidate_ids.len());
        for &id in candidate_ids {
            let (usage, balance, initialized) = cache
                .get(&id)
                .map(|c| (c.recent_usage, c.remaining, c.initialized))
                .unwrap_or((0, 0.0, false));
            // 未初始化的凭据视为使用次数最大，避免被优先选中
            let effective_usage = if initialized { usage } else { u32::MAX };
            // NaN 余额归一化为 0.0，避免 total_cmp 将 NaN 视为最大值
            let effective_balance = if balance.is_finite() { balance } else { 0.0 };
            scored.push((id, effective_usage, effective_balance));
        }

        // 第一优先级：使用次数最少
        let min_usage = scored.iter().map(|(_, usage, _)| *usage).min()?;
        scored.retain(|(_, usage, _)| *usage == min_usage);

        // 第二优先级：余额最多（使用次数相同）
        let mut max_balance = scored.first().map(|(_, _, b)| *b).unwrap_or(0.0);
        for &(_, _, balance) in &scored {
            if balance > max_balance {
                max_balance = balance;
            }
        }
        scored.retain(|(_, _, balance)| *balance == max_balance);

        if scored.len() == 1 {
            return Some(scored[0].0);
        }

        // 兜底：完全相同则轮询，避免总选第一个
        let index = rr % scored.len();
        Some(scored[index].0)
    }

    /// 获取 API 调用上下文
    ///
    /// 返回绑定了 id、credentials 和 token 的调用上下文
    /// 确保整个 API 调用过程中使用一致的凭据信息
    ///
    /// 选择策略：按优先级选择可用凭据
    /// 如果 Token 过期或即将过期，会自动刷新
    /// Token 刷新失败时会尝试下一个可用凭据（不计入失败次数）
    pub async fn acquire_context(&self) -> anyhow::Result<CallContext> {
        self.acquire_context_excluding(&[]).await
    }

    /// 与 `acquire_context` 相同，但额外把 `exclude_ids` 在第一轮就视作"已尝试"，
    /// 避免 retry 链路重新选回上一次失败的同一个凭据。
    pub async fn acquire_context_excluding(
        &self,
        exclude_ids: &[u64],
    ) -> anyhow::Result<CallContext> {
        // 检查是否需要自动恢复
        self.check_and_recover();

        let total = self.total_count();
        let mut tried_ids: Vec<u64> = exclude_ids.to_vec();
        // 当所有凭据都因“临时不可用”（冷却/速率限制）被跳过时，等待最短可用时间再重试。
        let mut min_wait: Option<std::time::Duration> = None;
        // 记录最短等待时间来自哪个凭据/原因，便于排障定位（冷却 vs 速率限制）。
        let mut min_wait_detail: Option<(u64, &'static str, std::time::Duration)> = None;
        // 追踪仅因冷却/速率限制被跳过的凭据数量。
        // 只有当“所有被跳过的凭据都是冷却/限流”时才触发 429 + Retry-After；
        // 若混杂了 token 刷新失败等非临时性错误，应走常规 sleep-retry 路径保留原有语义。
        let mut cooling_skipped: usize = 0;

        loop {
            // tried_ids 只会记录“本轮已经尝试过的可用凭据”（disabled 的不会被选中）。
            // 因此当存在部分 disabled 凭据时，tried_ids.len() 可能永远达不到 total，
            // 但已用尽所有可用凭据（常见于：全部被速率限制/冷却短暂挡住）。
            //
            // 这里用 available_count() 判断“可用集合是否已被尝试完”，避免误报
            // "所有凭据均已禁用（x/y）" 这类与事实不符的错误。
            let enabled_total = self.available_count();
            if enabled_total > 0 && tried_ids.len() >= enabled_total {
                if let Some(wait) = min_wait {
                    // 仅当本轮所有被跳过的凭据都因冷却/限流时，才以 429 + Retry-After 快速返回；
                    // 若混杂 token 刷新失败等非临时性错误，保留原有 sleep-retry 语义以避免吞掉真实错误。
                    let all_due_to_cooling = cooling_skipped == tried_ids.len();
                    if all_due_to_cooling && wait > ALL_CREDENTIALS_COOLDOWN_BAIL_THRESHOLD {
                        self.debug_log_availability_diagnostics(
                            "enabled_exhausted_bail_long_wait",
                            &tried_ids,
                            min_wait,
                            min_wait_detail,
                        );
                        // Retry-After 语义要求向上取整，避免客户端在实际等待结束前提前重试。
                        let secs = (wait.as_millis().div_ceil(1000) as u64).max(1);
                        let (cid, source) = min_wait_detail
                            .map(|(id, src, _)| (id, src))
                            .unwrap_or((0, "unknown"));
                        anyhow::bail!(
                            "所有凭据均处于冷却/速率限制（retry_after_secs={}，原因：{}，来自凭据 #{}）",
                            secs,
                            source,
                            cid
                        );
                    }
                    self.debug_log_availability_diagnostics(
                        "enabled_exhausted_sleep",
                        &tried_ids,
                        min_wait,
                        min_wait_detail,
                    );
                    tokio::time::sleep(wait).await;
                    tried_ids.clear();
                    cooling_skipped = 0;
                    min_wait = None;
                    min_wait_detail = None;
                    continue;
                }
                self.debug_log_availability_diagnostics(
                    "enabled_exhausted_bail",
                    &tried_ids,
                    min_wait,
                    min_wait_detail,
                );
                anyhow::bail!(
                    "所有可用凭据均无法获取有效 Token（可用: {}/{}）",
                    enabled_total,
                    total
                );
            }

            if tried_ids.len() >= total {
                if let Some(wait) = min_wait {
                    let all_due_to_cooling = cooling_skipped == tried_ids.len();
                    if all_due_to_cooling && wait > ALL_CREDENTIALS_COOLDOWN_BAIL_THRESHOLD {
                        self.debug_log_availability_diagnostics(
                            "total_exhausted_bail_long_wait",
                            &tried_ids,
                            min_wait,
                            min_wait_detail,
                        );
                        let secs = (wait.as_millis().div_ceil(1000) as u64).max(1);
                        let (cid, source) = min_wait_detail
                            .map(|(id, src, _)| (id, src))
                            .unwrap_or((0, "unknown"));
                        anyhow::bail!(
                            "所有凭据均处于冷却/速率限制（retry_after_secs={}，原因：{}，来自凭据 #{}）",
                            secs,
                            source,
                            cid
                        );
                    }
                    self.debug_log_availability_diagnostics(
                        "total_exhausted_sleep",
                        &tried_ids,
                        min_wait,
                        min_wait_detail,
                    );
                    tokio::time::sleep(wait).await;
                    tried_ids.clear();
                    cooling_skipped = 0;
                    min_wait = None;
                    min_wait_detail = None;
                    continue;
                }
                self.debug_log_availability_diagnostics(
                    "total_exhausted_bail",
                    &tried_ids,
                    min_wait,
                    min_wait_detail,
                );
                anyhow::bail!(
                    "所有凭据均无法获取有效 Token（可用: {}/{}）",
                    self.available_count(),
                    total
                );
            }

            let candidate_infos: Vec<(u64, u32, bool)> = {
                let mut entries = self.entries.lock();

                let mut candidates: Vec<(u64, u32, bool)> = entries
                    .iter()
                    .filter(|e| !e.disabled && !tried_ids.contains(&e.id))
                    .map(|e| (e.id, e.credentials.priority, e.credentials.runtime_only))
                    .collect();

                // 没有可用凭据：如果是"自动禁用导致全灭"，做一次类似重启的自愈
                if candidates.is_empty()
                    && entries.iter().any(|e| {
                        e.disabled && e.auto_heal_reason == Some(AutoHealReason::TooManyFailures)
                    })
                {
                    tracing::warn!(
                        "所有凭据均已被自动禁用，执行自愈：重置失败计数并重新启用（等价于重启）"
                    );
                    for e in entries.iter_mut() {
                        if e.auto_heal_reason == Some(AutoHealReason::TooManyFailures) {
                            e.disabled = false;
                            e.auto_heal_reason = None;
                            e.disable_reason = None;
                            e.failure_count = 0;
                        }
                    }

                    candidates = entries
                        .iter()
                        .filter(|e| !e.disabled && !tried_ids.contains(&e.id))
                        .map(|e| (e.id, e.credentials.priority, e.credentials.runtime_only))
                        .collect();
                }

                if candidates.is_empty() {
                    let available = entries.iter().filter(|e| !e.disabled).count();
                    if available == 0 {
                        anyhow::bail!("所有凭据均已禁用（{}/{}）", available, total);
                    }
                    anyhow::bail!(
                        "所有可用凭据均已尝试（可用: {}/{}，已尝试: {}/{}）",
                        available,
                        total,
                        tried_ids.len(),
                        available
                    );
                }

                candidates
            };

            // 按优先级选出候选集合；同优先级时，优先选择仅运行时的环境变量凭据，再做负载均衡选择
            let min_priority = candidate_infos
                .iter()
                .map(|(_, p, _)| *p)
                .min()
                .unwrap_or(0);
            let prefer_runtime_only = candidate_infos
                .iter()
                .any(|(_, p, runtime_only)| *p == min_priority && *runtime_only);
            let candidate_ids: Vec<u64> = candidate_infos
                .iter()
                .filter(|(_, p, runtime_only)| {
                    *p == min_priority && (!prefer_runtime_only || *runtime_only)
                })
                .map(|(id, _, _)| *id)
                .collect();
            let id = self
                .select_best_candidate_id(&candidate_ids)
                .ok_or_else(|| anyhow::anyhow!("没有可用凭据"))?;

            // 冷却/速率限制：把“临时不可用”的凭据视为本轮不可选，从而自然分流到其他凭据。
            if let Some((reason, remaining)) = self.cooldown_manager.check_cooldown(id) {
                tracing::trace!(
                    credential_id = %id,
                    reason = ?reason,
                    remaining_ms = %remaining.as_millis(),
                    "凭据处于冷却，跳过"
                );
                if min_wait.map(|w| remaining < w).unwrap_or(true) {
                    min_wait_detail = Some((id, "cooldown", remaining));
                }
                min_wait = Some(min_wait.map(|w| w.min(remaining)).unwrap_or(remaining));
                tried_ids.push(id);
                cooling_skipped += 1;
                continue;
            }
            if let Err(wait) = self.rate_limiter.try_acquire(id) {
                tracing::trace!(
                    credential_id = %id,
                    wait_ms = %wait.as_millis(),
                    "凭据触发速率限制，跳过"
                );
                if min_wait.map(|w| wait < w).unwrap_or(true) {
                    min_wait_detail = Some((id, "rate_limit", wait));
                }
                min_wait = Some(min_wait.map(|w| w.min(wait)).unwrap_or(wait));
                tried_ids.push(id);
                cooling_skipped += 1;
                continue;
            }

            let credentials = {
                let entries = self.entries.lock();
                entries
                    .iter()
                    .find(|e| e.id == id)
                    .map(|e| e.credentials.clone())
                    .ok_or_else(|| anyhow::anyhow!("凭据 #{} 不存在", id))?
            };

            // 尝试获取/刷新 Token
            match self.try_ensure_token(id, &credentials).await {
                Ok(ctx) => {
                    return Ok(ctx);
                }
                Err(e) => {
                    tracing::warn!("凭据 #{} Token 刷新失败，尝试下一个凭据: {}", id, e);
                    tried_ids.push(id);
                }
            }
        }
    }

    /// 获取指定用户的 API 调用上下文（带亲和性）
    ///
    /// 如果用户已绑定凭据且该凭据可用，优先使用绑定的凭据
    /// 否则使用默认的 acquire_context() 逻辑并建立新绑定
    #[allow(dead_code)]
    pub async fn acquire_context_for_user(
        &self,
        user_id: Option<&str>,
    ) -> anyhow::Result<CallContext> {
        self.acquire_context_for_user_excluding(user_id, &[]).await
    }

    /// 与 `acquire_context_for_user` 相同，但 `exclude_ids` 内的凭据会被强制跳过：
    /// 即使 affinity 命中了被排除的凭据，也会落入 LB 重新选号。
    /// 用于 retry 链路避免反复选回同一个失败凭据（实测 0% 切换率的关键修复）。
    pub async fn acquire_context_for_user_excluding(
        &self,
        user_id: Option<&str>,
        exclude_ids: &[u64],
    ) -> anyhow::Result<CallContext> {
        // 无 user_id 时走默认逻辑
        let user_id = match user_id {
            Some(id) if !id.is_empty() => id,
            _ => return self.acquire_context_excluding(exclude_ids).await,
        };

        // 默认保持用户绑定（用于连续对话）。当绑定凭据“临时不可用”（速率限制/短冷却）时，
        // 允许分流到其他凭据，但不强制重绑，避免频繁抖动。
        let mut keep_affinity_binding = false;

        if let Some(bound_id) = self.affinity.get(user_id) {
            // P0#1 修复：retry 时若 affinity 绑定的凭据在 exclude_ids 中（上次失败），
            // 跳过 affinity 短路逻辑，直接走 LB 重新选号。
            let bound_excluded = exclude_ids.contains(&bound_id);
            let is_enabled = !bound_excluded && {
                let entries = self.entries.lock();
                entries.iter().any(|e| e.id == bound_id && !e.disabled)
            };

            if is_enabled {
                if let Some((reason, remaining)) = self.cooldown_manager.check_cooldown(bound_id) {
                    // 对“长冷却”原因不保留绑定，避免长期命中后每次都先失败再回退。
                    keep_affinity_binding = matches!(
                        reason,
                        CooldownReason::RateLimitExceeded
                            | CooldownReason::TokenRefreshFailed
                            | CooldownReason::ServerError
                            | CooldownReason::ModelUnavailable
                    );
                    tracing::debug!(
                        user_id = %user_id,
                        credential_id = %bound_id,
                        reason = ?reason,
                        remaining_ms = %remaining.as_millis(),
                        keep_affinity_binding = %keep_affinity_binding,
                        "亲和性绑定凭据处于冷却，本次将分流"
                    );
                } else if let Err(wait) = self.rate_limiter.check_rate_limit(bound_id) {
                    // 只读检查，不消耗速率限制配额
                    keep_affinity_binding = true;
                    tracing::info!(
                        user_id = %mask_user_id(Some(user_id)),
                        credential_id = %bound_id,
                        wait_ms = %wait.as_millis(),
                        "亲和性绑定凭据触发速率限制，本次将分流"
                    );
                } else if let Err(wait) = self.rate_limiter.try_acquire(bound_id) {
                    // check_rate_limit 通过但 try_acquire 竞争失败（TOCTOU），保留绑定分流
                    keep_affinity_binding = true;
                    tracing::debug!(
                        user_id = %mask_user_id(Some(user_id)),
                        credential_id = %bound_id,
                        wait_ms = %wait.as_millis(),
                        "亲和性凭据 try_acquire 竞争失败，本次将分流"
                    );
                } else {
                    let credentials = {
                        let entries = self.entries.lock();
                        entries
                            .iter()
                            .find(|e| e.id == bound_id)
                            .map(|e| e.credentials.clone())
                    };

                    match credentials {
                        Some(creds) => match self.try_ensure_token(bound_id, &creds).await {
                            Ok(ctx) => {
                                self.affinity.touch(user_id);
                                return Ok(ctx);
                            }
                            Err(e) => {
                                tracing::debug!(
                                    user_id = %user_id,
                                    credential_id = %bound_id,
                                    error = %e,
                                    "亲和性绑定凭据 token 获取/刷新失败，本次将分流"
                                );
                            }
                        },
                        None => {
                            tracing::warn!(
                                user_id = %user_id,
                                credential_id = %bound_id,
                                "亲和性命中但凭据不存在，本次将分流"
                            );
                        }
                    }
                }
            }
        }

        let ctx = self.acquire_context_excluding(exclude_ids).await?;
        if !keep_affinity_binding {
            self.affinity.set(user_id, ctx.id);
        }
        Ok(ctx)
    }

    /// 获取缓存的余额（用于故障转移选择）
    #[allow(dead_code)]
    fn get_cached_balance(&self, id: u64) -> f64 {
        let cache = self.balance_cache.lock();
        if let Some(entry) = cache.get(&id) {
            // 动态 TTL：低余额 > 低频 > 高频
            let ttl = if entry.remaining < LOW_BALANCE_THRESHOLD {
                BALANCE_TTL_LOW_BALANCE_SECS
            } else if entry.recent_usage >= HIGH_FREQ_THRESHOLD {
                BALANCE_TTL_HIGH_FREQ_SECS
            } else {
                BALANCE_TTL_LOW_FREQ_SECS
            };
            if entry.cached_at.elapsed().as_secs() < ttl {
                return entry.remaining;
            }
        }
        // 缓存不存在或过期，返回 0（会回退到优先级选择）
        0.0
    }

    /// 更新余额缓存
    pub fn update_balance_cache(&self, id: u64, remaining: f64) {
        self.update_balance_cache_details(id, remaining, 0.0, None, None);
    }

    /// 更新余额缓存，并同步 overage/总额度快照，供 Admin UI 凭证卡片直接展示。
    pub fn update_balance_cache_details(
        &self,
        id: u64,
        remaining: f64,
        usage_limit: f64,
        overage_enabled: Option<bool>,
        overage_cap: Option<f64>,
    ) {
        let mut cache = self.balance_cache.lock();
        let now = std::time::Instant::now();
        // 保留现有使用计数
        let (
            recent_usage,
            usage_reset_at,
            previous_usage_limit,
            previous_overage_enabled,
            previous_overage_cap,
        ) = cache
            .get(&id)
            .map(|e| {
                (
                    e.recent_usage,
                    e.usage_reset_at,
                    e.usage_limit,
                    e.overage_enabled,
                    e.overage_cap,
                )
            })
            .unwrap_or((0, now, 0.0, false, 0.0));
        let known_overage_enabled = overage_enabled.unwrap_or(previous_overage_enabled);
        let known_overage_cap = overage_cap.unwrap_or(if known_overage_enabled {
            previous_overage_cap.max(DEFAULT_OVERAGE_CAP)
        } else {
            0.0
        });
        let known_usage_limit = if usage_limit > 0.0 {
            usage_limit
        } else if previous_usage_limit > 0.0 {
            previous_usage_limit
        } else if known_overage_enabled {
            remaining.max(0.0) + known_overage_cap
        } else {
            0.0
        };
        cache.insert(
            id,
            CachedBalance {
                remaining,
                usage_limit: known_usage_limit,
                overage_enabled: known_overage_enabled,
                overage_cap: known_overage_cap,
                cached_at: now,
                initialized: true,
                recent_usage,
                usage_reset_at,
            },
        );
    }

    /// 从持久化缓存恢复余额信息（用于服务启动后恢复 Admin UI 展示）
    pub fn restore_balance_cache(&self, id: u64, remaining: f64, cached_at_unix_secs: f64) {
        let mut cache = self.balance_cache.lock();
        let now_instant = std::time::Instant::now();
        let now_unix_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64();
        let age_secs = (now_unix_secs - cached_at_unix_secs).max(0.0);
        // 若系统 uptime < age_secs（如刚重启），checked_sub 会返回 None，
        // 此时设为足够旧的时间点（now - 24h），确保 TTL 判定视为已过期
        let restored_cached_at = now_instant
            .checked_sub(std::time::Duration::from_secs_f64(age_secs))
            .unwrap_or_else(|| {
                now_instant
                    .checked_sub(std::time::Duration::from_secs(86400))
                    .unwrap_or(now_instant)
            });

        let (
            recent_usage,
            usage_reset_at,
            previous_usage_limit,
            previous_overage_enabled,
            previous_overage_cap,
        ) = cache
            .get(&id)
            .map(|e| {
                (
                    e.recent_usage,
                    e.usage_reset_at,
                    e.usage_limit,
                    e.overage_enabled,
                    e.overage_cap,
                )
            })
            .unwrap_or((0, now_instant, 0.0, false, 0.0));

        let known_overage_enabled = previous_overage_enabled;
        let known_overage_cap = if known_overage_enabled {
            previous_overage_cap.max(DEFAULT_OVERAGE_CAP)
        } else {
            0.0
        };
        let known_usage_limit = if previous_usage_limit > 0.0 {
            previous_usage_limit
        } else if known_overage_enabled {
            remaining.max(0.0) + known_overage_cap
        } else {
            0.0
        };

        cache.insert(
            id,
            CachedBalance {
                remaining,
                usage_limit: known_usage_limit,
                overage_enabled: known_overage_enabled,
                overage_cap: known_overage_cap,
                cached_at: restored_cached_at,
                initialized: true,
                recent_usage,
                usage_reset_at,
            },
        );
    }

    /// 检查是否需要刷新余额缓存
    pub fn should_refresh_balance(&self, id: u64) -> bool {
        let cache = self.balance_cache.lock();
        if let Some(entry) = cache.get(&id) {
            // 未初始化的缓存需要立即刷新
            if !entry.initialized {
                return true;
            }
            // 使用动态 TTL 判断是否过期
            let ttl = if entry.remaining < LOW_BALANCE_THRESHOLD {
                BALANCE_TTL_LOW_BALANCE_SECS
            } else if entry.recent_usage >= HIGH_FREQ_THRESHOLD {
                BALANCE_TTL_HIGH_FREQ_SECS
            } else {
                BALANCE_TTL_LOW_FREQ_SECS
            };
            entry.cached_at.elapsed().as_secs() >= ttl
        } else {
            true // 无缓存，需要刷新
        }
    }

    /// 记录凭据使用（用于动态 TTL 计算和负载均衡）
    pub fn record_usage(&self, id: u64) {
        let mut cache = self.balance_cache.lock();
        let now = std::time::Instant::now();
        if let Some(entry) = cache.get_mut(&id) {
            // 重置周期过期则清零
            if entry.usage_reset_at.elapsed().as_secs() >= USAGE_COUNT_RESET_SECS {
                entry.recent_usage = 1;
                entry.usage_reset_at = now;
            } else {
                entry.recent_usage = entry.recent_usage.saturating_add(1);
            }
        } else {
            // 缓存条目不存在时创建新条目（余额未知设为 0）
            let overage_enabled = self.is_overage_known_enabled(id);
            cache.insert(
                id,
                CachedBalance {
                    remaining: 0.0,
                    usage_limit: 0.0,
                    overage_enabled,
                    overage_cap: if overage_enabled {
                        DEFAULT_OVERAGE_CAP
                    } else {
                        0.0
                    },
                    cached_at: now,
                    initialized: false,
                    recent_usage: 1,
                    usage_reset_at: now,
                },
            );
        }
    }

    /// 获取所有凭据的缓存余额信息（用于 Admin API）
    ///
    /// 返回每个凭据的缓存余额、缓存时间和 TTL
    pub fn get_all_cached_balances(&self) -> Vec<CachedBalanceInfo> {
        // 先获取 entries 的 ID 列表，避免同时持有两个锁
        let entry_ids: Vec<u64> = {
            let entries = self.entries.lock();
            entries.iter().map(|e| e.id).collect()
        };

        let cache = self.balance_cache.lock();
        let now_unix_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        entry_ids
            .iter()
            .filter_map(|&id| {
                cache.get(&id).map(|cached| {
                    // 计算动态 TTL
                    let ttl_secs = if !cached.initialized {
                        // 未初始化的缓存，TTL 设为 0（已过期）
                        0
                    } else if cached.remaining < LOW_BALANCE_THRESHOLD {
                        BALANCE_TTL_LOW_BALANCE_SECS
                    } else if cached.recent_usage >= HIGH_FREQ_THRESHOLD {
                        BALANCE_TTL_HIGH_FREQ_SECS
                    } else {
                        BALANCE_TTL_LOW_FREQ_SECS
                    };

                    // 计算缓存时间的 Unix 毫秒时间戳
                    let elapsed_ms = cached.cached_at.elapsed().as_millis() as u64;
                    let cached_at_unix_ms = now_unix_ms.saturating_sub(elapsed_ms);

                    CachedBalanceInfo {
                        id,
                        remaining: cached.remaining,
                        cached_at: cached_at_unix_ms,
                        ttl_secs,
                        usage_limit: cached.usage_limit,
                        overage_enabled: cached.overage_enabled,
                        overage_cap: cached.overage_cap,
                    }
                })
            })
            .collect()
    }

    /// 尝试使用指定凭据获取有效 Token
    ///
    /// 使用双重检查锁定模式，确保同一时间只有一个刷新操作
    ///
    /// # Arguments
    /// * `id` - 凭据 ID，用于更新正确的条目
    /// * `credentials` - 凭据信息
    async fn try_ensure_token(
        &self,
        id: u64,
        credentials: &KiroCredentials,
    ) -> anyhow::Result<CallContext> {
        // 获取配置快照（避免跨 await 持有读锁）
        let config = self.config.read().clone();

        let token_missing_or_truncated = |creds: &KiroCredentials| {
            if creds.is_api_key_credential() {
                return creds
                    .kiro_api_key
                    .as_deref()
                    .is_none_or(|t| t.trim().is_empty());
            }
            creds
                .access_token
                .as_deref()
                .is_none_or(|t| t.trim().is_empty() || t.ends_with("...") || t.contains("..."))
        };

        // 第一次检查（无锁）：快速判断是否需要刷新
        let needs_refresh = token_missing_or_truncated(credentials)
            || is_token_expired(credentials)
            || is_token_expiring_soon(credentials);

        let creds = if needs_refresh {
            // 获取刷新锁，确保同一时间只有一个刷新操作
            let _guard = self.refresh_lock.lock().await;

            // 第二次检查：获取锁后重新读取凭据，因为其他请求可能已经完成刷新
            let current_creds = {
                let entries = self.entries.lock();
                entries
                    .iter()
                    .find(|e| e.id == id)
                    .map(|e| e.credentials.clone())
                    .ok_or_else(|| anyhow::anyhow!("凭据 #{} 不存在", id))?
            };

            if token_missing_or_truncated(&current_creds)
                || is_token_expired(&current_creds)
                || is_token_expiring_soon(&current_creds)
            {
                // 确实需要刷新
                if current_creds.is_api_key_credential() {
                    current_creds
                } else {
                    let proxy = self.proxy.read().clone();
                    let new_creds =
                        match refresh_token_with_id(&current_creds, &config, proxy.as_ref(), id)
                            .await
                        {
                            Ok(creds) => {
                                let mut entries = self.entries.lock();
                                if let Some(entry) = entries.iter_mut().find(|e| e.id == id) {
                                    entry.refresh_failure_count = 0;
                                    if entry.disable_reason
                                        == Some(DisableReason::RefreshFailureLimit)
                                    {
                                        entry.disabled = false;
                                        entry.auto_heal_reason = None;
                                        entry.disable_reason = None;
                                    }
                                }
                                creds
                            }
                            Err(err) => {
                                if is_invalid_grant_error(&err) {
                                    self.mark_authentication_failed(id);
                                    tracing::warn!(
                                        credential_id = id,
                                        "凭据 Token 刷新失败（invalid_grant，已立即禁用）: {}",
                                        err
                                    );
                                    return Err(err);
                                }

                                let has_available = {
                                    let mut entries = self.entries.lock();
                                    if let Some(entry) = entries.iter_mut().find(|e| e.id == id) {
                                        entry.refresh_failure_count += 1;
                                        let refresh_failure_count = entry.refresh_failure_count;
                                        if refresh_failure_count >= MAX_FAILURES_PER_CREDENTIAL {
                                            entry.disabled = true;
                                            entry.auto_heal_reason =
                                                Some(AutoHealReason::TooManyFailures);
                                            entry.disable_reason =
                                                Some(DisableReason::RefreshFailureLimit);
                                        }
                                    }
                                    entries.iter().any(|e| !e.disabled && e.id != id)
                                };
                                tracing::warn!(
                                    credential_id = id,
                                    has_available,
                                    "凭据 Token 刷新失败: {}",
                                    err
                                );
                                return Err(err);
                            }
                        };

                    if is_token_expired(&new_creds) {
                        anyhow::bail!("刷新后的 Token 仍然无效或已过期");
                    }

                    // 更新凭据
                    {
                        let mut entries = self.entries.lock();
                        if let Some(entry) = entries.iter_mut().find(|e| e.id == id) {
                            entry.credentials = new_creds.clone();
                            // 更新哈希缓存
                            entry.refresh_token_hash = credential_secret_hash(&new_creds);
                        }
                    }

                    // 回写凭据到文件（仅多凭据格式），失败只记录警告
                    if let Err(e) = self.persist_credentials() {
                        tracing::warn!("Token 刷新后持久化失败（不影响本次请求）: {}", e);
                    }

                    new_creds
                }
            } else {
                // 其他请求已经完成刷新，直接使用新凭据
                tracing::debug!("Token 已被其他请求刷新，跳过刷新");
                current_creds
            }
        } else {
            credentials.clone()
        };

        let token = if creds.is_api_key_credential() {
            creds
                .kiro_api_key
                .clone()
                .ok_or_else(|| anyhow::anyhow!("没有可用的 kiroApiKey"))?
        } else {
            creds
                .access_token
                .clone()
                .ok_or_else(|| anyhow::anyhow!("没有可用的 accessToken"))?
        };

        Ok(CallContext {
            id,
            credentials: creds,
            token,
        })
    }

    /// 标记指定凭据的 accessToken 失效（强制触发后续刷新）
    ///
    /// 用于处理上游返回「bearer token invalid」但本地 expiresAt 未及时更新的场景：
    /// - 清空 accessToken（避免继续复用无效 token）
    /// - 将 expiresAt 设为当前时间（确保 is_token_expired() 为 true）
    ///
    /// 返回是否找到并更新了该凭据。
    pub fn invalidate_access_token(&self, id: u64) -> bool {
        let mut entries = self.entries.lock();
        let Some(entry) = entries.iter_mut().find(|e| e.id == id) else {
            return false;
        };

        entry.credentials.access_token = None;
        entry.credentials.expires_at = Some(Utc::now().to_rfc3339());
        true
    }

    /// 将凭据列表回写到源文件
    ///
    /// 仅在以下条件满足时回写：
    /// - 源文件是多凭据格式（数组）
    /// - credentials_path 已设置
    ///
    /// 注意：调用方应确保适当的同步机制，避免并发写入导致数据丢失。
    ///
    /// # Returns
    /// - `Ok(true)` - 成功写入文件
    /// - `Ok(false)` - 跳过写入（非多凭据格式或无路径配置）
    /// - `Err(_)` - 写入失败
    fn persist_credentials(&self) -> anyhow::Result<bool> {
        use anyhow::Context;

        // 仅多凭据格式才回写
        if !self.is_multiple_format {
            return Ok(false);
        }

        let path = match &self.credentials_path {
            Some(p) => p.clone(),
            None => return Ok(false),
        };

        // 在持有 entries 锁的情况下收集凭据并序列化
        // 这确保了快照的一致性
        let json = {
            let entries = self.entries.lock();
            let credentials: Vec<KiroCredentials> = entries
                .iter()
                .filter(|e| !e.credentials.runtime_only)
                .map(|e| {
                    let mut cred = e.credentials.clone();
                    cred.canonicalize_auth_method();
                    // 仅持久化手动禁用状态，自动禁用（失败阈值/额度用尽等）不落盘，
                    // 避免重启后自动禁用被误标记为手动禁用导致无法自愈
                    cred.disabled = e.disable_reason == Some(DisableReason::Manual);
                    cred
                })
                .collect();
            serde_json::to_string_pretty(&credentials).context("序列化凭据失败")?
        };

        // 原子写入：先写临时文件，再 rename 替换目标文件
        // rename 在同一文件系统上是原子操作，避免进程崩溃导致凭据文件损坏
        // 解析 symlink 以确保 rename 写入真实目标（而非替换 symlink 本身）
        let real_path = resolve_symlink_target(&path);
        let tmp_path = real_path.with_extension("json.tmp");

        let do_atomic_write = || -> anyhow::Result<()> {
            // 尝试保留原文件权限（避免 umask 导致权限放宽）
            let original_perms = std::fs::metadata(&real_path).ok().map(|m| m.permissions());

            std::fs::write(&tmp_path, &json)
                .with_context(|| format!("写入临时凭据文件失败: {:?}", tmp_path))?;

            if let Some(perms) = original_perms {
                // best-effort：权限复制失败不阻塞回写
                let _ = std::fs::set_permissions(&tmp_path, perms);
            }

            // 跨平台原子替换：Windows 上 rename 无法覆盖已存在文件，需先删除
            #[cfg(windows)]
            if real_path.exists() {
                std::fs::remove_file(&real_path)
                    .with_context(|| format!("删除旧凭据文件失败: {:?}", real_path))?;
            }

            std::fs::rename(&tmp_path, &real_path).with_context(|| {
                format!("原子替换凭据文件失败: {:?} -> {:?}", tmp_path, real_path)
            })?;
            Ok(())
        };

        if tokio::runtime::Handle::try_current().is_ok() {
            tokio::task::block_in_place(do_atomic_write)?;
        } else {
            do_atomic_write()?;
        }

        tracing::debug!("已回写凭据到文件: {:?}", path);
        Ok(true)
    }

    /// 获取当前全局代理配置（用于 SSO 登录等需要出站请求的场景）
    pub fn current_proxy(&self) -> Option<ProxyConfig> {
        self.proxy.read().clone()
    }

    /// 获取缓存目录（凭据文件所在目录）
    pub fn cache_dir(&self) -> Option<PathBuf> {
        self.credentials_path
            .as_ref()
            .and_then(|p| p.parent().map(|d| d.to_path_buf()))
    }

    /// 统计数据文件路径
    fn stats_path(&self) -> Option<PathBuf> {
        self.cache_dir().map(|d| d.join("kiro_stats.json"))
    }

    /// 从磁盘加载统计数据并应用到当前条目
    fn load_stats(&self) {
        let path = match self.stats_path() {
            Some(p) => p,
            None => return,
        };

        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => return, // 首次运行时文件不存在
        };

        let stats: HashMap<String, StatsEntry> = match serde_json::from_str(&content) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("解析统计缓存失败，将忽略: {}", e);
                return;
            }
        };

        let mut entries = self.entries.lock();
        for entry in entries.iter_mut() {
            if let Some(s) = stats.get(&entry.id.to_string()) {
                entry.success_count = s.success_count;
                entry.last_used_at = s.last_used_at.clone();
            }
        }
        *self.last_stats_save_at.lock() = Some(Instant::now());
        self.stats_dirty.store(false, Ordering::Relaxed);
        tracing::info!("已从缓存加载 {} 条统计数据", stats.len());
    }

    /// 将当前统计数据持久化到磁盘
    fn save_stats(&self) {
        let path = match self.stats_path() {
            Some(p) => p,
            None => return,
        };

        let stats: HashMap<String, StatsEntry> = {
            let entries = self.entries.lock();
            entries
                .iter()
                .map(|e| {
                    (
                        e.id.to_string(),
                        StatsEntry {
                            success_count: e.success_count,
                            last_used_at: e.last_used_at.clone(),
                        },
                    )
                })
                .collect()
        };

        match serde_json::to_string_pretty(&stats) {
            Ok(json) => {
                // 原子写入：先写临时文件，再重命名
                let tmp_path = path.with_extension("json.tmp");
                match std::fs::write(&tmp_path, json) {
                    Ok(_) => {
                        if let Err(e) = std::fs::rename(&tmp_path, &path) {
                            tracing::warn!("原子重命名统计缓存失败: {}", e);
                            let _ = std::fs::remove_file(&tmp_path);
                        } else {
                            *self.last_stats_save_at.lock() = Some(Instant::now());
                            self.stats_dirty.store(false, Ordering::Relaxed);
                        }
                    }
                    Err(e) => tracing::warn!("写入临时统计文件失败: {}", e),
                }
            }
            Err(e) => tracing::warn!("序列化统计数据失败: {}", e),
        }
    }

    /// 标记统计数据已更新，并按 debounce 策略决定是否立即落盘
    fn save_stats_debounced(&self) {
        self.stats_dirty.store(true, Ordering::Relaxed);

        let should_flush = {
            let last = *self.last_stats_save_at.lock();
            match last {
                Some(last_saved_at) => last_saved_at.elapsed() >= STATS_SAVE_DEBOUNCE,
                None => true,
            }
        };

        if should_flush {
            self.save_stats();
        }
    }

    /// 报告指定凭据 API 调用成功
    ///
    /// 重置该凭据的失败计数
    ///
    /// # Arguments
    /// * `id` - 凭据 ID（来自 CallContext）
    pub fn report_success(&self, id: u64) {
        // 重置 MODEL_TEMPORARILY_UNAVAILABLE 计数器
        self.model_unavailable_count.store(0, Ordering::SeqCst);

        // 记录使用次数（用于动态 TTL）
        self.record_usage(id);

        {
            let mut entries = self.entries.lock();
            if let Some(entry) = entries.iter_mut().find(|e| e.id == id) {
                entry.failure_count = 0;
                entry.success_count += 1;
                entry.last_used_at = Some(Utc::now().to_rfc3339());
                tracing::debug!(
                    "凭据 #{} API 调用成功（累计 {} 次）",
                    id,
                    entry.success_count
                );
            }
        }
        self.save_stats_debounced();
    }

    /// 报告指定凭据 API 调用失败
    ///
    /// 增加失败计数，达到阈值时禁用凭据
    /// 返回是否还有可用凭据可以重试
    ///
    /// # Arguments
    /// * `id` - 凭据 ID（来自 CallContext）
    pub fn report_failure(&self, id: u64) -> bool {
        let result = {
            let mut entries = self.entries.lock();

            let entry = match entries.iter_mut().find(|e| e.id == id) {
                Some(e) => e,
                None => return entries.iter().any(|e| !e.disabled),
            };

            entry.failure_count += 1;
            entry.last_used_at = Some(Utc::now().to_rfc3339());
            let failure_count = entry.failure_count;

            tracing::warn!(
                "凭据 #{} API 调用失败（{}/{}）",
                id,
                failure_count,
                MAX_FAILURES_PER_CREDENTIAL
            );

            if failure_count >= MAX_FAILURES_PER_CREDENTIAL {
                entry.disabled = true;
                entry.auto_heal_reason = Some(AutoHealReason::TooManyFailures);
                entry.disable_reason = Some(DisableReason::FailureLimit);
                tracing::error!("凭据 #{} 已连续失败 {} 次，已被禁用", id, failure_count);

                // 移除该凭据的亲和性绑定
                drop(entries);
                self.affinity.remove_by_credential(id);

                let entries = self.entries.lock();
                return entries.iter().any(|e| !e.disabled);
            }

            // 检查是否还有可用凭据
            entries.iter().any(|e| !e.disabled)
        };
        self.save_stats_debounced();
        result
    }

    /// 报告指定凭据额度已用尽
    ///
    /// 用于处理 402 Payment Required 且 reason 为 `MONTHLY_REQUEST_COUNT` 的场景：
    /// - 未知/未开启 overage 时立即禁用该凭据（不等待连续失败阈值）
    /// - 已知 overage 已开启时不禁用，避免把基础额度错误当成有效额度上限
    /// - 返回该凭据是否仍可继续参与后续尝试
    pub fn report_quota_exhausted(&self, id: u64) -> bool {
        let result = {
            let mut entries = self.entries.lock();

            let entry = match entries.iter_mut().find(|e| e.id == id) {
                Some(e) => e,
                None => return entries.iter().any(|e| !e.disabled),
            };

            if entry.disabled {
                return entries.iter().any(|e| !e.disabled);
            }

            if entry.overage_enabled == Some(true) {
                entry.last_used_at = Some(Utc::now().to_rfc3339());
                tracing::warn!(
                    "凭据 #{} 返回 MONTHLY_REQUEST_COUNT，但已知 overage 已开启；保留启用状态并等待后续余额刷新校准",
                    id
                );
                return true;
            }

            entry.disabled = true;
            entry.auto_heal_reason = Some(AutoHealReason::QuotaExceeded);
            entry.disable_reason = Some(DisableReason::QuotaExceeded);
            entry.last_used_at = Some(Utc::now().to_rfc3339());
            // 设为阈值，便于在管理面板中直观看到该凭据已不可用
            entry.failure_count = MAX_FAILURES_PER_CREDENTIAL;

            tracing::error!("凭据 #{} 额度已用尽（MONTHLY_REQUEST_COUNT），已被禁用", id);

            entries.iter().any(|e| !e.disabled)
        };
        self.save_stats_debounced();
        result
    }

    /// 报告 MODEL_TEMPORARILY_UNAVAILABLE 错误
    ///
    /// 累计达到阈值后禁用所有凭据，5分钟后自动恢复
    /// 返回是否触发了全局禁用
    pub fn report_model_unavailable(&self) -> bool {
        let count = self.model_unavailable_count.fetch_add(1, Ordering::SeqCst) + 1;
        tracing::warn!(
            "MODEL_TEMPORARILY_UNAVAILABLE 错误（{}/{}）",
            count,
            MODEL_UNAVAILABLE_THRESHOLD
        );

        if count >= MODEL_UNAVAILABLE_THRESHOLD {
            self.disable_all_credentials(DisableReason::ModelUnavailable);
            true
        } else {
            false
        }
    }

    /// 禁用所有凭据
    fn disable_all_credentials(&self, reason: DisableReason) {
        let mut entries = self.entries.lock();
        let mut recovery_time = self.global_recovery_time.lock();

        for entry in entries.iter_mut() {
            if !entry.disabled {
                entry.disabled = true;
                entry.disable_reason = Some(reason);
            }
        }

        // 设置恢复时间
        let recover_at = Utc::now() + Duration::minutes(GLOBAL_DISABLE_RECOVERY_MINUTES);
        *recovery_time = Some(recover_at);

        tracing::error!(
            "所有凭据已被禁用（原因: {:?}），将于 {} 自动恢复",
            reason,
            recover_at.format("%H:%M:%S")
        );
    }

    /// 检查并执行自动恢复
    ///
    /// 如果已到恢复时间，恢复因 ModelUnavailable 禁用的凭据
    /// 余额不足的凭据不会被恢复
    ///
    /// 返回是否执行了恢复
    pub fn check_and_recover(&self) -> bool {
        let should_recover = {
            let recovery_time = self.global_recovery_time.lock();
            recovery_time.map(|t| Utc::now() >= t).unwrap_or(false)
        };

        if !should_recover {
            return false;
        }

        let mut entries = self.entries.lock();
        let mut recovery_time = self.global_recovery_time.lock();
        let mut recovered_count = 0;

        for entry in entries.iter_mut() {
            // 只恢复因 ModelUnavailable 禁用的凭据，余额不足的不恢复
            if entry.disabled && entry.disable_reason == Some(DisableReason::ModelUnavailable) {
                entry.disabled = false;
                entry.disable_reason = None;
                entry.failure_count = 0;
                recovered_count += 1;
            }
        }

        // 重置全局状态
        *recovery_time = None;
        self.model_unavailable_count.store(0, Ordering::SeqCst);

        if recovered_count > 0 {
            tracing::info!("已自动恢复 {} 个凭据", recovered_count);
        }

        recovered_count > 0
    }

    /// 标记凭据为认证失败（如 invalid_grant，不会被自动恢复）
    pub fn mark_authentication_failed(&self, id: u64) {
        let mut entries = self.entries.lock();
        if let Some(entry) = entries.iter_mut().find(|e| e.id == id) {
            entry.disabled = true;
            entry.auto_heal_reason = None;
            entry.disable_reason = Some(DisableReason::AuthenticationFailed);
            tracing::warn!("凭据 #{} 已标记为认证失败", id);
        }
        drop(entries);
        self.affinity.remove_by_credential(id);
    }

    /// 标记凭据为账户暂停（不会被自动恢复）
    pub fn mark_account_suspended(&self, id: u64) {
        let mut entries = self.entries.lock();
        if let Some(entry) = entries.iter_mut().find(|e| e.id == id) {
            entry.disabled = true;
            entry.auto_heal_reason = None;
            entry.disable_reason = Some(DisableReason::AccountSuspended);
            tracing::warn!("凭据 #{} 已标记为账户暂停", id);
        }
        drop(entries);
        self.affinity.remove_by_credential(id);
    }

    /// 标记凭据为余额不足（不会被自动恢复）
    pub fn mark_insufficient_balance(&self, id: u64) {
        let mut entries = self.entries.lock();
        if let Some(entry) = entries.iter_mut().find(|e| e.id == id) {
            entry.disabled = true;
            entry.auto_heal_reason = None; // 清除自愈原因，防止被自愈循环错误恢复
            entry.disable_reason = Some(DisableReason::InsufficientBalance);
            tracing::warn!("凭据 #{} 已标记为余额不足", id);
        }
    }

    /// 返回当前所有未禁用凭据的 id（用于周期 balance 刷新等批量操作）
    pub fn all_enabled_credential_ids(&self) -> Vec<u64> {
        let entries = self.entries.lock();
        entries
            .iter()
            .filter(|e| !e.disabled)
            .map(|e| e.id)
            .collect()
    }

    /// 获取全局恢复时间（用于 Admin API）
    #[allow(dead_code)]
    pub fn get_recovery_time(&self) -> Option<DateTime<Utc>> {
        *self.global_recovery_time.lock()
    }

    /// 获取使用额度信息
    #[allow(dead_code)]
    pub async fn get_usage_limits(&self) -> anyhow::Result<UsageLimitsResponse> {
        let config = self.config.read().clone();
        let ctx = self.acquire_context().await?;
        let proxy = self.proxy.read().clone();
        get_usage_limits(&ctx.credentials, &config, &ctx.token, proxy.as_ref()).await
    }

    /// 初始化所有凭据的余额缓存
    ///
    /// 启动时顺序查询所有凭据的余额，每次间隔 0.5 秒避免触发限流。
    /// 查询失败的凭据会被跳过（保持 initialized: false）。
    ///
    /// # 返回
    /// - 成功初始化的凭据数量
    pub async fn initialize_balances(&self) -> usize {
        let credential_ids: Vec<u64> = {
            let entries = self.entries.lock();
            entries
                .iter()
                .filter(|e| !e.disabled)
                .map(|e| e.id)
                .collect()
        };

        if credential_ids.is_empty() {
            tracing::info!("无可用凭据，跳过余额初始化");
            return 0;
        }

        tracing::info!("正在初始化 {} 个凭据的余额...", credential_ids.len());

        let mut success_count = 0;

        // 顺序查询每个凭据的余额，间隔 0.5 秒避免触发限流
        for (index, &id) in credential_ids.iter().enumerate() {
            match self.get_usage_limits_for(id).await {
                Ok(limits) => {
                    // 计算有效剩余额度；开启 overage 时要把额外额度计入，否则基础额度用完会被误禁用
                    let overage_enabled = limits
                        .overage_enabled_reported()
                        .unwrap_or_else(|| self.is_overage_known_enabled(id));
                    let remaining = limits.remaining_balance_for_overage(overage_enabled);
                    let usage_limit = limits.effective_usage_limit_for_overage(overage_enabled);
                    let overage_cap = limits.overage_cap_for_enabled(overage_enabled);

                    self.update_balance_cache_details(
                        id,
                        remaining,
                        usage_limit,
                        Some(overage_enabled),
                        Some(overage_cap),
                    );

                    // 余额小于 1 时自动禁用凭据
                    if remaining < 1.0 {
                        let mut entries = self.entries.lock();
                        if let Some(entry) = entries.iter_mut().find(|e| e.id == id) {
                            entry.disabled = true;
                            entry.disable_reason = Some(DisableReason::InsufficientBalance);
                            tracing::warn!("凭据 #{} 余额不足 ({:.2})，已自动禁用", id, remaining);
                        }
                    } else {
                        tracing::info!("凭据 #{} 余额初始化成功: {:.2}", id, remaining);
                    }
                    success_count += 1;
                }
                Err(e) => {
                    tracing::warn!("凭据 #{} 余额查询失败: {}", id, e);
                }
            }

            // 非最后一个凭据时，间隔 0.5 秒
            if index < credential_ids.len() - 1 {
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            }
        }

        tracing::info!(
            "余额初始化完成: {}/{} 成功",
            success_count,
            credential_ids.len()
        );

        success_count
    }

    // ========================================================================
    // Admin API 方法
    // ========================================================================

    /// 获取管理器状态快照（用于 Admin API）
    pub fn snapshot(&self) -> ManagerSnapshot {
        let entries = self.entries.lock();
        let available = entries.iter().filter(|e| !e.disabled).count();

        ManagerSnapshot {
            entries: entries
                .iter()
                .map(|e| {
                    // 使用缓存的哈希，如果不存在则计算并缓存
                    let hash = e
                        .refresh_token_hash
                        .clone()
                        .or_else(|| credential_secret_hash(&e.credentials));

                    CredentialEntrySnapshot {
                        id: e.id,
                        priority: e.credentials.priority,
                        disabled: e.disabled,
                        disable_reason: e.disable_reason,
                        failure_count: e.failure_count,
                        refresh_failure_count: e.refresh_failure_count,
                        auth_method: e.credentials.auth_method.as_deref().map(|m| {
                            if m.eq_ignore_ascii_case("builder-id") || m.eq_ignore_ascii_case("iam")
                            {
                                "idc".to_string()
                            } else {
                                m.to_string()
                            }
                        }),
                        has_profile_arn: e.credentials.profile_arn.is_some(),
                        expires_at: e.credentials.expires_at.clone(),
                        refresh_token_hash: hash,
                        email: e.credentials.email.clone(),
                        subscription_title: e.credentials.subscription_title.clone(),
                        success_count: e.success_count,
                        last_used_at: e.last_used_at.clone(),
                        region: e.credentials.region.clone(),
                        api_region: e.credentials.api_region.clone(),
                        endpoint: e.credentials.endpoint.clone(),
                        idp: e.credentials.idp.clone(),
                        proxy_url: e.credentials.proxy_url.clone(),
                        proxy_username: e.credentials.proxy_username.clone(),
                        has_proxy_password: e.credentials.proxy_password.is_some(),
                        overage_enabled: e.overage_enabled,
                        overage_enabling: e.overage_enabling,
                        overage_last_error: e.overage_last_error.clone(),
                    }
                })
                .collect(),
            total: entries.len(),
            available,
        }
    }

    /// 批量导出凭据为可再导入的 JSON（KAM 原生嵌套格式）。
    ///
    /// - `ids` 为空时导出全部；否则仅导出匹配的 id（保持传入顺序）。
    /// - 含真实 refreshToken / clientId / clientSecret / profileArn 等，可被本项目
    ///   的 import-token-json 直接再导入。
    /// - 返回形如 `{ version, exportedAt, accounts: [ { email, machineId, credentials: {...} } ] }`。
    pub fn export_credentials(&self, ids: &[u64]) -> serde_json::Value {
        let entries = self.entries.lock();
        let selected: Vec<&CredentialEntry> = if ids.is_empty() {
            entries.iter().collect()
        } else {
            // 按传入 id 顺序导出，跳过不存在的
            ids.iter()
                .filter_map(|id| entries.iter().find(|e| e.id == *id))
                .collect()
        };

        let accounts: Vec<serde_json::Value> = selected
            .iter()
            .map(|e| {
                let c = &e.credentials;
                // credentials 子对象：仅写非空字段
                let mut cred = serde_json::Map::new();
                let mut put = |k: &str, v: &Option<String>| {
                    if let Some(s) = v.as_ref().filter(|s| !s.is_empty()) {
                        cred.insert(k.to_string(), serde_json::Value::String(s.clone()));
                    }
                };
                put("refreshToken", &c.refresh_token);
                put("accessToken", &c.access_token);
                put("clientId", &c.client_id);
                put("clientSecret", &c.client_secret);
                put("authMethod", &c.auth_method);
                put("provider", &c.provider);
                put("region", &c.region);
                put("apiRegion", &c.api_region);
                put("profileArn", &c.profile_arn);
                put("issuerUrl", &c.issuer_url);
                put("tokenEndpoint", &c.token_endpoint);
                put("scopes", &c.scopes);
                put("expiresAt", &c.expires_at);

                let mut account = serde_json::Map::new();
                account.insert("id".to_string(), serde_json::json!(e.id));
                if let Some(email) = c.email.as_ref().filter(|s| !s.is_empty()) {
                    account.insert("email".to_string(), serde_json::json!(email));
                }
                if let Some(mid) = c.machine_id.as_ref().filter(|s| !s.is_empty()) {
                    account.insert("machineId".to_string(), serde_json::json!(mid));
                }
                if let Some(idp) = c.idp.as_ref().filter(|s| !s.is_empty()) {
                    account.insert("idp".to_string(), serde_json::json!(idp));
                }
                account.insert("priority".to_string(), serde_json::json!(c.priority));
                account.insert("disabled".to_string(), serde_json::json!(e.disabled));
                account.insert(
                    "credentials".to_string(),
                    serde_json::Value::Object(cred),
                );
                serde_json::Value::Object(account)
            })
            .collect();

        let exported_at = chrono::Utc::now().timestamp_millis();
        serde_json::json!({
            "version": "1.1.2",
            "exportedAt": exported_at,
            "source": "kiro-rs",
            "accounts": accounts,
        })
    }

    /// 设置凭据禁用状态（Admin API）
    pub fn set_disabled(&self, id: u64, disabled: bool) -> anyhow::Result<()> {
        {
            let mut entries = self.entries.lock();
            let entry = entries
                .iter_mut()
                .find(|e| e.id == id)
                .ok_or_else(|| anyhow::anyhow!("凭据不存在: {}", id))?;
            entry.disabled = disabled;
            if !disabled {
                // 启用时重置失败计数
                entry.failure_count = 0;
                entry.auto_heal_reason = None;
                entry.disable_reason = None;
            } else {
                entry.auto_heal_reason = Some(AutoHealReason::Manual);
                entry.disable_reason = Some(DisableReason::Manual);
            }
        }
        // 持久化更改
        self.persist_credentials()?;
        Ok(())
    }

    /// 设置凭据优先级（Admin API）
    pub fn set_priority(&self, id: u64, priority: u32) -> anyhow::Result<()> {
        {
            let mut entries = self.entries.lock();
            let entry = entries
                .iter_mut()
                .find(|e| e.id == id)
                .ok_or_else(|| anyhow::anyhow!("凭据不存在: {}", id))?;
            entry.credentials.priority = priority;
        }
        // 持久化更改
        self.persist_credentials()?;
        Ok(())
    }

    /// 设置凭据 Region（Admin API）
    pub fn set_region(
        &self,
        id: u64,
        region: Option<String>,
        api_region: Option<String>,
    ) -> anyhow::Result<()> {
        {
            let mut entries = self.entries.lock();
            let entry = entries
                .iter_mut()
                .find(|e| e.id == id)
                .ok_or_else(|| anyhow::anyhow!("凭据不存在: {}", id))?;
            entry.credentials.region = region;
            entry.credentials.api_region = api_region;
        }
        self.persist_credentials()?;
        Ok(())
    }

    /// 缓存惰性解析出的 profileArn（external_idp 首次使用时由 ListAvailableProfiles 解析）。
    /// 写入运行时凭据并持久化，使后续请求零往返直接命中。
    pub fn set_profile_arn_for(&self, id: u64, profile_arn: String) -> anyhow::Result<()> {
        {
            let mut entries = self.entries.lock();
            let entry = entries
                .iter_mut()
                .find(|e| e.id == id)
                .ok_or_else(|| anyhow::anyhow!("凭据不存在: {}", id))?;
            entry.credentials.profile_arn = Some(profile_arn);
        }
        self.persist_credentials()?;
        Ok(())
    }

    /// 设置凭据 endpoint（Admin API）
    pub fn set_endpoint(&self, id: u64, endpoint: Option<String>) -> anyhow::Result<()> {
        {
            let mut entries = self.entries.lock();
            let entry = entries
                .iter_mut()
                .find(|e| e.id == id)
                .ok_or_else(|| anyhow::anyhow!("凭据不存在: {}", id))?;
            entry.credentials.endpoint = endpoint;
        }
        self.persist_credentials()?;
        Ok(())
    }

    /// 设置凭据级 Web Portal Idp（Admin API）
    pub fn set_idp_for(&self, id: u64, idp: Option<String>) -> anyhow::Result<()> {
        {
            let mut entries = self.entries.lock();
            let entry = entries
                .iter_mut()
                .find(|e| e.id == id)
                .ok_or_else(|| anyhow::anyhow!("凭据不存在: {}", id))?;
            entry.credentials.idp = idp.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
        }
        self.persist_credentials()?;
        Ok(())
    }

    /// 设置凭据级代理（Admin API）
    ///
    /// `proxy_url == Some("")` 视为清除代理回退到全局；`Some("direct")` 表示显式直连。
    pub fn set_proxy_for(
        &self,
        id: u64,
        proxy_url: Option<String>,
        proxy_username: Option<String>,
        proxy_password: Option<String>,
    ) -> anyhow::Result<()> {
        {
            let mut entries = self.entries.lock();
            let entry = entries
                .iter_mut()
                .find(|e| e.id == id)
                .ok_or_else(|| anyhow::anyhow!("凭据不存在: {}", id))?;
            entry.credentials.proxy_url = proxy_url
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty());
            entry.credentials.proxy_username = proxy_username
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty());
            entry.credentials.proxy_password = proxy_password.filter(|s| !s.is_empty());
        }
        self.persist_credentials()?;
        Ok(())
    }

    /// 读取指定凭据的超额状态缓存
    pub fn overage_status_for(&self, id: u64) -> anyhow::Result<OverageStatusSnapshot> {
        let entries = self.entries.lock();
        let entry = entries
            .iter()
            .find(|e| e.id == id)
            .ok_or_else(|| anyhow::anyhow!("凭据不存在: {}", id))?;
        Ok(OverageStatusSnapshot {
            id: entry.id,
            enabled: entry.overage_enabled,
            enabling: entry.overage_enabling,
            last_error: entry.overage_last_error.clone(),
            has_profile_arn: entry.credentials.profile_arn.is_some(),
            auth_method: entry.credentials.auth_method.clone(),
        })
    }

    /// 指定凭据是否已知开启 overage。
    ///
    /// 只把 `Some(true)` 视为开启；`None` 表示还没查询到 Web Portal 状态，
    /// 不能主动推断为关闭。
    pub fn is_overage_known_enabled(&self, id: u64) -> bool {
        self.overage_status_for(id)
            .ok()
            .and_then(|s| s.enabled)
            .unwrap_or(false)
    }

    /// 标记凭据正在执行 overage 开启任务（防止并发重复触发）
    ///
    /// 返回 `true` 表示成功获得"独占执行权"，`false` 表示已经有任务在跑。
    pub fn try_begin_overage_task(&self, id: u64) -> anyhow::Result<bool> {
        let mut entries = self.entries.lock();
        let entry = entries
            .iter_mut()
            .find(|e| e.id == id)
            .ok_or_else(|| anyhow::anyhow!("凭据不存在: {}", id))?;
        if entry.overage_enabling {
            return Ok(false);
        }
        entry.overage_enabling = true;
        entry.overage_last_error = None;
        Ok(true)
    }

    /// 任务结束时回填结果（成功/失败）
    pub fn finish_overage_task(&self, id: u64, result: Result<bool, String>) {
        let mut entries = self.entries.lock();
        if let Some(entry) = entries.iter_mut().find(|e| e.id == id) {
            entry.overage_enabling = false;
            let mut should_persist = false;
            match result {
                Ok(enabled) => {
                    entry.overage_enabled = Some(enabled);
                    entry.credentials.overage_enabled = Some(enabled);
                    entry.overage_last_error = None;
                    should_persist = true;
                }
                Err(err) => {
                    entry.overage_last_error = Some(err);
                }
            }
            drop(entries);
            if should_persist {
                if let Err(e) = self.persist_credentials() {
                    tracing::warn!("Overage 状态持久化失败（不影响本次状态更新）: {}", e);
                }
            }
        }
    }

    /// 仅刷新 overage_enabled 状态（不影响 enabling/last_error 标志）
    pub fn record_overage_status(&self, id: u64, enabled: bool) {
        let mut should_persist = false;
        let mut entries = self.entries.lock();
        if let Some(entry) = entries.iter_mut().find(|e| e.id == id) {
            entry.overage_enabled = Some(enabled);
            if entry.credentials.overage_enabled != Some(enabled) {
                entry.credentials.overage_enabled = Some(enabled);
                should_persist = true;
            }
        }
        drop(entries);
        if should_persist {
            if let Err(e) = self.persist_credentials() {
                tracing::warn!("Overage 状态持久化失败（不影响本次状态更新）: {}", e);
            }
        }
    }

    /// 获取指定凭据的 web portal 调用上下文（access_token + idp + profile_arn + 代理）
    ///
    /// 内部确保 token 有效（必要时刷新），仅供 web portal API 使用，
    /// 不影响 acquire_context 的负载均衡选择。
    pub async fn web_portal_context_for(&self, id: u64) -> anyhow::Result<WebPortalContext> {
        let config = self.config.read().clone();
        let credentials = {
            let entries = self.entries.lock();
            entries
                .iter()
                .find(|e| e.id == id)
                .map(|e| e.credentials.clone())
                .ok_or_else(|| anyhow::anyhow!("凭据不存在: {}", id))?
        };

        if credentials.is_api_key_credential() {
            anyhow::bail!("API Key 凭据不支持 Web Portal 接口");
        }

        let needs_refresh = is_token_expired(&credentials) || is_token_expiring_soon(&credentials);
        let final_creds = if needs_refresh {
            let _guard = self.refresh_lock.lock().await;
            let current = {
                let entries = self.entries.lock();
                entries
                    .iter()
                    .find(|e| e.id == id)
                    .map(|e| e.credentials.clone())
                    .ok_or_else(|| anyhow::anyhow!("凭据不存在: {}", id))?
            };
            if is_token_expired(&current) || is_token_expiring_soon(&current) {
                let proxy = self.proxy.read().clone();
                let new_creds = refresh_token_with_id(&current, &config, proxy.as_ref(), id)
                    .await
                    .inspect_err(|err| {
                        if is_invalid_grant_error(err) {
                            self.mark_authentication_failed(id);
                        }
                    })?;
                {
                    let mut entries = self.entries.lock();
                    if let Some(entry) = entries.iter_mut().find(|e| e.id == id) {
                        entry.credentials = new_creds.clone();
                        entry.refresh_token_hash = credential_secret_hash(&new_creds);
                    }
                }
                if let Err(e) = self.persist_credentials() {
                    tracing::warn!("Token 刷新后持久化失败（不影响本次请求）: {}", e);
                }
                new_creds
            } else {
                current
            }
        } else {
            credentials
        };

        let token = final_creds
            .access_token
            .clone()
            .ok_or_else(|| anyhow::anyhow!("凭据无 access_token"))?;
        let profile_arn = final_creds
            .profile_arn
            .clone()
            .filter(|s| !s.trim().is_empty());
        let idp = final_creds.effective_idp().to_string();
        if idp.is_empty() {
            anyhow::bail!(
                "凭据未提供 idp 且无法从 auth_method 推断（仅 social 凭据支持 Web Portal）"
            );
        }
        let proxy = final_creds.effective_proxy(self.proxy.read().as_ref());
        Ok(WebPortalContext {
            id,
            token,
            idp,
            profile_arn,
            proxy,
        })
    }

    /// 重置凭据失败计数并重新启用（Admin API）
    pub fn reset_and_enable(&self, id: u64) -> anyhow::Result<()> {
        {
            let mut entries = self.entries.lock();
            let entry = entries
                .iter_mut()
                .find(|e| e.id == id)
                .ok_or_else(|| anyhow::anyhow!("凭据不存在: {}", id))?;
            entry.failure_count = 0;
            entry.refresh_failure_count = 0;
            entry.disabled = false;
            entry.auto_heal_reason = None;
            entry.disable_reason = None;
        }
        // 持久化更改
        self.persist_credentials()?;
        Ok(())
    }

    /// 强制刷新指定凭据的 Token（Admin API）
    pub async fn force_refresh_token_for(&self, id: u64) -> anyhow::Result<()> {
        let config = self.config.read().clone();
        let credentials = {
            let entries = self.entries.lock();
            entries
                .iter()
                .find(|e| e.id == id)
                .map(|e| e.credentials.clone())
                .ok_or_else(|| anyhow::anyhow!("凭据不存在: {}", id))?
        };

        // 持有刷新锁，避免与业务请求自动刷新并发
        let _guard = self.refresh_lock.lock().await;

        if credentials.is_api_key_credential() {
            anyhow::bail!("API Key 凭据无需刷新 Token");
        }

        let proxy = self.proxy.read().clone();
        let new_creds = refresh_token_with_id(&credentials, &config, proxy.as_ref(), id).await?;

        {
            let mut entries = self.entries.lock();
            if let Some(entry) = entries.iter_mut().find(|e| e.id == id) {
                entry.credentials = new_creds.clone();
                entry.refresh_failure_count = 0;
                entry.refresh_token_hash = credential_secret_hash(&new_creds);

                // 仅对自动禁用（失败阈值/刷新失败）自动恢复，手动禁用状态保持不变
                if entry.disabled && entry.disable_reason != Some(DisableReason::Manual) {
                    entry.failure_count = 0;
                    entry.disabled = false;
                    entry.auto_heal_reason = None;
                    entry.disable_reason = None;
                }
            }
        }

        self.persist_credentials()?;
        Ok(())
    }

    /// 获取指定凭据的使用额度（Admin API）
    pub async fn get_usage_limits_for(&self, id: u64) -> anyhow::Result<UsageLimitsResponse> {
        let config = self.config.read().clone();
        let credentials = {
            let entries = self.entries.lock();
            entries
                .iter()
                .find(|e| e.id == id)
                .map(|e| e.credentials.clone())
                .ok_or_else(|| anyhow::anyhow!("凭据不存在: {}", id))?
        };

        // 检查是否需要刷新 token
        let needs_refresh = is_token_expired(&credentials) || is_token_expiring_soon(&credentials);

        let token =
            if credentials.is_api_key_credential() {
                credentials
                    .kiro_api_key
                    .clone()
                    .ok_or_else(|| anyhow::anyhow!("凭据无 kiroApiKey"))?
            } else if needs_refresh {
                let _guard = self.refresh_lock.lock().await;
                let current_creds = {
                    let entries = self.entries.lock();
                    entries
                        .iter()
                        .find(|e| e.id == id)
                        .map(|e| e.credentials.clone())
                        .ok_or_else(|| anyhow::anyhow!("凭据不存在: {}", id))?
                };

                if is_token_expired(&current_creds) || is_token_expiring_soon(&current_creds) {
                    let proxy = self.proxy.read().clone();
                    let new_creds =
                        match refresh_token_with_id(&current_creds, &config, proxy.as_ref(), id)
                            .await
                        {
                            Ok(creds) => creds,
                            Err(err) => {
                                if is_invalid_grant_error(&err) {
                                    self.mark_authentication_failed(id);
                                }
                                return Err(err);
                            }
                        };
                    {
                        let mut entries = self.entries.lock();
                        if let Some(entry) = entries.iter_mut().find(|e| e.id == id) {
                            entry.credentials = new_creds.clone();
                            // 更新哈希缓存
                            entry.refresh_token_hash = credential_secret_hash(&new_creds);
                        }
                    }
                    // 持久化失败只记录警告，不影响本次请求
                    if let Err(e) = self.persist_credentials() {
                        tracing::warn!("Token 刷新后持久化失败（不影响本次请求）: {}", e);
                    }
                    new_creds
                        .access_token
                        .ok_or_else(|| anyhow::anyhow!("刷新后无 access_token"))?
                } else {
                    current_creds
                        .access_token
                        .ok_or_else(|| anyhow::anyhow!("凭据无 access_token"))?
                }
            } else {
                credentials
                    .access_token
                    .ok_or_else(|| anyhow::anyhow!("凭据无 access_token"))?
            };

        let mut credentials = {
            let entries = self.entries.lock();
            entries
                .iter()
                .find(|e| e.id == id)
                .map(|e| e.credentials.clone())
                .ok_or_else(|| anyhow::anyhow!("凭据不存在: {}", id))?
        };

        let proxy = self.proxy.read().clone();
        let config = self.config.read().clone();

        // IdC / Enterprise 账号：额度端点强制要求 profileArn，缺失会 403。
        // 首次遇到时通过 ListAvailableProfiles 解析真实 ARN，存回凭据并持久化，
        // 后续请求直接复用（对齐 chaogei/Kiro-account-manager 参考实现）。
        if credentials.profile_arn.is_none() && is_idc_credential(&credentials) {
            match fetch_idc_profile_arn(&credentials, &config, &token, proxy.as_ref()).await {
                IdcProfileResolution::Resolved(arn) => {
                    credentials.profile_arn = Some(arn.clone());
                    let mut should_persist = false;
                    {
                        let mut entries = self.entries.lock();
                        if let Some(entry) = entries.iter_mut().find(|e| e.id == id) {
                            if entry.credentials.profile_arn.as_deref() != Some(arn.as_str()) {
                                entry.credentials.profile_arn = Some(arn);
                                should_persist = true;
                            }
                        }
                    }
                    if should_persist {
                        if let Err(e) = self.persist_credentials() {
                            tracing::warn!("IdC profileArn 解析后持久化失败（不影响本次请求）: {}", e);
                        }
                    }
                }
                IdcProfileResolution::NoProfile => {
                    // 账号没有任何可用 profile（token 有效但未分配 Q Developer/Kiro 应用）。
                    // 直接判定该凭证不可用，让上层验活流程标记并给出明确原因。
                    anyhow::bail!(
                        "该账号无可用的 CodeWhisperer/Kiro profile（未开通 Q Developer 或未分配应用），无法使用"
                    );
                }
                IdcProfileResolution::Unavailable => {
                    // 临时错误：不改状态，继续尝试（下面 get_usage_limits 大概率也会失败并按原逻辑处理）。
                    tracing::debug!("凭据 #{} profileArn 解析暂时不可用，保持缺失稍后重试", id);
                }
            }
        }

        match get_usage_limits(&credentials, &config, &token, proxy.as_ref()).await {
            Ok(usage) => {
                let subscription_title = usage.subscription_title().map(|title| title.to_string());
                let reported_overage_enabled = usage.overage_enabled_reported();
                let mut should_persist = false;

                {
                    let mut entries = self.entries.lock();
                    if let Some(entry) = entries.iter_mut().find(|e| e.id == id) {
                        if let Some(subscription_title) = subscription_title.as_deref() {
                            if entry.credentials.subscription_title.as_deref()
                                != Some(subscription_title)
                            {
                                entry.credentials.subscription_title =
                                    Some(subscription_title.to_string());
                                should_persist = true;
                            }
                        }

                        if let Some(enabled) = reported_overage_enabled {
                            entry.overage_enabled = Some(enabled);
                            if entry.credentials.overage_enabled != Some(enabled) {
                                entry.credentials.overage_enabled = Some(enabled);
                                should_persist = true;
                            }
                        }
                    }
                }

                if should_persist {
                    if let Err(e) = self.persist_credentials() {
                        tracing::warn!("凭据用量状态更新后持久化失败（不影响本次请求）: {}", e);
                    }
                }

                Ok(usage)
            }
            Err(err) => {
                if is_invalid_grant_error(&err) {
                    self.mark_authentication_failed(id);
                } else if is_temporarily_suspended_error(&err) {
                    self.mark_account_suspended(id);
                }
                Err(err)
            }
        }
    }

    /// 添加新凭据（Admin API）
    ///
    /// # 流程
    /// 1. 验证凭据基本字段（refresh_token 不为空）
    /// 2. 基于 refreshToken 的 SHA-256 哈希检测重复
    /// 3. 尝试刷新 Token 验证凭据有效性
    /// 4. 分配新 ID（当前最大 ID + 1）
    /// 5. 添加到 entries 列表
    /// 6. 持久化到配置文件
    ///
    /// # 返回
    /// - `Ok(u64)` - 新凭据 ID
    /// - `Err(_)` - 验证失败或添加失败
    pub async fn add_credential(&self, new_cred: KiroCredentials) -> anyhow::Result<u64> {
        let config = self.config.read().clone();
        // 1. 基本验证
        validate_credential_secret(&new_cred)?;

        // 2. 基于 refreshToken / API Key 的 SHA-256 哈希检测重复
        let new_secret_hash = credential_secret_hash(&new_cred)
            .ok_or_else(|| anyhow::anyhow!("缺少 refreshToken 或 kiroApiKey"))?;
        let duplicate_exists = {
            let entries = self.entries.lock();
            entries.iter().any(|entry| {
                let hash = entry
                    .refresh_token_hash
                    .clone()
                    .or_else(|| credential_secret_hash(&entry.credentials));
                hash.as_deref() == Some(new_secret_hash.as_str())
            })
        };
        if duplicate_exists {
            anyhow::bail!("凭据已存在（refreshToken 或 kiroApiKey 重复）");
        }

        // 3. 尝试验证凭据有效性
        let proxy = self.proxy.read().clone();
        let mut validated_cred = if new_cred.is_api_key_credential() {
            let token = new_cred
                .kiro_api_key
                .clone()
                .ok_or_else(|| anyhow::anyhow!("缺少 kiroApiKey"))?;
            let usage = get_usage_limits(&new_cred, &config, &token, proxy.as_ref()).await?;
            let mut cred = new_cred.clone();
            cred.access_token = None;
            cred.expires_at = None;
            cred.subscription_title = usage.subscription_title().map(|s| s.to_string());
            cred
        } else {
            refresh_token(&new_cred, &config, proxy.as_ref()).await?
        };

        // 4. 分配新 ID
        let new_id = {
            let entries = self.entries.lock();
            entries.iter().map(|e| e.id).max().unwrap_or(0) + 1
        };

        // 5. 设置 ID 并保留用户输入的元数据
        validated_cred.id = Some(new_id);
        validated_cred.priority = new_cred.priority;
        validated_cred.auth_method = new_cred.auth_method.clone();
        validated_cred.canonicalize_auth_method();
        validated_cred.client_id = new_cred.client_id;
        validated_cred.client_secret = new_cred.client_secret;
        validated_cred.region = new_cred.region;
        // 机器码：优先用导入/登录时携带的 machineId（如账号管理器导出的原生 UUID）；
        // 缺失时（如 SSO 登录路径）随机生成一个固定 UUID 并持久化，避免退化到
        // 「用 refreshToken 派生」——refreshToken 每次刷新都会变，会导致设备指纹漂移。
        validated_cred.machine_id = new_cred
            .machine_id
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .or_else(|| Some(uuid::Uuid::new_v4().to_string()));
        validated_cred.email = new_cred.email;
        validated_cred.api_region = new_cred.api_region;
        validated_cred.proxy_url = new_cred.proxy_url;
        validated_cred.proxy_username = new_cred.proxy_username;
        validated_cred.proxy_password = new_cred.proxy_password;
        validated_cred.kiro_api_key = new_cred.kiro_api_key;

        // 为新凭据生成设备指纹
        let fingerprint_seed = validated_cred
            .refresh_token
            .as_deref()
            .or(validated_cred.kiro_api_key.as_deref())
            .or(validated_cred.machine_id.as_deref())
            .map(|s| s.to_string())
            .unwrap_or_else(|| format!("credential-{}", new_id));
        let fingerprint = Fingerprint::generate_from_seed(&fingerprint_seed);

        let entry_secret_hash = credential_secret_hash(&validated_cred);

        {
            let mut entries = self.entries.lock();
            entries.push(CredentialEntry {
                id: new_id,
                credentials: validated_cred,
                failure_count: 0,
                refresh_failure_count: 0,
                disabled: false,
                auto_heal_reason: None,
                disable_reason: None,
                fingerprint,
                success_count: 0,
                last_used_at: None,
                refresh_token_hash: entry_secret_hash,
                overage_enabled: None,
                overage_enabling: false,
                overage_last_error: None,
            });
        }

        // P0#2 修复：立刻把新凭据写进 balance_cache。
        //
        // 雷暴避免设计：select_best_candidate_id 第一优先级是 `min(recent_usage)`。
        // 中位数 baseline 会让新凭据永远大于一半凭据的 recent_usage → 永远到不了候选 →
        // 永远不会被首次调用 → balance 永远不刷新 → 死锁（G/I 双 agent 共识）。
        //
        // 正确策略：用现有凭据中的 **max**(recent_usage) 作为 baseline + 取
        // 现有凭据 balance 中位数作为 remaining，让新凭据：
        //   1) recent_usage = max → 排在所有现有凭据之后，**不雷暴**
        //   2) 等老凭据的 usage 涨到等于这个 max 时，新凭据跟它们 tie
        //   3) tie-break 看 balance（已设为中位数 > 0），新凭据**能竞争**
        //   4) 自然轮入 LB，避免 0→47 次 429 突袭
        {
            let mut cache = self.balance_cache.lock();
            let now = std::time::Instant::now();
            let existing: Vec<(u32, f64)> = cache
                .iter()
                .filter(|(id, _)| **id != new_id)
                .map(|(_, e)| (e.recent_usage, e.remaining))
                .collect();
            let baseline_usage = existing.iter().map(|(u, _)| *u).max().unwrap_or(0);
            let mut balances: Vec<f64> = existing.iter().map(|(_, b)| *b).collect();
            balances.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            let baseline_balance = if balances.is_empty() {
                0.0
            } else {
                balances[balances.len() / 2]
            };
            cache.insert(
                new_id,
                CachedBalance {
                    remaining: baseline_balance,
                    usage_limit: 0.0,
                    overage_enabled: false,
                    overage_cap: 0.0,
                    cached_at: now,
                    initialized: true,
                    recent_usage: baseline_usage,
                    usage_reset_at: now,
                },
            );
            tracing::info!(
                "凭据 #{} 已加入 balance_cache: recent_usage={}（max baseline）, remaining={:.2}（中位数）",
                new_id,
                baseline_usage,
                baseline_balance
            );
        }

        // 6. 持久化
        self.persist_credentials()?;

        tracing::info!("成功添加凭据 #{}", new_id);
        Ok(new_id)
    }

    /// 删除凭据（Admin API）
    ///
    /// # 前置条件
    /// - 凭据必须已禁用（disabled = true）
    ///
    /// # 行为
    /// 1. 验证凭据存在
    /// 2. 验证凭据已禁用
    /// 3. 从 entries 移除
    /// 4. 持久化到文件
    ///
    /// # 返回
    /// - `Ok(())` - 删除成功
    /// - `Err(_)` - 凭据不存在、未禁用或持久化失败
    pub fn delete_credential(&self, id: u64) -> anyhow::Result<()> {
        {
            let mut entries = self.entries.lock();

            // 查找凭据
            let entry = entries
                .iter()
                .find(|e| e.id == id)
                .ok_or_else(|| anyhow::anyhow!("凭据不存在: {}", id))?;

            // 检查是否已禁用
            if !entry.disabled {
                anyhow::bail!("只能删除已禁用的凭据（请先禁用凭据 #{}）", id);
            }

            // 删除凭据
            entries.retain(|e| e.id != id);
        }

        // 持久化更改
        self.persist_credentials()?;

        // 立即回写统计数据，清除已删除凭据的残留条目
        self.save_stats();

        tracing::info!("已删除凭据 #{}", id);
        Ok(())
    }

    /// 检查是否存在具有相同 refreshToken 的凭据（用于批量导入去重）。
    ///
    /// 必须比较**完整** refreshToken：Azure AD / external_idp（甚至同租户 IdC）签发的
    /// refreshToken 有很长的公共固定前缀（如 `1.AUEB...` 头部数十字符都相同），
    /// 仅比前缀会把不同账号误判为重复。此处用完整字符串（常量时间比较无必要，
    /// 但用 == 完整比对即可保证正确性）。
    pub fn has_refresh_token_prefix(&self, refresh_token: &str) -> bool {
        let entries = self.entries.lock();
        entries.iter().any(|e| {
            e.credentials
                .refresh_token
                .as_deref()
                .map(|rt| rt == refresh_token)
                .unwrap_or(false)
        })
    }

    /// 检查是否存在具有相同 kiroApiKey 的凭据（api_key 批量导入去重，完整比对）。
    pub fn has_api_key(&self, api_key: &str) -> bool {
        let entries = self.entries.lock();
        entries.iter().any(|e| {
            e.credentials
                .kiro_api_key
                .as_deref()
                .map(|k| k == api_key)
                .unwrap_or(false)
        })
    }

    // ========================================================================
    // 增强特性：设备指纹、速率限制、冷却管理、后台刷新
    // ========================================================================

    #[allow(dead_code)]
    /// 获取凭据的设备指纹
    pub fn get_fingerprint(&self, id: u64) -> Option<Fingerprint> {
        let entries = self.entries.lock();
        entries
            .iter()
            .find(|e| e.id == id)
            .map(|e| e.fingerprint.clone())
    }

    #[allow(dead_code)]
    /// 获取速率限制器引用
    pub fn rate_limiter(&self) -> &RateLimiter {
        &self.rate_limiter
    }

    /// 获取冷却管理器引用
    #[allow(dead_code)]
    pub fn cooldown_manager(&self) -> &CooldownManager {
        &self.cooldown_manager
    }

    /// 检查凭据是否可用（综合检查：未禁用、未冷却、未超速率限制）
    #[allow(dead_code)]
    pub fn is_credential_available(&self, id: u64) -> bool {
        // 检查是否禁用
        let is_disabled = {
            let entries = self.entries.lock();
            entries
                .iter()
                .find(|e| e.id == id)
                .map(|e| e.disabled)
                .unwrap_or(true)
        };
        if is_disabled {
            return false;
        }

        // 检查冷却状态
        if !self.cooldown_manager.is_available(id) {
            return false;
        }

        // 检查速率限制
        self.rate_limiter.check_rate_limit(id).is_ok()
    }

    /// 设置凭据冷却（带原因分类）
    #[allow(dead_code)]
    pub fn set_credential_cooldown(&self, id: u64, reason: CooldownReason) -> std::time::Duration {
        self.cooldown_manager.set_cooldown(id, reason)
    }

    /// 设置凭据冷却（支持自定义时长）
    #[allow(dead_code)]
    pub fn set_credential_cooldown_with_duration(
        &self,
        id: u64,
        reason: CooldownReason,
        duration: Option<std::time::Duration>,
    ) -> std::time::Duration {
        {
            let mut entries = self.entries.lock();
            if let Some(entry) = entries.iter_mut().find(|e| e.id == id) {
                entry.last_used_at = Some(Utc::now().to_rfc3339());
            }
        }
        self.save_stats_debounced();
        self.cooldown_manager
            .set_cooldown_with_duration(id, reason, duration)
    }

    /// 清除凭据冷却
    #[allow(dead_code)]
    pub fn clear_credential_cooldown(&self, id: u64) -> bool {
        self.cooldown_manager.clear_cooldown(id)
    }

    /// 获取即将过期的凭据 ID 列表
    ///
    /// # Arguments
    /// * `minutes_before_expiry` - 过期前多少分钟视为即将过期
    #[allow(dead_code)]
    pub fn get_expiring_credential_ids(&self, minutes_before_expiry: i64) -> Vec<u64> {
        let entries = self.entries.lock();
        entries
            .iter()
            .filter(|e| {
                !e.disabled
                    && is_token_expiring_within(&e.credentials, minutes_before_expiry)
                        .unwrap_or(false)
            })
            .map(|e| e.id)
            .collect()
    }

    /// 启动后台 Token 刷新任务
    ///
    /// 定期检查并预刷新即将过期的 Token，避免请求时的刷新延迟。
    /// 返回 `BackgroundRefresher` 的 `Arc` 引用，调用方需要保持该引用以维持后台任务运行。
    #[allow(dead_code)]
    pub fn start_background_refresh(
        self: &Arc<Self>,
        config: BackgroundRefreshConfig,
    ) -> Arc<BackgroundRefresher> {
        let refresher = Arc::new(BackgroundRefresher::new(config.clone()));
        let manager = Arc::clone(self);
        let manager_for_ids = Arc::clone(self);

        let refresh_before_mins = config.refresh_before_expiry_mins;

        if let Err(e) = refresher.start(
            move |id| {
                let manager = Arc::clone(&manager);
                Box::pin(async move {
                    match manager.refresh_token_for_credential(id).await {
                        Ok(_) => {
                            tracing::debug!("后台刷新凭据 #{} Token 成功", id);
                            true
                        }
                        Err(e) => {
                            tracing::warn!("后台刷新凭据 #{} Token 失败: {}", id, e);
                            false
                        }
                    }
                })
            },
            move |mins| manager_for_ids.get_expiring_credential_ids(mins.max(refresh_before_mins)),
        ) {
            tracing::error!("启动后台刷新任务失败: {}", e);
        }

        tracing::info!("后台 Token 刷新任务已启动");
        refresher
    }

    /// 刷新指定凭据的 Token（带优雅降级）
    ///
    /// 如果刷新失败但现有 Token 仍有效，返回现有 Token（优雅降级）
    #[allow(dead_code)]
    pub async fn refresh_token_for_credential(&self, id: u64) -> anyhow::Result<RefreshResult> {
        let config = self.config.read().clone();
        let credentials = {
            let entries = self.entries.lock();
            entries
                .iter()
                .find(|e| e.id == id)
                .map(|e| e.credentials.clone())
                .ok_or_else(|| anyhow::anyhow!("凭据不存在: {}", id))?
        };

        // 尝试刷新
        let proxy = self.proxy.read().clone();
        match refresh_token_with_id(&credentials, &config, proxy.as_ref(), id).await {
            Ok(new_creds) => {
                // 更新凭据
                {
                    let mut entries = self.entries.lock();
                    if let Some(entry) = entries.iter_mut().find(|e| e.id == id) {
                        entry.credentials = new_creds.clone();
                        // 更新哈希缓存
                        entry.refresh_token_hash =
                            new_creds.refresh_token.as_deref().map(sha256_hex);
                    }
                }

                // 持久化
                if let Err(e) = self.persist_credentials() {
                    tracing::warn!("Token 刷新后持久化失败: {}", e);
                }

                let expires_at = new_creds.expires_at.unwrap_or_default();
                Ok(RefreshResult::success(id, expires_at))
            }
            Err(e) => {
                // 优雅降级：检查现有 Token 是否仍有效
                if !is_token_expired(&credentials) {
                    let expires_at = credentials.expires_at.unwrap_or_default();
                    tracing::warn!(
                        "凭据 #{} Token 刷新失败，使用现有 Token（优雅降级）: {}",
                        id,
                        e
                    );
                    Ok(RefreshResult::fallback(id, expires_at))
                } else {
                    // 设置冷却
                    self.cooldown_manager
                        .set_cooldown(id, CooldownReason::TokenRefreshFailed);
                    Err(e)
                }
            }
        }
    }

    /// 记录 API 调用成功（更新速率限制器）
    #[allow(dead_code)]
    pub fn record_api_success(&self, id: u64) {
        self.report_success(id);
        self.rate_limiter.record_success(id);
    }

    /// 记录 API 调用失败（更新速率限制器和冷却管理器）
    #[allow(dead_code)]
    pub fn record_api_failure(&self, id: u64, error_message: Option<&str>) -> bool {
        let has_available = self.report_failure(id);

        // 更新速率限制器
        let backoff = self.rate_limiter.record_failure(id, error_message);
        tracing::debug!("凭据 #{} 退避时间: {:?}", id, backoff);

        has_available
    }

    /// 清理过期的冷却状态
    #[allow(dead_code)]
    pub fn cleanup_expired_cooldowns(&self) -> usize {
        self.cooldown_manager.cleanup_expired()
    }
}

impl Drop for MultiTokenManager {
    fn drop(&mut self) {
        if self.stats_dirty.load(Ordering::Relaxed) {
            self.save_stats();
        }
    }
}

#[cfg(test)]
#[allow(clippy::field_reassign_with_default)]
mod tests {
    use super::*;

    #[test]
    fn test_token_manager_new() {
        let config = Config::default();
        let credentials = KiroCredentials::default();
        let tm = TokenManager::new(config, credentials, None);
        assert!(tm.credentials().access_token.is_none());
    }

    #[test]
    fn test_is_token_expired_with_expired_token() {
        let mut credentials = KiroCredentials::default();
        credentials.expires_at = Some("2020-01-01T00:00:00Z".to_string());
        assert!(is_token_expired(&credentials));
    }

    #[test]
    fn test_is_token_expired_with_valid_token() {
        let mut credentials = KiroCredentials::default();
        let future = Utc::now() + Duration::hours(1);
        credentials.expires_at = Some(future.to_rfc3339());
        assert!(!is_token_expired(&credentials));
    }

    #[test]
    fn test_is_token_expired_within_5_minutes() {
        let mut credentials = KiroCredentials::default();
        let expires = Utc::now() + Duration::minutes(3);
        credentials.expires_at = Some(expires.to_rfc3339());
        assert!(is_token_expired(&credentials));
    }

    #[test]
    fn test_is_token_expired_no_expires_at() {
        let credentials = KiroCredentials::default();
        assert!(is_token_expired(&credentials));
    }

    #[test]
    fn test_is_token_expiring_soon_within_10_minutes() {
        let mut credentials = KiroCredentials::default();
        let expires = Utc::now() + Duration::minutes(8);
        credentials.expires_at = Some(expires.to_rfc3339());
        assert!(is_token_expiring_soon(&credentials));
    }

    #[test]
    fn test_is_token_expiring_soon_beyond_10_minutes() {
        let mut credentials = KiroCredentials::default();
        let expires = Utc::now() + Duration::minutes(15);
        credentials.expires_at = Some(expires.to_rfc3339());
        assert!(!is_token_expiring_soon(&credentials));
    }

    #[test]
    fn test_validate_refresh_token_missing() {
        let credentials = KiroCredentials::default();
        let result = validate_refresh_token(&credentials);
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_refresh_token_valid() {
        let mut credentials = KiroCredentials::default();
        credentials.refresh_token = Some("a".repeat(150));
        let result = validate_refresh_token(&credentials);
        assert!(result.is_ok());
    }

    #[test]
    fn test_sha256_hex() {
        let result = sha256_hex("test");
        assert_eq!(
            result,
            "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08"
        );
    }

    #[tokio::test]
    async fn test_add_credential_reject_duplicate_refresh_token() {
        let config = Config::default();

        let mut existing = KiroCredentials::default();
        existing.refresh_token = Some("a".repeat(150));

        let manager = MultiTokenManager::new(config, vec![existing], None, None, false).unwrap();

        let mut duplicate = KiroCredentials::default();
        duplicate.refresh_token = Some("a".repeat(150));

        let result = manager.add_credential(duplicate).await;
        assert!(result.is_err());
        assert!(result.err().unwrap().to_string().contains("凭据已存在"));
    }

    #[test]
    fn test_multi_token_manager_restores_persisted_overage_status() {
        let config = Config::default();
        let cred = KiroCredentials {
            overage_enabled: Some(true),
            ..Default::default()
        };

        let manager = MultiTokenManager::new(config, vec![cred], None, None, false).unwrap();
        let status = manager.overage_status_for(1).unwrap();

        assert_eq!(status.enabled, Some(true));
        assert_eq!(manager.snapshot().entries[0].overage_enabled, Some(true));
    }

    // MultiTokenManager 测试

    #[test]
    fn test_multi_token_manager_new() {
        let config = Config::default();
        let mut cred1 = KiroCredentials::default();
        cred1.priority = 0;
        let mut cred2 = KiroCredentials::default();
        cred2.priority = 1;

        let manager =
            MultiTokenManager::new(config, vec![cred1, cred2], None, None, false).unwrap();
        assert_eq!(manager.total_count(), 2);
        assert_eq!(manager.available_count(), 2);
    }

    #[test]
    fn test_invalidate_access_token_marks_expired() {
        let config = Config::default();
        let mut credentials = KiroCredentials::default();
        credentials.refresh_token = Some("a".repeat(150));
        credentials.access_token = Some("some_token".to_string());
        credentials.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());

        let manager = MultiTokenManager::new(config, vec![credentials], None, None, false).unwrap();
        assert!(manager.invalidate_access_token(1));

        let snapshot = manager.snapshot();
        let entry = snapshot.entries.iter().find(|e| e.id == 1).unwrap();
        let mut cred = KiroCredentials::default();
        cred.expires_at = entry.expires_at.clone();
        assert!(is_token_expired(&cred));
    }

    #[test]
    fn test_multi_token_manager_empty_credentials() {
        let config = Config::default();
        let result = MultiTokenManager::new(config, vec![], None, None, false);
        // 支持 0 个凭据启动（可通过管理面板添加）
        assert!(result.is_ok());
        let manager = result.unwrap();
        assert_eq!(manager.total_count(), 0);
        assert_eq!(manager.available_count(), 0);
    }

    #[test]
    fn test_multi_token_manager_duplicate_ids() {
        let config = Config::default();
        let mut cred1 = KiroCredentials::default();
        cred1.id = Some(1);
        let mut cred2 = KiroCredentials::default();
        cred2.id = Some(1); // 重复 ID

        let result = MultiTokenManager::new(config, vec![cred1, cred2], None, None, false);
        assert!(result.is_err());
        let err_msg = result.err().unwrap().to_string();
        assert!(
            err_msg.contains("重复的凭据 ID"),
            "错误消息应包含 '重复的凭据 ID'，实际: {}",
            err_msg
        );
    }

    #[test]
    fn test_multi_token_manager_report_failure() {
        let config = Config::default();
        let cred1 = KiroCredentials::default();
        let cred2 = KiroCredentials::default();

        let manager =
            MultiTokenManager::new(config, vec![cred1, cred2], None, None, false).unwrap();

        // 凭据会自动分配 ID（从 1 开始）
        // MAX_FAILURES_PER_CREDENTIAL = 3，所以前两次失败不会禁用
        assert!(manager.report_failure(1));
        assert_eq!(manager.available_count(), 2);
        assert!(manager.report_failure(1));
        assert_eq!(manager.available_count(), 2);

        // 第三次失败会禁用第一个凭据
        assert!(manager.report_failure(1));
        assert_eq!(manager.available_count(), 1);

        // 继续失败第二个凭据（使用 ID 2），需要 3 次才会禁用
        assert!(manager.report_failure(2));
        assert!(manager.report_failure(2));
        assert!(!manager.report_failure(2)); // 所有凭据都禁用了
        assert_eq!(manager.available_count(), 0);
    }

    #[test]
    fn test_multi_token_manager_report_success() {
        let config = Config::default();
        let cred = KiroCredentials::default();

        let manager = MultiTokenManager::new(config, vec![cred], None, None, false).unwrap();

        // 失败一次（使用 ID 1）
        manager.report_failure(1);

        // 成功后重置计数（使用 ID 1）
        manager.report_success(1);

        // 再失败一次不会禁用（因为计数已重置）
        manager.report_failure(1);
        assert_eq!(manager.available_count(), 1);
    }

    #[tokio::test]
    async fn test_multi_token_manager_acquire_context_auto_recovers_all_disabled() {
        let config = Config::default();
        let mut cred1 = KiroCredentials::default();
        cred1.access_token = Some("t1".to_string());
        cred1.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());
        let mut cred2 = KiroCredentials::default();
        cred2.access_token = Some("t2".to_string());
        cred2.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());

        let manager =
            MultiTokenManager::new(config, vec![cred1, cred2], None, None, false).unwrap();

        // 凭据会自动分配 ID（从 1 开始）
        for _ in 0..MAX_FAILURES_PER_CREDENTIAL {
            manager.report_failure(1);
        }
        for _ in 0..MAX_FAILURES_PER_CREDENTIAL {
            manager.report_failure(2);
        }

        assert_eq!(manager.available_count(), 0);

        // 应触发自愈：重置失败计数并重新启用，避免必须重启进程
        let ctx = manager.acquire_context().await.unwrap();
        assert!(ctx.token == "t1" || ctx.token == "t2");
        assert_eq!(manager.available_count(), 2);
    }

    #[tokio::test]
    async fn test_multi_token_manager_acquire_context_prefers_higher_balance_when_usage_equal() {
        let config = Config::default();
        let mut cred1 = KiroCredentials::default();
        cred1.access_token = Some("t1".to_string());
        cred1.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());
        let mut cred2 = KiroCredentials::default();
        cred2.access_token = Some("t2".to_string());
        cred2.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());

        let manager =
            MultiTokenManager::new(config, vec![cred1, cred2], None, None, false).unwrap();

        // 两个凭据使用次数都为 0 时，应优先选择余额更高的
        manager.update_balance_cache(1, 100.0);
        manager.update_balance_cache(2, 200.0);

        let ctx = manager.acquire_context().await.unwrap();
        assert_eq!(ctx.id, 2);
    }

    #[tokio::test]
    async fn test_multi_token_manager_acquire_context_round_robin_when_balance_and_usage_equal() {
        let config = Config::default();
        let mut cred1 = KiroCredentials::default();
        cred1.access_token = Some("t1".to_string());
        cred1.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());
        let mut cred2 = KiroCredentials::default();
        cred2.access_token = Some("t2".to_string());
        cred2.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());

        let manager =
            MultiTokenManager::new(config, vec![cred1, cred2], None, None, false).unwrap();

        manager.update_balance_cache(1, 100.0);
        manager.update_balance_cache(2, 100.0);

        let ctx1 = manager.acquire_context().await.unwrap();
        let ctx2 = manager.acquire_context().await.unwrap();
        assert_ne!(ctx1.id, ctx2.id);
    }

    #[test]
    fn test_multi_token_manager_report_quota_exhausted() {
        let config = Config::default();
        let cred1 = KiroCredentials::default();
        let cred2 = KiroCredentials::default();

        let manager =
            MultiTokenManager::new(config, vec![cred1, cred2], None, None, false).unwrap();

        // 凭据会自动分配 ID（从 1 开始）
        assert_eq!(manager.available_count(), 2);
        assert!(manager.report_quota_exhausted(1));
        assert_eq!(manager.available_count(), 1);

        // 再禁用第二个后，无可用凭据
        assert!(!manager.report_quota_exhausted(2));
        assert_eq!(manager.available_count(), 0);
    }

    #[test]
    fn test_multi_token_manager_quota_exhausted_does_not_disable_known_overage_enabled() {
        let config = Config::default();
        let cred = KiroCredentials {
            overage_enabled: Some(true),
            ..Default::default()
        };

        let manager = MultiTokenManager::new(config, vec![cred], None, None, false).unwrap();

        assert!(manager.report_quota_exhausted(1));
        assert_eq!(manager.available_count(), 1);
        let snapshot = manager.snapshot();
        assert!(!snapshot.entries[0].disabled);
        assert_eq!(snapshot.entries[0].disable_reason, None);
    }

    #[tokio::test]
    async fn test_multi_token_manager_quota_disabled_is_not_auto_recovered() {
        let config = Config::default();
        let cred1 = KiroCredentials::default();
        let cred2 = KiroCredentials::default();

        let manager =
            MultiTokenManager::new(config, vec![cred1, cred2], None, None, false).unwrap();

        manager.report_quota_exhausted(1);
        manager.report_quota_exhausted(2);
        assert_eq!(manager.available_count(), 0);

        let err = manager.acquire_context().await.err().unwrap().to_string();
        assert!(
            err.contains("所有凭据均已禁用"),
            "错误应提示所有凭据禁用，实际: {}",
            err
        );
        assert_eq!(manager.available_count(), 0);
    }

    #[tokio::test]
    async fn test_multi_token_manager_rate_limited_with_some_disabled_does_not_report_all_disabled()
    {
        // 复现线上日志：
        // - total > available（部分凭据被禁用）
        // - 所有可用凭据都被速率限制/冷却暂时挡住
        // 期望：等待最短可用时间后继续尝试，而不是误报“所有凭据均已禁用（x/y）”。

        let mut config = Config::default();
        // 固定间隔 10ms，避免测试过慢且消除抖动带来的不确定性
        config.credential_rpm = Some(6000);

        let cred1 = KiroCredentials {
            access_token: Some("token-1".to_string()),
            expires_at: Some("2999-01-01T00:00:00Z".to_string()),
            ..Default::default()
        };
        let cred2 = KiroCredentials {
            access_token: Some("token-2".to_string()),
            expires_at: Some("2999-01-01T00:00:00Z".to_string()),
            ..Default::default()
        };

        let manager =
            MultiTokenManager::new(config, vec![cred1, cred2], None, None, false).unwrap();

        // 禁用 #2，仅保留一个可用凭据
        assert!(manager.report_quota_exhausted(2));
        assert_eq!(manager.available_count(), 1);

        // 预先占位：让 #1 在下一次 acquire_context() 时必然触发速率限制
        assert!(manager.rate_limiter().try_acquire(1).is_ok());

        // 关键断言：不会抛出“所有凭据均已禁用（1/2）”，而是等待后成功返回。
        let ctx = manager.acquire_context().await.unwrap();
        assert_eq!(ctx.id, 1);
    }

    #[test]
    fn test_set_credential_cooldown_with_duration_does_not_increment_failure_count() {
        let config = Config::default();
        let manager =
            MultiTokenManager::new(config, vec![KiroCredentials::default()], None, None, false)
                .unwrap();

        let cooldown = manager.set_credential_cooldown_with_duration(
            1,
            CooldownReason::RateLimitExceeded,
            Some(std::time::Duration::from_secs(120)),
        );
        assert_eq!(cooldown, std::time::Duration::from_secs(120));

        let snapshot = manager.snapshot();
        assert_eq!(snapshot.entries.len(), 1);
        assert_eq!(snapshot.entries[0].failure_count, 0);
        assert!(!snapshot.entries[0].disabled);
        assert!(snapshot.entries[0].last_used_at.is_some());

        let (reason, remaining) = manager.cooldown_manager().check_cooldown(1).unwrap();
        assert_eq!(reason, CooldownReason::RateLimitExceeded);
        assert!(remaining <= std::time::Duration::from_secs(120));
        assert!(remaining > std::time::Duration::from_secs(100));
    }

    #[tokio::test]
    async fn test_multi_token_manager_acquire_context_skips_rate_limited_credential() {
        let config = Config::default();
        let mut cred1 = KiroCredentials::default();
        cred1.access_token = Some("t1".to_string());
        cred1.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());
        cred1.priority = 0;

        let mut cred2 = KiroCredentials::default();
        cred2.access_token = Some("t2".to_string());
        cred2.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());
        cred2.priority = 0;

        let manager =
            MultiTokenManager::new(config, vec![cred1, cred2], None, None, false).unwrap();

        manager.set_credential_cooldown_with_duration(
            1,
            CooldownReason::RateLimitExceeded,
            Some(std::time::Duration::from_millis(200)),
        );

        let ctx = manager.acquire_context().await.unwrap();
        assert_eq!(ctx.id, 2);
    }

    #[tokio::test]
    async fn test_multi_token_manager_acquire_context_waits_until_rate_limit_cooldown_expires() {
        let config = Config::default();
        let mut cred = KiroCredentials::default();
        cred.access_token = Some("t1".to_string());
        cred.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());

        let manager = MultiTokenManager::new(config, vec![cred], None, None, false).unwrap();

        manager.set_credential_cooldown_with_duration(
            1,
            CooldownReason::RateLimitExceeded,
            Some(std::time::Duration::from_millis(150)),
        );

        let started = std::time::Instant::now();
        let ctx = manager.acquire_context().await.unwrap();
        let elapsed = started.elapsed();

        assert_eq!(ctx.id, 1);
        assert!(elapsed >= std::time::Duration::from_millis(120));
    }

    #[tokio::test]
    async fn test_acquire_context_bails_when_all_credentials_cooling_longer_than_threshold() {
        let config = Config::default();
        let mut cred = KiroCredentials::default();
        cred.access_token = Some("t1".to_string());
        cred.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());

        let manager = MultiTokenManager::new(config, vec![cred], None, None, false).unwrap();

        // 设置 10 秒冷却，超过 2 秒阈值
        manager.set_credential_cooldown_with_duration(
            1,
            CooldownReason::RateLimitExceeded,
            Some(std::time::Duration::from_secs(10)),
        );

        let started = std::time::Instant::now();
        let err = manager.acquire_context().await.err().unwrap();
        let elapsed = started.elapsed();

        // 应立即返回错误，不会长睡
        assert!(elapsed < std::time::Duration::from_secs(2));
        assert!(err.to_string().contains("所有凭据均处于冷却/速率限制"));
        assert!(err.to_string().contains("retry_after_secs="));
    }

    #[tokio::test]
    async fn test_acquire_context_bails_with_total_exhausted_branch() {
        let config = Config::default();
        let mut cred1 = KiroCredentials::default();
        cred1.access_token = Some("t1".to_string());
        cred1.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());
        cred1.priority = 0;

        let mut cred2 = KiroCredentials::default();
        cred2.access_token = Some("t2".to_string());
        cred2.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());
        cred2.priority = 1; // 不同优先级，确保两个都被尝试

        let manager =
            MultiTokenManager::new(config, vec![cred1, cred2], None, None, false).unwrap();

        // 两个凭据都设置长冷却
        manager.set_credential_cooldown_with_duration(
            1,
            CooldownReason::RateLimitExceeded,
            Some(std::time::Duration::from_secs(10)),
        );
        manager.set_credential_cooldown_with_duration(
            2,
            CooldownReason::ServerError,
            Some(std::time::Duration::from_secs(10)),
        );

        let started = std::time::Instant::now();
        let err = manager.acquire_context().await.err().unwrap();
        let elapsed = started.elapsed();

        assert!(elapsed < std::time::Duration::from_secs(2));
        assert!(err.to_string().contains("所有凭据均处于冷却/速率限制"));
        assert!(err.to_string().contains("retry_after_secs="));
    }

    /// 混合故障场景：一个凭据长冷却，一个凭据 token 刷新失败（access_token/refresh_token 均缺失）。
    /// 期望：不应快速返回 429（会错误吞掉真实的 token 刷新失败语义），应走常规 sleep 路径。
    /// 用 tokio::time::timeout 做短超时，避免测试卡在长 sleep 循环里。
    #[tokio::test]
    async fn test_acquire_context_does_not_bail_429_on_mixed_failures() {
        let config = Config::default();

        let mut cred1 = KiroCredentials::default();
        cred1.access_token = Some("t1".to_string());
        cred1.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());
        cred1.priority = 0;

        // 无 access_token / refresh_token / kiro_api_key —— try_ensure_token 会失败
        let mut cred2 = KiroCredentials::default();
        cred2.access_token = None;
        cred2.refresh_token = None;
        cred2.expires_at = None;
        cred2.priority = 0;

        let manager =
            MultiTokenManager::new(config, vec![cred1, cred2], None, None, false).unwrap();

        // cred1 长冷却（超过 2s 阈值），cred2 不设冷却但 token 刷新会失败
        manager.set_credential_cooldown_with_duration(
            1,
            CooldownReason::RateLimitExceeded,
            Some(std::time::Duration::from_secs(10)),
        );

        let result = tokio::time::timeout(
            std::time::Duration::from_millis(300),
            manager.acquire_context(),
        )
        .await;

        match result {
            Err(_timeout) => {
                // 超时说明进入了 sleep 循环——正是期望的行为（未提前 bail 429）。
            }
            Ok(Ok(_)) => panic!("混合故障场景不应成功获取 context"),
            Ok(Err(err)) => {
                assert!(
                    !err.to_string().contains("所有凭据均处于冷却/速率限制"),
                    "混合故障场景不应 bail 429：{}",
                    err
                );
            }
        }
    }

    #[tokio::test]
    async fn test_multi_token_manager_acquire_context_for_user_keeps_affinity_when_bound_credential_rate_limited()
     {
        let mut config = Config::default();
        config.credential_rpm = Some(60_000);

        let mut cred1 = KiroCredentials::default();
        cred1.access_token = Some("t1".to_string());
        cred1.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());
        cred1.priority = 0;

        let mut cred2 = KiroCredentials::default();
        cred2.access_token = Some("t2".to_string());
        cred2.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());
        cred2.priority = 0;

        let manager =
            MultiTokenManager::new(config, vec![cred1, cred2], None, None, false).unwrap();

        let first = manager
            .acquire_context_for_user(Some("user-a"))
            .await
            .unwrap();
        assert_eq!(first.id, 1);

        manager.set_credential_cooldown_with_duration(
            1,
            CooldownReason::RateLimitExceeded,
            Some(std::time::Duration::from_millis(200)),
        );

        let diverted = manager
            .acquire_context_for_user(Some("user-a"))
            .await
            .unwrap();
        assert_eq!(diverted.id, 2);

        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        let while_cooling = manager
            .acquire_context_for_user(Some("user-a"))
            .await
            .unwrap();
        assert_eq!(while_cooling.id, 2);

        tokio::time::sleep(std::time::Duration::from_millis(220)).await;
        let rebound = manager
            .acquire_context_for_user(Some("user-a"))
            .await
            .unwrap();
        assert_eq!(rebound.id, 1);
    }

    // ============ 凭据级 Region 优先级测试 ============

    /// 辅助函数：获取 OIDC 刷新使用的 region（用于测试）
    fn get_oidc_region_for_credential<'a>(
        credentials: &'a KiroCredentials,
        config: &'a Config,
    ) -> &'a str {
        credentials.region.as_ref().unwrap_or(&config.region)
    }

    #[test]
    fn test_build_idc_refresh_user_agents_uses_config_versions() {
        let mut config = Config::default();
        config.system_version = "darwin#25.4.0".to_string();
        config.node_version = "22.22.0".to_string();

        let (amz_user_agent, user_agent) = build_idc_refresh_user_agents(&config);

        assert_eq!(amz_user_agent, "aws-sdk-js/3.980.0 KiroIDE");
        assert!(user_agent.contains("os/darwin#25.4.0"));
        assert!(user_agent.contains("md/nodejs#22.22.0"));
        assert!(user_agent.contains("api/sso-oidc#3.980.0"));
    }

    #[test]
    fn test_build_usage_limit_user_agents_uses_config_versions() {
        let mut config = Config::default();
        config.kiro_version = "0.11.107".to_string();
        config.system_version = "win32#10.0.22631".to_string();
        config.node_version = "22.22.0".to_string();
        let credentials = KiroCredentials::default();
        let endpoint = IdeEndpoint::new();
        let ctx = RequestContext {
            credentials: &credentials,
            token: "test_token",
            machine_id: "machine123",
            config: &config,
        };

        let usage = endpoint.usage_request_parts(&ctx).unwrap();
        let amz_user_agent = usage
            .headers
            .iter()
            .find(|(name, _)| *name == "x-amz-user-agent")
            .map(|(_, value)| value.clone())
            .unwrap();
        let user_agent = usage
            .headers
            .iter()
            .find(|(name, _)| *name == "user-agent")
            .map(|(_, value)| value.clone())
            .unwrap();

        assert_eq!(
            amz_user_agent,
            "aws-sdk-js/1.0.0 KiroIDE-0.11.107-machine123"
        );
        assert!(user_agent.contains("os/win32#10.0.22631"));
        assert!(user_agent.contains("md/nodejs#22.22.0"));
        assert!(user_agent.contains("KiroIDE-0.11.107-machine123"));
    }

    #[test]
    fn test_credential_region_priority_uses_credential_region() {
        // 凭据配置了 region 时，应使用凭据的 region
        let mut config = Config::default();
        config.region = "us-west-2".to_string();

        let mut credentials = KiroCredentials::default();
        credentials.region = Some("eu-west-1".to_string());

        let region = get_oidc_region_for_credential(&credentials, &config);
        assert_eq!(region, "eu-west-1");
    }

    #[test]
    fn test_credential_region_priority_fallback_to_config() {
        // 凭据未配置 region 时，应回退到 config.region
        let mut config = Config::default();
        config.region = "us-west-2".to_string();

        let credentials = KiroCredentials::default();
        assert!(credentials.region.is_none());

        let region = get_oidc_region_for_credential(&credentials, &config);
        assert_eq!(region, "us-west-2");
    }

    #[test]
    fn test_multiple_credentials_use_respective_regions() {
        // 多凭据场景下，不同凭据使用各自的 region
        let mut config = Config::default();
        config.region = "ap-northeast-1".to_string();

        let mut cred1 = KiroCredentials::default();
        cred1.region = Some("us-east-1".to_string());

        let mut cred2 = KiroCredentials::default();
        cred2.region = Some("eu-west-1".to_string());

        let cred3 = KiroCredentials::default(); // 无 region，使用 config

        assert_eq!(get_oidc_region_for_credential(&cred1, &config), "us-east-1");
        assert_eq!(get_oidc_region_for_credential(&cred2, &config), "eu-west-1");
        assert_eq!(
            get_oidc_region_for_credential(&cred3, &config),
            "ap-northeast-1"
        );
    }

    #[test]
    fn test_idc_oidc_endpoint_uses_credential_region() {
        // 验证 IdC OIDC endpoint URL 使用凭据 region
        let mut config = Config::default();
        config.region = "us-west-2".to_string();

        let mut credentials = KiroCredentials::default();
        credentials.region = Some("eu-central-1".to_string());

        let region = get_oidc_region_for_credential(&credentials, &config);
        let refresh_url = format!("https://oidc.{}.amazonaws.com/token", region);

        assert_eq!(refresh_url, "https://oidc.eu-central-1.amazonaws.com/token");
    }

    #[test]
    fn test_social_refresh_endpoint_uses_credential_region() {
        // 验证 Social refresh endpoint URL 使用凭据 region
        let mut config = Config::default();
        config.region = "us-west-2".to_string();

        let mut credentials = KiroCredentials::default();
        credentials.region = Some("ap-southeast-1".to_string());

        let region = get_oidc_region_for_credential(&credentials, &config);
        let refresh_url = format!("https://prod.{}.auth.desktop.kiro.dev/refreshToken", region);

        assert_eq!(
            refresh_url,
            "https://prod.ap-southeast-1.auth.desktop.kiro.dev/refreshToken"
        );
    }

    /// Round 7 corrected: this test used to assert `api_host` used config.region
    /// even when credentials had its own region — but it only did a local
    /// `format!()`, never invoked `effective_api_region` / `host()`. The actual
    /// code uses **credentials.region first, falling back to config**. This
    /// test now exercises the real `effective_api_region` to lock in that
    /// behavior.
    #[test]
    fn test_api_call_uses_credentials_region_when_set() {
        let mut config = Config::default();
        config.region = "us-west-2".to_string();

        let mut credentials = KiroCredentials::default();
        credentials.region = Some("eu-west-1".to_string());

        // The real production behavior: credentials.region wins.
        let api_region = credentials.effective_api_region(&config);
        assert_eq!(
            api_region, "eu-west-1",
            "credentials.region must take precedence over config.region"
        );

        let api_host = format!("q.{}.amazonaws.com", api_region);
        assert_eq!(api_host, "q.eu-west-1.amazonaws.com");
    }

    /// Mirror: when credentials.region is None, falls back to config.region.
    #[test]
    fn test_api_call_falls_back_to_config_region() {
        let mut config = Config::default();
        config.region = "us-west-2".to_string();

        let credentials = KiroCredentials::default(); // region: None

        let api_region = credentials.effective_api_region(&config);
        assert_eq!(api_region, "us-west-2");
    }

    #[test]
    fn test_credential_region_empty_string_fallback_to_config() {
        // 空字符串 region 应回退到 config.region
        let mut config = Config::default();
        config.region = "us-west-2".to_string();

        let mut credentials = KiroCredentials::default();
        credentials.region = Some("".to_string());

        let region = credentials
            .region
            .as_ref()
            .filter(|r| !r.trim().is_empty())
            .unwrap_or(&config.region);
        // 空字符串应回退到 config.region
        assert_eq!(region, "us-west-2");
    }

    #[test]
    fn test_credential_region_whitespace_fallback_to_config() {
        // 纯空白字符 region 应回退到 config.region
        let mut config = Config::default();
        config.region = "us-west-2".to_string();

        let mut credentials = KiroCredentials::default();
        credentials.region = Some("   ".to_string());

        let region = credentials
            .region
            .as_ref()
            .filter(|r| !r.trim().is_empty())
            .unwrap_or(&config.region);
        assert_eq!(region, "us-west-2");
    }

    #[test]
    fn test_update_default_endpoint() {
        let mut config = Config::default();
        config.default_endpoint = "ide".to_string();

        let credentials = KiroCredentials::default();
        let manager = MultiTokenManager::new(config, vec![credentials], None, None, false).unwrap();

        assert_eq!(manager.config().default_endpoint, "ide");

        manager.update_default_endpoint("cli".to_string());
        assert_eq!(manager.config().default_endpoint, "cli");

        manager.update_default_endpoint("ide".to_string());
        assert_eq!(manager.config().default_endpoint, "ide");
    }
}
