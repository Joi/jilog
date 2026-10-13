# Run: 1xmf-correction-precision-signal-ledger
Instruction: Run `kata show jilog#1xmf` and work the issue in this worktree, following the dispatch rules (/do-it, starting rung floored at medium; not repoman-managed — no .repoman-managed marker; close with evidence; decide-and-notify).
Stage: code-review:C
Rung: heavy (start-floor heavy: hard-trigger — live prod config (joimba ~/.jilog.toml) + digest header meaning read by consumers outside the repo; BR2 REV1 NOV1 INT2 FC1 = 7; Lens: none). Floored-at-heavy-by-hard-trigger: yes
Spec: docs/superpowers/specs/2026-10-13-1xmf-correction-precision-signal-ledger-design.md   Plan: docs/superpowers/plans/2026-10-13-1xmf-correction-precision-signal-ledger.md
Agency project: — (no mcp__agency__* tools in this session; executed directly, see Notes)
Kata: jilog#1xmf
Fresheyes launches: 8/8 (budget spent: every further in-run independent pass goes to roborev --agent codex, then a fresh subagent)
Last reviewed: spec c39811d02051ad0d58d33d82a02151e899bf1c26 (pass 5, final; pass 4 0db6b546, pass 3 f4e127f3, pass 2 45449226, pass 1 67023b7b)
Last reviewed: plan ebb51cb8fba51720f481aa26a0e070d8e6d92f54 (pass 3, final; pass 2 e48e2a8d, pass 1 15fff915)
Last reviewed: code:A f87e9a1341fe27201bee569d0936f01ff050e8e6 (pass 3, final; committed as 7bf3643)
Last reviewed: code:B 71686193ca868ad473f19b6161f7a1aa422fe3c6 (pass 4 delta; pass 3 whole chunk 6e0650e, pass 2 af7f4a6, pass 1 e9b740f)
Pre-handoff review: — (not repoman-managed)

## Scorecards
Pass 1 [spec]: 1B/13S/0C/0R · fixed -/- · velocity = (—→14, escalation no) · judge: pre-floor   (fresheyes gpt-5.6-sol/high, run 20261013-101634-9089bd; 12 fixed in the spec rewrite, 2 rebutted as snapshot artifacts)
Pass 2 [spec]: 1B/0S/0C/0R · fixed 12/14 prior (2 rebutted) · velocity ↓ (14→1, escalation no) · judge: CONTINUE   (fresheyes delta 67023b7..4544922, run 20261013-102437-7b8c7c; finding: counted committed before digest durable)
Pass 3 [spec]: 1B/2S/1C/0R · fixed 1/1 prior · velocity ↑ (1→4, escalation no) · judge: CONTINUE   (subagent, lens correctness/completeness, delta 4544922..f4e127f; single-state ledger loses filed-unrendered; render retry never resolves creates-off; digest-fail window loss)
Pass 4 [spec]: 0B/1S/3C/0R · fixed 3/3 prior · velocity = (4→4, escalation no) · judge: CONTINUE   (subagent, lens implementability, delta f4e127f..0db6b54; .render must join the one pending set for retry_only_run)
Pass 5 [spec]: 0B/0S/1C/0R · fixed 1/1 prior · velocity ↓ (4→1, escalation no) · judge: STOP-CLASS   (subagent, lens failure modes/rollback, delta 0db6b54..c39811d; rollback note on .render added inline)
spec: passes: 5 · elapsed: ~90 min · STOP-CLASS
Pass 1 [plan]: 0B/6S/1C/0R · fixed -/- · velocity = (—→7, escalation no) · judge: pre-floor   (fresheyes run 20261013-103755-02495f)
Pass 2 [plan]: 1B/3S/3C/0R · fixed 7/7 prior · velocity = (7→7, escalation yes) · judge: (floor) CONTINUE   (fresheyes delta 15fff91..e48e2a8, run 20261013-104607-f3a98f; non-atomic digest write, FixtureReader ignores since, fixed .tmp, joimba backup overwrite)
Pass 3 [plan]: 0B/0S/2C/0R · fixed 7/7 prior · velocity ↓ (7→2, escalation no) · judge: STOP-CLASS   (subagent, lens correctness/completeness, delta e48e2a8..ebb51cb; cp -n guard and B5 listing fixed inline)
plan: passes: 3 · elapsed: ~45 min · STOP-CLASS
Pass 1 [code:A]: 0B/4S/6C/0R · fixed -/- · velocity = (—→10, escalation no) · judge: pre-floor   (Stage 1 subagent 0B/1S/4C + Stage 2 fresheyes run 20261013-105712-570d0b 0B/3S/2C; multiline dot-all, vacuous negative tests, persona test, trim, stale docs)
Pass 2 [code:A]: 0B/1S/3C/0R · fixed 10/10 prior · velocity ↓ (10→4, escalation no) · judge: CONTINUE (floor)   (Stage 1 PASS, mutation-verified; fresheyes run 20261013-110713-caeeda: README chat wording = chunk C, DEFERRED, fixed in tree; test labels fixed)
Pass 3 [code:A]: 0B/0S/1C/0R · fixed 4/4 prior · velocity ↓ (4→1, escalation no) · judge: STOP-CLASS   (subagent, workspace tests green on snapshot; cosmetic "don't" after an unpunctuated newline NOT fixed inline — behaviour change after the last pass, so filed as a follow-up per the review-cycle rule)
code:A: passes: 3 · elapsed: ~45 min · STOP-CLASS · committed tree = snapshot f87e9a1 + run artifacts
Pass 1 [code:B]: 0B/2S/1C/0R · fixed -/- · velocity = (—→3, escalation no) · judge: pre-floor   (Stage 1 subagent: untested .render resolve guard → test added, mutation-verified; issue_index wording aligned. Stage 2 roborev job 1536 by codex: detected ≠ window total → REBUTTED (spec §2), job commented + closed)
Pass 2 [code:B]: 0B/0S/0C/0R · fixed 1/1 prior (1 rebutted) · velocity ↓ (3→0, escalation no) · judge: CONTINUE (floor)   (Stage 1 PASS, mutation-verified; roborev job 1537 by codex: Pass)
Pass 3 [code:B]: 0B/1S/1C/0R · fixed -/- · velocity ↑ (0→2, escalation yes) · judge: CONTINUE   (subagent, failure-modes lens: pattern identity hashed growing counts → fixed: kind + stuck-loop tool; load() doc corrected)
Pass 4 [code:B]: 0B/0S/1C/0R · fixed 2/2 prior · velocity ↓ (2→1, escalation no) · judge: STOP-VELOCITY   (Stage 1 PASS; roborev job 1538 by codex: Pass; cosmetic — stuck-loop tool parsed from description prose — rebutted: health.rs is the only producer, structured field is follow-up material)
code:B: passes: 4 · elapsed: ~60 min · STOP-VELOCITY · committed tree = 7168619 + run artifacts

## Chunks
- [x] A — detector precision (item 1) — 3 code passes, STOP-CLASS; committed (item 4's diagnosis is documented in chunk C)
- [x] B — signal ledger (item 2) — 4 code passes (Stage 2 via roborev codex: budget spent), STOP-VELOCITY; committed
- [ ] C — `jilog config check`, README + architecture doc (item 3, docs for 1/2/4) — code, CLI test, README, architecture.html written; joimba step pending; awaiting chunk review

## Notes
- Agency MCP tools are not loaded in this session (no `mcp__agency__*` in the tool list). Execution is direct, chunk by chunk, with the plan's verification commands as the per-task check.
- Launch 1 was an accident: running `fresheyes.sh --help` to read its usage started a detached review with scope "--help" (handle 20261013-101029-a1c63e). Killed by process group within a minute; counted against the budget anyway.
- Spec pass 1, attempt 1 (launch 2): background task killed externally, exit 144, no watchdog status line (REASON unrecorded — treated as EXTERNAL). Retried once (launch 3), detached with nohup and a status file in the scratchpad.
- Prototype on the 2026-10-12 corpus: the issue's literal marker list takes 254 → 18; with "don't"/"do not" restricted to clause-initial use and "again" tied to a failure word, 254 → 7 (5 of 7 read as corrections or constraints). Folded into the plan (A1).
- The plan's chunk B/C rewrite (after spec pass 1) was applied with a python block replace rather than the Edit tool; checked afterwards with grep that no stale `in_window` text remained.
- A9 corpus measurement with the COMMITTED detector (`corpus_measure_from_env`, ignored test): 2026-10-12 corpus 254 → 7. Kept: 4 constraint corrections ("don't make the debrief available", "Don't post the audio… just link", "do not trust that reading", "don't write out the konomi"), 1 report of a repeated failure ("did it again… still saying not logged in"), 2 false positives (a question whose "?" is mid-line, "shouldn't there be more…?"; a request matching "not what" in "if not what is missing").
- C4 done 2026-10-13: joimba `hostname -s` = joimba; backup ~/.jilog.toml.bak-1xmf-20261013 (did not exist before; `cp -p`); `[tracker]` changed kata → none with the 41e2 comment; diff touches only that block. Not changed: azbd2's ~/.jilog.toml still says kata, but azbd2 has no nightly jilog LaunchAgent, so nothing files from it; noted for the close-out.
- Follow-ups to file at close-out: (1) clause-initial "don't" misses a line-initial "don't" after an unpunctuated line, and `^no[,.! ]` fires on "no problem, …" — detector recall/precision polish (chunk A pass 3, Stage 1 pass 1); (2) kata tracker treats kata's fuzzy-title create conflict as a failure → 14-day retry loop; (3) errors detector for Claude Code is_error tool_result blocks, filtered.
- roborev `review <sha>` installed its own pre-push hook in the shared ~/repos/jilog/.git/hooks/pre-push (roborev v0.71.0, flushes post-commit batches); left in place.
- Measurement corpus: macazbd ~/.amplifier/health/learning-digest-2026-10-12.md, Corrections section (254 lines), copied to the session scratchpad. It holds private message text, so it is NOT committed; the committed fixture is synthetic, modelled on the categories the issue lists.
