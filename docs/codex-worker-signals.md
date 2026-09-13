# Codex sessions and worker signals

Implemented for jibot-code#2qet, 2026-09-13.

`CodexReader::from_default()` discovers main sessions and pool profile sessions
on each scan. New profiles need no configuration. `from_roots(Vec<PathBuf>)`
selects explicit roots; `new(path)` remains the single-root compatibility API.
Missing roots are harmless. Overlapping roots and symlinks to the same rollout
are deduplicated by canonical path. Existing rollout session IDs stay unchanged.
The `seat` field is the sessions directory's parent name (`.codex` becomes
`main`). Seat metadata does not enable fleet chat heuristics.

Configure `[[reader]] type = "worker-signals"` in the jilog CLI configuration to
read operational evidence. It uses read-only `kata list --all --status all
--meta dispatch --limit 0 --json` and `kata show <project>#<short_id> --json`, including closed
issues so completed dispatches still contribute failures. Issue update, dispatch, and kickoff timestamps filter the candidate set before
fetching full records. Kata updates updated_at when comments are appended.
Comments are then filtered by their own timestamp and current dispatch start.
Per-source failures warn and preserve other readable evidence.
Only explicit first-line `review: fresheyes --gpt` or `--claude` commands count.
Commands containing both provider flags do not establish which provider ran.

The distinct error tool names are `codex_trust_prompt`, `same_model_review`, and
`codex_fallback_main`. The first two preserve the dispatch ID. Trust failures
require a matching kickoff ID, failed state, and `dialog:dir-trust:` detail.
Silence alone does not establish a trust failure. Each dispatch/kind pair has
one stable processed-session identity and follows existing P3 error filing,
tracker deduplication, and retry behavior.

Main-usage evidence comes from `~/.codex-pool/log/allocation.log`: only `exec`
rows selecting `main` while profile directories exist count, as requested in the
brief. This includes normal scored main selections. The launcher calls its
no-usable-seat path `pass`, a different condition not detected by this rule.
Main-use rows are grouped into one signal per host/local date. The title and
processed identity stay stable as that day gains rows; the first successful
scan records that day. These operational diagnostics never trigger P0 alerts.
The launcher logs
host-local timestamps without a timezone; scan the log on its originating host.
The log does not record dispatch IDs. Such signals explicitly use allocation
host/date identity rather than asserting an unverified dispatch link.
Profile existence is observed at scan time, not proven historically.

Missing-hook detection is deferred. A missing pane file after teardown does not
prove a hook failure; the detector needs a confirmed first turn and a live
session/dispatch identity. It must account for socket-qualified filenames,
ended records, remote hosts, and pane reuse before automatic filing.

The inspected opsctl `review nightly` wrapper constructs only an Amplifier
reader. Updating this jilog library alone does not add Codex readers to that
wrapper. Wiring its readers is an opsctl follow-up; no LaunchAgent is changed.
For direct acceptance, run `jilog --config <config> review nightly` with the two
reader entries above and an isolated digest directory and processed file.

Deferred implementation: jilog#4nd2 (hook state), opsctl#2kpx (nightly wiring).
