# Detector noise, recurrence, and docket labeling — design (jilog#42fd, jilog#15ax)

Date: 2026-09-03. Kata: jilog#42fd (detectors + recurrence), jilog#15ax (triage labeling).
Ruling: Joi, 2026-09-01 docket triage batch (https://notes.ito.com/d8788c0265bc8334fecda443/).

## Problem

The nightly `jilog review nightly --create-issues` run and the nightly
`jilog-triage` sweep together produce a docket Joi cannot use:

1. **Detector noise.** `iteration_runaway` fires at 25 tool calls with no
   intervening user message. Ten closed issues (jilog#aptc 100 calls, #z1dj 77,
   #n0r6 44, #4k0q 32, #ane1 27, #rz1z 25) are agents doing their job. The
   `mode` tool's denial results (`status: denied`, `denied_mode`, `clear_denied`)
   are user/guard refusals, not failures, yet they carry `success: false` and
   file as priority-1 errors (#02j8, #gahn, #qryp, #m7ys). Bash results with no
   diagnostic content — a bare timeout (#6s9q) or a nonzero exit with blank
   stderr and a non-diagnostic stdout (#mg3p, #dcpg) — file the same way.
2. **Recurrence fights the ruling.** Error-signal titles carry the tool name
   and message but no session id, so the same denial text in any later session
   matches a closed issue by title. `KataTracker::create` then reopens it,
   comments "Recurred on … closure may have been premature", and labels
   `jilog:recurred` — regardless of why it was closed. On 2026-09-02 this
   reopened eight issues Joi had closed `wontfix` the day before and pushed the
   docket from 27 to 51.
3. **joi-decision is stamped on everything.** The triager stamps `joi-decision`
   for both `decision` and `close` dispositions. 73 of 100 open joi-decision
   issues on 2026-09-01 were jilog auto-extractions; 43 of them carried an
   unexecuted close-PROPOSED comment. Real decisions are buried.

## Success criteria

- S1. `detect_iteration_runaway` does not fire for a sub-agent session
  (`0000000000000000-` prefix) at any count, and does not fire below 150 tool
  calls for any session. It still fires at 150+ for a root session.
- S2. `detect_errors` emits nothing for a `mode` tool result whose output has
  `status: "denied"` or a `denied_mode` field, or whose error code ends in
  `_denied`. It still emits for a `mode` error that is not a denial.
- S3. `detect_errors` emits nothing for a `bash` result that exactly matches one
  of two positively identified noise shapes: a bare timeout (the timeout
  sentence with blank or absent stdout and stderr) or a bare nonzero exit
  (integer `returncode` ≠ 0, blank stdout, blank stderr, no error text). Any
  other failed bash result — non-blank stderr, non-blank stdout of any kind,
  any error text other than the timeout sentence, a structured error object, or
  an unfamiliar envelope — is still emitted. Unknown shapes default to emission.
- S4. `KataTracker::create` reopens a closed title match only when the close
  reason is `done` (or absent, pre-reason daemons). A match closed `wontfix`,
  `duplicate`, `superseded`, or `audit-no-change` is returned as-is: no reopen,
  no comment, no `jilog:recurred` label.
- S5. The triager stamps `joi-decision` only alongside a
  `[jilog-triage] decision needed` comment that carries a question (contains
  `?`) and a named target. A `decision` verdict missing either is recorded with
  a marker comment and the `jilog:triaged` stamp, and no `joi-decision`.
- S6. A `close` verdict labels `close-proposed` (never `joi-decision`). The
  sweep's expiry phase closes `close-proposed` issues `wontfix` once the
  proposal is 7+ days old and unchallenged; challenged or younger proposals are
  left alone and counted.
- S7. Unit tests cover S1–S6; `cargo test --workspace` passes in jilog and
  `./scripts/test-gate.sh` (the marshal's hermetic gate) passes in
  amplifier-bundle-joi. README and `docs/architecture.html` describe the new
  threshold, the suppressed bash shapes, and the reason-aware reopen.
- S8. The jilog change is tagged and installed on the active Mac (joimba) and
  jibotmac; the triager change is landed by the marshal and pulled on joimba.
  macazbd's rollout is recorded as pending on jilog#42fd (powered off until
  2026-09-06).

## Approach

### A. jilog detectors (`crates/jilog-review`)

**Runaway** (`health.rs`): `ITERATION_RUNAWAY_MIN_TOOL_CALLS` rises 25 → 150,
above the 100-call figure Joi ruled normal. `detect_iteration_runaway` returns
`None` when `crate::reader::is_sub_agent_session(session_id)` is true. The
sub-agent prefix constant moves from `detectors.rs` to `reader.rs` as
`SUB_AGENT_PREFIX` with the helper; the P0 detector keeps using the same prefix
rule (behavior unchanged). Rationale: a sub-agent is driven by its parent, so
"tool calls between user messages" measures workload, not health; prompt count
is not an autonomy marker (session 80d5108a had five prompts and a ruled-noise
44-call stretch).

**Expected-noise filter** (`detectors.rs`): `detect_errors` calls
`is_expected_noise(tool_name, &data)` after parsing `success: false` and skips
the signal when true. The predicate is the union of:

- `is_mode_denial`: `tool_name == "mode"` and (`output.status == "denied"`, or
  `output.denied_mode` present, or an error code string — at `error.code` or
  top-level `code` — ending in `_denied`).
- `is_content_free_bash_failure`: `tool_name == "bash"` and the result matches
  one of exactly two positively identified shapes. Let `stdout`/`stderr` be
  `output.stdout`/`output.stderr` read as strings (absent or null = empty) and
  `blank(s)` = `s.trim().is_empty()`. Let `error_text` be `error` when it is a
  string, else `error.message` when `error` is an object with a string
  `message`, else top-level `message` when it is a string, else `None`; an
  `error` value of any other non-null type (array, number, object without a
  string `message`) makes the shape unrecognized.
  - **Bare timeout:** `error_text` matches
    `^Command timed out after \d+ seconds?\.?$` (trimmed, case-insensitive),
    and `blank(stdout)` and `blank(stderr)`.
  - **Bare nonzero exit:** `output.returncode` is an integer ≠ 0, `error` is
    null or absent, `error_text` is `None`, and `blank(stdout)` and
    `blank(stderr)`.
  Anything else is emitted: non-blank stderr, non-blank stdout (a banner, a
  checksum message, structured output — no marker list is consulted), any
  error text other than the timeout sentence, a structured error object, a
  timeout that also produced output, or an envelope with none of these fields.
  The rule is an allowlist of noise shapes, never a denylist of diagnostics.

Suppressed signals are not emitted anywhere (no digest line, no issue); a
`tracing::debug` line records the drop. Real error detection is unchanged for
every other tool and every other result. Accepted residual: a nonzero exit
whose stdout is only a heading (jilog#mg3p, `=== today's health report ===`)
still emits, because deciding that a line is "not diagnostic" would require
exactly the marker heuristics the lens forbids; that class is left to triage.

### B. jilog recurrence (`trackers/kata.rs`)

`list_closed` returns `Vec<ClosedIssue { issue: IssueRef, closed_reason:
Option<String> }>` parsed from kata's `closed_reason` field (kata ≥0.15 emits
it; older JSON yields `None`). In `create`, dedup pass 2 branches on the reason:

- `done` or `None` → existing behavior (reopen + comment + `jilog:recurred`).
- anything else → return the closed `IssueRef` unchanged, `tracing::info` the
  skip. The caller records it in `created_issues`, so the digest annotation
  still links the signal to the issue that holds the ruling.

Non-done reasons all mean "this title is intentionally not open work":
`wontfix` is the ruling, `duplicate`/`superseded` point elsewhere,
`audit-no-change` was reviewed. Reopening any of them contradicts a recorded
decision. The per-run memo keeps its shape (closed cache holds `ClosedIssue`).

"A recurring signal in a class ruled expected is not re-emitted" is satisfied
by A: those signals never leave the detector, so they neither file nor recur.

### C. Triager labeling (`amplifier-bundle-joi/src/amplifier_bundle_joi/jilog_triage.py`)

- Verdict schema gains `decision_target` ("the named repo, component, script,
  path, or person the question is about — never a placeholder like someone,
  team, or TBD"). `parse_verdict` cleans it (cap 120).
- `decision` disposition is **concrete** iff `decision_is_concrete(v)`:
  `question` contains `?`, is ≥ 20 characters, and has ≥ 4 whitespace-separated
  words; `decision_target` is ≥ 3 characters, contains an alphanumeric, and its
  lowercase form is not in `PLACEHOLDER_TARGETS` (`someone`, `anyone`, `team`,
  `human`, `user`, `operator`, `owner`, `tbd`, `n/a`, `none`, `null`,
  `unknown`, `?`). Concrete → comment `[jilog-triage] decision needed (for
  <target>): <question>`, `joi-decision`, `jilog:triaged`; outcome `decision`.
  Not concrete → comment `[jilog-triage] decision requested without a concrete
  question and named target — not stamped joi-decision`, `jilog:triaged`;
  outcome `decision_unstamped` (new tally key). `joi-decision` is added only
  when the concrete comment exists (written this run, or already present from
  a prior run). Tests include the syntactically valid but empty values `"?"`
  and `"someone"`.
- `close` disposition: label `close-proposed` (constant `CLOSE_PROPOSED_LABEL`)
  instead of `joi-decision`; the proposal comment gains the trailer
  `— auto-closes wontfix after 7 days unless challenged`. `close-proposed` and
  `joi-decision` both stay in `LABEL_DENY` (the model never assigns them).
- New phase `expire_close_proposals(run, fetch, mut, now, days=7, budget_s,
  max_items=50, log)`, run by `bin/jilog-triage` after the sweep (dry-run
  prints would-be closes). It shares the sweep's absolute run deadline: the CLI
  passes `budget_s = max(0, RUN_DEADLINE_S − elapsed)` and the phase stops at
  the budget or after `max_items`, counting the remainder as
  `expire_skipped_deadline` (they are re-evaluated the next night; nothing is
  lost by waiting). For each open `close-proposed` issue (`kata list --project
  jilog --status open --label close-proposed --limit 1000`): `fetch(ref)` and
  find the newest `[jilog-triage] close PROPOSED` comment; its `created_at`
  (RFC 3339, `Z` accepted) is the proposal time (`fetch_issue_full` now keeps
  `created_at` per comment). Challenged = issue has an owner, or carries
  `joi-decision`/`joi-ruled`, or any comment after the proposal whose body does
  not start with `[jilog-triage]` or `Recurred on`. **Fail closed:** a missing
  marker, a missing or unparseable proposal timestamp, or a later comment with
  a missing or unparseable timestamp is an `expire_errors` item and never
  closes. Age < 7 days → `expire_pending`. Otherwise **refetch immediately
  before closing** and re-run the same evaluation on the fresh snapshot (a
  claim, label, or comment that landed since the first fetch counts as a
  challenge); only then
  `kata close <ref> --reason wontfix --message "[jilog-triage] close proposal
  unchallenged for 7 days (proposed YYYY-MM-DD); closed per jilog#15ax —
  reopen with a comment if this is real work" --project jilog --agent`
  (the same explicit scope every existing mutation uses; asserted at argv
  level in tests). kata offers no revision-guarded close, so the refetch
  narrows the race to seconds — the same accepted residual `sweep` documents.
  Tally keys: `expired`, `expire_pending`, `expire_challenged`,
  `expire_skipped_deadline`, `expire_errors`, `expire_item_errors`.
  Author-based challenge detection is deliberately not used: the nightly job
  and Joi's interactive sessions share `KATA_AUTHOR` on the same host.
- CLI contract: `merge_tallies(sweep_tally, expire_tally)` (pure, tested)
  folds `expire_errors` into `errors` and `expire_item_errors` into
  `item_errors`, so the existing exit-status rule (rc 1 when `errors > 0`) and
  the `#ops-alerts` detail cover auto-close failures unchanged. A `KataError`
  from the expiry listing itself is one `expire_errors` item (the sweep already
  ran; rc 2 stays reserved for "could not run at all"). `tally_line` renders
  the new keys with `.get(k, 0)`.
- N = 7 days: one weekly docket cycle. The 2026-08-18 sweep spec rejected
  auto-close pending an evidence verifier; Joi's 2026-09-01 ruling replaces
  that with the unchallenged-window rule, and B guarantees an auto-closed
  (`wontfix`) issue is never reopened by recurrence.
- `bundle.md` `bundle.version` is bumped (AGENTS.md: every behavioral change),
  and the 2026-08-18 sweep spec's "Auto-close" rejection gets a superseded note.
- Migration (rollout, not code): open jilog issues that carry `joi-decision`
  and a close-PROPOSED marker but no decision-needed marker are relabeled
  `close-proposed` by hand, with the command recorded on jilog#15ax.

## Alternatives considered

- **Exempt sessions with ≤1 prompt as "autonomous".** Rejected: the ruled-noise
  44-call session had five prompts; the only reliable autonomy marker in the
  transcripts is the sub-agent id prefix.
- **Keep threshold 25 and exempt sub-agents only.** Rejected: #aptc (100), #z1dj
  (77), #5a9h (44) are root sessions Joi ruled normal.
- **Suppress every `success: false` from `mode`.** Rejected: a non-denial mode
  error (bad transition, internal failure) is real; the brief forbids weakening
  real detection.
- **Classify stdout with a diagnostic-marker list (`error`, `failed`, …) and
  suppress everything else.** Rejected (fresheyes pass 1, BLOCKER): a
  marker-free diagnostic such as `checksum mismatch: expected X, got Y` would
  vanish, and unknown failed envelopes would be suppressed by default. The
  lens requires unknown shapes to emit, so the rule is a positive allowlist
  of two noise shapes and #mg3p-style banner output stays emitted.
- **Skip reopen only for `wontfix`.** Rejected: `duplicate`/`superseded` closes
  name a canonical issue elsewhere; reopening them files the same work twice.
- **Filter noise classes in the tracker instead of the detector.** Rejected: a
  suppressed class must leave the digest too; the detector is the single choke
  point.
- **Auto-close with reason `done`.** Rejected: nothing verified the work; wontfix
  is the honest reason and is the one B protects from reopen.
- **Author-based challenge detection.** Rejected (see C).

## Blast radius and rollback

- jilog: `jilog-review` crate only. Public constant value change
  (`ITERATION_RUNAWAY_MIN_TOOL_CALLS`), new pub fn in `reader`, tracker
  internals. No CLI or JSON schema change. Docs: README (health table row,
  the bash-timeout example prose, the recurrence paragraph) and
  `docs/architecture.html` (the timeout example) are updated to match.
  Rollback: `cargo install --tag v0.7.0`.
- amplifier-bundle-joi: `jilog_triage.py`, `bin/jilog-triage`, tests,
  `bundle.md` (version bump), the 2026-08-18 spec note. The plist executes the
  working tree, so rollback is `git checkout` of the previous main on the
  host. The expiry phase only touches issues labeled `close-proposed`, which
  no issue carries before this change lands, and only ever closes `wontfix`
  (reversible with `kata reopen`).
- Fleet: joimba (active) and jibotmac get the new jilog binary; macazbd pending.

## Open questions

None blocking. Assumes kata ≥0.15 on the daemon (it is: `closed_reason` is
present in live listings).
