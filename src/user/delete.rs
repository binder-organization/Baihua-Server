use crate::ServerState;
use crate::common::StandardResponse;
use crate::common::error::ErrorResponse;
use crate::common::extractor::JsonBody;
use crate::middleware::authenticate::AuthenticatedUser;
use crate::user::verify_password;
use axum::Extension;
use axum::extract::State;
use axum::http::StatusCode;
use serde::Deserialize;
use serde_json::json;
use sqlx::Row;
use std::sync::Arc;
use tracing::{info, warn};

#[derive(Debug, Deserialize)]
pub struct DeleteAccountRequest {
    pub password: String,
}

// Delete the caller's account after re-verifying the password. Memberships
// and room requests cascade away; messages and rooms keep their rows with
// sender_id/created_by set to NULL.
pub async fn delete_account(
    State(state): State<Arc<ServerState>>,
    Extension(auth_user): Extension<AuthenticatedUser>,
    JsonBody(request): JsonBody<DeleteAccountRequest>,
) -> Result<StandardResponse, ErrorResponse> {
    if request.password.is_empty() {
        return Err(ErrorResponse::Validation(
            "Password cannot be empty.".to_string(),
        ));
    }

    let stored_hash = sqlx::query("SELECT password FROM users WHERE id = $1")
        .bind(auth_user.user_id)
        .fetch_optional(&state.pool)
        .await?
        .ok_or(ErrorResponse::NotFound("User not found.".to_string()))?
        .get::<String, _>("password");

    if !verify_password(request.password, stored_hash).await? {
        return Err(ErrorResponse::Authentication(
            "Password is incorrect.".to_string(),
        ));
    }

    // Capture a previously uploaded avatar file so it can be removed from
    // disk after the account row is gone; a failed removal only warns.
    let previous_avatar = sqlx::query("SELECT avatar FROM users WHERE id = $1")
        .bind(auth_user.user_id)
        .fetch_optional(&state.pool)
        .await?
        .and_then(|row| row.get::<Option<String>, _>("avatar"));

    let old_filename = previous_avatar
        .as_deref()
        .and_then(|avatar| avatar.strip_prefix("/static/avatars/"))
        .map(str::to_string);

    sqlx::query("DELETE FROM users WHERE id = $1")
        .bind(auth_user.user_id)
        .execute(&state.pool)
        .await?;
    if let Some(old_filename) = old_filename
        && let Err(err) = tokio::fs::remove_file(state.avatars_directory.join(&old_filename)).await
    {
        warn!(
            "Failed to delete the avatar file '{}' of the deleted user: {}.",
            old_filename, err
        );
    }

    info!("User {} deleted its account.", auth_user.user_id);

    Ok(StandardResponse::success(
        StatusCode::OK,
        "Account deleted successfully.".to_string(),
        json!(null),
    ))
}
