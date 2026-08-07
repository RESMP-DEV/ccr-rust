// SPDX-License-Identifier: AGPL-3.0-or-later
use anyhow::{Context, Result};
use async_trait::async_trait;
use reqwest::{header::HeaderMap, header::HeaderValue, Client};
use serde_json::{json, Value};

use tracing::{debug, info, warn};

use super::{NativeTool, ToolResult};
use crate::mcp::protocol::McpTool;

const JINA_SEARCH_BASE: &str = "https://s.jina.ai/";
const JINA_READER_BASE: &str = "https://r.jina.ai/";

/// Headers applied to every Jina request so AlphaHENG's bursty, repetitive
/// web-research workload does not trip cache/hot-path abuse heuristics. These
/// are ordinary HTTP headers with no API-cost impact: Jina charges per token
/// regardless of cache/no-cache.
fn jina_request_headers(api_key: &str) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(
        "Authorization",
        HeaderValue::from_str(&format!("Bearer {api_key}")).expect("valid header value"),
    );
    // Force a live fetch; ignore any cached copy.
    headers.insert("X-No-Cache", HeaderValue::from_static("true"));
    headers.insert("X-Ttl", HeaderValue::from_static("0"));
    // Prevent the request from being cached or tracked server-side.
    headers.insert("DNT", HeaderValue::from_static("1"));
    headers.insert("X-No-Track", HeaderValue::from_static("true"));
    headers
}

pub struct JinaTool {
    client: Client,
    api_key: String,
}

impl JinaTool {
    pub fn new(client: Client, api_key: String) -> Self {
        Self { client, api_key }
    }
}

#[async_trait]
impl NativeTool for JinaTool {
    fn tools(&self) -> Vec<McpTool> {
        vec![
            McpTool {
                name: "web_search_jina".to_string(),
                description: "Search the live web for any topic via Jina (s.jina.ai). \
                    Returns fresh results on every call — AlphaHENG's request posture \
                    bypasses Jina's response cache. \
                    Use for real-time web research when the repository index (search_code) \
                    does not apply. External web content only."
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
                            "maximum": 20,
                            "description": "Number of results (default: 10)."
                        }
                    },
                    "required": ["query"],
                    "additionalProperties": false
                }),
            },
            McpTool {
                name: "web_fetch_jina".to_string(),
                description: "Read a webpage's full content as clean Markdown via the Jina \
                    Reader (r.jina.ai). Use after web_search_jina when a snippet is \
                    insufficient, or to read any public URL. Batch multiple URLs in one call. \
                    External web content only."
                    .to_string(),
                inputSchema: json!({
                    "type": "object",
                    "properties": {
                        "urls": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "URLs to read."
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
            "web_search_jina" => self.search(arguments).await,
            "web_fetch_jina" => self.fetch(arguments).await,
            _ => Ok(ToolResult::error(format!("unknown jina tool: {name}"))),
        }
    }
}

impl JinaTool {
    async fn search(&self, args: Value) -> Result<ToolResult> {
        debug!("jina web search called");
        let query = args
            .get("query")
            .and_then(|v| v.as_str())
            .context("missing required field: query")?;
        let num_results = args
            .get("numResults")
            .and_then(|v| v.as_u64())
            .unwrap_or(10);

        let resp = match self
            .client
            .get(JINA_SEARCH_BASE)
            .query(&[("q", query)])
            .headers(jina_request_headers(&self.api_key))
            .header("Accept", "application/json")
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                warn!(error = %e, "jina search request failed");
                return Ok(ToolResult::error(format!(
                    "jina search request failed: {e}"
                )));
            }
        };

        let status = resp.status();
        let text = resp.text().await.context("failed to read jina response")?;

        if !status.is_success() {
            warn!(%status, "jina search failed");
            return Ok(ToolResult::error(format!(
                "jina search failed ({status}): {text}"
            )));
        }

        // Truncate to the requested number of results if JSON; otherwise pass through.
        let payload = self.truncate_search_results(&text, num_results);
        info!(response_len = payload.len(), "jina search succeeded");
        Ok(ToolResult::text(payload))
    }

    async fn fetch(&self, args: Value) -> Result<ToolResult> {
        debug!("jina web fetch called");
        let urls: Vec<String> = args
            .get("urls")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .context("missing required field: urls")?;
        if urls.is_empty() {
            return Ok(ToolResult::error("no urls provided"));
        }

        let mut parts: Vec<String> = Vec::with_capacity(urls.len());
        for raw_url in &urls {
            let target = format!("{JINA_READER_BASE}{raw_url}");
            let resp = match self
                .client
                .get(&target)
                .headers(jina_request_headers(&self.api_key))
                .send()
                .await
            {
                Ok(r) => r,
                Err(e) => {
                    warn!(url = %raw_url, error = %e, "jina fetch request failed");
                    parts.push(format!("--- {raw_url} (request failed: {e}) ---\n"));
                    continue;
                }
            };
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            if status.is_success() {
                parts.push(format!("--- {raw_url} ---\n{body}"));
            } else {
                warn!(url = %raw_url, %status, "jina fetch failed");
                parts.push(format!("--- {raw_url} (failed: {status}) ---\n{body}"));
            }
        }

        let combined = parts.join("\n\n");
        info!(
            url_count = urls.len(),
            response_len = combined.len(),
            "jina fetch succeeded"
        );
        Ok(ToolResult::text(combined))
    }

    /// Best-effort truncation of the s.jina.ai JSON response to `num` hits.
    fn truncate_search_results(&self, text: &str, num: u64) -> String {
        let Ok(parsed) = serde_json::from_str::<Value>(text) else {
            // Not JSON (e.g. plain markdown); return as-is.
            return text.to_string();
        };
        let mut trimmed = parsed;
        if let Some(data) = trimmed.get_mut("data").and_then(|d| d.as_array_mut()) {
            data.truncate(num as usize);
        }
        trimmed.to_string()
    }
}
