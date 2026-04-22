mod admin;
mod admin_ui;
mod anthropic;
mod common;
mod http_client;
pub mod image;
mod kiro;
mod model;
pub mod token;

use std::collections::HashMap;
use std::sync::Arc;

use clap::Parser;
use kiro::endpoint::{IdeEndpoint, KiroEndpoint};
use kiro::model::credentials::{CredentialsConfig, KiroCredentials};
use kiro::provider::KiroProvider;
use kiro::token_manager::MultiTokenManager;
use model::arg::Args;
use model::config::Config;
use parking_lot::RwLock;

#[tokio::main]
async fn main() {
    // 解析命令行参数
    let args = Args::parse();

    // 初始化日志
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    // 加载配置
    let config_path = args
        .config
        .unwrap_or_else(|| Config::default_config_path().to_string());
    let config = Config::load(&config_path).unwrap_or_else(|e| {
        tracing::error!("加载配置失败: {}", e);
        std::process::exit(1);
    });
    let config = Arc::new(RwLock::new(config));

    // 加载凭证（支持单对象或数组格式）
    let credentials_path = args
        .credentials
        .unwrap_or_else(|| KiroCredentials::default_credentials_path().to_string());
    let credentials_config = CredentialsConfig::load(&credentials_path).unwrap_or_else(|e| {
        tracing::error!("加载凭证失败: {}", e);
        std::process::exit(1);
    });

    // 判断是否为多凭据格式（用于刷新后回写）
    let is_multiple_format = credentials_config.is_multiple();

    // 转换为按优先级排序的凭据列表
    let mut credentials_list = credentials_config.into_sorted_credentials();

    if let Ok(kiro_api_key) = std::env::var("KIRO_API_KEY")
        && !kiro_api_key.trim().is_empty()
    {
        tracing::info!("检测到 KIRO_API_KEY 环境变量，添加 API Key 凭据（最高优先级）");
        credentials_list.insert(
            0,
            KiroCredentials {
                kiro_api_key: Some(kiro_api_key),
                auth_method: Some("api_key".to_string()),
                priority: u32::MIN,
                runtime_only: true,
                ..Default::default()
            },
        );
    }

    tracing::info!("已加载 {} 个凭据配置", credentials_list.len());

    let mut endpoints: HashMap<String, Arc<dyn KiroEndpoint>> = HashMap::new();
    {
        let ide: Arc<dyn KiroEndpoint> = Arc::new(IdeEndpoint::new());
        endpoints.insert(ide.name().to_string(), ide);
    }
    let endpoint_names: Vec<String> = endpoints.keys().cloned().collect();

    let default_endpoint = config.read().default_endpoint.clone();
    if !endpoints.contains_key(&default_endpoint) {
        tracing::error!(
            "第一阶段仅支持已注册 endpoint，当前 defaultEndpoint={} 不受支持，已注册: {:?}",
            default_endpoint,
            endpoint_names
        );
        std::process::exit(1);
    }
    for cred in &credentials_list {
        let endpoint = cred.effective_endpoint_name(Some(&default_endpoint));
        if !endpoints.contains_key(endpoint) {
            tracing::error!(
                "第一阶段仅支持已注册 endpoint，凭据 id={:?} 指定了未支持 endpoint={}，已注册: {:?}",
                cred.id,
                endpoint,
                endpoint_names
            );
            std::process::exit(1);
        }
    }

    // 获取第一个凭据用于日志显示
    let first_credentials = credentials_list.first().cloned().unwrap_or_default();
    #[cfg(feature = "sensitive-logs")]
    tracing::debug!("主凭证: {:?}", first_credentials);
    #[cfg(not(feature = "sensitive-logs"))]
    tracing::debug!(
        id = ?first_credentials.id,
        priority = first_credentials.priority,
        has_profile_arn = first_credentials.profile_arn.is_some(),
        has_expires_at = first_credentials.expires_at.is_some(),
        auth_method = ?first_credentials.auth_method.as_deref(),
        "主凭证摘要"
    );

    // 获取 API Key
    let api_key = config.read().api_key.clone().unwrap_or_else(|| {
        tracing::error!("配置文件中未设置 apiKey");
        std::process::exit(1);
    });

    // 构建代理配置
    let proxy_config = {
        let cfg = config.read();
        cfg.proxy_url.as_ref().map(|url| {
            let mut proxy = http_client::ProxyConfig::new(url);
            if let (Some(username), Some(password)) = (&cfg.proxy_username, &cfg.proxy_password) {
                proxy = proxy.with_auth(username, password);
            }
            proxy
        })
    };

    if proxy_config.is_some() {
        tracing::info!(
            "已配置 HTTP 代理: {}",
            config.read().proxy_url.as_ref().unwrap()
        );
    }

    // 创建 MultiTokenManager 和 KiroProvider
    let token_manager = MultiTokenManager::new(
        config.read().clone(),
        credentials_list,
        proxy_config.clone(),
        Some(credentials_path.into()),
        is_multiple_format,
    )
    .unwrap_or_else(|e| {
        tracing::error!("创建 Token 管理器失败: {}", e);
        std::process::exit(1);
    });
    let token_manager = Arc::new(token_manager);

    // 初始化余额缓存并按余额选择初始凭据
    let init_count = token_manager.initialize_balances().await;
    if init_count == 0 && token_manager.total_count() > 0 {
        tracing::warn!("所有凭据余额初始化失败，将按优先级选择凭据");
    }

    let kiro_provider = KiroProvider::with_proxy(
        token_manager.clone(),
        proxy_config.clone(),
        endpoints,
        default_endpoint.clone(),
    );
    let kiro_provider = Arc::new(kiro_provider);

    // 初始化 count_tokens 配置
    {
        let cfg = config.read();
        token::init_config(token::CountTokensConfig {
            api_url: cfg.count_tokens_api_url.clone(),
            api_key: cfg.count_tokens_api_key.clone(),
            auth_type: cfg.count_tokens_auth_type.clone(),
            proxy: proxy_config,
            tls_backend: cfg.tls_backend,
        });
    }

    // 创建共享的压缩配置（供 Anthropic 路由和 Admin API 共用，支持热更新）
    let compression_config = Arc::new(RwLock::new(config.read().compression.clone()));
    let prompt_cache_runtime = Arc::new(RwLock::new(anthropic::PromptCacheRuntime::new(
        config.read().prompt_cache_ttl_seconds,
        config.read().prompt_cache_accounting_enabled,
    )));

    // 构建 Anthropic API 路由（从第一个凭据获取 profile_arn）
    let anthropic_app = anthropic::create_router_with_provider(
        &api_key,
        Some(kiro_provider.clone()),
        first_credentials.profile_arn.clone(),
        compression_config.clone(),
        prompt_cache_runtime.clone(),
    );

    // 构建 Admin API 路由（如果配置了非空的 admin_api_key）
    // 安全检查：空字符串被视为未配置，防止空 key 绕过认证
    let admin_key_valid = config
        .read()
        .admin_api_key
        .as_ref()
        .map(|k| !k.trim().is_empty())
        .unwrap_or(false);

    let app = {
        let cfg = config.read();
        if let Some(admin_key) = &cfg.admin_api_key {
            if admin_key.trim().is_empty() {
                tracing::warn!("admin_api_key 配置为空，Admin API 未启用");
                anthropic_app
            } else {
                let admin_service = admin::AdminService::new(
                    token_manager.clone(),
                    Some(kiro_provider.clone()),
                    config.clone(),
                    compression_config.clone(),
                    prompt_cache_runtime.clone(),
                    endpoint_names.clone(),
                );
                let admin_state = admin::AdminState::new(admin_key, admin_service);
                let admin_app = admin::create_admin_router(admin_state);

                // 创建 Admin UI 路由
                let admin_ui_app = admin_ui::create_admin_ui_router();

                tracing::info!("Admin API 已启用");
                tracing::info!("Admin UI 已启用: /admin");
                anthropic_app
                    .nest("/api/admin", admin_app)
                    .nest("/admin", admin_ui_app)
            }
        } else {
            anthropic_app
        }
    };

    // 启动服务器
    let addr = {
        let cfg = config.read();
        format!("{}:{}", cfg.host, cfg.port)
    };
    tracing::info!("启动 Anthropic API 端点: {}", addr);
    #[cfg(feature = "sensitive-logs")]
    tracing::debug!("API Key: {}***", &api_key[..(api_key.len() / 2)]);
    #[cfg(not(feature = "sensitive-logs"))]
    tracing::info!(
        "API Key: ***{} (长度: {})",
        &api_key[api_key.len().saturating_sub(4)..],
        api_key.len()
    );
    tracing::info!("可用 API:");
    tracing::info!("  GET  /v1/models");
    tracing::info!("  POST /v1/messages");
    tracing::info!("  POST /v1/messages/count_tokens");
    if admin_key_valid {
        tracing::info!("Admin API:");
        tracing::info!("  GET  /api/admin/credentials");
        tracing::info!("  POST /api/admin/credentials/:index/disabled");
        tracing::info!("  POST /api/admin/credentials/:index/priority");
        tracing::info!("  POST /api/admin/credentials/:index/reset");
        tracing::info!("  GET  /api/admin/credentials/:index/balance");
        tracing::info!("Admin UI:");
        tracing::info!("  GET  /admin");
    }

    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .unwrap_or_else(|e| {
            tracing::error!("绑定监听地址失败 ({}): {}", addr, e);
            std::process::exit(1);
        });
    if let Err(e) = axum::serve(listener, app).await {
        tracing::error!("HTTP 服务异常退出: {}", e);
        std::process::exit(1);
    }
}
