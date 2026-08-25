# jibotmac digest split — tracked (kata) lane + local (hermes) lane

jibot-code#6rzb, Joi ruling 2026-08-24. Spec:
`docs/superpowers/specs/2026-08-24-jibotmac-tracker-flip-design.md`. Plan (all
verification-gate commands live there):
`docs/superpowers/plans/2026-08-24-jibotmac-tracker-flip.md`.

## The two lanes

| | TRACKED lane | LOCAL lane |
|---|---|---|
| Label | `com.jibot.jilog-nightly-tracked` (23:20) | `com.amplifier.nightly-learning` (22:50) |
| Program | `~/scripts/jilog-nightly-tracked.sh` (wrapper) | `jilog` directly |
| Config | `~/.jilog-tracked.toml` | `~/.jilog.toml` |
| Readers | `claude-code` ONLY | generic `hermes` ONLY |
| Tracker | kata project `jilog` (`--create-issues` once armed) | `none` |
| `--digest-dir` | `/Users/jibot/.amplifier/health` | `/Users/jibot/.jilog/digests` |
| `--processed-file` | `~/.jilog/telemetry/processed-sessions-tracked.txt` | `~/.jilog/telemetry/processed-sessions.txt` |

The privacy boundary is STRUCTURAL: the tracked config carries no
`hermes`/`generic` stanza, so LINE/Telegram/GIDC-email text never enters the
kata-filing pipeline. Rationale and the reviewed rebuttal of a
content-redaction alternative: spec section "Privacy boundary".
`~/.amplifier/health` is the fleet-standard tracked digest dir AND the path
the 0.6.0 kata tracker hardcodes into issue bodies — do not move it.
The two lanes' `--processed-file`/`--digest-dir` values MUST stay distinct
(shared defaults make whichever runs first starve the other).

## Wrapper exit codes (`jilog-nightly-tracked.sh`)

- `1` — kata preflight failed or timed out (`JILOG_TRACKED_PREFLIGHT_TIMEOUT_SECS`,
  default 60 s). jilog did NOT run; no session marked processed; next night
  retries.
- `2` — jilog ran, but its output contained `tracker.create failed` /
  `tracker.list_open failed` (warn-only in jilog). The digest still holds the
  signals; re-file by hand or let the follow-up fix land.
- `3` — jilog exceeded `JILOG_TRACKED_TIMEOUT_SECS` (default 1800 s); its
  process group was killed.
- else — jilog's own exit code. `0` + run log ending `OK` is a healthy night.

Run log: `~/.jilog/logs/nightly-tracked.run.log` (wrapper) plus launchd's
`nightly-tracked.{stdout,stderr}.log`.

## Arming / disarming `--create-issues`

The repo copy of `com.jibot.jilog-nightly-tracked.plist` is the FINAL ARMED
state. Stage-1 install strips the flag (plan Task 2 Step 6); arming re-adds
it (plan Task 5 Step 6). Both use python plistlib on the DEPLOYED file +
`plutil -lint` + tolerant `{ launchctl bootout … 2>/dev/null; true; }` then
`launchctl bootstrap`. To disarm: same plistlib edit removing the flag, then
bootout+bootstrap.

## Deploy notes

- Token: `KATA_AUTH_TOKEN` is injected from `~/.zshrc.local` env (`source`
  then `os.environ` in plistlib — never `grep|cut`, never on a command
  line), written under `umask 077`, atomic rename, file mode 0600.
- launchd needs `~/.jilog/logs` (and the canary's `logs/` dir) to exist
  BEFORE bootstrap — StandardOutPath parents are not created by launchd.
- Any jilog binary bump on this host: bootout+bootstrap BOTH digest labels
  and spool-ingest afterward (stale LWCR gotcha, jilog#jnts).

## Verification

Gates (a)–(h) with exact commands: plan Tasks 2–6. Quick health check:

```bash
ssh jibotmac 'tail -3 ~/.jilog/logs/nightly-tracked.run.log; launchctl list | grep -E "jilog-nightly-tracked|nightly-learning|spool-ingest"'
```

## Rollback (full, exact order)

```bash
ssh jibotmac 'launchctl bootout gui/$(id -u)/com.jibot.jilog-nightly-tracked'
ssh jibotmac 'rm ~/Library/LaunchAgents/com.jibot.jilog-nightly-tracked.plist ~/scripts/jilog-nightly-tracked.sh ~/.jilog-tracked.toml'
ssh jibotmac 'cp ~/.jilog/backup-6rzb/jilog.toml.orig ~/.jilog.toml'
ssh jibotmac 'cp ~/.jilog/backup-6rzb/com.amplifier.nightly-learning.plist ~/Library/LaunchAgents/'
ssh jibotmac 'launchctl bootout gui/$(id -u)/com.amplifier.nightly-learning 2>/dev/null; launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/com.amplifier.nightly-learning.plist'
ssh jibotmac 'trash ~/.amplifier/health ~/.jilog/telemetry/processed-sessions-tracked.txt 2>/dev/null || rm -rf ~/.amplifier/health ~/.jilog/telemetry/processed-sessions-tracked.txt'
```

Disclosure is prevented, not rolled back: issues already filed to kata stay
in tracker history — closing is containment, not erasure (spec "Blast
radius / rollback").
