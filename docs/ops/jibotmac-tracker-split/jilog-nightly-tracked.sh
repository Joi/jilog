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
# Exit codes: 1 preflight failed or timed out (jilog NOT run) / 2 REAL
# tracker errors in output / 3 jilog timeout / 4 jilog itself exited
# nonzero (its rc is in the run log; jilog's own 1/2 would collide with the
# wrapper's contract, so it is never passed through) / 5 ONLY the known
# jilog#fx51 create-parse pattern occurred (issues filed server-side,
# digest lacks backlinks — degraded-but-known, distinct from real trouble).
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

# Malformed overrides must never disable the hard caps (bash 3.2's -ge on a
# non-integer errors and returns false, which would loop forever).
case "$TIMEOUT_SECS" in
    ''|*[!0-9]*) TIMEOUT_SECS=1800 ;;
esac
case "$PREFLIGHT_TIMEOUT_SECS" in
    ''|*[!0-9]*) PREFLIGHT_TIMEOUT_SECS=60 ;;
esac

# Forward termination to the active child process group: launchd's bootout
# only signals THIS process group, and kata/jilog run in their own (that is
# what makes the timeout kills safe), so without a trap they would outlive
# the label and keep writing processed state.
child_pgid=""
cleanup_on_signal() {
    if [ -n "$child_pgid" ]; then
        kill -TERM -- "-$child_pgid" 2>/dev/null
        sleep 2
        kill -KILL -- "-$child_pgid" 2>/dev/null
    fi
    rm -f "${PRE_LOG:-}" "${JOB_LOG:-}" "$RUN_LOG.tmp"
    exit 143
}
trap cleanup_on_signal TERM INT HUP

mkdir -p "$LOG_DIR" "$DIGEST_DIR" "$(dirname "$PROCESSED")"

# Cap the append-only run log; the error-loop case (when this log matters
# most) would otherwise grow it by a full jilog output every night.
if [ -f "$RUN_LOG" ]; then
    { tail -n 5000 "$RUN_LOG" > "$RUN_LOG.tmp" 2>/dev/null && mv "$RUN_LOG.tmp" "$RUN_LOG"; } || rm -f "$RUN_LOG.tmp"
fi

ts() { date -u +%Y-%m-%dT%H:%M:%SZ; }

# --- 1. Preflight (BOUNDED): real daemon round-trip; fail WITHOUT running
# jilog. Runs in its own process group so a tunnel that accepts but never
# answers cannot wedge the label (the daemon-hang case, not just refusal).
# kata --json reports failures on STDOUT, so capture both streams and append
# them on the failure paths — a dead token must leave a diagnostic. ---
PRE_LOG="$(mktemp /tmp/jilog-preflight.XXXXXX)" || { echo "$(ts) mktemp failed; jilog NOT run" >>"$RUN_LOG"; exit 1; }
set -m
"$KATA_BIN" --project jilog --json list --status open >"$PRE_LOG" 2>&1 &
kpid=$!
child_pgid=$kpid
set +m
kwaited=0
while kill -0 "$kpid" 2>/dev/null; do
    if [ "$kwaited" -ge "$PREFLIGHT_TIMEOUT_SECS" ]; then
        cat "$PRE_LOG" >>"$RUN_LOG"
        echo "$(ts) preflight TIMEOUT after ${PREFLIGHT_TIMEOUT_SECS}s; killing kata; jilog NOT run (kata output above)" >>"$RUN_LOG"
        kill -TERM -- "-$kpid" 2>/dev/null
        sleep 2
        kill -KILL -- "-$kpid" 2>/dev/null
        rm -f "$PRE_LOG"
        exit 1
    fi
    sleep 2
    kwaited=$((kwaited + 2))
done
if ! wait "$kpid"; then
    cat "$PRE_LOG" >>"$RUN_LOG"
    echo "$(ts) preflight FAILED; jilog NOT run (kata output above)" >>"$RUN_LOG"
    rm -f "$PRE_LOG"
    exit 1
fi
child_pgid=""
rm -f "$PRE_LOG"

# --- 2. Bounded run, own process group, combined stdout+stderr capture. ---
JOB_LOG="$(mktemp /tmp/jilog-tracked.XXXXXX)" || { echo "$(ts) mktemp failed; jilog NOT run" >>"$RUN_LOG"; exit 1; }
set -m
"$JILOG_BIN" --config "$CONFIG" review nightly \
    --digest-dir "$DIGEST_DIR" \
    --processed-file "$PROCESSED" \
    "$@" >"$JOB_LOG" 2>&1 &
pid=$!
child_pgid=$pid
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
child_pgid=""

# --- 3. Result handling. Order matters: a nonzero jilog exit is a wholesale
# run failure (exit 4, real rc in the log — never passed through, jilog's
# own 1/2 would collide with this wrapper's contract). The tracker-error
# grep applies to completed runs, and the known jilog#fx51 create-response
# parse failure (missing field `number` against kata >=0.15 JSON; the issue
# IS created server-side) is separated from the fail-loud signal so exit 2
# keeps meaning REAL tracker trouble — but it still exits NONZERO (5): a
# night running with the known defect is degraded, never "healthy". ---
cat "$JOB_LOG" >>"$RUN_LOG"
if [ "$rc" -ne 0 ]; then
    echo "$(ts) jilog exited $rc; wrapper exit 4" >>"$RUN_LOG"
    rm -f "$JOB_LOG"
    exit 4
fi
if grep -E 'tracker\.create failed|tracker\.list_open failed' "$JOB_LOG" | grep -Evq 'missing field `number`'; then
    echo "$(ts) tracker errors in output; exit 2 (digest retains signals)" >>"$RUN_LOG"
    rm -f "$JOB_LOG"
    exit 2
fi
known=$(grep -Ec 'tracker\.create failed.*missing field `number`' "$JOB_LOG")
rm -f "$JOB_LOG"
if [ "$known" -gt 0 ]; then
    echo "$(ts) known create-parse defect (jilog#fx51): $known create response(s) unparsed; issues filed server-side, digest lacks backlinks; exit 5" >>"$RUN_LOG"
    exit 5
fi
echo "$(ts) OK" >>"$RUN_LOG"
exit 0
