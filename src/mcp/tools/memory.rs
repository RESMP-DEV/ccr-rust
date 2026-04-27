// SPDX-License-Identifier: AGPL-3.0-or-later
use std::path::PathBuf;

use anyhow::{Context, Result};
use async_trait::async_trait;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use tracing::{debug, info};

use super::{NativeTool, ToolResult};
use crate::mcp::protocol::McpTool;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entity {
    pub name: String,
    #[serde(rename = "entityType")]
    pub entity_type: String,
    pub observations: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Relation {
    pub from: String,
    pub to: String,
    #[serde(rename = "relationType")]
    pub relation_type: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Graph {
    entities: Vec<Entity>,
    relations: Vec<Relation>,
}

pub struct MemoryTool {
    graph: RwLock<Graph>,
    persist_path: Option<PathBuf>,
}

impl MemoryTool {
    pub fn new(persist_path: Option<PathBuf>) -> Self {
        debug!(persist_path = ?persist_path, "initializing memory tool");
        let graph = persist_path
            .as_ref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| serde_json::from_str::<Graph>(&s).ok())
            .unwrap_or_default();

        Self {
            graph: RwLock::new(graph),
            persist_path,
        }
    }

    fn persist(&self) {
        if let Some(path) = &self.persist_path {
            let graph = self.graph.read();
            let json = serde_json::to_string_pretty(&*graph).unwrap_or_default();
            // atomic write: temp file + rename
            let tmp = path.with_extension("tmp");
            if std::fs::write(&tmp, &json).is_ok() {
                let _ = std::fs::rename(&tmp, path);
            }
        }
    }
}

#[async_trait]
impl NativeTool for MemoryTool {
    fn tools(&self) -> Vec<McpTool> {
        vec![
            tool_def("create_entities", "Create multiple new entities in the knowledge graph", json!({
                "type": "object",
                "properties": {
                    "entities": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "name": { "type": "string" },
                                "entityType": { "type": "string" },
                                "observations": { "type": "array", "items": { "type": "string" } }
                            },
                            "required": ["name", "entityType", "observations"]
                        }
                    }
                },
                "required": ["entities"]
            })),
            tool_def("create_relations", "Create multiple new relations between entities in the knowledge graph", json!({
                "type": "object",
                "properties": {
                    "relations": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "from": { "type": "string" },
                                "to": { "type": "string" },
                                "relationType": { "type": "string" }
                            },
                            "required": ["from", "to", "relationType"]
                        }
                    }
                },
                "required": ["relations"]
            })),
            tool_def("add_observations", "Add new observations to existing entities", json!({
                "type": "object",
                "properties": {
                    "observations": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "entityName": { "type": "string" },
                                "contents": { "type": "array", "items": { "type": "string" } }
                            },
                            "required": ["entityName", "contents"]
                        }
                    }
                },
                "required": ["observations"]
            })),
            tool_def("delete_entities", "Delete multiple entities and their associated relations", json!({
                "type": "object",
                "properties": {
                    "entityNames": { "type": "array", "items": { "type": "string" } }
                },
                "required": ["entityNames"]
            })),
            tool_def("delete_observations", "Delete specific observations from entities", json!({
                "type": "object",
                "properties": {
                    "deletions": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "entityName": { "type": "string" },
                                "observations": { "type": "array", "items": { "type": "string" } }
                            },
                            "required": ["entityName", "observations"]
                        }
                    }
                },
                "required": ["deletions"]
            })),
            tool_def("delete_relations", "Delete multiple relations from the knowledge graph", json!({
                "type": "object",
                "properties": {
                    "relations": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "from": { "type": "string" },
                                "to": { "type": "string" },
                                "relationType": { "type": "string" }
                            },
                            "required": ["from", "to", "relationType"]
                        }
                    }
                },
                "required": ["relations"]
            })),
            tool_def("read_graph", "Read the entire knowledge graph", json!({
                "type": "object",
                "properties": {}
            })),
            tool_def("search_nodes", "Search for nodes in the knowledge graph based on a query", json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Search query to match against entity names, types, and observations" }
                },
                "required": ["query"]
            })),
            tool_def("open_nodes", "Open specific nodes in the knowledge graph by their names", json!({
                "type": "object",
                "properties": {
                    "names": { "type": "array", "items": { "type": "string" } }
                },
                "required": ["names"]
            })),
        ]
    }

    async fn call(&self, name: &str, arguments: Value) -> Result<ToolResult> {
        debug!(tool_name = name, "memory tool called");
        match name {
            "create_entities" => self.create_entities(arguments),
            "create_relations" => self.create_relations(arguments),
            "add_observations" => self.add_observations(arguments),
            "delete_entities" => self.delete_entities(arguments),
            "delete_observations" => self.delete_observations(arguments),
            "delete_relations" => self.delete_relations(arguments),
            "read_graph" => self.read_graph(),
            "search_nodes" => self.search_nodes(arguments),
            "open_nodes" => self.open_nodes(arguments),
            _ => Ok(ToolResult::error(format!("unknown memory tool: {name}"))),
        }
    }
}

impl MemoryTool {
    fn create_entities(&self, args: Value) -> Result<ToolResult> {
        debug!("creating entities in knowledge graph");
        let input: Vec<Entity> = serde_json::from_value(
            args.get("entities")
                .cloned()
                .context("missing entities")?,
        )
        .context("invalid entities")?;

        let mut graph = self.graph.write();
        let mut created = Vec::new();
        for entity in input {
            if let Some(existing) = graph.entities.iter_mut().find(|e| e.name == entity.name) {
                for obs in &entity.observations {
                    if !existing.observations.contains(obs) {
                        existing.observations.push(obs.clone());
                    }
                }
            } else {
                created.push(entity.clone());
                graph.entities.push(entity);
            }
        }
        drop(graph);
        self.persist();

        info!(created_count = created.len(), "entities created");
        Ok(ToolResult::text(
            serde_json::to_string(&json!({ "entities": created }))?,
        ))
    }

    fn create_relations(&self, args: Value) -> Result<ToolResult> {
        let input: Vec<Relation> = serde_json::from_value(
            args.get("relations")
                .cloned()
                .context("missing relations")?,
        )
        .context("invalid relations")?;

        let mut graph = self.graph.write();
        let mut created = Vec::new();
        for rel in input {
            let dup = graph.relations.iter().any(|r| {
                r.from == rel.from && r.to == rel.to && r.relation_type == rel.relation_type
            });
            if !dup {
                created.push(rel.clone());
                graph.relations.push(rel);
            }
        }
        drop(graph);
        self.persist();

        Ok(ToolResult::text(
            serde_json::to_string(&json!({ "relations": created }))?,
        ))
    }

    fn add_observations(&self, args: Value) -> Result<ToolResult> {
        let observations: Vec<Value> = serde_json::from_value(
            args.get("observations")
                .cloned()
                .context("missing observations")?,
        )?;

        let mut graph = self.graph.write();
        let mut results = Vec::new();
        for obs in observations {
            let entity_name = obs
                .get("entityName")
                .and_then(|v| v.as_str())
                .context("missing entityName")?
                .to_string();
            let contents: Vec<String> =
                serde_json::from_value(obs.get("contents").cloned().context("missing contents")?)?;

            if let Some(entity) = graph.entities.iter_mut().find(|e| e.name == entity_name) {
                let mut added = Vec::new();
                for c in contents {
                    if !entity.observations.contains(&c) {
                        entity.observations.push(c.clone());
                        added.push(c);
                    }
                }
                results.push(json!({ "entityName": entity_name, "addedObservations": added }));
            }
        }
        drop(graph);
        self.persist();

        Ok(ToolResult::text(
            serde_json::to_string(&json!({ "results": results }))?,
        ))
    }

    fn delete_entities(&self, args: Value) -> Result<ToolResult> {
        debug!("deleting entities from knowledge graph");
        let names: Vec<String> = serde_json::from_value(
            args.get("entityNames")
                .cloned()
                .context("missing entityNames")?,
        )?;

        let mut graph = self.graph.write();
        graph.entities.retain(|e| !names.contains(&e.name));
        graph
            .relations
            .retain(|r| !names.contains(&r.from) && !names.contains(&r.to));
        drop(graph);
        self.persist();

        Ok(ToolResult::text(
            serde_json::to_string(&json!({ "success": true, "message": "entities deleted" }))?,
        ))
    }

    fn delete_observations(&self, args: Value) -> Result<ToolResult> {
        let deletions: Vec<Value> = serde_json::from_value(
            args.get("deletions")
                .cloned()
                .context("missing deletions")?,
        )?;

        let mut graph = self.graph.write();
        for del in deletions {
            let entity_name = del
                .get("entityName")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let obs_to_del: Vec<String> = del
                .get("observations")
                .and_then(|v| serde_json::from_value(v.clone()).ok())
                .unwrap_or_default();
            if let Some(entity) = graph.entities.iter_mut().find(|e| e.name == entity_name) {
                entity.observations.retain(|o| !obs_to_del.contains(o));
            }
        }
        drop(graph);
        self.persist();

        Ok(ToolResult::text(
            serde_json::to_string(&json!({ "success": true, "message": "observations deleted" }))?,
        ))
    }

    fn delete_relations(&self, args: Value) -> Result<ToolResult> {
        let to_delete: Vec<Relation> = serde_json::from_value(
            args.get("relations")
                .cloned()
                .context("missing relations")?,
        )?;

        let mut graph = self.graph.write();
        graph.relations.retain(|r| {
            !to_delete
                .iter()
                .any(|d| d.from == r.from && d.to == r.to && d.relation_type == r.relation_type)
        });
        drop(graph);
        self.persist();

        Ok(ToolResult::text(
            serde_json::to_string(&json!({ "success": true, "message": "relations deleted" }))?,
        ))
    }

    fn read_graph(&self) -> Result<ToolResult> {
        let graph = self.graph.read();
        Ok(ToolResult::text(serde_json::to_string(&*graph)?))
    }

    fn search_nodes(&self, args: Value) -> Result<ToolResult> {
        debug!("searching knowledge graph nodes");
        let query = args
            .get("query")
            .and_then(|v| v.as_str())
            .context("missing query")?
            .to_lowercase();

        let graph = self.graph.read();
        let matched_entities: Vec<&Entity> = graph
            .entities
            .iter()
            .filter(|e| {
                e.name.to_lowercase().contains(&query)
                    || e.entity_type.to_lowercase().contains(&query)
                    || e.observations
                        .iter()
                        .any(|o| o.to_lowercase().contains(&query))
            })
            .collect();

        let matched_names: Vec<&str> = matched_entities.iter().map(|e| e.name.as_str()).collect();
        let matched_relations: Vec<&Relation> = graph
            .relations
            .iter()
            .filter(|r| matched_names.contains(&r.from.as_str()) || matched_names.contains(&r.to.as_str()))
            .collect();

        Ok(ToolResult::text(serde_json::to_string(&json!({
            "entities": matched_entities,
            "relations": matched_relations,
        }))?))
    }

    fn open_nodes(&self, args: Value) -> Result<ToolResult> {
        let names: Vec<String> =
            serde_json::from_value(args.get("names").cloned().context("missing names")?)?;

        let graph = self.graph.read();
        let matched_entities: Vec<&Entity> = graph
            .entities
            .iter()
            .filter(|e| names.contains(&e.name))
            .collect();

        let matched_names: Vec<&str> = matched_entities.iter().map(|e| e.name.as_str()).collect();
        let matched_relations: Vec<&Relation> = graph
            .relations
            .iter()
            .filter(|r| matched_names.contains(&r.from.as_str()) || matched_names.contains(&r.to.as_str()))
            .collect();

        Ok(ToolResult::text(serde_json::to_string(&json!({
            "entities": matched_entities,
            "relations": matched_relations,
        }))?))
    }
}

fn tool_def(name: &str, description: &str, schema: Value) -> McpTool {
    McpTool {
        name: name.to_string(),
        description: description.to_string(),
        inputSchema: schema,
    }
}
