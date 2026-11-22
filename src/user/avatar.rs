use super::profile;
use crate::ServerState;
use crate::common::StandardResponse;
use crate::common::error::ErrorResponse;
use crate::middleware::authenticate::AuthenticatedUser;
use axum::Extension;
use axum::extract::multipart::{Field, Multipart};
use axum::extract::{Path, State};
use axum::http::header;
use axum::response::{IntoResponse, Response};
use image::{ImageFormat, ImageReader};
use lazy_static::lazy_static;
use regex::Regex;
use sqlx::Row;
use std::io::Cursor;
use std::sync::Arc;
use tracing::warn;
use uuid::Uuid;

const AVATAR_URL_PREFIX: &str = "/static/avatars/";

fn format_for_content_type(content_type: &str) -> Option<ImageFormat> {
    match content_type {
        "image/jpeg" => Some(ImageFormat::Jpeg),
        "image/png" => Some(ImageFormat::Png),
        "image/gif" => Some(ImageFormat::Gif),
        "image/webp" => Some(ImageFormat::WebP),
        _ => None,
    }
}

fn process_image(
    bytes: Vec<u8>,
    claimed_format: ImageFormat,
    maximum_source_pixels: u64,
    maximum_dimension: u32,
) -> Result<Vec<u8>, ErrorResponse> {
    let actual_format = image::guess_format(&bytes)
        .map_err(|_| ErrorResponse::Validation("Avatar image content is invalid.".to_string()))?;
    if actual_format != claimed_format {
        return Err(ErrorResponse::Validation(
            "Avatar image content does not match its media type.".to_string(),
        ));
    }
    let dimensions = ImageReader::with_format(Cursor::new(&bytes), actual_format)
        .into_dimensions()
        .map_err(|_| ErrorResponse::Validation("Avatar image content is invalid.".to_string()))?;
    if dimensions.0 == 0
        || dimensions.1 == 0
        || u64::from(dimensions.0) * u64::from(dimensions.1) > maximum_source_pixels
    {
        return Err(ErrorResponse::Validation(
            "Avatar image dimensions exceed the configured limit.".to_string(),
        ));
    }
    let mut reader = ImageReader::with_format(Cursor::new(bytes), actual_format);
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(dimensions.0);
    limits.max_image_height = Some(dimensions.1);
    limits.max_alloc = Some(128 * 1024 * 1024);
    reader.limits(limits);
    let decoded = reader
        .decode()
        .map_err(|_| ErrorResponse::Validation("Avatar image content is invalid.".to_string()))?;
    // Re-encoding a bounded image drops metadata and keeps supplied bytes out of public storage.
    let processed = decoded.thumbnail(maximum_dimension, maximum_dimension);
    let mut output = Cursor::new(Vec::new());
    processed
        .write_to(&mut output, ImageFormat::Png)
        .map_err(|error| {
            warn!("Failed to encode avatar image: {error}");
            ErrorResponse::ServiceUnavailable("Avatar upload failed. Please try again.".to_string())
        })?;
    Ok(output.into_inner())
}

fn upload_retry_error(error: impl std::fmt::Display) -> ErrorResponse {
    warn!("Avatar upload failed: {error}");
    ErrorResponse::ServiceUnavailable("Avatar upload failed. Please try again.".to_string())
}

// Upload an avatar image file, store it on the local filesystem and point
// the user's avatar column at the public static URL serving that file.
pub async fn upload_avatar(
    State(state): State<Arc<ServerState>>,
    Extension(auth_user): Extension<AuthenticatedUser>,
    mut multipart: Multipart,
) -> Result<StandardResponse, ErrorResponse> {
    // Locate the 'file' part among the multipart fields. The field must be
    // processed inside the loop because a Field borrows the Multipart stream
    // mutably for its whole lifetime.
    while let Some(mut field) = multipart.next_field().await.map_err(|err| {
        ErrorResponse::BadRequest(format!("Failed to read multipart field: {}.", err))
    })? {
        if field.name() == Some("file") {
            return process_avatar_field(&state, &auth_user, &mut field).await;
        }
    }

    Err(ErrorResponse::Validation(
        "Upload field 'file' is required.".to_string(),
    ))
}

async fn process_avatar_field(
    state: &Arc<ServerState>,
    auth_user: &AuthenticatedUser,
    field: &mut Field<'_>,
) -> Result<StandardResponse, ErrorResponse> {
    let content_type = match field.content_type() {
        Some(content_type) => content_type,
        None => {
            return Err(ErrorResponse::Validation(
                "Avatar must be a JPEG, PNG, GIF or WebP image.".to_string(),
            ));
        }
    };
    let claimed_format = match format_for_content_type(content_type) {
        Some(format) => format,
        None => {
            return Err(ErrorResponse::Validation(
                "Avatar must be a JPEG, PNG, GIF or WebP image.".to_string(),
            ));
        }
    };

    // Stream the field in chunks so oversized uploads are rejected while
    // reading instead of buffering the whole body before the size check.
    let mut bytes: Vec<u8> = Vec::new();
    let mut total: usize = 0;
    loop {
        let chunk = field.chunk().await.map_err(|err| {
            ErrorResponse::BadRequest(format!("Failed to read multipart field: {}.", err))
        })?;
        let Some(chunk) = chunk else {
            break;
        };
        total += chunk.len();
        if total > state.configuration.avatar.max_bytes as usize {
            return Err(ErrorResponse::PayloadTooLarge(format!(
                "Avatar file exceeds the maximum allowed size of {} bytes.",
                state.configuration.avatar.max_bytes
            )));
        }
        bytes.extend_from_slice(&chunk);
    }

    if bytes.is_empty() {
        return Err(ErrorResponse::Validation(
            "Avatar file cannot be empty.".to_string(),
        ));
    }

    let maximum_source_pixels = state.configuration.avatar.maximum_source_pixels;
    let maximum_dimension = state.configuration.avatar.maximum_dimension;
    let processed_bytes = tokio::task::spawn_blocking(move || {
        process_image(
            bytes,
            claimed_format,
            maximum_source_pixels,
            maximum_dimension,
        )
    })
    .await
    .map_err(upload_retry_error)??;

    let filename = format!("{}.png", auth_user.user_id);
    let target_path = state.avatars_directory.join(&filename);
    let temporary_path = state
        .avatars_directory
        .join(format!("{}.part", Uuid::now_v7()));
    let backup_path = state
        .avatars_directory
        .join(format!("{}.backup", Uuid::now_v7()));
    let mut transaction = state.pool.begin().await.map_err(upload_retry_error)?;
    // Serialize replacements so concurrent uploads cannot delete each other's new avatar.
    let previous_avatar = sqlx::query("SELECT avatar FROM users WHERE id = $1 FOR UPDATE")
        .bind(auth_user.user_id)
        .fetch_one(&mut *transaction)
        .await
        .map_err(upload_retry_error)?
        .get::<Option<String>, _>("avatar");

    let old_filename = previous_avatar
        .as_deref()
        .and_then(|avatar| avatar.strip_prefix(AVATAR_URL_PREFIX))
        .map(str::to_string);

    if let Err(error) = tokio::fs::write(&temporary_path, processed_bytes).await {
        let _ = tokio::fs::remove_file(&temporary_path).await;
        return Err(upload_retry_error(error));
    }
    let previous_file_exists = match tokio::fs::try_exists(&target_path).await {
        Ok(exists) => exists,
        Err(error) => {
            let _ = tokio::fs::remove_file(&temporary_path).await;
            return Err(upload_retry_error(error));
        }
    };
    if previous_file_exists
        && let Err(error) = tokio::fs::hard_link(&target_path, &backup_path).await
    {
        let _ = tokio::fs::remove_file(&temporary_path).await;
        return Err(upload_retry_error(error));
    }
    if let Err(error) = tokio::fs::rename(&temporary_path, &target_path).await {
        let _ = tokio::fs::remove_file(&temporary_path).await;
        let _ = tokio::fs::remove_file(&backup_path).await;
        return Err(upload_retry_error(error));
    }

    let avatar_url = format!("{}{}", AVATAR_URL_PREFIX, filename);
    let save_result = sqlx::query("UPDATE users SET avatar = $1 WHERE id = $2")
        .bind(&avatar_url)
        .bind(auth_user.user_id)
        .execute(&mut *transaction)
        .await
        .map_err(upload_retry_error);
    let save_result = match save_result {
        Ok(_) => transaction.commit().await.map_err(upload_retry_error),
        Err(error) => Err(error),
    };
    if let Err(error) = save_result {
        if previous_file_exists {
            if let Err(restore_error) = tokio::fs::rename(&backup_path, &target_path).await {
                warn!("Failed to restore previous avatar: {restore_error}");
            }
        } else {
            let _ = tokio::fs::remove_file(&target_path).await;
        }
        return Err(error);
    }
    if previous_file_exists {
        let _ = tokio::fs::remove_file(&backup_path).await;
    }

    if let Some(old_filename) = old_filename.filter(|old_filename| old_filename != &filename)
        && let Err(err) = tokio::fs::remove_file(state.avatars_directory.join(&old_filename)).await
    {
        warn!(
            "Failed to delete the previous avatar file '{}': {}.",
            old_filename, err
        );
    }

    profile::respond_with_full_user(
        state,
        auth_user,
        "Avatar uploaded successfully.".to_string(),
    )
    .await
    .map_err(upload_retry_error)
}

// Serve an uploaded avatar file by its unguessable UUID filename. Public
// because <img> tags cannot attach Authorization headers.
pub async fn serve_avatar_file(
    State(state): State<Arc<ServerState>>,
    Path(filename): Path<String>,
) -> Result<Response, ErrorResponse> {
    lazy_static! {
        static ref AVATAR_FILENAME_REGEX: Regex = Regex::new(
            r"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\.(jpg|png|gif|webp)$"
        )
        .expect("Avatar filename regex is a compile-time constant");
    }

    // The strict pattern also rules out ".." path traversal segments.
    let Some(captures) = AVATAR_FILENAME_REGEX.captures(&filename) else {
        return Err(ErrorResponse::NotFound(
            "Avatar file not found.".to_string(),
        ));
    };

    let content_type = match &captures[1] {
        "jpg" => "image/jpeg",
        "png" => "image/png",
        "gif" => "image/gif",
        "webp" => "image/webp",
        _ => {
            return Err(ErrorResponse::NotFound(
                "Avatar file not found.".to_string(),
            ));
        }
    };

    let bytes = match tokio::fs::read(state.avatars_directory.join(&filename)).await {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Err(ErrorResponse::NotFound(
                "Avatar file not found.".to_string(),
            ));
        }
        Err(err) => {
            return Err(ErrorResponse::InternalError(format!(
                "Failed to read avatar file: {}.",
                err
            )));
        }
    };

    Ok((
        [
            (header::CONTENT_TYPE, content_type),
            (header::CACHE_CONTROL, "no-store"),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        ],
        bytes,
    )
        .into_response())
}
