// SPDX-License-Identifier: AGPL-3.0-or-later
pub mod context7;
pub mod exa;
pub mod memory;
#[cfg(feature = "sindexer")]
pub mod sindexer;

use anyhow::Result;
use async_trait::async_trait;
use serde_json::Value;
use tracing::{debug, info, warn};

use super::protocol::McpTool;

pub struct ToolResult {
    pub content: Vec<ToolContent>,
    pub is_error: bool,
}

pub struct ToolContent {
    pub r#type: &'static str,
    pub text: String,
}

impl ToolResult {
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            content: vec![ToolContent {
                r#type: "text",
                text: text.into(),
            }],
            is_error: false,
        }
    }

    pub fn error(text: impl Into<String>) -> Self {
        Self {
            content: vec![ToolContent {
                r#type: "text",
                text: text.into(),
            }],
            is_error: true,
        }
    }

    pub fn to_json(&self) -> Value {
        serde_json::json!({
            "content": self.content.iter().map(|c| {
                serde_json::json!({
                    "type": c.r#type,
                    "text": c.text,
                })
            }).collect::<Vec<_>>(),
            "isError": self.is_error,
        })
    }
}

#[async_trait]
pub trait NativeTool: Send + Sync {
    fn tools(&self) -> Vec<McpTool>;
    async fn call(&self, name: &str, arguments: Value) -> Result<ToolResult>;
}

pub struct ToolRegistry {
    tools: Vec<Box<dyn NativeTool>>,
}

impl ToolRegistry {
    pub fn new(tools: Vec<Box<dyn NativeTool>>) -> Self {
        Self { tools }
    }

    pub fn list_tools(&self) -> Vec<McpTool> {
        self.tools.iter().flat_map(|t| t.tools()).collect()
    }

    pub async fn call(&self, name: &str, arguments: Value) -> Result<ToolResult> {
        debug!(tool_name = name, "tool registry call");
        for tool in &self.tools {
            let names: Vec<String> = tool.tools().iter().map(|t| t.name.clone()).collect();
            if names.iter().any(|n| n == name) {
                let result = tool.call(name, arguments).await;
                info!(tool_name = name, success = result.is_ok(), "tool call completed");
                return result;
            }
        }
        warn!(tool_name = name, "unknown tool called");
        Ok(ToolResult::error(format!("unknown tool: {name}")))
    }
}
