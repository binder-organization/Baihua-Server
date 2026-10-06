use crate::ServerState;
use crate::authenticate::jsonwebtoken::{extract_token_from_header, validate_token};
use crate::chat::encrypted;
use crate::chat::{find_room_by_id, is_room_member};
use crate::common::error::ErrorResponse;
use crate::user::find_user_by_id;
use axum::extract::State;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::http::HeaderMap;
use axum::response::IntoResponse;
use chrono::Utc;
use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::mpsc;
use tokio::time::{Duration, interval};
use tracing::{error, warn};
use uuid::Uuid;

// Client -> Server
const WS_TYPING: &str = "typing";
const WS_PONG: &str = "pong";

// Server -> Client
const WS_USER_ONLINE: &str = "user_online";
const WS_USER_OFFLINE: &str = "user_offline";
const WS_TYPING_INDICATOR: &str = "typing";
const WS_NEW_MESSAGE: &str = "new_message";
const WS_MESSAGE_SENT: &str = "message_sent";
const WS_ERROR: &str = "error";

// Client -> Server
const WS_SEND_MESSAGE: &str = "send_message";

// Encrypted chat (Client -> Server)
const WS_ENCRYPT_REQUEST: &str = "encrypt_request";
const WS_ENCRYPT_ACCEPT: &str = "encrypt_accept";
const WS_ENCRYPT_READY: &str = "encrypt_ready";
const WS_ENCRYPT_MESSAGE: &str = "encrypt_message";
const WS_ENCRYPT_LEAVE: &str = "encrypt_leave";

fn extract_ws_token(headers: &HeaderMap) -> Result<String, ErrorResponse> {
    let auth_value = headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| {
            ErrorResponse::Authentication("Missing Authorization header.".to_string())
        })?;
    let token = extract_token_from_header(auth_value)?;
    Ok(token.to_string())
}

// HTTP handler that upgrades to WebSocket after JWT validation.
pub async fn ws_handler(
    ws: WebSocketUpgrade,
    headers: HeaderMap,
    State(state): State<Arc<ServerState>>,
) -> Result<impl IntoResponse, ErrorResponse> {
    // Reject browser-originated upgrades whose Origin is not allowlisted.
    // Non-browser clients (CLI, native apps) send no Origin header and are
    // always allowed.
    if let Some(origin) = headers.get("origin").and_then(|value| value.to_str().ok()) {
        let allowed = &state.configuration.websocket.allowed_origins;
        if !allowed.is_empty()
            && !allowed
                .iter()
                .any(|allowed_origin| allowed_origin == origin)
        {
            return Err(ErrorResponse::Forbidden(
                "Origin is not allowed.".to_string(),
            ));
        }
    }

    let token = extract_ws_token(&headers)?;
    let claims = validate_token(&token, &state.jwt_secret)?;

    let user_id = Uuid::parse_str(&claims.sub).map_err(|_| {
        error!("JWT sub claim is not a valid UUID: {}", claims.sub);
        ErrorResponse::Authentication("Invalid token.".to_string())
    })?;

    let user = find_user_by_id(user_id, &state.pool)
        .await?
        .ok_or_else(|| ErrorResponse::Authentication("User not found or inactive.".to_string()))?;

    if !user.is_active {
        return Err(ErrorResponse::Authentication(
            "User is inactive.".to_string(),
        ));
    }

    if claims.token_version != user.token_version {
        return Err(ErrorResponse::Authentication(
            "Session expired. Please reconnect.".to_string(),
        ));
    }

    // Snapshot the version before the user is moved into the socket task;
    // every later check compares live data against this frozen value.
    let token_version = user.token_version;

    Ok(ws.on_upgrade(move |socket| handle_socket(socket, state, user, token, token_version)))
}

// Main WebSocket lifecycle handler.
async fn handle_socket(
    socket: WebSocket,
    state: Arc<ServerState>,
    user: crate::user::User,
    token: String,
    token_version: i64,
) {
    let manager = &state.connection_manager;

    // Per-connection channels (created first so we can send error messages).
    let (msg_tx, mut msg_rx) = mpsc::unbounded_channel::<String>();

    let mut shutdown_rx = manager.shutdown_notification();

    // Split the WebSocket into sender + receiver halves.
    let (mut ws_sender, mut ws_receiver) = socket.split();

    // Query all rooms the user belongs to.
    let user_room_ids = match get_user_room_ids(&state.pool, user.id).await {
        Ok(ids) => ids,
        Err(error) => {
            let err_msg = json!({
                "type": WS_ERROR,
                "data": { "message": error.to_string() }
            })
            .to_string();
            let _ = ws_sender.send(Message::Text(err_msg.into())).await;
            return;
        }
    };

    // Presence: mark user online (only after rooms are loaded).
    let just_came_online = manager.user_connected(user.id);

    if just_came_online {
        manager.cancel_grace_periods_for_user(user.id);

        // Check for expired encrypted sessions.
        encrypted::check_expired_session_on_connect(&state, user.id, &state.pool).await;

        let online_msg = json!({
            "type": WS_USER_ONLINE,
            "data": {
                "user_id": user.id,
                "username": user.username,
            }
        })
        .to_string();
        for &room_id in &user_room_ids {
            manager.broadcast(room_id, &online_msg);
        }
    }

    // Track which rooms this connection is subscribed to (room_id -> subscription_id).
    let mut subscribed_rooms: HashMap<Uuid, Uuid> = HashMap::new();

    // Auto-subscribe: connect → immediately subscribe to all rooms.
    for &room_id in &user_room_ids {
        subscribe_to_room(user.id, room_id, &state, &msg_tx, &mut subscribed_rooms);
    }

    // Send a "connected" confirmation.
    let connected_msg = json!({
        "type": "connected",
        "data": {
            "user_id": user.id,
            "rooms": user_room_ids,
        }
    })
    .to_string();
    let _ = msg_tx.send(connected_msg);

    let ws_config = &state.configuration.websocket;

    // Heartbeat: send WebSocket protocol-level PING frames.
    // The client library auto-responds with PONG at the frame level.
    // If the send fails, the connection is dead and we break.
    let mut heartbeat = interval(Duration::from_secs(ws_config.heartbeat_interval_secs));
    heartbeat.tick().await; // skip the immediate first tick

    let mut msg_timestamps: VecDeque<Instant> = VecDeque::new();

    let mut re_validate = interval(Duration::from_secs(
        ws_config.token_revalidate_interval_secs,
    ));
    re_validate.tick().await; // skip the immediate first tick

    // Main event loop
    loop {
        tokio::select! {
            // Outgoing: forward internal channel messages to WebSocket.
            msg = msg_rx.recv() => {
                match msg {
                    Some(text) => {
                        if ws_sender.send(Message::Text(text.into())).await.is_err() {
                            break;
                        }
                    }
                    None => break,
                }
            }

            // Incoming: messages from WebSocket client.
            msg = ws_receiver.next() => {
                match msg {
                    Some(Ok(Message::Text(text))) => {
                        let now = Instant::now();
                        let rate_window = Duration::from_secs(ws_config.message_rate_window_secs);
                        while msg_timestamps.front().is_some_and(|t| now - *t > rate_window) {
                            msg_timestamps.pop_front();
                        }
                        if msg_timestamps.len() >= ws_config.message_rate_limit as usize {
                            let err_msg = json!({
                                "type": WS_ERROR,
                                "data": { "message": "Rate limit exceeded. Please slow down." }
                            }).to_string();
                            let _ = msg_tx.send(err_msg);
                        } else {
                            msg_timestamps.push_back(now);
                            match handle_incoming(
                                &state, &user, &text, token_version,
                            ).await {
                                Ok(Some(response)) => {
                                    let _ = msg_tx.send(response);
                                }
                                Ok(None) => {}
                                Err(error) => {
                                    let err_msg = json!({
                                        "type": WS_ERROR,
                                        "data": { "message": error.to_string() }
                                    }).to_string();
                                    let _ = msg_tx.send(err_msg);
                                }
                            }
                        }
                    }
                    Some(Ok(Message::Close(_))) | None => break,
                    _ => {}
                }
            }

            _ = heartbeat.tick() => {
                if ws_sender.send(Message::Ping(vec![].into())).await.is_err() {
                    break;
                }
            }

            _ = re_validate.tick() => {
                let claims = match validate_token(&token, &state.jwt_secret) {
                    Ok(claims) => claims,
                    Err(_) => {
                        let err_msg = json!({
                            "type": WS_ERROR,
                            "data": { "message": "Token expired. Please reconnect." }
                        }).to_string();
                        let _ = msg_tx.send(err_msg);
                        break;
                    }
                };
                match find_user_by_id(user.id, &state.pool).await {
                    Ok(Some(u)) if u.is_active && u.token_version == claims.token_version => {}
                    Ok(_) => {
                        let err_msg = json!({
                            "type": WS_ERROR,
                            "data": { "message": "Session expired. Please reconnect." }
                        }).to_string();
                        let _ = msg_tx.send(err_msg);
                        break;
                    }
                    Err(error) => {
                        error!("Re-validation DB error for user {}: {}", user.id, error);
                        let err_msg = json!({
                            "type": WS_ERROR,
                            "data": { "message": "Internal server error. Will retry." }
                        }).to_string();
                        let _ = msg_tx.send(err_msg);
                    }
                }
            }

            _ = shutdown_rx.recv() => {
                let _ = ws_sender.send(Message::Close(None)).await;
                break;
            }
        }
    }

    // Cleanup: cancel all room subscriptions to clean up ConnectionManager.subs.
    // Use cancel_stale_subscription so only this connection's forward tasks exit.
    for (&room_id, &subscription_id) in &subscribed_rooms {
        manager.cancel_stale_subscription(user.id, room_id, subscription_id);
    }

    let fully_offline = manager.user_disconnected(user.id);
    if fully_offline {
        // Start grace periods for all active encrypted sessions this user was in.
        encrypted::start_grace_periods_for_user(&state, user.id, &state.pool).await;

        let offline_msg = json!({
            "type": WS_USER_OFFLINE,
            "data": {
                "user_id": user.id,
                "username": user.username,
            }
        })
        .to_string();
        for &room_id in &user_room_ids {
            manager.broadcast(room_id, &offline_msg);
        }
    }
}

// Subscribe a connection to a room's broadcast channel.
// Spawns a background task that forwards messages from the room's
// broadcast channel to the connection's internal message channel.
// The task exits when the cancel signal fires (user left/kicked) or the
// connection closes.
fn subscribe_to_room(
    user_id: Uuid,
    room_id: Uuid,
    state: &Arc<ServerState>,
    msg_tx: &mpsc::UnboundedSender<String>,
    subscribed_rooms: &mut HashMap<Uuid, Uuid>,
) {
    if subscribed_rooms.contains_key(&room_id) {
        return;
    }

    let mut rx = state.connection_manager.subscribe(room_id);
    let (mut cancel_rx, subscription_id) = state
        .connection_manager
        .register_subscription(user_id, room_id);
    let forward_tx = msg_tx.clone();

    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = cancel_rx.changed() => {
                    // User left this room; stop forwarding.
                    break;
                }
                msg = rx.recv() => {
                    match msg {
                        Ok(text) => {
                            if is_own_typing_indicator(&text, user_id) {
                                continue;
                            }
                            if forward_tx.send(text).is_err() {
                                break;
                            }
                        }
                        Err(RecvError::Lagged(n)) => {
                            warn!(
                                "forward task lagged by {n} messages for user {user_id} in room {room_id}; continuing"
                            );
                            // Receiver is still valid — skipped messages are gone,
                            // but future messages will still arrive.
                        }
                        Err(RecvError::Closed) => break,
                    }
                }
            }
        }
    });

    subscribed_rooms.insert(room_id, subscription_id);
}

// Check whether a broadcast message is a typing indicator from the given user.
// Used in forward tasks to avoid echoing a user's own typing events back to them.
fn is_own_typing_indicator(message: &str, user_id: Uuid) -> bool {
    let value: serde_json::Value = match serde_json::from_str(message) {
        Ok(v) => v,
        Err(_) => return false,
    };
    let msg_type = match value.get("type").and_then(|v| v.as_str()) {
        Some(t) => t,
        None => return false,
    };
    if msg_type != WS_TYPING_INDICATOR {
        return false;
    }
    match value
        .get("data")
        .and_then(|d| d.get("user_id"))
        .and_then(|u| u.as_str())
    {
        Some(id_str) => {
            let id = match Uuid::parse_str(id_str) {
                Ok(id) => id,
                Err(_) => return false,
            };
            id == user_id
        }
        None => false,
    }
}

// Process a JSON message received from the client.
// Returns Ok(None) for messages that need no response,
// Ok(Some(response)) for ack messages to send back,
// or Err(error) for errors to send back.
async fn handle_incoming(
    state: &Arc<ServerState>,
    user: &crate::user::User,
    text: &str,
    token_version: i64,
) -> Result<Option<String>, ErrorResponse> {
    let value: serde_json::Value = serde_json::from_str(text)
        .map_err(|e| ErrorResponse::Json(format!("Invalid WS message JSON: {}", e)))?;

    let msg_type = value.get("type").and_then(|v| v.as_str()).ok_or_else(|| {
        ErrorResponse::Validation("Missing 'type' field in WS message.".to_string())
    })?;

    match msg_type {
        WS_TYPING => {
            let room_id_str = value
                .get("room_id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| {
                    ErrorResponse::Validation("Missing 'room_id' in typing.".to_string())
                })?;
            let room_id = Uuid::parse_str(room_id_str)
                .map_err(|_| ErrorResponse::Validation("Invalid room_id UUID.".to_string()))?;

            let typing_msg = json!({
                "type": WS_TYPING_INDICATOR,
                "data": {
                    "room_id": room_id,
                    "user_id": user.id,
                    "username": user.username,
                    "typing": true,
                }
            })
            .to_string();
            state.connection_manager.broadcast(room_id, &typing_msg);

            Ok(None)
        }

        WS_PONG => Ok(None),

        WS_SEND_MESSAGE => {
            let current_user = find_user_by_id(user.id, &state.pool)
                .await?
                .ok_or_else(|| ErrorResponse::Authentication("User not found.".to_string()))?;
            if !current_user.is_active {
                return Err(ErrorResponse::Authentication(
                    "User is inactive.".to_string(),
                ));
            }
            if current_user.token_version != token_version {
                return Err(ErrorResponse::Authentication(
                    "Session expired. Please reconnect.".to_string(),
                ));
            }

            let data = value.get("data").ok_or_else(|| {
                ErrorResponse::Validation("Missing 'data' field in send_message.".to_string())
            })?;

            let room_id_str = data
                .get("room_id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| {
                    ErrorResponse::Validation("Missing 'room_id' in send_message data.".to_string())
                })?;
            let room_id = Uuid::parse_str(room_id_str)
                .map_err(|_| ErrorResponse::Validation("Invalid room_id UUID.".to_string()))?;

            let content = data
                .get("content")
                .and_then(|v| v.as_str())
                .ok_or_else(|| {
                    ErrorResponse::Validation("Missing 'content' in send_message data.".to_string())
                })?;

            let content = crate::chat::validate_message_content(content.to_string(), 5000)?;

            find_room_by_id(&state.pool, room_id).await?;

            if !is_room_member(&state.pool, room_id, user.id).await? {
                return Err(ErrorResponse::Forbidden(
                    "You are not a member of this room.".to_string(),
                ));
            }

            let message_id = Uuid::now_v7();
            let now = Utc::now();

            sqlx::query(
                "INSERT INTO messages (id, room_id, sender_id, content, created_at) \
                 VALUES ($1, $2, $3, $4, $5)",
            )
            .bind(message_id)
            .bind(room_id)
            .bind(user.id)
            .bind(&content)
            .bind(now)
            .execute(&state.pool)
            .await?;

            let ws_message = json!({
                "type": WS_NEW_MESSAGE,
                "data": {
                    "id": message_id,
                    "room_id": room_id,
                    "sender_id": user.id,
                    "content": content,
                    "created_at": now.to_rfc3339(),
                }
            })
            .to_string();
            state.connection_manager.broadcast(room_id, &ws_message);

            let ack = json!({
                "type": WS_MESSAGE_SENT,
                "data": {
                    "id": message_id,
                    "room_id": room_id,
                    "sender_id": user.id,
                    "content": content,
                    "created_at": now.to_rfc3339(),
                }
            })
            .to_string();

            Ok(Some(ack))
        }

        WS_ENCRYPT_REQUEST => {
            let data = value.get("data").ok_or_else(|| {
                ErrorResponse::Validation("Missing 'data' in encrypt_request.".to_string())
            })?;
            let room_id = parse_uuid_field(data, "room_id")?;
            let public_key = parse_string_field(data, "public_key")?;
            let identity_key = parse_string_field(data, "identity_key")?;
            let signature = parse_string_field(data, "signature")?;
            encrypted::handle_encrypt_request(
                state,
                user,
                room_id,
                public_key,
                identity_key,
                signature,
            )
            .await
        }

        WS_ENCRYPT_ACCEPT => {
            let data = value.get("data").ok_or_else(|| {
                ErrorResponse::Validation("Missing 'data' in encrypt_accept.".to_string())
            })?;
            let room_id = parse_uuid_field(data, "room_id")?;
            let public_key = parse_string_field(data, "public_key")?;
            let identity_key = parse_string_field(data, "identity_key")?;
            let signature = parse_string_field(data, "signature")?;
            encrypted::handle_encrypt_accept(
                state,
                user,
                room_id,
                public_key,
                identity_key,
                signature,
            )
            .await
        }

        WS_ENCRYPT_READY => {
            let data = value.get("data").ok_or_else(|| {
                ErrorResponse::Validation("Missing 'data' in encrypt_ready.".to_string())
            })?;
            let room_id = parse_uuid_field(data, "room_id")?;
            encrypted::handle_encrypt_ready(state, user, room_id).await
        }

        WS_ENCRYPT_MESSAGE => {
            let data = value.get("data").ok_or_else(|| {
                ErrorResponse::Validation("Missing 'data' in encrypt_message.".to_string())
            })?;
            let room_id = parse_uuid_field(data, "room_id")?;
            let ciphertext = parse_string_field(data, "ciphertext")?;
            encrypted::handle_encrypt_message(state, user, room_id, ciphertext).await
        }

        WS_ENCRYPT_LEAVE => {
            let data = value.get("data").ok_or_else(|| {
                ErrorResponse::Validation("Missing 'data' in encrypt_leave.".to_string())
            })?;
            let room_id = parse_uuid_field(data, "room_id")?;
            encrypted::handle_encrypt_leave(state, user, room_id, &state.pool).await
        }

        _ => Err(ErrorResponse::Validation(format!(
            "Unknown WS message type: '{}'.",
            msg_type
        ))),
    }
}

// Query all room IDs the user is a member of.
async fn get_user_room_ids(pool: &sqlx::PgPool, user_id: Uuid) -> Result<Vec<Uuid>, ErrorResponse> {
    Ok(
        sqlx::query_scalar::<_, Uuid>("SELECT room_id FROM room_members WHERE user_id = $1")
            .bind(user_id)
            .fetch_all(pool)
            .await?,
    )
}

fn parse_uuid_field(data: &serde_json::Value, field: &str) -> Result<Uuid, ErrorResponse> {
    let raw = data
        .get(field)
        .and_then(|v| v.as_str())
        .ok_or_else(|| ErrorResponse::Validation(format!("Missing '{}' field.", field)))?;
    Uuid::parse_str(raw)
        .map_err(|_| ErrorResponse::Validation(format!("Invalid UUID for '{}'.", field)))
}

fn parse_string_field(data: &serde_json::Value, field: &str) -> Result<String, ErrorResponse> {
    data.get(field)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| ErrorResponse::Validation(format!("Missing '{}' field.", field)))
}
