use crate::Directory;
use crate::ServerState;
use crate::infrastructure::config::ServerConfiguration;
use crate::infrastructure::database::get_pool;
use crate::infrastructure::environment::Environment;
use crate::infrastructure::log;
use crate::middleware::rate_limit::SlidingWindowRateLimiter;
use crate::websocket::connection::ConnectionManager;
use anyhow::{Context, Result, anyhow};
use dirs::home_dir;
use jsonwebtoken::crypto::{CryptoProvider, rust_crypto::DEFAULT_PROVIDER};
use std::path::Path;
use std::sync::Arc;
use tokio::fs;
use tracing::info;

pub async fn initialize(
    environment: Environment,
) -> Result<(ServerState, tracing_appender::non_blocking::WorkerGuard)> {
    println!("Initialize: Start initializing the server.");

    // Determine the application directory.
    let home = home_dir().context("The user home directory cannot be obtained.")?;
    let app_directory = std::env::var("BAIHUA_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| home.join(".baihua"));

    ensure_app_directories(&app_directory).await?;

    // Load configuration.
    let mut configuration = load_or_create_profile(&app_directory).await?;
    if let Ok(grace_period) = std::env::var("BAIHUA_ENCRYPTED_GRACE_PERIOD_SECS") {
        configuration.websocket.encrypted_grace_period_secs = grace_period
            .parse()
            .context("Invalid encrypted session grace period.")?;
    }
    configuration.validate()?;

    let directory = Directory {
        app: app_directory.clone(),
        log: app_directory.join("logs"),
    };

    let guard = log::init_log(&directory, &configuration, environment)
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
        login_rate_limiter,
        register_rate_limiter,
        shutting_down: Arc::new(std::sync::atomic::AtomicBool::new(false)),
    };

    info!("Initialization completed.");

    Ok((state, guard))
}

async fn ensure_app_directories(app_directory: &Path) -> Result<()> {
    let directories = vec!["logs", "avatars"];

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
