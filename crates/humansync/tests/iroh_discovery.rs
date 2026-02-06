//! Integration tests for Iroh peer discovery
//!
//! These tests verify the Iroh 0.92.0 discovery functionality:
//! 1. Endpoint creation and direct address discovery
//! 2. Connection establishment between two endpoints
//! 3. Bidirectional communication over QUIC
//!
//! Run these tests with:
//! `cargo test -p humansync --test iroh_discovery`

use std::time::Duration;

use iroh::{Endpoint, NodeAddr, RelayMode, SecretKey, Watcher};
use tokio::time::timeout;

/// ALPN protocol identifier for tests
const TEST_ALPN: &[u8] = b"humansync-test/1";

/// Create a test endpoint with relay disabled (for fast local testing)
async fn create_local_endpoint() -> Endpoint {
    let secret_key = SecretKey::generate(rand::thread_rng());
    Endpoint::builder()
        .secret_key(secret_key)
        .alpns(vec![TEST_ALPN.to_vec()])
        .relay_mode(RelayMode::Disabled)
        .bind()
        .await
        .expect("failed to bind endpoint")
}

/// Test that we can create an endpoint and get its direct addresses
#[tokio::test]
async fn test_endpoint_direct_addresses() {
    let endpoint = create_local_endpoint().await;

    // Wait for direct addresses to be discovered
    let direct_addrs = timeout(Duration::from_secs(5), endpoint.direct_addresses().initialized())
        .await
        .expect("timeout waiting for direct addresses");

    // Should have at least one direct address (localhost binding)
    assert!(!direct_addrs.is_empty(), "should have at least one direct address");

    // The node ID should be valid
    let node_id = endpoint.node_id();
    assert!(!node_id.to_string().is_empty());

    endpoint.close().await;
}

/// Test that we can get a full NodeAddr
#[tokio::test]
async fn test_endpoint_node_addr() {
    let endpoint = create_local_endpoint().await;

    // Wait for node addr to be initialized
    let node_addr = timeout(Duration::from_secs(5), endpoint.node_addr().initialized())
        .await
        .expect("timeout waiting for node addr");

    // NodeAddr should have node_id and at least one address
    assert_eq!(node_addr.node_id, endpoint.node_id());
    assert!(
        !node_addr.direct_addresses.is_empty() || node_addr.relay_url.is_some(),
        "NodeAddr should have direct addresses or relay"
    );

    endpoint.close().await;
}

/// Test that two endpoints can discover each other and connect
#[tokio::test]
async fn test_two_endpoints_connect() {
    // Create two endpoints
    let endpoint1 = create_local_endpoint().await;
    let endpoint2 = create_local_endpoint().await;

    // Wait for both to have addresses
    let node_addr1 = timeout(Duration::from_secs(5), endpoint1.node_addr().initialized())
        .await
        .expect("timeout waiting for node1 addr");
    let node_addr2 = timeout(Duration::from_secs(5), endpoint2.node_addr().initialized())
        .await
        .expect("timeout waiting for node2 addr");

    // Save node IDs for later verification
    let node_id1 = node_addr1.node_id;
    let node_id2 = node_addr2.node_id;

    // Exchange addresses (simulating out-of-band discovery)
    endpoint1
        .add_node_addr(node_addr2.clone())
        .expect("failed to add node2 addr to endpoint1");
    endpoint2
        .add_node_addr(node_addr1.clone())
        .expect("failed to add node1 addr to endpoint2");

    // Endpoint1 connects to endpoint2
    let connect_task = tokio::spawn(async move {
        endpoint1
            .connect(node_addr2, TEST_ALPN)
            .await
            .expect("failed to connect")
    });

    // Endpoint2 accepts the connection
    let accept_task = tokio::spawn(async move {
        let incoming = endpoint2
            .accept()
            .await
            .expect("no incoming connection");
        incoming
            .accept()
            .expect("failed to start accepting")
            .await
            .expect("connection failed")
    });

    // Wait for both with timeout
    let (conn1, conn2) = timeout(
        Duration::from_secs(10),
        async { tokio::join!(connect_task, accept_task) },
    )
    .await
    .expect("connection timeout");

    let conn1 = conn1.expect("connect task panicked");
    let conn2 = conn2.expect("accept task panicked");

    // Verify connections are valid
    assert_eq!(conn1.remote_node_id().expect("no remote node id"), node_id2);
    assert_eq!(conn2.remote_node_id().expect("no remote node id"), node_id1);

    // Clean up
    conn1.close(0u32.into(), b"done");
    conn2.close(0u32.into(), b"done");
}

/// Test bidirectional data transfer over QUIC streams
#[tokio::test]
async fn test_bidirectional_stream_communication() {
    // Create two endpoints
    let endpoint1 = create_local_endpoint().await;
    let endpoint2 = create_local_endpoint().await;

    // Get and exchange addresses
    let node_addr1 = timeout(Duration::from_secs(5), endpoint1.node_addr().initialized())
        .await
        .expect("timeout waiting for node1 addr");
    let node_addr2 = timeout(Duration::from_secs(5), endpoint2.node_addr().initialized())
        .await
        .expect("timeout waiting for node2 addr");

    endpoint1.add_node_addr(node_addr2.clone()).unwrap();
    endpoint2.add_node_addr(node_addr1.clone()).unwrap();

    // Spawn the server task
    let server_task = tokio::spawn(async move {
        let incoming = endpoint2.accept().await.expect("no incoming");
        let conn = incoming.accept().expect("accept failed").await.expect("conn failed");

        // Accept a bidirectional stream
        let (mut send, mut recv) = conn.accept_bi().await.expect("accept_bi failed");

        // Read the message
        let mut buf = vec![0u8; 1024];
        let n = recv.read(&mut buf).await.expect("read failed").expect("no data");
        let msg = String::from_utf8_lossy(&buf[..n]).to_string();

        // Echo it back with modification
        let response = format!("Echo: {}", msg);
        send.write_all(response.as_bytes()).await.expect("write failed");
        send.finish().expect("finish failed");

        (conn, msg)
    });

    // Connect and send data
    let conn = endpoint1
        .connect(node_addr2, TEST_ALPN)
        .await
        .expect("connect failed");

    // Open a bidirectional stream
    let (mut send, mut recv) = conn.open_bi().await.expect("open_bi failed");

    // Send a message
    let message = "Hello from endpoint1!";
    send.write_all(message.as_bytes()).await.expect("write failed");
    send.finish().expect("finish failed");

    // Read the response
    let mut buf = vec![0u8; 1024];
    let n = recv.read(&mut buf).await.expect("read failed").expect("no data");
    let response = String::from_utf8_lossy(&buf[..n]).to_string();

    // Wait for server task
    let (server_conn, received_msg) = timeout(Duration::from_secs(10), server_task)
        .await
        .expect("timeout")
        .expect("server task panicked");

    // Verify
    assert_eq!(received_msg, message);
    assert_eq!(response, format!("Echo: {}", message));

    // Clean up
    conn.close(0u32.into(), b"done");
    server_conn.close(0u32.into(), b"done");
}

/// Test that the same secret key produces the same NodeId (identity persistence)
#[tokio::test]
async fn test_identity_persistence_with_secret_key() {
    // Generate a key
    let secret_key = SecretKey::generate(rand::thread_rng());
    let expected_node_id = secret_key.public();

    // Create first endpoint
    let endpoint1 = Endpoint::builder()
        .secret_key(secret_key.clone())
        .alpns(vec![TEST_ALPN.to_vec()])
        .relay_mode(RelayMode::Disabled)
        .bind()
        .await
        .expect("failed to bind endpoint1");

    let node_id1 = endpoint1.node_id();
    endpoint1.close().await;

    // Create second endpoint with same key
    let endpoint2 = Endpoint::builder()
        .secret_key(secret_key)
        .alpns(vec![TEST_ALPN.to_vec()])
        .relay_mode(RelayMode::Disabled)
        .bind()
        .await
        .expect("failed to bind endpoint2");

    let node_id2 = endpoint2.node_id();
    endpoint2.close().await;

    // Both should have the same NodeId
    assert_eq!(node_id1, expected_node_id);
    assert_eq!(node_id2, expected_node_id);
    assert_eq!(node_id1, node_id2);
}

/// Test connection with explicit direct address (no discovery needed)
#[tokio::test]
async fn test_connect_with_explicit_address() {
    let endpoint1 = create_local_endpoint().await;
    let endpoint2 = create_local_endpoint().await;

    // Get the direct addresses
    let addrs2 = timeout(Duration::from_secs(5), endpoint2.direct_addresses().initialized())
        .await
        .expect("timeout");

    // Create a NodeAddr manually with the direct addresses
    let node_addr2 = NodeAddr::from_parts(
        endpoint2.node_id(),
        None,  // No relay
        addrs2.iter().map(|a| a.addr),
    );

    // Connect using the explicit address
    let connect_task = tokio::spawn({
        let endpoint1 = endpoint1.clone();
        async move {
            endpoint1
                .connect(node_addr2, TEST_ALPN)
                .await
        }
    });

    let accept_task = tokio::spawn({
        let endpoint2 = endpoint2.clone();
        async move {
            let incoming = endpoint2.accept().await.expect("no incoming");
            incoming.accept().expect("accept failed").await
        }
    });

    let (conn_result, accept_result) = timeout(
        Duration::from_secs(10),
        async { tokio::join!(connect_task, accept_task) },
    )
    .await
    .expect("timeout");

    let conn1 = conn_result.expect("connect panicked").expect("connect failed");
    let conn2 = accept_result.expect("accept panicked").expect("accept failed");

    assert_eq!(conn1.remote_node_id().unwrap(), endpoint2.node_id());
    assert_eq!(conn2.remote_node_id().unwrap(), endpoint1.node_id());

    conn1.close(0u32.into(), b"done");
    conn2.close(0u32.into(), b"done");
    endpoint1.close().await;
    endpoint2.close().await;
}
