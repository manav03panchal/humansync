//! Integration tests for the HTTP API
//!
//! These tests exercise the API endpoints using tower::ServiceExt::oneshot()
//! without starting a real server or requiring network access.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use humansync::DeviceRegistryStore;
use humansync_server::{api, db::Database, AppState};
use iroh::{NodeId, SecretKey};
use tower::ServiceExt;

/// Generate a deterministic NodeId for testing
fn test_node_id(seed: u8) -> NodeId {
    let key_bytes = [seed; 32];
    let secret_key = SecretKey::from_bytes(&key_bytes);
    secret_key.public()
}

/// Create a test AppState with a temp directory
fn test_state(temp_dir: &std::path::Path) -> Arc<AppState> {
    let db_path = temp_dir.join("test.db");
    let db = Arc::new(Database::open(&db_path).unwrap());
    let registry_store = DeviceRegistryStore::new(temp_dir).unwrap();
    let server_node_id = test_node_id(255);

    Arc::new(AppState {
        peer: None,
        db,
        password: "test-password".to_string(),
        registry_store,
        node_id: server_node_id,
    })
}

/// Helper to read a response body as bytes
async fn body_bytes(body: Body) -> Vec<u8> {
    body.collect().await.unwrap().to_bytes().to_vec()
}

#[tokio::test]
async fn test_health_endpoint() {
    let temp_dir = tempfile::TempDir::new().unwrap();
    let state = test_state(temp_dir.path());
    let app = api::router(state);

    let req = Request::builder()
        .uri("/health")
        .body(Body::empty())
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();

    assert_eq!(resp.status(), StatusCode::OK);

    let body = body_bytes(resp.into_body()).await;
    assert_eq!(body, b"ok");
}

#[tokio::test]
async fn test_pair_device_success() {
    let temp_dir = tempfile::TempDir::new().unwrap();
    let state = test_state(temp_dir.path());
    let device_node_id = test_node_id(1);
    let app = api::router(state.clone());

    let pair_body = serde_json::json!({
        "node_id": device_node_id.to_string(),
        "device_name": "Test Laptop",
        "password": "test-password"
    });

    let req = Request::builder()
        .method("POST")
        .uri("/pair")
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&pair_body).unwrap()))
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = body_bytes(resp.into_body()).await;
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

    // Should return the server's node_id
    assert!(json["server_node_id"].is_string());
    let server_node_id = test_node_id(255);
    assert_eq!(json["server_node_id"].as_str().unwrap(), server_node_id.to_string());

    // Should return a devices list with the newly paired device
    let devices = json["devices"].as_array().unwrap();
    assert_eq!(devices.len(), 1);
    assert_eq!(devices[0]["name"].as_str().unwrap(), "Test Laptop");
    assert_eq!(
        devices[0]["node_id"].as_str().unwrap(),
        device_node_id.to_string()
    );
}

#[tokio::test]
async fn test_pair_device_wrong_password() {
    let temp_dir = tempfile::TempDir::new().unwrap();
    let state = test_state(temp_dir.path());
    let device_node_id = test_node_id(1);
    let app = api::router(state);

    let pair_body = serde_json::json!({
        "node_id": device_node_id.to_string(),
        "device_name": "Test Laptop",
        "password": "wrong-password"
    });

    let req = Request::builder()
        .method("POST")
        .uri("/pair")
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&pair_body).unwrap()))
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn test_pair_device_invalid_node_id() {
    let temp_dir = tempfile::TempDir::new().unwrap();
    let state = test_state(temp_dir.path());
    let app = api::router(state);

    let pair_body = serde_json::json!({
        "node_id": "not-a-valid-node-id",
        "device_name": "Test Laptop",
        "password": "test-password"
    });

    let req = Request::builder()
        .method("POST")
        .uri("/pair")
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&pair_body).unwrap()))
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn test_list_devices_authorized() {
    let temp_dir = tempfile::TempDir::new().unwrap();
    let state = test_state(temp_dir.path());
    let device_node_id = test_node_id(1);

    // Register a device in the database directly
    state.db.add_device(device_node_id, "Test Laptop").unwrap();

    let app = api::router(state);

    let req = Request::builder()
        .uri("/devices")
        .header("X-Node-Id", device_node_id.to_string())
        .body(Body::empty())
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = body_bytes(resp.into_body()).await;
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let devices = json.as_array().unwrap();
    assert_eq!(devices.len(), 1);
    assert_eq!(devices[0]["name"].as_str().unwrap(), "Test Laptop");
}

#[tokio::test]
async fn test_list_devices_unauthorized() {
    let temp_dir = tempfile::TempDir::new().unwrap();
    let state = test_state(temp_dir.path());
    let unknown_node_id = test_node_id(99);

    let app = api::router(state);

    let req = Request::builder()
        .uri("/devices")
        .header("X-Node-Id", unknown_node_id.to_string())
        .body(Body::empty())
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn test_revoke_device() {
    let temp_dir = tempfile::TempDir::new().unwrap();
    let state = test_state(temp_dir.path());
    let device_node_id = test_node_id(1);

    // Register a device first
    state.db.add_device(device_node_id, "Test Laptop").unwrap();

    // Revoke the device
    let app = api::router(state.clone());

    let revoke_body = serde_json::json!({
        "password": "test-password"
    });

    let req = Request::builder()
        .method("DELETE")
        .uri(format!("/devices/{}", device_node_id))
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&revoke_body).unwrap()))
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    // Verify device is no longer in the database
    assert!(!state.db.contains(&device_node_id).unwrap());
}

#[tokio::test]
async fn test_revoke_device_wrong_password() {
    let temp_dir = tempfile::TempDir::new().unwrap();
    let state = test_state(temp_dir.path());
    let device_node_id = test_node_id(1);

    // Register a device first
    state.db.add_device(device_node_id, "Test Laptop").unwrap();

    let app = api::router(state.clone());

    let revoke_body = serde_json::json!({
        "password": "wrong-password"
    });

    let req = Request::builder()
        .method("DELETE")
        .uri(format!("/devices/{}", device_node_id))
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&revoke_body).unwrap()))
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

    // Device should still exist
    assert!(state.db.contains(&device_node_id).unwrap());
}

/// Helper: build a pair request with the given password
fn pair_request(device_node_id: &NodeId, password: &str) -> Request<Body> {
    let pair_body = serde_json::json!({
        "node_id": device_node_id.to_string(),
        "device_name": "Test Laptop",
        "password": password,
    });
    Request::builder()
        .method("POST")
        .uri("/pair")
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&pair_body).unwrap()))
        .unwrap()
}

/// Helper: send a pair request through a cloned router and return the status code
async fn pair_status(app: &Router, device_node_id: &NodeId, password: &str) -> StatusCode {
    let req = pair_request(device_node_id, password);
    app.clone().oneshot(req).await.unwrap().status()
}

#[tokio::test]
async fn test_pair_rate_limit_allows_up_to_max() {
    let temp_dir = tempfile::TempDir::new().unwrap();
    let state = test_state(temp_dir.path());
    let device_node_id = test_node_id(1);
    let app = api::router(state);

    // The first 5 requests should all succeed (or fail with UNAUTHORIZED for
    // wrong password -- either way they should NOT be 429).
    for i in 0..5 {
        let status = pair_status(&app, &device_node_id, "wrong-password").await;
        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "Request {i} should have been UNAUTHORIZED, not rate-limited"
        );
    }

    // The 6th request should be rate-limited
    let status = pair_status(&app, &device_node_id, "wrong-password").await;
    assert_eq!(
        status,
        StatusCode::TOO_MANY_REQUESTS,
        "Request 6 should have been rate-limited"
    );
}

#[tokio::test]
async fn test_pair_rate_limit_returns_429_body() {
    let temp_dir = tempfile::TempDir::new().unwrap();
    let state = test_state(temp_dir.path());
    let device_node_id = test_node_id(1);
    let app = api::router(state);

    // Exhaust the rate limit
    for _ in 0..5 {
        pair_status(&app, &device_node_id, "wrong-password").await;
    }

    // The next request should return 429 with a descriptive body
    let req = pair_request(&device_node_id, "wrong-password");
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);

    let body = body_bytes(resp.into_body()).await;
    let body_str = String::from_utf8(body).unwrap();
    assert!(
        body_str.contains("Too many pairing attempts"),
        "Expected rate limit message in body, got: {body_str}"
    );
}
