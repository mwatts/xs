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
