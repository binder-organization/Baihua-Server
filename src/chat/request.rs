use crate::ServerState;
use crate::chat::room::create_private_room;
use crate::chat::validate_message_content;
use crate::common::StandardResponse;
use crate::common::error::ErrorResponse;
use crate::common::extractor::JsonBody;
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

// Rows are read with sqlx::Row::get like the rest of the chat module.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoomRequestStatus {
    Pending,
    Accepted,
    Declined,
    Expired,
    Cancelled,
}

impl RoomRequestStatus {
    // Database and JSON wire representation (lowercase).
    pub fn as_str(&self) -> &'static str {
        match self {
            RoomRequestStatus::Pending => "pending",
            RoomRequestStatus::Accepted => "accepted",
            RoomRequestStatus::Declined => "declined",
            RoomRequestStatus::Expired => "expired",
            RoomRequestStatus::Cancelled => "cancelled",
        }
    }

    pub fn from_str_checked(value: &str) -> Result<Self, ErrorResponse> {
        match value {
            "pending" => Ok(RoomRequestStatus::Pending),
            "accepted" => Ok(RoomRequestStatus::Accepted),
            "declined" => Ok(RoomRequestStatus::Declined),
            "expired" => Ok(RoomRequestStatus::Expired),
            "cancelled" => Ok(RoomRequestStatus::Cancelled),
            other => Err(ErrorResponse::InternalError(format!(
                "Unknown room request status stored in database: '{}'.",
                other
            ))),
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct CreateRoomRequestPayload {
    pub receiver_id: Uuid,
    // Encryption flag the sender wants for the room once accepted.
    #[serde(default)]
    pub is_encrypted: bool,
    pub message: String,
}

#[derive(Debug, Deserialize)]
pub struct ListRequestsQuery {
    #[serde(default)]
    pub limit: Option<u32>,
    #[serde(default)]
    pub offset: Option<u32>,
}

// Mark overdue pending requests as expired (lazy sweep, run per request
// creation to avoid a background job).
async fn expire_stale_requests(
    pool: &sqlx::PgPool,
    expiry_hours: u64,
) -> Result<(), ErrorResponse> {
    sqlx::query(
        "UPDATE room_requests SET status = 'expired' \
         WHERE status = 'pending' AND created_at < now() - make_interval(hours => $1)",
    )
    .bind(expiry_hours as i32)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn create_room_request(
    State(state): State<Arc<ServerState>>,
    Extension(auth_user): Extension<AuthenticatedUser>,
    JsonBody(payload): JsonBody<CreateRoomRequestPayload>,
) -> Result<StandardResponse, ErrorResponse> {
    let cfg = &state.configuration.room_request;

    if payload.receiver_id == auth_user.user_id {
        return Err(ErrorResponse::BadRequest(
            "You cannot send a room request to yourself.".to_string(),
        ));
    }

    // The receiver must exist and be active.
    let receiver = sqlx::query(
        "SELECT id FROM users WHERE id = $1 AND is_active = true AND email_verified_at IS NOT NULL",
    )
    .bind(payload.receiver_id)
    .fetch_optional(&state.pool)
    .await?;

    if receiver.is_none() {
        return Err(ErrorResponse::BadRequest(
            "Target user not found.".to_string(),
        ));
    }

    // Sweep overdue pending requests before any pending-based check below,
    // so expired rows do not block re-sending or consume the inbox cap.
    expire_stale_requests(&state.pool, cfg.expiry_hours).await?;

    // Enforce the sender's daily sending limit.
    let sent_today: i64 = sqlx::query_scalar(
        "SELECT count(*)::bigint FROM room_requests \
         WHERE sender_id = $1 AND created_at >= now() - interval '1 day'",
    )
    .bind(auth_user.user_id)
    .fetch_one(&state.pool)
    .await?;

    if sent_today >= cfg.send_daily_limit as i64 {
        return Err(ErrorResponse::TooManyRequests(
            "You have reached the daily room request limit.".to_string(),
        ));
    }

    // Enforce the receiver's pending inbox cap.
    let pending_count: i64 = sqlx::query_scalar(
        "SELECT count(*)::bigint FROM room_requests WHERE receiver_id = $1 AND status = 'pending'",
    )
    .bind(payload.receiver_id)
    .fetch_one(&state.pool)
    .await?;

    if pending_count >= cfg.pending_max as i64 {
        return Err(ErrorResponse::Conflict(
            "The target user has too many pending room requests.".to_string(),
        ));
    }

    // Idempotency: only one pending request per (sender, receiver) pair.
    let duplicate = sqlx::query(
        "SELECT id FROM room_requests \
         WHERE sender_id = $1 AND receiver_id = $2 AND status = 'pending'",
    )
    .bind(auth_user.user_id)
    .bind(payload.receiver_id)
    .fetch_optional(&state.pool)
    .await?;

    if duplicate.is_some() {
        return Err(ErrorResponse::Conflict(
            "A pending room request to this user already exists.".to_string(),
        ));
    }

    let message = validate_message_content(payload.message, cfg.message_max_bytes as usize)?;

    let request_id = Uuid::now_v7();
    let now = Utc::now();

    sqlx::query(
        "INSERT INTO room_requests (id, sender_id, receiver_id, message, status, is_encrypted, created_at) \
         VALUES ($1, $2, $3, $4, 'pending', $5, $6)",
    )
    .bind(request_id)
    .bind(auth_user.user_id)
    .bind(payload.receiver_id)
    .bind(message)
    .bind(payload.is_encrypted)
    .bind(now)
    .execute(&state.pool)
    .await?;

    Ok(StandardResponse::success(
        StatusCode::CREATED,
        "Room request sent successfully.".to_string(),
        json!({
            "request_id": request_id,
            "sender_id": auth_user.user_id,
            "receiver_id": payload.receiver_id,
            "status": RoomRequestStatus::Pending.as_str(),
            "created_at": now.to_rfc3339(),
        }),
    ))
}

pub async fn list_pending_requests(
    State(state): State<Arc<ServerState>>,
    Extension(auth_user): Extension<AuthenticatedUser>,
    Query(params): Query<ListRequestsQuery>,
) -> Result<StandardResponse, ErrorResponse> {
    let limit = params.limit.unwrap_or(50).min(100);
    let offset = params.offset.unwrap_or(0);

    let count: i64 = sqlx::query_scalar(
        "SELECT count(*)::bigint FROM room_requests WHERE receiver_id = $1 AND status = 'pending'",
    )
    .bind(auth_user.user_id)
    .fetch_one(&state.pool)
    .await?;

    let rows = sqlx::query(
        "SELECT r.id, r.message, r.is_encrypted, r.created_at, \
                u.id AS sender_user_id, u.username, u.nickname \
         FROM room_requests r \
         INNER JOIN users u ON u.id = r.sender_id \
         WHERE r.receiver_id = $1 AND r.status = 'pending' \
         ORDER BY r.created_at DESC \
         LIMIT $2 OFFSET $3",
    )
    .bind(auth_user.user_id)
    .bind(limit as i64)
    .bind(offset as i64)
    .fetch_all(&state.pool)
    .await?;

    let requests: Vec<serde_json::Value> = rows
        .iter()
        .map(|row| {
            json!({
                "id": row.get::<Uuid, _>("id"),
                "message": row.get::<String, _>("message"),
                "is_encrypted": row.get::<bool, _>("is_encrypted"),
                "created_at": row.get::<DateTime<Utc>, _>("created_at").to_rfc3339(),
                "sender": {
                    "user_id": row.get::<Uuid, _>("sender_user_id"),
                    "username": row.get::<String, _>("username"),
                    "nickname": row.get::<Option<String>, _>("nickname"),
                },
            })
        })
        .collect();

    Ok(StandardResponse::success(
        StatusCode::OK,
        "Pending room requests retrieved successfully.".to_string(),
        json!({
            "requests": requests,
            "count": count,
            "limit": limit,
            "offset": offset,
        }),
    ))
}

pub async fn list_sent_requests(
    State(state): State<Arc<ServerState>>,
    Extension(auth_user): Extension<AuthenticatedUser>,
    Query(params): Query<ListRequestsQuery>,
) -> Result<StandardResponse, ErrorResponse> {
    let limit = params.limit.unwrap_or(50).min(100);
    let offset = params.offset.unwrap_or(0);

    let count: i64 =
        sqlx::query_scalar("SELECT count(*)::bigint FROM room_requests WHERE sender_id = $1")
            .bind(auth_user.user_id)
            .fetch_one(&state.pool)
            .await?;

    let rows = sqlx::query(
        "SELECT r.id, r.message, r.is_encrypted, r.created_at, r.status, \
                u.id AS receiver_user_id, u.username, u.nickname \
         FROM room_requests r \
         INNER JOIN users u ON u.id = r.receiver_id \
         WHERE r.sender_id = $1 \
         ORDER BY r.created_at DESC \
         LIMIT $2 OFFSET $3",
    )
    .bind(auth_user.user_id)
    .bind(limit as i64)
    .bind(offset as i64)
    .fetch_all(&state.pool)
    .await?;

    let requests: Vec<serde_json::Value> = rows
        .iter()
        .map(|row| {
            json!({
                "id": row.get::<Uuid, _>("id"),
                "message": row.get::<String, _>("message"),
                "is_encrypted": row.get::<bool, _>("is_encrypted"),
                "status": row.get::<String, _>("status"),
                "created_at": row.get::<DateTime<Utc>, _>("created_at").to_rfc3339(),
                "receiver": {
                    "user_id": row.get::<Uuid, _>("receiver_user_id"),
                    "username": row.get::<String, _>("username"),
                    "nickname": row.get::<Option<String>, _>("nickname"),
                },
            })
        })
        .collect();

    Ok(StandardResponse::success(
        StatusCode::OK,
        "Sent room requests retrieved successfully.".to_string(),
        json!({
            "requests": requests,
            "count": count,
            "limit": limit,
            "offset": offset,
        }),
    ))
}

pub async fn accept_room_request(
    State(state): State<Arc<ServerState>>,
    Extension(auth_user): Extension<AuthenticatedUser>,
    Path(request_id): Path<Uuid>,
) -> Result<StandardResponse, ErrorResponse> {
    // Lock the request row so concurrent accepts serialize: the loser re-reads
    // status='accepted' and reuses the room created by the winner.
    let mut tx = state.pool.begin().await?;

    let row = sqlx::query(
        "SELECT id, sender_id, receiver_id, message, status, is_encrypted, created_at, responded_at \
         FROM room_requests WHERE id = $1 FOR UPDATE",
    )
    .bind(request_id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(ErrorResponse::NotFound(
        "Room request not found.".to_string(),
    ))?;

    let receiver_id: Uuid = row.get("receiver_id");
    if receiver_id != auth_user.user_id {
        return Err(ErrorResponse::Forbidden(
            "Only the receiver can accept this room request.".to_string(),
        ));
    }

    let status = RoomRequestStatus::from_str_checked(&row.get::<String, _>("status"))?;

    match status {
        // Idempotent path: the room already exists, so re-accepting only
        // needs the status update to be a no-op.
        RoomRequestStatus::Pending | RoomRequestStatus::Accepted => {}
        RoomRequestStatus::Declined | RoomRequestStatus::Expired | RoomRequestStatus::Cancelled => {
            return Err(ErrorResponse::BadRequest(
                "Room request has already been handled.".to_string(),
            ));
        }
    }

    let sender_id: Uuid = row.get("sender_id");
    let sender_username: String = sqlx::query_scalar("SELECT username FROM users WHERE id = $1")
        .bind(sender_id)
        .fetch_optional(&state.pool)
        .await?
        .ok_or(ErrorResponse::InternalError(
            "Sender user no longer exists.".to_string(),
        ))?;

    let room_request = crate::chat::room::CreateRoomRequest {
        username: Some(sender_username),
        name: None,
        usernames: None,
        is_group: false,
        is_encrypted: row.get::<bool, _>("is_encrypted"),
    };

    // Build the room directly through the private room creator (bypassing
    // the gating a plain POST /rooms would enforce).
    let room_response = create_private_room(&state, &auth_user, &room_request).await?;

    sqlx::query(
        "UPDATE room_requests SET status = 'accepted', responded_at = now() \
         WHERE id = $1 AND status = 'pending'",
    )
    .bind(request_id)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;

    let room_payload = room_response.body.data.ok_or(ErrorResponse::InternalError(
        "Failed to retrieve the created room.".to_string(),
    ))?;

    Ok(StandardResponse::success(
        StatusCode::OK,
        "Room request accepted.".to_string(),
        json!({
            "request_id": request_id,
            "status": RoomRequestStatus::Accepted.as_str(),
            "room": room_payload,
        }),
    ))
}

pub async fn decline_room_request(
    State(state): State<Arc<ServerState>>,
    Extension(auth_user): Extension<AuthenticatedUser>,
    Path(request_id): Path<Uuid>,
) -> Result<StandardResponse, ErrorResponse> {
    // Lock the request row so a concurrent accept cannot race the decline:
    // whoever wins sets the terminal status and the other sees it.
    let mut tx = state.pool.begin().await?;

    let row = sqlx::query(
        "SELECT id, sender_id, receiver_id, message, status, is_encrypted, created_at, responded_at \
         FROM room_requests WHERE id = $1 FOR UPDATE",
    )
    .bind(request_id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(ErrorResponse::NotFound(
        "Room request not found.".to_string(),
    ))?;

    let receiver_id: Uuid = row.get("receiver_id");
    if receiver_id != auth_user.user_id {
        return Err(ErrorResponse::Forbidden(
            "Only the receiver can decline this room request.".to_string(),
        ));
    }

    let status = RoomRequestStatus::from_str_checked(&row.get::<String, _>("status"))?;
    if status != RoomRequestStatus::Pending {
        return Err(ErrorResponse::BadRequest(
            "Room request has already been handled.".to_string(),
        ));
    }

    sqlx::query(
        "UPDATE room_requests SET status = 'declined', responded_at = now() \
         WHERE id = $1 AND status = 'pending'",
    )
    .bind(request_id)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;

    Ok(StandardResponse::success(
        StatusCode::OK,
        "Room request declined.".to_string(),
        json!({
            "request_id": request_id,
            "status": RoomRequestStatus::Declined.as_str(),
        }),
    ))
}

// The sender withdraws a still-pending request. Cancelling frees the
// (sender, receiver) pair for a fresh request and the receiver's inbox slot.
pub async fn cancel_room_request(
    State(state): State<Arc<ServerState>>,
    Extension(auth_user): Extension<AuthenticatedUser>,
    Path(request_id): Path<Uuid>,
) -> Result<StandardResponse, ErrorResponse> {
    // Lock the request row so a concurrent accept/decline cannot win after
    // the cancel reads the row.
    let mut tx = state.pool.begin().await?;

    let row = sqlx::query(
        "SELECT id, sender_id, receiver_id, message, status, is_encrypted, created_at, responded_at \
         FROM room_requests WHERE id = $1 FOR UPDATE",
    )
    .bind(request_id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(ErrorResponse::NotFound(
        "Room request not found.".to_string(),
    ))?;

    let sender_id: Uuid = row.get("sender_id");
    if sender_id != auth_user.user_id {
        return Err(ErrorResponse::Forbidden(
            "Only the sender can cancel this room request.".to_string(),
        ));
    }

    let status = RoomRequestStatus::from_str_checked(&row.get::<String, _>("status"))?;
    if status != RoomRequestStatus::Pending {
        return Err(ErrorResponse::BadRequest(
            "Room request has already been handled.".to_string(),
        ));
    }

    sqlx::query(
        "UPDATE room_requests SET status = 'cancelled' \
         WHERE id = $1 AND status = 'pending'",
    )
    .bind(request_id)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;

    Ok(StandardResponse::success(
        StatusCode::OK,
        "Room request cancelled.".to_string(),
        json!({
            "request_id": request_id,
            "status": RoomRequestStatus::Cancelled.as_str(),
        }),
    ))
}
