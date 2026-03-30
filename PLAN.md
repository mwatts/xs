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
