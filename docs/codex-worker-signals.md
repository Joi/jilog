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
fetching full records — except for an open codex dispatch, which is always a
candidate, because its missing-hook evidence is a rollout turn that can fall
inside the window long after the issue's own timestamps left it. Kata updates updated_at when comments are appended.
Comments are then filtered by their own timestamp and current dispatch start.
Per-source failures warn and preserve other readable evidence.
Only explicit first-line `review: fresheyes --gpt` or `--claude` commands count.
Commands containing both provider flags do not establish which provider ran.

The distinct error tool names are `codex_trust_prompt`, `same_model_review`,
`codex_fallback_main`, and `codex_missing_hook_state`. Every kind except
`codex_fallback_main` preserves the dispatch ID. Trust failures
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

`codex_missing_hook_state` (jilog#4nd2) reports a dispatched codex pane that
wrote no kata-dispatch hook state, which leaves it invisible to the liveness
watcher and to `--send-only`. A missing file proves nothing by itself, so the
kind is emitted only when every identity condition is confirmed on the scanning
host:

- the dispatch harness is `codex` and its recorded `host` is this machine;
- the issue is still open. A closed issue is a finished dispatch, and the
  liveness watcher deletes the hook state file, its `.ended.*` markers, and the
  default-server copy on every clean disarm — so their absence there is the
  normal end of a dispatch that worked;
- the recorded `(host, tmux_socket, pane)` still belongs to this dispatch id —
  a later dispatch on the same pane number means the pane was reused. A pane
  that is not `%<digits>` is kata-dispatch's phase-one placeholder, not a pane;
- neither socket-qualified name nor, on the default server, the transition copy
  (`<ref-slug>-<socket>-<pane#>.json`, `<ref-slug>-<pane#>.json`) exists;
- no terminal marker (`<stem>.ended.<session>`) exists and the recorded worktree
  is still present — either marks teardown;
- the seat has trusted the dispatch-state hook itself. Its `hooks.json` is
  resolved, the SessionStart entry whose command is exactly
  `<python…> <runner> dispatch-state` is located, and the seat's `config.toml`
  must carry a non-empty `trusted_hash` under that exact
  `[hooks.state."<resolved hooks.json>:session_start:<group>:<hook>"]` key —
  the shape `codex-parity-check` checks. The sibling `startup` hook's trust is
  not this hook's trust; untrusted hooks stop the pane at the hook-review
  dialog, a separate visible condition. `seat: "-"`, the placeholder for a
  dispatch with no pool profile, reads the main Codex home;
- the session kata-dispatch bound at kickoff (`metadata.kickoff.session_observed`,
  when the kickoff belongs to this dispatch) produced a first assistant turn.
  With no bound session, an interactive rollout in the worktree started at or
  after the dispatch counts, but a `codex exec` one never does: it carries no
  dispatch markers. A rescue or `codex resume` in the same worktree is a
  different session and is not this pane's evidence. The turn's timestamp is
  the signal's time;
- the pane is still sitting in the dispatch worktree, by the liveness watcher's
  own probe (`tmux -L <socket> display-message -p -t %<pane>`), reading
  `#{pane_dead}` alongside `#{pane_current_path}` so a pane kept by
  `remain-on-exit` does not read as live. The probe is bounded at 15 seconds:
  an unattended nightly must not be held open by an unresponsive tmux server,
  and a timeout reads as "could not confirm". A server that fails to answer is
  not asked again during that scan, so one unresponsive server costs the scan
  one timeout rather than one per candidate. Directories are compared
  resolved, so a symlinked component is not "the pane left the worktree";
- no hook state file has appeared since the scan opened. Both live checks run
  last, against the world as it is, because both cost a process or a syscall
  and both can retire a finding the snapshot supported. A hook state directory
  that cannot be read at this point counts as present, not absent.

The hook state directory defaults to `KATA_DISPATCH_STATE_DIR`, else
`~/.local/state/kata-dispatch`; an unreadable directory or an unreadable
hostname skips the kind rather than guessing. Rollouts are read from
`<codex_home>/sessions` and every pool profile's `sessions`, first line first,
and only for files modified inside the scan window. Identity is the dispatch id,
like the other dispatch kinds. A pane that has died without writing hook state
is deliberately not reported: the kata asks for a confirmed LIVE identity, and
a dead pane's silence has explanations this reader cannot rule out. The probe
confirms the pane, not the process inside it: `codex-parity-check` and the
liveness watcher go further and check the pane's foreground process, which this
reader does not. Nor is an existing state file parsed — any file or marker under
an expected name suppresses the kind, which can hide a worker whose file is
stale or malformed. Both are deliberate: this rule only ever chooses between
filing and silence, and silence is the safe answer.

The inspected opsctl `review nightly` wrapper constructs only an Amplifier
reader. Updating this jilog library alone does not add Codex readers to that
wrapper. Wiring its readers is an opsctl follow-up; no LaunchAgent is changed.
For direct acceptance, run `jilog --config <config> review nightly` with the two
reader entries above and an isolated digest directory and processed file.

Deferred implementation: opsctl#2kpx (nightly wiring).

Kata's fuzzy title check treats different dispatch IDs or dates with identical
error wording as near duplicates. After exact open/closed-title deduplication,
these four diagnostics use `--force-new` to allow distinct stable identities.
The idempotency key still prevents repeated creation. Ordinary transcript
errors and closed exact-match decisions keep the existing behavior.
