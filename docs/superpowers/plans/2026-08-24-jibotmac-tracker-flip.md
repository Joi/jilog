# jibotmac jilog Tracker Flip + Hermes Privacy Split — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: this run executes under /do-it —
> execution uses Agency (`mcp__agency__*`), one task per plan task, review at
> chunk boundaries. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Flip jibotmac's nightly jilog review to file recurrence issues into
kata project `jilog` — with Hermes-surface text structurally excluded — by
splitting the single nightly run into a TRACKED lane (claude-code reader only,
new config + wrapper + LaunchAgent) and the existing LOCAL lane (hermes reader
only, tracker=none).

**Architecture:** All machine changes happen on jibotmac over `ssh jibotmac`
(login user `jibot`), deployed from repo-committed artifact sources in
`docs/ops/jibotmac-tracker-split/` that this plan embeds IN FULL (the target
local config and plist are authored from the current jibotmac bytes captured
below — no snapshot-ordering dependency). The tracked lane = second TOML
config + a bash wrapper (kata preflight, hard timeout, fail-loud
tracker-error grep) + a new LaunchAgent with KATA_* env baked in. The
committed tracked plist is the FINAL (armed) state; Stage-1 deploy strips
`--create-issues` at install time and the arming step restores it, so
deployed-at-end == committed. Local lane is slimmed to hermes-only LAST,
after the tracked lane is proven.

**Tech Stack:** jilog 0.6.0 (existing binary, no code changes), kata v0.15.1
client, launchd, bash 3.2 (macOS default — no arrays/mapfile/GNU timeout).

**Spec:** docs/superpowers/specs/2026-08-24-jibotmac-tracker-flip-design.md
(read it first; its verification-gate letters (a)–(h) are referenced below).

## Global Constraints

- NOTHING on jibotmac is restarted/reloaded beyond: `com.jibot.kata-tunnel`
  (only if it breaks — currently healthy), `com.amplifier.nightly-learning`,
  the new `com.jibot.jilog-nightly-tracked`, one verification-only kickstart
  of `com.jibot.jilog-spool-ingest`, and the temporary canary label
  `com.jibot.jilog-canary-6rzb` (created + removed inside Task 5). No
  reboots. No gateway LaunchAgent (`ai.hermes.*`, `com.cloudflare.*`,
  `com.jibot.hermes-*`) changes.
- The tracked and local launches MUST use distinct `--processed-file` and
  `--digest-dir` values (tracked: `…/processed-sessions-tracked.txt` +
  `/Users/jibot/.amplifier/health`; local: `…/processed-sessions.txt` +
  `/Users/jibot/.jilog/digests`).
- No `hermes`/`generic` reader stanza may ever appear in the tracked config.
- Never commit or paste the real `KATA_AUTH_TOKEN` (repo copies carry the
  literal text `REDACTED`; injection reads it from the environment after
  `source ~/.zshrc.local`, NEVER via `grep | cut`, and all writes happen
  under `umask 077` with an atomic rename). Never put user text in kata
  bodies we author.
- All launchd plist changes load via `bootout` + `bootstrap` (never rely on
  kickstart to pick up edits).
- One change at a time on jibotmac; verify each step before the next. Every
  machine-mutating step is RETRY-SAFE as written (guards on snapshots and
  seeds) — do not improvise variants.
- Repo commits happen on branch `6rzb-jibotmac-jilog-tracker-flip` only.

## Captured current state (byte sources, fetched 2026-08-25 over ssh)

**Current `jibotmac:~/.jilog.toml`** (basis for the target local config; the
zones below must survive VERBATIM):

```toml
# jilog config — jibotmac (hermes host). Deployed 2026-07-05, jilog v0.2.0
# fleet rollout. Claude Code sessions only; no kata client on this host, so
# tracker stays "none" (digest only). Nightly run:
# ~/Library/LaunchAgents/com.amplifier.nightly-learning.plist (no
# active-mac-guard — this host is always-on, not part of the primary swap).

[[reader]]
type = "claude-code"
path = "~/.claude/projects"

[tracker]
type = "none"

# Fleet ledger authority (jilog#546w, wired 2026-08-18). Producers (joimba,
# macazbd via vault sync + joimba rsync bridge) push spool files into
# spool/public-ops; the nightly com.jibot.jilog-spool-ingest job validates,
# dedups, and commits them into public-ops-fleet — single writer: THIS host.
# jibotmac produces no public-ops segments itself; its ledger dir stays empty.
[[zone]]
id = "public-ops"
ledger_path = "~/switchboard/ops/ledgers/public-ops"
spool_path = "~/switchboard/ops/ledgers/spool/public-ops"
fleet_store_path = "~/switchboard/ops/ledgers/public-ops-fleet"

[[zone]]
id = "public-ops-fleet"
ledger_path = "~/switchboard/ops/ledgers/public-ops-fleet"
spool = false

# Hermes non-intimate surface export (jibot-code#ca49, wired 2026-08-20).
# Populated hourly by com.jibot.hermes-jilog-export (TSV allowlist, intimate
# profiles refused in code). Persona/chat routing needs jilog >= 0.6.0.
[[reader]]
type = "generic"
name = "hermes"
path = "/Users/jibot/.jilog/hermes-export/*/*.jsonl"
session_id_from = "file_stem"
```

**Current `jibotmac:~/Library/LaunchAgents/com.amplifier.nightly-learning.plist`**
(basis for the target local plist):

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<!--
  com.amplifier.nightly-learning - nightly jilog session review (jibotmac).
  jilog v0.2.0 (github.com/Joi/jilog, tag v0.2.0), binary copied from
  macazbd (both arm64 darwin; no cargo on this host). Config: ~/.jilog.toml.
  TEST:   launchctl kickstart gui/$(id -u)/com.amplifier.nightly-learning
  LOGS:   tail -f ~/.jilog/logs/nightly-learning.{stdout,stderr}.log
-->
<plist version="1.0">
<dict>
	<key>Label</key>
	<string>com.amplifier.nightly-learning</string>
	<key>ProgramArguments</key>
	<array>
		<string>/Users/jibot/.local/bin/jilog</string>
		<string>review</string>
		<string>nightly</string>
		<string>--digest-dir</string>
		<string>/Users/jibot/.jilog/digests</string>
	</array>
	<key>EnvironmentVariables</key>
	<dict>
		<key>HOME</key>
		<string>/Users/jibot</string>
		<key>PATH</key>
		<string>/Users/jibot/.local/bin:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin</string>
	</dict>
	<key>StartCalendarInterval</key>
	<dict>
		<key>Hour</key>
		<integer>22</integer>
		<key>Minute</key>
		<integer>50</integer>
	</dict>
	<key>StandardOutPath</key>
	<string>/Users/jibot/.jilog/logs/nightly-learning.stdout.log</string>
	<key>StandardErrorPath</key>
	<string>/Users/jibot/.jilog/logs/nightly-learning.stderr.log</string>
	<key>RunAtLoad</key>
	<false/>
	<key>Nice</key>
	<integer>10</integer>
</dict>
</plist>
```

Task 2 re-fetches both live files and DIFFS them against the blocks above;
any drift aborts the run for reassessment (state moved under us).

## File Structure

Repo (all new, under `docs/ops/jibotmac-tracker-split/`):
- `README.md` — runbook: lanes, verify gates, arm/disarm, rollback commands.
- `jilog-nightly-tracked.sh` — wrapper (→ `jibotmac:~/scripts/`, 0755).
- `jilog-tracked.toml` — tracked config (→ `jibotmac:~/.jilog-tracked.toml`).
- `com.jibot.jilog-nightly-tracked.plist` — tracked LaunchAgent in FINAL
  ARMED form (`--create-issues` present), token `REDACTED` (deployed with
  real token injected, 0600; Stage-1 install strips the flag).
- `com.amplifier.nightly-learning.plist` — target LOCAL plist.
- `jilog-local.toml` — target LOCAL config (hermes-only), deployed AS
  `~/.jilog.toml`. (Named `jilog-local` in the repo because `.gitignore`'s
  unanchored `jilog.toml` rule silently swallows the bare name — found by
  roborev on the first commit.)
- `canary/canary-6rzb.jsonl.tmpl` — fixture template (`__RUNID__`
  placeholder makes each canary run-unique).
- `canary/jilog-canary.toml` — canary config.
- `canary/com.jibot.jilog-canary-6rzb.plist` — one-shot canary label,
  token `REDACTED`.

Machine (jibotmac):
- `~/.jilog/backup-6rzb/` — write-once snapshots (originals + launchctl
  list) — guarded against overwrite on retry.
- Deployed copies of the artifacts.

## Chunks

- **Chunk A** = Task 1 (repo artifact authoring) — reviewable diff.
- **Chunk B** = Tasks 2–7 (jibotmac deploy + verification; evidence in the
  run manifest and the Task 7 commit).
- **Chunk C** = Task 8 (follow-ups) — reviewable diff.

---

### Task 1: Author all artifacts in the repo (Chunk A)

**Files:** everything under `docs/ops/jibotmac-tracker-split/` listed above.

**Interfaces:**
- Produces: the exact file contents Tasks 2–6 deploy verbatim (token
  injection and Stage-1 flag-strip excepted).

- [ ] **Step 1: Write `jilog-nightly-tracked.sh`.** BYTE SOURCE OF TRUTH:
  the committed artifact `docs/ops/jibotmac-tracker-split/jilog-nightly-tracked.sh`
  — deploy from THAT file, never from this block. The block below is the
  originally-authored version and is retained for the review record only;
  commit e3d0b0f hardened the artifact afterward (exit-4 mapping for
  jilog's own rc, fx51 pattern filtered from exit 2, preflight
  stdout+stderr capture, run-log truncation) without back-porting here.
  Original content as first authored:

```bash
#!/bin/bash
# jilog-nightly-tracked.sh — TRACKED (kata-filing) nightly jilog run, jibotmac.
# jibot-code#6rzb. Source of truth: jilog repo docs/ops/jibotmac-tracker-split/.
#
# Adds three behaviors the jilog binary does not have:
#   1. kata preflight — a real daemon round-trip BEFORE jilog runs, so a dead
#      tunnel/daemon fails the job without marking any session processed.
#   2. hard timeout — jilog runs in its own process group and is killed
#      (TERM then KILL) after JILOG_TRACKED_TIMEOUT_SECS, so a stalled kata
#      subprocess can never wedge the launchd label.
#   3. fail-loud grep — jilog's tracker errors are warn-only on its COMBINED
#      output (tracing writes to STDOUT); if the captured output contains
#      them, exit 2 so launchctl shows the failure.
# Exit codes: 1 preflight failed or timed out (jilog NOT run) / 2 tracker
# errors in output / 3 jilog timeout / else jilog's own exit code.
# Extra args (--create-issues once armed) pass through from the plist via
# "$@". Env overrides exist for the canary + harness tests only.
set -u

JILOG_BIN="${JILOG_TRACKED_JILOG_BIN:-/Users/jibot/.local/bin/jilog}"
KATA_BIN="${JILOG_TRACKED_KATA_BIN:-/Users/jibot/.local/bin/kata}"
TIMEOUT_SECS="${JILOG_TRACKED_TIMEOUT_SECS:-1800}"
PREFLIGHT_TIMEOUT_SECS="${JILOG_TRACKED_PREFLIGHT_TIMEOUT_SECS:-60}"
CONFIG="${JILOG_TRACKED_CONFIG:-/Users/jibot/.jilog-tracked.toml}"
DIGEST_DIR="${JILOG_TRACKED_DIGEST_DIR:-/Users/jibot/.amplifier/health}"
PROCESSED="${JILOG_TRACKED_PROCESSED_FILE:-/Users/jibot/.jilog/telemetry/processed-sessions-tracked.txt}"
LOG_DIR="${JILOG_TRACKED_LOG_DIR:-/Users/jibot/.jilog/logs}"
RUN_LOG="$LOG_DIR/nightly-tracked.run.log"

mkdir -p "$LOG_DIR" "$DIGEST_DIR" "$(dirname "$PROCESSED")"

ts() { date -u +%Y-%m-%dT%H:%M:%SZ; }

# --- 1. Preflight (BOUNDED): real daemon round-trip; fail WITHOUT running
# jilog. Runs in its own process group so a tunnel that accepts but never
# answers cannot wedge the label (the daemon-hang case, not just refusal). ---
set -m
"$KATA_BIN" --project jilog --json list --status open >/dev/null 2>>"$RUN_LOG" &
kpid=$!
set +m
kwaited=0
while kill -0 "$kpid" 2>/dev/null; do
    if [ "$kwaited" -ge "$PREFLIGHT_TIMEOUT_SECS" ]; then
        echo "$(ts) preflight TIMEOUT after ${PREFLIGHT_TIMEOUT_SECS}s; killing kata; jilog NOT run" >>"$RUN_LOG"
        kill -TERM -- "-$kpid" 2>/dev/null
        sleep 2
        kill -KILL -- "-$kpid" 2>/dev/null
        exit 1
    fi
    sleep 2
    kwaited=$((kwaited + 2))
done
if ! wait "$kpid"; then
    echo "$(ts) preflight FAILED: kata daemon unreachable; jilog NOT run" >>"$RUN_LOG"
    exit 1
fi

# --- 2. Bounded run, own process group, combined stdout+stderr capture. ---
JOB_LOG="$(mktemp /tmp/jilog-tracked.XXXXXX)" || exit 1
set -m
"$JILOG_BIN" --config "$CONFIG" review nightly \
    --digest-dir "$DIGEST_DIR" \
    --processed-file "$PROCESSED" \
    "$@" >"$JOB_LOG" 2>&1 &
pid=$!
set +m

waited=0
while kill -0 "$pid" 2>/dev/null; do
    if [ "$waited" -ge "$TIMEOUT_SECS" ]; then
        echo "$(ts) TIMEOUT after ${TIMEOUT_SECS}s; killing process group $pid" >>"$RUN_LOG"
        kill -TERM -- "-$pid" 2>/dev/null
        sleep 5
        kill -KILL -- "-$pid" 2>/dev/null
        cat "$JOB_LOG" >>"$RUN_LOG"
        rm -f "$JOB_LOG"
        exit 3
    fi
    sleep 5
    waited=$((waited + 5))
done
wait "$pid"
rc=$?

# --- 3. Fail-loud: surface jilog's warn-only tracker errors. ---
cat "$JOB_LOG" >>"$RUN_LOG"
if grep -Eq 'tracker\.create failed|tracker\.list_open failed' "$JOB_LOG"; then
    echo "$(ts) tracker errors in output; exit 2 (digest retains signals)" >>"$RUN_LOG"
    rm -f "$JOB_LOG"
    exit 2
fi
rm -f "$JOB_LOG"

if [ "$rc" -ne 0 ]; then
    echo "$(ts) jilog exited $rc" >>"$RUN_LOG"
    exit "$rc"
fi
echo "$(ts) OK" >>"$RUN_LOG"
exit 0
```

- [ ] **Step 2: Write `jilog-tracked.toml`**:

```toml
# ~/.jilog-tracked.toml — jibotmac TRACKED nightly run (jibot-code#6rzb).
# Non-Hermes readers ONLY. Hermes surfaces (LINE/Telegram/GIDC-email) must
# never reach fleet-visible kata (Joi ruling 2026-08-24); they are scanned by
# the LOCAL run (~/.jilog.toml, tracker = "none"). Launched by
# com.jibot.jilog-nightly-tracked via ~/scripts/jilog-nightly-tracked.sh with
# distinct --processed-file / --digest-dir (isolation requirement).
# NO [[zone]] stanzas here: zones belong to the default config (spool-ingest).

[[reader]]
type = "claude-code"
path = "~/.claude/projects"

[tracker]
type = "kata"
project = "jilog"
```

- [ ] **Step 3: Write `com.jibot.jilog-nightly-tracked.plist`** — FINAL
  ARMED form (this is what runs every night once Task 5 completes):

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<!--
  com.jibot.jilog-nightly-tracked - TRACKED nightly jilog review (jibotmac).
  jibot-code#6rzb. Runs the wrapper (preflight/timeout/fail-loud); the wrapper
  runs jilog with the tracked config. This file is the FINAL ARMED state;
  Stage-1 install strips the create-issues argument until the payload
  preview + canary gates pass. Deployed mode 0600 (embeds KATA_AUTH_TOKEN;
  REDACTED in the repo copy).
  TEST:  launchctl kickstart gui/$(id -u)/com.jibot.jilog-nightly-tracked
  LOGS:  ~/.jilog/logs/nightly-tracked.{run,stdout,stderr}.log
-->
<plist version="1.0">
<dict>
	<key>Label</key>
	<string>com.jibot.jilog-nightly-tracked</string>
	<key>ProgramArguments</key>
	<array>
		<string>/Users/jibot/scripts/jilog-nightly-tracked.sh</string>
		<string>--create-issues</string>
	</array>
	<key>EnvironmentVariables</key>
	<dict>
		<key>HOME</key>
		<string>/Users/jibot</string>
		<key>PATH</key>
		<string>/Users/jibot/.local/bin:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin</string>
		<key>KATA_SERVER</key>
		<string>http://127.0.0.1:7777</string>
		<key>KATA_AUTHOR</key>
		<string>joi-jibotmac</string>
		<key>KATA_AUTH_TOKEN</key>
		<string>REDACTED</string>
		<key>KATA_TRUST_PRIVATE_NETWORK</key>
		<string>1</string>
	</dict>
	<key>StartCalendarInterval</key>
	<dict>
		<key>Hour</key>
		<integer>23</integer>
		<key>Minute</key>
		<integer>20</integer>
	</dict>
	<key>StandardOutPath</key>
	<string>/Users/jibot/.jilog/logs/nightly-tracked.stdout.log</string>
	<key>StandardErrorPath</key>
	<string>/Users/jibot/.jilog/logs/nightly-tracked.stderr.log</string>
	<key>RunAtLoad</key>
	<false/>
	<key>Nice</key>
	<integer>10</integer>
</dict>
</plist>
```

- [ ] **Step 4: Write `com.amplifier.nightly-learning.plist`** — the
  "Captured current state" plist above with EXACTLY two changes: (i) two new
  ProgramArguments entries appended after the `--digest-dir` value string:

```xml
		<string>--processed-file</string>
		<string>/Users/jibot/.jilog/telemetry/processed-sessions.txt</string>
```

  and (ii) the comment block updated: the `Config: ~/.jilog.toml.` line
  extended with ` LOCAL lane (hermes-only) since jibot-code#6rzb; tracked
  lane = com.jibot.jilog-nightly-tracked.`, and the stale `jilog v0.2.0 …
  tag v0.2.0` reference corrected to `jilog v0.6.0` (the deployed binary —
  leaving v0.2.0 is misleading operational doc). Everything else
  byte-identical to the captured block.
- [ ] **Step 5: Write `jilog-local.toml`** — the target LOCAL config
  (deployed as `~/.jilog.toml`; repo name dodges the `.gitignore` rule),
  exactly:

```toml
# jilog config — jibotmac LOCAL-ONLY run (hermes surfaces; tracker = "none").
# Split 2026-08-25 (jibot-code#6rzb, Joi ruling 2026-08-24): Claude Code
# sessions moved to the TRACKED lane (~/.jilog-tracked.toml +
# com.jibot.jilog-nightly-tracked), which files to kata project jilog; hermes
# text stays in THIS local lane and must never move to a tracked config.
# Zones below also serve com.jibot.jilog-spool-ingest — do not remove.

[tracker]
type = "none"

# Fleet ledger authority (jilog#546w, wired 2026-08-18). Producers (joimba,
# macazbd via vault sync + joimba rsync bridge) push spool files into
# spool/public-ops; the nightly com.jibot.jilog-spool-ingest job validates,
# dedups, and commits them into public-ops-fleet — single writer: THIS host.
# jibotmac produces no public-ops segments itself; its ledger dir stays empty.
[[zone]]
id = "public-ops"
ledger_path = "~/switchboard/ops/ledgers/public-ops"
spool_path = "~/switchboard/ops/ledgers/spool/public-ops"
fleet_store_path = "~/switchboard/ops/ledgers/public-ops-fleet"

[[zone]]
id = "public-ops-fleet"
ledger_path = "~/switchboard/ops/ledgers/public-ops-fleet"
spool = false

# Hermes non-intimate surface export (jibot-code#ca49, wired 2026-08-20).
# Populated hourly by com.jibot.hermes-jilog-export (TSV allowlist, intimate
# profiles refused in code). Persona/chat routing needs jilog >= 0.6.0.
[[reader]]
type = "generic"
name = "hermes"
path = "/Users/jibot/.jilog/hermes-export/*/*.jsonl"
session_id_from = "file_stem"
```

  (Note: the zone comments and stanzas are byte-identical to the captured
  current file — verify with a local diff in Step 10. The header avoids the
  literal reader-stanza string so gate greps for `type = "claude-code"`
  return 0.)
- [ ] **Step 6: Write `canary/canary-6rzb.jsonl.tmpl`** (three flat-schema
  lines; the middle user line is 15–200 chars, no tool_result, content-free
  marker text — it becomes the canary issue's title/body; `__RUNID__` is
  replaced at deploy time so every canary run is unique and cannot pass
  vacuously via kata's idempotent reopen path):

```json
{"role":"assistant","content":"Canary probe transcript for jibot-code 6rzb gate c3."}
{"role":"user","content":"jilog canary 6rzb run __RUNID__ verification probe - please close"}
{"role":"assistant","content":"Acknowledged - canary correction recorded for gate c3."}
```

- [ ] **Step 7: Write `canary/jilog-canary.toml`**:

```toml
# Canary config for gate (c3) — jibot-code#6rzb. Files exactly ONE issue
# (from the fixture transcript's single correction window) into kata project
# jilog via the REAL tracker, from the launchd context. Removed after the
# gate passes.

[[reader]]
type = "claude-code"
path = "~/.jilog/canary-6rzb/fixture"

[tracker]
type = "kata"
project = "jilog"
```

- [ ] **Step 8: Write `canary/com.jibot.jilog-canary-6rzb.plist`**:

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>Label</key>
	<string>com.jibot.jilog-canary-6rzb</string>
	<key>ProgramArguments</key>
	<array>
		<string>/Users/jibot/scripts/jilog-nightly-tracked.sh</string>
		<string>--create-issues</string>
	</array>
	<key>EnvironmentVariables</key>
	<dict>
		<key>HOME</key>
		<string>/Users/jibot</string>
		<key>PATH</key>
		<string>/Users/jibot/.local/bin:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin</string>
		<key>KATA_SERVER</key>
		<string>http://127.0.0.1:7777</string>
		<key>KATA_AUTHOR</key>
		<string>joi-jibotmac</string>
		<key>KATA_AUTH_TOKEN</key>
		<string>REDACTED</string>
		<key>KATA_TRUST_PRIVATE_NETWORK</key>
		<string>1</string>
		<key>JILOG_TRACKED_CONFIG</key>
		<string>/Users/jibot/.jilog/canary-6rzb/jilog-canary.toml</string>
		<key>JILOG_TRACKED_DIGEST_DIR</key>
		<string>/Users/jibot/.jilog/canary-6rzb/digests</string>
		<key>JILOG_TRACKED_PROCESSED_FILE</key>
		<string>/Users/jibot/.jilog/canary-6rzb/processed.txt</string>
		<key>JILOG_TRACKED_LOG_DIR</key>
		<string>/Users/jibot/.jilog/canary-6rzb/logs</string>
	</dict>
	<key>StandardOutPath</key>
	<string>/Users/jibot/.jilog/canary-6rzb/logs/launchd.stdout.log</string>
	<key>StandardErrorPath</key>
	<string>/Users/jibot/.jilog/canary-6rzb/logs/launchd.stderr.log</string>
	<key>RunAtLoad</key>
	<true/>
	<key>Nice</key>
	<integer>10</integer>
</dict>
</plist>
```

- [ ] **Step 9: Write `README.md`** — runbook covering: the two lanes
  (paths, schedules, isolation table), the privacy boundary + rebuttal
  pointer to the spec, Stage-1/Stage-2 arming and disarming of
  `--create-issues` (python plistlib append/remove + bootout/bootstrap),
  wrapper exit codes (1 preflight / 2 tracker-errors / 3 timeout / else
  jilog's), verification gates (a)–(h) with the exact commands from Tasks
  2–6, and rollback as exact commands: `launchctl bootout
  gui/$UID/com.jibot.jilog-nightly-tracked`; `rm` its plist + wrapper +
  `~/.jilog-tracked.toml`; `cp ~/.jilog/backup-6rzb/jilog.toml.orig
  ~/.jilog.toml`; `cp ~/.jilog/backup-6rzb/com.amplifier.nightly-learning.plist
  ~/Library/LaunchAgents/`; `launchctl bootout … && launchctl bootstrap …`
  the local label; `trash` tracked digests + processed-sessions-tracked.txt.
- [ ] **Step 10: Local validation:**
  - `bash -n docs/ops/jibotmac-tracker-split/jilog-nightly-tracked.sh` — 0.
  - `plutil -lint docs/ops/jibotmac-tracker-split/*.plist
    docs/ops/jibotmac-tracker-split/canary/*.plist` — all OK.
  - `grep -c '<string>REDACTED</string>'` = 1 in each of the two token
    plists (the exact value node — the tracked plist's comment also says
    REDACTED, so a bare-word grep counts 2 and is NOT the gate).
  - Tracked-config positive assertions: in `jilog-tracked.toml`,
    `grep -c '^\[\[reader\]\]'` = 1, `grep -c 'type = "claude-code"'` = 1,
    `grep -c 'type = "kata"'` = 1, `grep -c 'project = "jilog"'` = 1, and
    `grep -Ec 'hermes|generic'` = 0 (ignoring the comment header: run the
    greps on `grep -v '^#' jilog-tracked.toml`).
  - Local-config equivalence — semantic assertion (this IS the gate; do not
    attempt grep-based diffs against the live file here):

```bash
python3 - docs/ops/jibotmac-tracker-split/jilog-local.toml <<'EOF'
import sys, tomllib
new = tomllib.load(open(sys.argv[1], 'rb'))
assert [r for r in new.get('reader', [])] == [
    {'type': 'generic', 'name': 'hermes',
     'path': '/Users/jibot/.jilog/hermes-export/*/*.jsonl',
     'session_id_from': 'file_stem'}], new.get('reader')
assert new['tracker'] == {'type': 'none'}
assert new['zone'] == [
    {'id': 'public-ops',
     'ledger_path': '~/switchboard/ops/ledgers/public-ops',
     'spool_path': '~/switchboard/ops/ledgers/spool/public-ops',
     'fleet_store_path': '~/switchboard/ops/ledgers/public-ops-fleet'},
    {'id': 'public-ops-fleet',
     'ledger_path': '~/switchboard/ops/ledgers/public-ops-fleet',
     'spool': False}], new['zone']
print('LOCAL-CONFIG-OK')
EOF
```

  (This Mac has python3 ≥3.11 with tomllib; the semantic zone comparison
  runs LOCALLY — the machine-side gate later only needs deployed ==
  committed bytes.)
- [ ] **Step 11: Commit:**

```bash
git add docs/superpowers docs/ops/jibotmac-tracker-split
git commit -m "docs(6rzb): jibotmac tracked/local digest split artifacts + pipeline docs"
```

---

### Task 2: jibotmac snapshots + tracked-lane deploy (Chunk B)

**Interfaces:**
- Consumes: Task 1 artifact files (scp source).
- Produces: deployed tracked lane, UNARMED, bootstrapped; write-once
  snapshots in `jibotmac:~/.jilog/backup-6rzb/`.

- [ ] **Step 1: Drift check.** `ssh jibotmac 'cat ~/.jilog.toml'` and
  `ssh jibotmac 'cat ~/Library/LaunchAgents/com.amplifier.nightly-learning.plist'`
  must equal the "Captured current state" blocks byte-for-byte (diff against
  temp copies locally). ANY drift → STOP, reassess (state moved under us).
- [ ] **Step 2: Write-once snapshot (retry-safe AND interruption-safe —
  every write goes to a `.tmp`, is verified with `cmp`, then atomically
  renamed; a half-written file can never be preserved as "the backup").**

```bash
ssh jibotmac 'mkdir -p ~/.jilog/backup-6rzb \
  && if [ ! -e ~/.jilog/backup-6rzb/jilog.toml.orig ]; then cp ~/.jilog.toml ~/.jilog/backup-6rzb/jilog.toml.orig.tmp && cmp ~/.jilog.toml ~/.jilog/backup-6rzb/jilog.toml.orig.tmp && mv ~/.jilog/backup-6rzb/jilog.toml.orig.tmp ~/.jilog/backup-6rzb/jilog.toml.orig; fi \
  && if [ ! -e ~/.jilog/backup-6rzb/com.amplifier.nightly-learning.plist ]; then cp ~/Library/LaunchAgents/com.amplifier.nightly-learning.plist ~/.jilog/backup-6rzb/nl.plist.tmp && cmp ~/Library/LaunchAgents/com.amplifier.nightly-learning.plist ~/.jilog/backup-6rzb/nl.plist.tmp && mv ~/.jilog/backup-6rzb/nl.plist.tmp ~/.jilog/backup-6rzb/com.amplifier.nightly-learning.plist; fi \
  && if [ ! -e ~/.jilog/backup-6rzb/launchctl-list-before.txt ]; then launchctl list > ~/.jilog/backup-6rzb/lcl.tmp && test -s ~/.jilog/backup-6rzb/lcl.tmp && mv ~/.jilog/backup-6rzb/lcl.tmp ~/.jilog/backup-6rzb/launchctl-list-before.txt; fi \
  && ls -la ~/.jilog/backup-6rzb/ && nc -z -w 3 127.0.0.1 7777 && echo TUNNEL-OK'
```

  Expect the three snapshot files + `TUNNEL-OK`.
- [ ] **Step 3: Dirs + guarded seed (same tmp+cmp+mv discipline).**

```bash
ssh jibotmac 'mkdir -p ~/.amplifier/health ~/.jilog/logs ~/.jilog/telemetry ~/scripts \
  && if [ ! -e ~/.jilog/telemetry/processed-sessions-tracked.txt ]; then cp ~/.jilog/telemetry/processed-sessions.txt ~/.jilog/telemetry/pst.tmp && cmp ~/.jilog/telemetry/processed-sessions.txt ~/.jilog/telemetry/pst.tmp && mv ~/.jilog/telemetry/pst.tmp ~/.jilog/telemetry/processed-sessions-tracked.txt && echo SEEDED; else echo SEED-EXISTS; fi \
  && wc -l ~/.jilog/telemetry/processed-sessions-tracked.txt'
```

- [ ] **Step 4: Deploy wrapper + tracked config.**
  `scp docs/ops/jibotmac-tracker-split/jilog-nightly-tracked.sh jibotmac:scripts/`
  and `scp docs/ops/jibotmac-tracker-split/jilog-tracked.toml jibotmac:.jilog-tracked.toml`;
  then verify deployed == committed and gate (a) tracked half ON THE HOST:

```bash
ssh jibotmac 'chmod 0755 ~/scripts/jilog-nightly-tracked.sh && bash -n ~/scripts/jilog-nightly-tracked.sh && echo WRAPPER-OK' \
  && diff <(ssh jibotmac cat scripts/jilog-nightly-tracked.sh) docs/ops/jibotmac-tracker-split/jilog-nightly-tracked.sh && echo WRAPPER-SYNC \
  && diff <(ssh jibotmac cat .jilog-tracked.toml) docs/ops/jibotmac-tracker-split/jilog-tracked.toml && echo TOML-SYNC
```

  (Deployed == committed bytes; the Task 1 Step 10 positive assertions
  therefore hold on the host copy too.)
- [ ] **Step 5: Dry-run with LIVE arguments.**

```bash
ssh jibotmac 'source ~/.zshrc.local && ~/.local/bin/jilog --config ~/.jilog-tracked.toml review nightly --digest-dir ~/.amplifier/health --processed-file ~/.jilog/telemetry/processed-sessions-tracked.txt --dry-run --json'
```

  Parses; record the `sessions_scanned` and signal counts from the JSON in
  the manifest (dry-run writes nothing, contacts no tracker).
- [ ] **Step 6: Install the tracked plist — token from env, umask 077,
  atomic, Stage-1 strip.** `scp docs/ops/jibotmac-tracker-split/com.jibot.jilog-nightly-tracked.plist
  jibotmac:/tmp/jilog-tracked.plist.in`, then:

```bash
ssh jibotmac 'source ~/.zshrc.local && umask 077 && /usr/bin/python3 - <<PYEOF
import os, plistlib
with open("/tmp/jilog-tracked.plist.in", "rb") as f:
    p = plistlib.load(f)
p["EnvironmentVariables"]["KATA_AUTH_TOKEN"] = os.environ["KATA_AUTH_TOKEN"]
# Stage 1: strip the arming flag; Task 5 Step 6 restores it.
p["ProgramArguments"] = [a for a in p["ProgramArguments"] if a != "--create-issues"]
tmp = "/Users/jibot/Library/LaunchAgents/.com.jibot.jilog-nightly-tracked.plist.tmp"
with open(tmp, "wb") as f:
    plistlib.dump(p, f)
os.rename(tmp, "/Users/jibot/Library/LaunchAgents/com.jibot.jilog-nightly-tracked.plist")
print("INSTALLED")
PYEOF
plutil -lint ~/Library/LaunchAgents/com.jibot.jilog-nightly-tracked.plist \
  && stat -f "%Lp" ~/Library/LaunchAgents/com.jibot.jilog-nightly-tracked.plist \
  && rm /tmp/jilog-tracked.plist.in'
```

  Expect `INSTALLED`, `OK`, and mode `600` (umask 077 makes the temp file
  0600 from birth; rename is atomic; token never appears on a command line).
- [ ] **Step 7: Bootstrap (unarmed Stage 1) — idempotent on retry.**

```bash
ssh jibotmac 'launchctl bootout gui/$(id -u)/com.jibot.jilog-nightly-tracked 2>/dev/null; launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/com.jibot.jilog-nightly-tracked.plist && launchctl print gui/$(id -u)/com.jibot.jilog-nightly-tracked | sed -n "/arguments = {/,/}/p"'
```

  (The leading bootout tolerates a prior attempt having loaded the label —
  bootstrap fails on an already-loaded label; the bootout no-ops when it is
  not loaded.) Arguments block shows ONLY the wrapper path (no
  `--create-issues`). If bootstrap fails: leave the on-disk plist in place
  (never delete the definition of a possibly-loaded label), diagnose via
  `launchctl print`, and reassess.

---

### Task 3: Wrapper harness tests on jibotmac (gates c, c2, e) (Chunk B)

Each test uses its OWN log dir (`/tmp/harness-<n>`) so runs cannot
contaminate each other; env comes from `source ~/.zshrc.local` (KATA_*
exported there — never `grep | cut`); every test ASSERTS its expected exit
code (`rc=$?; test "$rc" -eq N`) instead of merely printing it, and prints
PASS/FAIL. The executor runs each ssh through the Bash tool with a 300000 ms
tool timeout, which bounds the whole test even if the wrapper itself is
broken (its stub sleeps are 600 s precisely so a timeout failure is
detectable rather than racing the harness bound). FAIL branches exit
nonzero so automation cannot mistake a failed gate for a pass.
POST-HARDENING ADDENDUM: the wrapper gained exit codes 4 (jilog rc
nonzero), 5 (only the known fx51 pattern), and a TERM-forwarding trap after
these gates first ran; the additional stub tests covering those paths (real
create-error → 2, VERBATIM backticked fx51 line → 5, list_open warn → 2,
rc-7 → 4) are recorded with their outputs in the run manifest's Task 5
evidence block — re-run them after any wrapper change.

- [ ] **Step 1: Timeout test (gate c) — PID-recording stub, asserted.**

```bash
ssh jibotmac 'cat > /tmp/stub-hang.sh <<'"'"'EOF'"'"'
#!/bin/bash
sleep 600 &
echo $! > /tmp/stub-child.pid
( sleep 600 &
  echo $! > /tmp/stub-grandchild.pid
  wait ) &
wait
EOF
chmod +x /tmp/stub-hang.sh && rm -f /tmp/stub-child.pid /tmp/stub-grandchild.pid \
  && source ~/.zshrc.local \
  && JILOG_TRACKED_JILOG_BIN=/tmp/stub-hang.sh JILOG_TRACKED_TIMEOUT_SECS=5 \
     JILOG_TRACKED_LOG_DIR=/tmp/harness-1 \
     JILOG_TRACKED_DIGEST_DIR=/tmp/harness-1/digests \
     JILOG_TRACKED_PROCESSED_FILE=/tmp/harness-1/processed.txt \
     ~/scripts/jilog-nightly-tracked.sh; rc=$?; sleep 1; ok=1 \
  && test "$rc" -eq 3 || { echo "FAIL: exit=$rc want 3"; ok=0; } \
  ; for f in /tmp/stub-child.pid /tmp/stub-grandchild.pid; do
      if [ ! -f "$f" ]; then echo "FAIL: $f missing (stub never wrote it)"; ok=0;
      elif kill -0 "$(cat $f)" 2>/dev/null; then echo "FAIL: survivor $f pid=$(cat $f)"; ok=0;
      fi; done \
  ; if [ "$ok" -eq 1 ]; then echo "GATE-C-PASS"; else echo "GATE-C-FAIL"; exit 1; fi'
```

  Expect `GATE-C-PASS` (exit 3 AND both pid files present AND both PIDs
  dead — a missing pid file is a FAILURE, never counted as dead).
- [ ] **Step 2: Fail-loud grep test (gate c2), asserted.**

```bash
ssh jibotmac 'printf "#!/bin/bash\necho \"WARN tracker.create failed: probe\"\nexit 0\n" > /tmp/stub-warn.sh \
  && chmod +x /tmp/stub-warn.sh && source ~/.zshrc.local \
  && JILOG_TRACKED_JILOG_BIN=/tmp/stub-warn.sh JILOG_TRACKED_TIMEOUT_SECS=60 \
     JILOG_TRACKED_LOG_DIR=/tmp/harness-2 \
     JILOG_TRACKED_DIGEST_DIR=/tmp/harness-2/digests \
     JILOG_TRACKED_PROCESSED_FILE=/tmp/harness-2/processed.txt \
     ~/scripts/jilog-nightly-tracked.sh; rc=$?; \
  if [ "$rc" -eq 2 ]; then echo "GATE-C2-PASS"; else echo "GATE-C2-FAIL: exit=$rc want 2"; exit 1; fi'
```

  Expect `GATE-C2-PASS`.
- [ ] **Step 3: Preflight-negative test (gate e) — invocation sentinel,
  asserted.**

```bash
ssh jibotmac 'printf "#!/bin/bash\ntouch /tmp/harness-3/stub-ran.sentinel\nexit 0\n" > /tmp/stub-sentinel.sh \
  && chmod +x /tmp/stub-sentinel.sh && mkdir -p /tmp/harness-3 && rm -f /tmp/harness-3/stub-ran.sentinel \
  && source ~/.zshrc.local \
  && KATA_SERVER=http://127.0.0.1:7 \
     JILOG_TRACKED_JILOG_BIN=/tmp/stub-sentinel.sh JILOG_TRACKED_TIMEOUT_SECS=60 \
     JILOG_TRACKED_LOG_DIR=/tmp/harness-3 \
     JILOG_TRACKED_DIGEST_DIR=/tmp/harness-3/digests \
     JILOG_TRACKED_PROCESSED_FILE=/tmp/harness-3/processed.txt \
     ~/scripts/jilog-nightly-tracked.sh; rc=$?; \
  if [ "$rc" -eq 1 ] && [ ! -e /tmp/harness-3/stub-ran.sentinel ]; then echo "GATE-E-PASS"; else echo "GATE-E-FAIL: exit=$rc sentinel=$(test -e /tmp/harness-3/stub-ran.sentinel && echo present || echo absent)"; exit 1; fi'
```

  Expect `GATE-E-PASS` (preflight failed fast against the dead port; the
  jilog stage never executed).
- [ ] **Step 4: Preflight-HANG test (the daemon-accepts-but-never-answers
  case the bounded preflight exists for).**

```bash
ssh jibotmac 'printf "#!/bin/bash\nsleep 600\n" > /tmp/stub-hang-kata.sh \
  && chmod +x /tmp/stub-hang-kata.sh && mkdir -p /tmp/harness-4 && rm -f /tmp/harness-4/stub-ran.sentinel \
  && printf "#!/bin/bash\ntouch /tmp/harness-4/stub-ran.sentinel\nexit 0\n" > /tmp/stub-sentinel4.sh && chmod +x /tmp/stub-sentinel4.sh \
  && source ~/.zshrc.local \
  && JILOG_TRACKED_KATA_BIN=/tmp/stub-hang-kata.sh \
     JILOG_TRACKED_PREFLIGHT_TIMEOUT_SECS=5 \
     JILOG_TRACKED_JILOG_BIN=/tmp/stub-sentinel4.sh JILOG_TRACKED_TIMEOUT_SECS=60 \
     JILOG_TRACKED_LOG_DIR=/tmp/harness-4 \
     JILOG_TRACKED_DIGEST_DIR=/tmp/harness-4/digests \
     JILOG_TRACKED_PROCESSED_FILE=/tmp/harness-4/processed.txt \
     ~/scripts/jilog-nightly-tracked.sh; rc=$?; sleep 1; \
  if [ "$rc" -eq 1 ] && [ ! -e /tmp/harness-4/stub-ran.sentinel ] && ! pgrep -f stub-hang-kata >/dev/null; then echo "GATE-E2-PASS"; else echo "GATE-E2-FAIL: exit=$rc"; exit 1; fi'
```

  Expect `GATE-E2-PASS` within ~10s (hanging kata killed, jilog never ran).
- [ ] **Step 5: Clean up.** `ssh jibotmac 'rm -f /tmp/stub-hang.sh
  /tmp/stub-warn.sh /tmp/stub-sentinel.sh /tmp/stub-sentinel4.sh
  /tmp/stub-hang-kata.sh /tmp/stub-child.pid /tmp/stub-grandchild.pid &&
  rm -rf /tmp/harness-1 /tmp/harness-2 /tmp/harness-3 /tmp/harness-4'`.

---

### Task 4: Stage-1 live run + payload preview (gate d) (Chunk B)

- [ ] **Step 1: Kickstart.**

```bash
ssh jibotmac 'launchctl kickstart gui/$(id -u)/com.jibot.jilog-nightly-tracked && sleep 90 && tail -5 ~/.jilog/logs/nightly-tracked.run.log && launchctl list | grep jilog-nightly-tracked'
```

  Run log ends `OK`, last exit 0 — that IS the preflight evidence: the
  wrapper writes a preflight line only on FAILURE and exits 1 without
  running jilog, so an `OK` ending proves the kata round-trip succeeded
  from the launchd context.
- [ ] **Step 2: Payload preview (MANDATORY human-quality inspection).**

```bash
ssh jibotmac 'ls -la ~/.amplifier/health/ && cat ~/.amplifier/health/learning-digest-'"$(date -u +%Y-%m-%d)"'.md'
```

  Digest exists; INSPECT the full text: claude-code-derived content only,
  sane volume, no Hermes text, nothing secret-bearing. Record verdict +
  counts in the run manifest. If anything private/unexpected appears: STOP
  the pipeline, do not arm, reassess.
- [ ] **Step 3: Isolation evidence (gate b, tracked half).** The tracked
  paths live in the wrapper defaults + this run's outputs, so assert BOTH:

```bash
ssh jibotmac 'grep -c "/Users/jibot/.amplifier/health" ~/scripts/jilog-nightly-tracked.sh; grep -c "processed-sessions-tracked.txt" ~/scripts/jilog-nightly-tracked.sh; ls ~/.amplifier/health/ | head -3'
```

  Both greps ≥1; today's digest present in `~/.amplifier/health`. (Gate b is
  DEFINITION-level only: a same-day digest legitimately existing in
  `~/.jilog/digests` from the 22:50 local run is valid — its absence is NOT
  asserted.)

---

### Task 5: Create-path canary (gate c3) + arm Stage 2 (Chunk B)

- [ ] **Step 1: Time gate.** The real predicate is `date +%F` == `date -u
  +%F` on jibotmac (the UTC digest filename and the Local issue-body
  pointer must agree). EXECUTION CORRECTION: this step was written assuming
  jibotmac local = JST; ground truth is UTC+6, so the divergence window is
  00:00–05:59 local, and the predicate was verified directly instead of
  waiting for a wall-clock hour (run manifest Notes).
- [ ] **Step 2: Deploy canary (dirs FIRST — launchd needs the log dir to
  exist before bootstrap; RUNID makes the run unique).**

```bash
RUNID=$(date -u +%Y%m%dT%H%M%SZ) \
  && echo "$RUNID" > /tmp/canary-6rzb.runid \
  && sed "s/__RUNID__/$RUNID/" docs/ops/jibotmac-tracker-split/canary/canary-6rzb.jsonl.tmpl > /tmp/canary-6rzb.jsonl \
  && ssh jibotmac 'mkdir -p ~/.jilog/canary-6rzb/fixture ~/.jilog/canary-6rzb/digests ~/.jilog/canary-6rzb/logs' \
  && scp /tmp/canary-6rzb.jsonl "jibotmac:.jilog/canary-6rzb/fixture/canary-$RUNID.jsonl" \
  && scp docs/ops/jibotmac-tracker-split/canary/jilog-canary.toml jibotmac:.jilog/canary-6rzb/jilog-canary.toml \
  && ssh jibotmac "touch ~/.jilog/canary-6rzb/fixture/canary-$RUNID.jsonl" \
  && echo "RUNID=$RUNID"
```

  `RUNID` is PERSISTED in `/tmp/canary-6rzb.runid` — Steps 3–5 run in
  separate shells and MUST re-read it from there
  (`RUNID=$(cat /tmp/canary-6rzb.runid)`), never assume the variable
  survives. The RUNID is in the FILENAME too (`canary-$RUNID.jsonl`), so the
  jilog session id is unique per attempt — a retry after a failed create can
  never be silently skipped by the processed-sessions ledger (the file stem
  IS the session id for the claude-code reader).
  Precondition: `kata --project jilog --json list --status open` (from this
  Mac) contains NO issue mentioning `canary 6rzb run $RUNID`.
  **Failure path (any failure, before OR after the canary bootstrap):**
  attempt Step 5's bootout (tolerating "not loaded"), then `ssh jibotmac
  'rm -rf ~/.jilog/canary-6rzb'` — the retry of Step 2 starts from an EMPTY
  canary dir with a fresh RUNID. This is mandatory even for pre-bootstrap
  failures: a stale `canary-<oldRUNID>.jsonl` left in `fixture/` would make
  the eventual run file a SECOND issue under the old RUNID that nothing ever
  closes.
- [ ] **Step 3: Install + run the canary label.** `scp
  docs/ops/jibotmac-tracker-split/canary/com.jibot.jilog-canary-6rzb.plist
  jibotmac:/tmp/jilog-canary.plist.in`, then:

```bash
ssh jibotmac 'source ~/.zshrc.local && umask 077 && /usr/bin/python3 - <<PYEOF
import os, plistlib
with open("/tmp/jilog-canary.plist.in", "rb") as f:
    p = plistlib.load(f)
p["EnvironmentVariables"]["KATA_AUTH_TOKEN"] = os.environ["KATA_AUTH_TOKEN"]
tmp = "/Users/jibot/Library/LaunchAgents/.com.jibot.jilog-canary-6rzb.plist.tmp"
with open(tmp, "wb") as f:
    plistlib.dump(p, f)
os.rename(tmp, "/Users/jibot/Library/LaunchAgents/com.jibot.jilog-canary-6rzb.plist")
print("CANARY-INSTALLED")
PYEOF
plutil -lint ~/Library/LaunchAgents/com.jibot.jilog-canary-6rzb.plist \
  && stat -f "%Lp" ~/Library/LaunchAgents/com.jibot.jilog-canary-6rzb.plist \
  && rm /tmp/jilog-canary.plist.in'
```

  Expect `CANARY-INSTALLED`, `OK`, mode `600` (no argument-strip: the canary
  runs armed by design). Then:

```bash
ssh jibotmac 'launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/com.jibot.jilog-canary-6rzb.plist && sleep 60 && cat ~/.jilog/canary-6rzb/logs/nightly-tracked.run.log'
```

  RunAtLoad fires it once; run log ends `OK`.
- [ ] **Step 4: Verify + close the canary issue.** From this Mac
  (`RUNID=$(cat /tmp/canary-6rzb.runid)`): `kata --project jilog --json list
  --status open` → exactly one issue whose title contains `canary 6rzb run
  $RUNID` (the unique RUNID proves THIS run created it — not a reopen of an
  earlier canary). Close it (check `kata close --help` for the current flag
  set; `--done` + message is the established convention here):

```bash
kata --project jilog close <ref> --done --test "launchctl bootstrap gui/501 com.jibot.jilog-canary-6rzb.plist (one-shot canary run $RUNID)" --message "gate c3 canary (jibot-code#6rzb): tracked-lane create path verified end-to-end from launchd context on jibotmac (run $RUNID); closing as test evidence."
```

- [ ] **Step 5: Remove the canary — bootout is a hard gate, cleanup only
  after it succeeds.**

```bash
ssh jibotmac 'launchctl bootout gui/$(id -u)/com.jibot.jilog-canary-6rzb' \
  && ssh jibotmac 'rm ~/Library/LaunchAgents/com.jibot.jilog-canary-6rzb.plist' \
  && ssh jibotmac 'trash ~/.jilog/canary-6rzb 2>/dev/null || rm -rf ~/.jilog/canary-6rzb'
```

  If bootout fails: STOP — investigate; do NOT delete files while the label
  is loaded.
- [ ] **Step 6: Arm `--create-issues`.**

```bash
ssh jibotmac 'umask 077 && /usr/bin/python3 - <<PYEOF
import os, plistlib
path = "/Users/jibot/Library/LaunchAgents/com.jibot.jilog-nightly-tracked.plist"
with open(path, "rb") as f:
    p = plistlib.load(f)
if "--create-issues" not in p["ProgramArguments"]:
    p["ProgramArguments"].append("--create-issues")
tmp = path + ".tmp"
with open(tmp, "wb") as f:
    plistlib.dump(p, f)
os.rename(tmp, path)
print("ARMED")
PYEOF
plutil -lint ~/Library/LaunchAgents/com.jibot.jilog-nightly-tracked.plist \
  && { launchctl bootout gui/$(id -u)/com.jibot.jilog-nightly-tracked 2>/dev/null; true; } \
  && launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/com.jibot.jilog-nightly-tracked.plist \
  && launchctl print gui/$(id -u)/com.jibot.jilog-nightly-tracked | sed -n "/arguments = {/,/}/p"'
```

  Arguments block now lists the wrapper AND `--create-issues` — the deployed
  plist now equals the committed (armed) artifact modulo the real token.

---

### Task 6: Slim the local lane + system gates (f, g, h, a-local, b) (Chunk B)

- [ ] **Step 1: Stage + validate the slim config (live args), then swap
  atomically.**

```bash
scp docs/ops/jibotmac-tracker-split/jilog-local.toml jibotmac:.jilog.toml.new \
  && ssh jibotmac '~/.local/bin/jilog --config ~/.jilog.toml.new review nightly --digest-dir ~/.jilog/digests --processed-file ~/.jilog/telemetry/processed-sessions.txt --dry-run --json >/dev/null && echo LOADER-OK && mv ~/.jilog.toml.new ~/.jilog.toml'
```

  (Semantic zone equality vs the original was proven in Task 1 Step 10
  against the captured bytes, and Task 2 Step 1 proved the captured bytes
  are live; the machine gate is deployed == committed:
  `diff <(ssh jibotmac cat .jilog.toml) docs/ops/jibotmac-tracker-split/jilog-local.toml`.)
- [ ] **Step 2: Local config assertions (gate a, local half).**

```bash
ssh jibotmac 'grep -v "^#" ~/.jilog.toml > /tmp/lc.toml; grep -c "type = \"claude-code\"" /tmp/lc.toml; grep -c "name = \"hermes\"" /tmp/lc.toml; grep -c "type = \"none\"" /tmp/lc.toml; grep -c "^\[\[zone\]\]" /tmp/lc.toml; rm /tmp/lc.toml'
```

  Expect `0 / 1 / 1 / 2`.
- [ ] **Step 3: Local plist update — with two-artifact failure recovery.**
  `scp docs/ops/jibotmac-tracker-split/com.amplifier.nightly-learning.plist
  jibotmac:Library/LaunchAgents/com.amplifier.nightly-learning.plist`, then
  `ssh jibotmac 'plutil -lint ~/Library/LaunchAgents/com.amplifier.nightly-learning.plist
  && launchctl bootout gui/$(id -u)/com.amplifier.nightly-learning &&
  launchctl bootstrap gui/$(id -u)
  ~/Library/LaunchAgents/com.amplifier.nightly-learning.plist'`.
  **On ANY failure in this step or the next:** restore BOTH artifacts —
  `cp ~/.jilog/backup-6rzb/jilog.toml.orig ~/.jilog.toml && cp
  ~/.jilog/backup-6rzb/com.amplifier.nightly-learning.plist
  ~/Library/LaunchAgents/ && launchctl bootout
  gui/$(id -u)/com.amplifier.nightly-learning; launchctl bootstrap gui/$(id
  -u) ~/Library/LaunchAgents/com.amplifier.nightly-learning.plist` — then
  STOP and reassess (full pre-split coverage restored).
- [ ] **Step 4: Local lane live check (gate f) — two deterministic halves
  (jilog's stdout names no readers, and a zero-new-sessions night is valid,
  so neither "hermes in the log" nor "new digest" is a usable assertion):**
  - (f1) CONFIG-level: `ssh jibotmac '~/.local/bin/jilog --config
    ~/.jilog.toml review nightly --since 7d --digest-dir /tmp/f1-digests
    --processed-file /tmp/f1-processed.txt --dry-run --json; rm -rf
    /tmp/f1-digests /tmp/f1-processed.txt'` — JSON `sessions_scanned` > 0. The
    SCRATCH processed file (nonexistent → empty set) is deliberate: the real
    ledger already contains the hermes sessions, which would make a
    zero-count ambiguous; with an empty ledger the 14+ hermes export chunks
    inside 7 days MUST be discovered. Dry-run writes nothing and contacts no
    tracker, so scratch paths are inert. This proves the slimmed config
    actually discovers hermes transcripts.
  - (f2) JOB-level: record `wc -l` of
    `~/.jilog/logs/nightly-learning.stdout.log`, kickstart
    `com.amplifier.nightly-learning`, wait 10 s BEFORE the first poll (an
    immediate poll can catch launchd before the job spawns and read a stale
    `not running`), then poll `launchctl print gui/$(id
    -u)/com.amplifier.nightly-learning` every 10 s until BOTH `state = not
    running` AND the stdout log line count has INCREASED (deadline 120 s),
    then assert `last exit code = 0`.
- [ ] **Step 5: Isolation gate (b, full).**

```bash
ssh jibotmac 'launchctl print gui/$(id -u)/com.amplifier.nightly-learning | sed -n "/arguments = {/,/}/p"; grep -E "DIGEST_DIR=|PROCESSED=" ~/scripts/jilog-nightly-tracked.sh'
```

  Local arguments show `--digest-dir /Users/jibot/.jilog/digests` +
  `--processed-file …/processed-sessions.txt`; wrapper defaults show
  `~/.amplifier/health` + `processed-sessions-tracked.txt`. All four values
  pairwise distinct — record verbatim in the manifest.
- [ ] **Step 6: Spool-ingest gate (g) — poll to completion, don't race.**
  Kickstart `com.jibot.jilog-spool-ingest`, then poll `launchctl print
  gui/$(id -u)/com.jibot.jilog-spool-ingest` every 10 s (deadline 180 s)
  until `state = not running`; only then assert `last exit code = 0` (a
  30-second sleep can read the PREVIOUS run's exit while this one is still
  running — never sample mid-run).
- [ ] **Step 7: System gate (h) — label SETS + gateway PIDs, as three
  INDEPENDENT commands (never `&&`-chain off the diff: one added label is
  the EXPECTED state, and diff exits 1 on any difference, so a chained gate
  is guaranteed-fail).**

```bash
ssh jibotmac 'launchctl list > /tmp/launchctl-after.txt; diff <(awk "{print \$3}" ~/.jilog/backup-6rzb/launchctl-list-before.txt | grep -E "^com\.jibot\.|^com\.amplifier\.|^com\.joi\.|^ai\.hermes|^com\.cloudflare" | sort) <(awk "{print \$3}" /tmp/launchctl-after.txt | grep -E "^com\.jibot\.|^com\.amplifier\.|^com\.joi\.|^ai\.hermes|^com\.cloudflare" | sort) > /tmp/labelset.diff; true'
ssh jibotmac 'cat /tmp/labelset.diff'
ssh jibotmac 'for l in $(awk "{print \$3}" /tmp/launchctl-after.txt | grep -E "^ai\.hermes|^com\.cloudflare|^com\.jibot\.hermes-"); do echo "$l: before=$(awk -v l="$l" "\$3==l{print \$1}" ~/.jilog/backup-6rzb/launchctl-list-before.txt) after=$(awk -v l="$l" "\$3==l{print \$1}" /tmp/launchctl-after.txt)"; done'
```

  The diff is SCOPED to the label families this task can affect
  (`com.jibot.*`, `com.amplifier.*`, `com.joi.*`, `ai.hermes.*`,
  `com.cloudflare.*`) — transient `com.apple.*`/per-app XPC churn in a GUI
  session is not this gate's fault class and must not false-fail it.
  ASSERT on the captured output: `/tmp/labelset.diff` contains exactly one
  line of additions — `> com.jibot.jilog-nightly-tracked` — and no `<`
  (removal) lines (the canary label was already removed in Task 5); every
  gateway label's before/after PID pair is identical (awk exact `$3==l`
  match — `grep -w` treats hyphens as word boundaries and can double-match
  prefix-extended labels). Then tunnel:
  `nc -z -w 3 127.0.0.1 7777` OK plus one `ssh jibotmac 'source
  ~/.zshrc.local && ~/.local/bin/kata --project jilog --json list --status
  open >/dev/null && echo KATA-OK'`.

---

### Task 7: Sync repo copies to deployed reality + commit (Chunk B end)

- [ ] **Step 1:** Deployed-vs-committed diffs: wrapper, tracked toml, local
  toml, local plist — all byte-identical to repo sources. Tracked plist:
  identical modulo the token value (fetch deployed, replace token value with
  `REDACTED` via plistlib locally, diff semantically with `plutil -convert
  xml1`). Reconcile any drift INTO the repo copies.
- [ ] **Step 2:** Update the run manifest (gate evidence, counts, canary
  issue ref, RUNID) and finalize README.
- [ ] **Step 3: Commit:**

```bash
git add docs/ops/jibotmac-tracker-split docs/superpowers/runs
git commit -m "docs(6rzb): deployed-state sync + verification evidence for jibotmac split"
```

---

### Task 8: Follow-up issues + final commit (Chunk C)

- [ ] **Step 0: Search before filing (tracker contract).** For each intended
  issue run `kata search "<key phrases>" --agent` (also `--project jilog`
  variants); if an existing open issue already covers the defect, comment on
  it instead of filing a duplicate and record that ref — idempotency keys
  only dedupe OUR creates, not issues filed earlier under other titles.
- [ ] **Step 1:** File (or comment on) three kata issues (from THIS Mac,
  `--idempotency-key` each):
  1. project `jilog`: "tracked-run tracker failures are warn-only and
     processed state advances anyway" — body: digest.rs warn-only
     create/list_open + unconditional processed save; wrapper exit-2 grep on
     jibotmac is the interim mitigation; wants fail-loud/retry in the
     pipeline. Key `6rzb-fu-failloud-2026-08-25`. Label `jilog`.
  2. project `jilog`: "kata tracker hardcodes ~/.amplifier/health digest
     path + UTC/Local date mismatch in issue bodies" — body: kata.rs:369
     hardcode; review.rs UTC filename vs kata.rs Local pointer (diverges
     00:00–08:59 JST); jibotmac sidesteps by using the conventional dir.
     Key `6rzb-fu-digestpath-2026-08-25`. Label `jilog`.
  3. project `jibot-code`: "amplifierd WA-DM sessions unscanned while
     macazbd is not the active Mac" — body: the spec's Part-2 ground truth
     (persistence at macazbd:~/.amplifier/projects, verified; covered by
     macazbd config readers; nightly-learning is active-mac-guard-gated,
     active=joimba; options: ungated second local run / guard change /
     accept). Labels `jibot`, `jilog`, `host:macazbd`. Key
     `6rzb-fu-amplifierd-2026-08-25`. Link related 6rzb.
- [ ] **Step 2:** Record the three refs in the run manifest.
- [ ] **Step 3: Final commit (Chunk C is a reviewable diff and the manifest
  edits must not dangle uncommitted):**

```bash
git add docs/superpowers/runs docs/ops/jibotmac-tracker-split
git commit -m "docs(6rzb): follow-up refs + final run-manifest evidence"
```

---

### Task 9: Pipeline close-out (post-review — /do-it Steps 4–5 own this)

Not an Agency task; the pipeline itself executes it AFTER the chunk reviews:

- [ ] /do-it Step 4 two-stage review (Stage 1 subagent ∥ Stage 2 fresheyes,
  heavy loop) over the full branch diff `git diff main...HEAD`; fix or
  rebut every BLOCKER/SUBSTANTIVE; commit fixes.
- [ ] /do-it Step 5: verification gate (`cargo test --workspace`, artifact
  lints), push branch `6rzb-jibotmac-jilog-tracker-flip` to origin (repo is NOT
  marshal-managed; do not merge to main).
- [ ] Close `jibot-code#6rzb` with the full evidence comment (commits,
  config paths + redacted contents, tunnel health output, tracked + local
  run evidence with distinct processed-files/digest-dirs, canary ref +
  RUNID, follow-up refs, review summary naming the fresheyes passes) — the
  close-out comment itself contains NO secrets and NO user text.
- [ ] `/finish-worktree`.

---

## Verification commands (whole-result, pre-commit gate)

Repo side: `cargo test --workspace` (must stay green — the only crates/
change is two machine-contract comments in digest.rs, this proves they
break nothing), `bash -n` + `plutil -lint` over the artifact dir, REDACTED greps
and the tomllib assertion from Task 1 Step 10. Machine side: gates (a)–(h)
evidence recorded in the run manifest (Tasks 2–6).

## Rollback

Exact commands live in the README (Task 1 Step 9). Summary: bootout tracked
agent; delete its plist, wrapper, `~/.jilog-tracked.toml`; restore
`~/.jilog.toml` + local plist from `~/.jilog/backup-6rzb/`
(bootout+bootstrap); trash tracked digests + `processed-sessions-tracked.txt`.
