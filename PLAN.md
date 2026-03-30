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
