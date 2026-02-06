# HumanSync — MVP Product Requirements Document

**Version:** 0.2  
**Date:** February 5, 2026  
**Author:** HumanCorp  
**Status:** Draft  

---

## 1. Executive Summary

HumanSync is a self-hosted, local-first sync layer that gives users an iCloud-like experience across all their devices — without depending on any third-party cloud. The device is always the source of truth. The server is just another peer that happens to have good uptime.

The MVP targets a single user syncing data across their personal devices. One user, multiple devices, data syncs. That's it.

**v0.2 revision:** Architecture simplified to Iroh-only (no WireGuard/Headscale dependency). Auth reduced to password-based device pairing. Client is an in-process library, not a separate daemon.

---

## 2. Problem Statement

Users who want to own their data face a binary choice today: use a centralized cloud provider (iCloud, Google) and surrender control, or manage fragmented self-hosted tools that don't talk to each other. There is no self-hostable, open-source equivalent of iCloud's seamless cross-device sync that works offline, handles conflicts automatically, and requires near-zero configuration.

HumanCorp already ships multiple products (humanssh, humanboard, humanfileshare). Each currently manages its own data independently. Users cannot sign in once and have their data available across all HumanCorp apps on all their devices.

---

## 3. Vision

A single sign-in gives you access to all your data, on all your devices, across all HumanCorp apps. Offline by default. No loading spinners. No "you're offline" banners. If the server dies, nothing breaks. If two devices are on the same WiFi, they sync directly. The server catches up when it comes back.

---

## 4. MVP Scope

### 4.1 In Scope

- Single-user, multi-device sync
- One HumanCorp app (Humandocs) migrated to HumanSync as proof of concept
- Self-hosted server deployment (single Docker container)
- Offline-first operation on all devices
- Automatic conflict resolution via CRDTs (Automerge)
- Large file sync via iroh-blobs (separated from document sync)
- Peer-to-peer transport via Iroh (QUIC, NAT traversal, relay fallback)
- Local network discovery via mDNS
- Password-protected device pairing
- Persistent device identity (stable Iroh NodeId per device)
- Always-on cloud peer for async availability

### 4.2 Explicitly Out of Scope (Post-MVP)

- Multi-user sharing / collaboration
- MoQ video/voice/real-time media
- Web/browser client
- Admin dashboard
- App-level permission scoping
- Schema migration tooling
- End-to-end encryption beyond Iroh's QUIC TLS
- Mobile push notifications for sync events
- WireGuard / Headscale / Tailscale integration

---

## 5. Architecture

### 5.1 Layered Stack

```
┌──────────────────────────────────────────────────────────┐
│                    HumanCorp Apps                         │
│                 (Humandocs, future apps)                  │
├──────────────────────────────────────────────────────────┤
│              humansync (Rust client library)              │
│  • Automerge docs (CRDT)                                 │
│  • iroh-blobs (large files)                              │
│  • Persistent device identity                            │
│  • Background sync loop                                  │
├──────────────────────────────────────────────────────────┤
│                    Iroh (transport)                       │
│  • QUIC P2P connections                                  │
│  • NAT traversal (n0 relays or self-hosted)              │
│  • mDNS local discovery                                  │
├──────────────────────────────────────────────────────────┤
│              humansync-server (Docker)                    │
│  • Cloud peer (always-on Iroh node)                      │
│  • Device registry (list of node IDs)                    │
│  • Pairing API (password-protected)                      │
│  • Persistent storage (docs + blobs)                     │
└──────────────────────────────────────────────────────────┘
```

### 5.2 Component Responsibilities

| Layer | Responsibility | Owns |
|---|---|---|
| **Iroh** | P2P transport, NAT traversal, relay fallback, mDNS local discovery | "How do I reach other devices and move bytes between them?" |
| **Automerge** | CRDT-based document model, conflict resolution | "What is the data? How do merges resolve?" |
| **iroh-blobs** | Content-addressed large file sync, deduplication | "How do I move big files without bloating Automerge docs?" |
| **humansync** | Integration glue — in-process library that wires Iroh + Automerge + blobs | "Discover peers, sync docs, manage identity, expose simple API to apps" |
| **humansync-server** | Always-on cloud peer, device registry, pairing endpoint | "Be available when other devices sleep. Know which NodeIds belong to this user." |

### 5.3 Key Design Decisions

| Decision | Choice | Rationale |
|---|---|---|
| **Network layer** | Iroh only (no WireGuard) | Eliminates Headscale/Tailscale dependency. No VPN client required. One fewer moving part. Iroh handles NAT traversal, encryption, and relay natively. |
| **Auth model** | Simple password + device registry | User sets a server password during setup. New devices pair by presenting the password to the server's pairing API. Server adds their NodeId to the device registry. No OIDC, no certs, no keypair ceremony. |
| **Client model** | In-process library (no daemon) | humansync is a Rust crate that apps link against directly. No separate background process to install, manage, or keep alive. The app *is* the sync node. |
| **Storage** | Automerge docs + iroh-blobs | Structured data (notes, tasks, metadata) lives in Automerge documents. Large files (images, attachments, exports) go through iroh-blobs. This prevents Automerge docs from bloating and keeps blob sync efficient via content-addressing. |
| **Cloud peer** | Yes, always-on | Required for async availability — your phone syncs changes even when your laptop is asleep. The cloud peer is not authoritative; it's just a peer that never sleeps. |
| **Scale target** | ~10k documents, blobs separated | Automerge handles the structured data. Blobs handle the heavy files. This split keeps the CRDT layer fast and the storage layer scalable. |

### 5.4 How Auth Works (No WireGuard)

Without a tailnet as the trust boundary, the server maintains a device registry — a list of Iroh NodeIds that belong to this user.

**Pairing a new device:**

```
1. User deploys humansync-server, sets a password during setup
2. User installs app on a device
3. App generates a persistent Iroh NodeId (keypair stored locally)
4. App prompts: "Enter your server URL and password"
5. App calls POST /pair with { node_id, password }
6. Server verifies password → adds NodeId to device registry
7. Server returns its own NodeId + list of other registered devices
8. Device is now paired. Sync begins.
```

**Connection gating on the server:**

```
On every inbound Iroh connection:
  1. Extract remote NodeId from QUIC handshake
  2. Check: is this NodeId in the device registry?
  3. Yes → proceed with Automerge sync
  4. No  → drop connection
```

**Peer-to-peer between devices:**

```
Devices also gate connections against the device registry.
On pairing, each device receives the current registry.
Registry updates propagate as a special Automerge doc (_devices).
When Device A connects to Device B directly (mDNS or via server hint):
  → Both check each other's NodeId against their local copy of _devices
  → If both are registered → sync
  → If not → drop
```

### 5.5 Peer Discovery (Three Tiers)

```
Tier 1: mDNS (local network)
  → Iroh's built-in mDNS discovers peers on the same WiFi/LAN
  → Zero config, zero server involvement
  → Sub-millisecond latency

Tier 2: Server-mediated discovery
  → On startup, client calls GET /devices on humansync-server
  → Gets NodeIds + Iroh addressing info for all registered devices
  → Connects directly via Iroh QUIC (hole-punching)

Tier 3: Relay fallback
  → If direct QUIC fails (strict NAT, firewall)
  → Iroh falls back to relay servers (n0 public relays or self-hosted)
  → Data still encrypted end-to-end via QUIC TLS
```

---

## 6. Deliverables

### 6.1 humansync-server

A single Docker container that runs:

- **Cloud peer** — always-on Iroh node that participates in Automerge sync and iroh-blob transfers, persisted to disk
- **Device registry** — SQLite database mapping `{ node_id, device_name, paired_at, last_seen }`
- **Pairing API** — HTTP endpoint, password-protected:
  - `POST /pair` — register a new device NodeId (requires password)
  - `GET /devices` — list registered devices (requires valid NodeId)
  - `DELETE /devices/{node_id}` — revoke a device (requires password)
- **Iroh relay** (optional) — self-hosted relay for environments where n0 public relays are undesirable

**Deployment:**

```bash
docker run -d \
  -v humansync-data:/data \
  -p 4433:4433/udp \   # Iroh QUIC
  -p 8080:8080 \       # Pairing API
  humancorp/humansync-server \
  --password "your-server-password"
```

**Server persistence:** Automerge documents stored as files on disk (one file per document). Blobs stored via iroh-blobs' native content-addressed store. Device registry in SQLite. No external database.

**Resource requirements:** Runs on a $5 VPS. Minimal CPU. Memory scales with document count and blob cache size.

### 6.2 humansync (Rust client library)

An in-process Rust crate that any HumanCorp app links against. No separate daemon. The app is the sync node.

**Responsibilities:**

- **Persistent device identity** — generates an Iroh keypair on first launch, stores it locally, reuses across sessions
- **Peer discovery** — mDNS for local network, server API for remote, relay fallback
- **Automerge doc store** — local persistence of all documents to disk
- **iroh-blobs integration** — large file sync via content-addressed blob transfers
- **Background sync loop** — runs in a background thread/task within the app process
- **Device registry sync** — the `_devices` doc propagates the registry to all peers

**App-facing API:**

```rust
// Initialize — starts Iroh node, loads local docs, begins sync
let node = humansync::init(HumanSyncConfig {
    server_url: "https://my.server.com",
    storage_path: "~/.humansync",
    iroh_port: 4433,
})?;

// Pair this device (first time only)
node.pair("server-password").await?;

// Open or create a document
let doc = node.open_doc("notes/shopping-list")?;

// Read/write — always local, always instant
doc.put("eggs", 12)?;
let eggs: i64 = doc.get("eggs")?;

// Store a large file as a blob
let blob_hash = node.store_blob(Path::new("photo.jpg")).await?;

// Reference the blob from a document
doc.put("attachment", blob_hash)?;

// Retrieve a blob (fetches from peers if not local)
let path = node.get_blob(blob_hash).await?;

// List all docs in a namespace
let docs = node.list_docs("notes/")?;

// Sync status (for UI indicators if desired)
let status = node.sync_status();
// status.peers_connected: 2
// status.docs_pending: 0
// status.last_sync: 2026-02-05T01:30:00Z
```

**Sync loop (internal, pseudocode):**

```rust
async fn sync_loop(node: &HumanSyncNode) {
    loop {
        // Tier 1: mDNS discovers local peers automatically (Iroh built-in)

        // Tier 2: Ask server for remote peers
        let peers = node.server_api.get_devices().await?;
        for peer in peers {
            if let Ok(conn) = node.iroh.connect(peer.node_id).await {
                // Verify peer is in device registry
                if node.device_registry.contains(peer.node_id) {
                    sync_automerge_docs(&conn).await;
                    sync_blobs(&conn).await;
                }
            }
        }

        sleep(Duration::from_secs(30)).await;
    }
}
```

**Configuration:**

```toml
[server]
url = "https://my.server.com:8080"

[storage]
path = "~/.humansync/data"

[sync]
interval_secs = 30

[iroh]
port = 4433
# relay = "https://my-relay.example.com"  # optional self-hosted relay
```

### 6.3 Humandocs (First Migrated App)

The first HumanCorp app rebuilt on humansync, proving the architecture end-to-end.

**Migration approach:**

- Rip out current backend/API dependency
- Replace all data reads/writes with humansync document operations
- Large file attachments → iroh-blobs, referenced by hash in Automerge docs
- App works fully offline from first launch
- Sync is invisible to the user — no sync buttons, no conflict UI

---

## 7. Data Model

### 7.1 Document Namespace

```
/{user}/                              ← implicit, single-user MVP
  ├── _devices                        ← device registry (synced as CRDT)
  ├── _settings                       ← cross-app preferences
  │
  ├── humandocs/                      ← Humandocs app namespace
  │   ├── doc-{uuid}                  ← one Automerge doc per document
  │   ├── doc-{uuid}
  │   └── ...
  │
  └── {future-app}/                   ← reserved for future apps
      └── ...
```

### 7.2 Document vs. Blob Split

| Data Type | Storage | Examples |
|---|---|---|
| Structured, mergeable data | Automerge document | Note text, task lists, metadata, settings |
| Large binary files | iroh-blobs | Images, PDFs, attachments, exports |
| Blob references | Automerge field (hash) | `{ "attachment": "bafk...abc" }` |

**Rule of thumb:** If it's text or structured data that two devices might edit concurrently → Automerge. If it's a binary file that gets replaced wholesale → iroh-blob.

### 7.3 Document Granularity

Each Automerge document maps to one logical user object:

| App | One Document = |
|---|---|
| Humandocs | One document/note |
| Future: Tasks | One project |
| Future: Calendar | One calendar |

**Scale target:** ~10,000 Automerge documents. Beyond this, investigate pagination/lazy-loading of the document index. Blobs scale independently via content-addressing.

### 7.4 Schema Strategy

**Additive-only.** Fields can be added to documents. Fields are never removed or renamed. Each document carries a `_schema_version` field. Apps must handle reading documents with schemas older or newer than what they expect. Unknown fields are preserved, never dropped.

---

## 8. User Flows

### 8.1 First-Time Setup

```
1. User deploys humansync-server
   → docker run humancorp/humansync-server --password "..."
   → Server starts: cloud peer (Iroh node) + pairing API
   → Server has a public URL or IP

2. User installs Humandocs on first device (laptop)
   → App generates persistent Iroh NodeId (stored locally)
   → App prompts: "Enter your HumanSync server URL and password"
   → App calls POST /pair → server registers this device's NodeId
   → humansync starts syncing in-process
   → User is in. Starts using Humandocs.
```

### 8.2 Adding a Second Device

```
1. User installs Humandocs on phone
2. Enters same server URL + password
3. App calls POST /pair → phone's NodeId added to registry
4. Server returns list of all registered devices (laptop + cloud peer)
5. humansync connects to laptop (if reachable) and cloud peer via Iroh
6. Automerge syncs all documents, iroh-blobs syncs referenced files
7. All data appears on phone within seconds
```

### 8.3 Offline Editing

```
1. User turns off wifi on laptop
2. Edits a document on laptop (writes to local Automerge doc)
3. Edits the same document on phone (writes to local Automerge doc)
4. User turns wifi back on
5. humansync reconnects peers (mDNS on LAN, or via server)
6. Automerge merges both edits automatically (CRDT)
7. Both devices converge — no conflict UI, no data loss
```

### 8.4 Server Outage

```
1. humansync-server goes down
2. All apps continue working on all devices (local-first)
3. Devices on the same LAN discover each other via mDNS
4. Devices sync directly peer-to-peer without server involvement
5. Server comes back online
6. Cloud peer catches up automatically — just another peer
```

### 8.5 Device Revocation

```
1. User loses phone
2. Calls DELETE /devices/{phone_node_id} on humansync-server (password required)
3. Server removes phone from device registry
4. Updated _devices doc propagates to all remaining peers on next sync
5. Phone's NodeId is no longer recognized → all peers drop its connections
```

### 8.6 Large File Attachment

```
1. User attaches a photo to a Humandoc on laptop
2. humansync stores the photo via iroh-blobs → gets content hash
3. Automerge doc updated: { "attachment": "bafk...abc" }
4. Automerge doc syncs to phone (tiny — just the hash reference)
5. Phone's humansync sees the hash, fetches the blob from laptop/cloud peer
6. Photo available on phone
```

---

## 9. Tech Stack

| Component | Choice | Rationale |
|---|---|---|
| Language | Rust | Iroh and Automerge cores are Rust. Single language. Cross-compilation to all targets. |
| Transport | Iroh (QUIC) | P2P, NAT traversal, relay fallback, mDNS — all built-in. No VPN dependency. |
| Data model | Automerge | Mature CRDT library. JSON-like documents. Automatic conflict resolution. Official Iroh integration. |
| Large files | iroh-blobs | Content-addressed, deduplicated, efficient transfer. Separates heavy data from CRDT docs. |
| Local persistence | Filesystem | One file per Automerge doc. iroh-blobs' native store for binary data. |
| Device registry | SQLite (server) + Automerge doc (synced) | Server is source of truth for pairing. Synced `_devices` doc for peer-to-peer gating. |
| Server deploy | Docker | Single container. `docker run` and done. |
| Admin | CLI / HTTP API | No dashboard in MVP. `curl` or a future CLI tool. |

---

## 10. Security Model

### 10.1 Transport Encryption

All Iroh connections use QUIC with TLS 1.3. Data in transit is encrypted end-to-end between peers, including through relay servers. Relay servers see only encrypted bytes.

### 10.2 Device Authentication

- **Server pairing:** Password-based. The password is a shared secret set during server setup. It is only transmitted over HTTPS (pairing API) during the initial pairing request.
- **Peer-to-peer gating:** After pairing, devices authenticate each other by NodeId. Each device holds a synced copy of the device registry (`_devices` Automerge doc). Connections from unknown NodeIds are dropped before any data is exchanged.

### 10.3 Data at Rest

MVP: Data stored unencrypted on disk. The user's filesystem permissions are the access control boundary. Post-MVP: optional at-rest encryption with a key derived from the user's password.

### 10.4 Threat Model (MVP)

| Threat | Mitigation |
|---|---|
| Network eavesdropping | QUIC TLS on all connections |
| Unauthorized device sync | NodeId checked against device registry before any data flows |
| Server compromise | Server is just a peer. Attacker gets a copy of the data but can't tamper with other devices' local state (CRDTs are append-only). Revoke server, re-pair devices to a new server. |
| Lost/stolen device | Revoke via API. NodeId removed from registry. Peer-to-peer gating rejects the device on next sync. |
| Relay server sniffing | Relay only sees QUIC-encrypted bytes. Cannot read or modify data. |

---

## 11. Feasibility Assessment

| Component | Status | Risk Level |
|---|---|---|
| Iroh (transport) | Pre-1.0, production-deployed, 1.0 targeting Q1 2026 | **Medium** — API churn until stable release |
| iroh-blobs | Part of Iroh ecosystem, actively maintained | **Medium** — evaluate stability for large file workloads |
| Automerge | Mature, widely adopted | **Low** |
| Iroh + Automerge | Official n0 example repo, confirmed supported pattern | **Low–Medium** |
| mDNS local discovery | Built into Iroh, documented | **Low** |
| Self-hosted Iroh relay | Documented, stateless binary | **Low** |

**Highest risk:** The integration surface. Composing Iroh + Automerge + iroh-blobs into a single in-process library with a clean API is where 80% of engineering effort will go.

**Risk removed vs. v0.1:** Eliminating the WireGuard/Headscale dependency removes the untested Iroh-over-WireGuard combination and the VPN-client requirement on every device.

---

## 12. Milestones

| Week | Milestone | Deliverable | Success Criteria |
|---|---|---|---|
| **1–2** | Proof of Concept | Two devices syncing a single Automerge doc via Iroh. Hardcoded NodeIds. No server. | Edit on device A, appears on device B within 5 seconds. Verify mDNS discovery on LAN works. |
| **3–4** | Client Library | `humansync` crate. Persistent identity. Device registry. Background sync loop. iroh-blobs integration. | App links against humansync. `open_doc/put/get` works. Blobs transfer. Sync is automatic. |
| **5–6** | Server | `humansync-server` Docker image. Cloud peer. Pairing API. Device registry in SQLite. | `docker run`, pair a device, device syncs with cloud peer, data persists across restarts. |
| **7–8** | Humandocs Migration | Humandocs rebuilt on humansync. | App works offline. Syncs across 2+ devices. File attachments work via blobs. Passes acceptance test. |

---

## 13. Acceptance Test

The MVP ships when this sequence passes end-to-end:

```
 1. Deploy humansync-server (docker run --password "test")
 2. Install Humandocs on laptop
 3. Pair laptop (enter server URL + password)                 ✓ Pairing
 4. Create a document with text content
 5. Attach a photo to the document
 6. Install Humandocs on phone
 7. Pair phone (enter server URL + password)
 8. Document appears on phone with text                       ✓ Doc sync
 9. Photo attachment loads on phone                           ✓ Blob sync
10. Turn off wifi on laptop
11. Edit document on laptop
12. Edit same document on phone
13. Turn wifi back on
14. Both edits merged, no conflict, no data loss              ✓ CRDT merge
15. Kill humansync-server
16. Edit on laptop
17. Edit on phone (both on same LAN)
18. Edits sync between laptop and phone directly              ✓ P2P via mDNS
19. Restart humansync-server
20. Server catches up with both devices' changes              ✓ Server-as-peer
21. Revoke phone via DELETE /devices/{node_id}
22. Phone can no longer sync with laptop or server            ✓ Revocation
```

All 22 steps pass → MVP is complete.

---

## 14. Future Roadmap (Post-MVP)

These items are explicitly deferred but architecturally accounted for:

| Phase | Feature | Approach |
|---|---|---|
| **v0.2** | Multi-user sharing | Cloud peers connect over public Iroh. Share tokens with doc-level permissions. |
| **v0.3** | Community relays | Raspberry Pi / NAS as shared Iroh relay for LAN-speed cross-user sync. |
| **v0.4** | MoQ real-time media | Voice/video over MoQ. P2P for 1:1, self-hosted relay for group. |
| **v0.5** | App permission scoping | Apps declare which doc namespaces they need. Sync layer enforces. |
| **v0.6** | At-rest encryption | Encrypt local Automerge docs and blob store with key derived from user password. |
| **v0.7** | Headscale/Tailscale integration | Optional WireGuard mesh for users who want it. Transport-agnostic by design. |
| **v1.0** | SDK for third-party apps | Other developers build local-first apps on humansync. Open ecosystem. |

---

## 15. Open Questions

1. **Iroh API stability.** 1.0 is targeting Q1 2026. Pin to the latest RC and upgrade at 1.0, or track main? **Recommendation:** pin to latest RC.

2. **iroh-blobs maturity.** Need to validate: large file transfer reliability, resume after disconnect, storage overhead. **Action:** stress-test in Week 1–2 PoC with files >100MB.

3. **Device registry propagation lag.** When a device is revoked on the server, other devices learn about it on next sync of the `_devices` doc. A revoked device could still sync with peers that haven't received the update yet. **Acceptable for MVP?** Likely yes — the window is at most one sync interval (30s). Post-MVP: push revocation via Iroh gossip for near-instant propagation.

4. **Mobile background sync.** iOS and Android aggressively kill background processes. If humansync is in-process (no daemon), sync only happens when the app is foregrounded. **Acceptable for MVP?** Probably yes — cloud peer handles async availability. Post-MVP: investigate platform-specific background execution (iOS BGProcessingTask, Android WorkManager).

5. **Password security for pairing.** The server password is a single shared secret. If it's compromised, an attacker can register their own device. **Mitigation for MVP:** rate-limit the pairing endpoint, log all pair attempts. Post-MVP: TOTP, device approval flow, or passkey-based pairing.

6. **Document index scaling.** With ~10k docs, the client needs an efficient way to know which docs exist and which need sync. **Approach:** maintain an Automerge index doc (`_index`) that lists all doc IDs and their last-modified timestamps. Sync this doc first, then selectively sync changed docs.

---

*This document is the contract for what gets built. If it's not in Sections 4–6, it doesn't ship in MVP. Everything else is iteration.*
