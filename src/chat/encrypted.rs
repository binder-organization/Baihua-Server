use crate::ServerState;
use crate::common::error::ErrorResponse;
use crate::user::User;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use chrono::Utc;
use serde_json::json;
use sqlx::PgPool;
use sqlx::Row;
use std::sync::Arc;
use std::time::Duration;
use tracing::{error, info};
use uuid::Uuid;

// Public helpers

// Query all room IDs where is_encrypted=true and the user is a member.
pub async fn encrypted_rooms_for_user(
    pool: &PgPool,
    user_id: Uuid,
) -> Result<Vec<Uuid>, ErrorResponse> {
    let rows = sqlx::query_scalar::<_, Uuid>(
        "SELECT r.id FROM rooms r \
         INNER JOIN room_members rm ON r.id = rm.room_id \
         WHERE r.is_encrypted = true AND rm.user_id = $1",
    )
    .bind(user_id)
    .fetch_all(pool)
    .await?;

    Ok(rows)
}

// Handlers (called from WebSocket dispatch)

// Phase 1: User A initiates an encrypted session.
pub(crate) async fn handle_encrypt_request(
    state: &Arc<ServerState>,
    user: &User,
    room_id: Uuid,
    public_key: String,
    identity_key: String,
    signature: String,
) -> Result<Option<String>, ErrorResponse> {
    // Validate room exists and is a private room (not group).
    let room = sqlx::query("SELECT is_group, is_encrypted, created_by FROM rooms WHERE id = $1")
        .bind(room_id)
        .fetch_optional(&state.pool)
        .await?
        .ok_or(ErrorResponse::NotFound("Room not found.".to_string()))?;

    // Check room is not already in an active encrypted session.
    if state.connection_manager.is_session_active(room_id) {
        return Err(ErrorResponse::Conflict(
            "Room already has an active encrypted session.".to_string(),
        ));
    }

    // todo Add this feature to the group.
    if room.get("is_group") {
        return Err(ErrorResponse::BadRequest(
            "Encrypted chat is only supported in private rooms.".to_string(),
        ));
    }

    // Validate user is a member.
    if !crate::chat::is_room_member(&state.pool, room_id, user.id).await? {
        return Err(ErrorResponse::Forbidden(
            "You are not a member of this room.".to_string(),
        ));
    }

    // Find the other member (partner).
    let partner_id = sqlx::query_scalar::<_, Uuid>(
        "SELECT user_id FROM room_members WHERE room_id = $1 AND user_id != $2 LIMIT 1",
    )
    .bind(room_id)
    .bind(user.id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or(ErrorResponse::InternalError(
        "Room has no other member.".to_string(),
    ))?;

    // Check partner is online.
    if !state.connection_manager.is_user_online(partner_id) {
        return Err(ErrorResponse::Conflict(
            "Both users must be online to start an encrypted session.".to_string(),
        ));
    }

    // Mark the room as encrypted.
    sqlx::query("UPDATE rooms SET is_encrypted = true WHERE id = $1")
        .bind(room_id)
        .execute(&state.pool)
        .await?;

    // Mark room as pending (awaiting encrypt_accept).
    state.connection_manager.mark_pending(room_id);

    // Forward invitation to the room.
    let invitation = json!({
        "type": "encrypt_invitation",
        "data": {
            "room_id": room_id,
            "inviter_id": user.id,
            "inviter_username": user.username,
            "public_key": public_key,
            "identity_key": identity_key,
            "signature": signature,
        }
    })
    .to_string();
    state.connection_manager.broadcast(room_id, &invitation);

    Ok(None)
}

// Phase 2: User B accepts and sends their public key.
pub(crate) async fn handle_encrypt_accept(
    state: &Arc<ServerState>,
    user: &User,
    room_id: Uuid,
    public_key: String,
    identity_key: String,
    signature: String,
) -> Result<Option<String>, ErrorResponse> {
    // Validate room exists and is encrypted.
    let is_encrypted: bool = sqlx::query_scalar("SELECT is_encrypted FROM rooms WHERE id = $1")
        .bind(room_id)
        .fetch_optional(&state.pool)
        .await? // if_let when I want the bool
        .unwrap_or(false);

    if !is_encrypted {
        return Err(ErrorResponse::Conflict(
            "Room is not marked for encrypted chat.".to_string(),
        ));
    }

    // Validate user is a member.
    if !crate::chat::is_room_member(&state.pool, room_id, user.id).await? {
        return Err(ErrorResponse::Forbidden(
            "You are not a member of this room.".to_string(),
        ));
    }

    // Check session is not already active.
    if state.connection_manager.is_session_active(room_id) {
        return Err(ErrorResponse::Conflict(
            "Session is already active.".to_string(),
        ));
    }

    // Check there is a pending encrypt_request for this room.
    if !state.connection_manager.is_pending(room_id) {
        return Err(ErrorResponse::Conflict(
            "No pending encrypt request for this room.".to_string(),
        ));
    }

    // Clear pending state.
    state.connection_manager.clear_pending(room_id);

    // Forward acceptance to the room.
    let accept_msg = json!({
        "type": "encrypt_accept_response",
        "data": {
            "room_id": room_id,
            "acceptor_id": user.id,
            "public_key": public_key,
            "identity_key": identity_key,
            "signature": signature,
        }
    })
    .to_string();
    state.connection_manager.broadcast(room_id, &accept_msg);

    Ok(None)
}

// Phase 3: Each side confirms ready. Activate when both have signaled.
pub(crate) async fn handle_encrypt_ready(
    state: &Arc<ServerState>,
    user: &User,
    room_id: Uuid,
) -> Result<Option<String>, ErrorResponse> {
    // Validate room exists.
    let is_encrypted: bool = sqlx::query_scalar("SELECT is_encrypted FROM rooms WHERE id = $1")
        .bind(room_id)
        .fetch_optional(&state.pool)
        .await?
        .unwrap_or(false);

    if !is_encrypted {
        return Err(ErrorResponse::Conflict(
            "Room is not marked for encrypted chat.".to_string(),
        ));
    }

    if !crate::chat::is_room_member(&state.pool, room_id, user.id).await? {
        return Err(ErrorResponse::Forbidden(
            "You are not a member of this room.".to_string(),
        ));
    }

    if state.connection_manager.is_session_active(room_id) {
        return Err(ErrorResponse::Conflict(
            "Session is already active.".to_string(),
        ));
    }

    // Determine member index (0 = first to ready, 1 = second).
    // Query existing ready state to decide index.
    let members: Vec<(Uuid,)> = sqlx::query_as(
        "SELECT user_id FROM room_members WHERE room_id = $1 ORDER BY joined_at ASC, user_id ASC",
    )
    .bind(room_id)
    .fetch_all(&state.pool)
    .await?;

    let member_index =
        members
            .iter()
            .position(|(uid,)| *uid == user.id)
            .ok_or(ErrorResponse::Forbidden(
                "You are not a member of this room.".to_string(),
            ))?;

    // Mark this side as ready. Returns true when both are ready.
    let both_ready = state.connection_manager.set_ready(room_id, member_index);

    if both_ready {
        state.connection_manager.set_session_active(room_id);
        let ready_msg = json!({
            "type": "encrypt_session_ready",
            "data": {
                "room_id": room_id,
            }
        })
        .to_string();
        state.connection_manager.broadcast(room_id, &ready_msg);
    }

    Ok(None)
}

// Phase 4: Send an encrypted message.
pub(crate) async fn handle_encrypt_message(
    state: &Arc<ServerState>,
    user: &User,
    room_id: Uuid,
    ciphertext: String,
) -> Result<Option<String>, ErrorResponse> {
    // Must be in an active encrypted session.
    if !state.connection_manager.is_session_active(room_id) {
        return Err(ErrorResponse::Conflict(
            "No active encrypted session in this room.".to_string(),
        ));
    }

    // Validate membership.
    if !crate::chat::is_room_member(&state.pool, room_id, user.id).await? {
        return Err(ErrorResponse::Forbidden(
            "You are not a member of this room.".to_string(),
        ));
    }

    // Validate ciphertext is non-empty and within size limits.
    if ciphertext.is_empty() {
        return Err(ErrorResponse::Validation(
            "Ciphertext cannot be empty.".to_string(),
        ));
    }
    // Max 64KB on the wire (more than enough for any realistic message).
    if ciphertext.len() > 65536 {
        return Err(ErrorResponse::Validation(
            "Ciphertext exceeds maximum size.".to_string(),
        ));
    }

    // Decode base64. The decoded content is binary; we validate it
    // decodes correctly but never inspect the plaintext.
    let encrypted_bytes = BASE64.decode(ciphertext.as_bytes()).map_err(|error| {
        ErrorResponse::Validation(format!("Invalid base64 ciphertext: {}", error))
    })?;

    let message_id = Uuid::now_v7();
    let now = Utc::now();

    sqlx::query(
        "INSERT INTO messages (id, room_id, sender_id, content, encrypted_content, created_at) \
         VALUES ($1, $2, $3, NULL, $4, $5)",
    )
    .bind(message_id)
    .bind(room_id)
    .bind(user.id)
    .bind(&encrypted_bytes)
    .bind(now)
    .execute(&state.pool)
    .await?;

    // Broadcast ciphertext to the room (relay only).
    let ws_msg = json!({
        "type": "new_encrypted_message",
        "data": {
            "id": message_id,
            "room_id": room_id,
            "sender_id": user.id,
            "ciphertext": ciphertext,
            "created_at": now.to_rfc3339(),
        }
    })
    .to_string();
    state.connection_manager.broadcast(room_id, &ws_msg);

    // Ack to sender.
    let ack = json!({
        "type": "encrypted_message_sent",
        "data": {
            "id": message_id,
            "room_id": room_id,
        }
    })
    .to_string();

    Ok(Some(ack))
}

// Phase 5a: User voluntarily leaves the encrypted session.
pub(crate) async fn handle_encrypt_leave(
    state: &Arc<ServerState>,
    user: &User,
    room_id: Uuid,
    pool: &PgPool,
) -> Result<Option<String>, ErrorResponse> {
    if !state.connection_manager.is_session_active(room_id) {
        return Err(ErrorResponse::Conflict(
            "No active encrypted session in this room.".to_string(),
        ));
    }

    if !crate::chat::is_room_member(&state.pool, room_id, user.id).await? {
        return Err(ErrorResponse::Forbidden(
            "You are not a member of this room.".to_string(),
        ));
    }

    cleanup_encrypted_room(state, room_id, Some(user.id), None, pool).await;
    Ok(None)
}

// Lifecycle management

// Called when a user fully disconnects: start grace periods for all
// active encrypted rooms that user is a member of.
pub(crate) async fn start_grace_periods_for_user(
    state: &Arc<ServerState>,
    user_id: Uuid,
    pool: &PgPool,
) {
    let encrypted_rooms = match encrypted_rooms_for_user(pool, user_id).await {
        Ok(ids) => ids,
        Err(error) => {
            error!(
                "Failed to get encrypted rooms for user {}: {}",
                user_id, error
            );
            return;
        }
    };

    for room_id in encrypted_rooms {
        if !state.connection_manager.is_session_active(room_id) {
            continue;
        }

        let grace_period =
            Duration::from_secs(state.configuration.websocket.encrypted_grace_period_secs);
        state
            .connection_manager
            .start_grace_period(room_id, user_id, grace_period);

        let grace_until = (Utc::now() + grace_period).to_rfc3339();
        let msg = json!({
            "type": "encrypt_partner_disconnected",
            "data": {
                "room_id": room_id,
                "offline_user_id": user_id,
                "grace_until": grace_until,
            }
        })
        .to_string();
        state.connection_manager.broadcast(room_id, &msg);

        let state_clone = state.clone();
        let pool_clone = pool.clone();
        tokio::spawn(async move {
            grace_period_waiter(state_clone, room_id, user_id, pool_clone).await;
        });
    }
}

// Delay cleanup until the configured reconnect grace period has passed.
async fn grace_period_waiter(
    state: Arc<ServerState>,
    room_id: Uuid,
    offline_user_id: Uuid,
    pool: PgPool,
) {
    tokio::time::sleep(Duration::from_secs(
        state.configuration.websocket.encrypted_grace_period_secs,
    ))
    .await;

    // Check if the grace period is still active for this user+room.
    // If the user reconnected, cancel_grace_periods_for_user would have
    // removed the entry, so this returns None and we do nothing.
    let should_cleanup = match state.connection_manager.get_grace_period(room_id) {
        Some((uid, _deadline)) => uid == offline_user_id,
        None => false,
    };

    if should_cleanup {
        cleanup_encrypted_room(&state, room_id, None, Some(offline_user_id), &pool).await;
    }
}

// Delete all encrypted messages, reset room state, notify participants.
pub(crate) async fn cleanup_encrypted_room(
    state: &Arc<ServerState>,
    room_id: Uuid,
    terminated_by: Option<Uuid>,
    offline_user_id: Option<Uuid>,
    pool: &PgPool,
) {
    // Delete all messages (both encrypted and plaintext — room is being reset).
    if let Err(error) = sqlx::query("DELETE FROM messages WHERE room_id = $1")
        .bind(room_id)
        .execute(pool)
        .await
    {
        error!("Failed to delete messages for room {}: {}", room_id, error);
    }

    // Reset room encryption status.
    if let Err(error) = sqlx::query("UPDATE rooms SET is_encrypted = false WHERE id = $1")
        .bind(room_id)
        .execute(pool)
        .await
    {
        error!(
            "Failed to reset room encryption status for {}: {}",
            room_id, error
        );
    }

    // Clear in-memory state.
    state.connection_manager.remove_session(room_id);
    state.connection_manager.clear_pending(room_id);
    state.connection_manager.remove_ready_state(room_id);
    state.connection_manager.cancel_grace_period(room_id);

    // Notify participants.
    let reason = if terminated_by.is_some() {
        "user_left"
    } else {
        "partner_timeout"
    };

    let offline_id = terminated_by.or(offline_user_id);

    let end_msg = json!({
        "type": "encrypt_session_ended",
        "data": {
            "room_id": room_id,
            "reason": reason,
            "offline_user_id": offline_id,
        }
    })
    .to_string();
    state.connection_manager.broadcast(room_id, &end_msg);

    info!(
        "Encrypted session ended for room {} (reason: {}, offline_user: {:?})",
        room_id, reason, offline_id
    );
}

// On connect: check for expired encrypted sessions and notify user.
pub(crate) async fn check_expired_session_on_connect(
    state: &Arc<ServerState>,
    user_id: Uuid,
    pool: &PgPool,
) {
    let encrypted_rooms = match encrypted_rooms_for_user(pool, user_id).await {
        Ok(ids) => ids,
        Err(error) => {
            error!(
                "Failed to query encrypted rooms for user {}: {}",
                user_id, error
            );
            return;
        }
    };

    for room_id in encrypted_rooms {
        if state.connection_manager.is_session_active(room_id) {
            continue;
        }

        let expired_msg = json!({
            "type": "encrypt_session_expired",
            "data": {
                "room_id": room_id,
            }
        })
        .to_string();
        state.connection_manager.broadcast(room_id, &expired_msg);
    }
}
