---
date: 2026-05-24
topic: kiem-notes-app
---

# Kiem — P2P Notes App

## Summary

A native macOS/iOS notes app with an inline Markdown editing interface, built on a portable Rust core with a thin SwiftUI UI layer. Notes are GFM Markdown synced via Automerge CRDTs over local peer-to-peer networking. Identity via `did:plc` (ATProto's decentralized identity method). AI agents participate as full sync peers through a pure-Rust CLI. Designed for independence from cloud services and big company infrastructure.

---

## Problem Frame

Note-taking apps today force a choice: use a polished app tied to a company's cloud (Apple Notes, Notion, etc.) or use a local-only tool with no sync. The cloud-dependent path means your notes live on someone else's infrastructure, subject to their pricing, policies, and continuity. If the service shuts down or changes terms, your data is hostage.

The local-first movement has produced CRDT-based sync that can run without servers, but no production-quality notes app brings this to Apple platforms with a native experience. Meanwhile, AI agents are becoming active participants in knowledge work — writing notes, maintaining task lists, summarizing — but have no good way to share a note surface with their human counterpart. Existing setups require stitching together file sync services, cloud APIs, or manual copy-paste between agent output and note apps.

The gap is a notes app where human and AI writers share the same notes, sync happens automatically between devices on the same network, data never leaves devices you control, and the whole thing works without accounts, servers, or configuration.

---

## Actors

- A1. Human user: Takes notes, organizes with tags, reviews and edits AI-generated content, uses the native macOS/iOS app
- A2. AI agent: Reads, writes, and updates notes via CLI. Runs on an always-on device (e.g., Mac Mini). Participates as a full sync peer
- A3. Peer device: Any device running Kiem (app or CLI) on the local network. Discovers other peers and syncs automatically

---

## Key Flows

- F1. Human note-taking
  - **Trigger:** User opens the app and creates or edits a note
  - **Actors:** A1, A3
  - **Steps:** User opens Kiem → navigates via tag sidebar or smart filter → creates/selects a note → writes GFM Markdown → note persists locally → syncs to any discovered peers
  - **Outcome:** Note is saved locally and propagated to all reachable peers
  - **Covered by:** R1, R2, R3, R7, R8

- F2. AI agent note access
  - **Trigger:** AI agent creates, reads, or modifies a note via CLI
  - **Actors:** A2, A3
  - **Steps:** Agent invokes CLI to list/read/create/update notes → CLI operates on local Automerge documents → changes sync to discovered peers
  - **Outcome:** Agent's changes merge with any concurrent human edits without conflicts
  - **Covered by:** R4, R8, R9, R20

- F3. Peer discovery and sync
  - **Trigger:** A device running Kiem joins a network where other Kiem peers are present
  - **Actors:** A3
  - **Steps:** Device announces presence via local service discovery → discovers other peers → initiates Automerge sync → exchanges document changes bidirectionally → all peers converge
  - **Outcome:** All peers on the network have the same set of notes with all edits merged
  - **Covered by:** R7, R8, R9, R10

- F4. Offline editing and reconnection
  - **Trigger:** User edits notes while disconnected from other peers
  - **Actors:** A1 or A2, A3
  - **Steps:** User/agent edits notes offline → changes accumulate locally → device later joins a network with peers → sync fires automatically → offline edits merge with any changes made by other peers during the gap
  - **Outcome:** No data loss, no manual conflict resolution. All edits from all peers merge cleanly
  - **Covered by:** R8, R9, R10

---

## Requirements

**Core note management**

- R1. `P0` Notes are GFM Markdown documents with support for task lists (`- [ ]` / `- [x]`), headings, bold, italic, code blocks, and links
- R2. `P0` Notes are organized by inline `#hashtags` parsed from the note body. Tags appear in a sidebar for navigation. A note can have multiple tags
- R3. `P1` Built-in smart filters: Todo (notes containing unchecked task list items), Today (notes created or modified today), Untagged (notes with no hashtags), Pinned (user-pinned notes), Trash (soft-deleted notes)
- R4. `P0` Notes have structured metadata: author (human or agent identity), creation date, modification date. Title is derived from the note body (first H1 heading, or first line if no heading — title derived from content). Inline `#hashtags` remain the primary organization mechanism. Metadata is stored as a separate Automerge Map, denormalized to SQLite for fast queries
- R20. `P1` Notes are attributed to their author — human user or specific AI agent. Author identity is recorded in frontmatter so it's clear who wrote or last edited a note

**User interface**

- R5. `P0` Three-column layout on macOS: tag/filter sidebar, note list, note editor. Adapted for iOS screen sizes
- R6. `P0` Inline Markdown rendering in the editor — headings, bold, italic, code render visually as the user types, not in a separate preview pane
- R7. `P1` The app shows sync status — which peers are connected and whether sync is current

**Sync and networking**

- R8. `P0` Each note is a separate CRDT document. Concurrent edits from multiple writers (human or agent) merge automatically without conflicts or data loss
- R9. `P0` Peers discover each other on the local network via service discovery and sync automatically. No manual configuration, no IP addresses, no pairing flow for v1
- R10. `P0` Notes persist locally on each device. The app is fully functional offline. Sync happens opportunistically when peers are reachable
- R11. `P0` When an always-on device (e.g., Mac Mini running the CLI) is present on the network, it naturally becomes the always-available peer. No special "relay mode" or configuration — it's just a peer that never goes offline

**Identity**

- R12. `P2` Pluggable identity system — the architecture supports multiple DID methods. (v1 has only did:plc + did:key fallback; keep identity behind a module boundary but don't over-abstract)
- R13. `P1` Default identity provider is `did:plc` (ATProto) with `did:key` as an offline fallback. Users authenticate with an existing ATProto handle when PLC resolution is available. When offline or PLC is unreachable, local `did:key` identity is used automatically. No raw key management exposed to users

**Architecture**

- R14. `P0` Portable Rust core containing all logic: CRDT sync engine, storage, note model, tag parsing, identity, CLI interface
- R15. `P0` SwiftUI is a thin UI layer: views, navigation, editor, platform-specific service discovery. Business logic lives in the Rust core
- R16. `P0` The CLI is pure Rust with no Swift dependency. It is a first-class peer — same sync protocol, same capabilities as the app, minus the GUI
- R18. `P1` The CLI exposes a complete integration surface — creating notes, reading, updating, tagging, querying by filter (tags, todos, today), and listing
- R19. `P2` An MCP server exposes the same capabilities as the CLI via tool calls, enabling native integration with AI agents. Both CLI and MCP server are backed by the same Rust core
- R17. `P0` The Rust↔Swift boundary is bridged via FFI. The boundary is designed so the Rust core can be reused on other platforms without modification

**Note types and agent integration**

- R21. `P1` Notes serve multiple use cases with the same format: freeform notes, todo/task lists, agent plans, agent memories, and agent skills. The note format (frontmatter + GFM Markdown) is the universal unit across all use cases
- R22. `P2` Notes tagged as skills (e.g., `#skills`) can be loaded into agent configurations via an explicit `kiem setup-skills` command. This symlinks or registers skill-tagged notes into the agent's skill directory, making self-authored skills sync across devices via P2P and usable by any connected agent. Security note: v1 trusts the local network (WPA-encrypted WiFi); the manual setup step provides a human gate before skills become active
- R23. `P0` The Markdown flavor supports frontmatter for metadata, GFM for content (task lists, tables, code blocks), and inline `#hashtags` for organization — optimized to be readable and writable by both humans and agents

**Search and indexing**

- R24. `P0` Full-text search across all notes, available on all interfaces (app, CLI, MCP). Search index lives in the Rust core and updates incrementally as notes change via CRDT sync. Indexes note content, titles, tags, and frontmatter fields
- R25. `P2` Fuzzy search and advanced query features — tolerates typos, structured filters (by tag, author, date range, completion status). Ship basic full-text first, add fuzzy and structured queries when needed

---

## Acceptance Examples

- AE1. **Covers R8, R11.** Given a user editing a note on their iPhone and an AI agent updating the same note via CLI on a Mac Mini, when both are on the same WiFi network, then both sets of changes appear in the merged note on all devices without conflict markers or data loss
- AE2. **Covers R9, R10.** Given a user who edits three notes on their phone while away from home, when they return and their phone joins the home network where a Mac is running Kiem, then all three notes sync automatically without the user taking any action
- AE3. **Covers R2, R3.** Given a note containing `#project` and `#todo` hashtags and an unchecked task list item, then the note appears under both `project` and `todo` tags in the sidebar, and also appears in the Todo smart filter
- AE4. **Covers R13.** Given a new user installing Kiem, when they sign in with their existing ATProto handle (e.g., `@alice.bsky.social`), then their identity is established across all their devices without generating or managing cryptographic keys
- AE5. **Covers R16.** Given a Mac Mini running only the Kiem CLI (no Xcode, no SwiftUI), when an AI agent creates a note via the CLI, then the note syncs to other peers on the network identically to notes created in the app
- AE6. **Covers R20, R4.** Given a note created by Claude via CLI and later edited by the human user in the app, then the frontmatter shows the original author (the agent) and the note's edit history reflects both contributors
- AE7. **Covers R22.** Given a user who writes a skill note tagged `#skills` on their iPhone, when their Mac Mini syncs the note, then running `kiem setup-skills` symlinks that note into the agent's skill directory and the agent can invoke it in subsequent sessions

---

## Success Criteria

- A user or agent can find any note within seconds using partial terms, fuzzy matches, or structured filters — perfect recall across the full collection
- A user can install Kiem on their Mac and iPhone, write notes on both, and see edits appear on the other device within seconds — with zero configuration beyond installing the app
- An AI agent on an always-on Mac Mini can read and write notes via CLI that seamlessly merge with the user's own edits
- Notes remain fully accessible and editable when offline, with no degradation of the editing experience
- The user's data never transits or resides on infrastructure they don't control (for v1 local P2P scope)
- A developer can build the Rust core on Linux and run the CLI without any Apple toolchain

---

## Scope Boundaries

### Deferred for later

- Cross-network sync as a shipped feature (self-hosted relay or direct peer-to-peer connectivity). When this ships, end-to-end transport encryption is bundled — not optional for internet traffic. Encrypted channels established via `did:plc` identity. **Derisked early:** a spike proving two CLI peers syncing over WebSocket with DID-based auth and encryption runs before or in parallel with v1 feature work
- Multi-user note sharing (identity foundation supports it; requires access control and cross-user sync)
- Encrypted-at-rest notes (local storage encryption)
- Device pairing and peer authentication (v1 trusts the local network; cross-network requires DID-based peer authentication)
- Image and file attachment sync
- Import/migration tooling for existing notes apps
- Cross-network sync UX for non-technical users
- Android, Linux, Windows, and web clients — the Rust core architecture preserves this option; scope and branding for future platforms TBD

### Outside this product's identity

- Dependency on large vendors, US-based companies, or proprietary infrastructure for core functionality — independence is a design principle, not a preference. Solutions that introduce these dependencies are rejected by design
- Cloud-hosted sync service — Kiem is local-first and P2P by design, not a cloud app with offline support
- ATProto PDS as storage or discovery — ATProto provides identity only; using it as a data store fights the protocol's design assumptions (public data, content moderation, schema validation)
- Web client — Kiem is a native app, not a browser-based tool
- WYSIWYG rich-text editing — Kiem is a Markdown editor with inline rendering, not a word processor
- Knowledge graph, backlinks, or Zettelkasten features — Kiem is a notes app, not a knowledge management system
- Social or collaborative real-time editing — future sharing is async document exchange, not Google Docs-style co-editing

---

## Key Decisions

- **Rust core + thin SwiftUI:** Prioritizes cross-platform portability and makes the CLI a first-class citizen. Accepted tradeoff: two-language build complexity and FFI boundary design. Validated by Proton's mobile app architecture
- **Automerge CRDT (Rust crate):** Chosen over Yjs (stale Swift bindings), plain file sync (lossy conflict resolution), and git-as-sync (merge conflicts on concurrent edits). Automerge handles concurrent multi-writer merges — the core use case
- **`did:plc` for identity:** Chosen over raw keypairs (poor UX), `did:web` (requires domain ownership, too limiting), and `did:key` (not human-readable). `did:plc` provides accessible human-readable handles without requiring users to manage keys. The spec is open and decentralization work is underway, making it non-dependent on Bluesky. Pluggable architecture allows adding alternative DID methods later
- **Network framework over Multipeer Connectivity:** Multipeer Connectivity is effectively deprecated — Apple staff have acknowledged performance/reliability issues and published migration guidance. Network framework (NWBrowser/NWListener) with Bonjour is the current recommended stack, with iOS 26 adding Wi-Fi Aware
- **Frontmatter + inline hashtags:** YAML frontmatter for structured metadata (author, dates, machine-readable fields), inline `#hashtags` for human-authored organization. Both sync via the CRDT. Frontmatter handles what agents need to write and query; hashtags handle what humans do naturally while writing
- **Skills as notes:** Agent skills stored as Kiem notes become a P2P-synced skill distribution mechanism. Write a skill on any device, tag it `#skills`, and it propagates to all peers. A setup command bridges Kiem notes into agent skill directories — making the notes app the canonical source for self-authored agent capabilities
- **No ATProto for storage or discovery:** Explored using ATProto PDS for encrypted CRDT storage and device discovery. Rejected — ATProto is designed for public, interoperable social data. Encrypted blobs break moderation, feed generation, and schema validation. Publishing device addresses as records creates stale, leaky metadata. ATProto provides identity well; using it beyond that fights the protocol
- **Zero-configuration networking:** No relay mode toggle, no server setup, no pairing flow. Peers auto-discover and sync. Always-on sync emerges from topology (an always-on device is just a peer that never disconnects), not from configuration
- **Early spike for cross-network:** Rather than deferring cross-network sync blindly, a proof-of-concept spike runs early to validate the DID-auth + encrypted WebSocket + Automerge sync integration. This derisks the deferred work without pulling full cross-network UX into v1

---

## Dependencies / Assumptions

- Automerge Rust crate remains actively maintained and stable enough for production use (currently pre-1.0 but actively developed with regular releases)
- UniFFI (or similar) provides a viable Rust↔Swift bridge for the data and call patterns Kiem needs
- Apple's Network framework supports the service discovery and connection patterns needed for Automerge sync on iOS and macOS
- `did:plc` resolution and ATProto handle verification remain available as open, decentralizing infrastructure
- Users who want AI agent integration have an always-on device capable of running the Rust CLI
- Minimum deployment targets: iOS 17+ / macOS 14+ (for modern Network framework APIs and SwiftUI capabilities)

---

## Outstanding Questions

### Deferred to Planning

- [Affects R14][Resolved] Storage backend: SQLite via rusqlite. Automerge documents stored as BLOBs keyed by document UUID, with metadata columns (title, tags, modified_at) for fast listing and filtering. sled rejected (last release Oct 2024, still alpha, effectively unmaintained)
- [Affects R15, R17][Resolved] FFI bridge: UniFFI (v0.31.1, Mozilla-maintained). Pass serialized bytes (Automerge snapshots, sync messages) and plain data structs across the boundary. Keep swift-bridge as a documented fallback if UniFFI's generated Swift types prove unergonomic
- [Affects R6][Needs research] Survey open-source Markdown editors (Swift/SwiftUI and Rust) that could be adapted for inline rendering with Automerge as the backing store. Don't reinvent the wheel — prototype early by integrating an existing editor with Automerge's text type. This is the highest-risk technical item and should be validated before committing to full v1 scope
- [Affects R9][Resolved] Connection model: one NWConnection per peer, multiplexed document sync. Application protocol uses framed messages tagged with document ID. Discover via NWBrowser (Bonjour service type `_kiem._tcp`), accept via NWListener
- [Affects R13][Resolved] Minimal did:plc integration: (1) handle resolution via `com.atproto.identity.resolveHandle` HTTP endpoint, (2) DID document resolution via `plc.directory/{did}`. For v1 (trusted LAN), claim-and-trust: resolve handle, store as identity. For cross-network/v2: ATProto OAuth for cryptographic proof-of-control
- [Affects R16][Resolved] CLI UX: verb-noun commands (`kiem list`, `kiem show <id>`, `kiem create`, `kiem edit <id>`, `kiem search <query>`, `kiem tags`, `kiem sync-status`, `kiem setup-skills`). Default output is human-readable text; `--json` flag for structured agent output. Accept content via stdin. CLI and MCP tools map 1:1
- [Affects R4, R20, R23][Resolved] Metadata stored as a separate Automerge Map at the document root. Document schema: `NoteDoc { metadata: Map { title (derived), tags (derived), author_did, created_at, modified_at, type, pinned, deleted, has_unchecked_todos (derived) }, body: Text }`. Body contains the entire document including what becomes the title. Title is derived from body's first H1 heading or first line (title derived from content). Tags derived from inline `#hashtags`. Both are denormalized to metadata Map for fast SQLite queries. When exporting to Markdown files, serialize metadata as YAML frontmatter. Required: author_did, created_at. Derived: title, tags, has_unchecked_todos. Optional: type, pinned, deleted
- [Affects R22][Resolved] Skill setup: `kiem setup-skills` scans for `#skills` notes, exports each as Markdown to `~/.kiem/skills/`, symlinks into agent skill directory. Export-on-sync keeps content fresh (Rust core updates exported file when Automerge document changes). No watcher needed. Command is idempotent — re-running adds new skills, removes stale symlinks
- [Affects R24, R25][Resolved] Search library: tantivy (v0.26.1, embeddable Rust FTS engine). MeiliSearch rejected (it's a server, not an embeddable library; its core engine milli is not maintained as a public crate). For fuzzy UI matching (as-you-type on titles/tags), complement with nucleo. Index updates incrementally per-document on CRDT sync
