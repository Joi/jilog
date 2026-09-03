# Plan: detector noise, recurrence, docket labeling (jilog#42fd, jilog#15ax)

Spec: docs/superpowers/specs/2026-09-03-42fd-detector-noise-docket-labeling-design.md
Run: docs/superpowers/runs/2026-09-03-42fd-detector-noise-docket-labeling.md

Two repos. Chunks 1–2 are in this worktree (jilog, branch
`42fd-detector-noise-docket-labeling`, not marshal-managed). Chunk 3 is in
`/Users/joi/repos/.worktrees/15ax-close-proposed-labeling` (amplifier-bundle-joi,
marshal-managed; lands via `marshal-submit -r amplifier-bundle-joi -p
amplifier-bundle-joi -i jilog#15ax`). Line numbers are as of jilog commit
beebdb8 and amplifier-bundle-joi commit a95ea99.

Verification lens for every review pass: do not weaken real error detection —
an exit code accompanied by diagnostic content, or a mode error that is not a
denial, must still be emitted.

## Chunk 1 — jilog detectors

Files: `crates/jilog-review/src/reader.rs`, `crates/jilog-review/src/health.rs`,
`crates/jilog-review/src/detectors.rs`, `crates/jilog-review/src/lib.rs`.

- [ ] 1.1 `reader.rs` after `parse_session_role` (line 86): add
      `pub const SUB_AGENT_PREFIX: &str = "0000000000000000";` and
      `pub fn is_sub_agent_session(session_id: &str) -> bool` (prefix test), with
      a doc comment naming the amplifier sub-agent id convention
      (`0000000000000000-<hex>_<role>`) and that it is the only autonomy marker
      the transcripts carry. Export both from `lib.rs` next to
      `parse_session_role`.
- [ ] 1.2 `detectors.rs` line 82: delete the private `SUB_AGENT_PREFIX`; line
      397 uses `crate::reader::is_sub_agent_session(&e.session_id)`. Behavior of
      `detect_p0_alerts` is byte-identical (same prefix rule).
- [ ] 1.3 `health.rs` line 43: `ITERATION_RUNAWAY_MIN_TOOL_CALLS` 25 → 150; update
      the module-doc table (line 18) and the constant's doc to cite jilog#42fd
      and the 2026-09-01 ruling. `detect_iteration_runaway` (line 211): first
      statement `if crate::reader::is_sub_agent_session(session_id) { return
      None; }` with a comment on why (sub-agent = parent-driven; call count is
      workload, not health).
- [ ] 1.4 `health.rs` tests: existing `iteration_runaway_*` tests build event
      vectors by count — parametrize on the constant (they already use
      `ITERATION_RUNAWAY_MIN_TOOL_CALLS` or fixed 25; switch fixed literals to
      the constant). Add `iteration_runaway_exempts_sub_agent_sessions` (id
      `0000000000000000-abc_role`, 200 calls → None) and
      `iteration_runaway_root_session_fires_at_new_threshold` (150 → Some, 149
      → None). Add `detect_health_patterns` aggregate check that a sub-agent
      session with a stuck loop still yields the `stuck_loop` signal (only
      runaway is exempt).
- [ ] 1.5 `detectors.rs` `detect_errors` (line 209): after the `success == false`
      check and `tool_name` resolution (line 231), `if is_expected_noise(&tool_name,
      &data) { tracing::debug!(...); continue; }`. Add below
      `extract_error_message` (line 249):
      - `fn is_expected_noise(tool_name: &str, data: &Value) -> bool`
      - `fn is_mode_denial(tool_name, data)`: `tool_name == "mode"` && (
        `data["output"]["status"] == "denied"` || `data["output"]["denied_mode"]`
        is present || error-code string at `data["error"]["code"]` or
        `data["code"]` ends with `_denied`).
      - `fn is_content_free_bash_failure(tool_name, data)`: `tool_name == "bash"`
        && (`is_bare_timeout(data)` || `is_bare_nonzero_exit(data)`).
      - `fn error_text(data) -> Option<Option<String>>`: `Some(Some(s))` for a
        string `error`, `error.message` string, or top-level `message` string;
        `Some(None)` when `error` is null/absent and no `message`; `None`
        (unrecognized) when `error` is any other non-null type. Helpers
        `output_str(data, "stdout"|"stderr") -> String` (absent/null → empty)
        and `blank(&str)`.
      - `fn is_bare_timeout(data)`: `error_text` is `Some(Some(t))` with `t`
        matching `BARE_TIMEOUT_RE` = `(?i)^command timed out after \d+
        seconds?\.?$` (trimmed), and stdout and stderr blank.
      - `fn is_bare_nonzero_exit(data)`: `output.returncode` is an i64 ≠ 0,
        `error_text == Some(None)`, stdout and stderr blank.
      No marker list anywhere; anything not matching one of the two shapes
      falls through to emission. `tracing` and `regex` are already
      dependencies of jilog-review.
- [ ] 1.6 `detectors.rs` tests (mod at line 414, helper `tool(name, content)` at
      434): add
      - `errors_skip_mode_denial_status` (the #02j8 shape), `_denied_mode`
        (#gahn), `_clear_denied_code` (#qryp shape: `error: {code:
        "clear_denied", message: ...}`), `_flat_code` (`{"code":"switch_denied",
        "success":false}`);
      - `errors_keep_mode_non_denial` (`error: {code: "invalid_transition"}`)
        and `errors_keep_denied_status_from_other_tool` (`tool_name: "delegate"`,
        `output.status: denied` → still emitted, filter is mode-scoped);
      - `errors_skip_bash_bare_timeout` (`{"success":false,"error":{"message":
        "Command timed out after 30 seconds"}}` and the string-error form),
        `errors_skip_bash_bare_nonzero_exit` (#dcpg: returncode 1, stdout "",
        stderr "");
      - `errors_keep_bash_banner_stdout` (#mg3p shape — stdout `=== today's
        health report ===`, still emitted), `errors_keep_bash_stderr`,
        `errors_keep_bash_marker_free_stdout` (`checksum mismatch: expected X,
        got Y`), `errors_keep_bash_structured_error` (`error: {"code":"E1"}`
        with no message), `errors_keep_bash_timeout_with_output`,
        `errors_keep_bash_other_error_text` (`error: "spawn failed"`),
        `errors_keep_bash_unknown_envelope` (`{"success":false,"weird":1}`),
        `errors_keep_bash_zero_returncode_with_success_false`;
      - `errors_keep_unknown_tool_unchanged` (existing shapes still fire).
- [ ] 1.7 Verify: `cargo test -p jilog-review` green; `cargo test --workspace`
      green; `cargo clippy -p jilog-review` no new warnings.
- [ ] 1.8 Docs: README line 207 table row (≥150, sub-agent sessions exempt);
      README lines 320–344 (the bash-timeout example is now a suppressed shape —
      re-anchor the `kata create` example on a still-emitted error such as a
      bash failure with stderr text; recurrence paragraph gains the reason
      rule); `docs/architecture.html` lines 247–276 (same example swap).

Acceptance: S1, S2, S3 of the spec; no other detector's output changes (existing
tests untouched except literal-to-constant edits).

## Chunk 2 — jilog recurrence (kata tracker)

File: `crates/jilog-review/src/trackers/kata.rs`.

- [ ] 2.1 `KataIssue` (line 215): add `#[serde(default)] closed_reason:
      Option<String>`.
- [ ] 2.2 New private `#[derive(Debug, Clone)] struct ClosedIssue { issue: IssueRef,
      closed_reason: Option<String> }`. `closed_cache` (line 75, 85, 101) holds
      `Option<Result<Vec<ClosedIssue>, String>>`. `list_closed` (line 120) returns
      `Vec<ClosedIssue>`.
- [ ] 2.3 `fetch_list` (line 135) stays the open-listing path; add
      `fetch_closed_list` (or make `parse_list_response` return the reason
      alongside): implement `parse_listed_issues(stdout, want_status) ->
      Vec<(IssueRef, Option<String>)>` and have `parse_list_response` map it to
      `Vec<IssueRef>` so the existing tests at lines 842–913 keep passing
      unchanged. `list_closed` uses the reason-bearing variant.
- [ ] 2.4 `create` dedup pass 2 (line 298–316): match on
      `existing.closed_reason.as_deref()`: `None | Some("done")` → current reopen
      path; `Some(other)` → `tracing::info!("kata: '{}' matches {} closed {} — not
      reopening (jilog#42fd)", ...)` and `return Ok(existing.issue.clone())`. Memo
      untouched in that branch. Update the module doc table row for `reopen()`
      and the `reopen` doc comment.
- [ ] 2.5 Tests: `parse_listed_issues_carries_closed_reason` (modern JSON with
      `closed_reason: "wontfix"` → Some; row without it → None);
      `closed_reason_gate_reopens_only_done` — unit-test the decision as a pure
      fn `reopen_allowed(reason: Option<&str>) -> bool` (None/done → true;
      wontfix/duplicate/superseded/audit-no-change → false) so the branch is
      covered without a live daemon. Keep the existing smoke tests.
- [ ] 2.6 Verify: `cargo test -p jilog-review`, `cargo test --workspace`.

Acceptance: S4.

## Chunk 3 — amplifier-bundle-joi triager

Worktree: `/Users/joi/repos/.worktrees/15ax-close-proposed-labeling`. Files:
`src/amplifier_bundle_joi/jilog_triage.py`, `bin/jilog-triage`,
`tests/test_jilog_triage.py`, `docs/superpowers/specs/2026-08-18-jilog-triage-sweep-design.md`
(one paragraph noting the 2026-09-01 ruling supersedes "no auto-close").

- [ ] 3.1 Constants: `CLOSE_PROPOSED_LABEL = "close-proposed"`,
      `CLOSE_PROPOSED_TTL_DAYS = 7`, `DECISION_MARKER = "[jilog-triage] decision
      needed"`, `CLOSE_MARKER = "[jilog-triage] close PROPOSED"`,
      `BOT_COMMENT_PREFIXES = ("[jilog-triage]", "Recurred on ")`,
      `CHALLENGE_LABELS = {"joi-decision", "joi-ruled"}`. Add `close-proposed` to
      `LABEL_DENY`.
- [ ] 3.2 `PROMPT_TEMPLATE`: `question` → "for decision: the one concrete question
      a human must answer, ending in ?"; add `"decision_target": "<for decision:
      who or what must answer — a named person, repo, or component; or null>"`.
      Rules line: "decision requires BOTH a question and a decision_target".
- [ ] 3.3 `parse_verdict`: `"decision_target": _clean(v.get("decision_target"), 120)`.
      Add `PLACEHOLDER_TARGETS = {"someone", "anyone", "team", "human", "user",
      "operator", "owner", "tbd", "n/a", "none", "null", "unknown", "?"}` and
      `def decision_is_concrete(v) -> bool`: question contains `?`, `len >= 20`,
      `len(q.split()) >= 4`; target `len >= 3`, `any(ch.isalnum())`,
      `t.lower().strip() not in PLACEHOLDER_TARGETS`.
- [ ] 3.4 `apply_verdict` decision branch: if concrete → comment
      `f"{DECISION_MARKER} (for {target}): {q}"` unless a `DECISION_MARKER` comment
      exists; then `joi-decision`; then triaged; return `"decision"`. Else →
      comment `"[jilog-triage] decision requested without a concrete question and
      named target — not stamped joi-decision"` (dedup on that prefix), triaged,
      return `"decision_unstamped"`. Close branch: `CLOSE_PROPOSED_LABEL` replaces
      `joi-decision`; comment text gains ` — auto-closes wontfix after
      {CLOSE_PROPOSED_TTL_DAYS} days unless challenged`.
- [ ] 3.5 `fetch_issue_full`: each comment keeps `created_at` (string, may be
      missing → None); result keeps `labels`, `owner`.
- [ ] 3.6 New `expire_close_proposals(run=docket.run_kata, fetch=fetch_issue_full,
      mut=kata_mut, now=None, days=CLOSE_PROPOSED_TTL_DAYS, budget_s=RUN_DEADLINE_S,
      max_items=MAX_EXPIRE_ITEMS (50), _clock=time.monotonic, log=print) -> dict`:
      - list `["list", "--project", PROJECT, "--status", "open", "--label",
        CLOSE_PROPOSED_LABEL, "--limit", "1000"]`; a `KataError` here → one
        `expire_errors` entry, return.
      - cap at `max_items`; before each item, `_clock() - started > budget_s` →
        count the rest as `expire_skipped_deadline`, break.
      - pure helper `evaluate_proposal(full, now, days) -> ("challenged" |
        "pending" | "due", detail)`; raises `ValueError` on missing marker or
        any missing/unparseable timestamp (proposal or later comment). Timestamp
        parse: `datetime.fromisoformat(s.replace("Z", "+00:00"))`, must be
        tz-aware.
      - `due` → `fresh = fetch(ref)`; re-evaluate on `fresh`; only if still
        `due` → `mut(["close", ref, "--reason", "wontfix", "--message", msg,
        "--project", PROJECT, "--agent"])` and count `expired`; a changed
        verdict on the refetch counts as `expire_challenged`.
      - per-item `ValueError`/`KataError`/schema-drift classes → `expire_errors`
        + `expire_item_errors.append(f"{ref}@expire: {e}")`, continue.
      Returns `{"expired", "expire_pending", "expire_challenged",
      "expire_skipped_deadline", "expire_errors", "expire_item_errors"}`.
- [ ] 3.7 `merge_tallies(sweep_tally, expire_tally) -> dict` (pure): copies both,
      `errors += expire_errors`, `item_errors += expire_item_errors`.
      `tally_line`: append `decision_unstamped=… expired=… expire_pending=…
      expire_challenged=… expire_skipped_deadline=… expire_errors=…` via
      `.get(k, 0)`. `sweep` tally gains `"decision_unstamped": 0` and
      `"started"`-independent `elapsed_s` (float) so the CLI can pass the
      remaining budget.
- [ ] 3.8 `bin/jilog-triage`: after `sweep`, `budget = max(0, jt.RUN_DEADLINE_S -
      tally["elapsed_s"])`; `expire = jt.expire_close_proposals(budget_s=budget,
      **expire_kwargs)` (dry-run injects the printing `mut`); `tally =
      jt.merge_tallies(tally, expire)` before `tally_line`. No other CLI change:
      rc and `#ops-alerts` already key off `errors`/`item_errors`.
- [ ] 3.9 Tests (`tests/test_jilog_triage.py`, hermetic fakes): update
      `verdict()` with `decision_target: None`; `fake_run` handles `list` with
      `--label`; `show` returns comments with `created_at`.
      - decision concrete → `joi-decision` added, comment has `(for <target>)`;
      - decision without `?`, short question, target `"someone"`, target `"?"`,
        or missing target → no `joi-decision`, unstamped marker comment,
        triaged, tally `decision_unstamped`;
      - close → `close-proposed` added, `joi-decision` NOT in label adds;
      - `parse_verdict` rejects `close-proposed` in model labels;
      - expiry: 8-day-old unchallenged → close argv EXACT (`["close", ref,
        "--reason", "wontfix", "--message", ..., "--project", "jilog",
        "--agent"]`); 3-day-old → pending; owner set / joi-decision label /
        later human comment → challenged; later `[jilog-triage]` or `Recurred
        on` comment → still expires; refetch shows a new owner → challenged and
        no close; no marker / bad proposal timestamp / later comment without
        timestamp → error and no close; close mutation failure → error, others
        continue; budget exhausted → `expire_skipped_deadline`; listing
        `KataError` → one error, no raise;
      - `merge_tallies` folds errors; `tally_line` renders the new keys and
        tolerates their absence.
- [ ] 3.10 Verify: `./scripts/test-gate.sh` in the worktree (the marshal's gate;
      ~2 min).
- [ ] 3.11 Spec note: append to the 2026-08-18 sweep spec's rejected-alternatives
      "Auto-close" bullet: superseded 2026-09-01 (jilog#15ax) by the 7-day
      unchallenged rule; jilog ≥0.7.1 never reopens wontfix closes.
- [ ] 3.12 `bundle.md`: `bundle.version` 0.9.9 → 0.9.10 (AGENTS.md: behavioral
      change).

Acceptance: S5, S6, gate green.

## Landing and rollout (after code review)

- [ ] L1 jilog: bump workspace `version` 0.7.0 → 0.7.1 (`Cargo.toml` line 12,
      `cargo update -p jilog --workspace` to refresh Cargo.lock), commit
      `release v0.7.1: …`; merge branch into main per repo convention (linear
      history; fast-forward), push main, tag `v0.7.1`, push tag.
- [ ] L2 Install on joimba: `cargo install --git https://github.com/Joi/jilog
      --tag v0.7.1 jilog`; `jilog --version` → 0.7.1.
- [ ] L3 Install on jibotmac (user jibot, `~/.local/bin/jilog`, has
      com.amplifier.nightly-learning): check for cargo; if absent, build here
      for the same arch and scp the binary; verify version.
- [ ] L4 macazbd: pending (off until 2026-09-06) — comment on jilog#42fd with the
      exact install command.
- [ ] L5 amplifier-bundle-joi: push branch, `marshal-submit -r amplifier-bundle-joi
      -p amplifier-bundle-joi -i jilog#15ax` after the 15ax evidence comment;
      when landed, `git pull` in `~/repos/amplifier-bundle-joi` on joimba (the
      plist execs the working tree).
- [ ] L6 Migration: relabel open jilog issues carrying `joi-decision` + a
      close-PROPOSED marker and no decision-needed marker → remove
      `joi-decision`, add `close-proposed`; record the command and count.
- [ ] L7 Re-close the recurrence-reopened set (`kata list --project jilog
      --label jilog:recurred --status open`, the wontfix-on-09-01 subset)
      `--reason wontfix` citing the ruling and the v0.7.1 commit.
- [ ] L8 Verify: `jilog review nightly --dry-run --days 3` on joimba shows no
      mode-denial / bare-bash / sub-agent-runaway signals; `bin/jilog-triage
      --dry-run` shows no joi-decision on close proposals. Name the next
      scheduled runs (22:50 and 23:40 JST on joimba) in the close-out.

## Rollback

- jilog: `cargo install --git https://github.com/Joi/jilog --tag v0.7.0 jilog`.
- amplifier-bundle-joi: `git checkout a95ea99 -- src/amplifier_bundle_joi/jilog_triage.py bin/jilog-triage` on the host, or wait for a marshal revert.
- Relabel migration: `kata label rm <ref> close-proposed; kata label add <ref> joi-decision`.
