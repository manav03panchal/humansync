//! Integration tests for Automerge sync over Iroh QUIC
//!
//! These tests verify that two HumanSync nodes can:
//! 1. Sync a single document
//! 2. Handle offline edits and merge correctly
//!
//! Note: These tests use RelayMode::Disabled for fast local testing.
//! For tests that require n0 discovery or relay servers, mark them as #[ignore].
//!
//! Run all tests: `cargo test -p humansync --test sync_integration`
//! Run ignored tests: `cargo test -p humansync --test sync_integration -- --ignored`

use std::sync::Arc;
use std::time::Duration;

use automerge::AutoCommit;
use iroh::{Endpoint, RelayMode, SecretKey, Watcher};
use tempfile::TempDir;
use tokio::time::timeout;

use humansync::doc::Document;
use humansync::sync::automerge::{handle_sync_request, sync_docs};
use humansync::ALPN;

/// Create a test document with the given name
fn create_test_doc(name: &str, dir: &TempDir) -> Document {
    let path = dir.path().join(format!("{name}.automerge"));
    let doc = AutoCommit::new();
    Document::new(name.to_string(), path, doc)
}

/// Create a test Iroh endpoint
/// Using relay mode disabled but with explicit binding
async fn create_test_endpoint() -> Endpoint {
    let secret_key = SecretKey::generate(rand::thread_rng());
    Endpoint::builder()
        .secret_key(secret_key)
        .alpns(vec![ALPN.to_vec()])
        .relay_mode(RelayMode::Disabled)
        .bind()
        .await
        .expect("failed to bind endpoint")
}

/// Test that two nodes can sync a single document
#[tokio::test]
async fn test_two_nodes_sync_single_document() {
    // Create two Iroh endpoints
    let endpoint1 = create_test_endpoint().await;
    let endpoint2 = create_test_endpoint().await;

    // Get node addresses - wait for direct addresses to be available
    let ep1_nodeaddr = endpoint1.node_addr().initialized().await;
    let ep2_nodeaddr = endpoint2.node_addr().initialized().await;

    // Add each other's addresses
    endpoint1
        .add_node_addr(ep2_nodeaddr.clone())
        .expect("failed to add node2 addr");
    endpoint2
        .add_node_addr(ep1_nodeaddr.clone())
        .expect("failed to add node1 addr");

    // Create temporary directories for documents
    let dir1 = TempDir::new().unwrap();
    let dir2 = TempDir::new().unwrap();

    // Create documents - node1 has data, node2 is empty
    let doc1 = create_test_doc("test-doc", &dir1);
    let doc2 = Arc::new(create_test_doc("test-doc", &dir2));

    // Add data to doc1
    doc1.put("title", "Hello World").unwrap();
    doc1.put("count", 42i64).unwrap();
    doc1.put("active", true).unwrap();

    // Spawn a task to handle the incoming sync request on node2
    let doc2_for_handler = Arc::clone(&doc2);
    let endpoint2_clone = endpoint2.clone();

    let handler = tokio::spawn(async move {
        // Accept incoming connection
        let conn = endpoint2_clone
            .accept()
            .await
            .expect("no incoming connection")
            .accept()
            .expect("failed to accept connection")
            .await
            .expect("connection failed");

        // Handle the sync request
        handle_sync_request(&conn, &doc2_for_handler)
            .await
            .expect("sync handler failed");
    });

    // Node1 connects to node2 and initiates sync
    let conn = endpoint1
        .connect(ep2_nodeaddr, ALPN)
        .await
        .expect("failed to connect");

    // Sync doc1 with doc2
    sync_docs(&conn, &doc1).await.expect("sync failed");

    // Wait for handler to complete
    timeout(Duration::from_secs(10), handler)
        .await
        .expect("handler timed out")
        .expect("handler failed");

    // Verify both documents have the same data
    let title1: Option<String> = doc1.get("title").unwrap();
    let title2: Option<String> = doc2.get("title").unwrap();
    assert_eq!(title1, Some("Hello World".to_string()));
    assert_eq!(title2, Some("Hello World".to_string()));

    let count1: Option<i64> = doc1.get("count").unwrap();
    let count2: Option<i64> = doc2.get("count").unwrap();
    assert_eq!(count1, Some(42));
    assert_eq!(count2, Some(42));

    let active1: Option<bool> = doc1.get("active").unwrap();
    let active2: Option<bool> = doc2.get("active").unwrap();
    assert_eq!(active1, Some(true));
    assert_eq!(active2, Some(true));

    // Verify heads match (documents are in sync)
    assert_eq!(doc1.get_heads(), doc2.get_heads());

    // Clean up
    conn.close(0u32.into(), b"done");
    endpoint1.close().await;
    endpoint2.close().await;
}

/// Test that two nodes editing offline can sync and merge correctly
#[tokio::test]
async fn test_offline_edits_merge_correctly() {
    // Create two Iroh endpoints
    let endpoint1 = create_test_endpoint().await;
    let endpoint2 = create_test_endpoint().await;

    // Get and exchange node addresses
    let ep1_nodeaddr = endpoint1.node_addr().initialized().await;
    let ep2_nodeaddr = endpoint2.node_addr().initialized().await;

    endpoint1
        .add_node_addr(ep2_nodeaddr.clone())
        .expect("failed to add node2 addr");
    endpoint2
        .add_node_addr(ep1_nodeaddr.clone())
        .expect("failed to add node1 addr");

    // Create temporary directories
    let dir1 = TempDir::new().unwrap();
    let dir2 = TempDir::new().unwrap();

    // Create documents - both start empty
    let doc1 = create_test_doc("shared-doc", &dir1);
    let doc2 = Arc::new(create_test_doc("shared-doc", &dir2));

    // Simulate "offline" editing - each node adds different data
    // Node 1 adds some data
    doc1.put("from_node1", "value1").unwrap();
    doc1.put("node1_count", 100i64).unwrap();

    // Node 2 adds different data
    doc2.put("from_node2", "value2").unwrap();
    doc2.put("node2_count", 200i64).unwrap();

    // Also test conflict: both nodes set the same key
    doc1.put("conflict_key", "node1_version").unwrap();
    doc2.put("conflict_key", "node2_version").unwrap();

    // Now "reconnect" and sync
    let doc2_for_handler = Arc::clone(&doc2);
    let endpoint2_clone = endpoint2.clone();

    let handler = tokio::spawn(async move {
        let conn = endpoint2_clone
            .accept()
            .await
            .expect("no incoming connection")
            .accept()
            .expect("failed to accept connection")
            .await
            .expect("connection failed");

        handle_sync_request(&conn, &doc2_for_handler)
            .await
            .expect("sync handler failed");
    });

    let conn = endpoint1
        .connect(ep2_nodeaddr, ALPN)
        .await
        .expect("failed to connect");

    sync_docs(&conn, &doc1).await.expect("sync failed");

    timeout(Duration::from_secs(10), handler)
        .await
        .expect("handler timed out")
        .expect("handler failed");

    // After sync, both documents should have ALL the data from both nodes
    // (except for the conflict key, which Automerge resolves deterministically)

    // Check node1's data is in both
    let from_node1_1: Option<String> = doc1.get("from_node1").unwrap();
    let from_node1_2: Option<String> = doc2.get("from_node1").unwrap();
    assert_eq!(from_node1_1, Some("value1".to_string()));
    assert_eq!(from_node1_2, Some("value1".to_string()));

    let node1_count_1: Option<i64> = doc1.get("node1_count").unwrap();
    let node1_count_2: Option<i64> = doc2.get("node1_count").unwrap();
    assert_eq!(node1_count_1, Some(100));
    assert_eq!(node1_count_2, Some(100));

    // Check node2's data is in both
    let from_node2_1: Option<String> = doc1.get("from_node2").unwrap();
    let from_node2_2: Option<String> = doc2.get("from_node2").unwrap();
    assert_eq!(from_node2_1, Some("value2".to_string()));
    assert_eq!(from_node2_2, Some("value2".to_string()));

    let node2_count_1: Option<i64> = doc1.get("node2_count").unwrap();
    let node2_count_2: Option<i64> = doc2.get("node2_count").unwrap();
    assert_eq!(node2_count_1, Some(200));
    assert_eq!(node2_count_2, Some(200));

    // Check conflict resolution - both should have the same value
    let conflict1: Option<String> = doc1.get("conflict_key").unwrap();
    let conflict2: Option<String> = doc2.get("conflict_key").unwrap();
    assert_eq!(
        conflict1, conflict2,
        "Conflict resolution should produce the same value on both nodes"
    );

    // Verify heads match
    assert_eq!(doc1.get_heads(), doc2.get_heads());

    // Clean up
    conn.close(0u32.into(), b"done");
    endpoint1.close().await;
    endpoint2.close().await;
}

/// Test bidirectional sync - both nodes have changes and exchange them
#[tokio::test]
async fn test_bidirectional_sync() {
    let endpoint1 = create_test_endpoint().await;
    let endpoint2 = create_test_endpoint().await;

    // Get and exchange node addresses
    let ep1_nodeaddr = endpoint1.node_addr().initialized().await;
    let ep2_nodeaddr = endpoint2.node_addr().initialized().await;

    endpoint1
        .add_node_addr(ep2_nodeaddr.clone())
        .expect("failed to add node2 addr");
    endpoint2
        .add_node_addr(ep1_nodeaddr.clone())
        .expect("failed to add node1 addr");

    let dir1 = TempDir::new().unwrap();
    let dir2 = TempDir::new().unwrap();

    let doc1 = create_test_doc("bidirectional", &dir1);
    let doc2 = Arc::new(create_test_doc("bidirectional", &dir2));

    // Node 1 adds items 1-5
    for i in 1..=5 {
        doc1.put(&format!("item_{i}"), format!("from_node1_{i}"))
            .unwrap();
    }

    // Node 2 adds items 6-10
    for i in 6..=10 {
        doc2.put(&format!("item_{i}"), format!("from_node2_{i}"))
            .unwrap();
    }

    let doc2_for_handler = Arc::clone(&doc2);
    let endpoint2_clone = endpoint2.clone();

    let handler = tokio::spawn(async move {
        let conn = endpoint2_clone
            .accept()
            .await
            .expect("no incoming connection")
            .accept()
            .expect("failed to accept connection")
            .await
            .expect("connection failed");

        handle_sync_request(&conn, &doc2_for_handler)
            .await
            .expect("sync handler failed");
    });

    let conn = endpoint1
        .connect(ep2_nodeaddr, ALPN)
        .await
        .expect("failed to connect");

    sync_docs(&conn, &doc1).await.expect("sync failed");

    timeout(Duration::from_secs(10), handler)
        .await
        .expect("handler timed out")
        .expect("handler failed");

    // Verify both docs have all 10 items
    for i in 1..=10 {
        let key = format!("item_{i}");
        let val1: Option<String> = doc1.get(&key).unwrap();
        let val2: Option<String> = doc2.get(&key).unwrap();

        assert!(val1.is_some(), "doc1 missing {key}");
        assert!(val2.is_some(), "doc2 missing {key}");
        assert_eq!(val1, val2, "values for {key} don't match");
    }

    assert_eq!(doc1.get_heads(), doc2.get_heads());

    conn.close(0u32.into(), b"done");
    endpoint1.close().await;
    endpoint2.close().await;
}

/// Test multiple consecutive sync operations
#[tokio::test]
async fn test_multiple_sync_operations() {
    let endpoint1 = create_test_endpoint().await;
    let endpoint2 = create_test_endpoint().await;

    // Get and exchange node addresses
    let ep1_nodeaddr = endpoint1.node_addr().initialized().await;
    let ep2_nodeaddr = endpoint2.node_addr().initialized().await;

    endpoint1
        .add_node_addr(ep2_nodeaddr.clone())
        .expect("failed to add node2 addr");
    endpoint2
        .add_node_addr(ep1_nodeaddr.clone())
        .expect("failed to add node1 addr");

    let dir1 = TempDir::new().unwrap();
    let dir2 = TempDir::new().unwrap();

    let doc1 = Arc::new(create_test_doc("multi-sync", &dir1));
    let doc2 = Arc::new(create_test_doc("multi-sync", &dir2));

    // Perform multiple sync rounds
    for round in 1..=3 {
        // Add data on node1
        doc1.put(&format!("round_{round}_node1"), format!("data_{round}"))
            .unwrap();

        // Add data on node2
        doc2.put(&format!("round_{round}_node2"), format!("data_{round}"))
            .unwrap();

        // Sync
        let doc2_clone = Arc::clone(&doc2);
        let endpoint2_clone = endpoint2.clone();

        let handler = tokio::spawn(async move {
            let conn = endpoint2_clone
                .accept()
                .await
                .expect("no incoming connection")
                .accept()
                .expect("failed to accept connection")
                .await
                .expect("connection failed");

            handle_sync_request(&conn, &doc2_clone)
                .await
                .expect("sync handler failed");
        });

        let conn = endpoint1
            .connect(ep2_nodeaddr.clone(), ALPN)
            .await
            .expect("failed to connect");

        sync_docs(&conn, &doc1).await.expect("sync failed");

        timeout(Duration::from_secs(10), handler)
            .await
            .expect("handler timed out")
            .expect("handler failed");

        conn.close(0u32.into(), b"done");

        // Verify sync after each round
        assert_eq!(
            doc1.get_heads(),
            doc2.get_heads(),
            "Heads should match after round {round}"
        );
    }

    // Verify all data from all rounds is present
    for round in 1..=3 {
        let key1 = format!("round_{round}_node1");
        let key2 = format!("round_{round}_node2");

        let val1_1: Option<String> = doc1.get(&key1).unwrap();
        let val1_2: Option<String> = doc2.get(&key1).unwrap();
        let val2_1: Option<String> = doc1.get(&key2).unwrap();
        let val2_2: Option<String> = doc2.get(&key2).unwrap();

        assert_eq!(val1_1, val1_2);
        assert_eq!(val2_1, val2_2);
    }

    endpoint1.close().await;
    endpoint2.close().await;
}

/// Test syncing an empty document
#[tokio::test]
async fn test_sync_empty_documents() {
    let endpoint1 = create_test_endpoint().await;
    let endpoint2 = create_test_endpoint().await;

    // Get and exchange node addresses
    let ep1_nodeaddr = endpoint1.node_addr().initialized().await;
    let ep2_nodeaddr = endpoint2.node_addr().initialized().await;

    endpoint1
        .add_node_addr(ep2_nodeaddr.clone())
        .expect("failed to add node2 addr");
    endpoint2
        .add_node_addr(ep1_nodeaddr.clone())
        .expect("failed to add node1 addr");

    let dir1 = TempDir::new().unwrap();
    let dir2 = TempDir::new().unwrap();

    // Both documents are empty
    let doc1 = create_test_doc("empty", &dir1);
    let doc2 = Arc::new(create_test_doc("empty", &dir2));

    let doc2_for_handler = Arc::clone(&doc2);
    let endpoint2_clone = endpoint2.clone();

    let handler = tokio::spawn(async move {
        let conn = endpoint2_clone
            .accept()
            .await
            .expect("no incoming connection")
            .accept()
            .expect("failed to accept connection")
            .await
            .expect("connection failed");

        handle_sync_request(&conn, &doc2_for_handler)
            .await
            .expect("sync handler failed");
    });

    let conn = endpoint1
        .connect(ep2_nodeaddr, ALPN)
        .await
        .expect("failed to connect");

    sync_docs(&conn, &doc1).await.expect("sync failed");

    timeout(Duration::from_secs(10), handler)
        .await
        .expect("handler timed out")
        .expect("handler failed");

    // Both should still be empty and in sync
    assert!(doc1.keys().unwrap().is_empty());
    assert!(doc2.keys().unwrap().is_empty());
    assert_eq!(doc1.get_heads(), doc2.get_heads());

    conn.close(0u32.into(), b"done");
    endpoint1.close().await;
    endpoint2.close().await;
}
