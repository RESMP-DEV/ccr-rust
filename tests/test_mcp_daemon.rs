// SPDX-License-Identifier: AGPL-3.0-or-later
use serde_json::{json, Value};

async fn start_daemon(port: u16) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        ccr_rust::mcp::daemon::run(ccr_rust::mcp::daemon::DaemonArgs {
            port,
            host: "127.0.0.1".to_string(),
            memory_dir: None,
            pyright_root: None,
        })
        .await
        .ok();
    })
}

async fn mcp_post(port: u16, body: &Value) -> Value {
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("http://127.0.0.1:{port}/mcp"))
        .json(body)
        .send()
        .await
        .expect("request failed");
    resp.json().await.expect("json parse failed")
}

#[tokio::test]
async fn test_initialize_and_tools_list() {
    let port = 13457;
    let _handle = start_daemon(port).await;
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let init_resp = mcp_post(
        port,
        &json!({
            "jsonrpc": "2.0",
            "id": "1",
            "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": { "name": "test", "version": "0.1" }
            }
        }),
    )
    .await;

    assert_eq!(
        init_resp["result"]["serverInfo"]["name"],
        "ccr-rust-mcp-daemon"
    );

    let list_resp = mcp_post(
        port,
        &json!({
            "jsonrpc": "2.0",
            "id": "2",
            "method": "tools/list",
            "params": {}
        }),
    )
    .await;

    let tools = list_resp["result"]["tools"]
        .as_array()
        .expect("tools array");
    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();

    // context7 tools are always present (no API key needed)
    assert!(
        names.contains(&"resolve-library-id"),
        "missing context7 resolve"
    );
    assert!(names.contains(&"query-docs"), "missing context7 query");

    // memory tools are always present
    assert!(
        names.contains(&"create_entities"),
        "missing memory create_entities"
    );
    assert!(names.contains(&"read_graph"), "missing memory read_graph");
    assert!(
        names.contains(&"search_nodes"),
        "missing memory search_nodes"
    );
}

#[tokio::test]
async fn test_memory_crud() {
    let port = 13458;
    let _handle = start_daemon(port).await;
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    // Initialize
    mcp_post(
        port,
        &json!({
            "jsonrpc": "2.0", "id": "init", "method": "initialize",
            "params": { "protocolVersion": "2024-11-05", "capabilities": {}, "clientInfo": { "name": "test", "version": "0.1" } }
        }),
    ).await;

    // Create entity
    let create_resp = mcp_post(
        port,
        &json!({
            "jsonrpc": "2.0", "id": "c1", "method": "tools/call",
            "params": {
                "name": "create_entities",
                "arguments": {
                    "entities": [{
                        "name": "TestProject",
                        "entityType": "project",
                        "observations": ["uses Rust", "MCP daemon"]
                    }]
                }
            }
        }),
    )
    .await;

    assert!(create_resp["result"]["content"].is_array());
    assert!(!create_resp["result"]["isError"].as_bool().unwrap_or(true));

    // Read graph
    let read_resp = mcp_post(
        port,
        &json!({
            "jsonrpc": "2.0", "id": "r1", "method": "tools/call",
            "params": { "name": "read_graph", "arguments": {} }
        }),
    )
    .await;

    let content_text = read_resp["result"]["content"][0]["text"].as_str().unwrap();
    let graph: Value = serde_json::from_str(content_text).unwrap();
    assert_eq!(graph["entities"][0]["name"], "TestProject");

    // Search
    let search_resp = mcp_post(
        port,
        &json!({
            "jsonrpc": "2.0", "id": "s1", "method": "tools/call",
            "params": { "name": "search_nodes", "arguments": { "query": "Rust" } }
        }),
    )
    .await;

    let search_text = search_resp["result"]["content"][0]["text"]
        .as_str()
        .unwrap();
    let search_result: Value = serde_json::from_str(search_text).unwrap();
    assert!(!search_result["entities"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn test_health_endpoint() {
    let port = 13459;
    let _handle = start_daemon(port).await;
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let client = reqwest::Client::new();
    let resp = client
        .get(format!("http://127.0.0.1:{port}/health"))
        .send()
        .await
        .expect("health check failed");
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.text().await.unwrap(), "ok");
}
