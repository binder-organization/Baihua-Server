use crate::Directory;
use crate::ServerState;
use crate::infrastructure::config::ServerConfiguration;
use crate::infrastructure::database::get_pool;
use crate::infrastructure::environment::Environment;
use crate::infrastructure::log::{self, LogSystem};
use crate::middleware::rate_limit::SlidingWindowRateLimiter;
use crate::websocket::connection::ConnectionManager;
use anyhow::{Context, Result, anyhow};
use dirs::home_dir;
use jsonwebtoken::crypto::{CryptoProvider, rust_crypto::DEFAULT_PROVIDER};
use lettre::message::Mailbox;
use lettre::transport::smtp::client::Certificate;
use lettre::transport::smtp::client::TlsParameters;
use std::path::Path;
use std::sync::Arc;
use tokio::fs;
use tracing::info;

pub async fn initialize(environment: Environment) -> Result<(ServerState, LogSystem)> {
    println!("Initialize: Start initializing the server.");

    // Determine the application directory.
    let home = home_dir().context("The user home directory cannot be obtained.")?;
    let app_directory = std::env::var("BAIHUA_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| home.join(".baihua"));

    ensure_app_directories(&app_directory).await?;

    // Load configuration.
    let mut configuration = load_or_create_profile(&app_directory).await?;
    if let Ok(sender_address) = std::env::var("BAIHUA_MAIL_SENDER_ADDRESS")
        && !sender_address.is_empty()
    {
        configuration.email_verification.sender_address = sender_address;
    }
    if let Ok(sender_name) = std::env::var("BAIHUA_MAIL_SENDER_NAME")
        && !sender_name.is_empty()
    {
        configuration.email_verification.sender_name = sender_name;
    }
    if let Ok(server_address) = std::env::var("BAIHUA_MAIL_SERVER_ADDRESS")
        && !server_address.is_empty()
    {
        configuration.email_verification.server_address = server_address;
    }
    if let Ok(server_port) = std::env::var("BAIHUA_MAIL_SERVER_PORT")
        && !server_port.is_empty()
    {
        configuration.email_verification.server_port =
            server_port.parse().context("Invalid mail server port.")?;
    }
    if let Ok(security) = std::env::var("BAIHUA_MAIL_SECURITY")
        && !security.is_empty()
    {
        configuration.email_verification.security = security;
    }
    if let Ok(username) = std::env::var("BAIHUA_MAIL_USERNAME")
        && !username.is_empty()
    {
        configuration.email_verification.username = username;
    }
    if let Ok(subject) = std::env::var("BAIHUA_MAIL_SUBJECT")
        && !subject.is_empty()
    {
        configuration.email_verification.subject = subject;
    }
    if let Ok(template_path) = std::env::var("BAIHUA_MAIL_TEMPLATE_PATH")
        && !template_path.is_empty()
    {
        configuration.email_verification.template_path = template_path;
    }
    if let Ok(certificate_path) = std::env::var("BAIHUA_MAIL_TRUSTED_ROOT_CERTIFICATE_PATH")
        && !certificate_path.is_empty()
    {
        configuration
            .email_verification
            .trusted_root_certificate_path = certificate_path;
    }
    if let Ok(send_limit) = std::env::var("BAIHUA_MAIL_SENDS_PER_EMAIL_PER_MINUTE")
        && !send_limit.is_empty()
    {
        configuration.email_verification.sends_per_email_per_minute = send_limit
            .parse()
            .context("Invalid per-email send limit.")?;
    }
    if let Ok(token_lifetime) = std::env::var("BAIHUA_EMAIL_TOKEN_LIFETIME_MINUTES")
        && !token_lifetime.is_empty()
    {
        configuration.email_verification.token_lifetime_minutes = token_lifetime
            .parse()
            .context("Invalid email token lifetime.")?;
    }
    if let Ok(verification_url) = std::env::var("BAIHUA_VERIFICATION_BASE_URL")
        && !verification_url.is_empty()
    {
        configuration.email_verification.verification_url = verification_url;
    } else if let Ok(domain) = std::env::var("BAIHUA_DOMAIN")
        && !domain.is_empty()
    {
        configuration.email_verification.verification_url = format!("https://{domain}:2424");
    }
    if let Ok(grace_period) = std::env::var("BAIHUA_ENCRYPTED_GRACE_PERIOD_SECS") {
        configuration.websocket.encrypted_grace_period_secs = grace_period
            .parse()
            .context("Invalid encrypted session grace period.")?;
    }
    if let Ok(request_timeout) = std::env::var("BAIHUA_REQUEST_TIMEOUT_SECS") {
        configuration.web.request_timeout_secs = request_timeout
            .parse()
            .context("Invalid request timeout.")?;
    }
    if let Ok(require_encrypted_transport) = std::env::var("BAIHUA_REQUIRE_ENCRYPTED_TRANSPORT") {
        configuration.web.require_encrypted_transport = require_encrypted_transport
            .parse()
            .context("Invalid encrypted transport requirement.")?;
    }
    if let Ok(maximum_uploads) = std::env::var("BAIHUA_MAXIMUM_CONCURRENT_UPLOADS_PER_USER") {
        configuration.file.maximum_concurrent_uploads_per_user = maximum_uploads
            .parse()
            .context("Invalid maximum concurrent uploads per user.")?;
    }
    if let Ok(upload_idle_timeout) = std::env::var("BAIHUA_FILE_UPLOAD_IDLE_TIMEOUT_SECS") {
        configuration.file.upload_idle_timeout_secs = upload_idle_timeout
            .parse()
            .context("Invalid file upload idle timeout.")?;
    }
    configuration.validate()?;
    if configuration.email_verification.sender_address.is_empty()
        || configuration.email_verification.server_address.is_empty()
        || configuration.email_verification.verification_url.is_empty()
    {
        anyhow::bail!(
            "Email verification requires a sender address, mail server address, and public verification URL."
        );
    }
    format!(
        "{} <{}>",
        configuration.email_verification.sender_name,
        configuration.email_verification.sender_address
    )
    .parse::<Mailbox>()
    .context("Invalid email verification sender address.")?;
    if environment.is_production()
        && !configuration
            .email_verification
            .verification_url
            .starts_with("https://")
    {
        anyhow::bail!("The production verification URL must use encrypted transport.");
    }
    if !configuration.email_verification.template_path.is_empty() {
        let template = fs::read_to_string(&configuration.email_verification.template_path)
            .await
            .context("Failed to read the email verification template.")?;
        if !template.contains("{{verification_url}}") {
            anyhow::bail!("The email template must contain {{verification_url}}.");
        }
    }
    if !configuration
        .email_verification
        .trusted_root_certificate_path
        .is_empty()
    {
        let certificate = fs::read(
            &configuration
                .email_verification
                .trusted_root_certificate_path,
        )
        .await
        .context("Failed to read the trusted mail certificate.")?;
        let certificate =
            Certificate::from_pem(&certificate).context("Invalid trusted mail certificate.")?;
        TlsParameters::builder(configuration.email_verification.server_address.clone())
            .add_root_certificate(certificate)
            .build()
            .context("Failed to configure trusted mail certificate.")?;
    }
    if !configuration.email_verification.username.is_empty()
        && std::env::var("BAIHUA_MAIL_PASSWORD")
            .unwrap_or_default()
            .is_empty()
    {
        anyhow::bail!("BAIHUA_MAIL_PASSWORD is required when mail authentication is configured.");
    }

    let directory = Directory {
        app: app_directory.clone(),
        log: app_directory.join("logs"),
    };

    let log_system = log::init_log(&directory, &configuration, environment)
        .map_err(|error| anyhow!("Failed to initialize logging system: {}.", error))?;

    let pool = get_pool(environment, &configuration.database).await?;

    let migrations_path = if environment.is_production() {
        let exe = std::env::current_exe()
            .context("Failed to determine binary location for migrations.")?;
        let exe_dir = exe
            .parent()
            .context("Binary path has no parent directory.")?
            .to_path_buf();
        exe_dir.join("migrations")
    } else if environment.is_development() {
        // In the development environment, it directly calls existing SQL.
        std::path::PathBuf::from("migrations")
    } else {
        unreachable!()
    };

    let migrator = sqlx::migrate::Migrator::new(migrations_path)
        .await
        .context("Failed to load migrations.")?;

    migrator
        .run(&pool)
        .await
        .context("Failed to run migrations.")?;

    info!("Database initialized.");

    CryptoProvider::install_default(&DEFAULT_PROVIDER)
        .expect("Failed to install the default crypto provider.");

    let jwt_secret = environment
        .require_variable("JWT_SECRET", "default-jwt-secret-key")
        .context("Failed to load JWT secret.")?;

    let login_rate_limiter = Arc::new(SlidingWindowRateLimiter::new(
        configuration.rate_limit.login_max_requests,
        configuration.rate_limit.login_window_secs,
    ));
    let register_rate_limiter = Arc::new(SlidingWindowRateLimiter::new(
        configuration.rate_limit.register_max_requests,
        configuration.rate_limit.register_window_secs,
    ));

    let state = ServerState {
        configuration,
        pool,
        jwt_secret,
        environment,
        connection_manager: Arc::new(ConnectionManager::new()),
        avatars_directory: app_directory.join("avatars"),
        files_directory: app_directory.join("files"),
        file_uploads_directory: app_directory.join("files").join("uploads"),
        active_file_uploads: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        login_rate_limiter,
        register_rate_limiter,
        shutting_down: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        restart_requested: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        started_at: std::time::Instant::now(),
    };

    crate::chat::file::cleanup_pending_files(&state)
        .await
        .context("Failed to clean pending stored files.")?;

    info!("Initialization completed.");

    Ok((state, log_system))
}

async fn ensure_app_directories(app_directory: &Path) -> Result<()> {
    let directories = vec!["logs", "avatars", "files", "files/uploads"];

    for directory_name in directories {
        let directory_path = app_directory.join(directory_name);
        if !directory_path.exists() {
            fs::create_dir_all(&directory_path)
                .await
                .with_context(|| format!("Failed to create a directory:{:?}", directory_path))?;
            println!("Initialize: Create a directory: {:?}.", directory_path);
        }
    }

    Ok(())
}

async fn load_or_create_profile(app_directory: &Path) -> Result<ServerConfiguration> {
    let profile_path = app_directory.join("config.toml");

    let mut configuration = if profile_path.exists() {
        let content = fs::read_to_string(&profile_path)
            .await
            .context("Failed to read the profile.")?;

        let configuration: ServerConfiguration =
            toml::from_str(&content).context("Failed to parse the profile.")?;

        println!("Initialize: Load the configuration from an existing profile.");

        configuration
    } else {
        let configuration = ServerConfiguration::default();
        let toml_content = ServerConfiguration::default_config_content();

        fs::write(&profile_path, toml_content)
            .await
            .context("Write to the profile failed.")?;

        println!("Initialize: Create a profile: {:?}.", profile_path);

        configuration
    };

    // Allow the bind host to be overridden via environment variable so the
    // same binary works for local development (127.0.0.1) and containers
    // (0.0.0.0) without editing the config file.
    if let Ok(bind_host) = std::env::var("BIND_HOST") {
        configuration.web.host = bind_host;
    }

    Ok(configuration)
}
