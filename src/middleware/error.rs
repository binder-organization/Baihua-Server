use crate::ServerState;
use crate::common::StandardResponse;
use crate::common::error::ErrorResponse;
use axum::{
    Json,
    extract::{Request, State},
    http::{Method, StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use tracing::error;

// Panic error handler.
// Theoretically, the business logic layer should not throw a panic.
pub async fn panic(request: Request, next: Next) -> Response {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| next.run(request)));

    match result {
        Ok(response) => response.await,
        Err(_) => {
            let error = StandardResponse::error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "SERVER_CRASHES".to_string(),
                "The server encountered an error.".to_string(),
            );

            error!(
                "The server used 'panic!', returned an 'HTTP 500' error, ID: {}.",
                error.body.response_id
            );

            (error.status, Json(error.body)).into_response()
        }
    }
}

// Not found(404) error handler.
pub async fn not_found(request: Request, next: Next) -> Response {
    let response = next.run(request).await;

    if response.status() == StatusCode::NOT_FOUND {
        let error = StandardResponse::error(
            StatusCode::NOT_FOUND,
            "NOT_FOUND_ERROR".to_string(),
            "The requested resource was not found.".to_string(),
        );

        return (error.status, Json(error.body)).into_response();
    }

    response
}

// Method not allowed(405) error handler.
pub async fn method_not_allowed(request: Request, next: Next) -> Response {
    let response = next.run(request).await;

    if response.status() == StatusCode::METHOD_NOT_ALLOWED {
        return ErrorResponse::MethodNotAllowed(
            "The requested method is not allowed for this resource.".to_string(),
        )
        .into_response();
    }

    response
}

// Payload too large(413) error handler.
pub async fn payload_too_large(
    State(state): State<Arc<ServerState>>,
    request: Request,
    next: Next,
) -> Response {
    let file_upload_request = is_file_upload_request(&request);
    let response = next.run(request).await;

    if response.status() == StatusCode::PAYLOAD_TOO_LARGE {
        if response
            .headers()
            .get(header::CONTENT_TYPE)
            .is_some_and(|value| value.as_bytes().starts_with(b"application/json"))
        {
            return response;
        }
        if file_upload_request {
            return ErrorResponse::PayloadTooLarge(format!(
                "The file upload request exceeds the maximum allowed size of {} bytes.",
                state.configuration.file.max_bytes
            ))
            .into_response();
        }
        return ErrorResponse::PayloadTooLarge(format!(
            "The request body exceeds the maximum allowed size of {} bytes.",
            state.configuration.web.max_body_size
        ))
        .into_response();
    }

    response
}

// Request timeout(408) error handler.
// Scoped to /api/v1 routes only; WebSocket connections are long-lived and must not be timed out.
pub async fn request_timeout(
    State(state): State<Arc<ServerState>>,
    request: Request,
    next: Next,
) -> Response {
    if is_file_upload_request(&request) {
        return next.run(request).await;
    }
    let deadline = tokio::time::Duration::from_secs(state.configuration.web.request_timeout_secs);

    match tokio::time::timeout(deadline, next.run(request)).await {
        Ok(response) => response,
        Err(_) => ErrorResponse::RequestTimeout(format!(
            "The request exceeded the {} second timeout.",
            state.configuration.web.request_timeout_secs
        ))
        .into_response(),
    }
}

fn is_file_upload_request(request: &Request) -> bool {
    if request.method() != Method::POST {
        return false;
    }
    let segments: Vec<_> = request
        .uri()
        .path()
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect();
    matches!(
        segments.as_slice(),
        ["api", "v1", "chat", "rooms", _, "files"] | ["chat", "rooms", _, "files"]
    )
}

// Service unavailable(503) error handler — active during graceful shutdown.
pub async fn service_unavailable(
    State(state): State<Arc<ServerState>>,
    request: Request,
    next: Next,
) -> Response {
    if state.shutting_down.load(Ordering::SeqCst) {
        return ErrorResponse::ServiceUnavailable(
            "The server is shutting down. Please try again later.".to_string(),
        )
        .into_response();
    }

    next.run(request).await
}
