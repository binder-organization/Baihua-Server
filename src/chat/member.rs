use crate::ServerState;
use crate::common::StandardResponse;
use crate::common::error::ErrorResponse;
use crate::common::extractor::JsonBody;
use crate::middleware::authenticate::AuthenticatedUser;
use crate::user::find_user_by_username;
use axum::Extension;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::json;
use sqlx::Row;
use std::sync::Arc;
use uuid::Uuid;

use crate::chat::ROLE_MEMBER;
use crate::chat::{auto_promote_admin, get_member_count, is_room_admin, is_room_member};

#[derive(Debug, Deserialize)]
pub struct AddMembersRequest {
    pub usernames: Vec<String>,
}

// Add members to a group room. Only admins can add members.
pub async fn add_members(
    State(state): State<Arc<ServerState>>,
    Extension(auth_user): Extension<AuthenticatedUser>,
    Path(room_id): Path<Uuid>,
    JsonBody(request): JsonBody<AddMembersRequest>,
) -> Result<StandardResponse, ErrorResponse> {
    if request.usernames.is_empty() {
        return Err(ErrorResponse::BadRequest(
            "usernames list cannot be empty.".to_string(),
        ));
    }

    // Verify the requester is a room member.
    if !is_room_member(&state.pool, room_id, auth_user.user_id).await? {
        return Err(ErrorResponse::Forbidden(
            "You are not a member of this room.".to_string(),
        ));
    }

    // Only admins can add members in group rooms.
    if !is_room_admin(&state.pool, room_id, auth_user.user_id).await? {
        return Err(ErrorResponse::Forbidden(
            "Only room admins can add members.".to_string(),
        ));
    }

    // Resolve usernames to user IDs, check they exist and are not already members.
    let mut added = Vec::new();
    let now = Utc::now();

    for username in &request.usernames {
        let user =
            find_user_by_username(username, &state.pool)
                .await?
                .ok_or(ErrorResponse::NotFound(format!(
                    "User not found: {}",
                    username
                )))?;

        // Check if already a member.
        let already_member =
            sqlx::query("SELECT 1 FROM room_members WHERE room_id = $1 AND user_id = $2")
                .bind(room_id)
                .bind(user.id)
                .fetch_optional(&state.pool)
                .await?;

        if already_member.is_some() {
            continue; // Skip users who are already members.
        }

        sqlx::query(
            "INSERT INTO room_members (room_id, user_id, joined_at, role) VALUES ($1, $2, $3, $4)",
        )
        .bind(room_id)
        .bind(user.id)
        .bind(now)
        .bind(ROLE_MEMBER)
        .execute(&state.pool)
        .await?;

        added.push(json!({
            "user_id": user.id,
            "username": user.username,
            "joined_at": now.to_rfc3339(),
        }));
    }

    Ok(StandardResponse::success(
        StatusCode::OK,
        "Members added successfully.".to_string(),
        json!({
            "added": added,
            "added_count": added.len(),
        }),
    ))
}

// List all members of a room with their user info and roles.
pub async fn list_members(
    State(state): State<Arc<ServerState>>,
    Extension(auth_user): Extension<AuthenticatedUser>,
    Path(room_id): Path<Uuid>,
) -> Result<StandardResponse, ErrorResponse> {
    // Verify the requester is a room member.
    if !is_room_member(&state.pool, room_id, auth_user.user_id).await? {
        return Err(ErrorResponse::Forbidden(
            "You are not a member of this room.".to_string(),
        ));
    }

    let member_rows = sqlx::query(
        "SELECT rm.user_id, rm.role, rm.joined_at, u.username, u.nickname \
         FROM room_members rm \
         INNER JOIN users u ON rm.user_id = u.id \
         WHERE rm.room_id = $1 \
         ORDER BY rm.joined_at ASC",
    )
    .bind(room_id)
    .fetch_all(&state.pool)
    .await?;

    let members: Vec<serde_json::Value> = member_rows
        .iter()
        .map(|row| {
            json!({
                "user_id": row.get::<Uuid, _>("user_id"),
                "username": row.get::<String, _>("username"),
                "nickname": row.get::<Option<String>, _>("nickname"),
                "role": row.get::<String, _>("role"),
                "joined_at": row.get::<DateTime<Utc>, _>("joined_at").to_rfc3339(),
            })
        })
        .collect();

    Ok(StandardResponse::success(
        StatusCode::OK,
        "Members listed successfully.".to_string(),
        json!({
            "members": members,
            "count": members.len(),
        }),
    ))
}

// Remove a member from a room.
//
// If target_user_id == requesting user: self-leave (any member can do this).
// If target_user_id != requesting user: admin kick (admin only, group rooms only).
// Private room (is_group=false) members cannot be kicked by other members.
pub async fn remove_member(
    State(state): State<Arc<ServerState>>,
    Extension(auth_user): Extension<AuthenticatedUser>,
    Path((room_id, target_user_id)): Path<(Uuid, Uuid)>,
) -> Result<StandardResponse, ErrorResponse> {
    // Verify the requester is a room member.
    if !is_room_member(&state.pool, room_id, auth_user.user_id).await? {
        return Err(ErrorResponse::Forbidden(
            "You are not a member of this room.".to_string(),
        ));
    }

    let room_info = sqlx::query("SELECT is_group FROM rooms WHERE id = $1")
        .bind(room_id)
        .fetch_optional(&state.pool)
        .await?
        .ok_or(ErrorResponse::NotFound("Room not found.".to_string()))?;

    let is_group: bool = room_info.get("is_group");

    if target_user_id == auth_user.user_id {
        // Self-leave: any member can leave any room.
        handle_leave(&state, room_id, auth_user.user_id, is_group).await
    } else {
        // Admin kick: only for group rooms, only by admin.
        handle_kick(&state, room_id, auth_user.user_id, target_user_id, is_group).await
    }
}

// Handle self-leave. If last member, delete room. If last admin, auto-promote.
async fn handle_leave(
    state: &Arc<ServerState>,
    room_id: Uuid,
    user_id: Uuid,
    is_group: bool,
) -> Result<StandardResponse, ErrorResponse> {
    // Guard: verify the user is actually a member of this room.
    if !is_room_member(&state.pool, room_id, user_id).await? {
        return Err(ErrorResponse::Forbidden(
            "You are not a member of this room.".to_string(),
        ));
    }

    let member_count = get_member_count(&state.pool, room_id).await?;

    // If this is the last member, delete the room entirely (cascade handles messages + memberships).
    if member_count <= 1 {
        crate::chat::file::delete_room_with_files(state, room_id).await?;

        state
            .connection_manager
            .cancel_subscription(user_id, room_id);

        return Ok(StandardResponse::success(
            StatusCode::OK,
            "You left the room. The room has been deleted as you were the last member.".to_string(),
            json!({
                "room_id": room_id,
                "left_user_id": user_id,
                "room_deleted": true,
            }),
        ));
    }

    // For group rooms: if leaving user is an admin, try to auto-promote a successor before removing.
    if is_group && is_room_admin(&state.pool, room_id, user_id).await? {
        auto_promote_admin(state, room_id, user_id).await?;
    }

    // Remove the user from the room.
    sqlx::query("DELETE FROM room_members WHERE room_id = $1 AND user_id = $2")
        .bind(room_id)
        .bind(user_id)
        .execute(&state.pool)
        .await?;

    state
        .connection_manager
        .cancel_subscription(user_id, room_id);

    Ok(StandardResponse::success(
        StatusCode::OK,
        "You have left the room.".to_string(),
        json!({
            "room_id": room_id,
            "left_user_id": user_id,
            "room_deleted": false,
        }),
    ))
}

// Handle admin kicking a member from a group room.
async fn handle_kick(
    state: &Arc<ServerState>,
    room_id: Uuid,
    actor_id: Uuid,
    target_user_id: Uuid,
    is_group: bool,
) -> Result<StandardResponse, ErrorResponse> {
    // Kicking is only allowed in group rooms.
    if !is_group {
        return Err(ErrorResponse::Forbidden(
            "Cannot kick members from a private chat.".to_string(),
        ));
    }

    // Only admins can kick.
    if !is_room_admin(&state.pool, room_id, actor_id).await? {
        return Err(ErrorResponse::Forbidden(
            "Only room admins can remove members.".to_string(),
        ));
    }

    // Verify target is actually a member.
    if !is_room_member(&state.pool, room_id, target_user_id).await? {
        return Err(ErrorResponse::BadRequest(
            "Target user is not a member of this room.".to_string(),
        ));
    }

    // Check if target is also an admin.
    let target_is_admin = is_room_admin(&state.pool, room_id, target_user_id).await?;

    // Remove the target user.
    sqlx::query("DELETE FROM room_members WHERE room_id = $1 AND user_id = $2")
        .bind(room_id)
        .bind(target_user_id)
        .execute(&state.pool)
        .await?;

    state
        .connection_manager
        .cancel_subscription(target_user_id, room_id);

    // If the kicked user was an admin, auto-promote a successor.
    if target_is_admin {
        auto_promote_admin(state, room_id, target_user_id).await?;
    }

    Ok(StandardResponse::success(
        StatusCode::OK,
        "Member removed successfully.".to_string(),
        json!({
            "room_id": room_id,
            "removed_user_id": target_user_id,
        }),
    ))
}
