# jilog

> An append-only event ledger and nightly learning loop for personal AI infrastructure.

**Status: alpha — running in production on the author's systems; API may shift until 1.0.**

📐 **[Architecture overview](https://raw.githack.com/Joi/jilog/main/docs/architecture.html)** — illustrated walkthrough of how jilog works.

---

## What it does

jilog watches what your AI agents actually do — not call-level traces, but semantic events: content ingested, task supervised, correction applied, workaround used. It keeps a durable record of those events, then runs a nightly scan of your agent session transcripts to surface patterns: what the system keeps getting wrong, where prompts are failing, what's worth fixing.

The output of a nightly run is:
- A structured digest (markdown + JSON) of patterns found
- New issues filed in your issue tracker for novel learnings
- A check on whether last week's issues are still open ("did we actually improve?")
- Structured signal ready for your agent to synthesize prompt improvement suggestions

jilog is the **observation and structuring layer**. LLM synthesis, prompt rewrites, and triage decisions sit one level up — in the agent or workflow that wraps jilog. This keeps jilog Rust-pure, usable without an API key, and integrable with any agent stack.

**Cross-machine and cross-harness by design.** Pluggable readers normalize sessions from Claude Code, Amplifier, NanoClaw, or any JSONL agent stack into one `Signal` type, so the same nightly loop runs across all of them. `ledger-spool` replicates segment files between hosts, giving you one logical event ledger across a fleet — desktop, laptop, cloud worker, agent host — without a server in the middle.

### Why not LangSmith / Langfuse?

Those tools do call-level tracing: spans, token counts, latency, cost per request. jilog does something different: it aggregates *patterns across sessions over time* and asks whether the system is actually getting better. The two are complementary — trace your LLM calls with OTEL/Langfuse; use jilog to close the loop on whether yesterday's corrections and workarounds are still happening next week.

---

## Architecture

```
Event Plane      ── Append-only segment files (source of truth)
Projection       ── SQLite index (rebuildable at any time)
Action           ── jilog CLI
```

Segment files are the authority. SQLite is a rebuildable index. Nothing generated is manually edited.

---

## Quick Start

```bash
cargo install --git https://github.com/Joi/jilog jilog
```

Create `~/.jilog.toml`:

```toml
[[reader]]
type = "claude-code"
path = "~/.claude/projects"

[[reader]]
type = "amplifier"
path = "~/.amplifier/projects"

[tracker]
type = "github"
repo = "your-org/your-repo"
labels = ["jilog-learning"]

[[zones]]
id = "ops"
ledger_path = "~/.jilog/ledgers/ops"
```

Then:

```bash
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

| Reader | Scans | Status |
|---|---|---|
| `claude-code` | `~/.claude/projects/*/` JSONL | ✅ built-in |
| `amplifier` | `~/.amplifier/projects/*/transcript.jsonl` | ✅ built-in |
| `generic` | Any JSONL matching the jilog signal schema | ✅ built-in |
| `nanoclaw` | SSH into NanoClaw host, scan session logs | 🔜 planned |

Each reader emits normalized `Signal` types: corrections, errors, workarounds, deferrals, patterns. The nightly loop doesn't know which reader produced them. See the `Reader` trait in `crates/jilog-review/src/reader.rs` to implement your own.

---

## Trackers — pluggable issue backends

Learnings from the nightly loop can be filed as issues in any supported tracker:

| Tracker | Notes | Status |
|---|---|---|
| `beads` | JSONL in `.beads/`, git-managed | ✅ built-in |
| `github` | `gh issue` CLI wrapper | ✅ built-in |
| `none` | Markdown digest only, no issue creation | ✅ built-in |
| `kata` | Local SQLite daemon ([wesm/kata](https://github.com/wesm/kata)) | 🔜 planned |

```toml
[tracker]
type = "github"
repo = "Joi/jilog"
labels = ["jilog-learning"]
```

On subsequent nightly runs, jilog checks which `jilog-learning` issues are still open. If a pattern re-appears for an already-open issue, it's a bump — not a new filing. If the issue is closed and the pattern hasn't recurred, it counts as resolved.

See the `Tracker` trait in `crates/jilog-review/src/tracker.rs` to implement your own.

---

## Signal anatomy

Five signal types are detected today. One more (`Pattern`) is reserved in the enum for forward compatibility but no detector produces it yet.

| Signal        | What triggers it                                                    | Detector heuristic                                                                                       |
|---------------|---------------------------------------------------------------------|----------------------------------------------------------------------------------------------------------|
| `Correction`  | User pushes back on an assistant turn in 15–200 chars               | `assistant → user → assistant` window with a short user message in the middle                            |
| `Error`       | A tool call returned a structured failure                           | A `role: tool` message whose JSON content has `success: false`                                           |
| `Workaround`  | Assistant text admits a temporary or hacky path                     | First-match across `for now`, `temporary`, `workaround`, `hardcoded`, `TODO`, `FIXME`, `quick fix`, `hack` |
| `Deferral`    | Assistant text postpones work to a later session                    | First-match across `come back to`, `defer`, `punt on`, `leave for later`, `skipping for now`, `park for now`, `next session`, `circle back` |
| `P0 Alert`    | The same tool failed in **3+ distinct root sessions** in the window | Aggregation pass over `Error` signals (sub-agent sessions with the all-zero prefix are excluded)         |

Each emitted signal carries the `session_id` it came from, so digest and tracker entries always link back to the conversation that produced them.

---

## Example: a day's digest

`jilog review nightly` writes `<digest_dir>/learning-digest-<YYYY-MM-DD>.md`. The format is byte-stable — downstream tooling greps these files, so detectors and the renderer preserve the exact layout shown here.

````markdown
---
date: 2026-05-09
signals_captured: 7
p0_count: 1
corrections: 3
errors: 2
workarounds: 2
deferrals: 0
---

# Learning Digest — 2026-05-09

## P0 Alerts

- **P0 ALERT**: `bash` failed in 4 distinct sessions: 0e91a2b4-..., 2f1c8d77-..., 8a4b1e09-..., c5d76f02-...

## Corrections

- `0e91a2b4-7d3f-4e2a-9c1b-44a7f3d8a1e2` — 'no, use the gog cli for calendar, not osascript'
- `2f1c8d77-91ab-4c5d-8e6f-1a2b3c4d5e6f` — 'wrong path — that file is in jibrain/atlas, not switchboard/people'
- `8a4b1e09-3344-4abc-9def-0123456789ab` — 'stop. read the AGENTS.md before guessing again.'

## Errors

- `0e91a2b4-7d3f-4e2a-9c1b-44a7f3d8a1e2` / `bash`: Command timed out after 30 seconds
- `c5d76f02-aabb-4ccd-9eef-fedcba987654` / `read_file`: ~ was not expanded; use $HOME or an absolute path

## Workarounds

- `2f1c8d77-91ab-4c5d-8e6f-1a2b3c4d5e6f` pattern=`for now`: 'Hardcoding the retry count to 3 for now until we wire it through config.'
- `8a4b1e09-3344-4abc-9def-0123456789ab` pattern=`TODO`: 'TODO: replace this fallback path once the upstream module exposes the new API.'

## Deferrals

_No deferrals detected._
````

The frontmatter is machine-parseable. A common downstream check is "did yesterday's digest go silent on P0?" — `yq '.p0_count' learning-digest-*.md` gives a per-day series.

---

## Example: dry-run output

`--dry-run` skips writing the digest file and skips creating issues, but still prints what would have been emitted. Useful for tuning detectors or inspecting a fresh window before letting it touch the tracker:

```
$ jilog review nightly --dry-run --since 24h
scanned 38 sessions across 2 readers (amplifier=31, claude-code=7)

[correction] 0e91a2b4-... 'no, use the gog cli for calendar, not osascript'
[correction] 2f1c8d77-... 'wrong path — that file is in jibrain/atlas, not switchboard/people'
[error]      0e91a2b4-... bash: Command timed out after 30 seconds
[error]      c5d76f02-... read_file: ~ was not expanded; use $HOME or an absolute path
[workaround] 2f1c8d77-... [for now] 'Hardcoding the retry count to 3 for now until we wire it through config.'
[workaround] 8a4b1e09-... [TODO]    'TODO: replace this fallback path once the upstream module exposes the new API.'

[p0]         bash failed in 4 distinct sessions (threshold=3)

would write:  ~/ops/digests/learning-digest-2026-05-09.md
would create: 7 issues in tracker=beads (dry-run, skipped)
```

---

## Example: issues filed from a digest

When the tracker isn't `none` and `--dry-run` is off, each new signal becomes an issue. `Tracker::create` checks `list_open()` first and returns the existing handle if a matching open issue is already filed, so re-running the same window doesn't churn duplicates.

The issue title is built by `signal_title()` (see `crates/jilog-review/src/tracker.rs`):

```
[jilog/correction] <session_id>: <truncated context>
[jilog/error]      <tool_name>:  <truncated message>
[jilog/workaround] <pattern>:    <truncated context>
[jilog/deferral]   <session_id>: <truncated item>
```

A run that produced the digest above would land entries like these in `.beads/issues.jsonl` with `tracker.type = "beads"` (one JSON object per line, shown pretty-printed):

```jsonl
{
  "id": "myproj-a1b",
  "title": "[jilog/error] bash: Command timed out after 30 seconds",
  "type": "bug",
  "priority": 1,
  "status": "open",
  "labels": ["jilog", "p0", "tool:bash"],
  "body": "Detected by jilog/review on 2026-05-09.\n\nTool `bash` failed in 4 distinct root sessions in the last 24h, exceeding the P0 threshold (3).\n\nExample sessions:\n- 0e91a2b4-7d3f-4e2a-9c1b-44a7f3d8a1e2\n- 2f1c8d77-91ab-4c5d-8e6f-1a2b3c4d5e6f\n- 8a4b1e09-3344-4abc-9def-0123456789ab\n- c5d76f02-aabb-4ccd-9eef-fedcba987654\n\nFirst observed message:\n> Command timed out after 30 seconds\n",
  "source": "jilog/review",
  "created": "2026-05-09T08:02:11Z"
}
{
  "id": "myproj-a1c",
  "title": "[jilog/workaround] for now: 'Hardcoding the retry count to 3 for now until we wire it through co...'",
  "type": "chore",
  "priority": 3,
  "status": "open",
  "labels": ["jilog", "tech-debt", "pattern:for-now"],
  "body": "Detected by jilog/review on 2026-05-09.\n\nSession: 2f1c8d77-91ab-4c5d-8e6f-1a2b3c4d5e6f\nPattern: `for now`\n\nContext:\n> Hardcoding the retry count to 3 for now until we wire it through config.\n",
  "source": "jilog/review",
  "created": "2026-05-09T08:02:11Z"
}
{
  "id": "myproj-a1d",
  "title": "[jilog/correction] 0e91a2b4-7d3f-4e2a-9c1b-44a7f3d8a1e2: 'no, use the gog cli for calendar, not osascr...'",
  "type": "task",
  "priority": 3,
  "status": "open",
  "labels": ["jilog", "correction"],
  "body": "Detected by jilog/review on 2026-05-09.\n\nUser correction in session 0e91a2b4-7d3f-4e2a-9c1b-44a7f3d8a1e2:\n> no, use the gog cli for calendar, not osascript\n\nReview the assistant turn that immediately preceded this correction; the surrounding context likely points at a documented preference, a stale cached path, or a missing skill.\n",
  "source": "jilog/review",
  "created": "2026-05-09T08:02:11Z"
}
```

The next nightly run that observes the same `bash`-timeout cluster will see the existing `[jilog/error] bash: Command timed out after 30 seconds` issue in `list_open()` and **not** file a duplicate — it will return the same `IssueRef`, which is what the digest links back to.

---

## Recurring patterns we've actually seen

Examples of the kinds of things the pipeline tends to surface, drawn from real runs against Amplifier and Claude Code transcripts (paraphrased here to avoid leaking session content):

- **`P0 ALERT bash`** — agents hitting the default 30-second timeout on long-running builds, tests, or `find`. The fix is invariably one of `timeout=`, `run_in_background=true`, or "scope this to a directory."
- **`workaround [for now]`** — feature work that landed with a hardcoded config value waiting for an env-var or settings plumbing pass.
- **`workaround [TODO]`** — error paths the assistant explicitly flagged as incomplete; these are the highest-signal source of follow-up tickets.
- **`correction`** — short user pushbacks like *"wrong path"*, *"that's the old API"*, *"don't auto-resolve conflicts"*. Each one is a signal that some piece of context (a skill, an `AGENTS.md` note, a frontmatter convention) wasn't loaded or wasn't followed.
- **`error read_file: ~ was not expanded`** — recurring tilde-expansion bugs in tool surfaces; the fix is to either pass an absolute path or use `$HOME`.

The point of the nightly review is not that any single one of these is interesting in isolation — it's that the **same** signal showing up across many distinct sessions is a load-bearing operational fact. P0 aggregation turns "annoying-but-tolerable" into "file an issue tonight."

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
| `jilog-review` | Nightly review engine: `Reader` + `Tracker` traits, signal detectors, digest renderer |
| `jilog` | CLI binary: supervise, query, review, issues, status |

Built-in readers and trackers live as modules within `jilog-review`. Implement the `Reader` or `Tracker` trait to add your own without forking.

---

## Used by

- **opsctl** — Joi Ito's private personal AI infrastructure control plane, uses jilog as its ledger substrate and extends it with manifest validation, claims, and Joi-specific readers. Migration to jilog as an external dependency in progress.
- **deshi** — *(planned)* Executive assistant by Tatsuya Ishibe / [isbtty/deshi](https://github.com/isbtty/deshi).

---

## License

MIT
