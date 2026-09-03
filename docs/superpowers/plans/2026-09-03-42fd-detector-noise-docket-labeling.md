# Plan: detector noise, recurrence, docket labeling (jilog#42fd, jilog#15ax)

Spec: docs/superpowers/specs/2026-09-03-42fd-detector-noise-docket-labeling-design.md
Run: docs/superpowers/runs/2026-09-03-42fd-detector-noise-docket-labeling.md
Revision 2 (after fresheyes plan pass 1: 7 BLOCKER / 10 SUBSTANTIVE folded in).

Two repos. Chunks 1–2 are in this worktree (jilog, branch
`42fd-detector-noise-docket-labeling`, not marshal-managed). Chunk 3 is in
`/Users/joi/repos/.worktrees/15ax-close-proposed-labeling` (amplifier-bundle-joi,
marshal-managed; lands via `marshal-submit -r amplifier-bundle-joi -p
amplifier-bundle-joi -i jilog#15ax`). Line numbers are as of jilog commit
b5cfc43 and amplifier-bundle-joi commit a95ea99.

Verification lens for every review pass: do not weaken real error detection —
an exit code accompanied by diagnostic content, or a mode error that is not a
denial, must still be emitted.

## Chunk 1 — jilog detectors

Files: `crates/jilog-review/src/reader.rs`, `crates/jilog-review/src/health.rs`,
`crates/jilog-review/src/detectors.rs`, `crates/jilog-review/src/lib.rs`,
`README.md`, `docs/architecture.html`.

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
- [ ] 1.4 `health.rs` tests (lines 499–575): replace the literal 24/25/26/30 in
      the `iteration_runaway_*` tests with expressions on the constant
      (`T = ITERATION_RUNAWAY_MIN_TOOL_CALLS`, `T-1`, `T+1`, `T+5`); the
      evidence-string assertion becomes a `starts_with(format!("{} tool calls",
      T))` check. Add `iteration_runaway_exempts_sub_agent_sessions` (id
      `0000000000000000-abc_role`, `T+50` calls → None) and
      `iteration_runaway_root_session_fires_at_150` (exactly 150 → Some, 149 →
      None, asserting the constant is 150). Add
      `detect_health_patterns_sub_agent_keeps_stuck_loop` (sub-agent id with a
      4× identical-call run and `T+10` calls → `stuck_loop` present,
      `iteration_runaway` absent).
- [ ] 1.5 `detectors.rs` `detect_errors` (line 209): after the `success == false`
      check and `tool_name` resolution (line 231), `if is_expected_noise(&tool_name,
      &data) { tracing::debug!(...); continue; }`. Add below
      `extract_error_message` (line 249):
      - `fn is_expected_noise(tool_name: &str, data: &Value) -> bool` =
        `is_mode_denial(..) || is_content_free_bash_failure(..)`.
      - `fn is_mode_denial(tool_name, data)`: `tool_name == "mode"` && (
        `data["output"]["status"] == "denied"` || `data["output"]["denied_mode"]`
        is present (any type) || an error-code string at `data["error"]["code"]`
        or `data["code"]` ends with `_denied`).
      - `fn is_content_free_bash_failure(tool_name, data)`: `tool_name == "bash"`
        && (`is_bare_timeout(data)` || `is_bare_nonzero_exit(data)`).
      - `fn output_text(data, key) -> Option<String>`: `Some("")` when
        `output[key]` is absent or null; `Some(s)` for a string; **`None` for
        any other type** (object, array, number, bool) — a non-string field is
        an unrecognized shape and both bare predicates return `false` on
        `None` (fail open = emit).
      - `enum ErrText { Absent, Text(String), Unrecognized }` from
        `fn error_text(data)`: `Text` for a string `error`, for `error.message`
        string, or for top-level `message` string (checked in that order);
        `Absent` when `error` is null/absent and no string `message`;
        `Unrecognized` for any other non-null `error` (array, number, bool,
        object without a string `message`).
      - `fn is_bare_timeout(data)`: `error_text` is `Text(t)` with `t.trim()`
        matching `BARE_TIMEOUT_RE` = `(?i)^command timed out after \d+
        seconds?\.?$`, and `output_text(stdout) == Some(blank)` and
        `output_text(stderr) == Some(blank)`.
      - `fn is_bare_nonzero_exit(data)`: `output.returncode` is an i64 ≠ 0
        (`as_i64`), `error_text == Absent`, stdout and stderr both
        `Some(blank)`.
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
        stderr ""; and the absent-stdout/stderr variant);
      - `errors_keep_bash_banner_stdout` (#mg3p shape — stdout `=== today's
        health report ===`, still emitted), `errors_keep_bash_stderr`,
        `errors_keep_bash_marker_free_stdout` (`checksum mismatch: expected X,
        got Y`), `errors_keep_bash_structured_stdout` (returncode 1, stdout is
        an object `{"failed": ["a"]}`), `errors_keep_bash_array_stderr`
        (stderr is an array), `errors_keep_bash_structured_error` (`error:
        {"code":"E1"}` with no message), `errors_keep_bash_error_array`
        (`error: ["x"]`), `errors_keep_bash_timeout_with_output`,
        `errors_keep_bash_other_error_text` (`error: "spawn failed"`),
        `errors_keep_bash_unknown_envelope` (`{"success":false,"weird":1}`),
        `errors_keep_bash_zero_returncode_with_success_false`,
        `errors_keep_bash_string_returncode` (`returncode: "1"` → not an i64 →
        emitted);
      - `errors_keep_unknown_tool_unchanged` (existing shapes still fire).
- [ ] 1.7 Verify: `cargo test -p jilog-review` green; `cargo test --workspace`
      green; `cargo clippy -p jilog-review` no new warnings.
- [ ] 1.8 Docs:
      - README line 207 table row → `≥150 tool calls with no intervening user
        message (sub-agent sessions exempt)`; the `error` detector row (the
        table above line 207) gains "mode denials and content-free bash
        failures — bare timeout, bare nonzero exit with no output — are
        suppressed (jilog#42fd)".
      - README lines 320–344: the `kata create` example is re-anchored on a
        still-emitted error (bash failure with stderr text); the recurrence
        paragraph (line 342) states the reason rule (reopen only `done`/absent;
        `wontfix`/`duplicate`/`superseded`/`audit-no-change` return the closed
        ref untouched).
      - `docs/architecture.html` line 201 (error detector "how" line) gains the
        suppression sentence; lines 247–276 example swapped to match README.
        The page has no health-pattern entry and no recurrence prose
        (fresheyes code pass 1), so ADD a `Pattern` list item after `Error`
        naming the four detectors with the 150 threshold + sub-agent
        exemption, and extend the section-03 caption (line 361) with the
        reason-aware reopen rule.

Acceptance: S1, S2, S3, S7-docs of the spec; no other detector's output changes
(existing tests untouched except literal-to-constant edits).

## Chunk 2 — jilog recurrence (kata tracker)

File: `crates/jilog-review/src/trackers/kata.rs`.

- [ ] 2.1 `KataIssue` (line 215): add `#[serde(default)] closed_reason:
      Option<String>`.
- [ ] 2.2 New private `#[derive(Debug, Clone)] struct ClosedIssue { issue: IssueRef,
      closed_reason: Option<String> }`. `closed_cache` (lines 75, 85, 101) holds
      `Option<Result<Vec<ClosedIssue>, String>>`. `list_closed` (line 120) returns
      `Result<Vec<ClosedIssue>, _>`.
- [ ] 2.3 Parsing: `fn parse_listed_issues(stdout: &str, want_status: &str) ->
      Result<Vec<ClosedIssue>, JilogReviewError>` carries the loud failures
      (malformed JSON, missing `issues`, missing `status`, missing ref) exactly
      as today; `parse_list_response` becomes `parse_listed_issues(..).map(|v|
      v.into_iter().map(|c| c.issue).collect())` so the existing tests at lines
      842–913 pass unchanged. `fetch_list` stays for the open listing;
      `list_closed` calls a `fetch_closed_list` that runs the same command with
      `--status closed` and parses with `parse_listed_issues`.
- [ ] 2.4 `create` dedup pass 2 (lines 298–316): `if !reopen_allowed(existing.
      closed_reason.as_deref()) { tracing::info!(...); return Ok(existing.issue.
      clone()); }` before the reopen; the reopen branch then uses
      `existing.issue`. `pub(crate) fn reopen_allowed(reason: Option<&str>) ->
      bool` = `matches!(reason, Some("done"))` — an ABSENT reason does not
      reopen (roborev #1854/#1855/#1856/#1858: every live closed row carries
      the field, so `None` is drift and must fail safe). Memo untouched in the
      non-reopen branch. Update the module doc table row for `reopen()` and the
      `reopen` doc comment. The row type is `ListedIssue` (status-neutral;
      `closed_reason` is `None` for open rows).
- [ ] 2.5 Testability: `KataTracker` gains a `kata_bin: PathBuf` field (`"kata"`
      in production) used by `cmd()`, and `#[cfg(test)] fn
      with_seeded_listings(project, open: Vec<IssueRef>, closed:
      Vec<ListedIssue>, kata_bin: PathBuf) -> Self` pre-fills both memos so
      `create()` never shells out for listings and every other shell-out hits
      a recording stub (fresheyes code pass 1: the path must be asserted, not
      inferred from an error). Tests:
      - `parse_listed_issues_carries_closed_reason` (row with
        `closed_reason: "wontfix"` → Some; row without → None; the four loud
        failures still `Err`);
      - `reopen_allowed_only_for_done` (`Some("done")` → true; `None` and every
        other reason → false);
      - `create_returns_wontfix_match_without_reopen`: seeded open=[] and
        closed=[wontfix match for the signal's title] → `create` returns
        `Ok(that IssueRef)`, the recording stub logs NO call, and afterwards
        the open memo is still empty and the closed memo still holds the
        entry; the loop repeats for `duplicate`, `superseded`,
        `audit-no-change`, and `None` (absent reason never reopens);
      - `create_takes_reopen_path_for_done_match`: same seeding with
        `closed_reason: Some("done")` and `kata_bin` pointed at a recording
        `#!/bin/sh` stub (pid-scoped temp dir, exit 0) → `create` returns the
        reopened ref and the stub's argv log is exactly `reopen zz99`, the
        recurrence comment, `label add zz99 jilog:recurred`, with no `create`
        call; a second test with an exit-3 stub asserts the reopen failure
        surfaces as `Err` after exactly one call;
      - `create_returns_open_match_first`: seeded open=[match] → returns it
        without touching closed.
- [ ] 2.6 Verify: `cargo test -p jilog-review`, `cargo test --workspace`.

Acceptance: S4.

## Chunk 3 — amplifier-bundle-joi triager

Worktree: `/Users/joi/repos/.worktrees/15ax-close-proposed-labeling`. Files:
`src/amplifier_bundle_joi/jilog_triage.py`, `bin/jilog-triage`,
`tests/test_jilog_triage.py`, `bundle.md`,
`docs/superpowers/specs/2026-08-18-jilog-triage-sweep-design.md`.

- [ ] 3.1 Constants: `CLOSE_PROPOSED_LABEL = "close-proposed"`,
      `CLOSE_PROPOSED_TTL_DAYS = 7`, `MAX_EXPIRE_ITEMS = 50`,
      `DECISION_MARKER = "[jilog-triage] decision needed (for "` (the concrete
      form; legacy `decision needed:` comments never match it),
      `DECISION_UNSTAMPED_MARKER = "[jilog-triage] decision requested without a
      concrete question and named target"`, `CLOSE_MARKER = "[jilog-triage]
      close PROPOSED"`, `BOT_COMMENT_PREFIXES = ("[jilog-triage]", "Recurred on
      ")`, `CHALLENGE_LABELS = {"joi-decision", "joi-ruled"}`,
      `PLACEHOLDER_TARGETS` (see 3.3). Add `close-proposed` to `LABEL_DENY`.
      Module docstring: replace "The sweep never closes an issue …" with the
      7-day unchallenged rule and a pointer to this spec.
- [ ] 3.2 `PROMPT_TEMPLATE`: `question` → "for decision: the one concrete question
      a human must answer, ending in ?"; add `"decision_target": "<for decision:
      the named repo, component, script, path, or person the question is about
      — never a placeholder like someone, the team, or TBD; or null>"`. Rules
      line: "decision requires BOTH a question and a decision_target".
- [ ] 3.3 `parse_verdict`: `"decision_target": _clean(v.get("decision_target"), 120)`.
      `PLACEHOLDER_TARGETS = {"someone", "somebody", "anyone", "anybody",
      "them", "they", "he", "she", "it", "you", "me", "us", "we", "team",
      "human", "person", "user", "users", "operator", "owner", "maintainer",
      "tbd", "tba", "n/a", "na", "none", "null", "unknown", "unclear",
      "not sure", "unsure", "?"}`. `def decision_is_concrete(v) -> bool`:
      question `q`: contains `?`, `len(q) >= 20`, `len(q.split()) >= 4`;
      target `t`: normalize `n = t.lower().strip().rstrip("?.!")`, strip a
      leading article (`the `, `a `, `an `); require `"?" not in t`, `len(n) >=
      3`, `1 <= len(n.split()) <= 6`, `any(ch.isalnum() for ch in n)`, `n not
      in PLACEHOLDER_TARGETS`, and `NAME_RE.match(t)` with `NAME_RE =
      r"\A[A-Za-z0-9][A-Za-z0-9 ._/:#@+~-]{1,119}\Z"`. This is a placeholder
      filter, not a proof of naming — documented as such in the docstring.
- [ ] 3.4 `apply_verdict` decision branch: if concrete → `body =
      f"{DECISION_MARKER}{target}): {q}"`; write it unless
      `_has_marker_comment(full, DECISION_MARKER)` (only the concrete form
      dedups — a legacy `decision needed:` comment does not suppress it); then
      `joi-decision`; then triaged; return `"decision"`. Else → comment
      `DECISION_UNSTAMPED_MARKER + " — not stamped joi-decision"` (dedup on
      `DECISION_UNSTAMPED_MARKER`), triaged, return `"decision_unstamped"`.
      Close branch: `CLOSE_PROPOSED_LABEL` replaces `joi-decision`; comment text
      gains ` — auto-closes wontfix after {CLOSE_PROPOSED_TTL_DAYS} days unless
      challenged`.
- [ ] 3.5 `fetch_issue_full`: each comment keeps `created_at` (string or None);
      result keeps `labels`, `owner`, `status`.
- [ ] 3.6 Pure helpers:
      - `_parse_ts(s) -> datetime` (`fromisoformat` after `Z → +00:00`; must be
        tz-aware; raises `ValueError` otherwise or on None).
      - `evaluate_proposal(full, now, days) -> tuple[str, str]` returning
        `("withdrawn", why)` when `status != "open"` or `CLOSE_PROPOSED_LABEL`
        not in labels; `("challenged", why)` when owner set, a
        `CHALLENGE_LABELS` hit, or any comment created after the newest
        `CLOSE_MARKER` comment whose body does not start with a
        `BOT_COMMENT_PREFIXES` entry; `("pending", why)` when `now -
        proposed_at < timedelta(days=days)` (so exactly `days` old is due);
        else `("due", proposed_date)`. Newest marker = max `created_at` among
        `CLOSE_MARKER` comments. Raises `ValueError` when no marker, or any
        marker/later-comment timestamp is missing or unparseable.
      - `close_message(proposed_date, days) -> str` =
        `f"[jilog-triage] close proposal unchallenged for {days} days (proposed
        {proposed_date}); closed per jilog#15ax — reopen with a comment if this
        is real work"`.
      - `_list_rows(d) -> list[dict]`: the same schema-drift guard as
        `gather_untriaged` (non-list `issues` or a row without a string
        `short_id` → `KataError`).
- [ ] 3.7 `expire_close_proposals(run=docket.run_kata, fetch=fetch_issue_full,
      mut=kata_mut, now=None, days=CLOSE_PROPOSED_TTL_DAYS,
      budget_s=RUN_DEADLINE_S, max_items=MAX_EXPIRE_ITEMS, _clock=time.monotonic,
      log=print) -> dict`. `fetch` is called as `fetch(ref, run)` (the sweep's
      hermetic convention). `now` defaults to `datetime.now(timezone.utc)`.
      Budget rule: `over = lambda: _clock() - started >= budget_s` checked (a)
      before the listing — if over, log and return zero counts; (b) before each
      item; (c) after the first fetch, before the refetch; (d) after the
      refetch, before the close. Any `over()` hit counts the current item and
      all remaining as `expire_skipped_deadline` and stops. Items beyond
      `max_items` are counted as `expire_skipped_cap`. Listing `KataError` or
      `_list_rows` drift → one `expire_errors` entry (`"list@expire: …"`),
      return. Per item: `verdict, why = evaluate_proposal(fetch(ref, run), now,
      days)`; `withdrawn`/`challenged` → `expire_challenged` (log why);
      `pending` → `expire_pending`; `due` → budget check, `fresh = fetch(ref,
      run)`, re-evaluate; if still `due` → budget check, `mut(["close", ref,
      "--reason", "wontfix", "--message", close_message(why, days),
      "--project", PROJECT, "--agent"])`, `expired += 1`, `expired_refs.append
      (ref)`; else `expire_challenged`. Per-item `ValueError`, `KataError`,
      `AttributeError`, `TypeError`, `KeyError` → `expire_errors += 1`,
      `expire_item_errors.append(f"{ref}@expire: {e}")`, continue. Returns
      `{"expired", "expired_refs", "expire_pending", "expire_challenged",
      "expire_skipped_deadline", "expire_skipped_cap", "expire_errors",
      "expire_item_errors"}`. Logs one line per closed ref (`[jilog-triage]
      {ref}: expired → closed wontfix`) so the nightly log is the reopen list.
- [ ] 3.8 `sweep`: tally gains `"decision_unstamped": 0` and `"elapsed_s"`.
      `elapsed_s` is computed from the LAST clock value already read
      (`last = _clock()` assignments replace the bare `_clock()` calls in the
      loop; `tally["elapsed_s"] = last - started` after the loop) — no extra
      clock read, so `test_deadline_after_model_leaves_item_unstamped` (three
      iterator values) keeps passing. Test: with clock `[0, 0, 50, 5000, 5000,
      5000]` (existing deadline test) assert `elapsed_s == 5000`.
- [ ] 3.9 `merge_tallies(sweep_tally, expire_tally) -> dict` (pure): copies both,
      `errors += expire_errors`, `item_errors += expire_item_errors`.
      `tally_line`: append `decision_unstamped=… expired=… expire_pending=…
      expire_challenged=… expire_skipped_deadline=… expire_skipped_cap=…
      expire_errors=…` via `.get(k, 0)`.
- [ ] 3.10 CLI: move the body of `bin/jilog-triage:main` into
      `jilog_triage.cli_main(argv, sweep=sweep, expire=expire_close_proposals,
      notify=<callable>, today=None, out=print, err=print) -> int`; `bin/
      jilog-triage` keeps `notify()` and calls `sys.exit(jt.cli_main(sys.argv[1:],
      notify=notify))`. In `cli_main`: after `tally = sweep(**sweep_kwargs)`,
      `budget = max(0.0, RUN_DEADLINE_S - tally.get("elapsed_s", 0.0))`;
      `expire_kwargs = {"budget_s": budget}` plus, under `--dry-run`, the same
      printing `mut`; `tally = merge_tallies(tally, expire(**expire_kwargs))`;
      the rest unchanged (rc 1 when `errors`, alert detail from `item_errors`).
- [ ] 3.11 Tests (`tests/test_jilog_triage.py`, hermetic fakes): update
      `verdict()` with `decision_target: None`; `fake_run` handles `list` with
      `--label` (returns `proposals` rows) and `show` comments carrying
      `created_at`, plus `status`/`owner`/`labels` overrides per ref.
      - decision concrete (`"Should jilog's mode reader drop clear_denied
        results?"`, target `jilog/crates/jilog-review`) → `joi-decision` added,
        comment starts with `DECISION_MARKER`;
      - decision not concrete: no `?`; 3-word question; target `"someone"`,
        `"somebody"`, `"the team"`, `"who?"`, `"not sure"`, `"them"`, `"?"`,
        missing → no `joi-decision`, unstamped marker comment, triaged, tally
        `decision_unstamped`; unstamped marker dedups on retry;
      - legacy `[jilog-triage] decision needed: …` comment present + concrete
        verdict → the concrete comment IS written and `joi-decision` added;
        concrete marker already present → no second comment, label still added;
      - close → `close-proposed` added, `joi-decision` NOT in label adds,
        comment carries the trailer;
      - `parse_verdict` rejects `close-proposed` in model labels;
      - `evaluate_proposal`: exactly 7 days → due; 6d23h → pending; two
        markers, older 10d / newer 2d → pending (newest controls); withdrawn on
        status closed and on label removed; challenged on owner, on
        `joi-decision`, on a later human comment; later `[jilog-triage]` /
        `Recurred on` comments → still due; no marker → `ValueError`; marker
        without `created_at` → `ValueError`; later comment without
        `created_at` → `ValueError`;
      - `expire_close_proposals`: 8-day-old unchallenged → close argv EXACT
        (`["close", "ab12", "--reason", "wontfix", "--message",
        close_message(...), "--project", "jilog", "--agent"]`) and `expired_refs
        == ["ab12"]`; refetch shows new owner → challenged, no close; refetch
        shows label removed → challenged, no close; close mutation failure →
        error, next item still processed; 51 proposals → `expire_skipped_cap ==
        1`; budget expiring between fetch and refetch (iterator clock) → no
        close, `expire_skipped_deadline` counts it; budget 0 at entry → no
        listing call, zero counts; listing `KataError` → one error, no raise;
        listing with `issues: "nope"` → one error, no raise;
      - `merge_tallies` folds errors; `tally_line` renders the new keys and
        tolerates their absence;
      - `cli_main`: with fake sweep/expire/notify: expire receives
        `budget_s == RUN_DEADLINE_S - elapsed_s`; `--dry-run` passes a `mut`
        that only prints (assert the fake expire got a non-default `mut` and
        no notify calls); expire errors → rc 1 and the ops notify text contains
        the `@expire` detail; clean → rc 0 and one health post.
- [ ] 3.12 Verify: `./scripts/test-gate.sh` in the worktree (the marshal's gate;
      ~2 min).
- [ ] 3.13 Spec note: append to the 2026-08-18 sweep spec's rejected-alternatives
      "Auto-close" bullet: superseded 2026-09-01 (jilog#15ax) by the 7-day
      unchallenged rule; jilog ≥0.7.1 never reopens wontfix closes.
- [ ] 3.14 `bundle.md`: `bundle.version` 0.9.9 → 0.9.10 (AGENTS.md: behavioral
      change).

Acceptance: S5, S6, gate green.

## Landing and rollout (after code review)

- [ ] L1 jilog: bump workspace `version` 0.7.0 → 0.7.1 (`Cargo.toml` line 12);
      `cargo check --workspace` refreshes `Cargo.lock` (verify with `git diff
      --stat Cargo.lock`); commit `release v0.7.1: …`; merge branch into main
      per repo convention (linear history; fast-forward), push main, tag
      `v0.7.1`, push tag.
- [ ] L2a Keep the old binary for the L8 comparison: `cp ~/.cargo/bin/jilog
      ~/.cargo/bin/jilog.v070` on joimba (remove it after L8).
- [ ] L2 Install on joimba: `cargo install --git https://github.com/Joi/jilog
      --tag v0.7.1 jilog`; `jilog --version` → 0.7.1.
- [ ] L3 Install on jibotmac (user jibot, `~/.local/bin/jilog`, arm64, no cargo,
      runs com.amplifier.nightly-learning): `cargo build --release -p jilog`
      here, `scp target/release/jilog jibotmac:~/.local/bin/jilog.new && ssh
      jibotmac 'mv ~/.local/bin/jilog.new ~/.local/bin/jilog && ~/.local/bin/
      jilog --version'`.
- [ ] L4 macazbd: pending (off until 2026-09-06) — comment on jilog#42fd with the
      exact install command AND the constraint: macazbd's
      `com.amplifier.nightly-learning` (the only job there that runs `jilog
      review nightly --create-issues`) is behind `active-mac-guard.sh`, so it
      stays silent while joimba is active; v0.7.1 must be installed there
      before any cutover makes macazbd active, or its first v0.7.0 night
      would reopen the L7 re-closed set.
- [ ] L5 amplifier-bundle-joi: push branch, `marshal-submit -r amplifier-bundle-joi
      -p amplifier-bundle-joi -i jilog#15ax` after the 15ax evidence comment;
      when landed, `git pull` in `~/repos/amplifier-bundle-joi` on joimba (the
      plist execs the working tree) and `amplifier update -y` per AGENTS.md.
- [ ] L6 Migration (re-proposal, not a bare relabel — roborev #1854/#1855):
      for each open jilog issue carrying `joi-decision` + a close-PROPOSED
      marker and no decision-needed marker, in this order: (1) `kata comment
      <ref> --body "[jilog-triage] close PROPOSED: <original proposal text,
      trimmed> (re-proposed on migration 2026-09-03) — auto-closes wontfix
      after 7 days unless challenged" --project jilog`, (2) `kata label rm
      <ref> joi-decision`, (3) `kata label add <ref> close-proposed`. The fresh
      comment is the newest marker, so each migrated item gets a full 7-day
      window from today. Record the refs on jilog#15ax.
- [ ] L7 Re-close the recurrence-reopened set (`kata list --project jilog
      --label jilog:recurred --status open`, the wontfix-on-09-01 subset)
      `--reason wontfix` citing the ruling and the v0.7.1 commit.
- [ ] L8 Verify (non-vacuous): on joimba run the OLD and NEW binaries on the
      same window with a fresh processed file each —
      `~/.cargo/bin/jilog.v070 review nightly --dry-run --days 7 --json
      --processed-file "$(mktemp)"` vs the new binary — and compare
      `sessions_scanned` (> 0, equal) and `errors`/`patterns` (new ≤ old, with
      the delta explained by the suppressed classes in the digest diff). Run
      `bin/jilog-triage --dry-run` and assert no `label add … joi-decision`
      line for a close verdict. Name the next scheduled runs (22:50 and 23:40
      on joimba) in the close-out.

## Rollback

- jilog: `cargo install --git https://github.com/Joi/jilog --tag v0.7.0 jilog`.
- amplifier-bundle-joi: `git checkout a95ea99 -- src/amplifier_bundle_joi/jilog_triage.py bin/jilog-triage bundle.md` on the host, or a marshal revert.
- Relabel migration: `kata label rm <ref> close-proposed; kata label add <ref> joi-decision`.
- Auto-closed issues: every expired ref is logged (`[jilog-triage] <ref>: expired → closed wontfix`) in
  `~/.amplifier/launchd/logs/jilog-triage.stdout.log`; reopen with
  `kata reopen <ref> --project jilog` per ref (verified reversible: `kata reopen` exists and the recurrence change never touches open issues).
