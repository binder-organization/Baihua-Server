use crate::ServerState;
use crate::infrastructure::log::LogFilterHandle;
use crate::middleware::rate_limit::SlidingWindowRateLimiter;
use rustyline::DefaultEditor;
use std::net::IpAddr;
use std::time::Duration;
use tokio::sync::mpsc;
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;

pub enum CommandType {
    // Graceful shutdown requested by the console.
    Shutdown,
    // Graceful shutdown followed by a re-execution of the same binary.
    Restart,
}

pub async fn console(
    command_tx: mpsc::Sender<CommandType>,
    state: ServerState,
    log_filter_handle: LogFilterHandle,
) {
    info!("The console is started.");
    println!("Baihua Server v{} Console", env!("CARGO_PKG_VERSION"));
    println!("Type 'help' to get help.");

    let (line_tx, mut line_rx) = mpsc::channel::<String>(8);
    // The input thread must not print the next prompt while this loop is
    // still writing the output of the previous line, so every line is
    // acknowledged only after its command has been handled.
    let (acknowledgement_tx, acknowledgement_rx) = mpsc::channel::<()>(1);

    // The editor blocks the thread while waiting for a line of input, so it
    // runs on a dedicated thread instead of occupying a tokio worker.
    std::thread::spawn(move || forward_input_lines(line_tx, acknowledgement_rx));

    let mut current_filter = initial_log_filter(&state);

    loop {
        let line = match line_rx.recv().await {
            Some(line) => line,
            // The input thread ended, so no further command can arrive.
            None => break,
        };

        let mut words = line.split_whitespace();
        let command = words.next().unwrap_or_default();
        let arguments: Vec<&str> = words.collect();

        let console_finished = dispatch_command(
            command,
            &arguments,
            &command_tx,
            &state,
            &log_filter_handle,
            &mut current_filter,
        )
        .await;

        if console_finished {
            break;
        }

        // The output of this line is complete, so the input thread may print
        // the next prompt now.
        if acknowledgement_tx.send(()).await.is_err() {
            break;
        }
    }
}

// Runs one console command and reports whether the console must stop reading
// input, which is the case for stop and restart.
async fn dispatch_command(
    command: &str,
    arguments: &[&str],
    command_tx: &mpsc::Sender<CommandType>,
    state: &ServerState,
    log_filter_handle: &LogFilterHandle,
    current_filter: &mut String,
) -> bool {
    match command {
        "" => false,
        "help" => {
            print_help();
            false
        }
        "status" => {
            print_status(state).await;
            false
        }
        "connections" => {
            print_connections(state);
            false
        }
        "ratelimit" => {
            dispatch_rate_limit_command(arguments, state).await;
            false
        }
        "log" => {
            dispatch_log_command(arguments, log_filter_handle, current_filter);
            false
        }
        "stop" | "restart" => {
            let command_type = if command == "stop" {
                CommandType::Shutdown
            } else {
                CommandType::Restart
            };
            if command_tx.send(command_type).await.is_err() {
                warn!("Sending commands failed, and the server may be down.");
            }
            true
        }
        _ => {
            println!("Unknown command: '{}'. Type 'help' to see help.", command);
            false
        }
    }
}

// Reads lines from the terminal on a dedicated thread and forwards them to
// the asynchronous console loop. After each line it waits for the
// acknowledgement, so the prompt of the next readline appears only once the
// command output has been written.
fn forward_input_lines(line_tx: mpsc::Sender<String>, mut acknowledgement_rx: mpsc::Receiver<()>) {
    let mut editor = DefaultEditor::new().unwrap_or_else(|error| {
        error!("Console startup failed: {}", error);
        panic!("Console startup failed.");
    });

    loop {
        match editor.readline("Baihua >> ") {
            Ok(line) => {
                let _ = editor.add_history_entry(&line);
                // The receiver is dropped once the console loop ends.
                if line_tx.blocking_send(line).is_err() {
                    break;
                }
                // The acknowledgement channel closes when the console loop
                // ends without answering, for example after stop.
                if acknowledgement_rx.blocking_recv().is_none() {
                    break;
                }
            }
            Err(rustyline::error::ReadlineError::Interrupted) => {
                println!("Input interrupted (Ctrl-C).");
            }
            Err(rustyline::error::ReadlineError::Eof) => {
                println!("Receive EOF (Ctrl-D).");
                break;
            }
            Err(read_error) => {
                error!("An error occurred when reading a line: {:?}", read_error);
                break;
            }
        }
    }
}

// Prints the list of supported console commands.
fn print_help() {
    let commands: [(&str, &str); 8] = [
        ("status", "Show the server status."),
        (
            "connections",
            "Show WebSocket and encrypted session details.",
        ),
        ("ratelimit [show]", "Show the rate limiter records."),
        (
            "ratelimit clear [<address>]",
            "Clear rate limiter records for one address or every address.",
        ),
        (
            "log level [<filter>]",
            "Show or replace the global log filter.",
        ),
        ("restart", "Restart the server."),
        ("stop", "Shut down the server."),
        ("help", "Get help."),
    ];

    println!("Available commands:");
    for (command, description) in commands {
        println!("  {:<28}- {}", command, description);
    }
}

// Reports the live values the server collects while it runs.
async fn print_status(state: &ServerState) {
    let pool_size = state.pool.size();
    let pool_idle = state.pool.num_idle();
    let pool_maximum = state.pool.options().get_max_connections();
    let database_reachable = sqlx::query("SELECT 1").execute(&state.pool).await.is_ok();
    let connection_manager = &state.connection_manager;
    let environment = if state.environment.is_production() {
        "production"
    } else {
        "development"
    };
    let active_uploads: u32 = state
        .active_file_uploads
        .lock()
        .map(|uploads| uploads.values().sum::<u32>())
        // Recovering from a poisoned lock still reports the tracked uploads.
        .unwrap_or_else(|poisoned| poisoned.into_inner().values().sum());

    println!(
        "Baihua Server v{} ({})",
        env!("CARGO_PKG_VERSION"),
        environment
    );
    println!(
        "Address: {}:{}",
        state.configuration.web.host, state.configuration.web.port
    );
    println!("Uptime: {}", format_duration(state.started_at.elapsed()));
    println!(
        "Database: {}, pool {}/{} ({} idle)",
        if database_reachable {
            "reachable"
        } else {
            "unreachable"
        },
        pool_size,
        pool_maximum,
        pool_idle
    );
    println!(
        "WebSocket: {} user(s) online, {} connection(s)",
        connection_manager.online_user_count(),
        connection_manager.total_connection_count()
    );
    println!(
        "Encrypted sessions: {} active, {} pending, {} in grace period",
        connection_manager.active_session_rooms().len(),
        connection_manager.pending_session_rooms().len(),
        connection_manager.grace_period_rooms().len()
    );
    println!("File uploads: {} in progress", active_uploads);
}

// Shows who is connected right now: per-room subscribers and the encrypted
// sessions that are active, waiting or inside the reconnect grace period.
fn print_connections(state: &ServerState) {
    let connection_manager = &state.connection_manager;

    println!(
        "Online users: {} ({} connection(s) total).",
        connection_manager.online_user_count(),
        connection_manager.total_connection_count()
    );

    let mut room_subscriptions = connection_manager.room_subscription_counts();
    room_subscriptions
        .sort_by_key(|(room_id, subscribers)| (std::cmp::Reverse(*subscribers), *room_id));
    if room_subscriptions.is_empty() {
        println!("Room subscriptions: none.");
    } else {
        println!("Room subscriptions:");
        const MAXIMUM_LISTED_ROOMS: usize = 20;
        for (room_id, subscribers) in room_subscriptions.iter().take(MAXIMUM_LISTED_ROOMS) {
            println!("  {}: {} subscriber(s).", room_id, subscribers);
        }
        let remaining_rooms = room_subscriptions
            .len()
            .saturating_sub(MAXIMUM_LISTED_ROOMS);
        if remaining_rooms > 0 {
            println!("  ... and {} more room(s).", remaining_rooms);
        }
    }

    let active_sessions = connection_manager.active_session_rooms();
    println!("Active encrypted sessions: {}.", active_sessions.len());
    for room_id in active_sessions {
        println!("  {}", room_id);
    }

    println!(
        "Pending encrypted sessions: {}.",
        connection_manager.pending_session_rooms().len()
    );

    let grace_periods = connection_manager.grace_period_rooms();
    if grace_periods.is_empty() {
        println!("Grace periods: none.");
    } else {
        println!("Grace periods:");
        for (room_id, offline_user_id, deadline) in grace_periods {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            println!(
                "  room {}, offline user {}, expires in {}.",
                room_id,
                offline_user_id,
                format_duration(remaining)
            );
        }
    }
}

// Handles the subcommands of the ratelimit console command.
async fn dispatch_rate_limit_command(arguments: &[&str], state: &ServerState) {
    match arguments {
        [] | ["show"] => print_rate_limiters(state).await,
        ["clear"] => clear_rate_limiters(state, None).await,
        ["clear", address] => match address.parse::<IpAddr>() {
            Ok(parsed_address) => clear_rate_limiters(state, Some(parsed_address)).await,
            Err(_) => println!("'{}' is not a valid address.", address),
        },
        _ => println!("Usage: ratelimit [show] | ratelimit clear [<address>]"),
    }
}

// Prints both sliding window limiters with their current records.
async fn print_rate_limiters(state: &ServerState) {
    print_rate_limiter("Login", &state.login_rate_limiter).await;
    print_rate_limiter("Register", &state.register_rate_limiter).await;
}

// Prints one limiter: its configured capacity and the addresses that still
// hold requests inside the current window.
async fn print_rate_limiter(label: &str, limiter: &SlidingWindowRateLimiter) {
    println!(
        "{}: {} request(s) per {} second(s) window.",
        label,
        limiter.max_requests(),
        limiter.window_secs()
    );

    let snapshot = limiter.snapshot().await;
    if snapshot.is_empty() {
        println!("  No records.");
        return;
    }
    for (address, used) in snapshot {
        println!("  {}: {}/{} used.", address, used, limiter.max_requests());
    }
}

// Clears the recorded requests of both limiters so a blocked address can keep
// testing without waiting for the window to pass.
async fn clear_rate_limiters(state: &ServerState, address: Option<IpAddr>) {
    state.login_rate_limiter.clear(address).await;
    state.register_rate_limiter.clear(address).await;
    match address {
        Some(address) => println!("Cleared rate limiter records for {}.", address),
        None => println!("Cleared rate limiter records for every address."),
    }
}

// Handles the subcommands of the log console command. A replacement filter
// takes effect for the terminal output and the log file at the same time,
// because both layers share the one global filter.
fn dispatch_log_command(
    arguments: &[&str],
    log_filter_handle: &LogFilterHandle,
    current_filter: &mut String,
) {
    match arguments {
        ["level"] => {
            println!("Current log filter: {}.", current_filter);
            println!(
                "Usage: log level <filter>, for example 'log level debug' \
                 or 'log level baihua_server=trace,sqlx=warn'."
            );
        }
        ["level", directives @ ..] => {
            // Directives are comma separated, so words typed with spaces
            // become separate directives.
            let directives = directives.join(",");
            let filter = match EnvFilter::try_new(&directives) {
                Ok(filter) => filter,
                Err(parse_error) => {
                    println!("Invalid log filter '{}': {}.", directives, parse_error);
                    return;
                }
            };
            match log_filter_handle.reload(filter) {
                Ok(()) => {
                    *current_filter = directives;
                    println!("Log filter set to {}.", current_filter);
                }
                Err(reload_error) => {
                    println!("Failed to replace the log filter: {}.", reload_error);
                }
            }
        }
        _ => println!("Usage: log level [<filter>]"),
    }
}

// Determines the filter applied at startup so `log level` reports the
// effective value. The RUST_LOG variable wins over the configured level,
// matching the precedence used when logging starts.
fn initial_log_filter(state: &ServerState) -> String {
    std::env::var("RUST_LOG")
        .ok()
        .filter(|value| EnvFilter::try_new(value).is_ok())
        .unwrap_or_else(|| state.configuration.logs.level.clone())
}

// Formats a duration as hours, minutes and seconds for console reports.
fn format_duration(duration: Duration) -> String {
    let total_seconds = duration.as_secs();
    let hours = total_seconds / 3600;
    let minutes = (total_seconds % 3600) / 60;
    let seconds = total_seconds % 60;
    if hours > 0 {
        format!("{}h {}m {}s", hours, minutes, seconds)
    } else if minutes > 0 {
        format!("{}m {}s", minutes, seconds)
    } else {
        format!("{}s", seconds)
    }
}

#[cfg(test)]
mod tests {
    use super::format_duration;
    use std::time::Duration;

    #[test]
    fn format_duration_formats_seconds_minutes_and_hours() {
        assert_eq!(format_duration(Duration::from_secs(0)), "0s");
        assert_eq!(format_duration(Duration::from_secs(59)), "59s");
        assert_eq!(format_duration(Duration::from_secs(60)), "1m 0s");
        assert_eq!(format_duration(Duration::from_secs(3600)), "1h 0m 0s");
        assert_eq!(format_duration(Duration::from_secs(3725)), "1h 2m 5s");
    }
}
