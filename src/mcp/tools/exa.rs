// SPDX-License-Identifier: AGPL-3.0-or-later
use anyhow::{Context, Result};
use async_trait::async_trait;
use reqwest::Client;
use serde_json::{json, Value};

use tracing::{debug, info, warn};

use super::{NativeTool, ToolResult};
use crate::mcp::protocol::McpTool;

const EXA_API_BASE: &str = "https://api.exa.ai";

pub struct ExaTool {
    client: Client,
    api_key: String,
}

impl ExaTool {
    pub fn new(client: Client, api_key: String) -> Self {
        Self { client, api_key }
    }
}

#[async_trait]
impl NativeTool for ExaTool {
    fn tools(&self) -> Vec<McpTool> {
        vec![
            McpTool {
                name: "web_search_exa".to_string(),
                description: "Search the web for any topic and get clean, ready-to-use content. \
                    Query tips: describe the ideal page, not keywords. \
                    Use category:people / category:company to search through Linkedin profiles / companies."
                    .to_string(),
                inputSchema: json!({
                    "type": "object",
                    "properties": {
                        "query": {
                            "type": "string",
                            "description": "Natural language search query."
                        },
                        "numResults": {
                            "type": "number",
                            "minimum": 1,
                            "maximum": 100,
                            "description": "Number of results (default: 10)."
                        }
                    },
                    "required": ["query"],
                    "additionalProperties": false
                }),
            },
            McpTool {
                name: "web_fetch_exa".to_string(),
                description: "Read a webpage's full content as clean markdown. \
                    Use after web_search_exa when highlights are insufficient or to read any URL. \
                    Batch multiple URLs in one call."
                    .to_string(),
                inputSchema: json!({
                    "type": "object",
                    "properties": {
                        "urls": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "URLs to read."
                        },
                        "maxCharacters": {
                            "type": "number",
                            "minimum": 1,
                            "description": "Max characters per page (default: 3000)."
                        }
                    },
                    "required": ["urls"],
                    "additionalProperties": false
                }),
            },
        ]
    }

    async fn call(&self, name: &str, arguments: Value) -> Result<ToolResult> {
        match name {
            "web_search_exa" => self.search(arguments).await,
            "web_fetch_exa" => self.fetch(arguments).await,
            _ => Ok(ToolResult::error(format!("unknown exa tool: {name}"))),
        }
    }
}

impl ExaTool {
    async fn search(&self, args: Value) -> Result<ToolResult> {
        debug!("exa web search called");
        let query = args
            .get("query")
            .and_then(|v| v.as_str())
            .context("missing required field: query")?;
        let num_results = args
            .get("numResults")
            .and_then(|v| v.as_u64())
            .unwrap_or(10);

        let body = json!({
            "query": query,
            "numResults": num_results,
            "contents": {
                "text": { "maxCharacters": 1500 },
                "highlights": { "numSentences": 3 }
            }
        });

        let resp = self
            .client
            .post(format!("{EXA_API_BASE}/search"))
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await
            .context("exa search request failed")?;

        let status = resp.status();
        let text = resp.text().await.context("failed to read exa response")?;

        if !status.is_success() {
            warn!(%status, "exa search failed");
            return Ok(ToolResult::error(format!(
                "exa search failed ({status}): {text}"
            )));
        }

        info!(response_len = text.len(), "exa search succeeded");
        Ok(ToolResult::text(text))
    }

    async fn fetch(&self, args: Value) -> Result<ToolResult> {
        debug!("exa web fetch called");
        let urls: Vec<String> = args
            .get("urls")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .context("missing required field: urls")?;
        let max_chars = args
            .get("maxCharacters")
            .and_then(|v| v.as_u64())
            .unwrap_or(3000);

        let body = json!({
            "urls": urls,
            "contents": {
                "text": { "maxCharacters": max_chars }
            }
        });

        let resp = self
            .client
            .post(format!("{EXA_API_BASE}/contents"))
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await
            .context("exa fetch request failed")?;

        let status = resp.status();
        let text = resp.text().await.context("failed to read exa response")?;

        if !status.is_success() {
            warn!(%status, "exa fetch failed");
            return Ok(ToolResult::error(format!(
                "exa fetch failed ({status}): {text}"
            )));
        }

        info!(url_count = urls.len(), response_len = text.len(), "exa fetch succeeded");
        Ok(ToolResult::text(text))
    }
}
