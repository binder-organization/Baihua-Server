use crate::ServerState;
use crate::authenticate::jsonwebtoken::generate_token;
use crate::common::StandardResponse;
use crate::common::error::ErrorResponse;
use crate::common::extractor::JsonBody;
use crate::user::{LoginIdentifier, UserLogin, find_user_with_password, verify_password};
use axum::extract::State;
use axum::http::StatusCode;
use serde_json::json;
use std::sync::Arc;
use tracing::info;

pub async fn login(
    State(state): State<Arc<ServerState>>,
    JsonBody(user_login): JsonBody<UserLogin>,
) -> Result<StandardResponse, ErrorResponse> {
    let identifier = match (
        user_login
            .username
            .as_deref()
            .filter(|value| !value.is_empty()),
        user_login
            .email
            .as_deref()
            .filter(|value| !value.is_empty()),
    ) {
        (Some(username), None) => LoginIdentifier::Username(username),
        (None, Some(email)) => LoginIdentifier::Email(email),
        (None, None) => {
            return Err(ErrorResponse::Validation(
                "username or email is required.".to_string(),
            ));
        }
        (Some(_), Some(_)) => {
            return Err(ErrorResponse::Validation(
                "Provide either username or email.".to_string(),
            ));
        }
    };
    let Some((user, password_hash)) = find_user_with_password(identifier, &state.pool).await?
    else {
        return Err(ErrorResponse::Authentication(
            "Invalid login credentials.".to_string(),
        ));
    };

    if !verify_password(user_login.password, password_hash).await? {
        return Err(ErrorResponse::Authentication(
            "Invalid login credentials.".to_string(),
        ));
    }

    if user.email_verified_at.is_none() {
        return Err(ErrorResponse::Authentication(
            "Email address is not verified.".to_string(),
        ));
    }

    let token = generate_token(
        &state.jwt_secret,
        state.configuration.user.jsonwebtoken_expiration_hours,
        &user.id.to_string(),
        user.token_version,
    )
    .await?;

    info!(
        "User logged in: {}, id: {}.",
        user.username,
        user.id.to_string()
    );

    Ok(StandardResponse::success(
        StatusCode::OK,
        "User logged in successfully.".to_string(),
        json!({
            "token": token,
            "user": user,
        }),
    ))
}
