use crate::ServerState;
use crate::common::error::ErrorResponse;
use axum::{
    extract::{Request, State},
    middleware::Next,
    response::{IntoResponse, Response},
};
use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::RwLock;

struct RateLimitState {
    entries: HashMap<IpAddr, Vec<Instant>>,
    last_sweep: Instant,
}

pub(crate) struct SlidingWindowRateLimiter {
    max_requests: u32,
    window_secs: u64,
    inner: RwLock<RateLimitState>,
}

impl SlidingWindowRateLimiter {
    pub fn new(max_requests: u32, window_secs: u64) -> Self {
        Self {
            max_requests,
            window_secs,
            inner: RwLock::new(RateLimitState {
                entries: HashMap::new(),
                last_sweep: Instant::now(),
            }),
        }
    }

    // Returns true if the request should be allowed, false if rate limited.
    pub(crate) async fn allow(&self, ip: IpAddr) -> bool {
        let mut state = self.inner.write().await;
        let now = Instant::now();
        let window = std::time::Duration::from_secs(self.window_secs);

        // Sweep the whole map once per window to evict IPs that have gone
        // quiet. Without this, each distinct client IP would leave a permanent
        // entry and the map would grow without bound over time.
        if now.duration_since(state.last_sweep) >= window {
            state.entries.retain(|_, timestamps| {
                timestamps.retain(|&timestamp| now.duration_since(timestamp) < window);
                !timestamps.is_empty()
            });
            state.last_sweep = now;
        }

        let entries = state.entries.entry(ip).or_default();

        // Prune timestamps outside the window.
        entries.retain(|&timestamp| now.duration_since(timestamp) < window);

        if entries.len() >= self.max_requests as usize {
            return false;
        }

        entries.push(now);
        true
    }

    // Maximum number of requests one address may send per window.
    pub(crate) fn max_requests(&self) -> u32 {
        self.max_requests
    }

    // Length of the sliding window in seconds.
    pub(crate) fn window_secs(&self) -> u64 {
        self.window_secs
    }

    // Returns every tracked address with the number of requests still counted
    // inside the current window, ordered from most to least used.
    pub(crate) async fn snapshot(&self) -> Vec<(IpAddr, usize)> {
        let state = self.inner.read().await;
        let now = Instant::now();
        let window = std::time::Duration::from_secs(self.window_secs);
        let mut records: Vec<(IpAddr, usize)> = state
            .entries
            .iter()
            .filter_map(|(ip, timestamps)| {
                let used = timestamps
                    .iter()
                    .filter(|timestamp| now.duration_since(**timestamp) < window)
                    .count();
                (used > 0).then_some((*ip, used))
            })
            .collect();
        records.sort_by_key(|(ip, used)| (std::cmp::Reverse(*used), *ip));
        records
    }

    // Drops the recorded requests for one address, or for every address when
    // no address is given.
    pub(crate) async fn clear(&self, ip: Option<IpAddr>) {
        let mut state = self.inner.write().await;
        match ip {
            Some(ip) => {
                state.entries.remove(&ip);
            }
            None => state.entries.clear(),
        }
    }
}

fn extract_client_ip(request: &Request) -> Option<IpAddr> {
    // Priority 1: X-Forwarded-For (standard reverse proxy header).
    if let Some(value) = request.headers().get("x-forwarded-for")
        && let Ok(value) = value.to_str()
        && let Some(ip_str) = value.split(',').next().map(|s| s.trim())
        && let Ok(ip) = ip_str.parse::<IpAddr>()
    {
        return Some(ip);
    }

    // Priority 2: X-Real-IP (common nginx header).
    if let Some(value) = request.headers().get("x-real-ip")
        && let Ok(value) = value.to_str()
        && let Ok(ip) = value.parse::<IpAddr>()
    {
        return Some(ip);
    }

    None
}

pub async fn rate_limit_login(
    state: State<Arc<ServerState>>,
    request: Request,
    next: Next,
) -> Response {
    if let Some(ip) = extract_client_ip(&request)
        && !state.login_rate_limiter.allow(ip).await
    {
        return ErrorResponse::TooManyRequests(
            "Too many login attempts. Please try again later.".to_string(),
        )
        .into_response();
    }

    next.run(request).await
}

pub async fn rate_limit_register(
    state: State<Arc<ServerState>>,
    request: Request,
    next: Next,
) -> Response {
    if let Some(ip) = extract_client_ip(&request)
        && !state.register_rate_limiter.allow(ip).await
    {
        return ErrorResponse::TooManyRequests(
            "Too many registration attempts. Please try again later.".to_string(),
        )
        .into_response();
    }

    next.run(request).await
}

#[cfg(test)]
mod tests {
    use super::SlidingWindowRateLimiter;
    use std::net::IpAddr;

    #[tokio::test]
    async fn snapshot_counts_the_requests_inside_the_window() {
        let limiter = SlidingWindowRateLimiter::new(3, 60);
        let first_address: IpAddr = "192.0.2.1"
            .parse()
            .expect("The literal address must parse.");
        let second_address: IpAddr = "192.0.2.2"
            .parse()
            .expect("The literal address must parse.");

        assert!(limiter.allow(first_address).await);
        assert!(limiter.allow(first_address).await);
        assert!(limiter.allow(second_address).await);

        assert_eq!(
            limiter.snapshot().await,
            vec![(first_address, 2), (second_address, 1)]
        );
    }

    #[tokio::test]
    async fn clear_removes_one_address_and_keeps_the_others() {
        let limiter = SlidingWindowRateLimiter::new(3, 60);
        let blocked_address: IpAddr = "192.0.2.1"
            .parse()
            .expect("The literal address must parse.");
        let other_address: IpAddr = "192.0.2.2"
            .parse()
            .expect("The literal address must parse.");
        limiter.allow(blocked_address).await;
        limiter.allow(other_address).await;

        limiter.clear(Some(blocked_address)).await;
        assert_eq!(limiter.snapshot().await, vec![(other_address, 1)]);

        limiter.clear(None).await;
        assert!(limiter.snapshot().await.is_empty());
    }
}
