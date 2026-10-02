pub(crate) mod encrypted;
mod member;
mod message;
mod request;
mod room;

use crate::ServerState;
use crate::common::error::ErrorResponse;
use axum::Router;
use axum::routing::{get, post};
use sqlx::PgPool;
use std::sync::Arc;
use tracing::warn;
use uuid::Uuid;

pub const ROLE_ADMIN: &str = "admin";
pub const ROLE_MEMBER: &str = "member";

// Sanitize user-provided text shared by WebSocket messages and room
// requests: strip control characters (keep newlines), trim, then check
// the length against the caller-provided limit.
pub(crate) fn validate_message_content(
    content: String,
    max_bytes: usize,
) -> Result<String, ErrorResponse> {
    let sanitized: String = content
        .chars()
        .filter(|char| !char.is_control() || *char == '\n')
        .collect();

    let trimmed = sanitized.trim().to_string();

    if trimmed.is_empty() {
        return Err(ErrorResponse::Validation(
            "Message content cannot be empty.".to_string(),
        ));
    }

    if trimmed.len() > max_bytes {
        return Err(ErrorResponse::Validation(format!(
            "Message content exceeds {} bytes.",
            max_bytes
        )));
    }

    Ok(trimmed)
}

// Check if a user is a member of a room.
pub async fn is_room_member(
    pool: &PgPool,
    room_id: Uuid,
    user_id: Uuid,
) -> Result<bool, ErrorResponse> {
    let row = sqlx::query("SELECT 1 FROM room_members WHERE room_id = $1 AND user_id = $2")
        .bind(room_id)
        .bind(user_id)
        .fetch_optional(pool)
        .await?;

    Ok(row.is_some())
}

// Check if a room exists.
pub async fn find_room_by_id(pool: &PgPool, room_id: Uuid) -> Result<Uuid, ErrorResponse> {
    let row = sqlx::query("SELECT 1 FROM rooms WHERE id = $1")
        .bind(room_id)
        .fetch_optional(pool)
        .await?;

    row.map(|_| room_id)
        .ok_or_else(|| ErrorResponse::NotFound("Room not found.".to_string()))
}

// Check if a user is an admin of a group room.
pub async fn is_room_admin(
    pool: &PgPool,
    room_id: Uuid,
    user_id: Uuid,
) -> Result<bool, ErrorResponse> {
    let row =
        sqlx::query("SELECT 1 FROM room_members WHERE room_id = $1 AND user_id = $2 AND role = $3")
            .bind(room_id)
            .bind(user_id)
            .bind(ROLE_ADMIN)
            .fetch_optional(pool)
            .await?;

    Ok(row.is_some())
}

// Count members in a room.
pub async fn get_member_count(pool: &PgPool, room_id: Uuid) -> Result<i64, ErrorResponse> {
    let row = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM room_members WHERE room_id = $1")
        .bind(room_id)
        .fetch_one(pool)
        .await?;

    Ok(row)
}

// When the last admin leaves or is removed, promote a successor.
//
// Priority order:
// 1. The room creator (created_by), if still a member and not the excluded user.
// 2. The oldest remaining member by joined_at (excluding the leaving user).
// 3. The oldest remaining member by joined_at (no exclusion — last-resort fallback).
// If the room has no members left, it is deleted.
pub async fn auto_promote_admin(
    pool: &PgPool,
    room_id: Uuid,
    excluding_user_id: Uuid,
) -> Result<(), ErrorResponse> {
    // Look up the room's creator (NULL once the creator deleted the account).
    let creator_id =
        sqlx::query_scalar::<_, Option<Uuid>>("SELECT created_by FROM rooms WHERE id = $1")
            .bind(room_id)
            .fetch_optional(pool)
            .await?;

    // Room does not exist.
    let Some(creator_option) = creator_id else {
        warn!(
            "Room {} does not exist, skipping admin promotion.",
            room_id
        );
        return Ok(());
    };

    // Room exists, but the creator has deleted their account (created_by is NULL).
    // Fall through to Priority 2.
    if let Some(creator) = creator_option
        && creator != excluding_user_id
    {
        // Priority 1: promote the room creator if eligible.
        let is_member =
            sqlx::query("SELECT 1 FROM room_members WHERE room_id = $1 AND user_id = $2")
                .bind(room_id)
                .bind(creator)
                .fetch_optional(pool)
                .await?;

        if is_member.is_some() {
            sqlx::query("UPDATE room_members SET role = $1 WHERE room_id = $2 AND user_id = $3")
                .bind(ROLE_ADMIN)
                .bind(room_id)
                .bind(creator)
                .execute(pool)
                .await?;

            return Ok(());
        }
    }

    // Priority 2: fall back to the oldest remaining member (excluding the leaving user).
    let successor = sqlx::query_scalar::<_, Uuid>(
        "SELECT user_id FROM room_members \
         WHERE room_id = $1 AND user_id != $2 \
         ORDER BY joined_at ASC LIMIT 1",
    )
    .bind(room_id)
    .bind(excluding_user_id)
    .fetch_optional(pool)
    .await?;

    if let Some(user_id) = successor {
        sqlx::query("UPDATE room_members SET role = $1 WHERE room_id = $2 AND user_id = $3")
            .bind(ROLE_ADMIN)
            .bind(room_id)
            .bind(user_id)
            .execute(pool)
            .await?;

        return Ok(());
    }

    // Priority 3: last-resort fallback — promote the oldest member without exclusion.
    let fallback = sqlx::query_scalar::<_, Uuid>(
        "SELECT user_id FROM room_members \
         WHERE room_id = $1 \
         ORDER BY joined_at ASC LIMIT 1",
    )
    .bind(room_id)
    .fetch_optional(pool)
    .await?;

    if let Some(user_id) = fallback {
        sqlx::query("UPDATE room_members SET role = $1 WHERE room_id = $2 AND user_id = $3")
            .bind(ROLE_ADMIN)
            .bind(room_id)
            .bind(user_id)
            .execute(pool)
            .await?;
    } else {
        // No members left — delete the room.
        warn!(
            "Room {} has no members left, deleting.",
            room_id
        );
        sqlx::query("DELETE FROM rooms WHERE id = $1")
            .bind(room_id)
            .execute(pool)
            .await?;
    }

    Ok(())
}

pub fn router(state: Arc<ServerState>) -> Router<Arc<ServerState>> {
    Router::new()
        .route(
            "/rooms",
            get(room::list_rooms).post(room::create_or_get_room),
        )
        .route("/rooms/requests", post(request::create_room_request))
        .route(
            "/rooms/requests/pending",
            get(request::list_pending_requests),
        )
        .route("/rooms/requests/sent", get(request::list_sent_requests))
        .route(
            "/rooms/requests/{request_id}/accept",
            post(request::accept_room_request),
        )
        .route(
            "/rooms/requests/{request_id}/decline",
            post(request::decline_room_request),
        )
        .route(
            "/rooms/requests/{request_id}/cancel",
            post(request::cancel_room_request),
        )
        .route("/rooms/{room_id}", get(room::get_room_detail))
        .route(
            "/rooms/{room_id}/members",
            get(member::list_members).post(member::add_members),
        )
        .route(
            "/rooms/{room_id}/members/{user_id}",
            axum::routing::delete(member::remove_member),
        )
        .route("/rooms/{room_id}/messages", get(message::get_messages))
        .route_layer(axum::middleware::from_fn_with_state(
            state,
            crate::middleware::authenticate::authenticate,
        ))
}
