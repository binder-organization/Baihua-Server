use crate::ServerState;
use crate::common::StandardResponse;
use crate::common::error::ErrorResponse;
use axum::{extract::State, http::StatusCode};
use serde_json::json;
use std::sync::Arc;

pub async fn greet(
    State(state): State<Arc<ServerState>>,
) -> Result<StandardResponse, ErrorResponse> {
    sqlx::query("SELECT 1").execute(&state.pool).await?;

    Ok(StandardResponse::success(
        StatusCode::OK,
        state.configuration.greet.message.clone(),
        json!({
            "server_identity": "baihua",
            "server_version": env!("CARGO_PKG_VERSION"),
            "api_version": "v1",
            "uptime_seconds": state.started_at.elapsed().as_secs(),
            "health": {
                "status": "ok",
                "database": "reachable"
            }
        }),
    ))
}
