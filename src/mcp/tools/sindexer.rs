// SPDX-License-Identifier: AGPL-3.0-or-later
use std::path::PathBuf;

use anyhow::{Context, Result};
use async_trait::async_trait;
use serde_json::{json, Value};
use tracing::{debug, info};

use super::{NativeTool, ToolResult};
use crate::mcp::protocol::McpTool;

pub struct SindexerTool {
    sindexer: sindexer::Sindexer,
}

impl SindexerTool {
    pub fn new() -> Self {
        info!("sindexer native tool initialized");
        Self {
            sindexer: sindexer::Sindexer::from_env(),
        }
    }
}

#[async_trait]
impl NativeTool for SindexerTool {
    fn tools(&self) -> Vec<McpTool> {
        vec![
            McpTool {
                name: "index_codebase".to_string(),
                description: "Index a codebase directory for semantic code search. Walks the directory, \
                    parses source files, extracts code chunks, and generates embeddings. \
                    Use force=true to re-index an already indexed codebase.".to_string(),
                inputSchema: json!({
                    "type": "object",
                    "properties": {
                        "path": {
                            "type": "string",
                            "description": "Absolute path to the codebase directory to index"
                        },
                        "force": {
                            "type": "boolean",
                            "description": "Force re-indexing even if an index already exists",
                            "default": false
                        }
                    },
                    "required": ["path"]
                }),
            },
            McpTool {
                name: "search_code".to_string(),
                description: "Search indexed code using natural language or code queries. \
                    Returns the most semantically similar code chunks from the indexed codebase.".to_string(),
                inputSchema: json!({
                    "type": "object",
                    "properties": {
                        "path": {
                            "type": "string",
                            "description": "Absolute path to the indexed codebase"
                        },
                        "query": {
                            "type": "string",
                            "description": "Natural language or code query to search for"
                        },
                        "limit": {
                            "type": "integer",
                            "description": "Maximum number of results to return",
                            "default": 10
                        },
                        "extensions": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "Optional file extensions to filter results (e.g. [\"rs\", \"py\"])",
                            "default": []
                        }
                    },
                    "required": ["path", "query"]
                }),
            },
            McpTool {
                name: "get_indexing_status".to_string(),
                description: "Check the indexing status of a codebase.".to_string(),
                inputSchema: json!({
                    "type": "object",
                    "properties": {
                        "path": {
                            "type": "string",
                            "description": "Absolute path to the codebase to check"
                        }
                    },
                    "required": ["path"]
                }),
            },
            McpTool {
                name: "clear_index".to_string(),
                description: "Remove the index for a codebase, freeing up storage.".to_string(),
                inputSchema: json!({
                    "type": "object",
                    "properties": {
                        "path": {
                            "type": "string",
                            "description": "Absolute path to the codebase whose index should be cleared"
                        }
                    },
                    "required": ["path"]
                }),
            },
            McpTool {
                name: "list_collections".to_string(),
                description: "List all indexed codebase collections with row counts.".to_string(),
                inputSchema: json!({
                    "type": "object",
                    "properties": {}
                }),
            },
            McpTool {
                name: "collection_stats".to_string(),
                description: "Get statistics for a specific collection.".to_string(),
                inputSchema: json!({
                    "type": "object",
                    "properties": {
                        "collection_name": {
                            "type": "string",
                            "description": "Exact collection name"
                        }
                    },
                    "required": ["collection_name"]
                }),
            },
            McpTool {
                name: "drop_collection".to_string(),
                description: "Drop a specific collection from the vector database by name.".to_string(),
                inputSchema: json!({
                    "type": "object",
                    "properties": {
                        "collection_name": {
                            "type": "string",
                            "description": "Exact collection name to drop"
                        }
                    },
                    "required": ["collection_name"]
                }),
            },
        ]
    }

    async fn call(&self, name: &str, arguments: Value) -> Result<ToolResult> {
        debug!(tool = name, "sindexer tool call");
        match name {
            "index_codebase" => self.index_codebase(arguments).await,
            "search_code" => self.search_code(arguments).await,
            "get_indexing_status" => self.get_indexing_status(arguments).await,
            "clear_index" => self.clear_index(arguments).await,
            "list_collections" => self.list_collections().await,
            "collection_stats" => self.collection_stats(arguments).await,
            "drop_collection" => self.drop_collection(arguments).await,
            _ => Ok(ToolResult::error(format!("unknown sindexer tool: {name}"))),
        }
    }
}

impl SindexerTool {
    async fn index_codebase(&self, args: Value) -> Result<ToolResult> {
        let path = args.get("path").and_then(|v| v.as_str()).context("missing path")?;
        let force = args.get("force").and_then(|v| v.as_bool()).unwrap_or(false);
        let path = PathBuf::from(path);

        match self.sindexer.index(&path, force).await {
            Ok(result) => {
                let mode = if result.lexical_only { " (lexical only)" } else { "" };
                let msg = if result.warnings.is_empty() {
                    format!("Indexed {}{}", path.display(), mode)
                } else {
                    format!("Indexed {} ({}){}", path.display(), result.warnings.join("; "), mode)
                };
                Ok(ToolResult::text(serde_json::to_string(&json!({
                    "success": true,
                    "message": msg,
                    "path": path,
                    "files_indexed": result.files_indexed,
                    "chunks_created": result.chunks_created,
                    "duration_ms": result.duration_ms,
                }))?))
            }
            Err(e) => Ok(ToolResult::error(e.to_string())),
        }
    }

    async fn search_code(&self, args: Value) -> Result<ToolResult> {
        let path = args.get("path").and_then(|v| v.as_str()).context("missing path")?;
        let query = args.get("query").and_then(|v| v.as_str()).context("missing query")?;
        let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(10) as usize;
        let extensions: Vec<String> = args
            .get("extensions")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default();
        let path = PathBuf::from(path);

        match self.sindexer.search(&path, query, limit, &extensions).await {
            Ok(hits) => {
                let results: Vec<Value> = hits
                    .iter()
                    .map(|h| {
                        json!({
                            "file_path": h.file_path,
                            "relative_path": h.relative_path,
                            "content": h.content,
                            "start_line": h.start_line,
                            "end_line": h.end_line,
                            "language": h.language,
                            "score": h.score,
                        })
                    })
                    .collect();
                Ok(ToolResult::text(serde_json::to_string(&json!({
                    "results": results,
                    "count": results.len(),
                }))?))
            }
            Err(e) => Ok(ToolResult::error(e.to_string())),
        }
    }

    async fn get_indexing_status(&self, args: Value) -> Result<ToolResult> {
        let path = args.get("path").and_then(|v| v.as_str()).context("missing path")?;
        let path = PathBuf::from(path);
        let status = self.sindexer.status(&path);
        Ok(ToolResult::text(serde_json::to_string(&status)?))
    }

    async fn clear_index(&self, args: Value) -> Result<ToolResult> {
        let path = args.get("path").and_then(|v| v.as_str()).context("missing path")?;
        let path = PathBuf::from(path);

        match self.sindexer.clear(&path).await {
            Ok(()) => Ok(ToolResult::text(serde_json::to_string(&json!({
                "success": true,
                "message": format!("Cleared index for {}", path.display()),
                "path": path,
            }))?)),
            Err(e) => Ok(ToolResult::error(e.to_string())),
        }
    }

    async fn list_collections(&self) -> Result<ToolResult> {
        match self.sindexer.list_collections().await {
            Ok(collections) => {
                let items: Vec<Value> = collections
                    .iter()
                    .map(|c| json!({ "name": c.name, "row_count": c.row_count }))
                    .collect();
                Ok(ToolResult::text(serde_json::to_string(&json!({
                    "collections": items,
                    "count": items.len(),
                }))?))
            }
            Err(e) => Ok(ToolResult::error(e.to_string())),
        }
    }

    async fn collection_stats(&self, args: Value) -> Result<ToolResult> {
        let name = args
            .get("collection_name")
            .and_then(|v| v.as_str())
            .context("missing collection_name")?;

        match self.sindexer.collection_stats(name).await {
            Ok(row_count) => Ok(ToolResult::text(serde_json::to_string(&json!({
                "collection_name": name,
                "row_count": row_count,
            }))?)),
            Err(e) => Ok(ToolResult::error(e.to_string())),
        }
    }

    async fn drop_collection(&self, args: Value) -> Result<ToolResult> {
        let name = args
            .get("collection_name")
            .and_then(|v| v.as_str())
            .context("missing collection_name")?;

        match self.sindexer.drop_collection(name).await {
            Ok(existed) => {
                let msg = if existed {
                    format!("Dropped collection '{name}'")
                } else {
                    format!("Collection '{name}' does not exist")
                };
                Ok(ToolResult::text(serde_json::to_string(&json!({
                    "success": existed,
                    "message": msg,
                    "collection_name": name,
                }))?))
            }
            Err(e) => Ok(ToolResult::error(e.to_string())),
        }
    }
}
