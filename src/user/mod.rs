pub(crate) mod avatar;
mod delete;
mod login;
mod logout;
mod password;
mod profile;
mod register;
mod search;

use crate::common::error::ErrorResponse;
use crate::{ServerState, middleware};
use chrono::{DateTime, Utc};
use lazy_static::lazy_static;
use regex::Regex;
use serde::{Deserialize, Serialize};
use sqlx::{self, Row};
use std::sync::{Arc, LazyLock};
use tokio::sync::Semaphore;
use tracing::{error, info};
use uuid::Uuid;

static PASSWORD_COMPUTATION_LIMIT: LazyLock<Arc<Semaphore>> = LazyLock::new(|| {
    let parallelism = std::thread::available_parallelism()
        .map(|parallelism| parallelism.get())
        .unwrap_or(4)
        .min(8);
    Arc::new(Semaphore::new(parallelism))
});

async fn run_password_computation<ResultType, Operation>(
    operation: Operation,
) -> Result<ResultType, ErrorResponse>
where
    ResultType: Send + 'static,
    Operation: FnOnce() -> Result<ResultType, bcrypt::BcryptError> + Send + 'static,
{
    // Bound password work to avoid overwhelming the blocking thread pool.
    let permit = PASSWORD_COMPUTATION_LIMIT
        .clone()
        .acquire_owned()
        .await
        .map_err(|error| ErrorResponse::InternalError(error.to_string()))?;

    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        operation()
    })
    .await
    .map_err(|error| ErrorResponse::InternalError(format!("Password task failed: {error}.")))?
    .map_err(|error| ErrorResponse::InternalError(format!("Password operation failed: {error}.")))
}

pub(crate) async fn hash_password(password: String, cost: u32) -> Result<String, ErrorResponse> {
    run_password_computation(move || bcrypt::hash(password, cost)).await
}

pub(crate) async fn verify_password(
    password: String,
    stored_hash: String,
) -> Result<bool, ErrorResponse> {
    run_password_computation(move || bcrypt::verify(password, &stored_hash)).await
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct User {
    // Primary key.
    pub id: Uuid,
    pub username: String,
    pub email: String,
    // Optional and repeatable user nickname.
    pub nickname: Option<String>,
    pub phone_number: Option<String>,
    pub bio: Option<String>,
    pub avatar: Option<String>,
    // UTC datetime (serialized as RFC 3339).
    pub created_at: DateTime<Utc>,
    pub is_active: bool,
    // Monotonic counter bumped on logout and password change; used to
    // invalidate previously issued JWTs. Never exposed to clients.
    #[serde(skip_serializing)]
    pub token_version: i64,
}

#[derive(Clone, Debug, Deserialize)]
pub struct UserRegister {
    pub username: String,
    pub email: String,
    pub password: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct UserLogin {
    pub username: String,
    pub password: String,
}

impl UserRegister {
    pub async fn validate(&self) -> Result<(), ErrorResponse> {
        // Validate the regular expression of the mailbox.
        lazy_static! {
            static ref EMAIL_REGEX: Result<Regex, String> = Regex::new(
                r"^[a-zA-Z0-9.!#$%&'*+/=?^_`{|}~-]+@[a-zA-Z0-9](?:[a-zA-Z0-9-]{0,61}[a-zA-Z0-9])*(?:\.[a-zA-Z0-9](?:[a-zA-Z0-9-]{0,61}[a-zA-Z0-9])*)*$"
            ).map_err(|error| error.to_string());
        }

        // Validate username, email, and password.
        if self.username.is_empty() || self.email.is_empty() || self.password.is_empty() {
            return Err(ErrorResponse::Validation(
                "Username, email, and password cannot be empty.".to_string(),
            ));
        }

        // Email regex.
        match EMAIL_REGEX.as_ref() {
            Ok(regex) => {
                if !regex.is_match(&self.email) {
                    return Err(ErrorResponse::Validation(
                        "Invalid email format. Please provide a valid email address.".to_string(),
                    ));
                }
            }
            Err(error_msg) => {
                error!("Regex compilation failed: {}", error_msg);
                return Err(ErrorResponse::InternalError(
                    "Failed to compile email regex.".to_string(),
                ));
            }
        }

        // Check the mailbox length.
        if self.email.len() > 254 {
            return Err(ErrorResponse::Validation(
                "Email address is too long (maximum 254 characters).".to_string(),
            ));
        }

        // Check username.
        if self.username.len() <= 3 || self.username.len() >= 40 {
            return Err(ErrorResponse::Validation(
                "Username must be between 4 and 40 characters long.".to_string(),
            ));
        }

        // A username that parses as a UUID would make the /user/{user}
        // lookup ambiguous, so reject it at registration time.
        if Uuid::parse_str(&self.username).is_ok() {
            return Err(ErrorResponse::Validation(
                "Username cannot be in UUID format.".to_string(),
            ));
        }

        // Can't be mine!
        if self.email == "gav.zheng@outlook.com" {
            return Err(ErrorResponse::Validation(
                "Email cannot be 'gav.zheng@outlook.com'.".to_string(),
            ));
        }

        Ok(())
    }
}

pub async fn new_user(
    new_user: UserRegister,
    bcrypt_cost: u32,
    pool: &sqlx::Pool<sqlx::Postgres>,
) -> Result<User, ErrorResponse> {
    // Validate the user.
    new_user.validate().await?;

    // Check if username or email already exists.
    let existing_user = sqlx::query("SELECT id FROM users WHERE username = $1 OR email = $2")
        .bind(&new_user.username)
        .bind(&new_user.email)
        .fetch_optional(pool)
        .await?;

    if existing_user.is_some() {
        return Err(ErrorResponse::Validation(
            "Username or email already exists.".to_string(),
        ));
    }

    // Generate UUID and current time.
    let uuid = Uuid::now_v7();
    let created_at = Utc::now();

    // Hash password.
    let password_hashed = hash_password(new_user.password, bcrypt_cost).await?;

    // Insert user into database.
    sqlx::query(
        "INSERT INTO users (id, username, email, password, created_at, is_active) VALUES ($1, $2, $3, $4, $5, $6)"
    )
    .bind(uuid)
    .bind(&new_user.username)
    .bind(&new_user.email)
    .bind(&password_hashed)
    .bind(created_at)
    .bind(true)
    .execute(pool)
        .await?;

    info!(
        "New user: {} is created, UUID is: {}.",
        new_user.username, &uuid
    );

    Ok(User {
        id: uuid,
        username: new_user.username,
        email: new_user.email,
        nickname: None,
        phone_number: None,
        bio: None,
        avatar: None,
        created_at,
        is_active: true,
        token_version: 0,
    })
}

// Find user by UUID.
pub async fn find_user_by_id(
    id: Uuid,
    pool: &sqlx::Pool<sqlx::Postgres>,
) -> Result<Option<User>, ErrorResponse> {
    let row = sqlx::query(
        r#"SELECT id, username, email, nickname, phone_number, bio, avatar, created_at, is_active, token_version
           FROM users WHERE id = $1"#,
    )
    .bind(id)
    .fetch_optional(pool)
    .await?;

    match row {
        Some(row) => Ok(Some(User {
            id: row.get("id"),
            username: row.get("username"),
            email: row.get("email"),
            nickname: row.get("nickname"),
            phone_number: row.get("phone_number"),
            bio: row.get("bio"),
            avatar: row.get("avatar"),
            created_at: row.get("created_at"),
            is_active: row.get("is_active"),
            token_version: row.get("token_version"),
        })),
        None => Ok(None),
    }
}

// Find user by username.
pub async fn find_user_by_username(
    username: &str,
    pool: &sqlx::Pool<sqlx::Postgres>,
) -> Result<Option<User>, ErrorResponse> {
    let row = sqlx::query(
        r#"SELECT id, username, email, nickname, phone_number, bio, avatar, created_at, is_active, token_version
           FROM users WHERE username = $1"#,
    )
    .bind(username)
    .fetch_optional(pool)
    .await?;

    match row {
        Some(row) => Ok(Some(User {
            id: row.get("id"),
            username: row.get("username"),
            email: row.get("email"),
            nickname: row.get("nickname"),
            phone_number: row.get("phone_number"),
            bio: row.get("bio"),
            avatar: row.get("avatar"),
            created_at: row.get("created_at"),
            is_active: row.get("is_active"),
            token_version: row.get("token_version"),
        })),
        None => Ok(None),
    }
}

pub async fn find_user_with_password(
    username: &str,
    pool: &sqlx::Pool<sqlx::Postgres>,
) -> Result<Option<(User, String)>, ErrorResponse> {
    let row = sqlx::query(
        r#"SELECT id, username, email, password, nickname, phone_number, bio, avatar, created_at, is_active, token_version
           FROM users WHERE username = $1"#,
    )
    .bind(username)
    .fetch_optional(pool)
    .await?;

    match row {
        Some(row) => {
            let password: String = row.get("password");
            let user = User {
                id: row.get("id"),
                username: row.get("username"),
                email: row.get("email"),
                nickname: row.get("nickname"),
                phone_number: row.get("phone_number"),
                bio: row.get("bio"),
                avatar: row.get("avatar"),
                created_at: row.get("created_at"),
                is_active: row.get("is_active"),
                token_version: row.get("token_version"),
            };
            Ok(Some((user, password)))
        }
        None => Ok(None),
    }
}

pub fn router(state: Arc<ServerState>) -> axum::Router<Arc<ServerState>> {
    let public = axum::Router::new()
        .route(
            "/register",
            axum::routing::post(register::register)
                .route_layer(axum::middleware::from_fn_with_state(
                    state.clone(),
                    middleware::validate::validate_register,
                ))
                .route_layer(axum::middleware::from_fn_with_state(
                    state.clone(),
                    middleware::rate_limit::rate_limit_register,
                )),
        )
        .route(
            "/login",
            axum::routing::post(login::login)
                .route_layer(axum::middleware::from_fn_with_state(
                    state.clone(),
                    middleware::validate::validate_login,
                ))
                .route_layer(axum::middleware::from_fn_with_state(
                    state.clone(),
                    middleware::rate_limit::rate_limit_login,
                )),
        );

    let authenticated = axum::Router::new()
        .route("/search", axum::routing::get(search::search_users))
        .route("/{user}", axum::routing::get(profile::get_public_profile))
        .route(
            "/me",
            axum::routing::patch(profile::update_profile).delete(delete::delete_account),
        )
        .route("/me/logout", axum::routing::post(logout::logout))
        .route(
            "/me/password",
            axum::routing::patch(password::change_password),
        )
        .route("/me/avatar", axum::routing::post(avatar::upload_avatar))
        .route_layer(axum::middleware::from_fn_with_state(
            state,
            crate::middleware::authenticate::authenticate,
        ));

    axum::Router::new().merge(public).merge(authenticated)
}
