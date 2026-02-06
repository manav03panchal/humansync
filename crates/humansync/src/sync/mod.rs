//! Sync protocol implementation
//!
//! This module handles:
//! - Automerge document synchronization over Iroh
//! - Blob synchronization via iroh-blobs
//! - Peer discovery and connection management

pub mod automerge;
pub mod blobs;

// Re-exports
pub use self::automerge::{
    handle_doc_list_request, handle_sync_request, request_doc_list, run_sync_protocol, sync_docs,
    DOC_LIST_REQUEST,
};
pub use self::blobs::{
    extract_blob_refs, fetch_blob, fetch_blob_from_peer, handle_blob_request, sync_blobs,
    sync_blobs_with_peer, validate_blob, BlobStore, SharedBlobStore, BLOB_REQUEST_MSG,
    BLOB_RESPONSE_ERROR, BLOB_RESPONSE_NOT_FOUND, BLOB_RESPONSE_SUCCESS,
};
