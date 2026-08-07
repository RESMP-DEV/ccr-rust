// SPDX-License-Identifier: AGPL-3.0-or-later
use serde_json::{json, Value};
use wiremock::matchers::{header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const AUTH_TOKEN: &str = "mcp-daemon-test-token";

async fn start_daemon(port: u16) -> tokio::task::JoinHandle<()> {
    start_daemon_with_jina(port, None, None).await
}

async fn start_daemon_with_jina(
    port: u16,
    jina_api_key: Option<String>,
    jina_bases: Option<(String, String)>,
) -> tokio::task::JoinHandle<()> {
    let (jina_search_base, jina_reader_base) = jina_bases
        .map(|(search, reader)| (Some(search), Some(reader)))
        .unwrap_or((None, None));
    tokio::spawn(async move {
        ccr_rust::mcp::daemon::run(ccr_rust::mcp::daemon::DaemonArgs {
            port,
            host: "127.0.0.1".to_string(),
            auth_token: AUTH_TOKEN.to_string(),
            memory_dir: None,
            pyright_root: None,
            pyright_workspace_dir: None,
            jina_api_key,
            jina_search_base,
            jina_reader_base,
        })
        .await
        .ok();
    })
}

#[tokio::test]
async fn test_jina_tools_call_through_daemon_dispatch() {
    let upstream = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/search"))
        .and(query_param("q", "rust"))
        .and(query_param("num", "1"))
        .and(header("authorization", "Bearer test-jina-key"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [{"title": "one"}, {"title": "two"}]
        })))
        .expect(1)
        .mount(&upstream)
        .await;
    Mock::given(method("GET"))
        .and(path("/reader/https://example.com/page"))
        .and(header("authorization", "Bearer test-jina-key"))
        .respond_with(ResponseTemplate::new(200).set_body_string("page body"))
        .expect(1)
        .mount(&upstream)
        .await;

    let port = 13463;
    let _handle = start_daemon_with_jina(
        port,
        Some("test-jina-key".to_string()),
        Some((
            format!("{}/search", upstream.uri()),
            format!("{}/reader/", upstream.uri()),
        )),
    )
    .await;
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let init_resp = mcp_post(
        port,
        &json!({
            "jsonrpc": "2.0", "id": "init", "method": "initialize",
            "params": { "protocolVersion": "2024-11-05", "capabilities": {}, "clientInfo": { "name": "test", "version": "0.1" } }
        }),
    )
    .await;
    assert!(
        init_resp.get("error").is_none(),
        "initialize failed: {init_resp}"
    );

    let search = mcp_post(
        port,
        &json!({
            "jsonrpc": "2.0", "id": "jina-search", "method": "tools/call",
            "params": {"name": "web_search_jina", "arguments": {"query": "rust", "numResults": 1}}
        }),
    )
    .await;
    assert_eq!(search["result"]["isError"], false);
    let search_text = search["result"]["content"][0]["text"]
        .as_str()
        .expect("search text");
    let search_payload: Value = serde_json::from_str(search_text).expect("search JSON");
    assert_eq!(search_payload["data"].as_array().map(Vec::len), Some(1));

    let fetch = mcp_post(
        port,
        &json!({
            "jsonrpc": "2.0", "id": "jina-fetch", "method": "tools/call",
            "params": {"name": "web_fetch_jina", "arguments": {"urls": ["https://example.com/page"]}}
        }),
    )
    .await;
    assert_eq!(fetch["result"]["isError"], false);
    assert!(fetch["result"]["content"][0]["text"]
        .as_str()
        .expect("fetch text")
        .contains("page body"));
}

async fn mcp_post(port: u16, body: &Value) -> Value {
    let resp = send_mcp(port, body, Some(AUTH_TOKEN)).await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    resp.json().await.expect("json parse failed")
}

async fn send_mcp(port: u16, body: &Value, auth_token: Option<&str>) -> reqwest::Response {
    let client = reqwest::Client::new();
    let request = client
        .post(format!("http://127.0.0.1:{port}/mcp"))
        .json(body);
    let request = match auth_token {
        Some(token) => request.bearer_auth(token),
        None => request,
    };
    request.send().await.expect("request failed")
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
    let init_resp = mcp_post(
        port,
        &json!({
            "jsonrpc": "2.0", "id": "init", "method": "initialize",
            "params": { "protocolVersion": "2024-11-05", "capabilities": {}, "clientInfo": { "name": "test", "version": "0.1" } }
        }),
    ).await;
    assert!(
        init_resp.get("error").is_none(),
        "initialize failed: {init_resp}"
    );

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
        .bearer_auth(AUTH_TOKEN)
        .send()
        .await
        .expect("health check failed");
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.text().await.unwrap(), "ok");
}

#[tokio::test]
async fn test_health_and_mcp_require_correct_bearer_auth() {
    let port = 13460;
    let _handle = start_daemon(port).await;
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let client = reqwest::Client::new();
    for auth_token in [None, Some("wrong-token")] {
        let request = client.get(format!("http://127.0.0.1:{port}/health"));
        let request = match auth_token {
            Some(token) => request.bearer_auth(token),
            None => request,
        };
        let response = request.send().await.unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::UNAUTHORIZED);
        assert_eq!(
            response
                .headers()
                .get(reqwest::header::WWW_AUTHENTICATE)
                .unwrap(),
            "Bearer"
        );
    }

    let health = client
        .get(format!("http://127.0.0.1:{port}/health"))
        .bearer_auth(AUTH_TOKEN)
        .send()
        .await
        .unwrap();
    assert_eq!(health.status(), reqwest::StatusCode::OK);

    let ping = json!({"jsonrpc": "2.0", "id": "ping", "method": "ping"});
    for auth_token in [None, Some("wrong-token")] {
        let response = send_mcp(port, &ping, auth_token).await;
        assert_eq!(response.status(), reqwest::StatusCode::UNAUTHORIZED);
        assert_eq!(
            response
                .headers()
                .get(reqwest::header::WWW_AUTHENTICATE)
                .unwrap(),
            "Bearer"
        );
    }

    let response = send_mcp(port, &ping, Some(AUTH_TOKEN)).await;
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["result"], json!({}));
}

/// When JINA_API_KEY is set, the daemon must register the web_search_jina and
/// web_fetch_jina tools. Without the key they must be absent. This completes
/// the Python-side readiness contract (services.py _WEB_SEARCH_TOOLS).
#[tokio::test]
async fn test_jina_tools_registered_when_key_set() {
    // Use a unique port to avoid collisions with other tests.
    let port = 13461;
    let _handle = start_daemon_with_jina(port, Some("test-jina-key".to_string()), None).await;
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let init_resp = mcp_post(
        port,
        &json!({
            "jsonrpc": "2.0", "id": "init", "method": "initialize",
            "params": { "protocolVersion": "2024-11-05", "capabilities": {}, "clientInfo": { "name": "test", "version": "0.1" } }
        }),
    )
    .await;
    assert!(
        init_resp.get("error").is_none(),
        "initialize failed: {init_resp}"
    );

    let list_resp = mcp_post(
        port,
        &json!({
            "jsonrpc": "2.0", "id": "tl", "method": "tools/list", "params": {}
        }),
    )
    .await;

    let tools = list_resp["result"]["tools"]
        .as_array()
        .expect("tools array");
    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();

    assert!(
        names.contains(&"web_search_jina"),
        "web_search_jina must be registered when JINA_API_KEY is set: {names:?}"
    );
    assert!(
        names.contains(&"web_fetch_jina"),
        "web_fetch_jina must be registered when JINA_API_KEY is set: {names:?}"
    );
}

#[tokio::test]
async fn test_jina_tools_absent_when_key_override_is_empty() {
    let port = 13462;
    let _handle = start_daemon_with_jina(port, Some(String::new()), None).await;
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let init_resp = mcp_post(
        port,
        &json!({
            "jsonrpc": "2.0", "id": "init", "method": "initialize",
            "params": { "protocolVersion": "2024-11-05", "capabilities": {}, "clientInfo": { "name": "test", "version": "0.1" } }
        }),
    )
    .await;
    assert!(
        init_resp.get("error").is_none(),
        "initialize failed: {init_resp}"
    );

    let list_resp = mcp_post(
        port,
        &json!({
            "jsonrpc": "2.0", "id": "tl", "method": "tools/list", "params": {}
        }),
    )
    .await;

    let tools = list_resp["result"]["tools"]
        .as_array()
        .expect("tools array");
    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();

    assert!(
        !names.contains(&"web_search_jina"),
        "web_search_jina must be absent without JINA_API_KEY"
    );
    assert!(
        !names.contains(&"web_fetch_jina"),
        "web_fetch_jina must be absent without JINA_API_KEY"
    );
}
