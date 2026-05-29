//! Kiro Web Portal API（app.kiro.dev）
//!
//! 参考 Kiro-account-manager：
//! - POST https://app.kiro.dev/service/KiroWebPortalService/operation/{Operation}
//! - 协议：rpc-v2-cbor
//! - Content-Type/Accept: application/cbor
//! - Authorization: Bearer <accessToken>
//! - Cookie: Idp=<idp>; AccessToken=<accessToken>

#![allow(dead_code)]

use std::time::Duration;

use anyhow::Context;
use chrono::{DateTime, Utc};
use reqwest::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE, COOKIE, HeaderMap, HeaderValue};

use crate::http_client::{ProxyConfig, build_client};

#[allow(dead_code)]
const KIRO_API_BASE: &str = "https://app.kiro.dev/service/KiroWebPortalService/operation";
#[allow(dead_code)]
const KIRO_HOME_URL: &str = "https://app.kiro.dev/";
#[allow(dead_code)]
const SMITHY_PROTOCOL: &str = "rpc-v2-cbor";
const AMZ_SDK_REQUEST: &str = "attempt=1; max=1";
const X_AMZ_USER_AGENT: &str = "aws-sdk-js/1.0.0 kiro-rs/1.0.0";

/// Social 登录（Google/GitHub）默认使用的 Profile ARN。
/// 参考 Kiro-account-manager：social 凭据即使自身没有 profileArn，
/// 也可以用这个公共 ARN 提交 UpdateBillingPreferences。
pub const DEFAULT_SOCIAL_PROFILE_ARN: &str =
    "arn:aws:codewhisperer:us-east-1:699475941385:profile/EHGA3GRVQMUK";

/// Builder ID 登录默认使用的 Profile ARN。
pub const DEFAULT_BUILDER_ID_PROFILE_ARN: &str =
    "arn:aws:codewhisperer:us-east-1:638616132270:profile/AAAACCCCXXXX";

/// 根据认证方式解析有效的 Profile ARN。
///
/// 如果凭据自身有 profileArn 则直接使用；否则根据 idp/auth_method 回退到平台默认值。
/// 与 Kiro-account-manager 的 `resolveProfileArn` 逻辑对齐。
pub fn resolve_profile_arn<'a>(credential_profile_arn: Option<&'a str>, idp: &str) -> &'a str {
    if let Some(arn) = credential_profile_arn
        && !arn.trim().is_empty()
    {
        return arn;
    }
    // idp 为空或非 social 时使用 builder id 默认值
    let idp_lower = idp.to_lowercase();
    if idp_lower.contains("google") || idp_lower.contains("github") {
        DEFAULT_SOCIAL_PROFILE_ARN
    } else {
        DEFAULT_BUILDER_ID_PROFILE_ARN
    }
}

/// 调用写操作（如 UpdateBillingPreferences）必需的 CSRF 会话上下文
///
/// 通过带 Cookie 访问 `https://app.kiro.dev/` 拿到 HTML，从两个 meta 标签提取：
/// - `<meta name="csrf-token" content="...">` —— 写操作必须放进 `X-CSRF-Token` header；
/// - `<meta name="user-id" content="...">` —— 服务端会在 HTML 响应里 Set-Cookie `UserId=...`，
///   后续写操作的 Cookie 必须把它带回去（CSRF 校验依赖这个 cookie）。
#[derive(Debug, Clone)]
struct CsrfSession {
    csrf_token: String,
    user_id: String,
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GetUserInfoRequest {
    pub origin: String,
}

#[derive(Debug, serde::Serialize, serde::Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct UserInfoResponse {
    pub email: Option<String>,
    pub user_id: Option<String>,
    pub idp: Option<String>,
    pub status: Option<String>,
    pub feature_flags: Option<Vec<String>>,
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GetUserUsageAndLimitsRequest {
    pub is_email_required: bool,
    pub origin: String,
}

#[derive(Debug, serde::Serialize, serde::Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct UsageUserInfo {
    pub email: Option<String>,
    pub user_id: Option<String>,
}

#[derive(Debug, serde::Serialize, serde::Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SubscriptionInfo {
    pub r#type: Option<String>,
    pub subscription_title: Option<String>,
    pub upgrade_capability: Option<String>,
    pub overage_capability: Option<String>,
    pub subscription_management_target: Option<String>,
}

#[derive(Debug, serde::Serialize, serde::Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Bonus {
    pub bonus_code: Option<String>,
    pub display_name: Option<String>,

    pub usage_limit: Option<f64>,
    pub usage_limit_with_precision: Option<f64>,
    pub current_usage: Option<f64>,
    pub current_usage_with_precision: Option<f64>,

    pub status: Option<String>,
    pub expires_at: Option<String>,
}

#[derive(Debug, serde::Serialize, serde::Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct FreeTrialInfo {
    pub usage_limit: Option<f64>,
    pub usage_limit_with_precision: Option<f64>,
    pub current_usage: Option<f64>,
    pub current_usage_with_precision: Option<f64>,

    pub free_trial_expiry: Option<String>,
    pub free_trial_status: Option<String>,
}

#[derive(Debug, serde::Serialize, serde::Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct UsageBreakdownItem {
    pub resource_type: Option<String>,

    pub current_usage: Option<f64>,
    pub current_usage_with_precision: Option<f64>,
    pub usage_limit: Option<f64>,
    pub usage_limit_with_precision: Option<f64>,

    pub display_name: Option<String>,
    pub display_name_plural: Option<String>,
    pub currency: Option<String>,
    pub unit: Option<String>,
    pub overage_rate: Option<f64>,
    pub overage_cap: Option<f64>,

    pub free_trial_info: Option<FreeTrialInfo>,
    pub bonuses: Option<Vec<Bonus>>,
}

#[derive(Debug, serde::Serialize, serde::Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct OverageConfiguration {
    pub overage_enabled: Option<bool>,
}

#[derive(Debug, serde::Serialize, serde::Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct UsageAndLimitsResponse {
    pub user_info: Option<UsageUserInfo>,
    pub subscription_info: Option<SubscriptionInfo>,
    pub usage_breakdown_list: Option<Vec<UsageBreakdownItem>>,
    pub next_date_reset: Option<f64>,
    pub overage_configuration: Option<OverageConfiguration>,
}

#[derive(Debug, serde::Deserialize)]
struct CborErrorResponse {
    #[serde(rename = "__type")]
    pub type_name: Option<String>,
    pub message: Option<String>,
}

fn header_value(s: &str, name: &'static str) -> anyhow::Result<HeaderValue> {
    HeaderValue::from_str(s).with_context(|| format!("{} header 无效", name))
}

fn build_headers(
    access_token: &str,
    idp: &str,
    csrf: Option<&CsrfSession>,
) -> anyhow::Result<HeaderMap> {
    let mut headers = HeaderMap::new();

    headers.insert(ACCEPT, HeaderValue::from_static("application/cbor"));
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/cbor"));
    headers.insert("smithy-protocol", HeaderValue::from_static(SMITHY_PROTOCOL));
    headers.insert(
        "amz-sdk-invocation-id",
        header_value(&uuid::Uuid::new_v4().to_string(), "amz-sdk-invocation-id")?,
    );
    headers.insert("amz-sdk-request", HeaderValue::from_static(AMZ_SDK_REQUEST));
    headers.insert(
        "x-amz-user-agent",
        header_value(X_AMZ_USER_AGENT, "x-amz-user-agent")?,
    );

    headers.insert(
        AUTHORIZATION,
        header_value(&format!("Bearer {}", access_token), "authorization")?,
    );

    // Cookie 顺序：Idp / AccessToken 是基础鉴权；UserId 仅在写操作（带 CSRF）时附加，
    // 上游 CSRF 校验需要它和 X-CSRF-Token 一起回传。
    let cookie = match csrf {
        Some(s) => format!(
            "Idp={}; AccessToken={}; UserId={}",
            idp, access_token, s.user_id
        ),
        None => format!("Idp={}; AccessToken={}", idp, access_token),
    };
    headers.insert(COOKIE, header_value(&cookie, "cookie")?);

    if let Some(s) = csrf {
        headers.insert("x-csrf-token", header_value(&s.csrf_token, "x-csrf-token")?);
    }

    Ok(headers)
}

/// 获取 CSRF 会话上下文。
///
/// 实际请求 `https://app.kiro.dev/`（带 Idp / AccessToken Cookie），从 HTML 的
/// `<meta name="csrf-token">` 和 `<meta name="user-id">` 抽取两个值。
///
/// 上游对未登录用户返回的 HTML 里这两个 meta 都是占位注释（`<!-- CSRF token not available... -->`），
/// 所以 token 失效或 idp 错误会立刻在这里失败，便于早判错。
async fn fetch_csrf_session(
    access_token: &str,
    idp: &str,
    proxy: Option<&ProxyConfig>,
) -> anyhow::Result<CsrfSession> {
    let client = build_client(proxy, 30, crate::model::config::TlsBackend::NativeTls)?;
    let resp = client
        .get(KIRO_HOME_URL)
        .header(
            COOKIE,
            header_value(
                &format!("Idp={}; AccessToken={}", idp, access_token),
                "cookie",
            )?,
        )
        .timeout(Duration::from_secs(30))
        .send()
        .await
        .context("获取 CSRF 会话页失败")?;

    let status = resp.status();
    let html = resp.text().await.context("读取 CSRF 会话页响应失败")?;
    if !status.is_success() {
        anyhow::bail!("获取 CSRF 会话页失败：HTTP {}", status);
    }

    let csrf_token = extract_meta(&html, "csrf-token")
        .ok_or_else(|| anyhow::anyhow!("HTML 未包含 csrf-token meta（access_token 可能已失效）"))?;
    let user_id = extract_meta(&html, "user-id")
        .ok_or_else(|| anyhow::anyhow!("HTML 未包含 user-id meta（access_token 可能已失效）"))?;

    Ok(CsrfSession {
        csrf_token,
        user_id,
    })
}

/// 从 HTML 中提取 `<meta name="<name>" content="...">` 的 content。
///
/// 简单匹配，仅供 CSRF/User-ID 这两个固定 meta 使用。
fn extract_meta(html: &str, name: &str) -> Option<String> {
    let needle = format!("name=\"{}\"", name);
    let idx = html.find(&needle)?;
    let tail = &html[idx..];
    let content_idx = tail.find("content=\"")?;
    let after = &tail[content_idx + "content=\"".len()..];
    let end = after.find('"')?;
    Some(after[..end].to_string())
}

async fn request_cbor<TResp, TReq>(
    operation: &str,
    req: &TReq,
    access_token: &str,
    idp: &str,
    proxy: Option<&ProxyConfig>,
    csrf: Option<&CsrfSession>,
) -> anyhow::Result<TResp>
where
    TResp: for<'de> serde::Deserialize<'de>,
    TReq: serde::Serialize,
{
    let url = format!("{}/{}", KIRO_API_BASE, operation);

    let body = serde_cbor::to_vec(req).context("CBOR 编码失败")?;

    let client = build_client(proxy, 60, crate::model::config::TlsBackend::NativeTls)?;

    let resp = client
        .post(&url)
        .headers(build_headers(access_token, idp, csrf)?)
        .timeout(Duration::from_secs(60))
        .body(body)
        .send()
        .await
        .context("请求 Kiro Web Portal API 失败")?;

    let status = resp.status();
    let bytes = resp.bytes().await.context("读取响应失败")?;

    if !status.is_success() {
        // 尽力解析 CBOR 错误体
        if let Ok(err) = serde_cbor::from_slice::<CborErrorResponse>(&bytes) {
            let type_name = err
                .type_name
                .as_deref()
                .and_then(|s| s.split('#').next_back())
                .unwrap_or("HTTPError");
            let msg = err.message.unwrap_or_else(|| format!("HTTP {}", status));
            anyhow::bail!("{}: {}", type_name, msg);
        }

        let raw = String::from_utf8_lossy(&bytes);
        anyhow::bail!("HTTP {}: {}", status, raw);
    }

    let out = serde_cbor::from_slice::<TResp>(&bytes).context("CBOR 解码失败")?;
    Ok(out)
}

pub async fn get_user_info(
    access_token: &str,
    idp: &str,
    proxy: Option<&ProxyConfig>,
) -> anyhow::Result<UserInfoResponse> {
    request_cbor(
        "GetUserInfo",
        &GetUserInfoRequest {
            origin: "KIRO_IDE".to_string(),
        },
        access_token,
        idp,
        proxy,
        None,
    )
    .await
}

pub async fn get_user_usage_and_limits(
    access_token: &str,
    idp: &str,
    proxy: Option<&ProxyConfig>,
) -> anyhow::Result<UsageAndLimitsResponse> {
    request_cbor(
        "GetUserUsageAndLimits",
        &GetUserUsageAndLimitsRequest {
            is_email_required: true,
            origin: "KIRO_IDE".to_string(),
        },
        access_token,
        idp,
        proxy,
        None,
    )
    .await
}

// ============================================================================
// UpdateBillingPreferences —— 用于开启/关闭超额（overage）
// ============================================================================

/// UpdateBillingPreferences 请求体里的超额配置子结构
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct UpdateOverageConfiguration {
    pub overage_enabled: bool,
}

/// UpdateBillingPreferences 请求体
///
/// 抓包验证：POST /service/KiroWebPortalService/operation/UpdateBillingPreferences
/// CBOR 体形如 `{ overageConfiguration: { overageEnabled: true }, profileArn: "arn:..." }`
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct UpdateBillingPreferencesRequest {
    pub overage_configuration: UpdateOverageConfiguration,
    pub profile_arn: String,
}

/// UpdateBillingPreferences 响应（上游返回空对象，用 IgnoredAny 接收）
#[derive(Debug, serde::Deserialize)]
struct UpdateBillingPreferencesResponse {
    #[serde(flatten, default)]
    _ignored: serde::de::IgnoredAny,
}

/// 调用 UpdateBillingPreferences 开启或关闭超额
///
/// 必须传入 profileArn，否则上游会返回 400/ValidationException。
///
/// 写操作流程：
/// 1. 先 GET `https://app.kiro.dev/` 拿 CSRF token + UserId（[`fetch_csrf_session`]）；
/// 2. 然后 POST 操作时附加 `X-CSRF-Token` header 与 `UserId` cookie，
///    上游缺一不可（错误信息分别是 "CSRF token is required" / "Invalid CSRF token"）。
pub async fn update_billing_preferences(
    access_token: &str,
    idp: &str,
    profile_arn: &str,
    overage_enabled: bool,
    proxy: Option<&ProxyConfig>,
) -> anyhow::Result<()> {
    if profile_arn.trim().is_empty() {
        anyhow::bail!("UpdateBillingPreferences 需要 profileArn，但凭据未提供");
    }
    let csrf = fetch_csrf_session(access_token, idp, proxy).await?;
    let _resp: UpdateBillingPreferencesResponse = request_cbor(
        "UpdateBillingPreferences",
        &UpdateBillingPreferencesRequest {
            overage_configuration: UpdateOverageConfiguration { overage_enabled },
            profile_arn: profile_arn.to_string(),
        },
        access_token,
        idp,
        proxy,
        Some(&csrf),
    )
    .await?;
    Ok(())
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreditBonus {
    pub code: String,
    pub name: String,
    pub current: f64,
    pub limit: f64,
    pub expires_at: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreditsUsageSummary {
    pub current: f64,
    pub limit: f64,

    pub base_current: f64,
    pub base_limit: f64,

    pub free_trial_current: f64,
    pub free_trial_limit: f64,
    pub free_trial_expiry: Option<String>,

    pub bonuses: Vec<CreditBonus>,

    /// 上游用 epoch 秒（可能带小数），与 GetUserUsageAndLimits 的
    /// `nextDateReset` 字段一致；前端按需要本地化展示。
    pub next_reset_date: Option<f64>,
    pub overage_enabled: Option<bool>,

    pub resource_detail: Option<CreditsResourceDetail>,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreditsResourceDetail {
    pub display_name: Option<String>,
    pub display_name_plural: Option<String>,
    pub resource_type: Option<String>,
    pub currency: Option<String>,
    pub unit: Option<String>,
    pub overage_rate: Option<f64>,
    pub overage_cap: Option<f64>,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceUsageSummary {
    pub resource_type: Option<String>,
    pub display_name: Option<String>,
    pub unit: Option<String>,
    pub currency: Option<String>,
    pub current: f64,
    pub limit: f64,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountAggregateInfo {
    pub email: Option<String>,
    pub user_id: Option<String>,
    pub idp: Option<String>,
    pub status: Option<String>,
    pub feature_flags: Option<Vec<String>>,

    pub subscription_title: Option<String>,
    pub subscription_type: String,
    pub subscription: AccountSubscriptionDetails,

    /// 兼容旧 UI：Credits 汇总（如有）
    pub usage: CreditsUsageSummary,

    /// 全部资源用量明细（用于展示/调试）
    pub resources: Vec<ResourceUsageSummary>,

    /// 原始 GetUserUsageAndLimits 响应（不包含 token，仅包含用量/订阅信息）
    pub raw_usage: UsageAndLimitsResponse,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountSubscriptionDetails {
    pub raw_type: Option<String>,
    pub management_target: Option<String>,
    pub upgrade_capability: Option<String>,
    pub overage_capability: Option<String>,
}

fn norm_subscription_type(title: Option<&str>) -> String {
    let Some(t) = title else {
        return "Free".to_string();
    };
    let up = t.to_uppercase();
    if up.contains("PRO") {
        return "Pro".to_string();
    }
    if up.contains("ENTERPRISE") {
        return "Enterprise".to_string();
    }
    if up.contains("TEAMS") {
        return "Teams".to_string();
    }
    "Free".to_string()
}

fn pick_f64(primary: Option<f64>, fallback: Option<f64>) -> f64 {
    primary.or(fallback).unwrap_or(0.0)
}

fn parse_rfc3339(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|dt| dt.with_timezone(&Utc))
}

fn free_trial_is_effective(ft: &FreeTrialInfo) -> bool {
    match ft.free_trial_status.as_deref() {
        Some(s) => s.eq_ignore_ascii_case("ACTIVE"),
        None => {
            let limit = pick_f64(ft.usage_limit_with_precision, ft.usage_limit);
            let current = pick_f64(ft.current_usage_with_precision, ft.current_usage);
            limit > 0.0 || current > 0.0
        }
    }
}

fn bonus_is_effective(b: &Bonus) -> bool {
    match b.status.as_deref() {
        Some(s) => s.eq_ignore_ascii_case("ACTIVE"),
        None => {
            // 没有 status 时：优先用 expiresAt 判断是否仍有效；再用 limit/current 兜底。
            if let Some(exp) = b.expires_at.as_deref()
                && let Some(dt) = parse_rfc3339(exp)
            {
                return dt > Utc::now();
            }
            let limit = pick_f64(b.usage_limit_with_precision, b.usage_limit);
            let current = pick_f64(b.current_usage_with_precision, b.current_usage);
            limit > 0.0 || current > 0.0
        }
    }
}

pub fn aggregate_account_info(
    user_info: Option<UserInfoResponse>,
    usage: UsageAndLimitsResponse,
) -> AccountAggregateInfo {
    let credit = usage.usage_breakdown_list.as_ref().and_then(|l| {
        l.iter().find(|b| {
            b.resource_type
                .as_deref()
                .map(|t| t.eq_ignore_ascii_case("CREDIT"))
                .unwrap_or(false)
                || b.display_name
                    .as_deref()
                    .map(|t| t.eq_ignore_ascii_case("Credits"))
                    .unwrap_or(false)
        })
    });

    let base_limit = credit
        .map(|c| pick_f64(c.usage_limit_with_precision, c.usage_limit))
        .unwrap_or(0.0);
    let base_current = credit
        .map(|c| pick_f64(c.current_usage_with_precision, c.current_usage))
        .unwrap_or(0.0);

    let (free_trial_limit, free_trial_current, free_trial_expiry) =
        match credit.and_then(|c| c.free_trial_info.as_ref()) {
            Some(t) if free_trial_is_effective(t) => (
                pick_f64(t.usage_limit_with_precision, t.usage_limit),
                pick_f64(t.current_usage_with_precision, t.current_usage),
                t.free_trial_expiry.clone(),
            ),
            _ => (0.0, 0.0, None),
        };

    let bonuses: Vec<CreditBonus> = credit
        .and_then(|c| c.bonuses.as_ref())
        .map(|bs| {
            bs.iter()
                .filter(|b| bonus_is_effective(b))
                .map(|b| CreditBonus {
                    code: b.bonus_code.clone().unwrap_or_default(),
                    name: b.display_name.clone().unwrap_or_default(),
                    current: pick_f64(b.current_usage_with_precision, b.current_usage),
                    limit: pick_f64(b.usage_limit_with_precision, b.usage_limit),
                    expires_at: b.expires_at.clone(),
                })
                .collect()
        })
        .unwrap_or_default();

    let bonuses_limit: f64 = bonuses.iter().map(|b| b.limit).sum();
    let bonuses_current: f64 = bonuses.iter().map(|b| b.current).sum();

    let total_limit = base_limit + free_trial_limit + bonuses_limit;
    let total_current = base_current + free_trial_current + bonuses_current;

    let subscription_title = usage
        .subscription_info
        .as_ref()
        .and_then(|s| s.subscription_title.clone());

    let subscription_type = norm_subscription_type(subscription_title.as_deref());

    let email = usage
        .user_info
        .as_ref()
        .and_then(|u| u.email.clone())
        .or_else(|| user_info.as_ref().and_then(|u| u.email.clone()));

    let user_id = usage
        .user_info
        .as_ref()
        .and_then(|u| u.user_id.clone())
        .or_else(|| user_info.as_ref().and_then(|u| u.user_id.clone()));

    let overage_enabled = usage
        .overage_configuration
        .as_ref()
        .and_then(|o| o.overage_enabled);

    let resource_detail = credit.map(|c| CreditsResourceDetail {
        display_name: c.display_name.clone(),
        display_name_plural: c.display_name_plural.clone(),
        resource_type: c.resource_type.clone(),
        currency: c.currency.clone(),
        unit: c.unit.clone(),
        overage_rate: c.overage_rate,
        overage_cap: c.overage_cap,
    });

    AccountAggregateInfo {
        email,
        user_id,
        idp: user_info.as_ref().and_then(|u| u.idp.clone()),
        status: user_info.as_ref().and_then(|u| u.status.clone()),
        feature_flags: user_info.as_ref().and_then(|u| u.feature_flags.clone()),

        subscription_title,
        subscription_type,
        subscription: AccountSubscriptionDetails {
            raw_type: usage
                .subscription_info
                .as_ref()
                .and_then(|s| s.r#type.clone()),
            management_target: usage
                .subscription_info
                .as_ref()
                .and_then(|s| s.subscription_management_target.clone()),
            upgrade_capability: usage
                .subscription_info
                .as_ref()
                .and_then(|s| s.upgrade_capability.clone()),
            overage_capability: usage
                .subscription_info
                .as_ref()
                .and_then(|s| s.overage_capability.clone()),
        },

        usage: CreditsUsageSummary {
            current: total_current,
            limit: total_limit,

            base_current,
            base_limit,

            free_trial_current,
            free_trial_limit,
            free_trial_expiry,

            bonuses,

            next_reset_date: usage.next_date_reset.clone(),
            overage_enabled,

            resource_detail,
        },
        resources: usage
            .usage_breakdown_list
            .as_ref()
            .map(|l| {
                l.iter()
                    .map(|b| ResourceUsageSummary {
                        resource_type: b.resource_type.clone(),
                        display_name: b.display_name.clone(),
                        unit: b.unit.clone(),
                        currency: b.currency.clone(),
                        current: pick_f64(b.current_usage_with_precision, b.current_usage),
                        limit: pick_f64(b.usage_limit_with_precision, b.usage_limit),
                    })
                    .collect()
            })
            .unwrap_or_default(),
        raw_usage: usage,
    }
}
