use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerConfiguration {
    pub web: WebConfiguration,
    pub logs: LogsConfiguration,
    pub database: DatabaseConfiguration,
    pub user: UserConfiguration,
    #[serde(default)]
    pub rate_limit: RateLimitConfiguration,
    #[serde(default)]
    pub websocket: WebSocketConfiguration,
    #[serde(default)]
    pub room_request: RoomRequestConfiguration,
    #[serde(default)]
    pub avatar: AvatarConfiguration,
}

// Web server configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebConfiguration {
    pub host: String,
    pub port: u16,
    #[serde(default = "default_max_body_size")]
    pub max_body_size: u32,
    #[serde(default = "default_request_timeout_secs")]
    pub request_timeout_secs: u64,
}

// Log configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogsConfiguration {
    #[serde(default = "default_log_level")]
    pub level: String,
}

// Database connection pool configuration
// Connection credentials are provided via environment variables to avoid
// hardcoding credentials in config files. See .env.example for the
// required POSTGRES_* variables.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatabaseConfiguration {
    #[serde(default = "default_max_connections")]
    pub max_connections: u32,
    #[serde(default = "default_min_connections")]
    pub min_connections: u32,
    #[serde(default = "default_pool_idle_timeout_secs")]
    pub pool_idle_timeout_secs: u64,
    #[serde(default = "default_pool_max_lifetime_secs")]
    pub pool_max_lifetime_secs: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserConfiguration {
    #[serde(default = "default_username_length")]
    pub minimum_username_length: u32,
    #[serde(default = "default_maximum_username_length")]
    pub maximum_username_length: u32,
    #[serde(default = "default_jsonwebtoken_expiration_hours")]
    pub jsonwebtoken_expiration_hours: u32,
    #[serde(default = "default_bcrypt_cost")]
    pub bcrypt_cost: u32,
}

// Rate limiting configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RateLimitConfiguration {
    #[serde(default = "default_login_max_requests")]
    pub login_max_requests: u32,
    #[serde(default = "default_login_window_secs")]
    pub login_window_secs: u64,
    #[serde(default = "default_register_max_requests")]
    pub register_max_requests: u32,
    #[serde(default = "default_register_window_secs")]
    pub register_window_secs: u64,
}

impl Default for RateLimitConfiguration {
    fn default() -> Self {
        Self {
            login_max_requests: default_login_max_requests(),
            login_window_secs: default_login_window_secs(),
            register_max_requests: default_register_max_requests(),
            register_window_secs: default_register_window_secs(),
        }
    }
}

// WebSocket configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebSocketConfiguration {
    #[serde(default = "default_heartbeat_interval_secs")]
    pub heartbeat_interval_secs: u64,
    #[serde(default = "default_message_rate_limit")]
    pub message_rate_limit: u32,
    #[serde(default = "default_message_rate_window_secs")]
    pub message_rate_window_secs: u64,
    #[serde(default = "default_token_revalidate_interval_secs")]
    pub token_revalidate_interval_secs: u64,
    #[serde(default = "default_encrypted_grace_period_secs")]
    pub encrypted_grace_period_secs: u64,
    #[serde(default)]
    pub allowed_origins: Vec<String>,
}

impl Default for WebSocketConfiguration {
    fn default() -> Self {
        Self {
            heartbeat_interval_secs: default_heartbeat_interval_secs(),
            message_rate_limit: default_message_rate_limit(),
            message_rate_window_secs: default_message_rate_window_secs(),
            token_revalidate_interval_secs: default_token_revalidate_interval_secs(),
            encrypted_grace_period_secs: default_encrypted_grace_period_secs(),
            allowed_origins: Vec::new(),
        }
    }
}

// Room request configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoomRequestConfiguration {
    #[serde(default = "default_room_request_message_max_bytes")]
    pub message_max_bytes: u32,
    #[serde(default = "default_room_request_expiry_hours")]
    pub expiry_hours: u64,
    #[serde(default = "default_room_request_pending_max")]
    pub pending_max: u32,
    #[serde(default = "default_room_request_send_daily_limit")]
    pub send_daily_limit: u32,
}

impl Default for RoomRequestConfiguration {
    fn default() -> Self {
        Self {
            message_max_bytes: default_room_request_message_max_bytes(),
            expiry_hours: default_room_request_expiry_hours(),
            pending_max: default_room_request_pending_max(),
            send_daily_limit: default_room_request_send_daily_limit(),
        }
    }
}

// Avatar configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AvatarConfiguration {
    #[serde(default = "default_avatar_max_bytes")]
    pub max_bytes: u32,
}

impl Default for AvatarConfiguration {
    fn default() -> Self {
        Self {
            max_bytes: default_avatar_max_bytes(),
        }
    }
}

// Default value functions
fn default_max_body_size() -> u32 {
    10 * 1024 * 1024 // 10 MB
}
fn default_request_timeout_secs() -> u64 {
    30
}
fn default_log_level() -> String {
    "info".to_string()
}
fn default_max_connections() -> u32 {
    20
}
fn default_min_connections() -> u32 {
    5
}
fn default_pool_idle_timeout_secs() -> u64 {
    600
}
fn default_pool_max_lifetime_secs() -> u64 {
    3600
}
fn default_username_length() -> u32 {
    3
}
fn default_maximum_username_length() -> u32 {
    40
}
fn default_jsonwebtoken_expiration_hours() -> u32 {
    24
}
fn default_bcrypt_cost() -> u32 {
    10
}
fn default_login_max_requests() -> u32 {
    60
}
fn default_login_window_secs() -> u64 {
    60
}
fn default_register_max_requests() -> u32 {
    30
}
fn default_register_window_secs() -> u64 {
    60
}
fn default_heartbeat_interval_secs() -> u64 {
    30
}
fn default_message_rate_limit() -> u32 {
    30
}
fn default_message_rate_window_secs() -> u64 {
    10
}
fn default_token_revalidate_interval_secs() -> u64 {
    600
}
fn default_encrypted_grace_period_secs() -> u64 {
    30
}
fn default_room_request_message_max_bytes() -> u32 {
    500
}
fn default_room_request_expiry_hours() -> u64 {
    120 // 5 days
}
fn default_room_request_pending_max() -> u32 {
    50 // per receiver inbox cap
}
fn default_room_request_send_daily_limit() -> u32 {
    20 // per sender, 24h window
}
fn default_avatar_max_bytes() -> u32 {
    2 * 1024 * 1024 // 2 MB
}

impl Default for ServerConfiguration {
    fn default() -> Self {
        Self {
            web: WebConfiguration {
                host: "127.0.0.1".to_string(),
                port: 2424,
                max_body_size: default_max_body_size(),
                request_timeout_secs: default_request_timeout_secs(),
            },
            logs: LogsConfiguration {
                level: default_log_level(),
            },
            database: DatabaseConfiguration {
                max_connections: default_max_connections(),
                min_connections: default_min_connections(),
                pool_idle_timeout_secs: default_pool_idle_timeout_secs(),
                pool_max_lifetime_secs: default_pool_max_lifetime_secs(),
            },
            user: UserConfiguration {
                minimum_username_length: default_username_length(),
                maximum_username_length: default_maximum_username_length(),
                jsonwebtoken_expiration_hours: default_jsonwebtoken_expiration_hours(),
                bcrypt_cost: default_bcrypt_cost(),
            },
            rate_limit: RateLimitConfiguration::default(),
            websocket: WebSocketConfiguration::default(),
            room_request: RoomRequestConfiguration::default(),
            avatar: AvatarConfiguration {
                max_bytes: default_avatar_max_bytes(),
            },
        }
    }
}

impl ServerConfiguration {
    /// Returns the default configuration as a TOML string with detailed
    /// comments describing each field and its possible values.
    pub fn default_config_content() -> String {
        r#"# ---- Web Server ----

[web]
# The IP address or hostname to bind the server to.
# When omitted or left empty, the server binds to the loopback address,
# equivalent to "127.0.0.1". Set to "0.0.0.0" to accept connections from
# all network interfaces. Can also be overridden by the BIND_HOST
# environment variable.
host = "127.0.0.1"

# The TCP port the server listens on for incoming HTTP and WebSocket
# connections. Must be between 1 and 65535.
port = 2424

# The maximum size of a request body in bytes.
# When omitted, defaults to 10485760 (10 MB).
#
# Requests exceeding this size are rejected with a 400 Bad Request.
# Set a lower value in production to reduce exposure to large payloads.
max_body_size = 10485760

# The maximum time (in seconds) an HTTP request may take before it is
# aborted with a 408 Request Timeout. When omitted, defaults to 30.
# Applies to /api/v1 requests only; WebSocket connections are exempt.
request_timeout_secs = 30

# ---- Logging ----

[logs]
# The minimum log level to output. Messages at this level or higher are
# recorded. When this field is absent from the configuration file, the
# default value "info" is used.
#
# Available options (in ascending order of severity):
#   - "trace"   — most verbose, includes all diagnostic details
#   - "debug"   — detailed information for debugging
#   - "info"    — general operational messages (default)
#   - "warn"    — potentially harmful situations
#   - "error"   — error events that might still allow the application
#                 to continue running
level = "info"

# ---- Database Connection Pool ----
# Note: Connection credentials (user, password, host, port, database
# name) are configured through environment variables, not in this file.
# Refer to .env.example for the required POSTGRES_* variables.

[database]
# The maximum number of concurrent connections the connection pool can
# hold. When omitted, defaults to 20.
#
# A higher value allows more concurrent database operations but consumes
# more database server resources. Tune this based on your database
# server's capacity and expected workload.
max_connections = 20

# The minimum number of idle connections the pool maintains ready for
# immediate use. When omitted, defaults to 5.
#
# A higher value reduces latency for sudden traffic spikes but keeps
# more connections open when idle.
min_connections = 5

# The maximum idle duration (in seconds) for a pooled connection.
# Connections idle longer than this are closed. When omitted, defaults
# to 600 (10 minutes).
pool_idle_timeout_secs = 600

# The maximum lifetime (in seconds) for a pooled connection regardless
# of activity. Connections older than this are replaced. When omitted,
# defaults to 3600 (1 hour).
pool_max_lifetime_secs = 3600

# ---- User & Authentication ----

[user]
# The minimum length (in characters) required for usernames during
# registration. When omitted, defaults to 3.
#
# Must be at least 1 and must not exceed maximum_username_length.
minimum_username_length = 3

# The maximum length (in characters) allowed for usernames.
# When omitted, defaults to 40.
#
# Must be at least 1 and must not be less than minimum_username_length.
maximum_username_length = 40

# The number of hours after which a issued JWT token expires. The user
# must re-authenticate to obtain a new token after expiration. When
# omitted, defaults to 24.
#
# Must be greater than 0.
jsonwebtoken_expiration_hours = 24

# The bcrypt cost factor used when hashing passwords. When omitted,
# defaults to 10. Must be between 4 and 31; higher values are slower but
# more resistant to brute-force attacks.
bcrypt_cost = 10

# ---- Rate Limiting ----

[rate_limit]
# Maximum number of login requests allowed from a single IP within the
# window. When omitted, defaults to 60.
login_max_requests = 60

# Duration of the rate limit window in seconds for login requests.
# When omitted, defaults to 60.
login_window_secs = 60

# Maximum number of registration requests allowed from a single IP
# within the window. When omitted, defaults to 30.
register_max_requests = 30

# Duration of the rate limit window in seconds for registration
# requests. When omitted, defaults to 60.
register_window_secs = 60

# ---- WebSocket ----

[websocket]
# Interval (in seconds) between WebSocket protocol-level PING frames
# sent to the client. When omitted, defaults to 30.
#
# If the connection is behind a proxy or load balancer with a shorter
# idle timeout, set this to a value lower than that timeout.
heartbeat_interval_secs = 30

# Maximum number of WebSocket text messages a client can send within
# the rate limit window. When omitted, defaults to 30.
message_rate_limit = 30

# Duration of the rate limit window in seconds for WebSocket messages.
# When omitted, defaults to 10.
message_rate_window_secs = 10

# Interval (in seconds) between JWT token re-validation checks for an
# active WebSocket connection. When omitted, defaults to 600 (10 min).
token_revalidate_interval_secs = 600

# Time allowed for an encrypted chat partner to reconnect after disconnecting.
# When omitted, defaults to 30 seconds. Must be greater than 0.
encrypted_grace_period_secs = 30

# List of browser origins allowed to open a WebSocket connection. When a
# browser sends an Origin header that is not in this list, the upgrade is
# rejected with a 403. Non-browser clients (which send no Origin header)
# are always allowed. When empty, the check is disabled. Set this in
# production to prevent cross-site WebSocket hijacking.
allowed_origins = []

# ---- Room Requests ----

[room_request]
# Maximum length (in bytes) of the request message a user can send when
# requesting a private room. When omitted, defaults to 500.
message_max_bytes = 500

# How long (in hours) a pending room request stays valid before it is
# automatically marked as expired (5 days by default).
expiry_hours = 120

# Maximum number of pending room requests a single user can have in their
# inbox at once. When omitted, defaults to 50.
pending_max = 50

# Maximum number of room requests a single user can send per day (24h
# sliding window). When omitted, defaults to 20.
send_daily_limit = 20

# ---- Avatar ----

[avatar]
# The maximum size of an uploaded avatar image in bytes.
# When omitted, defaults to 2097152 (2 MB).
#
# Uploads exceeding this size are rejected with a 413 Payload Too Large.
max_bytes = 2097152
"#
        .to_string()
    }

    pub fn validate(&self) -> Result<()> {
        if self.web.port == 0 {
            bail!("The server port cannot be 0.");
        }
        if self.web.max_body_size == 0 {
            bail!("The maximum request body size cannot be 0.");
        }
        if self.web.request_timeout_secs == 0 {
            bail!("The request timeout cannot be 0.");
        }

        if self.database.max_connections < self.database.min_connections {
            bail!(
                "The maximum number of connections in the database cannot be less than the minimum number of connections."
            );
        }
        if self.database.pool_idle_timeout_secs == 0 {
            bail!("The pool idle timeout cannot be 0.");
        }
        if self.database.pool_max_lifetime_secs == 0 {
            bail!("The pool max lifetime cannot be 0.");
        }

        let valid_log_levels = ["trace", "debug", "info", "warn", "error"];
        if !valid_log_levels.contains(&self.logs.level.as_str()) {
            bail!("Invalid log level: {}", self.logs.level);
        }

        if self.user.minimum_username_length > self.user.maximum_username_length {
            bail!(
                "The minimum username length cannot be greater than the maximum username length."
            );
        }
        if self.user.minimum_username_length == 0 || self.user.maximum_username_length == 0 {
            bail!("The minimum or maximum value of the username length cannot be 0.");
        }
        if self.user.jsonwebtoken_expiration_hours == 0 {
            bail!("The jsonwebtoken expiration hours cannot be 0.");
        }
        if !(4..=31).contains(&self.user.bcrypt_cost) {
            bail!("The bcrypt cost must be between 4 and 31.");
        }

        if self.rate_limit.login_max_requests == 0 {
            bail!("The login rate limit max requests cannot be 0.");
        }
        if self.rate_limit.login_window_secs == 0 {
            bail!("The login rate limit window cannot be 0.");
        }
        if self.rate_limit.register_max_requests == 0 {
            bail!("The register rate limit max requests cannot be 0.");
        }
        if self.rate_limit.register_window_secs == 0 {
            bail!("The register rate limit window cannot be 0.");
        }

        if self.websocket.heartbeat_interval_secs == 0 {
            bail!("The WebSocket heartbeat interval cannot be 0.");
        }
        if self.websocket.message_rate_limit == 0 {
            bail!("The WebSocket message rate limit cannot be 0.");
        }
        if self.websocket.message_rate_window_secs == 0 {
            bail!("The WebSocket message rate window cannot be 0.");
        }
        if self.websocket.token_revalidate_interval_secs == 0 {
            bail!("The WebSocket token revalidate interval cannot be 0.");
        }
        if self.websocket.encrypted_grace_period_secs == 0 {
            bail!("The encrypted session grace period cannot be 0.");
        }

        if self.room_request.message_max_bytes == 0 {
            bail!("The room request message max bytes cannot be 0.");
        }
        if self.room_request.expiry_hours == 0 {
            bail!("The room request expiry hours cannot be 0.");
        }
        if self.room_request.pending_max == 0 {
            bail!("The room request pending max cannot be 0.");
        }
        if self.room_request.send_daily_limit == 0 {
            bail!("The room request send daily limit cannot be 0.");
        }

        if self.avatar.max_bytes == 0 {
            bail!("The avatar max bytes cannot be 0.");
        }

        Ok(())
    }
}
