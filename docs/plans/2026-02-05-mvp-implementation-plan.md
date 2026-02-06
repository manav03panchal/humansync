# HumanSync MVP Implementation Plan

**Created:** 2026-02-05
**Based on:** PRD v0.2
**Duration:** 8 weeks
**Last Updated:** 2026-02-06

---

## Overview

This plan breaks down the HumanSync MVP into actionable tasks across four 2-week milestones. Each section includes specific deliverables, test criteria, and risk checkpoints.

**Architecture Summary:**
- Iroh-only transport (no WireGuard/Headscale)
- Password-based device pairing
- In-process Rust library (no daemon)
- Automerge for documents + iroh-blobs for large files
- Always-on cloud peer for async availability

---

## Week 1-2: Proof of Concept

**Goal:** Validate core assumptions. Two devices syncing an Automerge doc over Iroh.

### Week 1: Iroh + Automerge Integration

#### Day 1-2: Project Setup
- [x] Initialize Rust workspace
  ```
  humansync/
  ├── Cargo.toml (workspace)
  ├── crates/
  │   ├── humansync/        # client library
  │   └── humansync-server/ # server binary
  └── docs/
  ```
- [x] Add dependencies: `iroh`, `iroh-blobs`, `automerge`
- [x] Pin Iroh to latest RC (check for 1.0 status)
- [ ] Basic CI setup (cargo check, cargo test, cargo clippy)

#### Day 3-4: Iroh Endpoint Basics
- [x] Create Iroh endpoint with persistent identity
  - Generate keypair on first run
  - Store to `~/.humansync/device_key`
  - Load on subsequent runs
- [ ] Enable mDNS discovery (`discovery_local_network()`)
- [x] Enable n0 relay discovery (`discovery_n0()`)
- [x] Test: two processes on same machine discover each other
- [x] Test: two machines on same LAN discover each other

#### Day 5: Automerge Document Basics
- [x] Create/open Automerge document
- [x] Basic put/get operations
- [x] Persist document to disk (single file)
- [x] Load document from disk on restart
- [x] Test: create doc, write data, restart, data persists

### Week 2: P2P Sync + Blob Validation

#### Day 6-7: Automerge Sync Over Iroh
- [x] Implement sync protocol over Iroh QUIC stream
  - Define ALPN: `b"humansync/1"`
  - Open bidirectional stream for sync
  - Exchange Automerge sync messages until converged
- [x] Test: Device A edits, Device B receives within 5 seconds
- [x] Test: Both devices edit offline, reconnect, merge correctly
- [ ] Test: Sync works via mDNS (same LAN)
- [x] Test: Sync works via relay (different networks)

#### Day 8-9: iroh-blobs Validation
- [x] Store a file as blob, get hash back
- [x] Retrieve blob by hash
- [x] Transfer blob between two devices
- [ ] **Stress test:** Transfer 100MB+ file
  - Measure: transfer speed, memory usage
  - Validate: resume after disconnect works
  - Document: any issues found

#### Day 10: PoC Integration
- [x] Combine: Automerge doc references blob hash
- [x] End-to-end: Device A creates doc with blob attachment, Device B receives both
- [x] Document learnings, API pain points, performance notes

### Week 1-2 Success Criteria
- [x] Two devices sync an Automerge doc within 5 seconds
- [ ] mDNS discovery works on LAN
- [x] Relay fallback works across networks
- [ ] Blob transfer works for files >100MB
- [x] No showstopper issues with Iroh/Automerge APIs

### Week 1-2 Risk Checkpoints
| Risk | Validation | Fallback |
|------|------------|----------|
| Iroh API instability | Does current RC work for our use case? | Pin to working version, track issues |
| iroh-blobs large file issues | 100MB+ stress test | Report upstream, implement chunked workaround |
| mDNS unreliable | Test on multiple networks | Rely more heavily on server-mediated discovery |

---

## Week 3-4: Client Library (`humansync` crate)

**Goal:** Production-quality client library with clean API for apps.

### Week 3: Core Library Structure

#### Day 11-12: Library Architecture
- [x] Define public API surface
  ```rust
  // Target API
  let node = humansync::init(config)?;
  node.pair("password").await?;
  let doc = node.open_doc("notes/shopping")?;
  doc.put("eggs", 12)?;
  let blob = node.store_blob(path).await?;
  ```
- [x] Internal module structure
  ```
  humansync/
  ├── lib.rs           # Public API
  ├── config.rs        # Configuration
  ├── identity.rs      # Device identity (keypair persistence)
  ├── node.rs          # Main HumanSyncNode struct
  ├── doc.rs           # Document handle
  ├── store.rs         # Local persistence (docs + blobs)
  ├── sync/
  │   ├── mod.rs
  │   ├── automerge.rs # Automerge sync protocol
  │   └── blobs.rs     # Blob sync
  ├── discovery.rs     # Peer discovery
  ├── registry.rs      # Device registry
  └── error.rs         # Error types
  ```

#### Day 13-14: Document Store
- [x] Implement local document store
  - Path: `~/.humansync/docs/{namespace}/{doc_id}`
  - One file per Automerge document
  - Atomic writes (write to temp, rename)
- [x] Document index (`_index` Automerge doc)
  - Lists all doc IDs + last modified timestamps
  - Synced first to enable selective sync
- [x] `open_doc()` — create or load
- [x] `list_docs(namespace)` — enumerate documents
- [x] `delete_doc()` — remove document

#### Day 15: Device Registry
- [x] `_devices` Automerge document structure
- [x] Load registry on startup
- [x] Check incoming connections against registry
- [x] Reject unknown NodeIds before any data exchange

### Week 4: Sync Loop + Integration

#### Day 16-17: Background Sync Loop
- [x] Spawn background tokio task on `init()`
- [x] Sync loop logic:
  1. ~~mDNS discovers local peers (Iroh automatic)~~ (not yet)
  2. Fetch peer list from server API
  3. For each known peer:
     - Check if in device registry
     - Connect via Iroh
     - Sync `_devices` doc first (registry updates)
     - Sync `_index` doc (document list)
     - Sync changed documents
     - Sync referenced blobs
  4. Sleep for `sync_interval` (default 30s)
  5. Repeat
- [x] Graceful shutdown on `node.shutdown()`
- [x] Expose `sync_status()` for UI indicators

#### Day 18-19: Blob Integration
- [x] `store_blob(path)` — add file to iroh-blobs, return hash
- [x] `get_blob(hash)` — fetch blob (local or from peers), return path
- [x] Blob reference in Automerge docs (store hash as string field)
- [x] Lazy blob fetch — only download when `get_blob()` called
- [x] Blob cache management (LRU eviction for storage limits)

#### Day 20: Testing + Polish
- [x] Unit tests for all modules
- [x] Integration test: full sync cycle between two nodes
- [x] Integration test: offline edit + reconnect + merge
- [x] Integration test: blob attachment flow
- [ ] Documentation: rustdoc for public API
- [x] Example: minimal app using humansync

### Week 3-4 Success Criteria
- [x] App can `cargo add humansync` and use clean API
- [x] `open_doc/put/get` works with local persistence
- [x] Background sync runs automatically
- [x] Device registry gates connections correctly
- [x] Blobs transfer on-demand
- [ ] All public APIs documented

### Week 3-4 Deliverables
- [x] `humansync` crate (not published, workspace member)
- [x] Integration test suite
- [ ] API documentation
- [x] Example application

---

## Week 5-6: Server (`humansync-server`)

**Goal:** Dockerized server with cloud peer + pairing API.

### Week 5: Cloud Peer + Persistence

#### Day 21-22: Server Binary Setup
- [x] Create `humansync-server` crate
- [x] CLI argument parsing (clap)
  ```
  humansync-server --password <PASSWORD> --data-dir /data --port 4433 --api-port 8080
  ```
- [x] Configuration via env vars + CLI args
- [x] Logging setup (tracing)

#### Day 23-24: Cloud Peer
- [x] Initialize Iroh endpoint (same as client, but always-on)
- [x] Persistent identity for server
- [x] Accept incoming connections
- [x] Gate connections against device registry
- [x] Participate in Automerge sync (same protocol as client)
- [x] Participate in blob sync
- [x] Persist all docs and blobs to disk

#### Day 25: Device Registry (Server-side)
- [x] SQLite database for device registry
  ```sql
  CREATE TABLE devices (
    node_id TEXT PRIMARY KEY,
    device_name TEXT,
    paired_at TEXT,
    last_seen TEXT
  );
  ```
- [x] On pairing: insert into SQLite + update `_devices` Automerge doc
- [x] On revocation: delete from SQLite + update `_devices` doc
- [x] Load registry from SQLite on startup

### Week 6: Pairing API + Docker

#### Day 26-27: HTTP Pairing API
- [x] HTTP server (axum)
- [x] Endpoints:
  ```
  POST /pair
  GET /devices
  DELETE /devices/{node_id}
  ```
- [x] Password verification (constant-time comparison)
- [ ] Rate limiting on /pair endpoint (prevent brute force)
- [x] Request logging

#### Day 28-29: Docker Packaging
- [x] Dockerfile (multi-stage: rust builder + debian slim)
- [x] docker-compose.yml for easy local testing
- [x] Health check endpoint (`GET /health`)
- [x] Graceful shutdown handling

#### Day 30: Server Testing
- [x] Test: pair a device, verify registry updated
- [x] Test: paired device can sync with server
- [x] Test: unpaired device rejected
- [x] Test: revoke device, verify rejected on next sync
- [x] Test: server restart, data persists
- [x] Test: multiple devices sync through server

### Week 5-6 Success Criteria
- [x] `docker run humancorp/humansync-server --password X` works
- [x] Device can pair via POST /pair
- [x] Paired devices sync with cloud peer
- [x] Data persists across server restarts
- [x] Revocation works via DELETE /devices/{id}

### Week 5-6 Deliverables
- [x] `humansync-server` binary
- [x] Docker image
- [x] docker-compose.yml
- [ ] Server API documentation

---

## Week 7-8: Humandocs Migration

**Goal:** First app running on humansync. Passes full acceptance test.

### Week 7: Humandocs Integration

#### Day 31-32: App Architecture
- [x] Define Humandocs data model on humansync
  ```
  humandocs/
  ├── doc-{uuid}     # One Automerge doc per document
  │   ├── title: string
  │   ├── content: string (Automerge Text CRDT)
  │   ├── created_at: timestamp
  │   ├── updated_at: timestamp
  │   └── attachments: [{ name, blob_hash }]
  ```
- [x] Design UI/UX for:
  - Initial setup (server URL + password)
  - Document list
  - Document editor
  - Attachment handling
  - Sync status indicator

#### Day 33-34: Core Integration
- [x] Initialize humansync on app startup
- [x] Pairing flow UI
  - First launch: prompt for server URL + password
  - Call `node.pair()`
  - Store config locally
- [x] Document list
  - `node.list_docs("humandocs/")`
  - Display in UI
- [x] Create document
  - Generate UUID
  - `node.open_doc("humandocs/doc-{uuid}")`
  - Initialize with empty content

#### Day 35: Document Editor
- [x] Load document content on open
- [x] Save changes to Automerge doc
  - Debounce writes (300ms debounce)
- [x] Handle concurrent edits (Automerge auto-merges)
- [x] No conflict UI needed — CRDT handles it

### Week 8: Attachments + Acceptance Testing

#### Day 36-37: Attachment Support
- [x] Add attachment button in editor
- [x] File picker -> `node.store_blob(path)`
- [x] Store blob hash in document's attachments array
- [x] Display attachments in document view
- [x] Click attachment -> `node.get_blob(hash)` -> open file

#### Day 38-39: Acceptance Test Execution
- [ ] Run full 22-step acceptance test (PRD Section 13)
- [ ] Document any failures
- [ ] Fix issues
- [ ] Re-run until all 22 steps pass

#### Day 40: Polish + Documentation
- [ ] Error handling polish (user-friendly messages)
- [ ] Loading states during sync
- [ ] App documentation (README, setup guide)
- [ ] Record demo video of acceptance test

### Week 7-8 Success Criteria
- [x] Humandocs works fully offline
- [x] Syncs across 2+ devices
- [x] File attachments work via blobs
- [ ] All 22 acceptance test steps pass

### Week 7-8 Deliverables
- [x] Humandocs app (Tauri — macOS desktop)
- [ ] User documentation
- [ ] Demo video
- [ ] Release build

---

## Acceptance Test Checklist

From PRD v0.2 Section 13:

```
[x]  1. Deploy humansync-server (docker run --password "test")
[x]  2. Install Humandocs on laptop
[x]  3. Pair laptop (enter server URL + password)
[x]  4. Create a document with text content
[x]  5. Attach a photo to the document
[x]  6. Install Humandocs on phone
[x]  7. Pair phone (enter server URL + password)
[x]  8. Document appears on phone with text
[x]  9. Photo attachment loads on phone
[x] 10. Turn off wifi on laptop
[x] 11. Edit document on laptop
[x] 12. Edit same document on phone
[x] 13. Turn wifi back on
[x] 14. Both edits merged, no conflict, no data loss
[ ] 15. Kill humansync-server
[ ] 16. Edit on laptop
[ ] 17. Edit on phone (both on same LAN)
[ ] 18. Edits sync between laptop and phone directly              ← needs mDNS
[ ] 19. Restart humansync-server
[ ] 20. Server catches up with both devices' changes
[x] 21. Revoke phone via DELETE /devices/{node_id}
[x] 22. Phone can no longer sync with laptop or server
```

**Status: 16/22 verified, 6 remaining (mostly P2P/mDNS and formal end-to-end run)**

---

## Bugs Fixed (Session 2026-02-06)

- [x] **Index file race condition**: Concurrent doc syncs corrupted `_index.automerge` — added `TokioMutex` to serialize index writes
- [x] **Index self-healing**: Corrupted index files now auto-recreate instead of failing forever
- [x] **Cursor activity tracking**: Added `activity` field to `CursorInfo` (typing/viewing)
- [x] **Google Docs-style cursors**: Replaced cursor bar with floating caret overlays at actual text position

---

## Remaining Gaps

| Item | Priority | Notes |
|------|----------|-------|
| **mDNS discovery** | High | Required for P2P without server (acceptance test steps 15-20). Need `discovery_local_network()` in Iroh endpoint builder. |
| **Rate limiting on /pair** | Medium | Prevent brute force on pairing endpoint |
| **100MB+ blob stress test** | Medium | Untested at scale |
| **CI setup** | Medium | No GitHub Actions / CI pipeline yet |
| **API documentation** | Low | Rustdoc for public humansync API |
| **Self-hosted relay** | Low | Optional, n0 relays work for now |
| **Formal 22-step test run** | High | Need to run complete acceptance test end-to-end |
| **Release build** | Medium | No optimized/signed build yet |

---

## Risk Register

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| Iroh 1.0 delayed or breaking changes | Medium | High | Pin to RC, abstract Iroh behind internal interface |
| iroh-blobs performance issues | Medium | Medium | Test early (Week 1-2), have chunking fallback |
| Automerge doc size limits | Low | Medium | Monitor doc sizes, implement pagination if needed |
| Mobile background sync limitations | High | Low | Accept for MVP — cloud peer handles async |
| Password brute-force on pairing | Medium | Medium | Rate limiting, lockout after N failures |
| Device registry propagation lag | Low | Low | Acceptable for MVP (30s window) |

---

## Dependencies

### External
- Iroh (latest RC, targeting 1.0)
- iroh-blobs
- Automerge
- Tokio
- SQLite (rusqlite)
- Axum (HTTP server)
- Clap (CLI parsing)
- Tracing (logging)

### Internal
- humansync crate must be stable before Humandocs migration
- Server must be running before testing full sync scenarios

---

## Decisions Made

1. **Humandocs platform:** Cross-platform (macOS, Windows, Linux, iOS, Android)
2. **UI framework:** Tauri (Rust backend + React frontend)
3. **Iroh version:** Latest (track releases)
4. **Blob storage limits:** TBD during implementation
5. **Frontend stack:** React + TypeScript

---

*This plan will be updated as implementation progresses. Check off items as completed.*
