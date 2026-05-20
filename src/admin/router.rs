//! Admin API 路由配置

use axum::{
    Router, middleware,
    routing::{delete, get, post},
};

use super::{
    handlers::{
        add_credential, delete_credential, enable_overage_sse, force_refresh_token,
        get_all_credentials, get_cached_balances, get_credential_balance, get_global_config,
        get_overage_status, get_proxy_config, import_token_json, reset_failure_count,
        set_credential_disabled, set_credential_endpoint, set_credential_idp,
        set_credential_priority, set_credential_proxy, set_credential_region, update_global_config,
        update_proxy_config,
    },
    middleware::{AdminState, admin_auth_middleware},
};

/// 创建 Admin API 路由
///
/// # 端点
/// - `GET /credentials` - 获取所有凭据状态
/// - `POST /credentials` - 添加新凭据
/// - `POST /credentials/import-token-json` - 批量导入 token.json
/// - `DELETE /credentials/:id` - 删除凭据
/// - `POST /credentials/:id/disabled` - 设置凭据禁用状态
/// - `POST /credentials/:id/priority` - 设置凭据优先级
/// - `POST /credentials/:id/reset` - 重置失败计数
/// - `GET /credentials/:id/balance` - 获取凭据余额
/// - `GET /credentials/balances/cached` - 获取所有凭据的缓存余额
///
/// # 认证
/// 需要 Admin API Key 认证，支持：
/// - `x-api-key` header
/// - `Authorization: Bearer <token>` header
pub fn create_admin_router(state: AdminState) -> Router {
    Router::new()
        .route(
            "/credentials",
            get(get_all_credentials).post(add_credential),
        )
        .route("/credentials/balances/cached", get(get_cached_balances))
        .route("/credentials/import-token-json", post(import_token_json))
        .route("/credentials/{id}", delete(delete_credential))
        .route("/credentials/{id}/disabled", post(set_credential_disabled))
        .route("/credentials/{id}/priority", post(set_credential_priority))
        .route("/credentials/{id}/region", post(set_credential_region))
        .route("/credentials/{id}/endpoint", post(set_credential_endpoint))
        .route("/credentials/{id}/idp", post(set_credential_idp))
        .route("/credentials/{id}/proxy", post(set_credential_proxy))
        .route("/credentials/{id}/overage", get(get_overage_status))
        .route("/credentials/{id}/overage/enable", get(enable_overage_sse))
        .route("/credentials/{id}/reset", post(reset_failure_count))
        .route("/credentials/{id}/refresh", post(force_refresh_token))
        .route("/credentials/{id}/balance", get(get_credential_balance))
        .route("/proxy", get(get_proxy_config).post(update_proxy_config))
        .route(
            "/config/global",
            get(get_global_config).put(update_global_config),
        )
        .layer(middleware::from_fn_with_state(
            state.clone(),
            admin_auth_middleware,
        ))
        .with_state(state)
}
