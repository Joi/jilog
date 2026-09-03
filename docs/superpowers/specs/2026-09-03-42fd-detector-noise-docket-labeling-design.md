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
- S3. `detect_errors` emits nothing for a `bash` result that is a bare timeout
  or a nonzero exit with blank stderr and non-diagnostic stdout. It still emits
  when stderr is non-blank, when stdout contains diagnostic markers, or when the
  error text is anything other than the bare timeout sentence.
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
- S7. Unit tests cover S1–S6; `cargo test --workspace` and the
  amplifier-bundle-joi `pytest -q` suite pass.
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
- `is_content_free_bash_failure`: `tool_name == "bash"` and no diagnostic
  content, where diagnostic content means: `output.stderr` non-blank; or
  `output.stdout` matching the diagnostic-marker regex (`error`, `fail(ed|ure)`,
  `exception`, `traceback`, `panic`, `fatal`, `denied`, `not found`, `no such`,
  `cannot`, `unable`, `invalid`, `refused`, `abort`, `permission`, `timed out`);
  or an error text (`error` string, `error.message`, or top-level `message`)
  that is anything other than the bare `Command timed out after N seconds`
  sentence. So a bare timeout with no output, or a nonzero `returncode` with
  blank stderr and marker-free stdout, is suppressed; everything else still
  fires.

Suppressed signals are not emitted anywhere (no digest line, no issue); a
`tracing::debug` line records the drop. Real error detection is unchanged for
every other tool and every diagnostic-bearing result.

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

- Verdict schema gains `decision_target` ("who or what must answer — a named
  person, repo, or component"). `parse_verdict` cleans it (cap 120).
- `decision` disposition: well-formed iff `question` contains `?` and
  `decision_target` is non-empty. Well-formed → comment
  `[jilog-triage] decision needed (for <target>): <question>`, `joi-decision`,
  `jilog:triaged`; outcome `decision`. Malformed → comment
  `[jilog-triage] decision requested without a concrete question and named
  target — not stamped joi-decision`, `jilog:triaged`; outcome
  `decision_unstamped` (new tally key). The label is never added without the
  well-formed comment existing (this run or a prior one).
- `close` disposition: label `close-proposed` (constant `CLOSE_PROPOSED_LABEL`)
  instead of `joi-decision`; the proposal comment gains the trailer
  `— auto-closes wontfix after 7 days unless challenged`. `close-proposed` and
  `joi-decision` both stay in `LABEL_DENY` (the model never assigns them).
- New phase `expire_close_proposals(run, fetch, mut, now, days=7, log)`, run by
  `bin/jilog-triage` after the sweep (dry-run prints would-be closes). For each
  open `close-proposed` issue: find the newest `[jilog-triage] close PROPOSED`
  comment (its `created_at` is the proposal time; `fetch_issue_full` now keeps
  `created_at` per comment). Challenged = issue has an owner, or carries
  `joi-decision`/`joi-ruled`, or any comment after the proposal whose body does
  not start with `[jilog-triage]` or `Recurred on`. Unchallenged and ≥7 days →
  `kata close <ref> --reason wontfix --message "[jilog-triage] close proposal
  unchallenged for 7 days (proposed YYYY-MM-DD); closed per jilog#15ax —
  reopen with a comment if this is real work"`. Tally keys: `expired`,
  `expire_pending`, `expire_challenged`, `expire_errors`. Author-based
  challenge detection is deliberately not used: the nightly job and Joi's
  interactive sessions share `KATA_AUTHOR` on the same host.
- N = 7 days: one weekly docket cycle. The 2026-08-18 sweep spec rejected
  auto-close pending an evidence verifier; Joi's 2026-09-01 ruling replaces
  that with the unchallenged-window rule, and B guarantees an auto-closed
  (`wontfix`) issue is never reopened by recurrence.
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
- **Treat any non-empty stdout as diagnostic.** Rejected: #mg3p's stdout is a
  report banner; the ruling names it noise. Marker matching keeps real failures
  (stderr text, error words) while dropping banners.
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
  internals. No CLI or JSON schema change. Rollback: `cargo install --tag
  v0.7.0`.
- amplifier-bundle-joi: `jilog_triage.py`, `bin/jilog-triage`, tests. The plist
  executes the working tree, so rollback is `git checkout` of the previous main
  on the host. The expiry phase only touches issues labeled `close-proposed`,
  which no issue carries before this change lands.
- Fleet: joimba (active) and jibotmac get the new jilog binary; macazbd pending.

## Open questions

None blocking. Assumes kata ≥0.15 on the daemon (it is: `closed_reason` is
present in live listings).
