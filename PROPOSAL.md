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
