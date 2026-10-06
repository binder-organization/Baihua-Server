use crate::ServerState;
use crate::common::StandardResponse;
use crate::common::error::ErrorResponse;
use crate::common::extractor::JsonBody;
use crate::middleware::authenticate::AuthenticatedUser;
use crate::user::find_user_by_username;
use axum::Extension;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::json;
use sqlx::Row;
use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::Arc;
use uuid::Uuid;

use crate::chat::{ROLE_ADMIN, ROLE_MEMBER};

#[derive(Debug, Deserialize)]
pub struct CreateRoomRequest {
    // Target username for private chat (used when is_group=false).
    #[serde(default)]
    pub username: Option<String>,
    // Group room name (required when is_group=true).
    #[serde(default)]
    pub name: Option<String>,
    // Usernames of initial group members (required when is_group=true).
    #[serde(default)]
    pub usernames: Option<Vec<String>>,
    // Whether this is a group room. Defaults to false (private chat).
    #[serde(default)]
    pub is_group: bool,
    // Whether this room uses end-to-end encryption (private chat only).
    #[serde(default)]
    pub is_encrypted: bool,
}

pub async fn create_or_get_room(
    State(state): State<Arc<ServerState>>,
    Extension(auth_user): Extension<AuthenticatedUser>,
    JsonBody(request): JsonBody<CreateRoomRequest>,
) -> Result<StandardResponse, ErrorResponse> {
    if request.is_group {
        create_group_room(&state, &auth_user, &request).await
    } else {
        // Private chat gating: a new private room may only be created when an
        // accepted room request exists between the two users. Existing rooms
        // (including rooms created before this feature) remain reachable.
        let username = request.username.as_ref().ok_or(ErrorResponse::BadRequest(
            "Target username is required for private chat.".to_string(),
        ))?;

        let target_user =
            find_user_by_username(username, &state.pool)
                .await?
                .ok_or(ErrorResponse::NotFound(
                    "Target user not found.".to_string(),
                ))?;

        if target_user.id == auth_user.user_id {
            return Err(ErrorResponse::BadRequest(
                "Cannot create a room with yourself.".to_string(),
            ));
        }

        let existing_room = find_private_room(
            &state.pool,
            auth_user.user_id,
            target_user.id,
            request.is_encrypted,
        )
        .await?;

        if existing_room.is_none() {
            let accepted_count: i64 = sqlx::query_scalar::<_, i64>(
                "SELECT count(*)::bigint FROM room_requests \
                 WHERE status = 'accepted' AND is_encrypted = $3 \
                   AND ((sender_id = $1 AND receiver_id = $2) \
                     OR (sender_id = $2 AND receiver_id = $1))",
            )
            .bind(auth_user.user_id)
            .bind(target_user.id)
            .bind(request.is_encrypted)
            .fetch_one(&state.pool)
            .await?;

            if accepted_count == 0 {
                return Err(ErrorResponse::Forbidden(
                    "A private room requires an accepted room request.".to_string(),
                ));
            }
        }

        create_private_room(&state, &auth_user, &request).await
    }
}

// Create a group room with multiple members. Creator becomes admin.
async fn create_group_room(
    state: &Arc<ServerState>,
    auth_user: &AuthenticatedUser,
    request: &CreateRoomRequest,
) -> Result<StandardResponse, ErrorResponse> {
    let name = request.name.as_ref().ok_or(ErrorResponse::BadRequest(
        "Group room name is required.".to_string(),
    ))?;

    if name.trim().is_empty() {
        return Err(ErrorResponse::BadRequest(
            "Group room name cannot be empty.".to_string(),
        ));
    }

    let usernames = request.usernames.as_ref().ok_or(ErrorResponse::BadRequest(
        "Group room members (usernames) are required.".to_string(),
    ))?;

    if usernames.is_empty() {
        return Err(ErrorResponse::BadRequest(
            "Group room must have at least one other member.".to_string(),
        ));
    }

    // Deduplicate and validate usernames.
    let mut unique_usernames: HashSet<&str> = HashSet::new();
    for username in usernames {
        if !unique_usernames.insert(username.as_str()) {
            return Err(ErrorResponse::BadRequest(format!(
                "Duplicate username: {}",
                username
            )));
        }
    }

    // Resolve all usernames to user IDs.
    let mut member_ids = Vec::with_capacity(usernames.len() + 1);
    member_ids.push(auth_user.user_id);

    for username in unique_usernames {
        let user =
            find_user_by_username(username, &state.pool)
                .await?
                .ok_or(ErrorResponse::NotFound(format!(
                    "User not found: {}",
                    username
                )))?;

        if user.id == auth_user.user_id {
            return Err(ErrorResponse::BadRequest(
                "Cannot add yourself to the usernames list. You are automatically included as the creator.".to_string(),
            ));
        }

        member_ids.push(user.id);
    }

    // Use a transaction for atomic room + members creation.
    let mut tx = state.pool.begin().await?;

    let room_id = Uuid::now_v7();
    let now = Utc::now();

    sqlx::query(
        "INSERT INTO rooms (id, name, created_by, created_at, is_group) VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(room_id)
    .bind(name)
    .bind(auth_user.user_id)
    .bind(now)
    .bind(true)
    .execute(&mut *tx)
    .await?;

    // Insert all members. Creator is admin, others are member.
    for (i, &user_id) in member_ids.iter().enumerate() {
        let role = if i == 0 { ROLE_ADMIN } else { ROLE_MEMBER };
        sqlx::query(
            "INSERT INTO room_members (room_id, user_id, joined_at, role) VALUES ($1, $2, $3, $4)",
        )
        .bind(room_id)
        .bind(user_id)
        .bind(now)
        .bind(role)
        .execute(&mut *tx)
        .await?;
    }

    tx.commit().await?;

    Ok(StandardResponse::success(
        StatusCode::CREATED,
        "Group room created successfully.".to_string(),
        json!({
            "id": room_id,
            "name": name,
            "created_by": auth_user.user_id,
            "created_at": now.to_rfc3339(),
            "is_group": true,
            "members": member_ids
        }),
    ))
}

// Look up an existing private (non-group) room shared by two users under
// the given encryption flag. Returns None when such a room does not exist.
// The executor is generic so callers can run the check inside a transaction.
async fn find_private_room<'c, E>(
    executor: E,
    user_a: Uuid,
    user_b: Uuid,
    is_encrypted: bool,
) -> Result<Option<Uuid>, ErrorResponse>
where
    E: sqlx::PgExecutor<'c>,
{
    let row = sqlx::query(
        "SELECT r.id FROM rooms r \
         INNER JOIN room_members m1 ON r.id = m1.room_id AND m1.user_id = $1 \
         INNER JOIN room_members m2 ON r.id = m2.room_id AND m2.user_id = $2 \
         WHERE r.is_group = false AND r.is_encrypted = $3",
    )
    .bind(user_a)
    .bind(user_b)
    .bind(is_encrypted)
    .fetch_optional(executor)
    .await?;

    Ok(row.map(|row| row.get::<Uuid, _>("id")))
}

// Create or find an existing private (2-person) room.
// The whole check-then-insert runs in one transaction guarded by an
// advisory lock keyed on the unordered user pair, so concurrent creations
// from either direction (two accepts, or direct POSTs) cannot double-create.
pub(crate) async fn create_private_room(
    state: &Arc<ServerState>,
    auth_user: &AuthenticatedUser,
    request: &CreateRoomRequest,
) -> Result<StandardResponse, ErrorResponse> {
    let username = request.username.as_ref().ok_or(ErrorResponse::BadRequest(
        "Target username is required for private chat.".to_string(),
    ))?;

    // Find target user by username.
    let target_user =
        find_user_by_username(username, &state.pool)
            .await?
            .ok_or(ErrorResponse::BadRequest(
                "Target user not found.".to_string(),
            ))?;

    // Don't allow creating a room with yourself.
    if target_user.id == auth_user.user_id {
        return Err(ErrorResponse::BadRequest(
            "Cannot create a room with yourself.".to_string(),
        ));
    }

    // The lock key must be direction-independent: (A,B) and (B,A) acquire
    // the same key so both orders of a cross-direction accept serialize.
    let (lock_user_a, lock_user_b) = if auth_user.user_id < target_user.id {
        (auth_user.user_id, target_user.id)
    } else {
        (target_user.id, auth_user.user_id)
    };

    let mut tx = state.pool.begin().await?;

    // Block other creators of the same pair until this transaction ends.
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1 || $2, 0))")
        .bind(lock_user_a.to_string())
        .bind(lock_user_b.to_string())
        .execute(&mut *tx)
        .await?;

    // Check if a private room already exists between these two users.
    // Encrypted and non-encrypted rooms are distinct; we match based on request.is_encrypted.
    let existing_room = find_private_room(
        &mut *tx,
        auth_user.user_id,
        target_user.id,
        request.is_encrypted,
    )
    .await?;

    if let Some(room_id) = existing_room {
        let row = sqlx::query(
            "SELECT id, name, created_by, created_at, is_group, is_encrypted FROM rooms WHERE id = $1",
        )
        .bind(room_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(ErrorResponse::NotFound("Room not found.".to_string()))?;

        let members = vec![auth_user.user_id, target_user.id];
        tx.rollback().await?;
        return Ok(StandardResponse::success(
            StatusCode::OK,
            "Room already exists.".to_string(),
            json!({
                "id": room_id,
                "name": Option::<String>::None,
            "created_by": row.get::<Option<Uuid>, _>("created_by"),
                "created_at": row.get::<DateTime<Utc>, _>("created_at").to_rfc3339(),
                "is_group": row.get::<bool, _>("is_group"),
                "is_encrypted": row.get::<bool, _>("is_encrypted"),
                "members": members
            }),
        ));
    }

    let room_id = Uuid::now_v7();
    let now = Utc::now();

    sqlx::query(
        "INSERT INTO rooms (id, name, created_by, created_at, is_group, is_encrypted) VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(room_id)
    .bind(Option::<String>::None)
    .bind(auth_user.user_id)
    .bind(now)
    .bind(false)
    .bind(request.is_encrypted)
    .execute(&mut *tx)
    .await?;

    // Add both users as members.
    let member_ids = vec![auth_user.user_id, target_user.id];
    for &user_id in &member_ids {
        sqlx::query("INSERT INTO room_members (room_id, user_id, joined_at) VALUES ($1, $2, $3)")
            .bind(room_id)
            .bind(user_id)
            .bind(now)
            .execute(&mut *tx)
            .await?;
    }

    tx.commit().await?;

    Ok(StandardResponse::success(
        StatusCode::CREATED,
        "Room created successfully.".to_string(),
        json!({
            "id": room_id,
            "name": Option::<String>::None,
            "created_by": auth_user.user_id,
            "created_at": now.to_rfc3339(),
            "is_group": false,
            "is_encrypted": request.is_encrypted,
            "members": member_ids
        }),
    ))
}

// Get detailed information about a single room.
pub async fn get_room_detail(
    State(state): State<Arc<ServerState>>,
    Extension(auth_user): Extension<AuthenticatedUser>,
    Path(room_id): Path<Uuid>,
) -> Result<StandardResponse, ErrorResponse> {
    crate::chat::find_room_by_id(&state.pool, room_id).await?;

    if !crate::chat::is_room_member(&state.pool, room_id, auth_user.user_id).await? {
        return Err(ErrorResponse::Forbidden(
            "You are not a member of this room.".to_string(),
        ));
    }

    let room_row = sqlx::query(
        "SELECT id, name, created_by, created_at, is_group, is_encrypted FROM rooms WHERE id = $1",
    )
    .bind(room_id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or(ErrorResponse::NotFound("Room not found.".to_string()))?;

    // Get members with user info.
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

    let member_count = members.len() as i64;

    Ok(StandardResponse::success(
        StatusCode::OK,
        "Room detail retrieved successfully.".to_string(),
        json!({
            "id": room_row.get::<Uuid, _>("id"),
            "name": room_row.get::<Option<String>, _>("name"),
            "created_by": room_row.get::<Option<Uuid>, _>("created_by"),
            "created_at": room_row.get::<DateTime<Utc>, _>("created_at").to_rfc3339(),
            "is_group": room_row.get::<bool, _>("is_group"),
            "is_encrypted": room_row.get::<bool, _>("is_encrypted"),
            "member_count": member_count,
            "members": members,
        }),
    ))
}

#[derive(Debug, Deserialize)]
pub struct ListRoomsQuery {
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}

// Keep the expensive room details within the requested page.
pub async fn list_rooms(
    State(state): State<Arc<ServerState>>,
    Extension(auth_user): Extension<AuthenticatedUser>,
    Query(params): Query<ListRoomsQuery>,
) -> Result<StandardResponse, ErrorResponse> {
    let limit = params.limit.unwrap_or(50);
    let offset = params.offset.unwrap_or(0);
    if !(1..=100).contains(&limit) {
        return Err(ErrorResponse::Validation(
            "Room page limit must be between 1 and 100.".to_string(),
        ));
    }
    if offset < 0 {
        return Err(ErrorResponse::Validation(
            "Room page offset cannot be negative.".to_string(),
        ));
    }

    let mut room_rows = sqlx::query(
        "SELECT r.id, r.name, r.created_by, r.created_at, r.is_group, r.is_encrypted, r.role, \
                mc.cnt AS member_count, \
                lm.msg_id AS last_msg_id, lm.content AS last_msg_content, \
                lm.created_at AS last_msg_created_at, lm.sender_username AS last_msg_sender_username \
         FROM ( \
             SELECT rooms.id, rooms.name, rooms.created_by, rooms.created_at, \
                    rooms.is_group, rooms.is_encrypted, room_members.role \
             FROM rooms \
             INNER JOIN room_members ON rooms.id = room_members.room_id \
             WHERE room_members.user_id = $1 \
             ORDER BY rooms.created_at DESC, rooms.id DESC \
             LIMIT $2 OFFSET $3 \
         ) r \
         LEFT JOIN LATERAL ( \
             SELECT COUNT(*) AS cnt FROM room_members WHERE room_id = r.id \
         ) mc ON true \
         LEFT JOIN LATERAL ( \
             SELECT m.id AS msg_id, COALESCE(m.content, 'File: ' || f.original_name) AS content, m.created_at, u.username AS sender_username \
             FROM messages m \
             LEFT JOIN users u ON m.sender_id = u.id \
             LEFT JOIN file_attachments f ON f.message_id = m.id \
             WHERE m.room_id = r.id \
             ORDER BY m.created_at DESC, m.id DESC \
             LIMIT 1 \
         ) lm ON true \
         ORDER BY r.created_at DESC, r.id DESC",
    )
    .bind(auth_user.user_id)
    .bind(limit + 1)
    .bind(offset)
    .fetch_all(&state.pool)
    .await?;

    let has_more = room_rows.len() > limit as usize;
    room_rows.truncate(limit as usize);

    // Collect room IDs for the member UUID query.
    let room_ids: Vec<Uuid> = room_rows.iter().map(|r| r.get("id")).collect();

    // Query 2: member UUIDs for all visible rooms.
    let member_rows = if room_ids.is_empty() {
        vec![]
    } else {
        sqlx::query("SELECT room_id, user_id FROM room_members WHERE room_id = ANY($1)")
            .bind(&room_ids)
            .fetch_all(&state.pool)
            .await?
    };

    // Group member UUIDs by room_id.
    let mut members_map: HashMap<Uuid, Vec<Uuid>> = HashMap::new();
    for row in &member_rows {
        let rid: Uuid = row.get("room_id");
        let uid: Uuid = row.get("user_id");
        members_map.entry(rid).or_default().push(uid);
    }

    // Build response.
    let mut rooms = Vec::new();
    for row in &room_rows {
        let room_id: Uuid = row.get("id");
        let is_encrypted: bool = row.get("is_encrypted");

        let last_message = if is_encrypted {
            None
        } else {
            row.get::<Option<String>, _>("last_msg_content")
                .map(|content| {
                    json!({
                        "id": row.get::<Uuid, _>("last_msg_id"),
                        "content": content,
                        "sender_username": row.get::<Option<String>, _>("last_msg_sender_username"),
                        "created_at": row.get::<DateTime<Utc>, _>("last_msg_created_at").to_rfc3339(),
                    })
                })
        };

        let members = members_map.get(&room_id).cloned().unwrap_or_default();

        rooms.push(json!({
            "id": room_id,
            "name": row.get::<Option<String>, _>("name"),
            "created_by": row.get::<Option<Uuid>, _>("created_by"),
            "created_at": row.get::<DateTime<Utc>, _>("created_at").to_rfc3339(),
            "is_group": row.get::<bool, _>("is_group"),
            "is_encrypted": is_encrypted,
            "member_count": row.get::<i64, _>("member_count"),
            "role": row.get::<String, _>("role"),
            "members": members,
            "last_message": last_message,
        }));
    }

    Ok(StandardResponse::success(
        StatusCode::OK,
        "Rooms listed successfully.".to_string(),
        json!({
            "rooms": rooms,
            "has_more": has_more,
            "limit": limit,
            "offset": offset,
        }),
    ))
}
