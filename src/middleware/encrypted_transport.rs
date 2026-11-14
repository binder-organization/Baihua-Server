use crate::ServerState;
use crate::common::error::ErrorResponse;
use axum::extract::{Request, State};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use std::sync::Arc;

pub async fn enforce_encrypted_transport(
    State(state): State<Arc<ServerState>>,
    request: Request,
    next: Next,
) -> Response {
    if !state.environment.is_production()
        || !state.configuration.web.require_encrypted_transport
        || request.uri().path() == "/health"
        || request
            .headers()
            .get("x-forwarded-proto")
            .is_some_and(|protocol| protocol == "https")
    {
        return next.run(request).await;
    }

    ErrorResponse::BadRequest("An encrypted client connection is required.".to_string())
        .into_response()
}
