use crate::ServerState;
use crate::common::StandardResponse;
use crate::common::error::ErrorResponse;
use crate::common::extractor::JsonBody;
use crate::middleware::authenticate::AuthenticatedUser;
use axum::Extension;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use lazy_static::lazy_static;
use regex::Regex;
use serde::Deserialize;
use serde_json::json;
use sqlx::Row;
use std::sync::Arc;
use uuid::Uuid;

#[derive(Debug, Deserialize)]
pub struct UpdateProfileRequest {
    #[serde(default, deserialize_with = "deserialize_optional_string")]
    pub nickname: Option<Option<String>>,
    #[serde(default, deserialize_with = "deserialize_optional_string")]
    pub phone_number: Option<Option<String>>,
    #[serde(default, deserialize_with = "deserialize_optional_string")]
    pub bio: Option<Option<String>>,
    #[serde(default, deserialize_with = "deserialize_optional_string")]
    pub avatar: Option<Option<String>>,
}

// Distinguish a missing field from an explicit null so that "unset"
// keeps the stored value while explicit null clears it.
fn deserialize_optional_string<'de, D>(deserializer: D) -> Result<Option<Option<String>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Option::<String>::deserialize(deserializer)?;
    Ok(Some(value))
}

fn validate_nickname(nickname: &str) -> Result<(), ErrorResponse> {
    let trimmed = nickname.trim();
    if trimmed.is_empty() {
        return Err(ErrorResponse::Validation(
            "Nickname cannot be empty.".to_string(),
        ));
    }
    if trimmed.len() > 40 {
        return Err(ErrorResponse::Validation(
            "Nickname must be at most 40 characters long.".to_string(),
        ));
    }
    Ok(())
}

fn validate_e164_phone(phone: &str) -> Result<(), ErrorResponse> {
    lazy_static! {
        static ref E164_REGEX: Regex =
            Regex::new(r"^\+[1-9]\d{1,14}$").expect("E.164 regex is a compile-time constant");
    }
    if !E164_REGEX.is_match(phone) {
        return Err(ErrorResponse::Validation(
            "Phone number must be in E.164 format, for example +8613800000000.".to_string(),
        ));
    }
    Ok(())
}

fn validate_bio(bio: &str) -> Result<(), ErrorResponse> {
    let trimmed = bio.trim();
    if trimmed.is_empty() {
        return Err(ErrorResponse::Validation(
            "Bio cannot be empty.".to_string(),
        ));
    }
    if trimmed.len() > 200 {
        return Err(ErrorResponse::Validation(
            "Bio must be at most 200 characters long.".to_string(),
        ));
    }
    Ok(())
}

fn validate_avatar(avatar: &str) -> Result<(), ErrorResponse> {
    let trimmed = avatar.trim();
    if trimmed.is_empty() {
        return Err(ErrorResponse::Validation(
            "Avatar URL cannot be empty.".to_string(),
        ));
    }
    if trimmed.len() > 2048 {
        return Err(ErrorResponse::Validation(
            "Avatar URL must be at most 2048 characters long.".to_string(),
        ));
    }
    if !trimmed.starts_with("http://") && !trimmed.starts_with("https://") {
        return Err(ErrorResponse::Validation(
            "Avatar URL must start with http:// or https://.".to_string(),
        ));
    }
    Ok(())
}

// Return the public profile of a user looked up by id or by exact
// username; the two lookup keys are equivalent and share the response shape.
pub async fn get_public_profile(
    State(state): State<Arc<ServerState>>,
    Extension(_auth_user): Extension<AuthenticatedUser>,
    Path(user_key): Path<String>,
) -> Result<StandardResponse, ErrorResponse> {
    let row = match Uuid::parse_str(&user_key) {
        Ok(user_id) => {
            sqlx::query(
                "SELECT id, username, nickname, bio, avatar FROM users \
                 WHERE is_enabled = true AND id = $1",
            )
            .bind(user_id)
            .fetch_optional(&state.pool)
            .await?
        }
        Err(_) => {
            sqlx::query(
                "SELECT id, username, nickname, bio, avatar FROM users \
                 WHERE is_enabled = true AND username = $1",
            )
            .bind(&user_key)
            .fetch_optional(&state.pool)
            .await?
        }
    };

    let Some(row) = row else {
        return Err(ErrorResponse::NotFound("User not found.".to_string()));
    };

    Ok(StandardResponse::success(
        StatusCode::OK,
        "User profile retrieved successfully.".to_string(),
        json!({
            "user": {
                "id": row.get::<Uuid, _>("id"),
                "username": row.get::<String, _>("username"),
                "nickname": row.get::<Option<String>, _>("nickname"),
                "bio": row.get::<Option<String>, _>("bio"),
                "avatar": row.get::<Option<String>, _>("avatar"),
            }
        }),
    ))
}

// Update the caller's own nickname, phone number, bio and avatar. Missing
// fields keep their stored value; explicit null clears the field; a
// non-empty string is validated before being written.
pub async fn update_profile(
    State(state): State<Arc<ServerState>>,
    Extension(auth_user): Extension<AuthenticatedUser>,
    JsonBody(request): JsonBody<UpdateProfileRequest>,
) -> Result<StandardResponse, ErrorResponse> {
    // An absent field means "no change" while explicit null means "clear";
    // both collapse to Option<Option<String>> before merging with current.
    let nickname_absent = request.nickname.is_none();
    let phone_absent = request.phone_number.is_none();
    let bio_absent = request.bio.is_none();
    let avatar_absent = request.avatar.is_none();
    if nickname_absent && phone_absent && bio_absent && avatar_absent {
        return Err(ErrorResponse::BadRequest(
            "Nothing to update: provide nickname, phone_number, bio and/or avatar.".to_string(),
        ));
    }

    let current =
        sqlx::query("SELECT nickname, phone_number, bio, avatar FROM users WHERE id = $1")
            .bind(auth_user.user_id)
            .fetch_one(&state.pool)
            .await?;

    let nickname = match request.nickname {
        Some(Some(value)) => {
            validate_nickname(&value)?;
            Some(value.trim().to_string())
        }
        Some(None) => None,
        None => current.get::<Option<String>, _>("nickname"),
    };

    let phone_number = match request.phone_number {
        Some(Some(value)) => {
            validate_e164_phone(&value)?;
            Some(value)
        }
        Some(None) => None,
        None => current.get::<Option<String>, _>("phone_number"),
    };

    let bio = match request.bio {
        Some(Some(value)) => {
            validate_bio(&value)?;
            Some(value.trim().to_string())
        }
        Some(None) => None,
        None => current.get::<Option<String>, _>("bio"),
    };

    let avatar = match request.avatar {
        Some(Some(value)) => {
            validate_avatar(&value)?;
            Some(value.trim().to_string())
        }
        Some(None) => None,
        None => current.get::<Option<String>, _>("avatar"),
    };

    sqlx::query(
        "UPDATE users SET nickname = $1, phone_number = $2, bio = $3, avatar = $4 WHERE id = $5",
    )
    .bind(nickname)
    .bind(phone_number)
    .bind(bio)
    .bind(avatar)
    .bind(auth_user.user_id)
    .execute(&state.pool)
    .await?;

    respond_with_full_user(
        &state,
        &auth_user,
        "User profile updated successfully.".to_string(),
    )
    .await
}

// Return the full user row after a successful update so the caller sees
// the persisted result, including fields they did not modify.
pub(crate) async fn respond_with_full_user(
    state: &Arc<ServerState>,
    auth_user: &AuthenticatedUser,
    message: String,
) -> Result<StandardResponse, ErrorResponse> {
    let user = sqlx::query(
        "SELECT id, username, email, nickname, phone_number, bio, avatar, created_at, is_enabled \
         FROM users WHERE id = $1",
    )
    .bind(auth_user.user_id)
    .fetch_one(&state.pool)
    .await?;

    Ok(StandardResponse::success(
        StatusCode::OK,
        message,
        json!({
            "user": {
                "id": user.get::<Uuid, _>("id"),
                "username": user.get::<String, _>("username"),
                "email": user.get::<String, _>("email"),
                "nickname": user.get::<Option<String>, _>("nickname"),
                "phone_number": user.get::<Option<String>, _>("phone_number"),
                "bio": user.get::<Option<String>, _>("bio"),
                "avatar": user.get::<Option<String>, _>("avatar"),
                "created_at": user.get::<chrono::DateTime<chrono::Utc>, _>("created_at"),
                "is_enabled": user.get::<bool, _>("is_enabled"),
            }
        }),
    ))
}
