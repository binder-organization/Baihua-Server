use crate::ServerState;
use crate::common::error::ErrorResponse;
use axum::body::Body as HttpBody;
use axum::body::Bytes;
use axum::extract::State;
use axum::http;
use axum::http::{HeaderMap, Request};
use axum::{middleware::Next, response::Response};
use serde_json::Value;
use std::sync::Arc;

async fn validate_json_body(
    max_body_size: usize,
    headers: &HeaderMap,
    request: Request<HttpBody>,
) -> Result<(http::request::Parts, Bytes, Value), ErrorResponse> {
    let content_type = headers
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("");

    if !content_type.starts_with("application/json") {
        return Err(ErrorResponse::Validation(
            "Content-Type must be 'application/json'.".to_string(),
        ));
    }

    let (parts, body) = request.into_parts();

    let body_bytes = axum::body::to_bytes(body, max_body_size)
        .await
        .map_err(|_| {
            ErrorResponse::PayloadTooLarge(format!(
                "Request body too large (max {} bytes).",
                max_body_size
            ))
        })?;

    let json_value: Value = serde_json::from_slice(&body_bytes)
        .map_err(|error| ErrorResponse::Json(format!("Invalid JSON format: {}.", error)))?;

    Ok((parts, body_bytes, json_value))
}

pub async fn validate_register(
    state: State<Arc<ServerState>>,
    headers: HeaderMap,
    request: Request<HttpBody>,
    next: Next,
) -> Result<Response, ErrorResponse> {
    let max_body_size = state.configuration.web.max_body_size as usize;
    let (parts, body_bytes, json_value) =
        validate_json_body(max_body_size, &headers, request).await?;

    json_value
        .get("username")
        .and_then(|value| value.as_str())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| ErrorResponse::Validation("username is required.".to_string()))?;

    json_value
        .get("email")
        .and_then(|value| value.as_str())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| ErrorResponse::Validation("email is required.".to_string()))?;

    json_value
        .get("password")
        .and_then(|value| value.as_str())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| ErrorResponse::Validation("password is required.".to_string()))?;

    let request = Request::from_parts(parts, HttpBody::from(body_bytes));
    Ok(next.run(request).await)
}

pub async fn validate_login(
    state: State<Arc<ServerState>>,
    headers: HeaderMap,
    request: Request<HttpBody>,
    next: Next,
) -> Result<Response, ErrorResponse> {
    let max_body_size = state.configuration.web.max_body_size as usize;
    let (parts, body_bytes, json_value) =
        validate_json_body(max_body_size, &headers, request).await?;

    let username = json_value
        .get("username")
        .and_then(|value| value.as_str())
        .filter(|value| !value.is_empty());
    let email = json_value
        .get("email")
        .and_then(|value| value.as_str())
        .filter(|value| !value.is_empty());
    match (username, email) {
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
        _ => {}
    }

    json_value
        .get("password")
        .and_then(|value| value.as_str())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| ErrorResponse::Validation("password is required.".to_string()))?;

    let request = Request::from_parts(parts, HttpBody::from(body_bytes));
    Ok(next.run(request).await)
}
