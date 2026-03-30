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
