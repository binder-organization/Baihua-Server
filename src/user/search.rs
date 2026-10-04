use crate::ServerState;
use crate::common::StandardResponse;
use crate::common::error::ErrorResponse;
use crate::middleware::authenticate::AuthenticatedUser;
use axum::Extension;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use serde::Deserialize;
use serde_json::json;
use sqlx::Row;
use std::sync::Arc;
use uuid::Uuid;

#[derive(Debug, Deserialize)]
pub struct SearchUsersQuery {
    pub username: Option<String>,
    pub user_id: Option<Uuid>,
    #[serde(default)]
    pub limit: Option<u32>,
    #[serde(default)]
    pub offset: Option<u32>,
}

// Search the active user directory by username (substring, case
// insensitive) or exact user_id. Only public profile fields are exposed;
// email and phone number are never returned.
pub async fn search_users(
    State(state): State<Arc<ServerState>>,
    Extension(_auth_user): Extension<AuthenticatedUser>,
    Query(params): Query<SearchUsersQuery>,
) -> Result<StandardResponse, ErrorResponse> {
    if params.username.is_none() && params.user_id.is_none() {
        return Err(ErrorResponse::Validation(
            "Either username or user_id is required.".to_string(),
        ));
    }

    // Cap the page size at 50 rows per request.
    let limit = params.limit.unwrap_or(20).min(50);
    if limit == 0 {
        return Err(ErrorResponse::Validation(
            "Search page limit must be at least 1.".to_string(),
        ));
    }
    let offset = params.offset.unwrap_or(0);

    let (rows, count) = match (&params.username, params.user_id) {
        (Some(username), _) => {
            let rows = sqlx::query(
                "SELECT id, username, nickname, bio, avatar FROM users \
                 WHERE is_active = true AND username ILIKE '%' || $1 || '%' \
                 ORDER BY created_at DESC LIMIT $2 OFFSET $3",
            )
            .bind(username)
            .bind(limit as i64)
            .bind(offset as i64)
            .fetch_all(&state.pool)
            .await?;

            let count: i64 = sqlx::query_scalar(
                "SELECT count(*)::bigint FROM users \
                 WHERE is_active = true AND username ILIKE '%' || $1 || '%'",
            )
            .bind(username)
            .fetch_one(&state.pool)
            .await?;

            (rows, count)
        }
        (None, Some(user_id)) => {
            let rows = sqlx::query(
                "SELECT id, username, nickname, bio, avatar FROM users \
                 WHERE is_active = true AND id = $1 \
                 ORDER BY created_at DESC LIMIT $2 OFFSET $3",
            )
            .bind(user_id)
            .bind(limit as i64)
            .bind(offset as i64)
            .fetch_all(&state.pool)
            .await?;

            let count: i64 = sqlx::query_scalar(
                "SELECT count(*)::bigint FROM users \
                 WHERE is_active = true AND id = $1",
            )
            .bind(user_id)
            .fetch_one(&state.pool)
            .await?;

            (rows, count)
        }
        // Guarded by the filter validation above.
        (None, None) => unreachable!("search filter validation runs before the query"),
    };

    let users: Vec<serde_json::Value> = rows
        .iter()
        .map(|row| {
            json!({
                "id": row.get::<Uuid, _>("id"),
                "username": row.get::<String, _>("username"),
                "nickname": row.get::<Option<String>, _>("nickname"),
                "bio": row.get::<Option<String>, _>("bio"),
                "avatar": row.get::<Option<String>, _>("avatar"),
            })
        })
        .collect();

    Ok(StandardResponse::success(
        StatusCode::OK,
        "Users searched successfully.".to_string(),
        json!({
            "users": users,
            "count": count,
            "limit": limit,
            "offset": offset,
        }),
    ))
}
