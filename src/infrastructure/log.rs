use crate::Directory;
use crate::infrastructure::config::ServerConfiguration;
use crate::infrastructure::environment::Environment;
use tracing::subscriber::set_global_default;
use tracing_appender::rolling::{RollingFileAppender, Rotation};
use tracing_subscriber::{
    EnvFilter, Registry,
    fmt::{self, format::FmtSpan},
    layer::SubscriberExt,
    reload,
};

// Runtime handle that replaces the global log filter. The same filter governs
// both the terminal and the file layer, so one reload updates both outputs.
pub type LogFilterHandle = reload::Handle<EnvFilter, Registry>;

// Keeps the log system alive after initialization and carries the handle used
// to change the filter while the server is running.
pub struct LogSystem {
    // Dropping the guard flushes every buffered log record to disk.
    pub guard: tracing_appender::non_blocking::WorkerGuard,
    // Consumed by the development console so `log level` can reload the filter.
    pub filter_handle: LogFilterHandle,
}

pub fn init_log(
    directory: &Directory,
    configuration: &ServerConfiguration,
    env: Environment,
) -> Result<LogSystem, Box<dyn std::error::Error>> {
    let logs_dir = directory.log.clone();
    let config = &configuration.logs;

    // Create a log directory.
    let file_appender = RollingFileAppender::builder()
        .rotation(Rotation::HOURLY)
        .filename_prefix("server")
        .filename_suffix("log")
        .build(logs_dir)
        .expect("Creating a log file writer failed.");

    let (writer, guard) = tracing_appender::non_blocking(file_appender);

    // Set up log system.
    // When the server runs in a development environment, enable more settings.
    if env.is_development() {
        let filter =
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(&config.level));
        // Wrapping the filter keeps it reloadable while the subscriber itself
        // stays installed for the whole process lifetime.
        let (filter_layer, filter_handle) = reload::Layer::new(filter);

        let file = fmt::layer()
            .json()
            .with_writer(writer.clone())
            .with_ansi(false)
            .with_span_events(FmtSpan::CLOSE)
            .with_current_span(false)
            .with_thread_names(true);

        let stdout = fmt::layer()
            .with_writer(std::io::stderr)
            .with_ansi(true)
            .with_level(true)
            .with_target(true)
            .with_thread_ids(false)
            .with_thread_names(true);

        let subscriber = Registry::default()
            .with(filter_layer)
            .with(stdout)
            .with(file);

        set_global_default(subscriber)?;

        tracing::info!("Log system initialization complete.");

        return Ok(LogSystem {
            guard,
            filter_handle,
        });
    }

    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(&config.level));
    let (filter_layer, filter_handle) = reload::Layer::new(filter);

    let file = fmt::layer()
        .json()
        .with_writer(writer)
        .with_ansi(false)
        .with_span_events(FmtSpan::CLOSE)
        .with_current_span(false)
        .with_thread_names(true);

    let stdout = fmt::layer()
        .with_writer(std::io::stderr)
        .with_ansi(true)
        .with_level(true)
        .with_target(true)
        .with_thread_ids(true);

    let subscriber = Registry::default()
        .with(filter_layer)
        .with(stdout)
        .with(file);

    set_global_default(subscriber)?;

    tracing::info!("Log system initialization complete.");

    Ok(LogSystem {
        guard,
        filter_handle,
    })
}
