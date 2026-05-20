//! Kiro CLI 端点实现

use reqwest::RequestBuilder;
use uuid::Uuid;

use super::{KiroEndpoint, RequestContext, UsageRequestParts};
use crate::kiro::model::credentials::KiroCredentials;

pub const CLI_ENDPOINT_NAME: &str = "cli";
const CLI_ORIGIN: &str = "KIRO_CLI";
// Verified against `kiro-cli 2.3.0` Q_LOG_LEVEL=trace capture 2026-05-12:
//   x-amz-target: AmazonCodeWhispererStreamingService.GenerateAssistantResponse
//   content-type: application/x-amz-json-1.0
//   user-agent:   aws-sdk-rust/1.3.15 ua/2.1 api/codewhispererstreaming/0.1.14474 os/{os} lang/rust/1.92.0 md/appVersion-2.3.0 app/AmazonQ-For-CLI
// NOTE: kiro-cli does NOT send `x-amzn-kiro-agent-mode` on this endpoint.
const CLI_API_TARGET: &str = "AmazonCodeWhispererStreamingService.GenerateAssistantResponse";
const CLI_STREAMING_API_VERSION: &str = "0.1.14474";
const CLI_RUNTIME_API_VERSION: &str = "0.1.14474";
const CLI_RUST_SDK_VERSION: &str = "1.3.15";
const CLI_RUST_VERSION: &str = "1.92.0";
const CLI_APP_VERSION: &str = "2.3.0";

fn cli_os_tag() -> &'static str {
    if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(target_os = "windows") {
        "windows"
    } else {
        "linux"
    }
}

pub struct CliEndpoint;

impl CliEndpoint {
    pub fn new() -> Self {
        Self
    }

    fn host(&self, ctx: &RequestContext<'_>) -> String {
        format!(
            "q.{}.amazonaws.com",
            ctx.credentials.effective_api_region(ctx.config)
        )
    }

    fn api_origin(&self) -> &'static str {
        CLI_ORIGIN
    }

    fn streaming_user_agent(&self) -> String {
        format!(
            "aws-sdk-rust/{sdk} ua/2.1 api/codewhispererstreaming/{api} os/{os} lang/rust/{rust} md/appVersion-{app} app/AmazonQ-For-CLI",
            sdk = CLI_RUST_SDK_VERSION,
            api = CLI_STREAMING_API_VERSION,
            os = cli_os_tag(),
            rust = CLI_RUST_VERSION,
            app = CLI_APP_VERSION,
        )
    }

    fn streaming_x_amz_user_agent(&self) -> String {
        format!(
            "aws-sdk-rust/{sdk} ua/2.1 api/codewhispererstreaming/{api} os/{os} lang/rust/{rust} m/F app/AmazonQ-For-CLI",
            sdk = CLI_RUST_SDK_VERSION,
            api = CLI_STREAMING_API_VERSION,
            os = cli_os_tag(),
            rust = CLI_RUST_VERSION,
        )
    }

    fn runtime_user_agent(&self) -> String {
        format!(
            "aws-sdk-rust/{sdk} ua/2.1 api/codewhispererruntime/{api} os/{os} lang/rust/{rust} md/appVersion-{app} app/AmazonQ-For-CLI",
            sdk = CLI_RUST_SDK_VERSION,
            api = CLI_RUNTIME_API_VERSION,
            os = cli_os_tag(),
            rust = CLI_RUST_VERSION,
            app = CLI_APP_VERSION,
        )
    }

    fn runtime_x_amz_user_agent(&self) -> String {
        format!(
            "aws-sdk-rust/{sdk} ua/2.1 api/codewhispererruntime/{api} os/{os} lang/rust/{rust} m/F app/AmazonQ-For-CLI",
            sdk = CLI_RUST_SDK_VERSION,
            api = CLI_RUNTIME_API_VERSION,
            os = cli_os_tag(),
            rust = CLI_RUST_VERSION,
        )
    }

    /// Rewrite `origin` and `modelId` to CLI-canonical values, but only on the
    /// userInputMessage nodes we know are protocol fields. The earlier naive
    /// implementation walked the entire JSON tree, which would silently
    /// overwrite user-supplied tool inputSchema properties named "origin" or
    /// "modelId" (since `Tool.toolSpecification.inputSchema.json` is an opaque
    /// `serde_json::Value` the user controls).
    ///
    /// Targets:
    ///   - conversationState.currentMessage.userInputMessage.{origin, modelId}
    ///   - conversationState.history[*].userInputMessage.{origin, modelId}
    fn rewrite_origin_and_model(state: &mut serde_json::Value) {
        fn set_uim(uim: &mut serde_json::Value) {
            let Some(obj) = uim.as_object_mut() else {
                return;
            };
            if obj.contains_key("origin") {
                obj.insert(
                    "origin".to_string(),
                    serde_json::Value::String(CLI_ORIGIN.to_string()),
                );
            }
            // kiro-cli 2.3.0 always sends modelId="auto" — the server picks the
            // model based on the user's subscription tier. Pinning specific
            // model IDs (CLAUDE_SONNET_4_..., etc.) returns 400 "Improperly
            // formed request" empirically.
            if obj.contains_key("modelId") {
                obj.insert(
                    "modelId".to_string(),
                    serde_json::Value::String("auto".to_string()),
                );
            }
        }
        let Some(cs) = state
            .get_mut("conversationState")
            .and_then(|v| v.as_object_mut())
        else {
            return;
        };
        if let Some(uim) = cs
            .get_mut("currentMessage")
            .and_then(|v| v.get_mut("userInputMessage"))
        {
            set_uim(uim);
        }
        if let Some(hist) = cs.get_mut("history").and_then(|v| v.as_array_mut()) {
            for entry in hist {
                if let Some(uim) = entry.get_mut("userInputMessage") {
                    set_uim(uim);
                }
            }
        }
    }

    /// kiro-cli wraps currentMessage.content with these markers (verified):
    ///   --- CONTEXT ENTRY BEGIN ---
    ///   Current time: <RFC3339 with offset>
    ///   --- CONTEXT ENTRY END ---
    ///
    ///   --- USER MESSAGE BEGIN ---
    ///   <user text>--- USER MESSAGE END ---
    ///
    /// We only wrap if the content isn't already wrapped (idempotent — guards
    /// against re-wrap when a transform_body runs twice in retry paths).
    fn wrap_current_message_content(state: &mut serde_json::Value) {
        let Some(current) = state.get_mut("currentMessage") else {
            return;
        };
        let Some(uim) = current.get_mut("userInputMessage") else {
            return;
        };
        let Some(content) = uim
            .get_mut("content")
            .and_then(|v| v.as_str())
            .map(String::from)
        else {
            return;
        };
        if content.contains("--- USER MESSAGE BEGIN ---")
            || content.contains("--- CONTEXT ENTRY BEGIN ---")
        {
            return; // already wrapped
        }
        let now = chrono::Local::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, false);
        // kiro-cli format: "Tuesday, 2026-05-12T20:25:05.551+08:00"
        let weekday = chrono::Local::now().format("%A").to_string();
        let wrapped = format!(
            "--- CONTEXT ENTRY BEGIN ---\nCurrent time: {weekday}, {now}\n--- CONTEXT ENTRY END ---\n\n--- USER MESSAGE BEGIN ---\n{content}--- USER MESSAGE END ---"
        );
        uim["content"] = serde_json::Value::String(wrapped);
    }

    fn inject_env_state(state: &mut serde_json::Value) {
        let env_state = serde_json::json!({
            "operatingSystem": if cfg!(target_os = "macos") { "macos" }
                              else if cfg!(target_os = "windows") { "windows" }
                              else { "linux" },
            "currentWorkingDirectory": std::env::current_dir()
                .ok()
                .and_then(|p| p.to_str().map(String::from))
                .unwrap_or_default(),
        });

        // currentMessage.userInputMessage.userInputMessageContext.envState
        if let Some(ctx) = state
            .get_mut("currentMessage")
            .and_then(|v| v.get_mut("userInputMessage"))
            .and_then(|v| v.get_mut("userInputMessageContext"))
            .and_then(|v| v.as_object_mut())
            && !ctx.contains_key("envState")
        {
            // Insert envState at the head so it precedes `tools` (matches
            // wire order). serde_json::Map preserves insertion order.
            let mut new_ctx = serde_json::Map::new();
            new_ctx.insert("envState".to_string(), env_state.clone());
            for (k, v) in ctx.iter() {
                new_ctx.insert(k.clone(), v.clone());
            }
            *ctx = new_ctx;
        }

        // history[*].userInputMessage.userInputMessageContext.envState
        // (history user turns also carry envState but NOT tools — golden shows
        // history[0].ctx = { envState } only.)
        // CRITICAL: kiro-cli wire order is { content, userInputMessageContext,
        // origin, modelId }. When ctx was skip_if_empty in serde, it's missing
        // from the map and a naive insert appends at end. We rebuild the map so
        // userInputMessageContext sits at the correct slot (right after content).
        if let Some(hist) = state.get_mut("history").and_then(|v| v.as_array_mut()) {
            for entry in hist {
                let Some(uim) = entry.get_mut("userInputMessage") else {
                    continue;
                };
                let Some(obj) = uim.as_object_mut() else {
                    continue;
                };

                // Build / update the ctx with envState first.
                let existing_ctx = obj
                    .remove("userInputMessageContext")
                    .unwrap_or_else(|| serde_json::json!({}));
                let mut new_ctx_map = serde_json::Map::new();
                new_ctx_map.insert("envState".to_string(), env_state.clone());
                if let serde_json::Value::Object(old_ctx) = existing_ctx {
                    for (k, v) in old_ctx {
                        if k != "envState" {
                            new_ctx_map.insert(k, v);
                        }
                    }
                }
                let new_ctx_val = serde_json::Value::Object(new_ctx_map);

                // Re-insert keys in golden wire order: content, ctx, origin, modelId, images.
                let content = obj.remove("content");
                let origin = obj.remove("origin");
                let model_id = obj.remove("modelId");
                let images = obj.remove("images");
                obj.clear();
                if let Some(v) = content {
                    obj.insert("content".to_string(), v);
                }
                obj.insert("userInputMessageContext".to_string(), new_ctx_val);
                if let Some(v) = origin {
                    obj.insert("origin".to_string(), v);
                }
                if let Some(v) = model_id {
                    obj.insert("modelId".to_string(), v);
                }
                if let Some(v) = images {
                    obj.insert("images".to_string(), v);
                }
            }
        }
    }

    /// Per-request `profileArn` injection (multi-credential safe).
    ///
    /// Without this, every request reuses `state.profile_arn` — a snapshot of
    /// the FIRST credential taken at startup (main.rs). When the pool rotates
    /// to a different credential, the request would carry credential[i]'s
    /// bearer token but credential[0]'s ARN — server-side this either 4xx's
    /// or accounts usage to the wrong tenant.
    ///
    /// Same auth-method gating as IDE endpoint: AWS SSO OIDC (`idc` /
    /// `builder-id`) credentials must NOT send profileArn — strip it.
    fn inject_profile_arn(body: &str, credentials: &KiroCredentials) -> anyhow::Result<String> {
        let mut request: serde_json::Value = serde_json::from_str(body)?;
        let Some(obj) = request.as_object_mut() else {
            return Ok(body.to_string());
        };
        let is_sso_oidc = matches!(
            credentials.auth_method.as_deref(),
            Some("builder-id") | Some("idc")
        ) || (credentials.client_id.is_some()
            && credentials.client_secret.is_some());
        if is_sso_oidc {
            obj.remove("profileArn");
        } else if let Some(arn) = credentials.profile_arn.as_deref() {
            obj.insert(
                "profileArn".to_string(),
                serde_json::Value::String(arn.to_string()),
            );
        }
        Ok(serde_json::to_string(&request)?)
    }

    fn transform_body(&self, body: &str, credentials: &KiroCredentials) -> anyhow::Result<String> {
        let mut request: serde_json::Value = serde_json::from_str(body)?;
        Self::rewrite_origin_and_model(&mut request);
        if let Some(cs) = request.get_mut("conversationState") {
            Self::wrap_current_message_content(cs);
            Self::inject_env_state(cs);
        }
        let body = serde_json::to_string(&request)?;
        Self::inject_profile_arn(&body, credentials)
    }
}

impl Default for CliEndpoint {
    fn default() -> Self {
        Self::new()
    }
}

impl KiroEndpoint for CliEndpoint {
    fn name(&self) -> &'static str {
        CLI_ENDPOINT_NAME
    }

    fn api_url(&self, ctx: &RequestContext<'_>) -> String {
        format!(
            "https://q.{}.amazonaws.com/generateAssistantResponse",
            ctx.credentials.effective_api_region(ctx.config)
        )
    }

    fn mcp_url(&self, ctx: &RequestContext<'_>) -> String {
        format!(
            "https://q.{}.amazonaws.com/mcp",
            ctx.credentials.effective_api_region(ctx.config)
        )
    }

    fn decorate_api(&self, req: RequestBuilder, ctx: &RequestContext<'_>) -> RequestBuilder {
        // Headers byte-aligned with kiro-cli 2.3.0 (Q_LOG_LEVEL=trace 2026-05-12):
        //   content-type: application/x-amz-json-1.0   (AWS JSON 1.0 framing — REQUIRED;
        //                                                 application/json gets 4xx)
        //   x-amz-target dispatches the RPC by name
        //   x-amzn-codewhisperer-optout: "false"  (literal lowercase string, not bool)
        //   x-amz-user-agent uses `m/F` (not `m/E`)
        //   amz-sdk-request: attempt=1; max=3
        //   NO `x-amzn-kiro-agent-mode` (that's IDE-only)
        //   NO `Accept: */*` (reqwest sets it automatically; kiro-cli trace shows the
        //                      SDK never sends an explicit Accept on this endpoint)
        let mut req = req
            .header("content-type", "application/x-amz-json-1.0")
            .header("X-Amz-Target", CLI_API_TARGET)
            .header("x-amzn-codewhisperer-optout", "false")
            .header("x-amz-user-agent", self.streaming_x_amz_user_agent())
            .header("user-agent", self.streaming_user_agent())
            .header("host", self.host(ctx))
            .header("amz-sdk-invocation-id", Uuid::new_v4().to_string())
            .header("amz-sdk-request", "attempt=1; max=3")
            .header("Authorization", format!("Bearer {}", ctx.token));

        if ctx.credentials.is_api_key_credential() {
            req = req.header("tokentype", "API_KEY");
        }
        req
    }

    fn decorate_mcp(&self, req: RequestBuilder, ctx: &RequestContext<'_>) -> RequestBuilder {
        let mut req = req
            .header("content-type", "application/json")
            .header("x-amz-user-agent", self.streaming_x_amz_user_agent())
            .header("user-agent", self.streaming_user_agent())
            .header("host", self.host(ctx))
            .header("amz-sdk-invocation-id", Uuid::new_v4().to_string())
            .header("amz-sdk-request", "attempt=1; max=3")
            .header("Authorization", format!("Bearer {}", ctx.token));

        if ctx.credentials.is_api_key_credential() {
            req = req.header("tokentype", "API_KEY");
        }
        req
    }

    fn transform_api_body(&self, body: &str, ctx: &RequestContext<'_>) -> anyhow::Result<String> {
        self.transform_body(body, ctx.credentials)
    }

    fn transform_mcp_body(&self, body: &str, ctx: &RequestContext<'_>) -> anyhow::Result<String> {
        self.transform_body(body, ctx.credentials)
    }

    fn usage_request_parts(&self, ctx: &RequestContext<'_>) -> anyhow::Result<UsageRequestParts> {
        let host = self.host(ctx);
        let url = format!(
            "https://{}/getUsageLimits?origin={}&resourceType=AGENTIC_REQUEST",
            host,
            self.api_origin()
        );

        let mut headers = vec![
            ("Accept", "application/json".to_string()),
            ("x-amz-user-agent", self.runtime_x_amz_user_agent()),
            ("user-agent", self.runtime_user_agent()),
            ("host", host),
            ("amz-sdk-invocation-id", Uuid::new_v4().to_string()),
            ("amz-sdk-request", "attempt=1; max=1".to_string()),
            ("Authorization", format!("Bearer {}", ctx.token)),
        ];

        if ctx.credentials.is_api_key_credential() {
            headers.push(("tokentype", "API_KEY".to_string()));
        }

        Ok(UsageRequestParts { url, headers })
    }
}
