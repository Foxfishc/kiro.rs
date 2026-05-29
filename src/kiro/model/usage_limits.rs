//! 使用额度查询数据模型
//!
//! 包含 getUsageLimits API 的响应类型定义

use serde::Deserialize;

/// Kiro 开启 overage 后额外允许的 CREDIT 额度。
///
/// Admin UI 当前也按 10000 展示；部分 getUsageLimits 响应不一定稳定返回 overageCap，
/// 因此在 overageEnabled=true 但缺少 overageCap 时使用该兜底值，避免把已开启超额的凭据误判为余额不足。
pub const DEFAULT_OVERAGE_CAP: f64 = 10_000.0;

/// 使用额度查询响应
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageLimitsResponse {
    /// 下次重置日期 (Unix 时间戳)
    #[serde(default)]
    #[allow(dead_code)]
    pub next_date_reset: Option<f64>,

    /// 订阅信息
    #[serde(default)]
    pub subscription_info: Option<SubscriptionInfo>,

    /// 使用量明细列表
    #[serde(default)]
    pub usage_breakdown_list: Vec<UsageBreakdown>,

    /// 超额配置（开启后基础额度用完仍有额外可用额度）
    #[serde(default)]
    pub overage_configuration: Option<OverageConfiguration>,
}

/// 超额配置
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OverageConfiguration {
    /// 是否开启超额
    #[serde(default)]
    pub overage_enabled: Option<bool>,
}

/// 订阅信息
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubscriptionInfo {
    /// 订阅标题 (KIRO PRO+ / KIRO FREE 等)
    #[serde(default)]
    pub subscription_title: Option<String>,
    /// 订阅超额能力标识（"OVERAGE_CAPABLE" 表示支持超额）
    #[serde(default)]
    pub overage_capability: Option<String>,
}

/// 使用量明细
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageBreakdown {
    /// 当前使用量
    #[serde(default)]
    #[allow(dead_code)]
    pub current_usage: i64,

    /// 当前使用量（精确值）
    #[serde(default)]
    pub current_usage_with_precision: f64,

    /// 奖励额度列表（可能为 null）
    #[serde(default)]
    pub bonuses: Option<Vec<Bonus>>,

    /// 免费试用信息
    #[serde(default)]
    pub free_trial_info: Option<FreeTrialInfo>,

    /// 下次重置日期 (Unix 时间戳)
    #[serde(default)]
    #[allow(dead_code)]
    pub next_date_reset: Option<f64>,

    /// 使用限额
    #[serde(default)]
    #[allow(dead_code)]
    pub usage_limit: i64,

    /// 使用限额（精确值）
    #[serde(default)]
    pub usage_limit_with_precision: f64,

    /// 超额上限（开启 overage 后的额外额度；部分响应可能缺失）
    #[serde(default)]
    pub overage_cap: Option<f64>,
}

/// 奖励额度
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Bonus {
    /// 当前使用量
    #[serde(default)]
    pub current_usage: f64,

    /// 使用限额
    #[serde(default)]
    pub usage_limit: f64,

    /// 状态 (ACTIVE / EXPIRED)
    #[serde(default)]
    pub status: Option<String>,
}

impl Bonus {
    /// 检查 bonus 是否处于激活状态（大小写不敏感）
    pub fn is_active(&self) -> bool {
        self.status
            .as_deref()
            .map(|s| s.eq_ignore_ascii_case("ACTIVE"))
            .unwrap_or(false)
    }
}

/// 免费试用信息
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FreeTrialInfo {
    /// 当前使用量
    #[serde(default)]
    #[allow(dead_code)]
    pub current_usage: i64,

    /// 当前使用量（精确值）
    #[serde(default)]
    pub current_usage_with_precision: f64,

    /// 免费试用过期时间 (Unix 时间戳)
    #[serde(default)]
    #[allow(dead_code)]
    pub free_trial_expiry: Option<f64>,

    /// 免费试用状态 (ACTIVE / EXPIRED)
    #[serde(default)]
    pub free_trial_status: Option<String>,

    /// 使用限额
    #[serde(default)]
    #[allow(dead_code)]
    pub usage_limit: i64,

    /// 使用限额（精确值）
    #[serde(default)]
    pub usage_limit_with_precision: f64,
}

// ============ 便捷方法实现 ============

impl FreeTrialInfo {
    /// 检查免费试用是否处于激活状态
    pub fn is_active(&self) -> bool {
        self.free_trial_status
            .as_deref()
            .map(|s| s == "ACTIVE")
            .unwrap_or(false)
    }
}

impl UsageLimitsResponse {
    /// 获取订阅标题
    pub fn subscription_title(&self) -> Option<&str> {
        self.subscription_info
            .as_ref()
            .and_then(|info| info.subscription_title.as_deref())
    }

    /// 获取订阅超额能力标识（"OVERAGE_CAPABLE" 表示该套餐支持开启超额）
    pub fn overage_capability(&self) -> Option<&str> {
        self.subscription_info
            .as_ref()
            .and_then(|info| info.overage_capability.as_deref())
    }

    /// 获取第一个使用量明细
    fn primary_breakdown(&self) -> Option<&UsageBreakdown> {
        self.usage_breakdown_list.first()
    }

    /// 获取总使用限额（精确值）
    ///
    /// 累加基础额度、激活的免费试用额度和激活的奖励额度
    #[allow(clippy::collapsible_if)]
    pub fn usage_limit(&self) -> f64 {
        let Some(breakdown) = self.primary_breakdown() else {
            return 0.0;
        };

        let mut total = breakdown.usage_limit_with_precision;

        // 累加激活的 free trial 额度
        if let Some(trial) = &breakdown.free_trial_info {
            if trial.is_active() {
                total += trial.usage_limit_with_precision;
            }
        }

        // 累加激活的 bonus 额度
        if let Some(bonuses) = &breakdown.bonuses {
            for bonus in bonuses {
                if bonus.is_active() {
                    total += bonus.usage_limit;
                }
            }
        }

        total
    }

    /// 获取总当前使用量（精确值）
    ///
    /// 累加基础使用量、激活的免费试用使用量和激活的奖励使用量
    #[allow(clippy::collapsible_if)]
    pub fn current_usage(&self) -> f64 {
        let Some(breakdown) = self.primary_breakdown() else {
            return 0.0;
        };

        let mut total = breakdown.current_usage_with_precision;

        // 累加激活的 free trial 使用量
        if let Some(trial) = &breakdown.free_trial_info {
            if trial.is_active() {
                total += trial.current_usage_with_precision;
            }
        }

        // 累加激活的 bonus 使用量
        if let Some(bonuses) = &breakdown.bonuses {
            for bonus in bonuses {
                if bonus.is_active() {
                    total += bonus.current_usage;
                }
            }
        }

        total
    }

    /// 当前响应是否表示已开启超额
    pub fn overage_enabled_reported(&self) -> Option<bool> {
        self.overage_configuration
            .as_ref()
            .and_then(|cfg| cfg.overage_enabled)
    }

    /// 当前响应是否表示已开启超额
    pub fn overage_enabled(&self) -> bool {
        self.overage_enabled_reported().unwrap_or(false)
    }

    /// 获取超额额度上限。
    ///
    /// 上游有时只返回 overageEnabled=true，不稳定返回 overageCap；此时按产品/UI 约定兜底为 10000。
    pub fn overage_cap(&self) -> f64 {
        self.overage_cap_for_enabled(self.overage_enabled())
    }

    /// 按调用方确认的 overage 状态获取超额额度上限。
    ///
    /// 有些 getUsageLimits 响应不返回 overageConfiguration；调用方可能已通过
    /// Web Portal/GetUserUsageAndLimits 或持久化状态确认 overage 已开启，此时仍应
    /// 把 cap 计入余额展示和禁用判断。
    pub fn overage_cap_for_enabled(&self, enabled: bool) -> f64 {
        if !enabled {
            return 0.0;
        }

        self.primary_breakdown()
            .and_then(|breakdown| breakdown.overage_cap)
            .filter(|cap| cap.is_finite() && *cap > 0.0)
            .unwrap_or(DEFAULT_OVERAGE_CAP)
    }

    /// 获取用于“是否还有余额”的有效总限额。
    ///
    /// 与 `usage_limit()` 的区别：这里会在 overageEnabled=true 时追加 overage cap，
    /// 避免基础额度用完但超额已开启时被自动禁用。
    pub fn effective_usage_limit(&self) -> f64 {
        self.usage_limit() + self.overage_cap()
    }

    /// 使用调用方确认的 overage 状态计算有效总限额。
    pub fn effective_usage_limit_for_overage(&self, overage_enabled: bool) -> f64 {
        self.usage_limit() + self.overage_cap_for_enabled(overage_enabled)
    }

    /// 获取有效剩余额度（不会返回负数）。
    pub fn remaining_balance(&self) -> f64 {
        (self.effective_usage_limit() - self.current_usage()).max(0.0)
    }

    /// 使用调用方确认的 overage 状态计算有效剩余额度。
    pub fn remaining_balance_for_overage(&self, overage_enabled: bool) -> f64 {
        (self.effective_usage_limit_for_overage(overage_enabled) - self.current_usage()).max(0.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remaining_balance_includes_default_overage_cap_when_enabled() {
        let resp: UsageLimitsResponse = serde_json::from_str(
            r#"{
                "overageConfiguration": { "overageEnabled": true },
                "usageBreakdownList": [
                    {
                        "currentUsageWithPrecision": 1001.0,
                        "usageLimitWithPrecision": 1000.0
                    }
                ]
            }"#,
        )
        .unwrap();

        assert_eq!(resp.usage_limit(), 1000.0);
        assert_eq!(resp.current_usage(), 1001.0);
        assert_eq!(resp.effective_usage_limit(), 11_000.0);
        assert_eq!(resp.remaining_balance(), 9_999.0);
    }

    #[test]
    fn remaining_balance_uses_explicit_overage_cap_when_present() {
        let resp: UsageLimitsResponse = serde_json::from_str(
            r#"{
                "overageConfiguration": { "overageEnabled": true },
                "usageBreakdownList": [
                    {
                        "currentUsageWithPrecision": 1200.0,
                        "usageLimitWithPrecision": 1000.0,
                        "overageCap": 500.0
                    }
                ]
            }"#,
        )
        .unwrap();

        assert_eq!(resp.effective_usage_limit(), 1500.0);
        assert_eq!(resp.remaining_balance(), 300.0);
    }

    #[test]
    fn remaining_balance_does_not_include_overage_when_disabled() {
        let resp: UsageLimitsResponse = serde_json::from_str(
            r#"{
                "overageConfiguration": { "overageEnabled": false },
                "usageBreakdownList": [
                    {
                        "currentUsageWithPrecision": 1001.0,
                        "usageLimitWithPrecision": 1000.0,
                        "overageCap": 10000.0
                    }
                ]
            }"#,
        )
        .unwrap();

        assert_eq!(resp.effective_usage_limit(), 1000.0);
        assert_eq!(resp.remaining_balance(), 0.0);
    }
}
