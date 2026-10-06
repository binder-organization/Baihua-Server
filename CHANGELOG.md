# Changelog

All notable changes to Baihua Server will be documented in this file.

## [0.1.5] - 2026-10-06

### Added

- **File messages** — add authenticated file upload and download endpoints for chat rooms, with streamed uploads, SHA-256 verification, per-user storage quotas, concurrent upload limits, content deduplication, and automatic cleanup when message references are removed.
- **Encrypted file messages** — allow encrypted private chat sessions to relay encrypted file bytes and encrypted metadata without exposing plaintext file contents to the server.
- **Public deployment profile** — add an optional reverse proxy profile with automatic certificate management for deployments that expose the server publicly.
- **Backup and restore guidance** — document coordinated backups and restores for the database, avatars, file messages, and certificate state.

### Changed

- **Room listing pagination** — add `limit`, `offset`, and `has_more` fields to room listing responses and validate pagination values.
- **Message and user search pagination validation** — reject non-positive page limits with a validation error instead of silently accepting invalid values.
- **Encrypted session configuration** — make the encrypted session disconnect grace period configurable through the profile configuration and environment variables.
- **Container startup** — prepare persistent storage directories with least-privilege ownership and fail safely when mounted paths are not writable by the service user.
- **Runtime and test infrastructure** — improve server throughput, isolate integration test resources, add file message coverage, and add development console commands for debugging and restart control.

### Fixed

- **WebSocket delivery races** — prevent disconnect cleanup and multi-connection broadcast handling from losing messages during reconnection or concurrent connection changes.
- **File cleanup** — reclaim uploaded file data when validation, database insertion, room cleanup, or encrypted session termination removes the final reference.
- **Upload limits** — enforce file size, request idle timeout, quota, and concurrent upload limits while streaming request bodies.

## [0.1.4] - 2026-08-21

### Breaking Changes

- **API response field renamed** — the `error_code` field in the standard JSON response envelope is renamed to `code`. Success responses now return `"code": "SUCCESS"` instead of `"error_code": "OK"`. Error responses return `"code": "<ERROR_CODE>"` (e.g. `"NOT_FOUND_ERROR"`, `"BAD_REQUEST_ERROR"`). Clients must update any code that reads the `error_code` field.
- **Seed dev user removed** — development mode no longer auto-creates a seed admin user. The `SEED_PASSWORD` environment variable is removed from `.env.example`. Developers must now register accounts manually via `POST /api/v1/user/register`.

### Added

- **Room request flow** — new `/api/v1/chat/rooms/requests` endpoints for sending, listing (pending/sent), accepting, declining, and cancelling room requests. Private room creation now requires mutual consent: the sender must first send a request, and the receiver must accept it before a room is created. A `room_requests` table with `cancelled` status and supporting indexes is added via `003_create_room_requests_table.sql`. Configurable via `[room_request]` section: `message_max_bytes` (default 500), `expiry_hours` (default 120), `pending_max` (default 50), `send_daily_limit` (default 20).
- **User self-management** — `DELETE /api/v1/user/me` deletes the account after re-verifying the password; rooms and messages are preserved by switching `created_by` and `sender_id` to nullable (`ON DELETE SET NULL`), so remaining members see null references instead of losing the conversation.
- **Session revocation** — `POST /api/v1/user/me/logout` bumps `users.token_version` so every previously issued JWT stops passing the version check. `PATCH /api/v1/user/me/password` verifies the old password before hashing and storing the new one, then bumps `token_version` to force all devices to re-authenticate. `token_version` is embedded in JWT claims and validated in the HTTP auth middleware, the WebSocket handshake, the periodic re-validation tick, and `send_message`, returning 401 "Session expired" on mismatch.
- **User profile fields** — `bio` (<= 200 chars) and `avatar` (http/https URL) fields added to the user profile. `PATCH /api/v1/user/me` keeps the absent/null/unset three-state semantics. `GET /api/v1/user/{user}` and `GET /api/v1/user/search` return bio and avatar in every user object.
- **Avatar upload** — `POST /api/v1/user/me/avatar` accepts a multipart image upload, limited to 2 MB (configurable via `[avatar] max_bytes`) with a jpeg/png/gif/webp whitelist; the file is streamed in chunks so oversized bodies are rejected with 413 while reading, saved under `~/.baihua/avatars/{uuid}.{ext}`, and the avatar column points at `/static/avatars/{filename}`. `GET /static/avatars/{filename}` serves uploaded avatars publicly with a strict UUID+extension regex to prevent path traversal.
- **Conflict error code** — `409 CONFLICT_ERROR` added for encrypted session state conflicts and room request state transitions.
- **JsonBody extractor** — consolidates JSON rejection handling across handlers, replacing per-handler parsing logic.
- **Error middleware** — 405 Method Not Allowed, 408 Request Timeout, 413 Content Too Large, 503 Service Unavailable error responses added.
- **Configurable request timeout and body size** — `web.request_timeout_secs` (default 30) and `web.max_body_size` (default 10 MB) are now configurable via the profile config instead of hardcoded.
- **WebSocket origin allowlist** — reject browser-originated WebSocket upgrades when the `Origin` header does not appear in a configurable `websocket.allowed_origins` allowlist, preventing cross-site WebSocket hijacking. Non-browser clients that omit the `Origin` header continue unaffected. The allowlist is empty by default (all origins permitted).
- **WebSocket configuration** — `websocket.heartbeat_interval_secs` (default 30), `websocket.message_rate_limit` (default 30), `websocket.message_rate_window_secs` (default 10), and `websocket.token_revalidate_interval_secs` (default 600) are now configurable via the profile config instead of hardcoded constants.
- **Rate limit configuration** — login and register rate limits are now configurable via `[rate_limit]` section: `login_max_requests` (default 60), `login_window_secs` (default 60), `register_max_requests` (default 30), `register_window_secs` (default 60). Rate limiters are no longer static singletons; they are per-instance on `ServerState`.
- **Configurable bcrypt cost factor** — `user.bcrypt_cost` (default 10, validated range 4–31) replaces the hardcoded library default.
- **Database pool configuration** — `database.pool_idle_timeout_secs` (default 600) and `database.pool_max_lifetime_secs` (default 3600) are now configurable via the profile config.
- **X-Request-ID echo** — every HTTP response echoes the `X-Request-ID` header back so clients can correlate a response with the request that triggered it. When the client omits the header, a server-generated UUID v7 is returned.
- **Room request expiry sweep** — background task periodically expires pending room requests.
- **BIND_HOST env var** — override bind address via environment variable; default stays `127.0.0.1` for local dev safety, containers set `BIND_HOST=0.0.0.0` via docker-compose.
- **Room request E2E test suite** — 26 scenarios (`tests/test_room_requests.py`) covering the request flow, the private room gate, and concurrent accepts.

### Changed

- **Private room creation gated** — `POST /api/v1/chat/rooms` for private chat now requires an accepted room request; concurrent room creations are serialized with an advisory lock.
- **User list replaced with search** — `src/user/list.rs` removed, `src/user/search.rs` added; the user list endpoint is replaced by a search endpoint.
- **Error semantics for missing resources** — `POST /api/v1/chat/rooms` with a nonexistent target username, `POST /api/v1/chat/rooms/{room_id}/members` with a nonexistent username, and `DELETE /api/v1/chat/rooms/{room_id}/members/{user_id}` on a nonexistent room now return `404 NOT_FOUND_ERROR` instead of `400 BAD_REQUEST_ERROR`.
- **Encrypted session state errors** — state-machine errors in encrypted chat changed from `400 BadRequest` to `409 Conflict` so clients can distinguish semantic errors from transient session-state conditions.
- **Database schema** — `rooms.created_by` and `messages.sender_id` drop `NOT NULL` and use `ON DELETE SET NULL`; `users` gains `bio`, `avatar`, and `token_version BIGINT NOT NULL DEFAULT 0` columns. Databases created with previous migration checksums must be recreated.
- **Migrations consolidated** — five migration files merged into two (`001_create_users_table.sql`, `002_create_chat_tables.sql`); `003_create_room_requests_table.sql` added.
- **Shutdown logic** — shutdown coordination refactored into `main.rs` via a `tokio::sync::oneshot` channel; graceful shutdown signals every live WebSocket connection to close so the server does not stall waiting for clients to disconnect. A `shutting_down` atomic flag on `ServerState` triggers 503 responses during shutdown.
- **Configuration** — hardcoded tunables moved into profile config; config structs renamed (`AppConfigure` → `ServerConfiguration`, `ServerConfigure` → `WebConfiguration`, etc.); Dockerfile no longer hardcodes a static `config.toml` (the server auto-generates config via `load_or_create_profile()` on first startup); default bind address changed to `0.0.0.0` for container compatibility.
- **Rate limiter refactor** — rate limiters are no longer `LazyLock` static singletons; they are `SlidingWindowRateLimiter` instances on `ServerState`, constructed with configurable `max_requests` and `window_secs` parameters. IP eviction is now performed during the allow check.
- **Type and method cleanup** — types and methods renamed for consistency; `src/common/success.rs` removed (dead code); `ApiResponse` trait removed; `StandardResponse` now implements `IntoResponse` directly.
- **`sender_id` / `created_by` nullable** — `chat/message.rs`, `chat/room.rs`, and `chat/mod.rs` read `sender_id` / `created_by` / last-sender username as nullable; `list_rooms` uses `LEFT JOIN users` so rooms and message previews survive an account deletion.
- **UUID username rejection** — registration rejects usernames that parse as a UUID so the `/user/{user}` lookup stays unambiguous.
- **WebSocket validation** — message validation in `src/websocket/handler.rs` deduplicated onto `chat::validate_message_content`.
- **Middleware state** — rate limit, validation, and error middleware now use `from_fn_with_state` to access `ServerState`, replacing static state with dependency injection.
- **Test infrastructure** — `tests/run_tests.py` drains server stdout on a background thread so a full pipe cannot block the server; Python test suites aligned with the new search endpoint and error codes.
- **145 tests pass** — new tests: `TestRegisterUuidUsernameRejected`, `TestPublicProfile`, `TestUpdateProfile`, `TestChangePassword`, `TestLogout`, `TestDeleteAccount`, `TestProfileBioAvatar`, `TestAvatarUpload`, `TestDeletedAccountPreservesChat`.

### Fixed

- **Rate limiter eviction** — quiet IPs are now evicted from the sliding window rate limiter to prevent stale entries from accumulating. A periodic sweep retains only entries within the current window.
- **Missing resource errors** — requests referencing nonexistent users or rooms now correctly return `404 NOT_FOUND_ERROR` instead of `400 BAD_REQUEST_ERROR`.
- **Encrypted session state conflicts** — state-machine errors in encrypted chat now return `409 CONFLICT_ERROR` instead of `400 BadRequest`.
- **Dockerfile startup failure** — removed hardcoded `config.toml` with wrong section names (`[server]` instead of `[web]`) and missing required fields that caused startup failure.
- **Graceful shutdown** — server no longer stalls waiting for WebSocket clients to disconnect on their own during shutdown; all live connections are signaled to close immediately.

## [0.1.3] - 2026-07-26

### Added

- **End-to-end encrypted private chat** — private rooms can now be promoted to encrypted sessions. A full handshake protocol (`encrypt_request` / `encrypt_accept` / `encrypt_ready`) exchanges X25519 ephemeral public keys over the WebSocket; messages are encrypted with AES-256-GCM and stored as `encrypted_content BYTEA` in the database. The server never touches plaintext — it relays public-key material and ciphertext only.
- **Encrypted session lifecycle** — session states (`pending_states`, `ready_states`, `active_sessions`) are tracked in `ConnectionManager` memory. Both users must send `encrypt_ready` before the session becomes active.
- **30-second grace period** — when either user disconnects, a 30-second timeout starts. If the user reconnects within that window, the session resumes; otherwise messages are purged and the session is terminated.
- **Grace period reconnect flow** — on reconnect, `cancel_grace_periods_for_user` cancels pending timeouts and `check_expired_session_on_connect` detects existing encrypted rooms to offer session resumption.
- **`encrypt_session_ended` / `encrypt_session_expired` server events** — clients are notified when a session ends (partner offline) or expires (both online but no active session).
- **`base64` crate** — added as a dependency for ciphertext encoding.
- **Encrypted room E2E test suite** — 16 tests (E1–E16) covering handshake, message relay, grace period recovery, timeout cleanup, reconnect flow, and coexistence with non-encrypted rooms.

### Changed

- **`CreateRoomRequest`** — added optional `is_encrypted` field to create encrypted private rooms.
- **Room creation query** — `create_private_room` now filters `is_encrypted = false` to avoid returning existing encrypted rooms as regular rooms.
- **Room list response** — last-message preview is omitted for encrypted rooms (`content` is always `null`).
- **Message history response** — encrypted rooms return `ciphertext` (base64-encoded) instead of `content` in `GET /api/v1/chat/rooms/{room_id}/messages`.
- **Broadcast dispatch** — encrypted messages are broadcast as `new_encrypted_message` and are never decrypted or inspected server-side.

## [0.1.2] - 2026-07-22

### Added

- **WebSocket message sending** — `send_message` client message type persisted and broadcast as `new_message`; `message_sent` acknowledgement sent to sender.
- **WebSocket rate limiting** — 30 application messages per 10 seconds per connection with `error` response on exceed.
- **WebSocket token re-validation** — periodic JWT and account-active check every 10 minutes; expired or deactivated sessions are disconnected.
- **Message content validation** — control character stripping (except newlines), empty rejection, 5000-byte maximum.
- **`NOT_FOUND_ERROR` error code** — `404` returned for room-not-found scenarios (replaces `400` in room detail; new in message listing).

### Changed

- **WebSocket authentication** — moved from query parameter (`?token=`) to `Authorization: Bearer <token>` header, consistent with the HTTP API.
- **Room detail endpoint** — `GET /api/v1/chat/rooms/{room_id}` now returns `404 NOT_FOUND_ERROR` instead of `400 BAD_REQUEST_ERROR` when the room does not exist.
- **Message listing endpoint** — `GET /api/v1/chat/rooms/{room_id}/messages` now validates room existence before checking membership, returning `404` for unknown rooms.
- **Broadcast channel capacity** — increased from 256 to 1024 to reduce lag under heavy load.
- **Typing indicator routing** — a user's own typing events are no longer echoed back to them.
- **Connection cleanup** — room subscriptions are explicitly cancelled on disconnect instead of relying on implicit drop.

### Fixed

- **Broadcast lag handling** — `RecvError::Lagged` is now logged and handled gracefully without breaking the forward task.

## [0.1.1] - 2026-07-14

### Added

- **Real-time messaging via WebSocket** — `GET /websocket` with JWT auth, auto-subscribe to rooms, `new_message` push on send.
- **Presence system** — `user_online` / `user_offline` events, multi-device connection counting.
- **Typing indicators** — `typing` event relayed to all room members.
- **Heartbeat** — protocol-level PING every 30s, dead connection detection.

## [0.1.0] - 2026-06-30

### Added

- **User registration and login** — create accounts with username/email/password (bcrypt hashed), authenticate via JWT (HS256, 24h expiry).
- **Private chat** — idempotent two-person room creation, send and retrieve messages with cursor-based pagination.
- **Group chat** — multi-person rooms with admin/member role model, member add/kick/leave with auto-promotion of last admin and auto-deletion of empty rooms.
- **Rate limiting (auth endpoints)** — sliding-window per-IP rate limiting on login (60/min) and register (30/min) with `X-Forwarded-For` / `X-Real-IP` detection; prevents brute-force and account creation abuse.
- **Request validation middleware** — body size limits (1 MB production, 10 MB development), Content-Type enforcement, JSON parsing validation, field-level checks for registration and login.
- **JWT authentication middleware** — Bearer token extraction, validation, expiry check, user-lookup guard for all chat and protected endpoints; tokens never logged in production.
- **Global error handling** — panic recovery, 404 catch-all, structured JSON error responses with UUID v7 `response_id`; internal error details hidden in production.
- **Request tracing middleware** — structured logging for every request.
- **Standard API response envelope** — uniform `{ response_id, error_code, message, data }` format across all endpoints.
- **Health check endpoint** — `GET /health` with PostgreSQL connectivity verification via `SELECT 1`.
- **Interactive console** — rustyline-based CLI (commands: `stop`, `help`) for server lifecycle management.
- **Dual-mode configuration** — development (human-readable logs, seed admin user, verbose errors) vs production (JSON logs, disabled console, generic error messages); config via `.env` with sensible defaults, auto-generation of `~/.baihua/config.toml`.
- **Docker deployment** — production and development Docker Compose profiles, database health check gating, Docker HEALTHCHECK.
- **CI workflows** — GitHub Actions for code quality checks (`cargo fmt`, `clippy`) and Docker-based integration tests.
- **Integration test suite** — Python pytest-based HTTP tests covering user, chat, and health endpoints.
