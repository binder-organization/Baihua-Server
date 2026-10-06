use crate::ServerState;
use crate::chat::{find_room_by_id, is_room_member};
use crate::common::StandardResponse;
use crate::common::error::ErrorResponse;
use crate::middleware::authenticate::AuthenticatedUser;
use axum::Extension;
use axum::body::{Body, Bytes};
use axum::extract::DefaultBodyLimit;
use axum::extract::multipart::Field;
use axum::extract::{Multipart, Path, State};
use axum::http::{StatusCode, header};
use axum::response::Response;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use chrono::Utc;
use futures_util::stream;
use serde_json::json;
use sha2::{Digest, Sha256};
use sqlx::Row;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tracing::warn;
use uuid::Uuid;

struct PendingUploadedFile {
    path: PathBuf,
    content_hash: String,
    byte_size: u64,
}

struct ActiveFileUpload {
    active_uploads: Arc<std::sync::Mutex<std::collections::HashMap<Uuid, u32>>>,
    user_id: Uuid,
}

impl Drop for ActiveFileUpload {
    fn drop(&mut self) {
        let Ok(mut active_uploads) = self.active_uploads.lock() else {
            return;
        };
        if let Some(active_count) = active_uploads.get_mut(&self.user_id) {
            *active_count = active_count.saturating_sub(1);
            if *active_count == 0 {
                active_uploads.remove(&self.user_id);
            }
        }
    }
}

pub async fn upload_file(
    State(state): State<Arc<ServerState>>,
    Extension(authenticated_user): Extension<AuthenticatedUser>,
    Path(room_id): Path<Uuid>,
    mut multipart: Multipart,
) -> Result<StandardResponse, ErrorResponse> {
    let _active_upload = begin_file_upload(&state, authenticated_user.user_id)?;
    find_room_by_id(&state.pool, room_id).await?;
    if !is_room_member(&state.pool, room_id, authenticated_user.user_id).await? {
        return Err(ErrorResponse::Forbidden(
            "You are not a member of this room.".to_string(),
        ));
    }
    let current_file_bytes = sqlx::query_scalar::<_, i64>(
        "SELECT COALESCE(SUM(file_attachments.byte_size), 0)::BIGINT FROM file_attachments JOIN messages ON messages.id = file_attachments.message_id WHERE messages.sender_id = $1",
    )
    .bind(authenticated_user.user_id)
    .fetch_one(&state.pool)
    .await?;
    let available_quota_before_upload = state
        .configuration
        .file
        .per_user_quota_bytes
        .saturating_sub(current_file_bytes.max(0) as u64);
    if available_quota_before_upload == 0 {
        return Err(ErrorResponse::PayloadTooLarge(
            "File storage quota exceeded.".to_string(),
        ));
    }
    let upload_idle_timeout =
        Duration::from_secs(state.configuration.file.upload_idle_timeout_secs);

    let mut supplied_hash = None;
    let mut encrypted_metadata = None;
    let mut uploaded_file: Option<PendingUploadedFile> = None;
    let mut original_name = None;
    let mut media_type = None;

    loop {
        let next_field =
            match tokio::time::timeout(upload_idle_timeout, multipart.next_field()).await {
                Ok(Ok(next_field)) => next_field,
                Ok(Err(error)) => {
                    if let Some(uploaded_file) = &uploaded_file {
                        remove_file_if_exists(&uploaded_file.path).await;
                    }
                    return Err(ErrorResponse::BadRequest(format!(
                        "Failed to read multipart field: {error}."
                    )));
                }
                Err(_) => {
                    if let Some(uploaded_file) = &uploaded_file {
                        remove_file_if_exists(&uploaded_file.path).await;
                    }
                    return Err(upload_stalled_error(
                        state.configuration.file.upload_idle_timeout_secs,
                    ));
                }
            };
        let Some(mut field) = next_field else {
            break;
        };
        match field.name() {
            Some("sha256") => {
                supplied_hash = Some(
                    match read_bounded_text(&mut field, 64, "File hash", upload_idle_timeout).await
                    {
                        Ok(hash) => hash,
                        Err(error @ ErrorResponse::RequestTimeout(_)) => {
                            if let Some(uploaded_file) = &uploaded_file {
                                remove_file_if_exists(&uploaded_file.path).await;
                            }
                            return Err(error);
                        }
                        Err(error) => {
                            if let Some(uploaded_file) = &uploaded_file {
                                remove_file_if_exists(&uploaded_file.path).await;
                            }
                            return Err(ErrorResponse::BadRequest(format!(
                                "Failed to read file hash: {error}."
                            )));
                        }
                    },
                );
            }
            Some("encrypted_metadata") => {
                encrypted_metadata = Some(
                    match read_bounded_text(
                        &mut field,
                        8192,
                        "Encrypted metadata",
                        upload_idle_timeout,
                    )
                    .await
                    {
                        Ok(metadata) => metadata,
                        Err(error @ ErrorResponse::RequestTimeout(_)) => {
                            if let Some(uploaded_file) = &uploaded_file {
                                remove_file_if_exists(&uploaded_file.path).await;
                            }
                            return Err(error);
                        }
                        Err(error) => {
                            if let Some(uploaded_file) = &uploaded_file {
                                remove_file_if_exists(&uploaded_file.path).await;
                            }
                            return Err(ErrorResponse::BadRequest(format!(
                                "Failed to read encrypted metadata: {error}."
                            )));
                        }
                    },
                );
            }
            Some("file") => {
                if uploaded_file.is_some() {
                    if let Some(uploaded_file) = &uploaded_file {
                        remove_file_if_exists(&uploaded_file.path).await;
                    }
                    return Err(ErrorResponse::Validation(
                        "Only one file is allowed.".to_string(),
                    ));
                }
                original_name = field.file_name().map(str::to_string);
                media_type = field.content_type().map(str::to_string);
                uploaded_file = Some(
                    receive_uploaded_file(&state, &mut field, available_quota_before_upload)
                        .await?,
                );
            }
            _ => {}
        }
    }

    let uploaded_file = uploaded_file
        .ok_or_else(|| ErrorResponse::Validation("Upload field 'file' is required.".to_string()))?;
    if uploaded_file.byte_size == 0 {
        remove_file_if_exists(&uploaded_file.path).await;
        return Err(ErrorResponse::Validation(
            "File cannot be empty.".to_string(),
        ));
    }
    let supplied_hash = supplied_hash.ok_or_else(|| {
        ErrorResponse::Validation("Upload field 'sha256' is required.".to_string())
    })?;
    if supplied_hash.len() != 64
        || !supplied_hash
            .bytes()
            .all(|character| character.is_ascii_hexdigit())
    {
        remove_file_if_exists(&uploaded_file.path).await;
        return Err(ErrorResponse::Validation(
            "File hash must be a SHA-256 hexadecimal string.".to_string(),
        ));
    }
    if !uploaded_file
        .content_hash
        .eq_ignore_ascii_case(&supplied_hash)
    {
        remove_file_if_exists(&uploaded_file.path).await;
        return Err(ErrorResponse::Validation(
            "File hash does not match uploaded bytes.".to_string(),
        ));
    }

    let mut transaction = match state.pool.begin().await {
        Ok(transaction) => transaction,
        Err(error) => {
            remove_file_if_exists(&uploaded_file.path).await;
            return Err(error.into());
        }
    };
    let room =
        match sqlx::query("SELECT is_group, is_encrypted FROM rooms WHERE id = $1 FOR UPDATE")
            .bind(room_id)
            .fetch_optional(&mut *transaction)
            .await
        {
            Ok(Some(room)) => room,
            Ok(None) => {
                remove_file_if_exists(&uploaded_file.path).await;
                return Err(ErrorResponse::NotFound("Room not found.".to_string()));
            }
            Err(error) => {
                remove_file_if_exists(&uploaded_file.path).await;
                return Err(error.into());
            }
        };
    let still_member = match sqlx::query(
        "SELECT 1 FROM room_members WHERE room_id = $1 AND user_id = $2 FOR UPDATE",
    )
    .bind(room_id)
    .bind(authenticated_user.user_id)
    .fetch_optional(&mut *transaction)
    .await
    {
        Ok(member) => member.is_some(),
        Err(error) => {
            remove_file_if_exists(&uploaded_file.path).await;
            return Err(error.into());
        }
    };
    if !still_member {
        remove_file_if_exists(&uploaded_file.path).await;
        return Err(ErrorResponse::Forbidden(
            "You are not a member of this room.".to_string(),
        ));
    }
    let encrypted: bool = room.get("is_encrypted");
    if encrypted
        && (room.get::<bool, _>("is_group") || !state.connection_manager.is_session_active(room_id))
    {
        remove_file_if_exists(&uploaded_file.path).await;
        return Err(ErrorResponse::Conflict(
            "No active encrypted session in this room.".to_string(),
        ));
    }
    let encrypted_metadata = if encrypted {
        let metadata = match encrypted_metadata {
            Some(metadata) => metadata,
            None => {
                remove_file_if_exists(&uploaded_file.path).await;
                return Err(ErrorResponse::Validation(
                    "Encrypted metadata is required for encrypted files.".to_string(),
                ));
            }
        };
        if metadata.len() > 8192
            || BASE64
                .decode(metadata.as_bytes())
                .map_or(true, |value| value.len() < 28)
        {
            remove_file_if_exists(&uploaded_file.path).await;
            return Err(ErrorResponse::Validation(
                "Encrypted metadata must be base64-encoded ciphertext.".to_string(),
            ));
        }
        Some(metadata)
    } else {
        None
    };
    if encrypted && uploaded_file.byte_size < 28 {
        remove_file_if_exists(&uploaded_file.path).await;
        return Err(ErrorResponse::Validation(
            "Encrypted file is too short.".to_string(),
        ));
    }
    let original_name = if encrypted {
        "encrypted-file".to_string()
    } else {
        let name = original_name.unwrap_or_default();
        if name.is_empty()
            || name.len() > 255
            || name
                .chars()
                .any(|character| character.is_control() || character == '/' || character == '\\')
        {
            remove_file_if_exists(&uploaded_file.path).await;
            return Err(ErrorResponse::Validation(
                "File name is invalid.".to_string(),
            ));
        }
        name
    };
    let media_type = if encrypted {
        "application/octet-stream".to_string()
    } else {
        let supplied_type = media_type.unwrap_or_else(|| "application/octet-stream".to_string());
        if supplied_type.len() > 127
            || !supplied_type.is_ascii()
            || supplied_type.chars().any(char::is_control)
        {
            remove_file_if_exists(&uploaded_file.path).await;
            return Err(ErrorResponse::Validation(
                "File media type is invalid.".to_string(),
            ));
        }
        supplied_type
    };

    if let Err(error) = sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(format!("file-user:{}", authenticated_user.user_id))
        .execute(&mut *transaction)
        .await
    {
        remove_file_if_exists(&uploaded_file.path).await;
        return Err(error.into());
    }
    let current_file_bytes = match sqlx::query_scalar::<_, i64>(
        "SELECT COALESCE(SUM(file_attachments.byte_size), 0)::BIGINT FROM file_attachments JOIN messages ON messages.id = file_attachments.message_id WHERE messages.sender_id = $1",
    )
    .bind(authenticated_user.user_id)
    .fetch_one(&mut *transaction)
    .await
    {
        Ok(current_file_bytes) => current_file_bytes,
        Err(error) => {
            remove_file_if_exists(&uploaded_file.path).await;
            return Err(error.into());
        }
    };
    let available_quota = state
        .configuration
        .file
        .per_user_quota_bytes
        .saturating_sub(current_file_bytes.max(0) as u64);
    if uploaded_file.byte_size > available_quota {
        remove_file_if_exists(&uploaded_file.path).await;
        return Err(ErrorResponse::PayloadTooLarge(
            "File storage quota exceeded.".to_string(),
        ));
    }

    let actual_hash = uploaded_file.content_hash.clone();
    if let Err(error) = sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(format!("stored-file:{actual_hash}"))
        .execute(&mut *transaction)
        .await
    {
        remove_file_if_exists(&uploaded_file.path).await;
        return Err(error.into());
    }
    let file_path = state.files_directory.join(&actual_hash);
    let target_exists = match tokio::fs::try_exists(&file_path).await {
        Ok(target_exists) => target_exists,
        Err(error) => {
            remove_file_if_exists(&uploaded_file.path).await;
            return Err(ErrorResponse::InternalError(format!(
                "Failed to inspect stored file: {error}."
            )));
        }
    };
    let newly_saved = if target_exists {
        remove_file_if_exists(&uploaded_file.path).await;
        false
    } else {
        match tokio::fs::rename(&uploaded_file.path, &file_path).await {
            Ok(()) => {}
            Err(error) => {
                remove_file_if_exists(&uploaded_file.path).await;
                return Err(ErrorResponse::InternalError(format!(
                    "Failed to save file: {error}."
                )));
            }
        }
        true
    };

    let message_id = Uuid::now_v7();
    let now = Utc::now();
    let save_result = async {
        sqlx::query("INSERT INTO stored_files (content_hash, byte_size) VALUES ($1, $2) ON CONFLICT (content_hash) DO NOTHING")
            .bind(&actual_hash).bind(uploaded_file.byte_size as i64)
            .execute(&mut *transaction).await?;
        sqlx::query("INSERT INTO messages (id, room_id, sender_id, content, created_at) VALUES ($1, $2, $3, NULL, $4)")
            .bind(message_id).bind(room_id).bind(authenticated_user.user_id).bind(now)
            .execute(&mut *transaction).await?;
        sqlx::query("INSERT INTO file_attachments (message_id, content_hash, original_name, media_type, byte_size, encrypted, encrypted_metadata) VALUES ($1, $2, $3, $4, $5, $6, $7)")
            .bind(message_id).bind(&actual_hash).bind(&original_name).bind(&media_type)
            .bind(uploaded_file.byte_size as i64).bind(encrypted).bind(&encrypted_metadata)
            .execute(&mut *transaction).await?;
        transaction.commit().await?;
        Ok::<(), ErrorResponse>(())
    }.await;
    if let Err(error) = save_result {
        if newly_saved
            && let Err(cleanup_error) =
                remove_newly_saved_file_if_unreferenced(&state, &actual_hash, &file_path).await
        {
            warn!("Failed to inspect file after database write error: {cleanup_error}");
        }
        return Err(error);
    }

    let attachment = json!({
        "id": message_id,
        "room_id": room_id,
        "sender_id": authenticated_user.user_id,
        "content": null,
        "file": {
            "sha256": actual_hash,
            "name": original_name,
            "media_type": media_type,
            "byte_size": uploaded_file.byte_size,
            "encrypted": encrypted,
            "encrypted_metadata": encrypted_metadata,
            "download_url": format!("/api/v1/chat/rooms/{room_id}/files/{message_id}"),
        },
        "created_at": now.to_rfc3339(),
    });
    state.connection_manager.broadcast(
        room_id,
        &json!({"type": "new_file", "data": attachment}).to_string(),
    );
    Ok(StandardResponse::success(
        StatusCode::CREATED,
        "File sent successfully.".to_string(),
        attachment,
    ))
}

pub async fn download_file(
    State(state): State<Arc<ServerState>>,
    Extension(authenticated_user): Extension<AuthenticatedUser>,
    Path((room_id, message_id)): Path<(Uuid, Uuid)>,
) -> Result<Response, ErrorResponse> {
    find_room_by_id(&state.pool, room_id).await?;
    if !is_room_member(&state.pool, room_id, authenticated_user.user_id).await? {
        return Err(ErrorResponse::Forbidden(
            "You are not a member of this room.".to_string(),
        ));
    }
    let row = sqlx::query(
        "SELECT file_attachments.content_hash, file_attachments.original_name, file_attachments.media_type, file_attachments.byte_size, file_attachments.encrypted FROM file_attachments JOIN messages ON messages.id = file_attachments.message_id WHERE messages.id = $1 AND messages.room_id = $2",
    )
    .bind(message_id).bind(room_id).fetch_optional(&state.pool).await?
    .ok_or_else(|| ErrorResponse::NotFound("File not found.".to_string()))?;
    let content_hash = row.get::<String, _>("content_hash");
    let original_name = row.get::<String, _>("original_name");
    let media_type = row.get::<String, _>("media_type");
    let byte_size = row.get::<i64, _>("byte_size");
    let encrypted = row.get::<bool, _>("encrypted");
    let file = tokio::fs::File::open(state.files_directory.join(content_hash.trim()))
        .await
        .map_err(|error| match error.kind() {
            std::io::ErrorKind::NotFound => ErrorResponse::NotFound("File not found.".to_string()),
            _ => ErrorResponse::InternalError(format!("Failed to read file: {error}.")),
        })?;
    let content_type = if encrypted {
        "application/octet-stream".to_string()
    } else {
        media_type
    };
    let content_disposition = format!(
        "attachment; filename*=UTF-8''{}",
        percent_encode_filename(&original_name)
    );
    let stream = stream_file(
        file,
        state.configuration.file.transfer_rate_limit_enabled,
        state.configuration.file.download_mibps,
    );
    let body = Body::from_stream(stream);
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, content_type)
        .header(header::CONTENT_DISPOSITION, content_disposition)
        .header(header::CONTENT_LENGTH, byte_size)
        .header(header::CACHE_CONTROL, "no-store")
        .header("x-content-type-options", "nosniff")
        .body(body)
        .map_err(|error| {
            ErrorResponse::InternalError(format!("Failed to build response: {error}."))
        })
}

pub(crate) async fn delete_unreferenced_files(
    state: &ServerState,
    hashes: Vec<String>,
) -> Result<(), ErrorResponse> {
    for hash in hashes {
        let mut transaction = state.pool.begin().await?;
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
            .bind(format!("stored-file:{}", hash.trim()))
            .execute(&mut *transaction)
            .await?;
        let reference_count = sqlx::query_scalar::<_, i64>(
            "SELECT reference_count FROM stored_files WHERE content_hash = $1 FOR UPDATE",
        )
        .bind(&hash)
        .fetch_optional(&mut *transaction)
        .await?;
        if reference_count != Some(0) {
            continue;
        }
        match tokio::fs::remove_file(state.files_directory.join(hash.trim())).await {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                warn!("Failed to remove unreferenced file {}: {}", hash, error);
                continue;
            }
        }
        sqlx::query("DELETE FROM stored_files WHERE content_hash = $1 AND reference_count = 0")
            .bind(&hash)
            .execute(&mut *transaction)
            .await?;
        transaction.commit().await?;
    }
    Ok(())
}

pub(crate) async fn cleanup_pending_files(state: &ServerState) -> Result<(), ErrorResponse> {
    let hashes = sqlx::query_scalar::<_, String>(
        "SELECT content_hash FROM stored_files WHERE reference_count = 0",
    )
    .fetch_all(&state.pool)
    .await?;
    delete_unreferenced_files(state, hashes).await?;
    cleanup_orphaned_completed_files(state).await?;
    cleanup_temporary_uploads(state).await;
    Ok(())
}

pub(crate) async fn delete_room_with_files(
    state: &ServerState,
    room_id: Uuid,
) -> Result<(), ErrorResponse> {
    let mut transaction = state.pool.begin().await?;
    sqlx::query("SELECT 1 FROM rooms WHERE id = $1 FOR UPDATE")
        .bind(room_id)
        .fetch_optional(&mut *transaction)
        .await?;
    let hashes = sqlx::query_scalar::<_, String>(
        "SELECT DISTINCT file_attachments.content_hash FROM file_attachments JOIN messages ON messages.id = file_attachments.message_id WHERE messages.room_id = $1",
    ).bind(room_id).fetch_all(&mut *transaction).await?;
    sqlx::query("DELETE FROM rooms WHERE id = $1")
        .bind(room_id)
        .execute(&mut *transaction)
        .await?;
    transaction.commit().await?;
    delete_unreferenced_files(state, hashes).await
}

// TODO: A client can keep one of these slots open indefinitely by sending a chunk within every
// idle window, so the remaining controls are this per-user slot count and the bandwidth limit,
// which is disabled by default.
fn begin_file_upload(
    state: &ServerState,
    user_id: Uuid,
) -> Result<ActiveFileUpload, ErrorResponse> {
    let mut active_uploads = state.active_file_uploads.lock().map_err(|error| {
        ErrorResponse::InternalError(format!("Failed to track active file uploads: {error}."))
    })?;
    let active_count = active_uploads.entry(user_id).or_insert(0);
    if *active_count >= state.configuration.file.maximum_concurrent_uploads_per_user {
        return Err(ErrorResponse::TooManyRequests(
            "Too many file uploads are already in progress.".to_string(),
        ));
    }
    *active_count += 1;
    drop(active_uploads);
    Ok(ActiveFileUpload {
        active_uploads: state.active_file_uploads.clone(),
        user_id,
    })
}

async fn read_bounded_text(
    field: &mut Field<'_>,
    maximum_bytes: usize,
    field_label: &str,
    upload_idle_timeout: Duration,
) -> Result<String, ErrorResponse> {
    let mut value = Vec::with_capacity(maximum_bytes.min(1024));
    while let Some(chunk) = tokio::time::timeout(upload_idle_timeout, field.chunk())
        .await
        .map_err(|_| upload_stalled_error(upload_idle_timeout.as_secs()))?
        .map_err(|error| {
            ErrorResponse::BadRequest(format!(
                "Failed to read {}: {error}.",
                field_label.to_lowercase()
            ))
        })?
    {
        if value.len().saturating_add(chunk.len()) > maximum_bytes {
            return Err(ErrorResponse::Validation(format!(
                "{field_label} exceeds {maximum_bytes} bytes."
            )));
        }
        value.extend_from_slice(&chunk);
    }
    String::from_utf8(value).map_err(|error| {
        ErrorResponse::Validation(format!("{field_label} is not valid UTF-8: {error}."))
    })
}

pub(crate) fn upload_body_limit_layer(state: &ServerState) -> DefaultBodyLimit {
    let overhead = 1024 * 1024;
    let max_bytes = state
        .configuration
        .file
        .max_bytes
        .saturating_add(overhead)
        .min(usize::MAX as u64) as usize;
    DefaultBodyLimit::max(max_bytes)
}

async fn receive_uploaded_file(
    state: &ServerState,
    field: &mut Field<'_>,
    available_quota_before_upload: u64,
) -> Result<PendingUploadedFile, ErrorResponse> {
    let temporary_path = state
        .file_uploads_directory
        .join(format!("{}.part", Uuid::now_v7()));
    let mut file = tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary_path)
        .await
        .map_err(|error| {
            ErrorResponse::InternalError(format!("Failed to create upload file: {error}."))
        })?;
    let mut hasher = Sha256::new();
    let mut byte_size = 0_u64;
    let started_at = Instant::now();
    loop {
        let chunk = match tokio::time::timeout(
            Duration::from_secs(state.configuration.file.upload_idle_timeout_secs),
            field.chunk(),
        )
        .await
        {
            Ok(Ok(Some(chunk))) => chunk,
            Ok(Ok(None)) => break,
            Ok(Err(error)) => {
                remove_file_if_exists(&temporary_path).await;
                return Err(ErrorResponse::BadRequest(format!(
                    "Failed to read file: {error}."
                )));
            }
            Err(_) => {
                remove_file_if_exists(&temporary_path).await;
                return Err(upload_stalled_error(
                    state.configuration.file.upload_idle_timeout_secs,
                ));
            }
        };
        byte_size = byte_size.saturating_add(chunk.len() as u64);
        if byte_size > state.configuration.file.max_bytes {
            remove_file_if_exists(&temporary_path).await;
            return Err(ErrorResponse::PayloadTooLarge(
                "File exceeds the configured file size.".to_string(),
            ));
        }
        if byte_size > available_quota_before_upload {
            remove_file_if_exists(&temporary_path).await;
            return Err(ErrorResponse::PayloadTooLarge(
                "File storage quota exceeded.".to_string(),
            ));
        }
        if let Err(error) = file.write_all(&chunk).await {
            remove_file_if_exists(&temporary_path).await;
            return Err(ErrorResponse::InternalError(format!(
                "Failed to write upload file: {error}."
            )));
        }
        hasher.update(&chunk);
        throttle_transfer(
            state.configuration.file.transfer_rate_limit_enabled,
            state.configuration.file.upload_mibps,
            byte_size,
            started_at,
        )
        .await;
    }
    if let Err(error) = file.flush().await {
        remove_file_if_exists(&temporary_path).await;
        return Err(ErrorResponse::InternalError(format!(
            "Failed to flush upload file: {error}."
        )));
    }
    if let Err(error) = file.sync_all().await {
        remove_file_if_exists(&temporary_path).await;
        return Err(ErrorResponse::InternalError(format!(
            "Failed to synchronize upload file: {error}."
        )));
    }
    Ok(PendingUploadedFile {
        path: temporary_path,
        content_hash: format!("{:x}", hasher.finalize()),
        byte_size,
    })
}

fn upload_stalled_error(timeout_seconds: u64) -> ErrorResponse {
    ErrorResponse::RequestTimeout(format!(
        "The file upload received no data for {timeout_seconds} seconds."
    ))
}

async fn cleanup_orphaned_completed_files(state: &ServerState) -> Result<(), ErrorResponse> {
    let stored_hashes: HashSet<String> =
        sqlx::query_scalar::<_, String>("SELECT content_hash FROM stored_files")
            .fetch_all(&state.pool)
            .await?
            .into_iter()
            .map(|hash| hash.trim().to_string())
            .collect();
    let mut entries = tokio::fs::read_dir(&state.files_directory)
        .await
        .map_err(|error| ErrorResponse::InternalError(format!("Failed to read files: {error}.")))?;
    while let Some(entry) = entries.next_entry().await.map_err(|error| {
        ErrorResponse::InternalError(format!("Failed to read file entry: {error}."))
    })? {
        let file_type = entry.file_type().await.map_err(|error| {
            ErrorResponse::InternalError(format!("Failed to read file type: {error}."))
        })?;
        if !file_type.is_file() {
            continue;
        }
        let filename = entry.file_name().to_string_lossy().to_string();
        let is_hash = filename.len() == 64
            && filename
                .bytes()
                .all(|character| character.is_ascii_hexdigit());
        if is_hash && !stored_hashes.contains(&filename) {
            remove_file_if_exists(&entry.path()).await;
        }
    }
    Ok(())
}

async fn cleanup_temporary_uploads(state: &ServerState) {
    let mut entries = match tokio::fs::read_dir(&state.file_uploads_directory).await {
        Ok(entries) => entries,
        Err(error) => {
            warn!(
                "Failed to read temporary upload directory {}: {}",
                state.file_uploads_directory.display(),
                error
            );
            return;
        }
    };
    loop {
        match entries.next_entry().await {
            Ok(Some(entry)) => {
                let path = entry.path();
                if path.extension().and_then(|extension| extension.to_str()) == Some("part") {
                    remove_file_if_exists(&path).await;
                }
            }
            Ok(None) => break,
            Err(error) => {
                warn!("Failed to read temporary upload entry: {}", error);
                break;
            }
        }
    }
}

fn stream_file(
    file: tokio::fs::File,
    limit_enabled: bool,
    mibps: u32,
) -> impl futures_util::Stream<Item = Result<Bytes, std::io::Error>> {
    stream::unfold(
        (file, vec![0_u8; 64 * 1024], 0_u64, Instant::now()),
        move |(mut file, mut buffer, mut sent_bytes, started_at)| async move {
            match file.read(&mut buffer).await {
                Ok(0) => None,
                Ok(read_bytes) => {
                    sent_bytes = sent_bytes.saturating_add(read_bytes as u64);
                    throttle_transfer(limit_enabled, mibps, sent_bytes, started_at).await;
                    let chunk = Bytes::copy_from_slice(&buffer[..read_bytes]);
                    Some((Ok(chunk), (file, buffer, sent_bytes, started_at)))
                }
                Err(error) => Some((Err(error), (file, buffer, sent_bytes, started_at))),
            }
        },
    )
}

async fn throttle_transfer(
    limit_enabled: bool,
    mibps: u32,
    transferred_bytes: u64,
    started_at: Instant,
) {
    if !limit_enabled || mibps == 0 {
        return;
    }
    let bits_per_second = mibps as u128 * 1024 * 1024;
    let expected_nanos = transferred_bytes as u128 * 8 * 1_000_000_000 / bits_per_second;
    let elapsed_nanos = started_at.elapsed().as_nanos();
    if expected_nanos > elapsed_nanos {
        let sleep_nanos = (expected_nanos - elapsed_nanos).min(u64::MAX as u128) as u64;
        tokio::time::sleep(std::time::Duration::from_nanos(sleep_nanos)).await;
    }
}

async fn remove_file_if_exists(path: &PathBuf) {
    match tokio::fs::remove_file(path).await {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => warn!("Failed to remove file {}: {}", path.display(), error),
    }
}

async fn remove_newly_saved_file_if_unreferenced(
    state: &ServerState,
    content_hash: &str,
    file_path: &PathBuf,
) -> Result<(), ErrorResponse> {
    let mut transaction = state.pool.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(format!("stored-file:{content_hash}"))
        .execute(&mut *transaction)
        .await?;
    let stored_file_exists: bool =
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM stored_files WHERE content_hash = $1)")
            .bind(content_hash)
            .fetch_one(&mut *transaction)
            .await?;
    if !stored_file_exists {
        remove_file_if_exists(file_path).await;
    }
    transaction.commit().await?;
    Ok(())
}

fn percent_encode_filename(filename: &str) -> String {
    let mut encoded = String::new();
    for byte in filename.as_bytes() {
        match *byte {
            b'A'..=b'Z'
            | b'a'..=b'z'
            | b'0'..=b'9'
            | b'!'
            | b'#'
            | b'$'
            | b'&'
            | b'+'
            | b'-'
            | b'.'
            | b'^'
            | b'_'
            | b'`'
            | b'|'
            | b'~' => encoded.push(*byte as char),
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
}
