// SPDX-License-Identifier: AGPL-3.0-or-later
//! MCP Streamable HTTP daemon — shared process serving native tools to N agents.
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use axum::extract::{Request, State};
use axum::http::{header::WWW_AUTHENTICATE, HeaderMap, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use reqwest::Client;
use serde_json::{json, Value};

use crate::mcp::auth::BearerAuth;
use crate::mcp::protocol::JsonRpcMessage;
use crate::mcp::tools::context7::Context7Tool;
use crate::mcp::tools::exa::ExaTool;
use crate::mcp::tools::memory::MemoryTool;
use crate::mcp::tools::pyright::PyrightTool;
use crate::mcp::tools::ToolRegistry;

struct DaemonState {
    registry: ToolRegistry,
    auth: BearerAuth,
}

pub struct DaemonArgs {
    pub port: u16,
    pub host: String,
    pub memory_dir: Option<PathBuf>,
    pub pyright_root: Option<PathBuf>,
    pub pyright_workspace_dir: Option<PathBuf>,
    pub auth_token: String,
}

pub async fn run(args: DaemonArgs) -> Result<()> {
    let DaemonArgs {
        port,
        host,
        memory_dir,
        pyright_root,
        pyright_workspace_dir,
        auth_token,
    } = args;
    let auth = BearerAuth::new(auth_token)?;

    let http_client = Client::builder()
        .user_agent("ccr-rust-mcp-daemon")
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .context("failed to build HTTP client")?;

    let mut tools: Vec<Box<dyn crate::mcp::tools::NativeTool>> = Vec::new();

    if let Ok(exa_key) = std::env::var("EXA_API_KEY") {
        if !exa_key.is_empty() {
            tracing::info!("exa tool enabled");
            tools.push(Box::new(ExaTool::new(http_client.clone(), exa_key)));
        }
    }

    tracing::info!("context7 tool enabled");
    tools.push(Box::new(Context7Tool::new(http_client.clone())));

    let memory_path = memory_dir.map(|dir| {
        std::fs::create_dir_all(&dir).ok();
        dir.join("memory.json")
    });
    tracing::info!("memory tool enabled (persist: {:?})", memory_path);
    tools.push(Box::new(MemoryTool::new(memory_path)));

    #[cfg(feature = "sindexer")]
    {
        use crate::mcp::tools::sindexer::SindexerTool;
        tracing::info!("sindexer tool enabled");
        tools.push(Box::new(SindexerTool::new()));
    }

    match (pyright_root, pyright_workspace_dir) {
        (Some(project_root), Some(workspace_dir)) => {
            tracing::info!(
                project_root = %project_root.display(),
                workspace_dir = %workspace_dir.display(),
                "pyright tool enabled"
            );
            tools.push(Box::new(PyrightTool::new(
                project_root,
                workspace_dir,
                3,
            )?));
        }
        (Some(_), None) => anyhow::bail!(
            "--pyright-workspace-dir or CCR_MCP_PYRIGHT_WORKSPACE_DIR is required with --pyright-root"
        ),
        (None, Some(_)) => anyhow::bail!(
            "--pyright-root or PYRIGHT_PROJECT_ROOT is required with --pyright-workspace-dir"
        ),
        (None, None) => {}
    }

    let state = Arc::new(DaemonState {
        registry: ToolRegistry::new(tools),
        auth,
    });

    let app = Router::new()
        .route("/mcp", post(handle_mcp))
        .route("/health", get(handle_health))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            require_bearer,
        ))
        .with_state(state);

    let host: std::net::IpAddr = host.parse().context("invalid host address")?;
    let addr = SocketAddr::from((host, port));
    tracing::info!("mcp-daemon listening on {addr}");

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .context("failed to bind")?;
    axum::serve(listener, app).await.context("server error")?;

    Ok(())
}

async fn require_bearer(
    State(state): State<Arc<DaemonState>>,
    request: Request,
    next: Next,
) -> Response {
    if state.auth.authorizes(request.headers()) {
        next.run(request).await
    } else {
        (
            StatusCode::UNAUTHORIZED,
            [(WWW_AUTHENTICATE, "Bearer")],
            String::new(),
        )
            .into_response()
    }
}

async fn handle_health() -> impl IntoResponse {
    (StatusCode::OK, "ok")
}

async fn handle_mcp(
    State(state): State<Arc<DaemonState>>,
    headers: HeaderMap,
    body: String,
) -> impl IntoResponse {
    let accept = headers
        .get("accept")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    let request: JsonRpcMessage = match serde_json::from_str(&body) {
        Ok(msg) => msg,
        Err(err) => {
            let resp = rpc_error(None, -32700, format!("parse error: {err}"));
            return mcp_response(resp, accept);
        }
    };

    let request_id = request.id.clone();
    let response = match dispatch(&state.registry, request).await {
        Ok(Some(resp)) => resp,
        Ok(None) => {
            return (
                StatusCode::ACCEPTED,
                [("content-type", "application/json")],
                String::new(),
            );
        }
        Err(err) => match request_id {
            Some(id) => rpc_error(Some(id), -32000, err.to_string()),
            None => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    [("content-type", "application/json")],
                    json!({"error": err.to_string()}).to_string(),
                );
            }
        },
    };

    mcp_response(response, accept)
}

fn mcp_response(
    msg: JsonRpcMessage,
    _accept: &str,
) -> (StatusCode, [(&'static str, &'static str); 1], String) {
    let body = serde_json::to_string(&msg).unwrap_or_default();
    (StatusCode::OK, [("content-type", "application/json")], body)
}

async fn dispatch(
    registry: &ToolRegistry,
    request: JsonRpcMessage,
) -> Result<Option<JsonRpcMessage>> {
    let Some(method) = request.method.as_deref() else {
        return Ok(None);
    };

    match method {
        "initialize" => Ok(Some(rpc_result(
            request.id,
            json!({
                "protocolVersion": "2024-11-05",
                "serverInfo": {
                    "name": "ccr-rust-mcp-daemon",
                    "version": env!("CARGO_PKG_VERSION")
                },
                "capabilities": {
                    "tools": { "listChanged": false }
                }
            }),
        ))),
        "notifications/initialized" => Ok(None),
        "ping" => Ok(Some(rpc_result(request.id, json!({})))),
        "tools/list" => Ok(Some(rpc_result(
            request.id,
            json!({ "tools": registry.list_tools() }),
        ))),
        "tools/call" => {
            let params = request
                .params
                .clone()
                .context("tools/call missing params")?;
            let name = params
                .get("name")
                .and_then(|v| v.as_str())
                .context("tools/call missing name")?;
            let arguments = params.get("arguments").cloned().unwrap_or(json!({}));

            match registry.call(name, arguments).await {
                Ok(result) => Ok(Some(rpc_result(request.id, result.to_json()))),
                Err(err) => Ok(Some(rpc_error(
                    request.id,
                    -32000,
                    format!("tool error: {err}"),
                ))),
            }
        }
        _ => {
            if request.id.is_some() {
                Ok(Some(rpc_error(
                    request.id,
                    -32601,
                    format!("method not found: {method}"),
                )))
            } else {
                Ok(None)
            }
        }
    }
}

fn rpc_result(id: Option<Value>, result: Value) -> JsonRpcMessage {
    JsonRpcMessage {
        jsonrpc: Some("2.0".to_string()),
        id,
        method: None,
        params: None,
        result: Some(result),
        error: None,
    }
}

fn rpc_error(id: Option<Value>, code: i64, message: impl Into<String>) -> JsonRpcMessage {
    JsonRpcMessage {
        jsonrpc: Some("2.0".to_string()),
        id,
        method: None,
        params: None,
        result: None,
        error: Some(json!({
            "code": code,
            "message": message.into()
        })),
    }
}
