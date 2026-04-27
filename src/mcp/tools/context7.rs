// SPDX-License-Identifier: AGPL-3.0-or-later
use anyhow::{Context, Result};
use async_trait::async_trait;
use reqwest::Client;
use serde_json::{json, Value};

use tracing::{debug, info, warn};

use super::{NativeTool, ToolResult};
use crate::mcp::protocol::McpTool;

const C7_API_BASE: &str = "https://api.context7.com/v2";

pub struct Context7Tool {
    client: Client,
}

impl Context7Tool {
    pub fn new(client: Client) -> Self {
        Self { client }
    }
}

#[async_trait]
impl NativeTool for Context7Tool {
    fn tools(&self) -> Vec<McpTool> {
        vec![
            McpTool {
                name: "resolve-library-id".to_string(),
                description: "Resolves a package/product name to a Context7-compatible library ID. \
                    Call this before query-docs to obtain a valid library ID."
                    .to_string(),
                inputSchema: json!({
                    "type": "object",
                    "properties": {
                        "query": {
                            "type": "string",
                            "description": "The question or task you need help with."
                        },
                        "libraryName": {
                            "type": "string",
                            "description": "Library name to search for (e.g., 'Next.js', 'React')."
                        }
                    },
                    "required": ["query", "libraryName"]
                }),
            },
            McpTool {
                name: "query-docs".to_string(),
                description: "Retrieves up-to-date documentation and code examples from Context7 \
                    for any programming library or framework. Call resolve-library-id first."
                    .to_string(),
                inputSchema: json!({
                    "type": "object",
                    "properties": {
                        "libraryId": {
                            "type": "string",
                            "description": "Context7-compatible library ID (e.g., '/mongodb/docs')."
                        },
                        "query": {
                            "type": "string",
                            "description": "The question or task you need help with."
                        },
                        "researchMode": {
                            "type": "boolean",
                            "description": "Set true for deep research mode on retry."
                        }
                    },
                    "required": ["libraryId", "query"]
                }),
            },
        ]
    }

    async fn call(&self, name: &str, arguments: Value) -> Result<ToolResult> {
        match name {
            "resolve-library-id" => self.resolve_library(arguments).await,
            "query-docs" => self.query_docs(arguments).await,
            _ => Ok(ToolResult::error(format!("unknown context7 tool: {name}"))),
        }
    }
}

impl Context7Tool {
    async fn resolve_library(&self, args: Value) -> Result<ToolResult> {
        debug!("context7 resolve_library called");
        let query = args
            .get("query")
            .and_then(|v| v.as_str())
            .context("missing required field: query")?;
        let library_name = args
            .get("libraryName")
            .and_then(|v| v.as_str())
            .context("missing required field: libraryName")?;

        let resp = self
            .client
            .get(format!("{C7_API_BASE}/libs/search"))
            .query(&[("query", query), ("libraryName", library_name)])
            .send()
            .await
            .context("context7 resolve request failed")?;

        let status = resp.status();
        let text = resp
            .text()
            .await
            .context("failed to read context7 response")?;

        if !status.is_success() {
            warn!(%status, "context7 resolve failed");
            return Ok(ToolResult::error(format!(
                "context7 resolve failed ({status}): {text}"
            )));
        }

        info!("context7 resolve_library succeeded");
        Ok(ToolResult::text(text))
    }

    async fn query_docs(&self, args: Value) -> Result<ToolResult> {
        debug!("context7 query_docs called");
        let library_id = args
            .get("libraryId")
            .and_then(|v| v.as_str())
            .context("missing required field: libraryId")?;
        let query = args
            .get("query")
            .and_then(|v| v.as_str())
            .context("missing required field: query")?;

        let mut params = vec![("query", query), ("libraryId", library_id)];
        let research_str;
        if let Some(true) = args.get("researchMode").and_then(|v| v.as_bool()) {
            research_str = "true".to_string();
            params.push(("researchMode", &research_str));
        }

        let resp = self
            .client
            .get(format!("{C7_API_BASE}/context"))
            .query(&params)
            .send()
            .await
            .context("context7 query request failed")?;

        let status = resp.status();
        let text = resp
            .text()
            .await
            .context("failed to read context7 response")?;

        if !status.is_success() {
            warn!(%status, "context7 query failed");
            return Ok(ToolResult::error(format!(
                "context7 query failed ({status}): {text}"
            )));
        }

        info!(response_len = text.len(), "context7 query_docs succeeded");
        Ok(ToolResult::text(text))
    }
}
