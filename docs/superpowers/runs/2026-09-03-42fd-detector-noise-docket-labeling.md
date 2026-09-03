# Run: 42fd-detector-noise-docket-labeling
Instruction: start at medium. Task: jilog#42fd + jilog#15ax (brief in TASK-jilog#42fd.md at worktree root). (1) 42fd detectors: iteration-runaway exempts autonomous sessions (or threshold above the observed 100-call normal); mode-switch denials (clear_denied / denied_mode) are not emitted as error signals; content-free bash failures (bare timeout, bare nonzero exit with no diagnostic content) are not emitted. (2) 42fd recurrence: a recurring signal in a class ruled expected is not re-emitted, and recurrence never reopens or comment-flags an issue closed wontfix; done-closed issues may keep existing recurred behavior. (3) 15ax labeling: triager stamps joi-decision ONLY when it writes a decision-needed comment with a concrete question AND a named target; close-PROPOSED items get a close-proposed label with an N-day unchallenged auto-close (pick sensible N, document it). (4) Tests cover each new behavior; existing suite passes. Do not weaken real error detection. Repo is Rust workspace, not marshal-managed; after review, merge to main per repo convention and push, then roll out to active fleet Macs (macazbd is off until 2026-09-06 — record as pending).
Stage: plan-review
Rung: medium (start-floor medium: user-instructed floor "start at medium" — dispatched session, independent-model review required; evaluator dispatch skipped because the instruction fixed the floor)
Lens: do not weaken real error detection — an exit code accompanied by diagnostic content, or a mode error that is not a denial, must still be emitted.
Spec: docs/superpowers/specs/2026-09-03-42fd-detector-noise-docket-labeling-design.md   Plan: docs/superpowers/plans/2026-09-03-42fd-detector-noise-docket-labeling.md
Agency project: —
Kata: jilog#42fd (+ jilog#15ax, claimed 2026-09-03)

## Scorecards
Pass 1 [spec]: 3B/5S/1C/0R · fixed -/- · velocity = (—→9, escalation no) · judge: n/a-medium
  spec passes: 1 · elapsed: 9 min · fresheyes gpt, REASON=- · all 3B+5S fixed in the spec (bash rule → positive allowlist of two shapes, banner stdout still emits; close argv `--project jilog --agent`; gate = test-gate.sh; concrete-decision validator with placeholder set; expiry errors fold into `errors`; shared run budget + item cap; pre-close refetch + fail-closed timestamps; bundle.version bump), 1C fixed (README/architecture.html in scope).

## Chunks
- [ ] chunk 1 — jilog detectors (health.rs runaway threshold + sub-agent exemption; detectors.rs mode-denial and content-free bash filters)
- [ ] chunk 2 — jilog recurrence (kata tracker: closed_reason-aware dedup; never reopen non-done closes)
- [ ] chunk 3 — amplifier-bundle-joi triager (decision target gating; close-proposed label; 7-day unchallenged auto-close phase)

## Notes
- Two repos: jilog (this worktree, not marshal-managed) and amplifier-bundle-joi (marshal-managed, slug `amplifier-bundle-joi`, project `amplifier-bundle-joi`, gate `./scripts/test-gate.sh`). The 15ax code path (`src/amplifier_bundle_joi/jilog_triage.py`) lives in amplifier-bundle-joi, not jilog — the brief's "Repo: jilog" covers 42fd only. Chunk 3 is built in a kwt worktree of amplifier-bundle-joi and lands via marshal-submit -i jilog#15ax.
- Kata prior-work sweep: no `experiment`-labeled issues touch jilog detectors/triage/docket. Related: jilog#ecsb (dogfood window close-out, --create-issues on since 2026-08-18); the 2026-08-18 jilog-triage-sweep spec in amplifier-bundle-joi rejected auto-close in v1 pending an evidence verifier — Joi's 2026-09-01 ruling (15ax) supersedes that with the N-day unchallenged rule.
- Autonomy cannot be inferred from prompt count: session 80d5108a (jilog#5a9h, 44-call runaway ruled noise) had 5 user prompts. Sub-agent sessions (0000000000000000- prefix / _role suffix, parent_id set) are the only autonomy marker the transcripts carry.
