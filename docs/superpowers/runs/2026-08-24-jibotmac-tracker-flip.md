# Run: jibotmac-tracker-flip

Instruction: Execute kata jibot-code#6rzb per TASK-jibot-code#6rzb.md in this worktree: (1) on jibotmac (ssh jibotmac), install the kata client, fix/replace the unhealthy com.jibot.kata-tunnel LaunchAgent (last exit 255), and flip ~/.jilog.toml tracker to kata project jilog — but per Joi's 2026-08-24 ruling, split the digest into a TRACKED run from a second config carrying only non-Hermes readers, and a LOCAL-only tracker=none run carrying the Hermes reader; the two launches MUST use distinct --processed-file and --digest-dir values. (2) Investigate amplifierd WhatsApp-DM session persistence (likely container/gateway-side on cell-jibot, read-only there) and add jilog reader coverage or file a scoped follow-up with findings. Start at medium rung — never light; independent-model (fresheyes) review required before finishing. Repo is NOT marshal-managed: end with the normal finishing flow on branch 6rzb-jibotmac-tracker-flip. Commit redacted copies of machine-local config/launchd artifacts into the branch so the reviewer can see them. Close the kata issue with full evidence, then /finish-worktree.

Stage: executing
Rung: heavy (start-floor heavy: hard-trigger — security-sensitive surface [Hermes user text / KATA_AUTH_TOKEN] + production config on jibotmac; BR2 REV1 NOV1 INT1 FC2 = 7; dispatcher floor was medium, evaluator raised). No early exit — hard-trigger-floored.
Lens (mandatory focus in EVERY review pass): "do not restart anything beyond the kata tunnel and the jilog launchd jobs; no reboots; no changes to gateway LaunchAgents; never put secret values or user text in kata issue bodies filed from the tracked run"; "the two launches MUST use distinct --processed-file and --digest-dir values"; "LINE/Telegram/GIDC-email user text must never land in fleet-visible kata issues"
Spec: docs/superpowers/specs/2026-08-24-jibotmac-tracker-flip-design.md
Plan: docs/superpowers/plans/2026-08-24-jibotmac-tracker-flip.md
Agency project: —
Kata: jibot-code#6rzb (pre-existing, already claimed — no new issue filed)

## Scorecards
Pass 1 [spec]: 1B/9S/1C/0R · fixed -/- · velocity = (—→11, escalation no) · judge: pre-floor
  (fresheyes/gpt. BLOCKER "tracked run needs content redaction" REBUTTED in part: Joi's ruling scopes the privacy boundary to Hermes surfaces; claude-code→kata snippets are the fleet-wide baseline since 2026-07-05; boundary is structural (no hermes reader in tracked config). Spec gained an explicit Privacy-boundary section. 8 SUBSTANTIVE adopted: wrapper preflight+timeout, processed-file seeding, transactional cutover+backups+plutil lint, 0600 token plist, REDACTED literal, disclosure-prevention rollback language, restart prohibition; 2 deferred-with-follow-up (jilog fail-loud tracker semantics; hardcoded digest path in issue bodies). COSMETIC fixed.)

Pass 2 [spec]: 1B/7S/0C/0R · fixed 10/10 prior · velocity ↓ (11→8, escalation no) · judge: pre-floor
  (fresheyes/gpt. BLOCKER = same redaction claim re-asserted; rebuttal STRENGTHENED in spec (issue scoped part 1 "cold-implementable" on existing binary; ruling ratifies structural split; fleet precedent). All 7 SUBSTANTIVE adopted: wrapper post-run stderr grep for tracker.create/list_open failures (exit 2), two-stage arming of --create-issues with live-args Stage-1 digest as payload preview, tracked-lane-first cutover ordering with both-artifact restore, timeout harness test w/ stub child+grandchild + env overrides, log-dir mkdir before bootstrap, argument-level isolation gate (not content divergence), tracked digest dir → ~/.amplifier/health making the 0.6.0 hardcoded issue-body path accurate with zero code change.)

Pass 3 [spec]: 1B/2S/3C/0R · fixed 8/8 prior · velocity ↓ (8→6, escalation no) · judge: STOP-VELOCITY
  (independent subagent, correctness/completeness lens. BLOCKER: stderr grep dead — tracing writes stdout; fixed with combined capture + gate c2. SUBSTANTIVE: deterministic create-path canary (gate c3); label-SET launchctl gate (h). 3 COSMETIC fixed. Judge: all fixed adequately, convergence signature (later passes debugging earlier passes' fixes), spec approved.)

Pass 1 [plan]: 4B/12S/0C/0R · fixed -/- · velocity = (—→16, escalation no) · judge: pre-floor
  (fresheyes/gpt. All 16 adopted via full plan rewrite: current jibotmac file bytes embedded as byte sources (kills the snapshot-ordering blocker + enables local semantic zone check via tomllib), gate greps corrected (type-level, comment-stripped, positive assertions), env-sourced token + umask 077 + atomic rename install, PID-recording timeout stub, per-test log dirs + invocation sentinel for gate e, canary dirs-before-bootstrap + RUNID-unique fixture + bootout-gated cleanup, launchctl print "arguments = {" sed blocks, wrapper-defaults-based gate b, two-artifact failure recovery in task 6, committed tracked plist now FINAL ARMED form with Stage-1 strip-at-install.)

Pass 2 [plan]: 3B/11S/1C/0R · fixed 16/16 prior · velocity ↓ (16→15, escalation no) · judge: pre-floor
  (fresheyes/gpt RETRY — attempt 1 died without a review, see Notes. Fixed: bounded preflight in own process group + hang harness test (gate e2), exact-node REDACTED grep, dead diff command removed, tmp+cmp+mv snapshot/seed, idempotent bootstrap w/ bootout prefix + never-delete-loaded-label, asserted harness gates (PASS/FAIL lines, missing pid file = FAIL, executor 300s tool timeout), gate b definition-level only, RUNID persisted to file + RUNID in fixture FILENAME (session-id-unique retries) + failure-path cleanup, gate f split into scratch-ledger dry-run (f1) + poll-to-completion job check (f2), gate g poll-to-completion, gate h three independent commands asserting the diff content, kata search-before-filing, Task 8 final commit + Task 9 close-out steps, v0.2.0 comment corrected. Rebutted: reviewer's speculative `--test` close flag — plan verifies flags via kata close --help and keeps the established --done convention.)

Pass 3 [plan]: 0B/3S/5C/0R · fixed 15/15 prior · velocity ↓ (15→8, escalation no) · judge: STOP-VELOCITY
  (independent subagent, implementability lens — empirically verified bash-3.2 set -u/"$@", pgrep ancestor exclusion, flat-fixture glob, fail-loud grep strings, zero-session digest skeleton, RUNID title truncation. 3 SUBSTANTIVE fixed: tolerant arm-step bootout, unconditional canary-dir wipe on any failure, family-scoped gate-h diff. 5 COSMETIC fixed. Judge: all extensions of known fault classes, plan approved.)

## Chunks

## Notes
- Dispatcher floored the start at medium; user instruction forbids downgrade to light.
- Investigation deltas vs the brief: kata client already installed on jibotmac (v0.15.1, 2026-08-22, hygiene rollout) with working env in ~/.zshrc.local; tunnel currently connected (255s are historical DNS flaps); hermes-export lane exits rc:1 on missing gidc-email state.db (pre-existing, out of scope, noted for close-out); amplifierd sessions persist on macazbd:~/.amplifier/projects (already covered by macazbd config readers but active-mac-gated; joimba active → unscanned) → Part 2 ends as scoped follow-up issue.
- Start-floor evaluator dispatched on the spec draft; verdict: heavy (recorded above).
- Plan pass 2 attempt 1: fresheyes died mid-run (codex cache-TTL error) AND the watchdog attached a concurrent session's log/result from shared /tmp/fresheyes-logs (a pxy2-buzz-catchup review — discarded as not ours). Retried once per protocol.
