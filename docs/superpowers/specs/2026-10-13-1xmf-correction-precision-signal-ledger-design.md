# Correction precision, one filing per signal, and raw signals filed nowhere (jilog#1xmf)

Ruling jibot-code#41e2 (Joi 2026-10-13), decision 5; parent jibot-code#8zph.

## Problem

Measured 2026-10-13 on macazbd's guarded nightly job (`com.amplifier.nightly-learning`, readers claude-code, codex, pi, worker-signals, agentsview):

1. **The correction detector has no precision in coding sessions.** `detect_corrections` (`crates/jilog-review/src/detectors.rs`) counts every user message of 15–200 characters between two assistant turns. Only chat sessions (persona present) also require corrective language (`CHAT_CORRECTION_PATTERNS`). The 2026-10-12 digest lists 254 corrections; a sample of twelve reads "update with new sources", "give me an updated PDF to send to Walter", "check in - I replied to imessage". The issue's own sample of 30 found none that is a correction, plus two email addresses and a `[structured-output-enforce]` system-prompt fragment.
2. **The same signal is counted on more than one night.** The processed-sessions file stops a clean session from being rescanned, but a session with a failed tracker create is unmarked and rescanned every night for up to 14 days (the jilog#1dvk retry sidecar). On 2026-10-12 the kata tracker refused 124 creates across 55 sessions with `kata create conflict: 1 existing issue matches this title` (kata's fuzzy title gate). Those 55 sessions are rescanned nightly and every signal in them, filed or not, is recounted in that night's digest. That is why five consecutive digests were near-identical (weekly synthesis of 2026-10-11).
3. **Raw signals are filed as issues.** 2,188 kata records in September. macazbd already has `[tracker] type = "none"`; joimba's `~/.jilog.toml` still says `kata` and would resume filing if joimba became the active Mac.
4. **errors: 0 and patterns: 0 every night.** Diagnosis below: structural, not a detector bug.

### Item 4 diagnosis

What each reader gives the two detectors, from the reader source:

| Reader | `role: "tool"` results (feeds `detect_errors`) | event stream via `load_events` (feeds `detect_health_patterns`) |
|---|---|---|
| amplifier | yes (`load_events_jsonl` maps `tool:post`) | yes |
| context-intelligence | yes (same `load_events_jsonl`) | yes |
| pi | yes (`toolResult` → tool role) | yes |
| nanoclaw | yes | yes |
| worker-signals | yes (synthesized diagnostics) | no |
| generic | only if the JSONL itself holds tool-role lines | no |
| claude-code | no — tool results are user-role `tool_result` blocks | no |
| codex | no — tool calls are dropped | no |
| copilot | no | no |
| agentsview | no — non user/assistant rows are skipped | no |

Since the 2026-08-26 de-overlap (jibot-code#41m3) the amplifier and context-intelligence readers run in the separate `com.amplifier.jilog-nightly-amplifierd` job, whose digests do report errors (16 on 2026-10-05, 2 on 2026-10-11). The guarded job keeps pi (rare on macazbd) and worker-signals (fires only on dispatch diagnostics), so its `errors: 0` and `patterns: 0` are what its reader set can produce. A Claude Code error detector is not added here: Claude Code marks every non-zero Bash exit `is_error: true`, so an unfiltered detector would replace a zero with a flood. It is filed as a follow-up.

## Success criteria

1. A committed synthetic fixture modelled on the issue's 30-sample categories (requests, approvals, questions, two email addresses, two system-prompt fragments) plus named genuine corrections asserts the exact set returned: every negative excluded (precision; ≤ 2 of the 30 sample-like lines may match, and the test pins which) and every named positive kept (recall). Both system-prompt fragments are excluded by the marker rule, asserted separately.
2. The 2026-10-12 corpus (254 lines; private message text, so measured locally and not committed) is re-run through the final pattern set, and the count and the kept lines' categories are recorded in the run manifest and the kata close-out. This is a report, not a gate.
3. Two runs over the same window, the second on the next date (as the nightly job does with an overlapping window) and with the session forced to rescan, record each signal once: the second digest reports `corrections: 0` (new) with `corrections_detected: N`, and the tracker is not called again for a signal already filed.
4. A signal whose tracker create failed is not recounted on the retry run but is offered to the tracker again; a creates-disabled run in between does not mark it filed.
5. `jilog config check` lists, for each of the errors and patterns detectors, the configured readers that can feed it (or "none"), treats an empty reader list as the default amplifier reader, and warns with exit 1 when the tracker is `kata` or `github`.
6. `cargo test --workspace` passes; README and `docs/architecture.html` describe the new flow; joimba's `~/.jilog.toml` says `type = "none"` with a backup.
7. jilog#5ssm and jilog#26w9 are each left with a one-line reason in the close-out.

## Approach

### 1. Detector precision (`detectors.rs`)

- One correction path for every session kind: the triple + length window + `tool_result` exclusion stay; then three drops, then the corrective-language requirement.
  - Drop a system-prompt marker: text containing `[structured-output-enforce]`, `<system-reminder>`, `<command-name>`, `<local-command-stdout>` or `<task-notification>` (harness injections that arrive as user turns).
  - Drop a question: trimmed text ending in `?`.
  - Drop a bare confirmation: trimmed, lower-cased, trailing `.`/`!` stripped, equal to one of "ok", "okay", "looks good", "yes", "yep", "do it", "go ahead", "sounds good", "thanks", "lgtm". Most are below the 15-character floor already; the list guards the floor changing.
  - Require corrective language: `CHAT_CORRECTION_PATTERNS`, renamed `CORRECTION_PATTERNS`, with the additions the issue names.
- Where the issue's literal words were tightened, measured on the 2026-10-12 corpus (literal list: 254 → 18; tightened: 254 → 7):
  - `don't` / `do not` only clause-initially — `(^|[.;:!,-]\s*|\band\s+)(just\s+|please\s+)?(don['’]?t|do not)\b` ("I just don't think" stays out; the typographic apostrophe ’ counts like '). "I don't think it completed" is a report, not a correction; "don't post the audio" and "- don't make the debrief available" stay.
  - `again` only with a failure word in the same message (`still|wrong|same|broken|fail…|not`). Bare "again" kept "check in again" and "send the auth again" (requests) and would keep "thanks again for your help".
  - `actually` only at the start and followed by a redirect (`no|not|don't|instead|should|meant|wrong|use|change`). The existing test pins "actually, that sounds great to me" as not a correction.
  - `I meant`, `not what I`, `undo`, `revert`, `you missed` as written.
- `detect_corrections_chat` stays as a public alias of `detect_corrections`; `run_review` calls one function.
- Workaround and deferral detectors skip an assistant message containing a system-prompt marker (an assistant turn quoting harness text is echoing it, not deferring).

### 2. Signal ledger (`crates/jilog-review/src/signal_ledger.rs`, wired in `digest.rs`)

Counting state and filing state are separate.

- **Identity.** `<kind>:<session_id>:<hex16>`, `hex16` = FNV-1a 64 (hand-written, stable across Rust versions, no new dependency) over the signal's identifying fields encoded unambiguously: each field as `<byte length>:<bytes>` concatenated. Fields: correction (context); error (tool_name, message); workaround (pattern, context); deferral (item); pattern (kind, plus the tool name for a stuck loop — not the description, whose counts grow while a session runs; found in code review of chunk B). The n-th repeat (n ≥ 2) of one identity within one session in one run gets `#<n>`, so two genuine repeats stay two signals. A content hash instead of a message index: `Signal` carries no index, and a hash is stable when a session grows. The identity includes the session id, as the issue specifies; it therefore does not merge one signal seen under two reader-specific session ids or two day-chunk ids (jilog#26w9, jilog#e31r stay open for those).
- **Storage.** A sidecar next to the processed file, `<processed-file>.signals`, named like the `.retry` sidecar so independent jobs never share one. One line per signal: `<first-seen date>\t<flags>\t<id>`, where flags is `c` (counted: a written digest has rendered it), `f` (filed: the tracker accepted it) or `cf` (both). The two flags are independent. Missing file = empty; malformed lines skipped with a warning. Saved atomically (temp file + rename). Entries whose first-seen date is more than 90 days before the run date are dropped on save.
- **Per run**, after detection, for each signal:
  - not counted → **new**: rendered and counted in the digest; gains `c` once that digest is on disk.
  - counted → not rendered, not counted as new.
  - creates attempted (`--create-issues`, not dry run) and not filed → offered to the tracker, whether new or counted; success adds `f`; failure leaves it unfiled, its session goes to the `.retry` sidecar exactly as today, and a later run offers it again without recounting it.
  - creates not attempted → nothing gains `f`. The existing rule keeps a pending `.retry` entry unresolved when creates were not attempted, so a creates-disabled run between retries cannot drain it.
- **Render sidecar.** `<processed-file>.render`, the same `timestamp\tsession id` format as `.retry` and loaded with the same `RetrySessions` code, holds sessions with a new signal no written digest has rendered yet. It is separate from `.retry` because the two resolve on different conditions: a `.retry` entry resolves when its creates succeed, a `.render` entry when a written digest has rendered its signals. The pending set is `.retry` ∪ `.render`: that one set feeds the discovery widening (`effective_since`), the in-window backfill filter and the same-date `retry_only_run` test, so a rerun that scans only `.render` sessions preserves the earlier digest exactly as a retry-only rerun does. `.render` entries follow the same 14-day cap and garbage collection as `.retry`; a dropped `.render` entry is logged as "unrendered signals are lost". Like `.retry`, `.render` is never written on a dry run. A `.render` entry resolves on any run that scans the session and either finds no new signal in it or writes its digest.
- **Write order and failure.** A signal gains `c` only after a digest rendering it is on disk; a session leaves the processed file's protection only through a sidecar, so a later run can always rediscover it.
  1. `.retry` (as today), then `.render` with every session that has a new signal this run, plus the unresolved older entries. This is the durable intent: whatever fails after this point, those sessions are rediscovered.
  2. Ledger save adding `f` for this run's successful creates. Filing is recorded before anything else can fail, so a later failure cannot cause a second create.
  3. Digest write, when the existing rules write one (unchanged in shape: a same-date rerun that scanned zero sessions or only pending sessions, `.retry` or `.render`, preserves the earlier digest).
  4. Only if step 3 wrote: ledger save adding `c` for every new signal; then `.render` saved again without the sessions whose new signals that digest rendered.
  5. Processed file: unmark every session still in `.render` (and its alias key, as the failed-session unmark does today, jilog#heyg), alongside the existing failed and pending unmarks.
  Failure at each step: step 1 fails → today's behaviour, the run errors before any other write. Step 2 fails → nothing is marked; the sessions are in `.render`, so the next run rescans them, renders the signals and offers them to the tracker again, which the tracker's title match absorbs (kata open/closed lookup; `none` is a no-op) — the existing at-least-once filing contract. Step 3 fails → `f` is kept, nothing gains `c`, and the sessions are in `.render`: the next run rediscovers them even outside its window, renders the signals and does not refile them. Step 4 fails → the digest exists but the signals lack `c` and the sessions are still in `.render`: the next run renders them once more. That is a double count after a failed write, never a loss.
  Tests inject a digest-write failure (digest dir path occupied by a regular file, removed before the next run) with the session outside the next run's window; cover a same-date retry-only rerun whose retried session gained a new signal, followed by a third same-date run in which that session is pending only through `.render` (the earlier digest must survive); and a `.render` entry resolving on a creates-disabled host.
- **Header.** `corrections: N` … `patterns: N` and `signals_captured: N` keep their names and now count new signals. New lines `signals_detected: N` and `corrections_detected: N` … `patterns_detected: N` count everything detected among the sessions this run scanned. They are named "detected" rather than "total in window" because the processed file skips sessions an earlier run already scanned, so a true window total would need a rescan; the README says so.
- **Aggregates.** P0 alerts are computed over every error detected this run, seen or new: P0 is a standing alarm, and dropping a seen error would hide a third failing session behind two earlier ones. The persona rollup also counts detected signals, and its line in the digest says "detected". Recurrence-cost annotations attach to new signals only, since only those are rendered.
- `DigestReport` gains `detected: Option<KindCounts>`; the CLI summary adds `(<n> detected)` and `--json` adds a `detected` object (null without a ledger).

### 3. Config (`crates/jilog/src/commands/config.rs`, README)

- `jilog config check` loads the config and prints:
  - `errors detector: fed by <reader types>` or `errors detector: no configured reader feeds it (…)`; the same for `patterns detector:`. Capability per reader type comes from the table above (generic reported as "may feed, if its files hold tool-role lines"). An empty reader list is evaluated as the default amplifier reader, matching `JilogConfig::into_readers`.
  - `warn: tracker <type> files every raw signal as an issue; the recommended fleet config is [tracker] type = "none" (the weekly synthesis files issues)` for `kata` and `github`.
  - exit 1 when a warn line printed, 0 otherwise.
- README: the recommended fleet config uses `type = "none"`, and a sentence says the weekly synthesis, not the nightly scan, files issues; the dedup and `--create-issues` sections describe the signal ledger and the new header lines; a "Why errors and patterns can read zero" paragraph carries the table; the correction detector section says corrective language is required for every session kind.
- `docs/architecture.html`: the nightly flow gains the ledger step and the tracker-none default.
- joimba: run over `ssh joimba`, first checking `hostname -s` prints `joimba`; back up `~/.jilog.toml` to `~/.jilog.toml.bak-1xmf-20261013`, set the `[tracker]` block to macazbd's (`type = "none"` with the 41e2 comment), show the block after the edit. This is the issue's own instruction (step 3) and is reversible from the backup. If joimba does not answer, the close-out says so and a follow-up is filed.

## Alternatives considered

- **Message index as identity** (the issue allows it). `Signal` has no index; adding one to every detector and reader is wider, and an index shifts when a reader changes how it filters lines. Rejected for the content hash.
- **One state (seen or not) with failed creates left out.** Recounts exactly the failing signals every night, and a creates-disabled run would mark unfiled signals settled. Rejected; this is why the ledger has two states (fresheyes pass 1).
- **Store the ledger in the event ledger (`ledger-core` zone).** That store is append-only and fleet-replicated; signal bookkeeping is per-job local state like the processed file. Rejected.
- **Treat kata's fuzzy-title conflict as success.** Ends the 55-session retry loop for kata users, but the recommended config files nowhere and the ledger already stops the recount. Follow-up.
- **Teach `detect_errors` Claude Code `is_error` tool results.** Floods; follow-up for a filtered design.
- **Keep the chat/coding split and only add the drops.** The 2026-10-12 sample is requests and approvals, which no drop removes. Rejected; the ruling is corrective language everywhere.

## Blast radius and rollback

- Every host running jilog: digest counts and contents change; old digests are untouched. Every existing header key keeps its name; `corrections:` etc. now mean new signals, and `_detected` keys are added. The weekly synthesis reads digests as text and benefits from the drop in repeats.
- The ledger file is new; deleting it restores the old counting for one night.
- After a revert, the old code ignores `.render`. A session listed there has already been unmarked from the processed file; if its transcript is older than the old code's window, its unrendered signals are not picked up. Before reverting, run the new build once with a window covering the oldest `.render` entry, or accept that loss.
- joimba config: restore from the backup.
- Rollback: revert the branch's merge commit.

## Related issues

- jilog#5ssm (same-date rerun overwrites): left open. A same-date rerun that scans zero or only retried sessions still preserves the earlier digest (unchanged), and one that scans new sessions still rewrites it from that run's lists. The approved fix, immutable per-run reports with a derived daily view, is its own design.
- jilog#26w9 (dedup suppresses the second reader): left open. The ledger identity includes the session id and neither fixes nor worsens the processed-file collision; the approved fix (reader-qualified processed and retry identity) is independent.
- jilog#e31r (day-chunked sessions): left open; chunk ids differ, so the ledger does not merge them.
- jilog#21yg (tool_result echoes as corrections): its exclusion is kept.

## Replaces (yd8p)

- Nightly raw-signal kata filing — deprecated: recommended config `type = "none"`, `config check` warns; the triage sweep retires in amplifier-bundle-joi's sibling issue.
- Learning digest and processed-sessions file — kept; the digest reports new and detected counts.
- Weekly synthesis — kept, updated in the sibling issue to file its patterns as issues.

## Open questions

None blocking. Assumes: the guarded job's processed file is the default `~/.jilog/telemetry/processed-sessions.txt`, so its ledger lands at `~/.jilog/telemetry/processed-sessions.txt.signals` with no plist change.
