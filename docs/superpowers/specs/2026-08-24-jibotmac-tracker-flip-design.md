# jibotmac jilog tracker flip + hermes privacy split — design

Date: 2026-08-24 · Kata: jibot-code#6rzb · Branch: 6rzb-jibotmac-jilog-tracker-flip

## Problem

jibotmac runs a nightly jilog review (`com.amplifier.nightly-learning`, 22:50)
with `tracker = "none"` — digests are produced but no recurrence annotations
land in kata, because at deploy time (2026-07-05) the host had no kata client.
Since jibot-code#ca49 the same nightly run also scans Hermes surface exports
(LINE / Telegram / GIDC-email user text and jibot replies) via the `hermes`
generic reader.

Joi ruled 2026-08-24: when the tracker flips to kata, hermes-reader snippets
must NOT land in fleet-visible kata issues. The flip therefore requires a
split: a TRACKED run carrying only non-Hermes readers, and a LOCAL-only
`tracker = "none"` run carrying the Hermes reader.

Fresheyes isolation requirement (2026-08-22 comment, MANDATORY): the two
launches MUST use distinct `--processed-file` and `--digest-dir` values. Both
currently default to `~/.jilog/telemetry/processed-sessions.txt` and can share
a digest dir; whichever runs first marks sessions processed before the other
sees them, and same-date digest output collides.

Part 2 of the issue: locate where the amplifierd WhatsApp-DM agent (kata
jibot-code#tgjt) persists sessions and add jilog coverage, or file a scoped
follow-up with findings.

## Ground truth (verified 2026-08-24/25 over ssh)

State has moved since the issue body (2026-07-13) and the brief (2026-08-18):

- **kata client EXISTS on jibotmac**: `~/.local/bin/kata` v0.15.1 (installed
  2026-08-22, presumably by the kata-hygiene rollout). `~/.zshrc.local`
  carries `KATA_SERVER=http://127.0.0.1:7777`, `KATA_AUTHOR=joi-jibotmac`,
  `KATA_AUTH_TOKEN=<redacted>`, `KATA_TRUST_PRIVATE_NETWORK=1`.
  `kata --project jilog list --status open` works end-to-end from jibotmac.
- **Tunnel is CONNECTED**: `com.jibot.kata-tunnel` PID holds the
  `127.0.0.1:7777` listener; the plist is byte-for-byte the fleet-standard
  pattern (same target `exedev@kata-server.exe.xyz`, same options as
  `com.joi.kata-tunnel` on the fleet Macs). The "last exit 255" is
  historical: `/tmp/kata-tunnel.err.log` shows DNS-resolution failures
  (`Could not resolve hostname kata-server.exe.xyz`) from network blips;
  KeepAlive + ThrottleInterval=10 restart it and it reconnects. No
  replacement needed — record diagnosis + health evidence.
- **jilog 0.6.0** at `~/.local/bin/jilog`; `~/.jilog.toml` has readers
  `claude-code` + generic `hermes`, tracker none, and the two spool zones
  (`public-ops` authority wiring — used by `com.jibot.jilog-spool-ingest`,
  which reads the SAME default config and must keep working).
- **Nightly plist today**: runs `jilog review nightly --digest-dir
  /Users/jibot/.jilog/digests` (default config, default processed file, no
  `--create-issues`).
- **Canonical tracked-run pattern** (macazbd `com.amplifier.nightly-learning`):
  KATA_* env baked into the plist's EnvironmentVariables (launchd sources
  nothing), PATH includes `~/.local/bin` (KataTracker shells out to `kata` on
  PATH), and `--create-issues` on the command line.
- **hermes-export lane rc:1 (pre-existing, out of scope)**: hourly
  `com.jibot.hermes-jilog-export` logs `REFUSED profile=gidc-email … no
  regular state.db` and exits 1; the other profiles export fine (default: 14
  chunks current). Noted for close-out, not fixed here.
- **Part 2 resolved by investigation**: amplifierd does NOT run on cell-jibot
  or jibotmac. It runs on macazbd (`com.amplifier.amplifierd`, port 8410,
  reached from cell-jibot via the amplifierd.ito.com tunnel — kata
  jibot-code#tgjt). Sessions persist at `macazbd:~/.amplifier/projects/…`
  (recent WA-DM sessions verified present). cell-jibot's host has only
  `~/.config/amplifierd/credentials.env`; the onecli-gateway container volume
  holds gateway data, not amplifier sessions. macazbd's `~/.jilog.toml`
  ALREADY carries `amplifier` + `context-intelligence` readers covering that
  path — but macazbd's nightly-learning is active-mac-guard wrapped, and the
  active Mac is currently joimba, so those sessions are NOT being scanned
  while macazbd is not primary. Fixing that (ungated second run on macazbd,
  guard change, or accept scan-on-active semantics) is a design decision on a
  different host → scoped follow-up issue, per the brief.

## Privacy boundary (explicit interpretation, reviewed)

The mandatory rule is: Hermes-surface text (LINE / Telegram / GIDC-email user
text and jibot replies) must never reach fleet-visible kata, and no secret
values may be put in issue bodies by us. jilog's kata tracker DOES quote short
signal snippets (correction text, tool error strings) from whatever readers
feed the tracked run — that is its designed behavior fleet-wide: joimba and
macazbd have filed exactly such issues from `claude-code`/`amplifier` readers
since 2026-07-05, and Joi's 2026-08-24 ruling adopts the second-config split
(non-Hermes readers tracked) rather than a snippet-redaction mode. jibotmac's
`claude-code` sessions are the jibot user's automation sessions — the same
exposure class as every other fleet Mac's tracked sessions. So the boundary
enforced here is: **no Hermes reader in the tracked config** (structural — the
private text never enters the tracked pipeline), not a new content-redaction
feature. A tracked-run redaction/content-free issue mode is a jilog feature
request, out of scope; see follow-ups.

Two independent reviews pressed for a content-free/redacted issue format for
the tracked run, reading the brief's gotcha line ("NEVER put secret values or
user text in kata issue bodies filed from the tracked run") as banning all
snippet text. That reading is rejected on the issue's own record, and the
rebuttal is part of this design:
- The issue body scopes part 1 as **"cold-implementable"** — install client,
  fix tunnel, point tracker env at the daemon — i.e. config work against the
  EXISTING 0.6.0 binary, whose kata tracker quotes snippets by design. A
  snippet-redaction mode would require new jilog code, a release, and a
  jibotmac binary bump; the issue explicitly did not scope that.
- The DECISION-NEEDED comment (2026-08-22) and Joi's ruling (2026-08-24)
  frame the entire privacy question as "do hermes-reader snippets land in
  fleet-visible kata?" and ratify the second-config split as the complete
  answer. No party ever proposed redacting non-Hermes snippets.
- The gotcha's own em-dash clause states its intent: "the whole point of the
  split is that hermes surfaces stay off the fleet tracker." Read in context
  it (a) restates the Hermes boundary and (b) instructs the operating session
  not to paste secrets/user text into kata bodies IT writes (close-out
  comments, follow-ups) — both honored here.
- Fleet precedent: joimba and macazbd tracked runs have filed
  snippet-carrying issues from claude-code/amplifier readers since
  2026-07-05, with Joi's knowledge, and the flip's stated purpose is to give
  jibotmac the same recurrence annotations.
If Joi wants a stricter content-free mode fleet-wide, that is the jilog
feature-request follow-up; it does not gate this cutover.

Hard prohibition carried through implementation and close-out verification:
on jibotmac, nothing is restarted, reloaded, or reconfigured beyond
`com.jibot.kata-tunnel` (if needed — currently healthy) and the jilog
launchd jobs: the two digest LaunchAgents (`com.amplifier.nightly-learning`,
new `com.jibot.jilog-nightly-tracked`) plus one verification-only kickstart
of `com.jibot.jilog-spool-ingest` (gate g — a scheduled jilog job run
off-schedule once to prove zones survived; its plist is not touched); no
reboots; no gateway/Hermes/signal-cli LaunchAgent changes; no other service
restarts of any kind.

## Success criteria

1. A tracked nightly run on jibotmac files recurrence issues into kata
   project `jilog` from non-Hermes readers only (today: `claude-code`), via a
   second config file. Verified by one manual kickstart producing a digest in
   the tracked digest dir AND a positive launchd-context kata probe: the run
   goes through a wrapper script whose preflight performs a real kata daemon
   round-trip (`kata --project jilog list`) inside the launchd environment
   and fails the job loudly if it does not succeed — so a "clean no-signal
   run" can no longer mask a missing binary, bad token, or dead tunnel.
2. The existing local run keeps scanning the `hermes` reader with
   `tracker = "none"`, unchanged output location. Verified by one manual
   kickstart.
3. Isolation: tracked run uses `--processed-file
   ~/.jilog/telemetry/processed-sessions-tracked.txt` and `--digest-dir
   ~/.amplifier/health` (fleet convention; matches the issue-body path the
   0.6.0 binary hardcodes); local run keeps (now explicit)
   `~/.jilog/telemetry/processed-sessions.txt` and `~/.jilog/digests`.
   Distinct values visible at the launch-definition level.
4. No Hermes-derived text can reach kata: the tracked config contains no
   `hermes` reader stanza; the local config's tracker stays `none`.
5. `com.jibot.jilog-spool-ingest` (same default config) still healthy after
   the `~/.jilog.toml` edit (zones untouched).
6. Tunnel health evidence recorded; no gateway LaunchAgents touched; no
   reboots.
7. Follow-ups filed: Part 2 macazbd-findings issue, plus two scoped jilog
   defect issues surfaced by review (warn-only tracker failures with
   processed-state advance; hardcoded digest path in kata issue bodies). No
   mutations on cell-jibot or macazbd.
8. Redacted copies of both configs and both plists committed to this branch
   under `docs/ops/jibotmac-tracker-split/` with a runbook, so the
   independent review sees the machine-local artifacts.
9. jibot-code#6rzb closed with evidence (commits, paths, redacted contents,
   tunnel health output, one tracked + one local run showing distinct
   processed-files/digest-dirs, review summary).

## Approach

All jibotmac changes over `ssh jibotmac`, one at a time, verify each step.

1. **Tracked config** `~/.jilog-tracked.toml` (new file): `claude-code`
   reader; `[tracker] type = "kata", project = "jilog"`; NO zones (review
   does not need them; explicit `--digest-dir` overrides the zone-derived
   default anyway; keeping zones out avoids any spool interaction).
2. **Local config** `~/.jilog.toml` (edit): remove the `claude-code` reader
   stanza (it moves to the tracked run); keep the `hermes` generic reader,
   `tracker = "none"`, and both `[[zone]]` stanzas verbatim (spool-ingest
   depends on them). Update header comment.
3. **Tracked-run wrapper script** `~/scripts/jilog-nightly-tracked.sh` (new,
   mode 0755): the LaunchAgent runs this, not jilog directly. It provides the
   fail-loud and bounded semantics the Rust pipeline lacks today
   (tracker-create failures are warn-only and the processed file advances
   regardless — see follow-ups):
   - Preflight: `kata --project jilog --json list --status open
     > /dev/null` — a real daemon round-trip in the launchd context. On
     failure: log + `exit 1` WITHOUT running jilog, so no session is marked
     processed and the next night retries. (launchd surfaces the nonzero
     exit in `launchctl list`.)
   - Bounded run: launch jilog in the background, wait with a hard cap
     (default 1800 s, `JILOG_TRACKED_TIMEOUT_SECS` override for testing;
     macOS has no GNU timeout), on overrun `kill` the jilog process group
     (wrapper starts jilog via `set -m`/new process group so descendants —
     including a blocked `kata` child — die with it), TERM then KILL, and
     exit nonzero — a stalled daemon call can never wedge the label and
     silently absorb future scheduled runs. Binary path overridable via
     `JILOG_TRACKED_JILOG_BIN` (default `/Users/jibot/.local/bin/jilog`)
     solely so the timeout path is testable with a stub.
   - Post-run fail-loud: the wrapper captures jilog's COMBINED
     stdout+stderr into one log file (`tracing_subscriber::fmt()`'s default
     writer is STDOUT — a stderr-only grep would never fire) and greps that
     log for `tracker.create failed` / `tracker.list_open failed` (the
     pipeline's warn-only tracker errors); on match it exits nonzero — the
     loss window between preflight and a mid-run daemon death becomes
     VISIBLE instead of silent. Signals from such a run are not lost
     content-wise: the digest file retains them for manual/agent re-filing;
     true retry semantics are the jilog follow-up. [FINAL CONTRACT, refined
     during code review: 2 = real tracker errors; 4 = jilog's own nonzero
     rc; 5 = only the known jilog#fx51 create-parse pattern
     (degraded-but-known); 143 = signal trap. The ops README's exit-code
     table is authoritative.]
   - All paths absolute; `mkdir -p` of digest/log dirs as belt-and-braces
     (launchd needs the log dir to exist BEFORE bootstrap — that is an
     install-step gate, see cutover).
4. **Processed-state seeding (one-time, before arming the tracked agent)**:
   copy `~/.jilog/telemetry/processed-sessions.txt` →
   `processed-sessions-tracked.txt`. Every session already digested by the
   historical local run starts marked processed for the tracked run too, so
   the fresh file cannot re-file historical signals (a long-lived transcript
   with a recent mtime would otherwise pass the `--days 1` mtime filter and
   emit old signals). First live run therefore files only post-cutover
   sessions.
5. **Tracked digest dir = `/Users/jibot/.amplifier/health`** (not a bespoke
   `digests-tracked`): this is the fleet convention for tracked digests
   (macazbd's tracked run writes there), and — decisively — the 0.6.0 kata
   tracker HARDCODES `~/.amplifier/health/learning-digest-<date>.md` into
   every issue body it files. Using the conventional dir makes filed issue
   bodies point at the REAL digest with zero code change. Distinct from the
   local run's `~/.jilog/digests` — isolation requirement satisfied. The
   hardcode itself is still a jilog defect (any host deviating gets wrong
   pointers) → follow-up remains.
6. **Local run plist** `com.amplifier.nightly-learning` (edit): add explicit
   `--processed-file /Users/jibot/.jilog/telemetry/processed-sessions.txt`
   (digest-dir already explicit). Keep 22:50 schedule.
7. **Tracked run plist** `com.jibot.jilog-nightly-tracked` (new): program =
   the wrapper script; EnvironmentVariables: HOME, PATH (with
   `/Users/jibot/.local/bin` first, for the `kata` shell-out), KATA_SERVER,
   KATA_AUTHOR=joi-jibotmac, KATA_AUTH_TOKEN (value from `~/.zshrc.local`,
   XML-escaped by writing the plist with plutil/python, verified with
   `plutil -lint`), KATA_TRUST_PRIVATE_NETWORK=1 — launchd sources nothing
   (macazbd pattern). Plist file mode 0600, owner jibot (it embeds the
   token). Schedule 23:20. RunAtLoad false, Nice 10, absolute log paths
   under `~/.jilog/logs/`. Inside the wrapper, jilog runs with `--config
   /Users/jibot/.jilog-tracked.toml review nightly --digest-dir
   /Users/jibot/.amplifier/health --processed-file
   /Users/jibot/.jilog/telemetry/processed-sessions-tracked.txt` — plus
   `--create-issues` only once armed (two-stage arming, below).
8. **Cutover order — tracked lane proven BEFORE local coverage narrows**
   (pass-2 review): while the tracked lane is being installed and verified,
   `~/.jilog.toml` still carries BOTH readers, so any tracked-lane failure
   leaves the existing full local coverage intact (the two lanes use
   distinct processed files, so the overlap is harmless duplication, not
   interference). Only after the tracked lane passes its gates is the
   `claude-code` stanza removed from the local config. On any downstream
   failure, restore BOTH snapshot artifacts (config + plist) and re-bootstrap.
   - Snapshot originals first: `~/.jilog.toml` and the local-run plist →
     `~/.jilog/backup-6rzb/` on jibotmac before any edit.
   - Validate before load: configs parse via the real loader (`jilog
     --config <file> review nightly --dry-run --json` with the EXACT live
     `--digest-dir`/`--processed-file` arguments), plists pass
     `plutil -lint`, wrapper passes `bash -n`, log/digest dirs exist
     (`mkdir -p` BEFORE bootstrap — launchd maps StandardOut/ErrorPath at
     job start and cannot create parent dirs).
   - All agent reloads via bootout + bootstrap (plist edits require it; also
     the LWCR-safe path from jilog#jnts). If a bootstrap fails, restore the
     snapshot and bootstrap it back immediately — the Hermes local review is
     never left unloaded.
9. **Two-stage arming of `--create-issues`** (pass-2 review: dry-run JSON
   cannot preview payloads): Stage 1 — the tracked agent runs live WITHOUT
   `--create-issues`; the produced digest in `~/.amplifier/health` IS the
   exact payload preview (same readers, same processed file, same window).
   Inspect it: only claude-code-derived content, sane volume, nothing
   secret-bearing. Cost of this stage: signals in that first window are
   marked processed without being filed (acceptable — they remain in the
   digest; seeding already suppresses history). Stage 2 — add
   `--create-issues` to the wrapper invocation, bootout+bootstrap, verified
   by the NEXT nightly (or a manual kickstart) filing real issues.
10. **Verify** (deterministic gates, in order):
   (a) TOML assertion: tracked config contains exactly one `[[reader]]`
       (`type = "claude-code"`) and `[tracker] type = "kata"` /
       `project = "jilog"`; string `hermes` and `generic` absent. Local
       config after the slim-down: exactly the `hermes` generic reader,
       `tracker = "none"`, both `[[zone]]` stanzas byte-identical to the
       snapshot.
   (b) Isolation assertion: the two launch definitions name distinct
       absolute `--processed-file` and `--digest-dir` paths (argument-level
       check — content divergence is NOT the gate; a clean night leaves a
       seeded copy identical, which is valid).
   (c) Wrapper timeout harness test (as jibot, from CLI): run the wrapper
       with `JILOG_TRACKED_TIMEOUT_SECS=5` and `JILOG_TRACKED_JILOG_BIN`
       pointed at a stub that spawns a child and a grandchild sleep; assert
       nonzero exit within budget and zero surviving stub processes.
   (c2) Fail-loud grep test (CLI): point `JILOG_TRACKED_JILOG_BIN` at a stub
       that prints `tracker.create failed: probe` on STDOUT and exits 0;
       assert the wrapper exits 2 — proves the combined-capture grep path is
       alive, not just the timeout and preflight paths.
   (c3) Create-path canary (deterministic end-to-end probe; Stage-2
       verification must not depend on real signals appearing): a throwaway
       config pointing the claude-code reader at a fixture transcript
       (authored by us, content-free marker text) + the real kata tracker,
       run from the launchd context (one-shot test label or `launchctl
       kickstart` of the tracked agent with the fixture config), files
       exactly one real issue into kata project `jilog`; verify it exists,
       then close it citing this run as test evidence. This exercises the
       full create invocation (`--label`/`--priority`/`--idempotency-key`)
       that preflight's `list` cannot.
   (d) Kickstart tracked agent (Stage 1): wrapper preflight proves kata
       reachability from the launchd context; digest lands in
       `~/.amplifier/health`; nonzero-exit path exercised by the preflight
       test below.
   (e) Preflight negative test (CLI): run the wrapper with `KATA_SERVER`
       pointed at a dead port — assert it exits nonzero and jilog never ran
       (processed file mtime unchanged).
   (f) Kickstart local agent: digest lands in `~/.jilog/digests`, hermes
       reader scanned.
   (g) `com.jibot.jilog-spool-ingest` kickstart → exit 0 (zones untouched).
   (h) Tunnel still connected; `launchctl print` shows both digest labels
       loaded and healthy; no label ADDED or REMOVED except
       `com.jibot.jilog-nightly-tracked` (compare the label SETS from
       before/after `launchctl list` snapshots — PID/last-exit columns churn
       on every periodic job and are NOT the gate), and `launchctl print`
       confirms no gateway label (`ai.hermes.*`, `com.cloudflare.*`,
       `com.jibot.hermes-*`) was restarted (unchanged PIDs).
11. **Repo artifacts**: commit redacted copies of the two configs, two
   plists, and wrapper script + runbook under
   `docs/ops/jibotmac-tracker-split/`. The token value is replaced with the
   literal text `REDACTED` (plain XML text node — no angle brackets, so the
   committed copy stays valid XML and cannot be mistaken for the real value).
12. **Follow-ups to file** (scoped jilog issues, found by review — not
    forced into this change):
    - jilog: tracker-create failures are warn-only and the processed file
      advances anyway (`digest.rs` warn + unconditional save) — signals from
      a failed tracked run are silently dropped; wants fail-loud/retry
      semantics in the pipeline itself (the wrapper preflight only narrows
      the window: a daemon dying mid-run still hits it).
    - jilog: `trackers/kata.rs` hardcodes `~/.amplifier/health/learning-digest-…`
      in issue bodies; any host whose digest dir deviates gets wrong
      pointers. Thread the real digest path in. (jibotmac sidesteps it by
      adopting the conventional dir — see approach step 5 — but the defect
      stands for the general case.) Include the UTC/Local date mismatch:
      the digest filename uses `Utc::now()` (review.rs) while the issue-body
      pointer uses `Local::now()` (kata.rs) — they diverge for runs between
      00:00 local until the dates realign. [Execution correction: this was
      drafted assuming jibotmac local = JST (window 00:00–08:59); ground
      truth is UTC+6, window 00:00–05:59. The binding predicate is
      `date +%F` == `date -u +%F` on the host.]
    - jibot-code: Part 2 findings — amplifierd (WA-DM agent) persists
      sessions on macazbd:`~/.amplifier/projects`; covered by macazbd's
      config readers but its nightly-learning is active-mac-guard-gated and
      joimba is active, so those sessions are currently unscanned; needs its
      own design (ungated second run vs guard change vs accept).
13. Close 6rzb with evidence; `/finish-worktree`.

## Alternatives considered

- **Single config + `--json` post-filter** stripping hermes signals from the
  tracked run: rejected in the 2026-08-22 comment and by Joi's ruling — the
  two-config split is the ratified design; a filter leaves the private text
  one bug away from a fleet-visible issue body.
- **Replacing the kata tunnel** with a direct ZeroTier client connection:
  macOS Local-Network privacy blocks the Go binary's direct outbound (proven
  fleet-wide); the loopback SSH tunnel is the proven fix and is currently
  healthy. No change.
- **Fresh install of the kata client**: unnecessary — v0.15.1 already present
  and working; reinstalling risks divergence from the hygiene rollout that
  put it there.
- **Forcing Part 2 into this change** (e.g. editing macazbd's guard or adding
  an ungated macazbd run now): rejected — different host, interacts with the
  active-mac single-writer design and tracked-digest duplication; the brief
  explicitly prefers a scoped follow-up when investigation shows it needs its
  own design.
- **Sharing one processed-file between the runs**: forbidden by the ruling
  (whichever runs first starves the other).

## Blast radius / rollback

- Blast radius: jibotmac jilog config + two jilog LaunchAgents + one wrapper
  script; kata project `jilog` receives new issues from the tracked run.
  Gateway/Hermes agents, spool zones, and cell-jibot untouched. Repo changes
  are docs-only. [Execution note: plus two comment-only lines in
  crates/jilog-review/src/digest.rs marking the warn strings as a machine
  contract for the jibotmac wrapper — no behavior change.]
- Mass-filing prevention is layered: processed-state seeding (historical
  sessions pre-marked), the `--days 1` mtime window, and the mandatory
  pre-arming `--dry-run --json` volume check. (`--days 1` alone is NOT
  sufficient — a recently-modified long transcript passes the mtime filter
  and can emit old signals.)
- **Disclosure is prevented, not rolled back.** Once an issue body reaches
  the fleet-visible kata project it is in tracker history; closing it does
  not unpublish it. Prevention layers: no Hermes reader in the tracked
  config (structural), dry-run inspection before arming, and the seeded
  processed file. Containment if something private is ever filed anyway:
  immediately close the issue, note the exposure, and escalate to Joi for a
  daemon-side purge decision — never treat closure as erasure.
- Machine-state rollback: `launchctl bootout` the tracked agent + delete its
  plist, wrapper, and `~/.jilog-tracked.toml`; restore `~/.jilog.toml` and
  the local-run plist from `~/.jilog/backup-6rzb/` snapshots
  (bootout+bootstrap the local agent); remove the tracked digests +
  `processed-sessions-tracked.txt` — exact form in the ops README's
  rollback block (a `find -newer` deploy-snapshot anchor, so only files
  this deployment produced are touched; the README is authoritative).

## Open questions

- None blocking. Tracked-run schedule chosen 23:20 to keep the two runs
  serialized in time — defense in depth on top of the distinct
  processed-file/digest-dir isolation (after the split the two runs scan
  different sources, so this is scheduling hygiene, not a correctness
  requirement).
