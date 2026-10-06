use crate::ServerState;
use crate::console::CommandType;
use crate::greet::greet;
use crate::health::health_check;
use crate::middleware;
use crate::websocket;
use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::routing::get;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use tracing::info;

pub async fn server(
    command_rx: Option<tokio::sync::mpsc::Receiver<CommandType>>,
    mut shutdown_rx: tokio::sync::oneshot::Receiver<()>,
    state: ServerState,
) -> Result<(), anyhow::Error> {
    let address = format!(
        "{}:{}",
        state.configuration.web.host, state.configuration.web.port
    );

    info!("Server running at {}.", &address);

    let state = Arc::new(state);

    let app = Router::new()
        .route("/greet", get(greet))
        .route("/health", get(health_check))
        .route("/websocket", get(websocket::handler::ws_handler))
        .route(
            "/static/avatars/{filename}",
            axum::routing::get(crate::user::avatar::serve_avatar_file),
        )
        .nest("/api/v1", api_v1(state.clone()))
        .layer(axum::middleware::from_fn(middleware::tracing::tracing))
        .layer(axum::middleware::from_fn(middleware::error::panic))
        .layer(axum::middleware::from_fn(middleware::error::not_found))
        .layer(axum::middleware::from_fn(
            middleware::error::method_not_allowed,
        ))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            middleware::error::payload_too_large,
        ))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            middleware::error::service_unavailable,
        ))
        .layer(DefaultBodyLimit::max(
            state.configuration.web.max_body_size as usize,
        ))
        .with_state(state.clone());

    let listener = tokio::net::TcpListener::bind(&address).await?;

    // Shutdown triggered by main.rs (OS signal) or console command.
    let shutdown_handler = async move {
        // If no command channel exists (production), this future never resolves.
        let command_future = async {
            let mut rx = match command_rx {
                Some(rx) => rx,
                None => std::future::pending().await,
            };
            rx.recv().await
        };

        tokio::select! {
            _ = &mut shutdown_rx => {
                info!("Received OS shutdown signal.");
            }
            reason = command_future => {
                match reason {
                    Some(CommandType::Shutdown) => {
                        info!("Received shutdown command from console.");
                    }
                    Some(CommandType::Restart) => {
                        state.restart_requested.store(true, Ordering::SeqCst);
                        info!("Received restart command from console.");
                    }
                    None => {
                        info!("Command channel closed.");
                    }
                }
            }
        }

        state.connection_manager.initiate_shutdown();
    };

    // Start the server with graceful shutdown and proper error handling.
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_handler)
        .await?;

    info!("Server shut down.");
    Ok(())
}

fn api_v1(state: Arc<ServerState>) -> Router<Arc<ServerState>> {
    Router::new()
        .nest("/user", crate::user::router(state.clone()))
        .nest("/chat", crate::chat::router(state.clone()))
        .layer(axum::middleware::from_fn_with_state(
            state,
            middleware::error::request_timeout,
        ))
}
