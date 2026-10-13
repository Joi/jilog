# Plan: correction precision, signal ledger, tracker none (jilog#1xmf)

Spec: `docs/superpowers/specs/2026-10-13-1xmf-correction-precision-signal-ledger-design.md`.
Branch: `1xmf-jilog-correction-detector-requires-corre`. Three chunks, each committed on its own after its review.

Verification for every chunk: `cargo build --verbose` and `cargo test --verbose` from the workspace root — the two steps `.github/workflows/rust.yml` runs (no clippy step; jilog has no repoman row and no `.repoman-managed` marker).

Note: chunk A's code was written while the spec's later review passes ran, since it touches only the detectors; it is reviewed as a chunk like the others before its commit.

## Chunk A — detector precision (spec item 1)

Files: `crates/jilog-review/src/detectors.rs`, `crates/jilog-review/src/digest.rs` (call site ~line 374), `crates/jilog-review/src/lib.rs` (re-export unchanged).

- [ ] A1. Rename `CHAT_CORRECTION_PATTERNS` → `CORRECTION_PATTERNS` (detectors.rs ~89) and its compiler `chat_correction_regex` → `correction_regex`. Replace the two broad patterns `\bdon'?t\b` and `\bdo not\b` with one clause-initial pattern `(?i)(^|[.;:!,-]\s*|\band\s+)(just\s+|please\s+)?(don'?t|do not)\b`, so a first-person "I don't think" or "I just don't think" no longer matches while an imperative "don't post", "- don't make", "and don't write" do. Append:
  - `(?i)\bI meant\b`, `(?i)\bnot what I\b`, `(?i)\bundo\b`, `(?i)\brevert\b`, `(?i)\byou missed\b`
  - again, tied to a failure word: `(?i)\b(still|wrong|same|broken|fail\w*|not)\b.*\bagain\b|\bagain\b.*\b(still|wrong|same|broken|fail\w*|not)\b`
  - actually, tied to a redirect: `(?i)^actually\b.*\b(no|not|don'?t|instead|should|meant|wrong|use|change)\b`
- [ ] A2. Add `const SYSTEM_MARKERS: &[&str] = &["[structured-output-enforce]", "<system-reminder>", "<command-name>", "<local-command-stdout>", "<task-notification>"];` and `fn has_system_marker(text: &str) -> bool`.
- [ ] A3. Add `const BARE_CONFIRMATIONS: &[&str] = &["ok", "okay", "looks good", "yes", "yep", "do it", "go ahead", "sounds good", "thanks", "lgtm"];` and `fn is_bare_confirmation(text: &str) -> bool` (trim, lowercase, strip trailing `.!` , compare).
- [ ] A4. `detect_corrections_impl` loses its `chat: bool` parameter. After the length checks, in order: `has_system_marker(&content_str)` → skip; `trimmed.ends_with('?')` → skip; `is_bare_confirmation(trimmed)` → skip; `!correction_regex().is_match(trimmed)` → skip. `detect_corrections` and `detect_corrections_chat` both call it (doc comment on `_chat`: kept as an alias, every session kind now uses one detector).
- [ ] A5. `detect_workarounds` and `detect_deferrals`: `if has_system_marker(&text) { continue; }` after the empty check.
- [ ] A6. digest.rs ~374: replace the persona branch with one `detect_corrections` call; drop `detect_corrections_chat` from the `use` list if unused.
- [ ] A7. Tests (detectors.rs `mod tests`):
  - Update `corrections_basic_triple`, `corrections_multiple_in_one_transcript`, `corrections_real_user_string_still_detected`, `corrections_digest_2026_06_24_fixture`, `chat_corrections_require_corrective_language` and any other test whose user text has no corrective marker: change the text to a corrective one, or flip the assertion where the test's point is now "not a correction". Keep each test's original intent (length window, tool_result exclusion, role pattern).
  - New `corrections_require_corrective_language_in_coding_sessions`: the issue's examples ("fix that cashflow thing", "the mockups look good", "anything else in this session?", "update with new sources") → 0.
  - New `corrections_sample_fixture_exact_set`: 30 synthetic sample-like lines (requests, approvals, questions, two `name@example.com` addresses, two system-prompt fragments `[structured-output-enforce] You MUST call the StructuredOutput tool to complete` and `<system-reminder>…`) plus six named genuine corrections, one per marker family (`no, …`, clause-initial `don't`, `that's not`, `revert`, `I meant`, failure-word `again`). Assert the returned contexts EQUAL the expected set: at most 2 sample-like lines (named in the test), all six positives (recall), neither system fragment (asserted separately).
  - New `corrections_drop_questions_confirmations_and_markers`, `corrections_new_markers_fire` (one case per new marker), `corrections_first_person_dont_is_not_a_correction` ("I don't think it completed"), `chat_corrections_ignore_conversational_marker_lookalikes` stays green unchanged.
  - New `workarounds_and_deferrals_skip_system_markers`.
  - Each drop condition isolated, so no test passes vacuously: direct unit tests of `has_system_marker` (every marker true, a lookalike `<system>` false) and `is_bare_confirmation` (every list entry, with case and trailing `.`/`!` variants, true; "ok then do it differently" false); and a detector-level case per drop whose text would otherwise be a correction — a marker-bearing message carrying corrective language ("[structured-output-enforce] don't stop, wrong tool"), a 15+-character question with a marker ("no, did you mean the other repo?"). The bare-confirmation drop cannot be isolated at detector level while every entry is under the 15-character floor; the helper test is its gate, and a comment says so.
- [ ] A8. Check `crates/jilog-review/tests/golden_digest.rs`, `tests/reader_pipeline.rs` and `crates/jilog/tests/golden/` for correction counts that change; update the golden files only where the change is the intended one, and say which in the commit message.
- [ ] A9. Measure on the private corpus with the committed detector: an `#[ignore]`d test `corpus_measure_from_env` in detectors.rs reads the file named by `JILOG_CORRECTION_CORPUS` (one user message per line; skipped with a message when unset), runs each through `detect_corrections` as an assistant/user/assistant triple and prints the count and the kept lines. The corpus file stays in the session scratchpad (private message text, never committed). Record "2026-10-12 corpus: 254 → N" plus the kept lines' categories (correction, constraint, report, request) in the run manifest and in the kata close-out comment.

Acceptance A: `cargo test --workspace` green; the sample fixture returns exactly its expected set (≤ 2 sample-like lines, all six positives, no system fragment).

## Chunk B — signal ledger (spec item 2)

Files: new `crates/jilog-review/src/signal_ledger.rs`; `crates/jilog-review/src/lib.rs` (`pub mod signal_ledger;`); `crates/jilog-review/src/digest.rs`; `crates/jilog/src/commands/review.rs`.

- [ ] B1. `signal_ledger.rs`:
  - `fn fnv1a64(bytes: &[u8]) -> u64` (hand-written; offset 0xcbf29ce484222325, prime 0x100000001b3).
  - `pub fn signal_identity(signal: &Signal) -> String` = `<kind>:<session_id>:<016x>` of FNV-1a over the fields, each encoded `<byte len>:<bytes>` and concatenated — correction (context); error (tool_name, message); workaround (pattern, context); deferral (item); pattern (kind, description). Kind strings from `Signal::kind()`.
  - `pub fn assign_ids(signals: &[Signal]) -> Vec<String>`: identity per signal, `#<n>` appended to the n-th repeat (n ≥ 2) of one identity, in order.
  - `pub struct Flags { counted: bool, filed: bool }`; `pub struct SignalLedger { entries: HashMap<String, (NaiveDate, Flags)> }` with `empty()`, `load(path)` (missing = empty; malformed lines or unknown flag strings skipped with `tracing::warn!`), `is_counted(&str)`, `is_filed(&str)`, `mark_counted(id, date)`, `mark_filed(id, date)` (each sets its flag and keeps the earliest first-seen date), `save(path, run_date)` (drop entries with first-seen < run_date − 90 days; sorted `date\t<c|f|cf>\tid` lines; written through `reader.rs`'s existing `write_atomic` — unique `create_new` temp file, `sync_all`, rename — made `pub(crate)` for this). `render_path(processed_file)` → `<name>.render` beside it.
  - `pub fn ledger_path(processed_file: &Path) -> PathBuf` → `<file name>.signals` beside it.
  - `pub struct KindCounts { corrections, errors, workarounds, deferrals, patterns }` (+ `total()`), `Serialize`, `Default`, `Clone`.
- [ ] B2. digest.rs `run_review`:
  - `let mut ledger = processed_file.map(|p| SignalLedger::load(&ledger_path(p))).transpose()?;`
  - After the reader loop: `p0_alerts` and the persona rollup stay as they are (over everything detected). Build `detected: KindCounts` from the five list lengths. If a ledger is loaded: build `Vec<Signal>` (correction, error, workaround, deferral, pattern order) and `assign_ids`; keep `(signal, id)` pairs; `new` = not `is_counted`; rebuild the five `all_*` lists from the new signals (rendering, recurrence annotations and `DigestReport` lists use them); `to_offer` = pairs not `is_filed`. `new_sessions` = sessions of the new signals. Load `.render` with `RetrySessions::load(render_path)`; its entries join `.retry`'s in ONE pending set: `effective_since` (`min_modified` of both), `pending_ids` (the in-window backfill filter) and `retry_only_run` all read the union. The GC drop warning for a `.render` entry says "unrendered signals are lost". Both sidecars are written only when not dry run.
  - Tracker loop iterates `to_offer` when a ledger is loaded, else all signals as today. Per-signal success → `filed_ids.insert(id)`; failure → existing `failed_sessions` bookkeeping. `issue_index` entries are added only for signals that are rendered (new).
  - Recurrence annotations: computed over the new lists only.
  - Not dry run, ledger loaded — the spec's five-step write order, exactly:
    1. Move the `should_write` computation above the sidecar block. Save `.retry` exactly as today. Build `.render` entries: every session in `new_sessions` (with its `scanned_modified` time), plus each older `.render` entry that is neither resolved (resolved = scanned this run AND (no new signal in it OR `should_write`)) nor past the GC floor; save it.
    2. `mark_filed` for every id in `filed_ids`; save the ledger.
    3. `write_digest` (when `should_write`), which now writes through `write_atomic` instead of `std::fs::write`, so a failed write never leaves a truncated digest that a later same-date run would preserve.
    4. If the digest was written: `mark_counted(id, run date)` for every new id; save the ledger; save `.render` again without the sessions in `new_sessions`.
    5. Processed-file save: unmark every session still in `.render` and its alias, alongside the existing failed/pending unmarks.
  - Comments at steps 2 and 4 cite the spec's failure table.
- [ ] B3. Header: `render_digest` keeps its signature (24 call sites) and calls a new `render_digest_full(..., detected: Option<&KindCounts>)` with `None`. With `Some`, after `patterns:` it writes `signals_detected: N`, then `corrections_detected:` … `patterns_detected:`. The persona section heading line gains "(detected)" only when `detected` is `Some`. `write_digest` gains `detected: Option<&KindCounts>` (its call sites: `run_review` and the unit test in digest.rs near line 1380 — `tests/golden_digest.rs` calls `render_digest`, whose signature is unchanged, so it needs no edit).
- [ ] B4. `DigestReport.detected: Option<KindCounts>`; `digest_report_json` adds `"detected"` (object or null). Move the human summary line in `run_nightly` into `fn summary_line(report: &DigestReport) -> String`, which appends ` (<n> detected)` when `detected` is `Some`. Call-site and golden updates in `crates/jilog/src/commands/review.rs` tests: the `DigestReport` literal in `digest_report()` gains `detected: None`; `json_output_has_documented_keys` adds `"detected"` to its key set; regenerate `crates/jilog/tests/golden/review-nightly.json` with `UPDATE_GOLDEN=1` (it gains `"detected": null`) and check the diff is that one key. New tests: `json_detected_object_populated` (a report with `detected: Some(..)` serializes the five counts) and `summary_line_appends_detected`.
- [ ] B5. Tests:
  - signal_ledger.rs: identity stable; length-prefixing separates `("ab","c")` from `("a","bc")`; repeat suffixes; load/save round trip with both states; retention keeps day −90, drops day −91; malformed line skipped; `ledger_path` naming.
  - digest.rs (FixtureReader + a counting fake tracker; any test about the discovery window uses the existing `SinceAwareReader` with an explicit old `modified`, never `FixtureReader`, which ignores `since`):
    - `rescan_next_night_counts_and_files_once`: run 1 (date D, creates on) → N new, N creates; clear the processed file; run 2 (date D+1, same `since`) → 0 new, `detected` = N, 0 creates; digest D+1 header `corrections: 0` and `corrections_detected: N`.
    - `failed_create_is_retried_without_recount`: tracker fails one signal; run 2 (D+1, session rescanned via the retry sidecar) → that signal offered again (create count +1), digest D+1 `corrections: 0`; its sibling not offered again.
    - `creates_disabled_run_does_not_mark_filed`: run 1 creates off → 0 creates, ledger flags `c` only; run 2 creates on, session rescanned → 0 new, signal offered and filed.
    - `digest_write_failure_keeps_filed_and_rerenders`: run 1 with the digest dir path occupied by a regular file → run errors; remove that file; ledger flags `f` only; session in `.render`; run 2 with a good dir and a `since` AFTER the session's modified time (`SinceAwareReader`; first assert that a run without the `.render` file discovers nothing) → session rediscovered via `.render`, signals rendered, 0 creates; `.render` empty afterwards.
    - `render_entry_resolves_without_creates`: same as above with creates off in both runs → after run 2 `.render` is empty and the processed file marks the session.
    - `same_date_retry_only_run_with_new_signal_is_not_lost`: run 1 date D writes the digest and leaves one session pending (failed create); the session gains a new correction; run 2 date D (retry-only → digest preserved) → new signal not counted, session unmarked and in `.render`; run 3 date D (session pending only via `.render`) → digest D still holds run 1's signals; run 4 date D+1 → the new signal is rendered once.
    - `dry_run_does_not_write_ledger`; `no_processed_file_means_no_ledger` (no `_detected` lines; behaviour as before).
    - `render_digest_detected_header`: all five kinds; pins `signals_detected`, each `*_detected` key and the `(detected)` persona heading, and their absence when `detected` is `None`.
    - `p0_counts_seen_errors`: three sessions failing one tool across two runs still produce the P0 alert on the run that sees the third.

Acceptance B: every B5 test passes, among them `render_digest_detected_header` (all five kinds: pins `signals_detected`, each `*_detected` key and the `(detected)` persona heading, and their absence when `detected` is `None`); `cargo test --workspace` green.

## Chunk C — config check, docs, joimba config (spec item 3, docs for 1/2/4)

Files: new `crates/jilog/src/commands/config.rs`; `crates/jilog/src/main.rs`; `README.md`; `docs/architecture.html`.

- [ ] C1. `jilog config check`: `Cmd::Config(ConfigArgs)` with subcommand `Check`. `fn check_lines(cfg: &JilogConfig) -> (Vec<String>, bool)` (lines, warned) so tests need no process exit; `run` prints the lines and returns exit 1 via `std::process::exit` when warned.
  - Capability table as a `match` over every `ReaderConfig` variant (no wildcard arm, so a new variant fails to compile until classified): errors = amplifier, context-intelligence, pi, nanoclaw, worker-signals, generic ("may"); patterns = amplifier, context-intelligence, pi, nanoclaw. Empty `readers` → evaluated as `[amplifier]` and a line says so.
  - Lines: `errors detector: fed by <types>` / `errors detector: no configured reader feeds it`; same for patterns; `warn: tracker …` for kata/github; `tracker: none (raw signals are not filed)` otherwise.
  - Tests: every variant classified (one config per variant); claude-code + codex + agentsview → both "no configured reader" lines; mixed (claude-code + pi) → pi listed for both; empty readers → amplifier; kata tracker → warned; none → not warned.
  - CLI test `crates/jilog/tests/config_check.rs`: runs `env!("CARGO_BIN_EXE_jilog")` with `--config <temp file> config check` and asserts exit status 1 plus the warn line for a kata tracker, exit status 0 for `type = "none"`, so the subcommand spelling and the exit branch are both exercised.
- [ ] C2. README per the spec (§3).
- [ ] C3. `docs/architecture.html` per the spec (§3), keeping the page's structure and styling.
- [ ] C4. joimba: `ssh joimba 'hostname -s'` must print `joimba`; then back up `~/.jilog.toml` → `~/.jilog.toml.bak-1xmf-20261013` first running `[ -e ~/.jilog.toml.bak-1xmf-20261013 ] && exit 1` (BSD `cp -n` exits 0 silently when the target exists, so it cannot be the guard), then `cp ~/.jilog.toml ~/.jilog.toml.bak-1xmf-20261013` — a retry must never overwrite the original backup, replace the `[tracker]` block with `type = "none"` plus the 41e2 comment (python over ssh, editing only that block), print the block afterwards. Unreachable → record and file a follow-up. No repo file changes.

Acceptance C: config tests pass; README and architecture doc describe the ledger, header lines, tracker none and the reader table; joimba says `type = "none"` (or a follow-up is filed).

## Follow-ups to file (not on this branch)

- kata tracker treats kata's fuzzy-title `create conflict` as a failure, so the session is retried nightly for 14 days (124 failures across 55 sessions on 2026-10-12).
- errors detector for Claude Code `is_error` tool_result blocks, filtered (non-zero Bash exits would flood it).

## Close-out (kata comment before close)

Landed SHAs; `cargo test` result; the review passes by artifact; the corpus measurement (A9); one line each: jilog#5ssm left open — same-date reruns that scan new sessions still rewrite the digest from one run's lists, the approved per-run-report design is separate; jilog#26w9 left open — the ledger identity includes the session id and does not touch reader-qualified processed/retry identity; jilog#e31r left open — chunk ids differ; the joimba outcome (C4); follow-ups filed.

## Rollback

First run the new build once with a `--since` covering the oldest `.render` entry, so no session unmarked there is stranded (the old code ignores `.render`), or record that loss as accepted. Then revert the merge commit on main. Delete `<processed-file>.signals` on a host to restore the old counting for one night. joimba: `mv ~/.jilog.toml.bak-1xmf-20261013 ~/.jilog.toml`.
