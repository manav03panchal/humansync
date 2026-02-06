//! HTTP API for device pairing and management

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Instant;

use axum::{
    extract::{ConnectInfo, FromRequestParts, Path, State},
    http::StatusCode,
    routing::{delete, get, post},
    Json, Router,
};
use iroh::NodeId;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::AppState;

/// Extractor for the client's IP address.
///
/// Reads from `ConnectInfo<SocketAddr>` request extensions when available
/// (i.e. when the server is started with `into_make_service_with_connect_info`).
/// Falls back to `127.0.0.1` when unavailable (e.g. in test environments).
struct ClientIp(IpAddr);

impl<S: Send + Sync> FromRequestParts<S> for ClientIp {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        _state: &S,
    ) -> Result<Self, Self::Rejection> {
        let ip = parts
            .extensions
            .get::<ConnectInfo<SocketAddr>>()
            .map(|ci| ci.0.ip())
            .unwrap_or(IpAddr::V4(Ipv4Addr::LOCALHOST));
        Ok(ClientIp(ip))
    }
}

/// Maximum number of pairing attempts per IP within the time window.
const RATE_LIMIT_MAX_REQUESTS: usize = 5;

/// Sliding window duration for rate limiting (in seconds).
const RATE_LIMIT_WINDOW_SECS: u64 = 60;

/// In-memory rate limiter that tracks request timestamps per IP address.
///
/// Uses a sliding window approach: for each incoming request, expired entries
/// (older than `RATE_LIMIT_WINDOW_SECS`) are pruned, and then the current
/// count is checked against `RATE_LIMIT_MAX_REQUESTS`.
#[derive(Debug, Clone)]
pub struct RateLimiter {
    /// Map from IP address to a list of request timestamps within the current window.
    requests: Arc<Mutex<HashMap<IpAddr, Vec<Instant>>>>,
}

impl RateLimiter {
    /// Create a new, empty rate limiter.
    pub fn new() -> Self {
        Self {
            requests: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Check whether the given IP address is allowed to make a request.
    ///
    /// Returns `true` if the request is allowed, `false` if the rate limit
    /// has been exceeded. As a side-effect, expired timestamps are pruned and
    /// (if allowed) the current timestamp is recorded.
    pub fn check(&self, ip: IpAddr) -> bool {
        let now = Instant::now();
        let window = std::time::Duration::from_secs(RATE_LIMIT_WINDOW_SECS);
        let mut map = self.requests.lock();

        let timestamps = map.entry(ip).or_default();

        // Remove timestamps outside the sliding window
        timestamps.retain(|&t| now.duration_since(t) < window);

        if timestamps.len() >= RATE_LIMIT_MAX_REQUESTS {
            false
        } else {
            timestamps.push(now);
            true
        }
    }
}

impl Default for RateLimiter {
    fn default() -> Self {
        Self::new()
    }
}

/// Create the API router
pub fn router(state: Arc<AppState>) -> Router {
    let rate_limiter = RateLimiter::new();

    Router::new()
        .route("/health", get(health))
        .route("/pair", post(pair_device))
        .route("/devices", get(list_devices))
        .route("/devices/{node_id}", delete(revoke_device))
        .with_state((state, rate_limiter))
}

/// Shared state type for all route handlers.
type SharedState = (Arc<AppState>, RateLimiter);

/// Health check endpoint
async fn health() -> &'static str {
    "ok"
}

/// Request body for device pairing
#[derive(Debug, Deserialize)]
struct PairRequest {
    /// Device's Iroh NodeId
    node_id: String,
    /// Human-readable device name
    device_name: String,
    /// Server password
    password: String,
}

/// Response for successful pairing
#[derive(Debug, Serialize)]
struct PairResponse {
    /// Server's NodeId
    server_node_id: String,
    /// List of all registered devices
    devices: Vec<DeviceInfo>,
}

/// Device information in API responses
#[derive(Debug, Serialize)]
struct DeviceInfo {
    node_id: String,
    name: String,
    paired_at: String,
    last_seen: String,
}

/// Pair a new device
async fn pair_device(
    State((state, rate_limiter)): State<SharedState>,
    ClientIp(ip): ClientIp,
    Json(req): Json<PairRequest>,
) -> Result<Json<PairResponse>, (StatusCode, String)> {

    // Rate limit check
    if !rate_limiter.check(ip) {
        warn!(ip = %ip, "Pairing rate limit exceeded");
        return Err((
            StatusCode::TOO_MANY_REQUESTS,
            "Too many pairing attempts. Please try again later.".to_string(),
        ));
    }

    // Verify password
    if req.password != state.password {
        warn!(node_id = %req.node_id, "Pairing failed: invalid password");
        return Err((StatusCode::UNAUTHORIZED, "Invalid password".to_string()));
    }

    // Parse node ID
    let node_id: NodeId = req
        .node_id
        .parse()
        .map_err(|_| (StatusCode::BAD_REQUEST, "Invalid node_id".to_string()))?;

    // Add device to SQLite registry
    state
        .db
        .add_device(node_id, &req.device_name)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    // Update the _devices Automerge doc so the change propagates via sync
    {
        let device_info = humansync::DeviceInfo::new(node_id, &req.device_name);
        state
            .registry_store
            .add(device_info)
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("failed to update _devices doc: {e}")))?;
        state
            .registry_store
            .save()
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("failed to save _devices doc: {e}")))?;
    }

    info!(node_id = %node_id, name = %req.device_name, "Device paired successfully");

    // Get all devices
    let devices = state
        .db
        .list_devices()
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .into_iter()
        .map(|d| DeviceInfo {
            node_id: d.node_id.to_string(),
            name: d.name,
            paired_at: d.paired_at.to_rfc3339(),
            last_seen: d.last_seen.to_rfc3339(),
        })
        .collect();

    Ok(Json(PairResponse {
        server_node_id: state.node_id.to_string(),
        devices,
    }))
}

/// List all registered devices
async fn list_devices(
    State((state, _rate_limiter)): State<SharedState>,
    headers: axum::http::HeaderMap,
) -> Result<Json<Vec<DeviceInfo>>, (StatusCode, String)> {
    // Verify the requester is a registered device
    let node_id_header = headers
        .get("X-Node-Id")
        .and_then(|v| v.to_str().ok())
        .ok_or((StatusCode::UNAUTHORIZED, "Missing X-Node-Id header".to_string()))?;

    let node_id: NodeId = node_id_header
        .parse()
        .map_err(|_| (StatusCode::BAD_REQUEST, "Invalid node_id".to_string()))?;

    if !state
        .db
        .contains(&node_id)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    {
        warn!(node_id = %node_id, "Unauthorized device list request");
        return Err((StatusCode::UNAUTHORIZED, "Device not registered".to_string()));
    }

    // Touch the device to update last_seen
    let _ = state.db.touch_device(&node_id);

    let devices = state
        .db
        .list_devices()
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .into_iter()
        .map(|d| DeviceInfo {
            node_id: d.node_id.to_string(),
            name: d.name,
            paired_at: d.paired_at.to_rfc3339(),
            last_seen: d.last_seen.to_rfc3339(),
        })
        .collect();

    Ok(Json(devices))
}

/// Request body for device revocation
#[derive(Debug, Deserialize)]
struct RevokeRequest {
    password: String,
}

/// Revoke a device
async fn revoke_device(
    State((state, _rate_limiter)): State<SharedState>,
    Path(node_id_str): Path<String>,
    Json(req): Json<RevokeRequest>,
) -> Result<StatusCode, (StatusCode, String)> {
    // Verify password
    if req.password != state.password {
        warn!(node_id = %node_id_str, "Revocation failed: invalid password");
        return Err((StatusCode::UNAUTHORIZED, "Invalid password".to_string()));
    }

    // Parse node ID
    let node_id: NodeId = node_id_str
        .parse()
        .map_err(|_| (StatusCode::BAD_REQUEST, "Invalid node_id".to_string()))?;

    // Remove device from SQLite registry
    let removed = state
        .db
        .remove_device(&node_id)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    if removed {
        // Update the _devices Automerge doc so the revocation propagates via sync
        let _ = state.registry_store.remove(&node_id);
        let _ = state.registry_store.save();

        info!(node_id = %node_id, "Device revoked");
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err((StatusCode::NOT_FOUND, "Device not found".to_string()))
    }
}
