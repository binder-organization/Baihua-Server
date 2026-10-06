// Baihua Server, by Gavin Zheng on January 1, 2026.

mod authenticate;
mod chat;
mod common;
mod console;
mod greet;
mod health;
mod infrastructure;
mod middleware;
mod server;
mod user;
mod websocket;

use anyhow::Result;
use infrastructure::config::ServerConfiguration;
use infrastructure::environment::Environment;
use infrastructure::initialize;
use middleware::rate_limit::SlidingWindowRateLimiter;
use sqlx::PgPool;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tracing::{error, info};
use websocket::connection::ConnectionManager;

#[derive(Debug, Clone)]
pub struct Directory {
    pub app: PathBuf,
    pub log: PathBuf,
}

#[derive(Clone)]
pub struct ServerState {
    pub configuration: ServerConfiguration,
    pub pool: PgPool,
    pub jwt_secret: String,
    pub environment: Environment,
    pub connection_manager: Arc<ConnectionManager>,
    pub avatars_directory: PathBuf,
    pub files_directory: PathBuf,
    pub file_uploads_directory: PathBuf,
    pub active_file_uploads: Arc<std::sync::Mutex<HashMap<uuid::Uuid, u32>>>,
    pub(crate) login_rate_limiter: Arc<SlidingWindowRateLimiter>,
    pub(crate) register_rate_limiter: Arc<SlidingWindowRateLimiter>,
    pub(crate) shutting_down: Arc<AtomicBool>,
}

#[tokio::main]
async fn main() -> Result<()> {
    // Determine the production/development environment.
    let environment = Environment::from_environment();

    if environment.is_development() {
        let _ = dotenvy::dotenv();
    }

    println!(
        "Baihua Server - v0.1.4 ({}) by Gavin Zheng et al.",
        if environment.is_production() {
            "production"
        } else {
            "development"
        }
    );

    println!(
        "Baihua is an open-source software distributed under the Apache License, Version 2.0."
    );
    println!("No more war, Peace is our dream.");

    // Initialize the server.
    let (state, log_guard) = match initialize::initialize(environment).await {
        Ok((server_state, guard)) => (server_state, guard),
        Err(error) => {
            eprintln!(
                "\x1b[31mInitialize Error:\x1b[0m Server initialization failed: {}",
                error
            );
            return Err(error);
        }
    };

    info!(
        "Server address: {}:{}.",
        state.configuration.web.host, state.configuration.web.port
    );

    // server.rs receives notification for graceful shutdown.
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();

    let mut server_handle = if state.environment.is_development() {
        let (command_tx, command_rx) = tokio::sync::mpsc::channel::<console::CommandType>(32);
        tokio::spawn(console::console(command_tx));
        tokio::spawn(server::server(Some(command_rx), shutdown_rx, state.clone()))
    } else {
        tokio::spawn(server::server(None, shutdown_rx, state.clone()))
    };

    let (shutdown_reason, exit_code) = tokio::select! {
        _ = shutdown_signal() => {
            state.shutting_down.store(true, Ordering::SeqCst);
            let _ = shutdown_tx.send(());
            ("Received shutdown signal.", 0)
        }
        result = &mut server_handle => {
            match result {
                Ok(Ok(())) => ("Server completed.", 0),
                // Server returned an error.
                Ok(Err(error)) => {
                    error!("Server task failed: {error}");
                    ("Server task failed.", 1)
                }
                // Server panicked.
                Err(error) => {
                    error!("Server task panicked: {error}");
                    ("Server task panicked.", 1)
                }
            }
        }
    };

    if !server_handle.is_finished() {
        let _ = server_handle.await;
    }

    info!("Shutting down: {shutdown_reason}");

    info!("Saving log.");
    drop(log_guard);
    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

    println!("Goodbye!");

    if exit_code != 0 {
        std::process::exit(exit_code);
    }

    Ok(())
}

// Waits for either SIGINT or SIGTERM (UNIX) or Ctrl-C (Windows).
async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut sigint = signal(SignalKind::interrupt()).expect("Failed to set up SIGINT handler.");
        let mut sigterm =
            signal(SignalKind::terminate()).expect("Failed to set up SIGTERM handler.");

        tokio::select! {
            _ = sigint.recv() => {}
            _ = sigterm.recv() => {}
        }
    }

    #[cfg(not(unix))]
    {
        tokio::signal::ctrl_c()
            .await
            .expect("Failed to listen for Ctrl-C.");
    }
}
