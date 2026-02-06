//! HTTP API for device pairing and management

use std::sync::Arc;

use axum::{
    extract::{Path, State},
    http::StatusCode,
    routing::{delete, get, post},
    Json, Router,
};
use iroh::NodeId;
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::AppState;

/// Create the API router
pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/pair", post(pair_device))
        .route("/devices", get(list_devices))
        .route("/devices/{node_id}", delete(revoke_device))
        .with_state(state)
}

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
    State(state): State<Arc<AppState>>,
    Json(req): Json<PairRequest>,
) -> Result<Json<PairResponse>, (StatusCode, String)> {
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
    State(state): State<Arc<AppState>>,
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
    State(state): State<Arc<AppState>>,
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
