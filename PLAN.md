# Embeddable Library Mode

Allow Rust consumers (Tessera, Foedus, and eventually iOS via UniFFI) to use
xs's Store directly in-process without spawning an HTTP server.

## Current State

- `lib.rs` exports all modules publicly with no feature gating.
- Every consumer pulls the entire dependency graph: Nushell (6 crates), hyper,
  iroh, clap, console/chrono.
- `Store` already has a solid public API (append, read, get, remove, cas_*) but
  requires manual CAS-then-append choreography for common operations.

## Design

### Feature Flags

| Feature | Enables | Default |
|---------|---------|---------|
| (core, always on) | `store`, `error`, `scru128` | yes |
| `nu` | `nu`, `processor` modules, `nu_modules_at()` | yes |
| `server` | `api`, `client`, `listener`, `trace` | yes |
| `full` | both `nu` + `server` | meta |

`default = ["nu", "server"]` preserves backward compatibility. Embedding
consumers use `default-features = false`.

### Engine Facade

New `src/store/engine.rs` wrapping `Store` with ergonomic embedding API:

```rust
pub struct Engine { store: Store }

impl Engine {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StoreError>;
    pub fn store(&self) -> &Store;

    // Handles CAS write + frame append in one call
    pub fn append(
        &self,
        topic: impl Into<String>,
        content: Option<&[u8]>,
        meta: Option<serde_json::Value>,
        ttl: Option<TTL>,
    ) -> Result<Frame, Error>;

    pub async fn read(&self, options: ReadOptions) -> Receiver<Frame>;
    pub fn read_sync(&self, options: ReadOptions) -> impl Iterator<Item = Frame>;
    pub fn get(&self, id: &Scru128Id) -> Option<Frame>;
    pub fn remove(&self, id: &Scru128Id) -> Result<(), Error>;

    pub async fn cas_insert(&self, content: &[u8]) -> Result<Integrity, cacache::Error>;
    pub fn cas_insert_sync(&self, content: &[u8]) -> Result<Integrity, cacache::Error>;
    pub async fn cas_read(&self, hash: &Integrity) -> Result<Vec<u8>, cacache::Error>;
    pub fn cas_read_sync(&self, hash: &Integrity) -> Result<Vec<u8>, cacache::Error>;

    // Convenience: read with follow=On, new=true
    pub async fn subscribe(&self) -> Receiver<Frame>;
    pub async fn subscribe_topic(&self, topic: impl Into<String>) -> Receiver<Frame>;

    pub async fn wait_for_gc(&self);
}
```

## Implementation Steps

### Phase 1: Feature-gate Cargo.toml

- Add `[features]` section with `nu` and `server` features.
- Mark Nushell deps (`nu-cli`, `nu-command`, `nu-protocol`, `nu-cmd-lang`,
  `nu-engine`, `nu-parser`) as `optional = true` under `nu`.
- Mark HTTP/network deps (`http`, `hyper`, `iroh`, `rustls`, `tokio-rustls`,
  `url`, `base64`, `console`, `chrono`, `clap`, `serde_urlencoded`, `dirs`,
  `nix`, etc.) as `optional = true` under `server`.
- Core deps remain non-optional: `fjall`, `cacache`, `scru128`, `ssri`, `serde`,
  `serde_json`, `tokio`, `crossbeam-channel`, `bon`, `tracing`, `bytes`,
  `futures`, `tempfile`.

### Phase 2: Conditional compilation in lib.rs

```rust
pub mod error;
pub mod scru128;
pub mod store;

#[cfg(feature = "nu")]
pub mod nu;
#[cfg(feature = "nu")]
pub mod processor;

#[cfg(feature = "server")]
pub mod api;
#[cfg(feature = "server")]
pub mod client;
#[cfg(feature = "server")]
pub mod listener;
#[cfg(feature = "server")]
pub mod trace;
```

### Phase 3: Gate Store methods with server/nu semantics

- `#[cfg(feature = "nu")]` on `Store::nu_modules_at()`.
- `#[cfg(feature = "server")]` on `ReadOptions::from_query()` and
  `ReadOptions::to_query_string()` (depend on `serde_urlencoded`/`url`).

### Phase 4: Create Engine facade

- New file `src/store/engine.rs` with the API above.
- Re-export from `store/mod.rs`.
- Re-export key types at crate root: `Engine`, `Frame`, `ReadOptions`,
  `FollowOption`, `TTL`, `Store`, `StoreError`.

### Phase 5: Binary required-features

```toml
[[bin]]
name = "xs"
path = "src/main.rs"
required-features = ["nu", "server"]
```

### Phase 6: Integration test

New `tests/embed_core_only.rs` exercising `Engine` without Nushell or HTTP:
- Run with `cargo test --no-default-features --test embed_core_only`

### Phase 7: CI matrix expansion

Test `--no-default-features`, `--features nu`, `--features server`, and default.

## iOS/UniFFI Path

Core-only deps are all pure Rust and compile for `aarch64-apple-ios`. The
`Engine` facade with sync methods maps naturally to UniFFI. Tokio "full"
features may need slimming for iOS (only `sync`, `rt`, `rt-multi-thread`,
`macros`, `time` are needed by Store).

## Embedding Example

```toml
# Minimal
cross-stream = { version = "0.12", default-features = false }

# With Nushell processors
cross-stream = { version = "0.12", default-features = false, features = ["nu"] }

# Full (default, current behavior)
cross-stream = { version = "0.12" }
```

```rust
let engine = xs::Engine::open("./my-store")?;

engine.append("sensor.temperature", Some(b"22.5"), None, None)?;

let frames = engine.read_sync(
    ReadOptions::builder().topic("sensor.*".into()).last(10).build()
);
for frame in frames {
    println!("{}: {}", frame.topic, frame.id);
}
```

---

# Graceful Shutdown

Coordinate shutdown of all xs server resources: HTTP listeners, actors,
services, actions, GC thread, and fjall store.

## Current State

- `src/api.rs:458` has a TODO for graceful shutdown.
- Services already partially react to `xs.stopping` (break loop, wait 2s).
- Actors and actions have no shutdown handling.
- SIGTERM is not handled (only ctrl-c via `tokio::signal::ctrl_c()`).
- Processor task handles are mostly discarded (only service handle is captured).

## Resource Inventory

| Resource | Location | Shutdown Need |
|----------|----------|---------------|
| TCP/Unix/Iroh listeners | `api.rs` listener_loop | Stop accepting connections |
| HTTP connections | `api.rs` per-connection spawns | Drain in-flight requests |
| Actor processor | `main.rs` tokio::spawn | Stop LifecycleReader loop |
| Individual actors | `actor.rs` Actor::spawn | Break recv() loop |
| Service processor | `main.rs` tokio::spawn | Already reacts to xs.stopping |
| Individual services | `service.rs` spawn_thread | Already signals engine interrupt |
| Action processor | `main.rs` tokio::spawn | Stop loop, drain in-flight |
| Log stream | `main.rs` trace::log_stream | Terminates when broadcast drops |
| GC worker thread | `store/mod.rs` std::thread | Exits when gc_tx is dropped |
| Fjall database | `store/mod.rs` Store | Persist and close cleanly |

## Shutdown Protocol

```
Phase 1: SIGNAL
  - Receive SIGINT or SIGTERM
  - Append xs.stopping frame
  - Cancel listener accept loops via CancellationToken

Phase 2: DRAIN (up to timeout)
  - Wait for in-flight HTTP connections (JoinSet)
  - Services react to xs.stopping (already implemented)
  - Actors see xs.stopping, emit .unregistered, break
  - Action processor breaks loop, drains in-flight executions

Phase 3: FORCE STOP (after timeout)
  - Abort remaining HTTP/actor/action tasks
  - Drop GC sender to let GC thread drain

Phase 4: FLUSH & CLOSE
  - Append xs.stop frame
  - store.wait_for_gc()
  - Explicit fjall persist
  - Remove Unix socket file
```

## Implementation Steps

### Step 1: Signal handling (SIGINT + SIGTERM)

File: `src/main.rs`

Add a helper function:

```rust
async fn wait_for_shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut sigint = signal(SignalKind::interrupt()).unwrap();
        let mut sigterm = signal(SignalKind::terminate()).unwrap();
        tokio::select! {
            _ = sigint.recv() => {}
            _ = sigterm.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        tokio::signal::ctrl_c().await.ok();
    }
}
```

### Step 2: Thread CancellationToken through the server

File: `src/api.rs`

- `serve()` accepts a `CancellationToken` parameter.
- Pass token into `listener_loop`.
- `listener_loop` uses `tokio::select!` between `listener.accept()` and
  `token.cancelled()`. Break on cancellation.
- Track spawned connection handles in a `tokio::task::JoinSet` for drain.

```rust
async fn listener_loop(
    listener: Listener,
    store: Store,
    engine: nu::Engine,
    token: CancellationToken,
) -> Result<()> {
    let mut connections = JoinSet::new();
    loop {
        tokio::select! {
            biased;
            _ = token.cancelled() => break,
            result = listener.accept() => {
                let (stream, _) = result?;
                connections.spawn(/* connection handler */);
            }
        }
    }
    // Drain in-flight connections (they'll finish naturally)
    while connections.join_next().await.is_some() {}
    Ok(())
}
```

### Step 3: Restructure main.rs serve()

File: `src/main.rs`

- Create CancellationToken at top of `serve()`.
- Capture ALL processor JoinHandles (actor, service, action, log).
- Pass token to `api::serve()`.
- Replace current `tokio::select!` with structured shutdown:

```rust
let token = CancellationToken::new();

let actor_handle = tokio::spawn(actor::run(store.clone()));
let service_handle = tokio::spawn(service::run(store.clone()));
let action_handle = tokio::spawn(action::run(store.clone()));
let log_handle = tokio::spawn(log_stream(store.clone()));
let api_handle = tokio::spawn(api::serve(..., token.clone()));

wait_for_shutdown_signal().await;

// Phase 1
store.append(Frame::builder("xs.stopping").build())?;
token.cancel();

// Phase 2: drain with timeout
let drain_timeout = Duration::from_secs(5);
let _ = tokio::time::timeout(drain_timeout, async {
    let _ = api_handle.await;
    let _ = service_handle.await;
    let _ = action_handle.await;
    let _ = actor_handle.await;
}).await;

// Phase 4
store.append(Frame::builder("xs.stop").build())?;
store.wait_for_gc().await;
```

### Step 4: Action processor reacts to xs.stopping

File: `src/processor/action/serve.rs`

- In the `Lifecycle::Live(frame)` match arm, check for
  `frame.topic == "xs.stopping"` and break.
- Track in-flight `execute_action` spawns in a JoinSet.
- On break, drain the JoinSet with a brief timeout.

### Step 5: Actor processor reacts to xs.stopping

File: `src/processor/actor/serve.rs`

- In the `Lifecycle::Live` match arm, check for `frame.topic == "xs.stopping"`
  and break.

File: `src/processor/actor/actor.rs`

- In `Actor::serve()`, when `frame.topic == "xs.stopping"`, emit an
  `{topic}.unregistered` frame with reason "shutdown", then break.

### Step 6: Clean up Unix socket on shutdown

File: `src/main.rs`

After store is closed, remove `store_path.join("sock")` if it exists.

### Step 7: Shutdown timeout constant

```rust
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);
```

Optionally expose as `--shutdown-timeout` CLI flag on the serve command.

## Design Decisions

- **No /shutdown endpoint.** Signal-based shutdown is the standard for daemons.
  A client can trigger shutdown by appending `xs.stopping` to the stream if
  needed.
- **No Store::shutdown() method.** Rely on dropping all Store clones in the
  right order. The GC thread exits when all gc_tx senders are dropped.
- **5-second default timeout.** Long enough for most in-flight work, short
  enough to not frustrate users.

## Risks

- Broadcast channel buffer (1024) is large enough that `xs.stopping` will
  always be delivered to subscribers.
- Actors replaying long histories may take time to reach `xs.stopping`. The
  timeout handles this.
- Service OS threads using Nushell check `signals.interrupted()` periodically.
  Long-running scripts that don't check will be force-killed by the timeout.

---

# Snapshots / Compaction

Manage unbounded store growth. xs is append-only with TTL-based retention, but
there is no way to truncate history or create a checkpoint.

## Three Operations

1. **Compact** -- remove frames older than a boundary (by ID or timestamp),
   globally or per-topic.
2. **Snapshot** -- export the latest frame per topic plus referenced CAS
   content as a portable archive.
3. **CAS GC** -- remove orphaned CAS content no longer referenced by any frame.

## Compaction Algorithm

```
compact(before: Scru128Id, topic: Option<String>):
  1. Lock append_lock (prevents new frames during scan)
  2. Iterate frames (optionally filtered by topic) up to but not including `before`
  3. Skip xs.* system frames (preserve operational audit trail)
  4. Collect frame IDs to remove
  5. Release append_lock
  6. For each collected ID: store.remove(id)
  7. Append xs.compacted frame with meta:
     {"before": "<id>", "topic": "<topic|null>", "frames_removed": N}
  8. Return CompactResult { frames_removed, boundary_id }
```

Safety properties:
- System frames (`xs.*`) are never compacted.
- Boundary is exclusive (the `before` ID itself is never removed).
- Append lock held only during scan, not during bulk delete.
- Idempotent: running with the same boundary twice is a no-op.

## Snapshot Algorithm

```
snapshot(output_path):
  1. Scan all frames, keep only the latest per topic
  2. Collect referenced CAS hashes
  3. Write frames as NDJSON (frames.jsonl)
  4. Write CAS content alongside (cas/ directory)
```

Output is compatible with the existing `.import` command.

## CAS Orphan GC

```
gc_cas_orphans():
  1. Scan all frames, collect every non-None frame.hash
  2. List all cacache entries via cacache::list_sync()
  3. For each entry not in the referenced set: cacache::remove_hash_sync()
  4. Return count of removed entries
```

Double-check pattern: re-verify each hash before deletion to handle races
with concurrent appends.

## Implementation Steps

### Phase 1: Core Store Methods

File: `src/store/mod.rs`

1. Add `GCTask::Compact` variant:
   ```rust
   Compact {
       before: Scru128Id,
       topic: Option<String>,
       response: oneshot::Sender<CompactResult>,
   }
   ```

2. Define `CompactResult`:
   ```rust
   pub struct CompactResult {
       pub frames_removed: u64,
       pub boundary_id: Scru128Id,
   }
   ```

3. Add `Store::compact(before, topic)` method.
4. Add `Store::compact_by_timestamp(timestamp)` convenience method.
   Convert timestamp to synthetic Scru128Id or scan for boundary.
5. Add `Store::snapshot(writer)` method.
6. Add `Store::collect_referenced_hashes()` method.
7. Add `Store::gc_cas_orphans()` method.
8. Handle `GCTask::Compact` in `spawn_gc_worker`.

### Phase 2: HTTP API

File: `src/api.rs`

1. Add route variants:
   - `POST /compact` -- query params: `before`, `before_timestamp`, `topic`
   - `GET /snapshot` -- streams NDJSON archive
   - `POST /gc/cas` -- triggers CAS orphan cleanup

2. Handlers return JSON results:
   - compact: `{"frames_removed": N, "boundary_id": "..."}`
   - gc/cas: `{"orphans_removed": N}`

### Phase 3: CLI Subcommands

File: `src/main.rs`

1. `xs compact <addr>`
   - `--before <id>` -- compact frames before this SCRU128 ID
   - `--before-timestamp <iso8601>` -- compact frames older than timestamp
   - `--topic <topic>` -- restrict to a specific topic
   - `--dry-run` -- report what would be removed

2. `xs snapshot <addr>`
   - `--output <path>` -- directory to write (default: stdout as NDJSON)

3. `xs gc-cas <addr>`
   - `--dry-run` -- report orphans without removing

### Phase 4: Client Functions

File: `src/client/commands.rs`

- `compact(addr, before, before_timestamp, topic, dry_run)`
- `snapshot(addr, writer)`
- `gc_cas(addr, dry_run)`

### Phase 5: Nushell Commands

File: `xs.nu`

- `.compact` with `--before`, `--before-timestamp`, `--topic`, `--dry-run`
- `.snapshot` with path argument
- `.gc-cas` with `--dry-run`

### Phase 6: Actor Replay Safety

File: `src/processor/actor/actor.rs`

When an actor with `start: "first"` encounters an `xs.compacted` frame during
replay, log a tracing warning indicating partial history. No hard failure --
actors are expected to be resilient to missing history.

### Phase 7: Tests

File: `src/store/` (unit tests)

1. `test_compact_by_id` -- append 10 frames, compact before frame 5, verify
   only frames 5-10 remain, verify xs.compacted exists.
2. `test_compact_by_topic` -- multi-topic, compact one, verify others untouched.
3. `test_compact_preserves_system_frames` -- xs.* frames survive compaction.
4. `test_compact_idempotent` -- compact same boundary twice, second is no-op.
5. `test_snapshot` -- multi-topic, snapshot contains only latest per topic with
   CAS content.
6. `test_gc_cas_orphans` -- append with CAS, remove frames, gc, verify orphaned
   content gone and referenced content preserved.

## Risks

- **Compacting active actor history**: actors with `start: "first"` replaying
  will see partial history. The xs.compacted marker alerts them. Document that
  users should stop actors before compacting, or accept partial replay.

- **CAS orphan false positives**: a frame appended between hash collection and
  deletion could be orphaned incorrectly. The double-check pattern mitigates
  this.

- **Large store performance**: iterating millions of frames for bulk delete is
  slow. Future optimization: batch deletes in fjall. For now, the GC thread
  keeps it off the hot path.

## Integration with TTL/GC

Compaction is routed through the existing GC worker thread via
`GCTask::Compact`, serialized with other GC operations (`Remove`,
`CheckLastTTL`, `Drain`). After compaction, an optional `--gc-cas` flag chains
CAS orphan cleanup.
