use crate::ServerState;
use crate::chat::{find_room_by_id, is_room_member};
use crate::common::StandardResponse;
use crate::common::error::ErrorResponse;
use crate::middleware::authenticate::AuthenticatedUser;
use axum::Extension;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::json;
use sqlx::Row;
use std::sync::Arc;
use uuid::Uuid;

#[derive(Debug, Deserialize)]
pub struct GetMessagesQuery {
    pub limit: Option<i64>,
    pub before: Option<Uuid>,
}

pub async fn get_messages(
    State(state): State<Arc<ServerState>>,
    Extension(auth_user): Extension<AuthenticatedUser>,
    Path(room_id): Path<Uuid>,
    Query(params): Query<GetMessagesQuery>,
) -> Result<StandardResponse, ErrorResponse> {
    let limit = params.limit.unwrap_or(50).min(100);
    if limit < 1 {
        return Err(ErrorResponse::Validation(
            "Message page limit must be at least 1.".to_string(),
        ));
    }

    find_room_by_id(&state.pool, room_id).await?;

    if !is_room_member(&state.pool, room_id, auth_user.user_id).await? {
        return Err(ErrorResponse::Forbidden(
            "You are not a member of this room.".to_string(),
        ));
    }

    let is_encrypted: bool = sqlx::query_scalar("SELECT is_encrypted FROM rooms WHERE id = $1")
        .bind(room_id)
        .fetch_one(&state.pool)
        .await?;

    // Keyset pagination: use (created_at, id) composite to guarantee deterministic ordering
    // even when two messages share the same created_at timestamp.
    // For encrypted rooms, select encrypted_content as base64 instead of plaintext content.
    // We alias the result to `content` so both branches use the same column name.
    let content_expr = if is_encrypted {
        "encode(encrypted_content, 'base64') AS content"
    } else {
        "content"
    };

    let query = format!(
        "SELECT messages.id, messages.room_id, messages.sender_id, {content_expr}, messages.created_at, file_attachments.content_hash, file_attachments.original_name, file_attachments.media_type, file_attachments.byte_size, file_attachments.encrypted, file_attachments.encrypted_metadata FROM messages LEFT JOIN file_attachments ON file_attachments.message_id = messages.id"
    );

    let rows = if let Some(before_id) = params.before {
        let q = format!(
            "{query} \
             WHERE room_id = $1 \
               AND (messages.created_at, messages.id) < (SELECT created_at, id FROM messages WHERE id = $2 AND room_id = $1) \
             ORDER BY messages.created_at DESC, messages.id DESC LIMIT $3",
        );
        sqlx::query(&q)
            .bind(room_id)
            .bind(before_id)
            .bind(limit + 1)
            .fetch_all(&state.pool)
            .await
    } else {
        let q = format!(
            "{query} \
             WHERE room_id = $1 \
             ORDER BY messages.created_at DESC, messages.id DESC LIMIT $2",
        );
        sqlx::query(&q)
            .bind(room_id)
            .bind(limit + 1)
            .fetch_all(&state.pool)
            .await
    }?;

    let has_more = rows.len() > limit as usize;
    let visible = rows.iter().take(limit as usize);

    let messages: Vec<serde_json::Value> = visible
        .map(|row| {
            let content_key = if is_encrypted {
                "ciphertext"
            } else {
                "content"
            };
            let file = row.get::<Option<String>, _>("content_hash").map(|hash| json!({
                "sha256": hash.trim(),
                "name": row.get::<String, _>("original_name"),
                "media_type": row.get::<String, _>("media_type"),
                "byte_size": row.get::<i64, _>("byte_size"),
                "encrypted": row.get::<bool, _>("encrypted"),
                "encrypted_metadata": row.get::<Option<String>, _>("encrypted_metadata"),
                "download_url": format!("/api/v1/chat/rooms/{room_id}/files/{}", row.get::<Uuid, _>("id")),
            }));
            json!({
                "id": row.get::<Uuid, _>("id"),
                "room_id": row.get::<Uuid, _>("room_id"),
                "sender_id": row.get::<Option<Uuid>, _>("sender_id"),
                content_key: row.get::<Option<String>, _>("content"),
                "file": file,
                "created_at": row.get::<DateTime<Utc>, _>("created_at").to_rfc3339(),
            })
        })
        .collect();

    let next_cursor = if has_more {
        let last_idx = (limit as usize).saturating_sub(1);
        rows.get(last_idx).map(|row| row.get::<Uuid, _>("id"))
    } else {
        None
    };

    Ok(StandardResponse::success(
        StatusCode::OK,
        "Messages retrieved successfully.".to_string(),
        json!({
            "messages": messages,
            "has_more": has_more,
            "next_cursor": next_cursor,
        }),
    ))
}
