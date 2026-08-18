# jilog

[![Rust](https://github.com/Joi/jilog/actions/workflows/rust.yml/badge.svg)](https://github.com/Joi/jilog/actions/workflows/rust.yml)

> An append-only event ledger and nightly learning loop for personal AI infrastructure.

**Status: alpha — running in production on the author's systems; API may shift until 1.0.**

Works with Amplifier: a built-in reader scans Amplifier session transcripts, and the same nightly loop covers Claude Code, Codex, and GitHub Copilot sessions.

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

**Cross-machine and cross-harness by design.** Pluggable readers normalize sessions from Claude Code, Amplifier, Codex, GitHub Copilot, or any JSONL agent stack into one `Signal` type, so the same nightly loop runs across all of them — including NanoClaw cell bots, whose signals carry persona + channel dimensions. `ledger-spool` replicates segment files between hosts, giving you one logical event ledger across a fleet — desktop, laptop, cloud worker, agent host — without a server in the middle: every machine runs `jilog spool emit` (own-host segments into a synced spool directory), one always-on authority runs `jilog spool ingest` (validate, deduplicate, commit into the single-writer fleet store), and any file-sync tool you already run (Syncthing here) is the transport.

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

## Quick Start (5 minutes)

**1. Install** — pinned release or moving main:

```bash
cargo install --git https://github.com/Joi/jilog --tag v0.5.0 jilog   # pinned
cargo install --git https://github.com/Joi/jilog jilog                # moving main
```

Fleet replication (`jilog spool`, below) ships in v0.5.0 — it requires
v0.5.0 or later; earlier tags (v0.2.0–v0.4.0) do not have the `spool`
command. (The v0.5.0 tag is created with the release this section ships
in.)

**2. Configure one reader.** Create `~/.jilog.toml` pointing at whichever agent you already run — one reader is enough to start (add more later; see the readers table below):

```toml
[[reader]]
type = "claude-code"
path = "~/.claude/projects"
```

**3. Run the review:**

```bash
jilog review nightly --since 7d
```

**4. Read the digest.** The path is printed on completion (default `~/.jilog/digests/learning-digest-<date>.md`) — corrections, errors, workarounds, deferrals, health patterns, and spend from the last 7 days. That's the loop; run it nightly from cron/launchd.

From there, grow into the rest:

```toml
# More readers — amplifier/context-intelligence/pi add health patterns + spend
[[reader]]
type = "amplifier"
path = "~/.amplifier/projects"

[[reader]]
type = "pi"
path = "~/.pi/agent/sessions"

# Fleet chat bots (NanoClaw cells): persona + channel dimensions, health
# patterns, and a per-cell trust-tier allowlist (exclude wins; a non-empty
# include admits only matching agents — matched on agent id/persona/folder;
# with any filter set, agents the v2.db can't resolve are skipped: fail closed)
[[reader]]
type = "nanoclaw"
path = "~/nanoclaw/data"
exclude = ["bifbot"]

# A tracker turns recurring signals into issues (kata, github, none)
[tracker]
type = "github"
repo = "your-org/your-repo"

# Zones for the event ledger (jilog query; supervise is planned)
[[zone]]
id = "ops"
ledger_path = "~/.jilog/ledgers/ops"
```

```bash
# Query the event ledger
jilog query --since 7d
jilog query --since 24h --subsystem "review-*" --json

# Nightly loop with issue filing, machine-readable output
jilog review nightly --create-issues
jilog review nightly --json | your-agent synthesize-suggestions
```

---

## Readers — pluggable session log types

jilog can scan transcripts from different agent systems. Configure one or more readers:

| Reader | Scans | Health signals | Status |
|---|---|---|---|
| `claude-code` | `~/.claude/projects/**/*.jsonl` (`{type, message: {role, content}}` wrapper format) | — | ✅ built-in |
| `amplifier` | `~/.amplifier/projects/<project>/sessions/<sess>/{transcript,events}.jsonl` (both legacy flat and current nested layouts; `events.jsonl` is synthesized into Schema-B on the fly) | ✅ (events.jsonl sessions) | ✅ built-in |
| `context-intelligence` | `~/.amplifier/projects/<project>/sessions/<sess>/context-intelligence/events.jsonl` (amplifier-bundle-context-intelligence event streams; sibling `metadata.json` is version-gated per contract — format `context-intelligence`, semver major 1 — incompatible sessions are skipped with a warning) | ✅ | ✅ built-in |
| `codex` | `~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl` (Codex CLI rollouts; user + assistant `response_item` messages) | — | ✅ built-in |
| `copilot` | `~/.copilot/session-state/<uuid>/events.jsonl` (GitHub Copilot CLI; `user.message` + `assistant.message` events) | — | ✅ built-in |
| `pi` | `~/.pi/agent/sessions/<project-slug>/<timestamp>_<uuid>.jsonl` ([pi.dev](https://pi.dev) coding agent, session format v3; user + assistant + toolResult messages, per-call usage/cost → spend section) | ✅ | ✅ built-in |
| `generic` | Any JSONL of `{"role","content","name"}` lines at a configured glob (`path`, `~` ok) with `session_id_from = "parent_dir" \| "file_stem"`; an optional first line `{"_jilog": {"persona": …, "channel": …}}` stamps the fleet dimensions (Personas rollup + chat-tuned corrections) — how Hermes profiles on jibotmac reach jilog (cell-fleet `scripts/hermes-jilog-export.py`) | — | ✅ built-in |
| `nanoclaw` | `<data>/v2-sessions/<agent-id>/.claude-shared/projects/**/*.jsonl` (NanoClaw cell agent sessions — Claude Code format plus queue-operation entries; persona + channel mapped from the cell's `v2.db`, trust-tier `include`/`exclude` allowlist; run on-cell or against a read-only mirror) | ✅ | ✅ built-in |

Each reader emits normalized `Signal` types: corrections, errors, workarounds, deferrals, patterns. The nightly loop doesn't know which reader produced them. See the `Reader` trait in `crates/jilog-review/src/reader.rs` to implement your own.

**Health signals** require a richer event stream than chat messages (compactions, resumes, tool calls with arguments). Readers with ✅ implement `Reader::load_events`; the others degrade gracefully — the same pipeline runs, they just never produce `Pattern` signals.

---

## Trackers — pluggable issue backends

Learnings from the nightly loop can be filed as issues in any supported tracker:

| Tracker | Notes | Status |
|---|---|---|
| `kata` | Local SQLite daemon ([kenn-io/kata](https://github.com/kenn-io/kata)) | ✅ built-in |
| `github` | `gh issue` CLI wrapper | ✅ built-in |
| `none` | Markdown digest only, no issue creation | ✅ built-in |

```toml
[tracker]
type = "github"
repo = "Joi/jilog"
labels = ["jilog-learning"]
```

Or against a kata local daemon ([kenn-io/kata](https://github.com/kenn-io/kata)):

```toml
[tracker]
type = "kata"
project = "jilog"
```

`project` is the kata project name, created via `kata init --project jilog` in any workspace directory. Set `KATA_AUTHOR=jilog-extractor` so unattended runs are filterable in the events stream.

kata's `--idempotency-key` is wired up natively: a re-run of the extractor against the same digest is a no-op even if `list_open()` misses, because kata returns `duplicate_candidates` on a repeat key. Labels emitted are `jilog` and `jilog:<kind>` (kata enforces `[a-z0-9._:-]` on label charset, so `:` replaces the slash separator used by the github backend).

On subsequent nightly runs, jilog checks which `jilog-learning` issues are still open. If a pattern re-appears for an already-open issue, it's a bump — not a new filing. If the issue is closed and the pattern hasn't recurred, it counts as resolved.

See the `Tracker` trait in `crates/jilog-review/src/tracker.rs` to implement your own.

---

## Signal anatomy

Six signal types are detected today.

| Signal        | What triggers it                                                    | Detector heuristic                                                                                       |
|---------------|---------------------------------------------------------------------|----------------------------------------------------------------------------------------------------------|
| `Correction`  | User pushes back on an assistant turn in 15–200 chars               | `assistant → user → assistant` window with a short user message in the middle                            |
| `Error`       | A tool call returned a structured failure                           | A `role: tool` message whose JSON content has `success: false`                                           |
| `Workaround`  | Assistant text admits a temporary or hacky path                     | First-match across `for now`, `temporary`, `workaround`, `hardcoded`, `TODO`, `FIXME`, `quick fix`, `hack` |
| `Deferral`    | Assistant text postpones work to a later session                    | First-match across `come back to`, `defer`, `punt on`, `leave for later`, `skipping for now`, `park for now`, `next session`, `circle back` |
| `Pattern`     | The session's event stream shows a mechanical health problem       | Four detectors over `SessionEvent` streams — see the table below                                          |
| `P0 Alert`    | The same tool failed in **3+ distinct root sessions** in the window | Aggregation pass over `Error` signals (sub-agent sessions with the all-zero prefix are excluded)         |

Each emitted signal carries the `session_id` it came from, so digest and tracker entries always link back to the conversation that produced them.

### Persona and channel dimensions (fleet sessions)

Readers for chat-bot fleets (nanoclaw) stamp each handle with a **persona** (which bot: `jibot`, `bifbot`, …) and a **channel** (which group/surface). Every signal from those sessions carries both, digest bullets are prefixed `` `persona@channel` ``, and a **Personas** section rolls up per-bot counts — including bots that produced *no* signals, so the digest can say one bot needed four corrections this week and another needed none. `review nightly --json` exposes the same rollup under a `personas` key. Coding-session output is unchanged.

Chat sessions also swap the correction heuristic: in a group chat, a short user message after an assistant turn is usually just conversation, so the chat-tuned detector additionally requires corrective language (`no, …`, `don't`, `stop`, `wrong`, `should never`, …).

### Health-pattern detectors

`Pattern` signals come from four pure-Rust detectors over the kernel-ish event stream of readers that implement `load_events` (amplifier, context-intelligence, pi, nanoclaw). Thresholds follow the amplifier context-intelligence signals-reference, tuned conservatively; the constants live in `crates/jilog-review/src/health.rs`.

| `pattern_kind`      | Fires when                                                        |
|---------------------|-------------------------------------------------------------------|
| `compaction_storm`  | ≥3 compaction events within a 10-minute window                    |
| `stuck_loop`        | Same tool called with identical arguments ≥4 times consecutively  |
| `resume_storm`      | ≥3 resumes of one session within 30 minutes                       |
| `iteration_runaway` | ≥25 tool calls with no intervening user message                   |

Each `Pattern` carries an `evidence` string (e.g. `4 compactions 09:01-09:08`) that lands verbatim in the digest's Patterns section and in filed issues.

### Cost-weighted digests

Amplifier providers stamp `cost_usd` per LLM call into the session files jilog already reads. Readers with an event stream (amplifier, context-intelligence) sum `usage.{cost_usd,input_tokens,output_tokens}` across `llm:response` events per session; money math is `rust_decimal` end to end, never floats. Two things come out of this:

- A **Spend** section in the digest — total, per-role, and per-model breakdowns. Sub-agent roles are parsed from the session-id suffix (`<uuid>_<role>`). The section renders only when at least one scanned session carried usage data; unpriced models (cost `null`) count tokens but no dollars.
- A **week-over-week cost annotation** — a signal whose issue was already open before the run is a recurrence, and its digest line gains the summed cost of the sessions it recurred in: `(recurred in sessions totaling $4.20)`. Cost as weight: the recurring problems burning the most money surface first.

Boundary: jilog reports spend it **observed** in session files. It does not fetch prices, maintain rate tables, or reconcile with provider billing.

---

## Example: a real digest

`jilog review nightly` writes `<digest_dir>/learning-digest-<YYYY-MM-DD>.md`. The format is byte-stable — downstream tooling greps these files, so detectors and the renderer preserve the exact layout shown here.

This is a real digest from the author's machine (2026-07-05, 7-day window, 348 sessions), trimmed to a few lines per section:

````markdown
---
date: 2026-07-05
signals_captured: 77
p0_count: 0
corrections: 12
errors: 56
workarounds: 6
deferrals: 1
patterns: 2
---

# Learning Digest — 2026-07-05

## P0 Alerts

_No P0 alerts._

## Corrections

- `842c45ce-77b2-4d72-b995-f2a10466eb40` — 'do granola re-auth'
- `842c45ce-77b2-4d72-b995-f2a10466eb40` — 'it looks like you skipped the dash.ito.com build'
- `842c45ce-77b2-4d72-b995-f2a10466eb40` — 'fix the launchd issue'

## Errors

- `0000000000000000-907abeae456d42fe_self` / `todo`: {"message":"Todo 0 missing required fields (content, status, activeForm)"}
- `842c45ce-77b2-4d72-b995-f2a10466eb40` / `bash`: {"message":"Command timed out after 30 seconds"}
- `ee58d934-1049-4da0-b5b3-9a00f50efcc7` / `bash`: {"message":"Command timed out after 25 seconds"}

## Workarounds

- `0000000000000000-79f0e43ee2304cdb_self` pattern=`TODO`: 'All data collected. Let me update the todo list and finalize — this is the "nothing new to process" path.'

## Deferrals

- `ae5a0552-47f6-4030-af78-09fe71542d3d` pattern=`next session`

## Patterns

- `0000000000000000-cfa24544004845c7_self` kind=`iteration_runaway`: 53 tool calls without a user message 04:32-04:38
- `ee58d934-1049-4da0-b5b3-9a00f50efcc7` kind=`iteration_runaway`: 38 tool calls without a user message 01:35-01:54

## Spend

- **Total**: $441.83598225 across 323 of 347 session(s) with usage data
- **Tokens**: 127488222 in / 1619003 out

### Spend by role

- `(root)`: $68.89777650
- `joi-gtd-morning-runner`: $5.59667100
- `self`: $364.83495800

### Spend by model

- `claude-haiku-4-5-20251001`: $0.62172475
- `claude-opus-4-8`: $439.32940550
- `claude-sonnet-5`: $1.88485200
````

The frontmatter is machine-parseable. A common downstream check is "did yesterday's digest go silent on P0?" — `yq '.p0_count' learning-digest-*.md` gives a per-day series. The Patterns lines are the health detectors at work (two sub-agent runs blew past 25 tool calls without a user turn), and Spend is what those 347 sessions actually cost, split by sub-agent role and model.

---

## Example: dry-run output

`--dry-run` skips writing the digest file and skips creating issues, but still prints the summary counts. Useful for tuning detectors or inspecting a fresh window before letting it touch the tracker (real output, same run as the digest above):

```
$ jilog review nightly --dry-run --since 7d
12 corrections, 56 errors, 6 workarounds, 1 deferrals, 2 patterns, 0 P0 alert(s), 348 session(s) scanned
Spend: $441.83598225 across 323 of 347 session(s) with usage data
```

Add `--json` for the machine-readable version — same counts plus per-tool P0 session lists, the spend breakdown, and refs for any issues created.

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

With `tracker.type = "kata"`, a run that produced the digest above shells out to the `kata` CLI once per new signal. This is the exact invocation jilog builds for the `bash` timeout error:

```console
$ kata --project jilog --json create \
    "[jilog/error] bash: Command timed out after 30 seconds" \
    --body "Detected by jilog review pipeline on 2026-05-09.

## Source
- Session: 0e91a2b4-7d3f-4e2a-9c1b-44a7f3d8a1e2
- Kind: error
- See \`~/.amplifier/health/learning-digest-2026-05-09.md\` for the full digest window this signal came from.

## Signal
Tool: bash
Message: Command timed out after 30 seconds" \
    --label jilog --label jilog:error \
    --idempotency-key "[jilog/error]-bash:-Command-timed-out-after-30-seconds" \
    --priority 1
```

Priorities are fixed per kind: errors file at priority 1, corrections and patterns at 2, workarounds and deferrals at 3. The idempotency key is the whitespace-slugged title, so re-filing the same signal is a no-op at the kata daemon even if the `list_open()` dedup pass misses.

Recurrence reopens rather than duplicates: if a **closed** kata issue carries the same title, jilog reopens it, comments `Recurred on <date> — closure may have been premature.`, and adds the `jilog:recurred` label.

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
jilog review nightly [--json]       # Nightly learning digest + issue filing
jilog query [--since N] [--json]    # Filter ledger events
jilog spool emit                    # Producer: this host's new segments -> spool
jilog spool ingest                  # Authority: spool -> deduped fleet store
jilog spool status                  # Spool counts + emit cursors per zone
```

### Fleet replication (`jilog spool`)

One logical ledger across many machines, no server. Every host emits its
own segments into a spool directory that your existing file sync
(Syncthing) already replicates; a single always-on authority ingests —
validate, dedup, commit — into a fleet store only it writes. Configure
the spool per zone; only the authority sets `fleet_store_path`:

```toml
[[zone]]
id = "public-ops"
ledger_path = "~/switchboard/ops/ledgers/public-ops"
# spool_path defaults to <ledger parent>/spool/<zone-id>
# AUTHORITY MACHINE ONLY:
fleet_store_path = "~/switchboard/ops/ledgers/public-ops-fleet"

[[zone]]
id = "public-ops-fleet"
ledger_path = "~/switchboard/ops/ledgers/public-ops-fleet"
spool = false
# index defaults to ~/.jilog/index/public-ops-fleet.sqlite (local disk,
# NEVER inside the synced tree); override with index_path = "..."
```

Add the fleet store as a read-only `[[zone]]` on every machine and
`jilog query --zone public-ops-fleet` sees the merged, deduplicated
history anywhere. The `spool = false` flag is required on that mirror
zone: without it, every host's `spool emit` would re-emit the synced
fleet mirror into an orphan spool of its own. It also moves the zone's
SQLite index to a LOCAL default (`~/.jilog/index/<zone-id>.sqlite`) —
a SQLite file must never live inside a synced tree, where a background
sync of a mid-transaction db ships corruption. Each consumer builds its
own local index: `jilog query` refreshes it lazily on mirror zones, and
on the authority `spool ingest` refreshes it right after committing.
Emit is cursor-tracked and triple-idempotent (cursor, spool
content-compare, store dedup) — losing state is slow, never wrong.

Planned (the ledger crates already support them; CLI wiring pending): `supervise` (wrap tasks with ledger events + retry), `review sessions`, `issues list`, `issues pending`, `rebuild`, `status`.

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
| `ledger-spool` | Cross-machine transport with dedup and integrity checks (wired via `jilog spool emit`/`ingest`) |
| `jilog-review` | Nightly review engine: `Reader` + `Tracker` traits, signal detectors, digest renderer |
| `jilog` | CLI binary: review, query (supervise/issues/status planned) |

Built-in readers and trackers live as modules within `jilog-review`. Implement the `Reader` or `Tracker` trait to add your own without forking.

---

## Used by

- **opsctl** — Joi Ito's private personal AI infrastructure control plane, uses jilog as its ledger substrate and extends it with manifest validation, claims, and Joi-specific readers. Migration to jilog as an external dependency in progress.
- **deshi** — *(planned)* Executive assistant by Tatsuya Ishibe / [isbtty/deshi](https://github.com/isbtty/deshi).

---

## License

MIT
