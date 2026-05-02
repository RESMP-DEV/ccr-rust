// SPDX-License-Identifier: AGPL-3.0-or-later
//! Focused unit tests for catalog behaviour.

use super::catalog::{CompressionLevel, DuplicateToolSignal, ToolCatalog};
use super::protocol::McpTool;
use serde_json::json;

/// Build a minimal tool definition.
fn make_tool(name: &str, description: &str) -> McpTool {
    McpTool {
        name: name.to_string(),
        description: description.to_string(),
        inputSchema: json!({}),
    }
}

#[test]
fn mcp_catalog_duplicate_tool_names_are_not_silent() {
    let mut catalog = ToolCatalog::default();

    // First backend registers "tool-a".
    let dups1 = catalog.add_backend_tools(0, vec![make_tool("tool-a", "from backend 0")]);
    assert!(dups1.is_empty(), "first registration must not produce duplicate signals");
    assert_eq!(catalog.tools.len(), 1);

    // Second backend also registers "tool-a" — this should NOT be silent.
    let dups2 = catalog.add_backend_tools(1, vec![make_tool("tool-a", "from backend 1")]);
    assert_eq!(dups2.len(), 1, "second registration must emit a duplicate signal");

    let signal = &dups2[0];
    assert_eq!(signal.tool_name, "tool-a");
    assert_eq!(signal.existing_backend, 0, "first backend should be recorded as existing");
    assert_eq!(signal.conflicting_backend, 1, "second backend should be recorded as conflicting");

    // The catalog should now contain exactly one tool (the latest definition wins).
    assert_eq!(catalog.tools.len(), 1);
    assert_eq!(catalog.tools[0].description, "from backend 1");

    // Routing should map to the latest backend.
    assert_eq!(catalog.route("tool-a"), Some(1));
}

#[test]
fn mcp_catalog_level_aliases_are_consistent() {
    // Verify supported_levels exposes every alias exactly once and all parse correctly.
    let levels = ToolCatalog::supported_levels();

    // "none" -> None
    assert!(levels.contains(&"none"));
    assert_eq!(CompressionLevel::parse("none"), Some(CompressionLevel::None));

    // "minimal" and its aliases -> Minimal
    assert!(levels.contains(&"minimal"));
    assert!(levels.contains(&"min"));
    assert!(levels.contains(&"high"));
    assert!(levels.contains(&"aggressive"));
    assert_eq!(CompressionLevel::parse("minimal"), Some(CompressionLevel::Minimal));
    assert_eq!(CompressionLevel::parse("min"), Some(CompressionLevel::Minimal));
    assert_eq!(CompressionLevel::parse("high"), Some(CompressionLevel::Minimal));
    assert_eq!(CompressionLevel::parse("aggressive"), Some(CompressionLevel::Minimal));

    // "medium" and its aliases -> Medium
    assert!(levels.contains(&"medium"));
    assert!(levels.contains(&"med"));
    assert!(levels.contains(&"light"));
    assert_eq!(CompressionLevel::parse("medium"), Some(CompressionLevel::Medium));
    assert_eq!(CompressionLevel::parse("med"), Some(CompressionLevel::Medium));
    assert_eq!(CompressionLevel::parse("light"), Some(CompressionLevel::Medium));

    // Unknown strings must not parse.
    assert_eq!(CompressionLevel::parse("full"), None);
    assert_eq!(CompressionLevel::parse("detailed"), None);
    assert_eq!(CompressionLevel::parse(""), None);

    // Ensure no duplicate entries in the canonical list.
    let mut unique = levels.clone();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(levels.len(), unique.len(), "supported_levels must not contain duplicates");
}

#[test]
fn compress_rejects_unknown_levels() {
    let catalog = ToolCatalog::default();
    let result = catalog.compress("full");
    assert!(result.is_err(), "unknown compression level must return an error");
    assert!(result.unwrap_err().contains("unknown compression level"));
}

#[test]
fn compress_levels_behave_stably() {
    let mut catalog = ToolCatalog::default();
    catalog.add_backend_tools(0, vec![
        make_tool("t1", "  alpha   beta  gamma  "),
        make_tool("t2", "single"),
    ]);

    // None: description cleared, schema emptied.
    let none_result = catalog.compress("none").unwrap();
    assert_eq!(none_result[0].description, "");
    assert!(none_result[0].inputSchema.as_object().unwrap().is_empty());
    assert_eq!(none_result[1].description, "");

    // Minimal: description cleared, schema preserved.
    let min_result = catalog.compress("min").unwrap();
    assert_eq!(min_result[0].description, "");
    assert!(!min_result[0].inputSchema.as_object().unwrap().is_empty());

    // Medium: whitespace-normalised and truncated to 256 chars.
    let med_result = catalog.compress("medium").unwrap();
    assert_eq!(med_result[0].description, "alpha beta gamma");
    assert!(med_result[0].description.len() <= 256);

    // Aliases produce identical output.
    let alias_names = ["min", "high", "aggressive"];
    for &alias in &alias_names {
        let alias_result = catalog.compress(alias).unwrap();
        assert_eq!(alias_result[0].description, min_result[0].description);
    }

    let medium_alias_names = ["med", "light"];
    for &alias in &medium_alias_names {
        let alias_result = catalog.compress(alias).unwrap();
        assert_eq!(alias_result[0].description, med_result[0].description);
    }
}