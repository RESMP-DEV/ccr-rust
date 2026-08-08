// SPDX-License-Identifier: AGPL-3.0-or-later
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use futures::stream::{self, StreamExt};
use reqwest::{header::HeaderMap, header::HeaderValue, Client};
use serde_json::{json, Value};

use tracing::{debug, info, warn};

use super::{NativeTool, ToolResult};
use crate::mcp::protocol::McpTool;

const JINA_SEARCH_BASE: &str = "https://s.jina.ai/";
const JINA_READER_BASE: &str = "https://r.jina.ai/";
const MAX_FETCH_URLS: usize = 20;
const MAX_CONCURRENT_FETCHES: usize = 4;
const PER_URL_TIMEOUT: Duration = Duration::from_secs(30);
const FETCH_BATCH_TIMEOUT: Duration = Duration::from_secs(165);

/// Headers applied to every Jina request to control Jina's caching and
/// tracking behavior. These headers do not affect API cost: Jina charges per
/// token regardless of cache/no-cache.
fn jina_request_headers() -> HeaderMap {
    let mut headers = HeaderMap::new();
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
    search_base: String,
    reader_base: String,
}

impl JinaTool {
    pub fn new(client: Client, api_key: String) -> Result<Self> {
        Self::new_with_base_urls(client, api_key, JINA_SEARCH_BASE, JINA_READER_BASE)
    }

    pub(crate) fn new_with_base_urls(
        client: Client,
        api_key: String,
        search_base: impl Into<String>,
        reader_base: impl Into<String>,
    ) -> Result<Self> {
        if api_key.trim().is_empty() {
            return Err(anyhow!("JINA_API_KEY must not be empty"));
        }
        HeaderValue::from_str(&format!("Bearer {api_key}"))
            .context("JINA_API_KEY contains characters that are invalid in an HTTP header")?;
        let search_base = search_base.into();
        let mut reader_base = reader_base.into();
        if !reader_base.ends_with('/') {
            reader_base.push('/');
        }
        reqwest::Url::parse(&search_base).context("invalid Jina search base URL")?;
        reqwest::Url::parse(&reader_base).context("invalid Jina reader base URL")?;
        Ok(Self {
            client,
            api_key,
            search_base,
            reader_base,
        })
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
                            "type": "integer",
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
                            "minItems": 1,
                            "maxItems": MAX_FETCH_URLS,
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
        let Some(query) = args.get("query").and_then(|value| value.as_str()) else {
            return Ok(ToolResult::error("missing required field: query"));
        };
        let num_results = match args.get("numResults") {
            None => 10,
            Some(value) => match value.as_u64() {
                Some(value) => value,
                None => {
                    return Ok(ToolResult::error(
                        "numResults must be an integer between 1 and 20",
                    ));
                }
            },
        };
        if !(1..=20).contains(&num_results) {
            return Ok(ToolResult::error(
                "numResults must be an integer between 1 and 20",
            ));
        }

        let num_results_param = num_results.to_string();
        let resp = match self
            .client
            .get(&self.search_base)
            .query(&[("q", query), ("num", num_results_param.as_str())])
            .bearer_auth(&self.api_key)
            .headers(jina_request_headers())
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

        let payload = match self.truncate_search_results(&text, num_results) {
            Ok(payload) => payload,
            Err(error) => {
                warn!(%error, "jina search returned invalid JSON");
                return Ok(ToolResult::error(format!(
                    "jina search returned invalid JSON: {error}"
                )));
            }
        };
        info!(response_len = payload.len(), "jina search succeeded");
        Ok(ToolResult::text(payload))
    }

    async fn fetch(&self, mut args: Value) -> Result<ToolResult> {
        debug!("jina web fetch called");
        let Some(urls_value) = args.get_mut("urls").map(Value::take) else {
            return Ok(ToolResult::error("missing required field: urls"));
        };
        let Some(url_values) = urls_value.as_array() else {
            return Ok(ToolResult::error("urls must be an array of strings"));
        };
        if url_values.len() > MAX_FETCH_URLS {
            return Ok(ToolResult::error(format!(
                "too many urls: received {}, maximum is {MAX_FETCH_URLS}",
                url_values.len()
            )));
        }
        let urls: Vec<String> = match serde_json::from_value(urls_value) {
            Ok(urls) => urls,
            Err(_) => return Ok(ToolResult::error("urls must be an array of strings")),
        };
        if urls.is_empty() {
            return Ok(ToolResult::error("no urls provided"));
        }

        let fetches = stream::iter(urls.iter().cloned().map(|raw_url| async move {
            let timeout_url = raw_url.clone();
            match tokio::time::timeout(PER_URL_TIMEOUT, self.fetch_one(raw_url)).await {
                Ok(outcome) => outcome,
                Err(_) => FetchOutcome::failure(format!(
                    "--- {timeout_url} (timed out after {} seconds) ---\n",
                    PER_URL_TIMEOUT.as_secs()
                )),
            }
        }))
        .buffered(MAX_CONCURRENT_FETCHES)
        .collect::<Vec<_>>();
        let outcomes = match tokio::time::timeout(FETCH_BATCH_TIMEOUT, fetches).await {
            Ok(outcomes) => outcomes,
            Err(_) => {
                warn!(
                    url_count = urls.len(),
                    timeout_seconds = FETCH_BATCH_TIMEOUT.as_secs(),
                    "jina fetch batch timed out"
                );
                return Ok(ToolResult::error(format!(
                    "jina fetch batch timed out after {} seconds",
                    FETCH_BATCH_TIMEOUT.as_secs()
                )));
            }
        };

        let success_count = outcomes.iter().filter(|outcome| outcome.succeeded).count();
        let failure_count = outcomes.len() - success_count;
        let combined = outcomes
            .into_iter()
            .map(|outcome| outcome.text)
            .collect::<Vec<_>>()
            .join("\n\n");
        info!(
            url_count = urls.len(),
            success_count,
            failure_count,
            response_len = combined.len(),
            "jina fetch completed"
        );
        if success_count == 0 {
            warn!(failure_count, "all jina fetches failed");
            Ok(ToolResult::error(combined))
        } else {
            Ok(ToolResult::text(combined))
        }
    }

    async fn fetch_one(&self, raw_url: String) -> FetchOutcome {
        let parsed_url = match reqwest::Url::parse(&raw_url) {
            Ok(url)
                if matches!(url.scheme(), "http" | "https")
                    && url.host_str().is_some_and(|host| !host.is_empty()) =>
            {
                url
            }
            _ => {
                return FetchOutcome::failure(format!(
                    "--- {raw_url} (invalid URL: expected http or https with a host) ---\n"
                ));
            }
        };
        if is_forbidden_fetch_host(&parsed_url) {
            return FetchOutcome::failure(format!(
                "--- {raw_url} (invalid URL: private or local hosts are not allowed) ---\n"
            ));
        }
        let target = format!("{}{parsed_url}", self.reader_base);
        let resp = match self
            .client
            .get(&target)
            .bearer_auth(&self.api_key)
            .headers(jina_request_headers())
            .header("Accept", "text/plain")
            .send()
            .await
        {
            Ok(response) => response,
            Err(error) => {
                warn!(url = %raw_url, %error, "jina fetch request failed");
                return FetchOutcome::failure(format!(
                    "--- {raw_url} (request failed: {error}) ---\n"
                ));
            }
        };
        let status = resp.status();
        let body = match resp.text().await {
            Ok(body) => body,
            Err(error) => {
                warn!(url = %raw_url, %error, "jina fetch body read failed");
                return FetchOutcome::failure(format!(
                    "--- {raw_url} (body read failed: {error}) ---\n"
                ));
            }
        };
        if status.is_success() {
            FetchOutcome::success(format!("--- {raw_url} ---\n{body}"))
        } else {
            warn!(url = %raw_url, %status, "jina fetch failed");
            FetchOutcome::failure(format!("--- {raw_url} (failed: {status}) ---\n{body}"))
        }
    }

    /// Truncate the s.jina.ai JSON response to `num` hits.
    fn truncate_search_results(&self, text: &str, num: u64) -> serde_json::Result<String> {
        let parsed = serde_json::from_str::<Value>(text)?;
        let mut trimmed = parsed;
        if let Some(data) = trimmed.get_mut("data").and_then(|d| d.as_array_mut()) {
            data.truncate(num as usize);
        }
        Ok(trimmed.to_string())
    }
}

fn is_forbidden_fetch_host(url: &reqwest::Url) -> bool {
    let Some(host) = url.host_str() else {
        return true;
    };
    let normalized = host
        .trim_end_matches('.')
        .trim_start_matches('[')
        .trim_end_matches(']')
        .to_ascii_lowercase();
    if normalized == "localhost"
        || normalized.ends_with(".localhost")
        || normalized.ends_with(".local")
    {
        return true;
    }
    let Ok(address) = normalized.parse::<std::net::IpAddr>() else {
        return looks_like_noncanonical_ipv4(&normalized);
    };
    match address {
        std::net::IpAddr::V4(address) => is_forbidden_ipv4(address),
        std::net::IpAddr::V6(address) => {
            if let Some(address) = address.to_ipv4_mapped() {
                return is_forbidden_ipv4(address);
            }
            address.is_loopback()
                || address.is_unspecified()
                || address.is_multicast()
                || address.is_unique_local()
                || address.is_unicast_link_local()
        }
    }
}

fn looks_like_noncanonical_ipv4(host: &str) -> bool {
    let decimal_or_dotted = host
        .chars()
        .all(|character| character.is_ascii_digit() || character == '.');
    let hexadecimal = host.strip_prefix("0x").is_some_and(|digits| {
        !digits.is_empty()
            && digits
                .chars()
                .all(|character| character.is_ascii_hexdigit())
    });
    decimal_or_dotted || hexadecimal
}

fn is_forbidden_ipv4(address: std::net::Ipv4Addr) -> bool {
    address.is_private()
        || address.is_loopback()
        || address.is_link_local()
        || address.is_unspecified()
        || address.is_multicast()
        || address.is_broadcast()
}

struct FetchOutcome {
    text: String,
    succeeded: bool,
}

impl FetchOutcome {
    fn success(text: String) -> Self {
        Self {
            text,
            succeeded: true,
        }
    }

    fn failure(text: String) -> Self {
        Self {
            text,
            succeeded: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use serde_json::json;
    use wiremock::matchers::{header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::{JinaTool, NativeTool};

    fn test_tool(server: &MockServer) -> JinaTool {
        JinaTool::new_with_base_urls(
            reqwest::Client::new(),
            "test-key".to_string(),
            format!("{}/", server.uri()),
            format!("{}/", server.uri()),
        )
        .expect("valid test configuration")
    }

    #[test]
    fn constructor_rejects_api_keys_that_cannot_form_a_header() {
        let result = JinaTool::new(reqwest::Client::new(), "bad\nkey".to_string());

        assert!(result.is_err());
    }

    #[test]
    fn constructor_normalizes_reader_base_trailing_slash() {
        let tool = JinaTool::new_with_base_urls(
            reqwest::Client::new(),
            "test-key".to_string(),
            "http://example.test/search",
            "http://example.test/reader",
        )
        .expect("valid test configuration");

        assert_eq!(tool.search_base, "http://example.test/search");
        assert_eq!(tool.reader_base, "http://example.test/reader/");
    }

    #[tokio::test]
    async fn search_sends_auth_and_privacy_headers_and_truncates_results() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/"))
            .and(query_param("q", "rust"))
            .and(query_param("num", "2"))
            .and(header("authorization", "Bearer test-key"))
            .and(header("x-no-cache", "true"))
            .and(header("x-ttl", "0"))
            .and(header("dnt", "1"))
            .and(header("x-no-track", "true"))
            .and(header("accept", "application/json"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": [{"title": "one"}, {"title": "two"}, {"title": "three"}]
            })))
            .expect(1)
            .mount(&server)
            .await;

        let result = test_tool(&server)
            .call("web_search_jina", json!({"query": "rust", "numResults": 2}))
            .await
            .expect("tool call succeeds");

        assert!(!result.is_error);
        let payload: serde_json::Value =
            serde_json::from_str(&result.content[0].text).expect("JSON search response");
        assert_eq!(payload["data"].as_array().map(Vec::len), Some(2));
    }

    #[tokio::test]
    async fn search_reports_a_successful_non_json_response_as_an_error() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_string("not json"))
            .expect(1)
            .mount(&server)
            .await;

        let result = test_tool(&server)
            .call("web_search_jina", json!({"query": "rust"}))
            .await
            .expect("tool call succeeds");

        assert!(result.is_error);
        assert!(result.content[0].text.contains("invalid JSON"));
    }

    #[tokio::test]
    async fn search_rejects_non_integer_result_counts_before_sending() {
        let server = MockServer::start().await;

        let result = test_tool(&server)
            .call(
                "web_search_jina",
                json!({"query": "rust", "numResults": 1.5}),
            )
            .await
            .expect("tool call succeeds");

        assert!(result.is_error);
        assert!(result.content[0].text.contains("must be an integer"));
        assert!(server
            .received_requests()
            .await
            .expect("request log")
            .is_empty());
    }

    #[tokio::test]
    async fn search_reports_missing_query_as_tool_error() {
        let server = MockServer::start().await;

        let result = test_tool(&server)
            .call("web_search_jina", json!({}))
            .await
            .expect("tool call succeeds");

        assert!(result.is_error);
        assert!(result.content[0]
            .text
            .contains("missing required field: query"));
    }

    #[tokio::test]
    async fn fetch_preserves_order_and_reports_mixed_outcomes() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/https://example.com/ok"))
            .and(header("accept", "text/plain"))
            .respond_with(ResponseTemplate::new(200).set_body_string("ok body"))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/https://example.com/fail"))
            .respond_with(ResponseTemplate::new(503).set_body_string("unavailable"))
            .expect(1)
            .mount(&server)
            .await;

        let result = test_tool(&server)
            .call(
                "web_fetch_jina",
                json!({"urls": [
                    "https://example.com/ok",
                    "not-a-url",
                    "https://example.com/fail"
                ]}),
            )
            .await
            .expect("tool call succeeds");

        assert!(!result.is_error);
        let text = &result.content[0].text;
        let ok = text.find("https://example.com/ok").expect("successful URL");
        let invalid = text.find("not-a-url").expect("invalid URL");
        let failed = text.find("https://example.com/fail").expect("failed URL");
        assert!(ok < invalid && invalid < failed);
        assert!(text.contains("ok body"));
        assert!(text.contains("503 Service Unavailable"));
    }

    #[tokio::test]
    async fn fetch_runs_requests_concurrently() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_delay(Duration::from_millis(150))
                    .set_body_string("body"),
            )
            .expect(4)
            .mount(&server)
            .await;
        let urls = (0..4)
            .map(|index| format!("https://example.com/{index}"))
            .collect::<Vec<_>>();

        let started = Instant::now();
        let result = test_tool(&server)
            .call("web_fetch_jina", json!({"urls": urls}))
            .await
            .expect("tool call succeeds");

        assert!(!result.is_error);
        assert!(
            started.elapsed() < Duration::from_millis(450),
            "four 150 ms requests should execute concurrently"
        );
    }

    #[tokio::test]
    async fn fetch_rejects_missing_malformed_and_hostless_urls() {
        let server = MockServer::start().await;
        let tool = test_tool(&server);

        let missing = tool
            .call("web_fetch_jina", json!({}))
            .await
            .expect("tool call succeeds");
        let malformed = tool
            .call("web_fetch_jina", json!({"urls": [1, 2]}))
            .await
            .expect("tool call succeeds");
        let hostless = tool
            .call("web_fetch_jina", json!({"urls": ["not-a-url"]}))
            .await
            .expect("tool call succeeds");
        let too_many = tool
            .call(
                "web_fetch_jina",
                json!({"urls": vec!["https://example.com"; super::MAX_FETCH_URLS + 1]}),
            )
            .await
            .expect("tool call succeeds");
        let private_urls = [
            "http://127.0.0.1/admin",
            "http://169.254.169.254/latest/meta-data/",
            "http://localhost:8080/admin",
            "http://[::1]/admin",
            "http://[::ffff:127.0.0.1]/admin",
            "http://[::ffff:10.0.0.1]/admin",
            "http://2130706433/admin",
            "http://0x7f000001/admin",
            "http://127.1/admin",
        ];

        assert!(missing.is_error);
        assert!(missing.content[0].text.contains("missing required field"));
        assert!(malformed.is_error);
        assert!(malformed.content[0].text.contains("array of strings"));
        assert!(hostless.is_error);
        assert!(hostless.content[0].text.contains("with a host"));
        assert!(too_many.is_error);
        assert!(too_many.content[0].text.contains("too many urls"));
        for private_url in private_urls {
            let private_host = tool
                .call("web_fetch_jina", json!({"urls": [private_url]}))
                .await
                .expect("tool call succeeds");
            assert!(private_host.is_error, "{private_url} was not rejected");
            assert!(
                private_host.content[0]
                    .text
                    .contains("private or local hosts are not allowed"),
                "unexpected rejection for {private_url}: {}",
                private_host.content[0].text
            );
        }
        assert!(server
            .received_requests()
            .await
            .expect("request log")
            .is_empty());
    }
}
