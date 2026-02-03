use axum::{
    body::Body,
    http::{header, StatusCode},
    response::{IntoResponse, Response},
};
use bytes::Bytes;
use futures::StreamExt;
use tokio_stream::wrappers::ReceiverStream;

use crate::metrics::increment_active_streams;

pub async fn stream_response(resp: reqwest::Response) -> Response {
    increment_active_streams(1);
    
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Bytes, std::io::Error>>(100);
    
    tokio::spawn(async move {
        let mut stream = resp.bytes_stream();
        
        while let Some(chunk) = stream.next().await {
            match chunk {
                Ok(bytes) => {
                    if tx.send(Ok(bytes)).await.is_err() {
                        break;
                    }
                }
                Err(e) => {
                    let _ = tx.send(Err(std::io::Error::new(
                        std::io::ErrorKind::Other,
                        e.to_string(),
                    ))).await;
                    break;
                }
            }
        }
        
        increment_active_streams(-1);
    });
    
    let body = Body::from_stream(ReceiverStream::new(rx));
    
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .header(header::CACHE_CONTROL, "no-cache")
        .header(header::CONNECTION, "keep-alive")
        .body(body)
        .unwrap()
}
