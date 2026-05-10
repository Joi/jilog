# jilog

> An append-only event ledger and nightly learning loop for personal AI infrastructure.

**Status: early design / pre-release**

---

## What it does

jilog watches what your AI agents actually do — not call-level traces, but semantic events: content ingested, task supervised, correction applied, workaround used. It keeps a durable record of those events, then runs a nightly scan of your agent session transcripts to surface patterns: what the system keeps getting wrong, where prompts are failing, what's worth fixing.

The output of a nightly run is:
- A structured digest (markdown + JSON) of patterns found
- New issues filed in your issue tracker for novel learnings
- A check on whether last week's issues are still open ("did we actually improve?")
- Structured signal ready for your agent to synthesize prompt improvement suggestions

jilog is the **observation and structuring layer**. LLM synthesis, prompt rewrites, and triage decisions sit one level up — in the agent or workflow that wraps jilog. This keeps jilog Rust-pure, usable without an API key, and integrable with any agent stack.

---

## Architecture

```
Intent           ── What your agents are supposed to do (config)
Event Plane      ── Append-only segment files (source of truth)
Projection       ── SQLite index (rebuildable at any time)
Action           ── jilog CLI
```

Segment files are the authority. SQLite is a rebuildable index. Nothing generated is manually edited.

---

## Quick Start

```bash
cargo install jilog

# Configure
cp jilog.example.toml jilog.toml

# Wrap any recurring task with ledger events
jilog supervise --task "jibrain-heartbeat" -- ./jibrain-heartbeat.sh

# Query what happened
jilog query --since 7d
jilog query --since 24h --subsystem "review-*" --json

# Run the nightly learning loop
jilog review nightly
jilog review nightly --json | your-agent synthesize-suggestions
```

---

## Readers — pluggable session log types

jilog can scan transcripts from different agent systems. Configure one or more readers:

| Reader | Scans | Notes |
|---|---|---|
| `claude-code` | `~/.claude/projects/*/` JSONL | Default for Claude Code users |
| `amplifier` | `~/.amplifier/projects/*/transcript.jsonl` | Amplifier sessions |
| `nanoclaw` | SSH into NanoClaw host, scan session logs | Multi-channel setups |
| `generic` | Any JSONL matching the jilog signal schema | BYO agent system |

```toml
# jilog.toml
[[reader]]
type = "claude-code"
path = "~/.claude/projects"

[[reader]]
type = "amplifier"
path = "~/.amplifier/projects"
```

Each reader emits normalized `Signal` types: corrections, errors, workarounds, deferrals, patterns. The nightly loop doesn't know which reader produced them.

---

## Trackers — pluggable issue backends

Learnings from the nightly loop can be filed as issues in any supported tracker:

| Tracker | Notes |
|---|---|
| `beads` | JSONL in `.beads/`, git-managed |
| `kata` | Local SQLite daemon (wesm/kata) |
| `github` | `gh issue` CLI wrapper |
| `none` | Markdown digest only, no issue creation |

```toml
[tracker]
type = "github"
repo = "Joi/jilog"
labels = ["jilog-learning"]
```

On subsequent nightly runs, jilog checks which `jilog-learning` issues are still open. If a pattern re-appears for an already-open issue, it's a bump — not a new filing. If the issue is closed and the pattern hasn't recurred, it counts as resolved.

---

## Commands

```bash
jilog supervise                     # Wrap tasks with ledger events + retry
jilog query [--since N] [--json]    # Filter ledger events
jilog review nightly [--json]       # Nightly learning digest + issue filing
jilog review sessions               # Session-level summary by reader
jilog issues list                   # Open jilog-learning issues across trackers
jilog issues pending                # Learnings not yet filed
jilog rebuild                       # Rebuild SQLite from segment files
jilog status                        # Ledger health
```

---

## Event model

Ten core event classes. All stored as append-only segment files; nothing is deleted.

| Class | When |
|---|---|
| `ingest` | Content arrived |
| `route` | Content directed to destination |
| `decision` | Human or system decision |
| `state_change` | Object state transition |
| `health` | System health observation |
| `delivery` | Notification delivered |
| `projection` | Projection refreshed |
| `note_meta` | Operational note lifecycle |
| `review` | Nightly review run |
| `learning` | Pattern extracted from session |

---

## Crates

| Crate | Purpose |
|---|---|
| `ledger-core` | Event types, segment format, CRC32 integrity, zone store |
| `ledger-sqlite` | Rebuildable SQLite projection, event queries |
| `ledger-spool` | Cross-machine transport with dedup and integrity checks |
| `jilog-review` | Nightly review engine: signal extraction, dedup, digest generation |
| `jilog` | CLI binary: supervise, query, review, issues, status |

Readers and trackers are compiled in via feature flags or separate crates in `readers/` and `trackers/`.

---

## Used by

- **opsctl** — Joi Ito's private personal AI infrastructure control plane, uses jilog as its ledger substrate and extends it with manifest validation, claims, and Joi-specific readers.
- **deshi** — *(planned)* Executive assistant by Tatsuya Ishibe / isbtty.

---

## License

MIT
