use std::path::Path;

use crate::error::Error;
use crate::store::{FollowOption, Frame, ReadOptions, Store, StoreError, TTL};

use scru128::Scru128Id;

/// High-level facade over Store for embedding xs in-process.
///
/// Handles CAS write + frame creation in a single call, and provides
/// convenience methods for common patterns like subscribing to topics.
pub struct Engine {
    store: Store,
}

impl Engine {
    /// Open (or create) a store at the given path.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StoreError> {
        let store = Store::new(path.as_ref().to_path_buf())?;
        Ok(Engine { store })
    }

    /// Access the underlying Store directly.
    pub fn store(&self) -> &Store {
        &self.store
    }

    /// Append a frame, optionally writing content to CAS first.
    ///
    /// If `content` is provided, it is written to CAS and the resulting hash
    /// is attached to the frame. This combines what would otherwise be two
    /// separate calls (cas_insert + append).
    pub fn append(
        &self,
        topic: impl Into<String>,
        content: Option<&[u8]>,
        meta: Option<serde_json::Value>,
        ttl: Option<TTL>,
    ) -> Result<Frame, Error> {
        let hash = match content {
            Some(bytes) => Some(self.store.cas_insert_sync(bytes)?),
            None => None,
        };

        let frame = Frame::builder(topic)
            .maybe_hash(hash)
            .maybe_meta(meta)
            .maybe_ttl(ttl)
            .build();
        self.store.append(frame)
    }

    /// Read frames synchronously with the given options.
    pub fn read_sync(&self, options: ReadOptions) -> impl Iterator<Item = Frame> + '_ {
        self.store.read_sync(options)
    }

    /// Read frames asynchronously with the given options.
    pub async fn read(&self, options: ReadOptions) -> tokio::sync::mpsc::Receiver<Frame> {
        self.store.read(options).await
    }

    /// Get a single frame by ID.
    pub fn get(&self, id: &Scru128Id) -> Option<Frame> {
        self.store.get(id)
    }

    /// Remove a frame by ID.
    pub fn remove(&self, id: &Scru128Id) -> Result<(), Error> {
        self.store.remove(id)
    }

    /// Insert content into CAS (async).
    pub async fn cas_insert(&self, content: &[u8]) -> cacache::Result<ssri::Integrity> {
        self.store.cas_insert(content).await
    }

    /// Insert content into CAS (sync).
    pub fn cas_insert_sync(&self, content: &[u8]) -> cacache::Result<ssri::Integrity> {
        self.store.cas_insert_sync(content)
    }

    /// Read content from CAS by hash (async).
    pub async fn cas_read(&self, hash: &ssri::Integrity) -> cacache::Result<Vec<u8>> {
        self.store.cas_read(hash).await
    }

    /// Read content from CAS by hash (sync).
    pub fn cas_read_sync(&self, hash: &ssri::Integrity) -> cacache::Result<Vec<u8>> {
        self.store.cas_read_sync(hash)
    }

    /// Subscribe to all new frames (follow mode, new only).
    pub async fn subscribe(&self) -> tokio::sync::mpsc::Receiver<Frame> {
        let options = ReadOptions::builder()
            .follow(FollowOption::On)
            .new(true)
            .build();
        self.store.read(options).await
    }

    /// Subscribe to new frames matching a specific topic.
    pub async fn subscribe_topic(
        &self,
        topic: impl Into<String>,
    ) -> tokio::sync::mpsc::Receiver<Frame> {
        let options = ReadOptions::builder()
            .follow(FollowOption::On)
            .new(true)
            .topic(topic.into())
            .build();
        self.store.read(options).await
    }

    /// Wait for all pending GC tasks to complete.
    pub async fn wait_for_gc(&self) {
        self.store.wait_for_gc().await
    }
}
