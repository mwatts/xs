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
