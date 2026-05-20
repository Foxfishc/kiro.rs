//! Admin API HTTP 处理器

use axum::{
    Json,
    extract::{Path, State},
    response::{IntoResponse, Sse, sse::Event},
};
use futures::StreamExt;

use super::{
    middleware::AdminState,
    types::{
        AddCredentialRequest, ImportTokenJsonRequest, OverageStatusResponse,
        SetCredentialProxyRequest, SetDisabledRequest, SetEndpointRequest, SetIdpRequest,
        SetPriorityRequest, SetRegionRequest, SuccessResponse, UpdateProxyConfigRequest,
    },
};
use crate::kiro::overage;

/// GET /api/admin/credentials
/// 获取所有凭据状态
pub async fn get_all_credentials(State(state): State<AdminState>) -> impl IntoResponse {
    let response = state.service.get_all_credentials();
    Json(response)
}

/// POST /api/admin/credentials/:id/disabled
/// 设置凭据禁用状态
pub async fn set_credential_disabled(
    State(state): State<AdminState>,
    Path(id): Path<u64>,
    Json(payload): Json<SetDisabledRequest>,
) -> impl IntoResponse {
    match state.service.set_disabled(id, payload.disabled) {
        Ok(_) => {
            let action = if payload.disabled { "禁用" } else { "启用" };
            Json(SuccessResponse::new(format!("凭据 #{} 已{}", id, action))).into_response()
        }
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// POST /api/admin/credentials/:id/priority
/// 设置凭据优先级
pub async fn set_credential_priority(
    State(state): State<AdminState>,
    Path(id): Path<u64>,
    Json(payload): Json<SetPriorityRequest>,
) -> impl IntoResponse {
    match state.service.set_priority(id, payload.priority) {
        Ok(_) => Json(SuccessResponse::new(format!(
            "凭据 #{} 优先级已设置为 {}",
            id, payload.priority
        )))
        .into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// POST /api/admin/credentials/:id/region
/// 设置凭据 Region
pub async fn set_credential_region(
    State(state): State<AdminState>,
    Path(id): Path<u64>,
    Json(payload): Json<SetRegionRequest>,
) -> impl IntoResponse {
    match state
        .service
        .set_region(id, payload.region, payload.api_region)
    {
        Ok(_) => Json(SuccessResponse::new(format!("凭据 #{} Region 已更新", id))).into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// POST /api/admin/credentials/:id/endpoint
/// 设置凭据 endpoint
pub async fn set_credential_endpoint(
    State(state): State<AdminState>,
    Path(id): Path<u64>,
    Json(payload): Json<SetEndpointRequest>,
) -> impl IntoResponse {
    match state.service.set_endpoint(id, payload.endpoint) {
        Ok(_) => Json(SuccessResponse::new(format!(
            "凭据 #{} endpoint 已更新",
            id
        )))
        .into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// POST /api/admin/credentials/:id/reset
/// 重置失败计数并重新启用
pub async fn reset_failure_count(
    State(state): State<AdminState>,
    Path(id): Path<u64>,
) -> impl IntoResponse {
    match state.service.reset_and_enable(id) {
        Ok(_) => Json(SuccessResponse::new(format!(
            "凭据 #{} 失败计数已重置并重新启用",
            id
        )))
        .into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// POST /api/admin/credentials/:id/refresh
/// 强制刷新指定凭据 Token
pub async fn force_refresh_token(
    State(state): State<AdminState>,
    Path(id): Path<u64>,
) -> impl IntoResponse {
    match state.service.force_refresh_token(id).await {
        Ok(_) => Json(SuccessResponse::new(format!(
            "凭据 #{} Token 已强制刷新",
            id
        )))
        .into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// GET /api/admin/credentials/:id/balance
/// 获取指定凭据的余额
pub async fn get_credential_balance(
    State(state): State<AdminState>,
    Path(id): Path<u64>,
) -> impl IntoResponse {
    match state.service.get_balance(id).await {
        Ok(response) => Json(response).into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// GET /api/admin/credentials/balances/cached
/// 获取所有凭据的缓存余额
pub async fn get_cached_balances(State(state): State<AdminState>) -> impl IntoResponse {
    Json(state.service.get_cached_balances())
}

/// POST /api/admin/credentials
/// 添加新凭据
pub async fn add_credential(
    State(state): State<AdminState>,
    Json(payload): Json<AddCredentialRequest>,
) -> impl IntoResponse {
    match state.service.add_credential(payload).await {
        Ok(response) => Json(response).into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// DELETE /api/admin/credentials/:id
/// 删除凭据
pub async fn delete_credential(
    State(state): State<AdminState>,
    Path(id): Path<u64>,
) -> impl IntoResponse {
    match state.service.delete_credential(id) {
        Ok(_) => Json(SuccessResponse::new(format!("凭据 #{} 已删除", id))).into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// POST /api/admin/credentials/import-token-json
/// 批量导入 token.json
pub async fn import_token_json(
    State(state): State<AdminState>,
    Json(payload): Json<ImportTokenJsonRequest>,
) -> impl IntoResponse {
    let response = state.service.import_token_json(payload).await;
    Json(response)
}

/// GET /proxy - 获取全局代理配置
pub async fn get_proxy_config(State(state): State<AdminState>) -> impl IntoResponse {
    Json(state.service.get_proxy_config())
}

/// POST /proxy - 更新全局代理配置
pub async fn update_proxy_config(
    State(state): State<AdminState>,
    Json(req): Json<UpdateProxyConfigRequest>,
) -> impl IntoResponse {
    match state.service.update_proxy_config(req).await {
        Ok(_) => Json(SuccessResponse::new("全局代理配置已更新")).into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// GET /api/admin/config/global - 获取全局配置
pub async fn get_global_config(State(state): State<AdminState>) -> impl IntoResponse {
    let response = state.service.get_global_config();
    Json(response)
}

/// PUT /api/admin/config/global - 更新全局配置
pub async fn update_global_config(
    State(state): State<AdminState>,
    Json(req): Json<super::types::UpdateGlobalConfigRequest>,
) -> impl IntoResponse {
    match state.service.update_global_config(req).await {
        Ok(_) => Json(SuccessResponse::new("全局配置已更新")).into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// POST /api/admin/credentials/:id/idp - 设置凭据级 Web Portal Idp
pub async fn set_credential_idp(
    State(state): State<AdminState>,
    Path(id): Path<u64>,
    Json(payload): Json<SetIdpRequest>,
) -> impl IntoResponse {
    match state.service.set_idp(id, payload.idp) {
        Ok(_) => Json(SuccessResponse::new(format!("凭据 #{} idp 已更新", id))).into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// POST /api/admin/credentials/:id/proxy - 设置凭据级代理
pub async fn set_credential_proxy(
    State(state): State<AdminState>,
    Path(id): Path<u64>,
    Json(payload): Json<SetCredentialProxyRequest>,
) -> impl IntoResponse {
    match state.service.set_credential_proxy(
        id,
        payload.proxy_url,
        payload.proxy_username,
        payload.proxy_password,
    ) {
        Ok(_) => Json(SuccessResponse::new(format!("凭据 #{} 代理已更新", id))).into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// GET /api/admin/credentials/:id/overage - 读取凭据 overage 状态
pub async fn get_overage_status(
    State(state): State<AdminState>,
    Path(id): Path<u64>,
) -> impl IntoResponse {
    match state.service.overage_status(id) {
        Ok(snap) => Json(OverageStatusResponse {
            id: snap.id,
            enabled: snap.enabled,
            enabling: snap.enabling,
            last_error: snap.last_error,
            has_profile_arn: snap.has_profile_arn,
            auth_method: snap.auth_method,
        })
        .into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// GET /api/admin/credentials/:id/overage/enable - 开启 overage（SSE 流）
///
/// 1. 通过 `try_begin_overage_task` 占用执行权（防止并发触发）；
/// 2. 后台 spawn 一个轮询任务，把进度事件推到 SSE；
/// 3. 客户端断开不会取消后台任务（fire-and-forget），任务完成后状态会落到
///    `overage_enabled` / `overage_last_error`，前端再次打开页面可以看到。
pub async fn enable_overage_sse(
    State(state): State<AdminState>,
    Path(id): Path<u64>,
) -> axum::response::Response {
    use axum::http::StatusCode;

    match state.service.try_begin_overage_task(id) {
        Ok(true) => {
            let manager = state.service.token_manager_arc();
            let stream = overage::start_overage_stream(manager, id).map(|event| {
                Event::default()
                    .json_data(&event)
                    .or_else(|_| Ok::<Event, std::convert::Infallible>(Event::default()))
            });
            Sse::new(stream)
                .keep_alive(axum::response::sse::KeepAlive::default())
                .into_response()
        }
        Ok(false) => (
            StatusCode::CONFLICT,
            Json(serde_json::json!({
                "error": "overage 任务正在进行中，请稍后再试或刷新查看状态",
                "credentialId": id,
            })),
        )
            .into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}
