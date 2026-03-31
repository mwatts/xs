# Proposal: Embeddable Library Mode for xs

## What

A feature-gated build of `cross-stream` that exposes the core store and a new
`Engine` facade without pulling in Nushell, HTTP, or CLI dependencies. Consumers
opt in to the pieces they need via Cargo features.

## Why

xs is the event backbone for Tessera and Foedus. Both need in-process access to
the store. Today, consuming `cross-stream` as a library drags in the full
dependency graph (6 Nushell crates, hyper, iroh, clap, chrono, console, etc.)
even when the consumer only needs append/read/subscribe.

Separately, the iOS/UniFFI path (Thyra, Mikra) requires a minimal pure-Rust core
that cross-compiles to `aarch64-apple-ios`. The current dependency surface
includes C bindings and platform-specific code that complicate mobile builds.

Feature gating solves both problems without forking or restructuring the crate.

## Feature Flags

| Feature | What it enables | Default? |
|---------|-----------------|----------|
| (core)  | `store`, `error`, `scru128`, `Engine` facade | always on |
| `nu`    | Nushell integration, processor module | yes |
| `server`| HTTP API, client, listener, tracing | yes |

`default = ["nu", "server"]` -- existing users see no change.

## How to Consume

### Minimal (store only, no Nushell or HTTP)

```toml
[dependencies]
cross-stream = { version = "0.12", default-features = false }
```

### With Nushell processors, no HTTP

```toml
[dependencies]
cross-stream = { version = "0.12", default-features = false, features = ["nu"] }
```

### Full (current behavior, the default)

```toml
[dependencies]
cross-stream = { version = "0.12" }
```

## Engine API

The `Engine` struct wraps `Store` and handles CAS-then-append choreography in a
single call. Sync and async variants are provided.

```rust
use xs::{Engine, ReadOptions};

// Open or create a store
let engine = Engine::open("./my-store")?;

// Append a frame (CAS write + frame creation in one step)
engine.append("sensor.temperature", Some(b"22.5"), None, None)?;

// Read last 10 frames matching a topic
let frames = engine.read_sync(
    ReadOptions::builder().topic("sensor.*".into()).last(10).build()
);
for frame in frames {
    println!("{}: {}", frame.topic, frame.id);
}

// Subscribe to new frames (async)
let mut rx = engine.subscribe().await;
while let Some(frame) = rx.recv().await {
    // process frame
}
```

The full Engine API surface is documented in PLAN.md.

## What This Does Not Change

- The `xs` binary requires both `nu` and `server` features, same as today.
- The store format is unchanged. An embedded consumer and the CLI can share the
  same store directory.
- No new crate splits. This is feature gating within the existing
  `cross-stream` crate.

## iOS/UniFFI Path

Core-only dependencies are pure Rust and compile for `aarch64-apple-ios`. The
`Engine` facade with its sync methods maps directly to UniFFI exports. Tokio
feature requirements can be slimmed to `sync`, `rt`, `rt-multi-thread`, `macros`,
and `time`.

## Implementation

See PLAN.md for the phased implementation steps, dependency classification, and
CI matrix details.

---

# Proposal: Graceful Shutdown

## What

Coordinated shutdown for all xs server resources. On receiving SIGINT or
SIGTERM, the server drains in-flight work, notifies actors and services
via the stream protocol, persists state, and exits cleanly.

## Why

Today, `xs serve` only handles ctrl-c (SIGINT) and does so abruptly:

- Listeners, actors, actions, and spawned connections are abandoned mid-flight.
- SIGTERM (the signal sent by systemd, Docker, process managers) is ignored
  entirely.
- The fjall store may not be flushed. In-flight appends can be lost.
- The Unix socket file is not cleaned up.
- There is a TODO at `src/api.rs:458` acknowledging the gap.

Services partially react to `xs.stopping` already, but nothing else in the
system participates.

## Behavior After This Change

### Signal handling

Both SIGINT and SIGTERM trigger the same orderly shutdown sequence. On
non-Unix platforms, ctrl-c is handled as before.

### Shutdown sequence

1. **Signal received.** The server appends an `xs.stopping` frame to the
   stream and cancels listener accept loops. No new connections are accepted.

2. **Drain phase (up to timeout).** In-flight HTTP requests complete. Services
   and actors observe `xs.stopping` through their normal stream subscriptions
   and wind down. Actors emit `{topic}.unregistered` with reason "shutdown"
   before exiting. Action processors stop accepting new work and drain
   in-flight executions.

3. **Force stop (after timeout).** Any remaining tasks are aborted.

4. **Flush and close.** The server appends an `xs.stop` frame, waits for the
   GC worker to drain, persists the fjall store, and removes the Unix socket
   file.

### Stream protocol additions

| Topic | When | Purpose |
|-------|------|---------|
| `xs.stopping` | Phase 1 | Tells subscribers shutdown has begun |
| `xs.stop` | Phase 4 | Final frame; confirms clean shutdown |

Any client or service subscribed to the stream will see `xs.stopping` and can
use it to finish work before the timeout expires.

### CLI

One optional new flag on `xs serve`:

```
--shutdown-timeout <seconds>    Drain timeout before force-stopping tasks (default: 5)
```

No other CLI changes. No new subcommands.

### What stays the same

- No HTTP shutdown endpoint. Shutdown is signal-driven, which is the standard
  for Unix daemons. A client that needs programmatic shutdown can append
  `xs.stopping` to the stream directly.
- The store API does not change. Cleanup relies on dropping Store handles in
  the correct order.
- Existing service shutdown behavior (react to `xs.stopping`, wait 2s) is
  preserved and now part of a coordinated sequence rather than ad-hoc.

## Risks

- Actors replaying long histories may not reach `xs.stopping` before the
  timeout. They will be force-stopped.
- Nushell service scripts that do not periodically check `signals.interrupted()`
  will block until the timeout expires, then be killed.
- The broadcast channel buffer (1024) is large enough that `xs.stopping`
  delivery is not a concern in practice.

## See Also

`PLAN.md` contains the full implementation plan with code-level details.

---

# Proposal: Snapshots and Compaction

## Problem

xs is append-only. Frames accumulate indefinitely. There is no mechanism to:

- Truncate old history to reclaim disk space
- Export a point-in-time checkpoint for backup or migration
- Clean up CAS content orphaned after frame deletion (e.g. via TTL expiry)

Long-running instances grow without bound. Operators have no tools to manage
this beyond stopping the process and manually editing storage.

## Proposed Operations

Three new operations, each independent:

### 1. Compact

Remove frames older than a given boundary. The boundary is specified as a
SCRU128 ID or a timestamp. Optionally scoped to a single topic.

Safety guarantees:
- System frames (`xs.*`) are never removed.
- The boundary frame itself is never removed.
- An `xs.compacted` marker frame is appended after each compaction, recording
  what was removed.
- Idempotent: compacting with the same boundary twice is a no-op.
- Supports `--dry-run` to preview what would be removed.

### 2. Snapshot

Export the latest frame per topic, plus all referenced CAS content, as a
portable archive. Output is NDJSON (compatible with `.import`), so a snapshot
can be loaded into a fresh xs instance.

Use cases: pre-migration backup, seeding a new environment, disaster recovery.

### 3. CAS GC

Scan all frames, collect every referenced content hash, then remove any CAS
entries not in that set. Handles races with concurrent appends via a
double-check before each deletion.

Supports `--dry-run` to report orphans without deleting.

## Interface

All three operations are exposed through:

- **CLI subcommands**
- **HTTP endpoints**
- **Nushell commands** (in `xs.nu`)

### CLI

```
# Compact frames older than 7 days
xs compact ./store --before-timestamp 2026-03-23T00:00:00Z

# Compact a single topic
xs compact ./store --before-timestamp 2026-03-23T00:00:00Z --topic sensor.readings

# Dry run
xs compact ./store --before-timestamp 2026-03-23T00:00:00Z --dry-run

# Snapshot to a directory
xs snapshot ./store --output /backups/2026-03-30/

# Clean up orphaned CAS content
xs gc-cas ./store
xs gc-cas ./store --dry-run
```

### HTTP

```
POST /compact?before_timestamp=2026-03-23T00:00:00Z
POST /compact?before_timestamp=2026-03-23T00:00:00Z&topic=sensor.readings
GET  /snapshot
POST /gc/cas
```

Compact and GC return JSON: `{"frames_removed": N}` / `{"orphans_removed": N}`.
Snapshot streams NDJSON.

### Nushell

```nu
# Compact everything older than 7 days
.compact --before-timestamp (date now | $in - 7day | format date "%Y-%m-%dT%H:%M:%SZ")

# Snapshot before a migration
.snapshot /backups/pre-migration/

# GC orphaned CAS content
.gc-cas
.gc-cas --dry-run
```

## Interaction with Existing Features

- **TTL/GC**: Compaction runs on the existing GC worker thread, serialized with
  TTL expiry and drain operations. No new concurrency concerns.
- **Actors**: Actors with `start: "first"` replaying after compaction will see
  partial history. The `xs.compacted` marker frame signals this. Actors should
  be resilient to missing history, or be stopped before compacting.
- **Import**: Snapshot output is NDJSON, directly compatible with `.import`.

## What This Does Not Cover

- Automatic scheduled compaction (can be built with existing cron + CLI).
- Incremental or streaming snapshots.
- Cross-store replication.

These may be added later. This proposal targets the minimum viable operations
for managing store growth.
