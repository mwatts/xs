//! Integration test exercising the Engine facade without Nu or HTTP features.
//! Run with: cargo test --no-default-features --test embed_core_only

use xs::store::{FollowOption, ReadOptions, TTL};
use xs::Engine;

#[test]
fn engine_open_and_append() {
    let dir = tempfile::tempdir().unwrap();
    let engine = Engine::open(dir.path()).unwrap();

    let frame = engine
        .append("test.topic", Some(b"hello"), None, None)
        .unwrap();

    assert_eq!(frame.topic, "test.topic");
    assert!(frame.hash.is_some());
}

#[test]
fn engine_append_with_meta() {
    let dir = tempfile::tempdir().unwrap();
    let engine = Engine::open(dir.path()).unwrap();

    let meta = serde_json::json!({"key": "value"});
    let frame = engine
        .append("test.meta", None, Some(meta.clone()), None)
        .unwrap();

    assert_eq!(frame.topic, "test.meta");
    assert!(frame.hash.is_none());
    assert_eq!(frame.meta, Some(meta));
}

#[test]
fn engine_append_with_ttl() {
    let dir = tempfile::tempdir().unwrap();
    let engine = Engine::open(dir.path()).unwrap();

    let frame = engine
        .append("test.ttl", Some(b"data"), None, Some(TTL::Ephemeral))
        .unwrap();

    assert_eq!(frame.ttl, Some(TTL::Ephemeral));
}

#[test]
fn engine_cas_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let engine = Engine::open(dir.path()).unwrap();

    let hash = engine.cas_insert_sync(b"round-trip-data").unwrap();
    let data = engine.cas_read_sync(&hash).unwrap();

    assert_eq!(data, b"round-trip-data");
}

#[test]
fn engine_get_and_remove() {
    let dir = tempfile::tempdir().unwrap();
    let engine = Engine::open(dir.path()).unwrap();

    let frame = engine
        .append("test.remove", Some(b"bye"), None, None)
        .unwrap();
    let id = frame.id;

    assert!(engine.get(&id).is_some());
    engine.remove(&id).unwrap();
    assert!(engine.get(&id).is_none());
}

#[test]
fn engine_read_sync() {
    let dir = tempfile::tempdir().unwrap();
    let engine = Engine::open(dir.path()).unwrap();

    engine.append("a.topic", Some(b"1"), None, None).unwrap();
    engine.append("b.topic", Some(b"2"), None, None).unwrap();
    engine.append("a.topic", Some(b"3"), None, None).unwrap();

    let options = ReadOptions::builder()
        .follow(FollowOption::Off)
        .topic("a.topic".to_string())
        .build();
    let frames: Vec<_> = engine.read_sync(options).collect();
    assert_eq!(frames.len(), 2);
    assert!(frames.iter().all(|f| f.topic == "a.topic"));
}

#[test]
fn engine_read_sync_last() {
    let dir = tempfile::tempdir().unwrap();
    let engine = Engine::open(dir.path()).unwrap();

    for i in 0..5 {
        engine
            .append("count", Some(format!("{i}").as_bytes()), None, None)
            .unwrap();
    }

    let options = ReadOptions::builder()
        .follow(FollowOption::Off)
        .last(2_usize)
        .build();
    let frames: Vec<_> = engine.read_sync(options).collect();
    assert_eq!(frames.len(), 2);
}

#[tokio::test]
async fn engine_subscribe() {
    let dir = tempfile::tempdir().unwrap();
    let engine = Engine::open(dir.path()).unwrap();

    let mut rx = engine.subscribe_topic("live.events").await;

    engine
        .append("live.events", Some(b"event1"), None, None)
        .unwrap();

    let frame = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
        .await
        .expect("timed out waiting for frame")
        .expect("channel closed");

    assert_eq!(frame.topic, "live.events");
}
