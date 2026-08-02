// SPDX-License-Identifier: AGPL-3.0-or-later

use axum::{
    body::{to_bytes, Body},
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};

use super::{
    convert_openai_json_response_to_responses, convert_openai_stream_response_to_responses,
    decode_request_body, parse_json_payload, responses_request_to_openai_chat_request,
    DecodeRequestBodyError, MAX_RESPONSES_BODY_BYTES,
};
use crate::router::{openai_compat::handle_chat_completions, AppState};

fn body_read_error_status(error: &(dyn std::error::Error + 'static)) -> StatusCode {
    if error
        .source()
        .is_some_and(|source| source.is::<http_body_util::LengthLimitError>())
    {
        StatusCode::PAYLOAD_TOO_LARGE
    } else {
        StatusCode::BAD_REQUEST
    }
}

fn decode_error_status(error: &DecodeRequestBodyError) -> StatusCode {
    if error.is_payload_too_large() {
        StatusCode::PAYLOAD_TOO_LARGE
    } else {
        StatusCode::BAD_REQUEST
    }
}

/// Handle OpenAI Responses API requests.
pub async fn handle_responses(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let body_bytes = match to_bytes(body, MAX_RESPONSES_BODY_BYTES).await {
        Ok(bytes) => bytes,
        Err(err) => {
            let status = body_read_error_status(&err);
            return (
                status,
                Json(serde_json::json!({
                    "error": {
                        "message": format!("Failed to read request body: {}", err)
                    }
                })),
            )
                .into_response();
        }
    };

    let decoded = match decode_request_body(&body_bytes, &headers) {
        Ok(bytes) => bytes,
        Err(err) => {
            return (
                decode_error_status(&err),
                Json(serde_json::json!({"error": {"message": err.to_string()}})),
            )
                .into_response();
        }
    };

    let request_body = match parse_json_payload(&decoded) {
        Ok(value) => value,
        Err(err) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": {"message": err}})),
            )
                .into_response();
        }
    };

    let stream_requested = request_body
        .get("stream")
        .and_then(|value| value.as_bool())
        .unwrap_or(true);

    let openai_chat_request = match responses_request_to_openai_chat_request(&request_body) {
        Ok(request) => request,
        Err(err) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": {"message": err}})),
            )
                .into_response();
        }
    };

    let openai_response =
        handle_chat_completions(State(state), headers, Json(openai_chat_request)).await;

    if stream_requested {
        convert_openai_stream_response_to_responses(openai_response).await
    } else {
        convert_openai_json_response_to_responses(openai_response).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn responses_requests_require_valid_input_or_instructions() {
        let missing = responses_request_to_openai_chat_request(&json!({"model": "test"}));
        assert_eq!(
            missing.unwrap_err(),
            "responses request requires 'input' or 'instructions'"
        );

        let invalid = responses_request_to_openai_chat_request(&json!({
            "model": "test",
            "input": {"unexpected": true}
        }));
        assert_eq!(
            invalid.unwrap_err(),
            "responses request 'input' must be text or an array"
        );

        let reasoning = responses_request_to_openai_chat_request(&json!({
            "model": "test",
            "input": "think carefully",
            "reasoning": {"effort": "high", "summary": "detailed"}
        }))
        .unwrap();
        assert_eq!(
            reasoning["reasoning"],
            json!({"effort": "high", "summary": "detailed"})
        );
    }

    #[test]
    fn zstd_request_decompression_is_bounded() {
        let encoded = zstd::stream::encode_all(
            std::io::Cursor::new(vec![0_u8; MAX_RESPONSES_BODY_BYTES + 1]),
            1,
        )
        .unwrap();
        let mut headers = HeaderMap::new();
        headers.insert(
            axum::http::header::CONTENT_ENCODING,
            axum::http::HeaderValue::from_static("zstd"),
        );

        let error = decode_request_body(&encoded, &headers).unwrap_err();
        assert!(error.contains("exceeds"));
        assert_eq!(decode_error_status(&error), StatusCode::PAYLOAD_TOO_LARGE);
    }

    #[test]
    fn zstd_request_rejects_frames_above_window_limit() {
        let mut encoder = zstd::stream::Encoder::new(Vec::new(), 1).unwrap();
        encoder.include_contentsize(false).unwrap();
        encoder.long_distance_matching(true).unwrap();
        encoder
            .window_log(super::super::MAX_RESPONSES_ZSTD_WINDOW_LOG + 1)
            .unwrap();
        std::io::Write::write_all(&mut encoder, b"small request").unwrap();
        let encoded = encoder.finish().unwrap();
        let mut headers = HeaderMap::new();
        headers.insert(
            axum::http::header::CONTENT_ENCODING,
            axum::http::HeaderValue::from_static("zstd"),
        );

        let error = decode_request_body(&encoded, &headers).unwrap_err();

        assert!(
            error.contains("requires too much memory"),
            "unexpected zstd error: {error}"
        );
    }

    #[tokio::test]
    async fn oversized_request_body_maps_to_payload_too_large() {
        let error = to_bytes(
            Body::from(vec![0_u8; MAX_RESPONSES_BODY_BYTES + 1]),
            MAX_RESPONSES_BODY_BYTES,
        )
        .await
        .unwrap_err();

        assert_eq!(
            body_read_error_status(&error),
            StatusCode::PAYLOAD_TOO_LARGE
        );
    }
}
