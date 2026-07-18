// SPDX-License-Identifier: AGPL-3.0-or-later
use std::collections::HashMap;
use tracing::{debug, info, warn};

use super::protocol::McpTool;

#[derive(Debug, Clone, Default)]
pub struct ToolCatalog {
    tools: Vec<McpTool>,
    routes: HashMap<String, usize>,
}

impl ToolCatalog {
    pub fn add_backend_tools(&mut self, idx: usize, tools: Vec<McpTool>) {
        info!(
            backend_idx = idx,
            tool_count = tools.len(),
            "registering backend tools"
        );
        for tool in tools {
            let name = tool.name.clone();
            self.routes.insert(name.clone(), idx);
            if let Some(existing) = self.tools.iter_mut().find(|entry| entry.name == name) {
                *existing = tool;
            } else {
                self.tools.push(tool);
            }
        }
    }

    pub fn compress(&self, level: &str) -> Vec<McpTool> {
        self.tools
            .iter()
            .cloned()
            .map(|mut tool| {
                match level.to_ascii_lowercase().as_str() {
                    "none" => {
                        tool.description.clear();
                        tool.inputSchema = serde_json::json!({});
                    }
                    "minimal" | "min" | "high" | "aggressive" => {
                        tool.description.clear();
                    }
                    "medium" | "light" => {
                        let normalized = tool
                            .description
                            .split_whitespace()
                            .collect::<Vec<_>>()
                            .join(" ");
                        tool.description = normalized.chars().take(256).collect();
                    }
                    _ => {}
                }
                tool
            })
            .collect()
    }

    pub fn get_schema(&self, name: &str) -> Option<&McpTool> {
        self.tools.iter().find(|tool| tool.name == name)
    }

    pub fn route(&self, name: &str) -> Option<usize> {
        let result = self.routes.get(name).copied();
        if result.is_none() {
            warn!(tool_name = name, "no route found for tool");
        } else {
            debug!(tool_name = name, backend_idx = ?result, "tool routed");
        }
        result
    }
}
