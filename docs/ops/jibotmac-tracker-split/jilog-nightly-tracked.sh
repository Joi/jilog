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
