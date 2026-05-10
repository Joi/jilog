# jilog

Pluggable session-log review and append-only event ledger.

`jilog` provides two reusable building blocks for systems that want to learn
from their own operational telemetry:

1. **An append-only event ledger** (`ledger-core`, `ledger-sqlite`,
   `ledger-spool`) — segment-based, integrity-checked, with a rebuildable
   SQLite projection and a cross-machine spool transport.
2. **A pluggable session-log review pipeline** (`jilog-review`) — a `Reader`
   trait for "where do session logs live and how do they parse?", a `Tracker`
   trait for "where do issues get filed?", and a set of detectors that emit
   `Signal`s (corrections, errors, workarounds, P0 alerts) into a daily
   markdown digest and/or an issue tracker.

The included CLI binary `jilog` wires a small TOML config to concrete
implementations and runs the review pipeline.

## Workspace layout

```
crates/
  ledger-core/      append-only event ledger (types, segments, integrity)
  ledger-sqlite/    rebuildable SQLite projection over segments
  ledger-spool/     cross-machine spool transport for event segments
  jilog-review/     Reader/Tracker traits, signal detectors, digest renderer
  jilog/            CLI binary (config -> readers + tracker -> digest)
```

## Built-in plugins

**Readers** (where session logs live):

- `amplifier`  — `~/.amplifier/projects/*/transcript.jsonl` (Anthropic-style chat blocks)
- `claude-code` — `~/.claude/projects/**/*.jsonl`
- `generic`    — configurable path glob + JSONL message schema

**Trackers** (where issues get filed):

- `beads`   — reads/writes `.beads/issues.jsonl` via the `bd` CLI
- `github`  — wraps `gh issue create / list / view`
- `none`    — markdown digest only (no issue creation)

## Quick start

```bash
cargo install --path crates/jilog
```

Drop a `jilog.toml` at `~/.jilog.toml`:

```toml
[[reader]]
type = "amplifier"
path = "~/.amplifier/projects"

[[reader]]
type = "claude-code"
path = "~/.claude/projects"

[tracker]
type = "beads"
repo = "~/repos/my-project"

[[zones]]
id = "public-ops"
ledger_path = "~/ops/ledgers/public-ops"
```

Then:

```bash
jilog review nightly                # produce today's digest
jilog review nightly --dry-run      # don't file issues, just print signals
jilog query --class Decision --limit 50
```

## Design

`jilog-review` operates on `Vec<Message>` (an Anthropic-style chat-message
shape with `role` / `content` / `name`). Each `Reader` is responsible for
turning its native session-log format into that shape; detectors are
format-agnostic above the parse layer.

`Tracker::create` is dedup-aware: implementations check `list_open()` first
and return the existing `IssueRef` if a matching open issue already exists.

## Status

- Extracted from `opsctl` 2026-05-10. Same authors and license.
- Public API is `0.1` — expect minor breaks until `0.2`.
- 32+ tests in the ledger crates, 38+ tests in `jilog-review` (ported from
  `opsctl::review_nightly`).

## License

MIT.
