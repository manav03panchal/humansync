//! PoC Integration Test - Day 10 End-to-End Demonstration
//!
//! This test validates the core PoC requirements from the HumanSync MVP plan:
//! 1. Node A creates an Automerge document with text content
//! 2. Node A stores a file as a blob (using iroh-blobs)
//! 3. Node A references the blob hash in the document
//! 4. Node B syncs the document from Node A
//! 5. Node B retrieves the blob using the hash from the document
//! 6. Verify Node B has the same document content AND the blob data matches
//!
//! Run with: `cargo test --test poc_integration`
//! For more reliable results: `cargo test --test poc_integration -- --test-threads=1`

use std::sync::Arc;
use std::time::Duration;

use automerge::AutoCommit;
use iroh::{Endpoint, RelayMode, SecretKey, Watcher};
use tempfile::TempDir;
use tokio::time::timeout;

use humansync::doc::Document;
use humansync::sync::automerge::{handle_sync_request, sync_docs};
use humansync::sync::blobs::BlobStore;
use humansync::ALPN;

/// Create a test document with the given name in the specified directory
fn create_test_doc(name: &str, dir: &TempDir) -> Document {
    let path = dir.path().join(format!("{name}.automerge"));
    let doc = AutoCommit::new();
    Document::new(name.to_string(), path, doc)
}

/// Create a test Iroh endpoint with relay disabled for local testing
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

/// Create a blob store in a temporary directory
async fn create_test_blob_store(dir: &TempDir) -> BlobStore {
    let blobs_path = dir.path().join("blobs");
    BlobStore::new(&blobs_path, None)
        .await
        .expect("failed to create blob store")
}

/// Helper to set up two connected endpoints
async fn setup_connected_endpoints() -> (Endpoint, Endpoint, iroh::NodeAddr, iroh::NodeAddr) {
    let endpoint_a = create_test_endpoint().await;
    let endpoint_b = create_test_endpoint().await;

    let addr_a = endpoint_a.node_addr().initialized().await;
    let addr_b = endpoint_b.node_addr().initialized().await;

    endpoint_a
        .add_node_addr(addr_b.clone())
        .expect("failed to add node B address");
    endpoint_b
        .add_node_addr(addr_a.clone())
        .expect("failed to add node A address");

    (endpoint_a, endpoint_b, addr_a, addr_b)
}

/// PoC End-to-End Test: Document sync with blob attachment
///
/// This test demonstrates the complete flow from the MVP requirements:
/// - Node A creates a document with content and a blob attachment
/// - Node B syncs and receives both the document and can verify the blob hash
///
/// Note: Blob transfer between nodes is not yet implemented (see sync/blobs.rs TODO).
/// This test verifies that:
/// 1. Documents sync correctly with blob hash references
/// 2. Blob storage and retrieval works locally on each node
/// 3. The blob hash stored in the document matches the actual blob
#[tokio::test]
async fn test_poc_document_with_blob_attachment() {
    // === Setup: Create two nodes with separate storage ===
    let node_a_dir = TempDir::new().expect("failed to create temp dir for node A");
    let node_b_dir = TempDir::new().expect("failed to create temp dir for node B");

    // Create connected Iroh endpoints
    let (endpoint_a, endpoint_b, _addr_a, addr_b) = setup_connected_endpoints().await;

    // Create documents for each node
    let doc_a = create_test_doc("humandocs/doc-001", &node_a_dir);
    let doc_b = Arc::new(create_test_doc("humandocs/doc-001", &node_b_dir));

    // Create blob store for Node A (Node B will create one after sync to simulate fetch)
    let blob_store_a = create_test_blob_store(&node_a_dir).await;

    // === Step 1: Node A creates document with text content ===
    doc_a.put("title", "My First Document").unwrap();
    doc_a.put("content", "This is the document content with some text.").unwrap();
    doc_a.put("created_at", "2026-02-05T10:00:00Z").unwrap();

    // === Step 2: Node A stores a file as a blob ===
    let test_file_content = b"This is the content of an attached file.\nIt could be an image, PDF, or any binary data.";
    let blob_hash = blob_store_a
        .store_bytes(test_file_content.to_vec())
        .await
        .expect("failed to store blob");

    // Verify blob is stored correctly on Node A
    assert!(blob_store_a.has_blob(&blob_hash).await.unwrap());
    let blob_size = blob_store_a.blob_size(&blob_hash).await.unwrap();
    assert_eq!(blob_size, test_file_content.len() as u64);

    // === Step 3: Node A references the blob hash in the document ===
    doc_a.put("attachment_hash", &blob_hash).unwrap();
    doc_a.put("attachment_name", "test_attachment.txt").unwrap();

    // === Step 4: Node B syncs the document from Node A ===
    let doc_b_for_handler = Arc::clone(&doc_b);
    let endpoint_b_clone = endpoint_b.clone();

    // Spawn handler on Node B to accept incoming sync
    let sync_handler = tokio::spawn(async move {
        let conn = endpoint_b_clone
            .accept()
            .await
            .expect("no incoming connection")
            .accept()
            .expect("failed to accept connection")
            .await
            .expect("connection failed");

        handle_sync_request(&conn, &doc_b_for_handler)
            .await
            .expect("sync handler failed");
    });

    // Node A initiates sync to Node B
    let conn = endpoint_a
        .connect(addr_b, ALPN)
        .await
        .expect("failed to connect to node B");

    sync_docs(&conn, &doc_a)
        .await
        .expect("document sync failed");

    // Wait for sync handler to complete
    timeout(Duration::from_secs(10), sync_handler)
        .await
        .expect("sync handler timed out")
        .expect("sync handler failed");

    // === Step 5: Verify Node B has the same document content ===
    let title_a: Option<String> = doc_a.get("title").unwrap();
    let title_b: Option<String> = doc_b.get("title").unwrap();
    assert_eq!(title_a, Some("My First Document".to_string()));
    assert_eq!(title_b, Some("My First Document".to_string()));

    let content_a: Option<String> = doc_a.get("content").unwrap();
    let content_b: Option<String> = doc_b.get("content").unwrap();
    assert_eq!(content_a, content_b);

    let created_a: Option<String> = doc_a.get("created_at").unwrap();
    let created_b: Option<String> = doc_b.get("created_at").unwrap();
    assert_eq!(created_a, created_b);

    // === Step 6: Verify Node B received the blob hash reference ===
    let hash_a: Option<String> = doc_a.get("attachment_hash").unwrap();
    let hash_b: Option<String> = doc_b.get("attachment_hash").unwrap();
    assert_eq!(hash_a, hash_b);
    assert_eq!(hash_b, Some(blob_hash.clone()));

    let name_a: Option<String> = doc_a.get("attachment_name").unwrap();
    let name_b: Option<String> = doc_b.get("attachment_name").unwrap();
    assert_eq!(name_a, name_b);

    // Verify document heads match (fully in sync)
    assert_eq!(doc_a.get_heads(), doc_b.get_heads());

    // === Verify: Blob data integrity ===
    // On Node A, retrieve and verify the blob
    let retrieved_bytes = blob_store_a
        .get_bytes(&blob_hash)
        .await
        .expect("failed to retrieve blob from node A");
    assert_eq!(&retrieved_bytes[..], test_file_content);

    // Node B would retrieve the blob using the hash from the synced document
    // Note: Actual peer-to-peer blob transfer is not yet implemented
    // For this PoC, we demonstrate that Node B has the hash and could fetch it
    let hash_from_doc_b: String = doc_b.get("attachment_hash").unwrap().unwrap();
    assert_eq!(hash_from_doc_b, blob_hash);

    // Now create Node B's blob store and demonstrate content-addressed storage
    // (This simulates what would happen after peer-to-peer blob transfer)
    let blob_store_b = create_test_blob_store(&node_b_dir).await;
    let blob_hash_b = blob_store_b
        .store_bytes(test_file_content.to_vec())
        .await
        .expect("failed to store blob on node B");
    assert_eq!(blob_hash_b, blob_hash, "Content-addressed hashes should match");

    // Clean up
    conn.close(0u32.into(), b"done");
    endpoint_a.close().await;
    endpoint_b.close().await;

    println!("PoC Test PASSED: Document with blob attachment synced successfully!");
    println!("  - Document title: My First Document");
    println!("  - Blob hash: {blob_hash}");
    println!("  - Blob size: {blob_size} bytes");
}

/// Test: Multiple documents with multiple blob attachments
///
/// Simulates a more realistic scenario with multiple documents,
/// each having different attachments.
#[tokio::test]
async fn test_poc_multiple_documents_with_blobs() {
    let node_a_dir = TempDir::new().unwrap();
    let node_b_dir = TempDir::new().unwrap();

    // Create connected endpoints
    let (endpoint_a, endpoint_b, _addr_a, addr_b) = setup_connected_endpoints().await;

    // Create two documents on Node A
    let doc1_a = create_test_doc("humandocs/doc-001", &node_a_dir);
    let doc2_a = create_test_doc("humandocs/doc-002", &node_a_dir);

    let doc1_b = Arc::new(create_test_doc("humandocs/doc-001", &node_b_dir));
    let doc2_b = Arc::new(create_test_doc("humandocs/doc-002", &node_b_dir));

    let blob_store_a = create_test_blob_store(&node_a_dir).await;

    // Document 1: A note with an image attachment
    doc1_a.put("title", "Meeting Notes").unwrap();
    doc1_a.put("content", "Discussed Q1 roadmap").unwrap();

    let image_data = vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]; // PNG header
    let image_hash = blob_store_a.store_bytes(image_data.clone()).await.unwrap();
    doc1_a.put("image_hash", &image_hash).unwrap();

    // Document 2: A document with PDF attachment
    doc2_a.put("title", "Project Proposal").unwrap();
    doc2_a.put("content", "Budget and timeline").unwrap();

    let pdf_data = b"%PDF-1.4 simulated content";
    let pdf_hash = blob_store_a.store_bytes(pdf_data.to_vec()).await.unwrap();
    doc2_a.put("pdf_hash", &pdf_hash).unwrap();

    // Sync Document 1
    let doc1_b_clone = Arc::clone(&doc1_b);
    let endpoint_b_clone = endpoint_b.clone();

    let handler1 = tokio::spawn(async move {
        let conn = endpoint_b_clone
            .accept()
            .await
            .unwrap()
            .accept()
            .unwrap()
            .await
            .unwrap();
        handle_sync_request(&conn, &doc1_b_clone).await.unwrap();
    });

    let conn1 = endpoint_a.connect(addr_b.clone(), ALPN).await.unwrap();
    sync_docs(&conn1, &doc1_a).await.unwrap();
    timeout(Duration::from_secs(10), handler1).await.unwrap().unwrap();
    conn1.close(0u32.into(), b"done");

    // Sync Document 2
    let doc2_b_clone = Arc::clone(&doc2_b);
    let endpoint_b_clone = endpoint_b.clone();

    let handler2 = tokio::spawn(async move {
        let conn = endpoint_b_clone
            .accept()
            .await
            .unwrap()
            .accept()
            .unwrap()
            .await
            .unwrap();
        handle_sync_request(&conn, &doc2_b_clone).await.unwrap();
    });

    let conn2 = endpoint_a.connect(addr_b.clone(), ALPN).await.unwrap();
    sync_docs(&conn2, &doc2_a).await.unwrap();
    timeout(Duration::from_secs(10), handler2).await.unwrap().unwrap();
    conn2.close(0u32.into(), b"done");

    // Verify both documents synced correctly
    assert_eq!(
        doc1_a.get::<String>("title").unwrap(),
        doc1_b.get::<String>("title").unwrap()
    );
    assert_eq!(
        doc1_a.get::<String>("image_hash").unwrap(),
        doc1_b.get::<String>("image_hash").unwrap()
    );

    assert_eq!(
        doc2_a.get::<String>("title").unwrap(),
        doc2_b.get::<String>("title").unwrap()
    );
    assert_eq!(
        doc2_a.get::<String>("pdf_hash").unwrap(),
        doc2_b.get::<String>("pdf_hash").unwrap()
    );

    // Verify blob hashes are different (different content)
    assert_ne!(image_hash, pdf_hash);

    endpoint_a.close().await;
    endpoint_b.close().await;

    println!("PoC Test PASSED: Multiple documents with blob attachments synced!");
}

/// Test: Offline edit and merge with blob references
///
/// Simulates the offline editing scenario:
/// 1. Both nodes have a document
/// 2. Node A adds an attachment while "offline"
/// 3. Node B edits content while "offline"
/// 4. They sync and both edits merge correctly
#[tokio::test]
async fn test_poc_offline_edit_with_blob_merge() {
    let node_a_dir = TempDir::new().unwrap();
    let node_b_dir = TempDir::new().unwrap();

    // Create connected endpoints
    let (endpoint_a, endpoint_b, _addr_a, addr_b) = setup_connected_endpoints().await;

    let doc_a = create_test_doc("shared-doc", &node_a_dir);
    let doc_b = Arc::new(create_test_doc("shared-doc", &node_b_dir));

    let blob_store_a = create_test_blob_store(&node_a_dir).await;

    // Both nodes start with the same initial content
    doc_a.put("title", "Shared Document").unwrap();
    doc_b.put("title", "Shared Document").unwrap();

    // === Simulate offline editing ===

    // Node A adds a blob attachment while "offline"
    let attachment_data = b"Important file attached by Node A";
    let attachment_hash = blob_store_a.store_bytes(attachment_data.to_vec()).await.unwrap();
    doc_a.put("attachment", &attachment_hash).unwrap();
    doc_a.put("attached_by", "Node A").unwrap();

    // Node B edits the content while "offline"
    doc_b.put("last_editor", "Node B").unwrap();
    doc_b.put("edit_note", "Added comments section").unwrap();

    // === Reconnect and sync ===
    let doc_b_clone = Arc::clone(&doc_b);
    let endpoint_b_clone = endpoint_b.clone();

    let handler = tokio::spawn(async move {
        let conn = endpoint_b_clone
            .accept()
            .await
            .unwrap()
            .accept()
            .unwrap()
            .await
            .unwrap();
        handle_sync_request(&conn, &doc_b_clone).await.unwrap();
    });

    let conn = endpoint_a.connect(addr_b, ALPN).await.unwrap();
    sync_docs(&conn, &doc_a).await.unwrap();
    timeout(Duration::from_secs(10), handler).await.unwrap().unwrap();
    conn.close(0u32.into(), b"done");

    // === Verify merge: Both nodes have all content ===

    // Both should have the title (same initial content)
    assert_eq!(
        doc_a.get::<String>("title").unwrap(),
        Some("Shared Document".to_string())
    );
    assert_eq!(
        doc_b.get::<String>("title").unwrap(),
        Some("Shared Document".to_string())
    );

    // Both should have Node A's attachment
    assert_eq!(
        doc_a.get::<String>("attachment").unwrap(),
        Some(attachment_hash.clone())
    );
    assert_eq!(
        doc_b.get::<String>("attachment").unwrap(),
        Some(attachment_hash)
    );
    assert_eq!(
        doc_a.get::<String>("attached_by").unwrap(),
        Some("Node A".to_string())
    );
    assert_eq!(
        doc_b.get::<String>("attached_by").unwrap(),
        Some("Node A".to_string())
    );

    // Both should have Node B's edits
    assert_eq!(
        doc_a.get::<String>("last_editor").unwrap(),
        Some("Node B".to_string())
    );
    assert_eq!(
        doc_b.get::<String>("last_editor").unwrap(),
        Some("Node B".to_string())
    );
    assert_eq!(
        doc_a.get::<String>("edit_note").unwrap(),
        Some("Added comments section".to_string())
    );
    assert_eq!(
        doc_b.get::<String>("edit_note").unwrap(),
        Some("Added comments section".to_string())
    );

    // Document heads should match (fully converged)
    assert_eq!(doc_a.get_heads(), doc_b.get_heads());

    endpoint_a.close().await;
    endpoint_b.close().await;

    println!("PoC Test PASSED: Offline edits merged correctly with blob references!");
}

/// Test: Large blob handling
///
/// Tests that the system handles larger blobs correctly and the hash
/// is properly synchronized in the document.
#[tokio::test]
async fn test_poc_large_blob_reference() {
    let node_a_dir = TempDir::new().unwrap();
    let node_b_dir = TempDir::new().unwrap();

    // Create connected endpoints
    let (endpoint_a, endpoint_b, _addr_a, addr_b) = setup_connected_endpoints().await;

    let doc_a = create_test_doc("large-file-doc", &node_a_dir);
    let doc_b = Arc::new(create_test_doc("large-file-doc", &node_b_dir));

    let blob_store_a = create_test_blob_store(&node_a_dir).await;

    // Create a 1MB blob (simulating a photo or document)
    let large_data: Vec<u8> = (0..1024 * 1024).map(|i| (i % 256) as u8).collect();
    let large_hash = blob_store_a.store_bytes(large_data.clone()).await.unwrap();

    // Create document with reference to large blob
    doc_a.put("title", "Photo Album").unwrap();
    doc_a.put("photo_hash", &large_hash).unwrap();
    doc_a.put("photo_size_bytes", 1024 * 1024i64).unwrap();

    // Sync document
    let doc_b_clone = Arc::clone(&doc_b);
    let endpoint_b_clone = endpoint_b.clone();

    let handler = tokio::spawn(async move {
        let conn = endpoint_b_clone
            .accept()
            .await
            .unwrap()
            .accept()
            .unwrap()
            .await
            .unwrap();
        handle_sync_request(&conn, &doc_b_clone).await.unwrap();
    });

    let conn = endpoint_a.connect(addr_b, ALPN).await.unwrap();
    sync_docs(&conn, &doc_a).await.unwrap();
    timeout(Duration::from_secs(10), handler).await.unwrap().unwrap();
    conn.close(0u32.into(), b"done");

    // Verify Node B received the hash
    let hash_b: Option<String> = doc_b.get("photo_hash").unwrap();
    assert_eq!(hash_b, Some(large_hash.clone()));

    let size_b: Option<i64> = doc_b.get("photo_size_bytes").unwrap();
    assert_eq!(size_b, Some(1024 * 1024));

    // Verify blob integrity on Node A
    let size_a = blob_store_a.blob_size(&large_hash).await.unwrap();
    assert_eq!(size_a, 1024 * 1024);

    // Simulate Node B fetching and storing the blob (what would happen after P2P transfer)
    let blob_store_b = create_test_blob_store(&node_b_dir).await;
    let hash_b_stored = blob_store_b.store_bytes(large_data).await.unwrap();
    assert_eq!(hash_b_stored, large_hash);

    endpoint_a.close().await;
    endpoint_b.close().await;

    println!("PoC Test PASSED: Large blob (1MB) reference synced correctly!");
}

/// Test: File-based blob storage
///
/// Tests storing an actual file (from disk) as a blob.
#[tokio::test]
async fn test_poc_file_blob_storage() {
    let node_dir = TempDir::new().unwrap();
    let blob_store = create_test_blob_store(&node_dir).await;

    // Create a test file
    let test_file_path = node_dir.path().join("test_document.txt");
    let file_content = "This is a test document that would be attached to an Automerge doc.\n\
                        It contains multiple lines of text.\n\
                        And demonstrates file-based blob storage.";
    tokio::fs::write(&test_file_path, file_content)
        .await
        .expect("failed to write test file");

    // Store file as blob
    let file_hash = blob_store
        .store_file(&test_file_path)
        .await
        .expect("failed to store file as blob");

    // Verify blob exists and has correct size
    assert!(blob_store.has_blob(&file_hash).await.unwrap());
    let blob_size = blob_store.blob_size(&file_hash).await.unwrap();
    assert_eq!(blob_size, file_content.len() as u64);

    // Retrieve blob and verify content
    let retrieved = blob_store.get_bytes(&file_hash).await.unwrap();
    assert_eq!(&retrieved[..], file_content.as_bytes());

    // Also test export to file
    let export_path = blob_store.get_blob(&file_hash).await.unwrap();
    let exported_content = tokio::fs::read(&export_path).await.unwrap();
    assert_eq!(exported_content, file_content.as_bytes());

    println!("PoC Test PASSED: File-based blob storage works correctly!");
    println!("  - File hash: {file_hash}");
    println!("  - File size: {blob_size} bytes");
}

/// Test: Validate blob content integrity
///
/// Tests the validate_blob function to ensure content integrity verification works.
#[tokio::test]
async fn test_poc_blob_validation() {
    use humansync::sync::blobs::validate_blob;

    let node_dir = TempDir::new().unwrap();
    let blob_store = create_test_blob_store(&node_dir).await;

    let original_data = b"Content that must remain intact";
    let hash = blob_store.store_bytes(original_data.to_vec()).await.unwrap();

    // Retrieve and validate
    let retrieved = blob_store.get_bytes(&hash).await.unwrap();

    // The hash is a 64-character hex string, but validate_blob uses BLAKE3
    // For this test, we verify the content matches what we stored
    assert_eq!(&retrieved[..], original_data);

    // Verify BLAKE3 hash matches
    let computed_hash = blake3::hash(original_data);
    let computed_hex = computed_hash.to_hex().to_string();
    assert_eq!(computed_hex, hash);

    // Test validation function
    assert!(validate_blob(original_data, &hash));
    assert!(!validate_blob(b"tampered content", &hash));

    println!("PoC Test PASSED: Blob content validation works correctly!");
}
