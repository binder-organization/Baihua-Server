use crate::ServerState;
use crate::authenticate::jsonwebtoken::generate_token;
use crate::common::StandardResponse;
use crate::common::error::ErrorResponse;
use crate::common::extractor::JsonBody;
use crate::user::{UserLogin, find_user_with_password, verify_password};
use axum::extract::State;
use axum::http::StatusCode;
use serde_json::json;
use std::sync::Arc;
use tracing::info;

pub async fn login(
    State(state): State<Arc<ServerState>>,
    JsonBody(user_login): JsonBody<UserLogin>,
) -> Result<StandardResponse, ErrorResponse> {
    let Some((user, password_hash)) =
        find_user_with_password(&user_login.username, &state.pool).await?
    else {
        return Err(ErrorResponse::Authentication(
            "Invalid username or password.".to_string(),
        ));
    };

    if !verify_password(user_login.password, password_hash).await? {
        return Err(ErrorResponse::Authentication(
            "Invalid username or password.".to_string(),
        ));
    }

    let token = generate_token(
        &state.jwt_secret,
        state.configuration.user.jsonwebtoken_expiration_hours,
        &user.id.to_string(),
        user.token_version,
    )
    .await?;

    if state.environment.is_production() {
        // Production environments should not expose JWT tokens.
        info!(
            "User logged in: {}, id: {}.",
            user_login.username,
            user.id.to_string()
        );
    } else if state.environment.is_development() {
        info!(
            "User logged in: {}, id: {}, token: {}.",
            user_login.username,
            user.id.to_string(),
            token
        );
    }

    Ok(StandardResponse::success(
        StatusCode::OK,
        "User logged in successfully.".to_string(),
        json!({
            "token": token,
            "user": user,
        }),
    ))
}
