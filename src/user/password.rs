use crate::ServerState;
use crate::common::StandardResponse;
use crate::common::error::ErrorResponse;
use crate::common::extractor::JsonBody;
use crate::middleware::authenticate::AuthenticatedUser;
use crate::user::{hash_password, verify_password};
use axum::Extension;
use axum::extract::State;
use axum::http::StatusCode;
use serde::Deserialize;
use serde_json::json;
use sqlx::Row;
use std::sync::Arc;
use tracing::info;

#[derive(Debug, Deserialize)]
pub struct ChangePasswordRequest {
    pub old_password: String,
    pub new_password: String,
}

// Change the caller's password. The old password must be verified first so
// that a leaked token alone is not enough to take over the account.
pub async fn change_password(
    State(state): State<Arc<ServerState>>,
    Extension(auth_user): Extension<AuthenticatedUser>,
    JsonBody(request): JsonBody<ChangePasswordRequest>,
) -> Result<StandardResponse, ErrorResponse> {
    if request.old_password.is_empty() || request.new_password.is_empty() {
        return Err(ErrorResponse::Validation(
            "Old password and new password cannot be empty.".to_string(),
        ));
    }

    let stored_hash = sqlx::query("SELECT password FROM users WHERE id = $1")
        .bind(auth_user.user_id)
        .fetch_optional(&state.pool)
        .await?
        .ok_or(ErrorResponse::NotFound("User not found.".to_string()))?
        .get::<String, _>("password");

    if !verify_password(request.old_password, stored_hash).await? {
        return Err(ErrorResponse::Authentication(
            "Old password is incorrect.".to_string(),
        ));
    }

    let new_hash =
        hash_password(request.new_password, state.configuration.user.bcrypt_cost).await?;

    // Bump token_version so every previously issued JWT (other devices
    // included) stops passing the version check.
    sqlx::query("UPDATE users SET password = $1, token_version = token_version + 1 WHERE id = $2")
        .bind(new_hash)
        .bind(auth_user.user_id)
        .execute(&state.pool)
        .await?;

    info!("User {} changed its password.", auth_user.user_id);

    Ok(StandardResponse::success(
        StatusCode::OK,
        "Password changed successfully.".to_string(),
        json!(null),
    ))
}
