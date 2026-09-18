// SPDX-License-Identifier: AGPL-3.0-or-later
//! Exercise preset registration through the actual CLI, not a test-only router.

use serde_json::json;
use std::net::TcpListener;
use std::process::{Child, Command, Stdio};
use std::time::Duration;
use wiremock::matchers::{body_json, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

struct ServerProcess(Child);

impl Drop for ServerProcess {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[tokio::test]
async fn cli_dispatches_named_presets_and_rejects_unknown_routes() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("reserve a local port");
    let port = listener.local_addr().unwrap().port();
    let upstream = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(body_json(json!({
            "model": "test-model",
            "messages": [{"role": "user", "content": "hello"}],
            "stream": false,
            "max_tokens": 64,
            "temperature": 0.2
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "msg_preset",
            "type": "message",
            "role": "assistant",
            "model": "test-model",
            "content": [{"type": "text", "text": "preset reached upstream"}],
            "stop_reason": "end_turn",
            "stop_sequence": null,
            "usage": {"input_tokens": 5, "output_tokens": 4}
        })))
        .expect(1)
        .mount(&upstream)
        .await;

    let directory = tempfile::tempdir().unwrap();
    let config_path = directory.path().join("config.json");
    std::fs::write(
        &config_path,
        serde_json::to_vec(&json!({
            "Providers": [{
                "name": "test",
                "api_base_url": format!("{}/v1", upstream.uri()),
                "api_key": "synthetic",
                "protocol": "anthropic",
                "models": ["test-model"]
            }],
            "Router": {"default": "test,test-model"},
            "Presets": {
                "chosen": {"route": "test,test-model", "max_tokens": 64, "temperature": 0.2}
            },
            "Persistence": {"mode": "none"},
            "API_TIMEOUT_MS": 2000
        }))
        .unwrap(),
    )
    .unwrap();

    drop(listener);
    let mut server = ServerProcess(
        Command::new(env!("CARGO_BIN_EXE_ccr-rust"))
            .args(["--config", config_path.to_str().unwrap(), "start"])
            .args(["--host", "127.0.0.1", "--port", &port.to_string()])
            .current_dir(directory.path())
            .env_clear()
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("start CCR CLI"),
    );
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
        .build()
        .unwrap();
    let base = format!("http://127.0.0.1:{port}");
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            assert!(server.0.try_wait().unwrap().is_none(), "CLI exited early");
            if let Ok(response) = client.get(format!("{base}/health")).send().await {
                if response.status().is_success() {
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("CLI health deadline");

    let listing: serde_json::Value = client
        .get(format!("{base}/v1/presets"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(listing[0]["name"], "chosen");
    let request = json!({
        "model": "missing-provider,wrong-model",
        "messages": [{"role": "user", "content": "hello"}],
        "stream": false,
        "max_tokens": 1,
        "temperature": 1.5
    });
    let response = client
        .post(format!("{base}/preset/chosen/v1/messages"))
        .json(&request)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(body["content"][0]["text"], "preset reached upstream");

    for endpoint in ["/preset/missing/v1/messages", "/preset/chosen/v1/responses"] {
        let response = client
            .post(format!("{base}{endpoint}"))
            .json(&request)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::NOT_FOUND);
    }
    upstream.verify().await;
}
