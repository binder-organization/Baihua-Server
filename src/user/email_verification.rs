use crate::ServerState;
use crate::common::StandardResponse;
use crate::common::error::ErrorResponse;
use axum::Json;
use axum::extract::{
    Query, State,
    rejection::{JsonRejection, QueryRejection},
};
use axum::http::StatusCode;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{Duration, Utc};
use lettre::message::{Mailbox, Message};
use lettre::transport::smtp::authentication::Credentials;
use lettre::transport::smtp::client::{Certificate, Tls, TlsParameters};
use lettre::{SmtpTransport, Transport};
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use sqlx::Row;
use std::sync::Arc;
use std::time::Duration as StandardDuration;
use tracing::warn;
use uuid::Uuid;

#[derive(Deserialize)]
pub struct VerificationRequest {
    token: String,
}

#[derive(Deserialize)]
pub struct ResendRequest {
    email: String,
    username: Option<String>,
}

pub async fn verify_email(
    State(state): State<Arc<ServerState>>,
    request: Result<Query<VerificationRequest>, QueryRejection>,
) -> Result<StandardResponse, ErrorResponse> {
    let Query(request) = request
        .map_err(|_| ErrorResponse::Validation("A verification token is required.".to_string()))?;
    let token = URL_SAFE_NO_PAD
        .decode(&request.token)
        .map_err(|_| ErrorResponse::Validation("Invalid verification token.".to_string()))?;
    if token.len() != 32 {
        return Err(ErrorResponse::Validation(
            "Invalid verification token.".to_string(),
        ));
    }
    let token_hash = Sha256::digest(&token);
    let user_id: Uuid =
        sqlx::query_scalar("SELECT user_id FROM email_verification_tokens WHERE token_hash = $1")
            .bind(token_hash.as_slice())
            .fetch_optional(&state.pool)
            .await?
            .ok_or_else(|| {
                ErrorResponse::Validation("Verification link has expired or was used.".to_string())
            })?;
    let mut transaction = state.pool.begin().await?;
    sqlx::query("SELECT id FROM users WHERE id = $1 FOR UPDATE")
        .bind(user_id)
        .fetch_optional(&mut *transaction)
        .await?
        .ok_or_else(|| {
            ErrorResponse::Validation("Verification link has expired or was used.".to_string())
        })?;
    sqlx::query(
        "SELECT 1 FROM email_verification_tokens WHERE user_id = $1 AND token_hash = $2 AND expires_at > now() FOR UPDATE",
    )
    .bind(user_id)
    .bind(token_hash.as_slice())
    .fetch_optional(&mut *transaction)
    .await?
    .ok_or_else(|| ErrorResponse::Validation("Verification link has expired or was used.".to_string()))?;
    sqlx::query(
        "UPDATE users SET email_verified_at = now() WHERE id = $1 AND email_verified_at IS NULL",
    )
    .bind(user_id)
    .execute(&mut *transaction)
    .await?;
    sqlx::query("DELETE FROM email_verification_tokens WHERE user_id = $1")
        .bind(user_id)
        .execute(&mut *transaction)
        .await?;
    transaction.commit().await?;

    Ok(StandardResponse::success(
        StatusCode::OK,
        "Email address verified successfully.".to_string(),
        json!(null),
    ))
}

pub async fn resend_verification(
    State(state): State<Arc<ServerState>>,
    request: Result<Json<ResendRequest>, JsonRejection>,
) -> Result<StandardResponse, ErrorResponse> {
    let Json(request) = request
        .map_err(|_| ErrorResponse::Validation("A valid email address is required.".to_string()))?;
    if request.email.len() > 254 || !request.email.contains('@') {
        return Err(ErrorResponse::Validation(
            "Invalid email address.".to_string(),
        ));
    }
    let user_id = if let Some(username) = request.username.as_deref() {
        sqlx::query_scalar(
            "SELECT id FROM users WHERE lower(email) = lower($1) AND username = $2 AND email_verified_at IS NULL",
        )
        .bind(&request.email)
        .bind(username)
        .fetch_optional(&state.pool)
        .await?
    } else {
        let matching_users: Vec<Uuid> = sqlx::query_scalar(
            "SELECT id FROM users WHERE lower(email) = lower($1) AND email_verified_at IS NULL LIMIT 2",
        )
        .bind(&request.email)
        .fetch_all(&state.pool)
        .await?;
        if matching_users.len() == 1 {
            matching_users.into_iter().next()
        } else {
            None
        }
    };
    if let Some(user_id) = user_id {
        match send_verification(&state, user_id).await {
            Err(
                ErrorResponse::TooManyRequests(_)
                | ErrorResponse::ServiceUnavailable(_)
                | ErrorResponse::NotFound(_),
            ) => {}
            result => result?,
        }
    }
    Ok(StandardResponse::success(
        StatusCode::OK,
        "If this address needs verification, a message has been sent.".to_string(),
        json!(null),
    ))
}

pub async fn send_verification(state: &ServerState, user_id: Uuid) -> Result<(), ErrorResponse> {
    let configuration = &state.configuration.email_verification;
    let mut transaction = state.pool.begin().await?;
    let row = sqlx::query(
        "SELECT username, email, email_verified_at FROM users WHERE id = $1 FOR UPDATE",
    )
    .bind(user_id)
    .fetch_optional(&mut *transaction)
    .await?
    .ok_or_else(|| ErrorResponse::NotFound("User not found.".to_string()))?;
    if row
        .get::<Option<chrono::DateTime<Utc>>, _>("email_verified_at")
        .is_some()
    {
        return Ok(());
    }
    let username: String = row.get("username");
    let email: String = row.get("email");
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended(lower($1::text), 0))")
        .bind(&email)
        .execute(&mut *transaction)
        .await?;
    sqlx::query(
        "DELETE FROM email_verification_sends WHERE user_id = $1 AND sent_at <= now() - interval '1 minute'",
    )
    .bind(user_id)
    .execute(&mut *transaction)
    .await?;
    let sends: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM email_verification_sends AS sends JOIN users ON users.id = sends.user_id WHERE lower(users.email) = lower($1) AND sends.sent_at > now() - interval '1 minute'",
    )
    .bind(&email)
    .fetch_one(&mut *transaction)
    .await?;
    if sends >= i64::from(configuration.sends_per_email_per_minute) {
        return Err(ErrorResponse::TooManyRequests(
            "Email verification send limit reached. Try again in one minute.".to_string(),
        ));
    }

    let token: [u8; 32] = rand::random();
    let token_hash = Sha256::digest(token);
    let encoded_token = URL_SAFE_NO_PAD.encode(token);
    let verification_url = format!(
        "{}/api/v1/user/verify-email?token={}",
        configuration.verification_url.trim_end_matches('/'),
        encoded_token
    );
    let content = if configuration.template_path.is_empty() {
        "Hello {{username}},\n\nVerify your email address: {{verification_url}}\n\nThis link expires in {{expires_minutes}} minutes.\n"
            .to_string()
    } else {
        tokio::fs::read_to_string(&configuration.template_path)
            .await
            .map_err(|error| {
                ErrorResponse::InternalError(format!("Cannot read mail template: {error}"))
            })?
    };
    let content = content
        .replace("{{username}}", &username)
        .replace("{{verification_url}}", &verification_url)
        .replace(
            "{{expires_minutes}}",
            &configuration.token_lifetime_minutes.to_string(),
        );
    let sender: Mailbox = format!(
        "{} <{}>",
        configuration.sender_name, configuration.sender_address
    )
    .parse()
    .map_err(|error| ErrorResponse::InternalError(format!("Invalid mail sender: {error}")))?;
    let recipient: Mailbox = email.parse().map_err(|error| {
        ErrorResponse::InternalError(format!("Invalid recipient address: {error}"))
    })?;
    let message = Message::builder()
        .from(sender)
        .to(recipient)
        .subject(&configuration.subject)
        .body(content)
        .map_err(|error| {
            ErrorResponse::InternalError(format!("Cannot build verification message: {error}"))
        })?;
    let server_address = configuration.server_address.clone();
    let server_port = configuration.server_port;
    let security = configuration.security.clone();
    let mail_username = configuration.username.clone();
    let mail_password = std::env::var("BAIHUA_MAIL_PASSWORD").unwrap_or_default();
    let trusted_root_certificate_path = configuration.trusted_root_certificate_path.clone();
    tokio::task::spawn_blocking(move || {
        let mut builder = if security == "implicit" {
            SmtpTransport::relay(&server_address)
        } else {
            SmtpTransport::starttls_relay(&server_address)
        }
        .map_err(|error| error.to_string())?
        .port(server_port)
        .timeout(Some(StandardDuration::from_secs(10)));
        if !trusted_root_certificate_path.is_empty() {
            let certificate =
                std::fs::read(&trusted_root_certificate_path).map_err(|error| error.to_string())?;
            let certificate =
                Certificate::from_pem(&certificate).map_err(|error| error.to_string())?;
            let parameters = TlsParameters::builder(server_address)
                .add_root_certificate(certificate)
                .build()
                .map_err(|error| error.to_string())?;
            builder = builder.tls(if security == "implicit" {
                Tls::Wrapper(parameters)
            } else {
                Tls::Required(parameters)
            });
        }
        if !mail_username.is_empty() {
            builder = builder.credentials(Credentials::new(mail_username, mail_password));
        }
        builder
            .build()
            .send(&message)
            .map_err(|error| error.to_string())?;
        Ok::<(), String>(())
    })
    .await
    .map_err(|error| {
        warn!("Mail task failed: {error}");
        ErrorResponse::ServiceUnavailable("Email delivery is temporarily unavailable.".to_string())
    })?
    .map_err(|error| {
        warn!("Mail delivery failed: {error}");
        ErrorResponse::ServiceUnavailable("Email delivery is temporarily unavailable.".to_string())
    })?;

    sqlx::query(
        "INSERT INTO email_verification_tokens (user_id, token_hash, expires_at) VALUES ($1, $2, $3) ON CONFLICT (user_id) DO UPDATE SET token_hash = excluded.token_hash, expires_at = excluded.expires_at",
    )
    .bind(user_id)
    .bind(token_hash.as_slice())
    .bind(Utc::now() + Duration::minutes(i64::from(configuration.token_lifetime_minutes)))
    .execute(&mut *transaction)
    .await?;
    sqlx::query("INSERT INTO email_verification_sends (user_id) VALUES ($1)")
        .bind(user_id)
        .execute(&mut *transaction)
        .await?;
    transaction.commit().await?;
    Ok(())
}
