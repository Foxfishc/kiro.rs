//! 后台开启超额（overage）任务的事件流
//!
//! 业务流程：
//! 1. 调用 `UpdateBillingPreferences` 把 `overageEnabled = true` 提交到 Kiro Web Portal；
//! 2. 轮询 `GetUserUsageAndLimits` 直到 `creditsUsageSummary.overageEnabled == true`
//!    （或超时，作业失败）；
//! 3. 每一步都通过 `tokio::sync::mpsc` 把进度事件投递给 SSE handler。
//!
//! 设计要点：
//! - 任务本身是「fire and forget」：handler 拿到 stream 后，即使断开连接，
//!   `tokio::spawn` 出来的后台任务仍会跑完（成功后落库 overage_enabled=true）。
//! - 同一个凭据并发开启时，由 `MultiTokenManager::try_begin_overage_task` 作为门闩。
//! - 失败原因通过 `finish_overage_task` 写回 `overage_last_error`，前端再次打开页面可以看到。
//!
//! 事件类型见 [`OverageEvent`]。

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::Serialize;
use tokio::sync::mpsc;
use tokio_stream::Stream;
use tokio_stream::wrappers::ReceiverStream;

use crate::kiro::token_manager::MultiTokenManager;
use crate::kiro::web_portal;

/// 单步事件，序列化后通过 SSE `data:` 字段下发到前端
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum OverageEvent {
    /// 准备阶段：拿到 token + idp + profileArn
    Prepared { idp: String, has_profile_arn: bool },
    /// 调用 UpdateBillingPreferences 前
    SubmittingUpdate,
    /// UpdateBillingPreferences 成功
    UpdateAccepted,
    /// 进入轮询阶段
    PollingStarted { interval_ms: u64, timeout_ms: u64 },
    /// 单次轮询结果
    PollTick {
        attempt: u32,
        overage_enabled: Option<bool>,
        elapsed_ms: u64,
    },
    /// 任务成功完成
    Done { overage_enabled: bool },
    /// 任务失败，message 是给用户看的简短中文原因
    Error { message: String },
}

/// 启动后台开启超额任务，返回事件流。
///
/// 调用方负责：
/// 1. 已通过 [`MultiTokenManager::try_begin_overage_task`] 获得独占执行权；
/// 2. 在 stream 终止后（无论是 Done 还是 Error）会自动调用 `finish_overage_task`
///    回填状态。
///
/// 内部用 `tokio::spawn` 跑实际工作，handler 只需把流接到 SSE。
pub fn start_overage_stream(
    manager: Arc<MultiTokenManager>,
    credential_id: u64,
) -> impl Stream<Item = OverageEvent> + Send + 'static {
    let (tx, rx) = mpsc::channel::<OverageEvent>(16);

    tokio::spawn(async move {
        run(manager, credential_id, tx).await;
    });

    ReceiverStream::new(rx)
}

/// 轮询参数：每秒一次，最多 30 秒
const POLL_INTERVAL: Duration = Duration::from_secs(1);
const POLL_TIMEOUT: Duration = Duration::from_secs(30);

async fn run(manager: Arc<MultiTokenManager>, credential_id: u64, tx: mpsc::Sender<OverageEvent>) {
    // 任务结束时无论成功失败都要把状态写回 manager
    let mut final_result: Result<bool, String> = Err("任务未完成".to_string());

    let outcome = run_inner(&manager, credential_id, &tx, &mut final_result).await;

    match outcome {
        Ok(()) => {}
        Err(message) => {
            let _ = tx.send(OverageEvent::Error { message }).await;
        }
    }

    manager.finish_overage_task(credential_id, final_result);
}

async fn run_inner(
    manager: &Arc<MultiTokenManager>,
    credential_id: u64,
    tx: &mpsc::Sender<OverageEvent>,
    final_result: &mut Result<bool, String>,
) -> Result<(), String> {
    // 1. 拿凭据上下文
    let ctx = manager
        .web_portal_context_for(credential_id)
        .await
        .map_err(|e| {
            let msg = format!("获取凭据上下文失败：{}", e);
            *final_result = Err(msg.clone());
            msg
        })?;

    let _ = tx
        .send(OverageEvent::Prepared {
            idp: ctx.idp.clone(),
            has_profile_arn: ctx.profile_arn.is_some(),
        })
        .await;

    let profile_arn = ctx.profile_arn.as_deref().ok_or_else(|| {
        let msg = "凭据缺少 profileArn，无法开启超额（请先刷新 Token）".to_string();
        *final_result = Err(msg.clone());
        msg
    })?;

    // 2. 提交 UpdateBillingPreferences
    let _ = tx.send(OverageEvent::SubmittingUpdate).await;
    web_portal::update_billing_preferences(
        &ctx.token,
        &ctx.idp,
        profile_arn,
        true,
        ctx.proxy.as_ref(),
    )
    .await
    .map_err(|e| {
        let msg = format!("UpdateBillingPreferences 调用失败：{}", e);
        *final_result = Err(msg.clone());
        msg
    })?;

    let _ = tx.send(OverageEvent::UpdateAccepted).await;

    // 3. 轮询 GetUserUsageAndLimits 直到 overageEnabled == true
    let _ = tx
        .send(OverageEvent::PollingStarted {
            interval_ms: POLL_INTERVAL.as_millis() as u64,
            timeout_ms: POLL_TIMEOUT.as_millis() as u64,
        })
        .await;

    let started = Instant::now();
    let mut attempt: u32 = 0;
    loop {
        attempt += 1;
        // 第一次立即查一次（很多场景上游写入是同步的），之后再 sleep
        if attempt > 1 {
            tokio::time::sleep(POLL_INTERVAL).await;
        }

        let elapsed = started.elapsed();
        let usage =
            web_portal::get_user_usage_and_limits(&ctx.token, &ctx.idp, ctx.proxy.as_ref()).await;

        let overage_enabled = usage
            .as_ref()
            .ok()
            .and_then(|u| u.overage_configuration.as_ref())
            .and_then(|c| c.overage_enabled);

        let _ = tx
            .send(OverageEvent::PollTick {
                attempt,
                overage_enabled,
                elapsed_ms: elapsed.as_millis() as u64,
            })
            .await;

        if let Some(true) = overage_enabled {
            manager.record_overage_status(credential_id, true);
            *final_result = Ok(true);
            let _ = tx
                .send(OverageEvent::Done {
                    overage_enabled: true,
                })
                .await;
            return Ok(());
        }

        if elapsed >= POLL_TIMEOUT {
            let msg = format!(
                "等待超额状态生效超时（{}s 内未观察到 overageEnabled=true）",
                POLL_TIMEOUT.as_secs()
            );
            *final_result = Err(msg.clone());
            return Err(msg);
        }
    }
}
