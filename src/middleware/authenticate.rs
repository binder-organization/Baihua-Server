use crate::ServerState;
use crate::authenticate::jsonwebtoken::{extract_token_from_header, validate_token};
use crate::common::error::ErrorResponse;
use crate::user::find_user_by_id;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::{extract::Request, middleware::Next, response::Response};
use std::sync::Arc;
use tracing::error;
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct AuthenticatedUser {
    pub user_id: Uuid,
    #[allow(dead_code)]
    pub access_token: String,
}

pub async fn authenticate(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    request: Request,
    next: Next,
) -> Result<Response, ErrorResponse> {
    let authenticate_header = headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .ok_or(ErrorResponse::Authentication(
            "Missing authorization header.".to_string(),
        ))?;

    let token = extract_token_from_header(authenticate_header)?;

    let claims = validate_token(token, &state.jwt_secret)?;

    let user_id = Uuid::parse_str(&claims.sub).map_err(|_| {
        error!("JWT sub claim is not a valid UUID: '{}'.", claims.sub);
        ErrorResponse::Authentication("Invalid JsonWebToken.".to_string())
    })?;

    let user = find_user_by_id(user_id, &state.pool).await?;
    match &user {
        Some(user) if user.is_active && user.email_verified_at.is_some() => {}
        _ => {
            return Err(ErrorResponse::Authentication(
                "User not found or inactive.".to_string(),
            ));
        }
    }

    // The token carries the version in effect when it was issued; a logout
    // or password change bumps the stored version, revoking every old token.
    if let Some(user) = user
        && claims.token_version != user.token_version
    {
        return Err(ErrorResponse::Authentication(
            "Session expired. Please log in again.".to_string(),
        ));
    }

    let mut request = request;
    request.extensions_mut().insert(AuthenticatedUser {
        user_id,
        access_token: token.to_string(),
    });

    Ok(next.run(request).await)
}
