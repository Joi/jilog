//! `jilog spool` — wire ledger-spool replication for real (kata jilog#546w).
//!
//! Three subcommands turn the fleet-ledger claim from README prose into a
//! running system:
//!
//! - `spool emit`   (every machine): copy THIS host's new sealed segments
//!   from the local zone ledger into the spool's `incoming/` directory.
//!   Cursor-tracked, idempotent, own-host-only.
//! - `spool ingest` (authority machine only): validate, deduplicate, and
//!   commit everything in `incoming/` into the authoritative FLEET store,
//!   moving ingested segments to `processed/` (audit trail).
//! - `spool status` — counts and cursors, for humans and health checks.
//!
//! # Topology (decided 2026-08-18, jilog#546w)
//!
//! The spool and the fleet store both live inside the Syncthing-synced
//! switchboard tree, so Syncthing is the transport — no server, no new
//! daemon. The AUTHORITY (jibotmac: always-on, never travels) is the ONLY
//! writer of the fleet store; every other machine only writes its own
//! host-prefixed segments into `incoming/`. Single-writer discipline is
//! what makes a bidirectionally-synced store safe: two machines never
//! write the same path, so Syncthing has nothing to conflict on.
//!
//! ```text
//! producer (any Mac)                      authority (jibotmac)
//!   local zone ledger  --spool emit-->  spool/<zone>/incoming/
//!        |                                   |   (Syncthing moves the
//!        |                                   |    files between hosts)
//!        v                                   v
//!   (unchanged)                     --spool ingest--> fleet store
//!                                              \--> spool/<zone>/processed/
//! ```
//!
//! # Rust concepts in this file
//!
//! - **Cursor files as plain JSON**: the emit cursor is one small local
//!   file (`~/.jilog/spool-cursors/<zone>-<host>.json`). Losing it is
//!   harmless: emit also skips segments already present in `incoming/`
//!   or `processed/`, and the ingester deduplicates against the store —
//!   three independent layers of idempotency instead of one clever one.
//! - **Shelling out for the hostname**: same choice opsctl made — the
//!   `hostname -s` child process is simpler than a platform crate, and
//!   the source names must MATCH opsctl's segment sources exactly.
//!
//! # Emit correctness model (never lose a segment)
//!
//! Correctness comes from the EVERY-RUN SCAN, not the cursor: each run
//! re-examines ALL own-host segments in the zone ledger. A segment
//! absent from both `incoming/` and `processed/` is (re-)emitted —
//! gaps below any high-water mark are backfilled every run. A spool
//! copy that EXISTS is never taken on faith: emit re-reads every
//! existing copy and compares content — identical means skip,
//! different means a conflict failure (hostname collision or
//! reinitialized ledger), never a silent drop. This costs a read per
//! own-host segment each run; `processed/` is the persistent audit
//! trail it compares against. Limitation: pruning `processed/` makes
//! emit re-spool everything it covered, as backfills — harmless (the
//! ingester's store dedup skips identical content) but noisy.
//!
//! The CURSOR is a recorded high-water mark, not a correctness
//! mechanism: candidates are processed in ascending order and the
//! first failure (unreadable segment, listing error, identity
//! mismatch, spool conflict, or write error) freezes it below the
//! failing seq, so `spool status` shows where replication stalled.
//! Losing or corrupting it costs nothing but the marker.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};
use serde::{Deserialize, Serialize};

use jilog_review::util::expand_tilde;
use ledger_core::{Segment, SegmentStore, ZoneId};
use ledger_spool::{SpoolIngester, SpoolWriter, valid_source_name};
use ledger_sqlite::LedgerDb;

use crate::config::JilogConfig;

#[derive(Args, Debug)]
pub struct SpoolArgs {
    #[command(subcommand)]
    pub cmd: SpoolCmd,
}

#[derive(Subcommand, Debug)]
pub enum SpoolCmd {
    /// Copy this host's new sealed segments into the spool (producer side).
    Emit(EmitArgs),
    /// Ingest spooled segments into the fleet store (authority side).
    Ingest(IngestArgs),
    /// Show spool and cursor state per zone.
    Status(StatusArgs),
}

#[derive(Args, Debug)]
pub struct EmitArgs {
    /// Zone to emit (default: every zone with a resolvable spool path).
    #[arg(long)]
    pub zone: Option<String>,
    /// Source name override (default: `hostname -s`; tests only).
    #[arg(long)]
    pub source: Option<String>,
    /// Cursor directory override (default ~/.jilog/spool-cursors).
    #[arg(long)]
    pub cursor_dir: Option<PathBuf>,
}

#[derive(Args, Debug)]
pub struct IngestArgs {
    /// Zone to ingest (default: every zone with a fleet_store_path).
    #[arg(long)]
    pub zone: Option<String>,
}

#[derive(Args, Debug)]
pub struct StatusArgs {
    /// Zone to show (default: all).
    #[arg(long)]
    pub zone: Option<String>,
    /// Source name override for cursor lookup (default: `hostname -s`).
    #[arg(long)]
    pub source: Option<String>,
    /// Cursor directory override (default ~/.jilog/spool-cursors).
    #[arg(long)]
    pub cursor_dir: Option<PathBuf>,
}

// ---------------------------------------------------------------------------
// Zone resolution
// ---------------------------------------------------------------------------

/// A zone's resolved spool geometry.
struct SpoolZone {
    id: String,
    ledger_path: PathBuf,
    spool_root: PathBuf,
    fleet_store_path: Option<PathBuf>,
    /// From `ZoneConfig::spool` — false marks a read-only mirror zone
    /// (e.g. the synced fleet store) that emit/status must skip.
    spool_enabled: bool,
}

fn resolve_zones(cfg: &JilogConfig, only: &Option<String>) -> Result<Vec<SpoolZone>> {
    let mut out = Vec::new();
    for z in &cfg.zones {
        if let Some(want) = only {
            if &z.id != want {
                continue;
            }
        }
        let ledger_path = expand_tilde(&z.ledger_path);
        // Default spool location: a `spool/<zone-id>` sibling of the zone
        // ledger directory — inside the same synced tree, so the transport
        // comes free.
        let spool_root = match &z.spool_path {
            Some(p) => expand_tilde(p),
            None => ledger_path
                .parent()
                .map(|parent| parent.join("spool").join(&z.id))
                .context("zone ledger_path has no parent directory")?,
        };
        out.push(SpoolZone {
            id: z.id.clone(),
            ledger_path,
            spool_root,
            fleet_store_path: z.fleet_store_path.as_deref().map(expand_tilde),
            spool_enabled: z.spool,
        });
    }
    if out.is_empty() {
        bail!(
            "no zones matched (--zone {:?}, configured: {:?})",
            only,
            cfg.zones.iter().map(|z| &z.id).collect::<Vec<_>>()
        );
    }
    Ok(out)
}

/// Short hostname via `hostname -s`, or None when it cannot be
/// determined. Emit treats None as a hard error (a source name of
/// "unknown" would collide across every misconfigured machine in the
/// fleet); status merely displays "unknown".
fn hostname() -> Option<String> {
    std::process::Command::new("hostname")
        .arg("-s")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

// ---------------------------------------------------------------------------
// Emit (producer)
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Serialize, Deserialize)]
struct EmitCursor {
    /// Highest sequence number already emitted for this (zone, source).
    last_emitted_seq: u64,
}

fn cursor_path(dir: &Path, zone: &str, source: &str) -> PathBuf {
    dir.join(format!("{zone}-{source}.json"))
}

fn load_cursor(path: &Path) -> EmitCursor {
    // A missing or unreadable cursor is NOT an error: emit re-checks the
    // spool itself, so the worst case is a slower (not wrong) run.
    fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn store_cursor(path: &Path, cur: &EmitCursor) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, serde_json::to_string_pretty(cur)?)?;
    fs::rename(&tmp, path)?;
    Ok(())
}

pub fn run_emit(cfg: &JilogConfig, args: EmitArgs) -> Result<()> {
    let source = match args.source.clone() {
        Some(s) => s,
        // Hard error, never a silent "unknown": an "unknown" source
        // would collide across every misconfigured machine in the fleet.
        None => hostname().context(
            "cannot determine this host's name (`hostname -s` failed or returned \
             empty); refusing to emit — pass --source explicitly",
        )?,
    };
    if !valid_source_name(&source) {
        bail!(
            "invalid source name {source:?}: must match \
             ^[A-Za-z0-9][A-Za-z0-9._-]{{0,63}}$ (the ingester rejects anything else)"
        );
    }
    let cursor_dir = args
        .cursor_dir
        .clone()
        .unwrap_or_else(|| expand_tilde("~/.jilog/spool-cursors"));

    let mut had_failures = false;
    for zone in resolve_zones(cfg, &args.zone)? {
        if !zone.spool_enabled {
            // Read-only mirror zone (spool = false): never re-emit an
            // already-replicated store into an orphan spool.
            continue;
        }
        let store = SegmentStore::new(ZoneId::new(&zone.id), &zone.ledger_path);
        let writer = SpoolWriter::new(&zone.spool_root);
        let incoming_dir = zone.spool_root.join("incoming");
        let processed_dir = zone.spool_root.join("processed");
        let cpath = cursor_path(&cursor_dir, &zone.id, &source);
        let mut cursor = load_cursor(&cpath);

        // Own-host segments only: every machine is the sole authority on
        // its own history, and this is what keeps a shared, synced
        // incoming/ collision-free by construction.
        //
        // EVERY own-host segment is a candidate — above the cursor
        // (new) and at/below it (potential backfill). An existing spool
        // copy is never treated as proof of replication by name alone:
        // it is re-read and content-compared below, so a hostname
        // collision or a reinitialized local ledger surfaces as a
        // conflict instead of a silent drop. Cost: one read per
        // own-host segment per run — correctness over cheap stats.
        // (Pruning processed/ re-spools its segments as backfills:
        // harmless — ingester dedup skips identical content — but noisy.)
        let (entries, list_errors) = store
            .list_segments_with_errors()
            .with_context(|| format!("list segments in {}", zone.ledger_path.display()))?;
        let mut candidates: Vec<u64> = entries
            .into_iter()
            .filter(|(src, _, _)| src == &source)
            .map(|(_, seq, _)| seq)
            .collect();
        candidates.sort_unstable();

        let mut emitted = 0usize;
        let mut skipped = 0usize;
        // The cursor advances only through CONTIGUOUSLY-successful
        // sequences: the first failure freezes it below the failing seq,
        // so that segment is retried next run. Later candidates are
        // still processed (best-effort batch, same as the ingester).
        //
        // An unreadable or unparseable ledger entry could be ANY seq of
        // ours, so it freezes the cursor for the whole run (advance
        // starts false) — the contiguity rule with unknown position.
        let mut advance = list_errors.is_empty();
        for err in &list_errors {
            eprintln!("spool emit [{}]: {err}", zone.id);
            had_failures = true;
        }
        let fail = |msg: String, advance: &mut bool, had_failures: &mut bool| {
            eprintln!("{msg}");
            *had_failures = true;
            *advance = false;
        };
        for seq in candidates {
            let segment = match store.read_segment(&source, seq) {
                Ok(s) => s,
                Err(e) => {
                    fail(
                        format!("spool emit: {source}-{seq:06} unreadable: {e}"),
                        &mut advance,
                        &mut had_failures,
                    );
                    continue;
                }
            };
            // The DESERIALIZED segment must be what its filename claims:
            // a valid source name, THIS host, this seq. The filename —
            // and thus every destination path — derives from these
            // fields, so a hostile `source` inside an own-host-named
            // file must die here, before any path is constructed.
            if !valid_source_name(&segment.source)
                || segment.source != source
                || segment.source_seq != seq
            {
                fail(
                    format!(
                        "spool emit: {source}-{seq:06} identity mismatch: file claims \
                         source={:?} seq={} — refusing to spool",
                        segment.source, segment.source_seq
                    ),
                    &mut advance,
                    &mut had_failures,
                );
                continue;
            }
            // Never spool a corrupt segment: verify the checksum here,
            // at the source, instead of letting the ingester discover it
            // fleet-wide later.
            match segment.verify() {
                Ok(true) => {}
                Ok(false) => {
                    fail(
                        format!(
                            "spool emit: {source}-{seq:06} fails checksum verification — \
                             refusing to spool corrupt segment"
                        ),
                        &mut advance,
                        &mut had_failures,
                    );
                    continue;
                }
                Err(e) => {
                    fail(
                        format!("spool emit: {source}-{seq:06} verify error: {e}"),
                        &mut advance,
                        &mut had_failures,
                    );
                    continue;
                }
            }
            let fname = segment.filename();
            // Existing spool copies? Compare CONTENT of EVERY one that
            // exists — processed/ AND incoming/ — don't trust names,
            // and don't let an identical copy in one dir vouch for a
            // conflicting or unreadable copy in the other.
            let copies: Vec<PathBuf> = [processed_dir.join(&fname), incoming_dir.join(&fname)]
                .into_iter()
                .filter(|p| p.exists())
                .collect();
            if copies.is_empty() {
                // Count only real writes; a write failure is recorded and
                // the batch continues (later segments and zones still run).
                // The writer's no-clobber publish can also report an
                // identical copy that appeared in the race window — that
                // is a skip, not an emit.
                match writer.write(&segment) {
                    Ok((_, ledger_core::PublishOutcome::Published)) => emitted += 1,
                    Ok((_, ledger_core::PublishOutcome::AlreadyIdentical)) => skipped += 1,
                    Err(e) => {
                        fail(
                            format!("spool emit: spool-write {fname} failed: {e}"),
                            &mut advance,
                            &mut had_failures,
                        );
                        continue;
                    }
                }
            } else {
                let mut bad = false;
                for spool_copy in &copies {
                    match Segment::read_from_file(spool_copy) {
                        Ok(prior) if prior.content_matches(&segment) => {}
                        Ok(_) => {
                            fail(
                                format!(
                                    "spool emit: {} conflicts with local {fname}: same \
                                     identity, DIFFERENT content — possible hostname \
                                     collision or reinitialized ledger; not overwriting",
                                    spool_copy.display()
                                ),
                                &mut advance,
                                &mut had_failures,
                            );
                            bad = true;
                        }
                        Err(e) => {
                            fail(
                                format!(
                                    "spool emit: spool copy {} unreadable ({e}); refusing \
                                     to assume it matches local {fname}",
                                    spool_copy.display()
                                ),
                                &mut advance,
                                &mut had_failures,
                            );
                            bad = true;
                        }
                    }
                }
                if bad {
                    continue;
                }
                skipped += 1;
            }
            if advance && seq > cursor.last_emitted_seq {
                cursor.last_emitted_seq = seq;
            }
        }
        store_cursor(&cpath, &cursor)?;
        println!(
            "spool emit [{}]: source={} emitted={} skipped={} cursor={}",
            zone.id, source, emitted, skipped, cursor.last_emitted_seq
        );
    }
    if had_failures {
        bail!("spool emit finished with per-segment failures (see stderr)");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Ingest (authority)
// ---------------------------------------------------------------------------

pub fn run_ingest(cfg: &JilogConfig, args: IngestArgs) -> Result<()> {
    let mut ran_any = false;
    let mut had_failures = false;
    for zone in resolve_zones(cfg, &args.zone)? {
        let Some(fleet_path) = &zone.fleet_store_path else {
            // Producers legitimately have no fleet_store_path; only the
            // authority configures one. Skipping silently would hide a
            // misconfigured authority though — say what happened.
            println!(
                "spool ingest [{}]: no fleet_store_path configured — skipping (producer host?)",
                zone.id
            );
            continue;
        };
        ran_any = true;
        let store = SegmentStore::new(ZoneId::new(&zone.id), fleet_path);
        store
            .ensure_dirs()
            .with_context(|| format!("create fleet store at {}", fleet_path.display()))?;
        let report = SpoolIngester::new(&zone.spool_root)
            .ingest(&store)
            .with_context(|| format!("ingest spool {}", zone.spool_root.display()))?;
        print!("spool ingest [{}]: ", zone.id);
        report.print_summary();
        if !report.failed.is_empty() {
            had_failures = true;
        }
        // Keep the fleet store queryable WITHOUT ever writing SQLite
        // into the Syncthing-synced tree (a background sync of a
        // mid-transaction db ships corruption). The index lives at the
        // MIRROR zone's local index_db_path (spool = false zone whose
        // ledger_path is this fleet store); if no mirror zone is
        // configured here, each consumer's `jilog query` builds its own
        // local index lazily instead.
        let mirror = cfg
            .zones
            .iter()
            .find(|zc| !zc.spool && expand_tilde(&zc.ledger_path) == *fleet_path);
        match mirror {
            Some(zc) => {
                let db_path = zc.index_db_path();
                if let Some(parent) = db_path.parent() {
                    fs::create_dir_all(parent)
                        .with_context(|| format!("create index dir {}", parent.display()))?;
                }
                let refresh = LedgerDb::open(&db_path)
                    .and_then(|mut db| db.refresh_from_store(&store))
                    .with_context(|| format!("refresh fleet index at {}", db_path.display()))?;
                if refresh.events_indexed > 0 {
                    println!(
                        "spool ingest [{}]: indexed {} new event(s) into {}",
                        zone.id,
                        refresh.events_indexed,
                        db_path.display()
                    );
                }
                for (src, seq, err) in &refresh.failed {
                    eprintln!(
                        "spool ingest [{}]: index skipped corrupt segment {src}-{seq:06}: {err}",
                        zone.id
                    );
                    had_failures = true;
                }
            }
            None => println!(
                "spool ingest [{}]: no spool=false mirror [[zone]] for {} — index refresh \
                 left to each consumer's `jilog query`",
                zone.id,
                fleet_path.display()
            ),
        }
    }
    if !ran_any {
        bail!(
            "no zone has a fleet_store_path — this host is not configured as the \
             spool authority (set fleet_store_path on the zone in ~/.jilog.toml)"
        );
    }
    if had_failures {
        bail!("spool ingest finished with failed segments (left in incoming/)");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Status
// ---------------------------------------------------------------------------

pub fn run_status(cfg: &JilogConfig, args: StatusArgs) -> Result<()> {
    let cursor_dir = args
        .cursor_dir
        .clone()
        .unwrap_or_else(|| expand_tilde("~/.jilog/spool-cursors"));
    // An unknown hostname means the cursor CANNOT be observed — that is
    // a status failure for every enabled zone, never a healthy zero.
    let host = args.source.clone().or_else(hostname);
    // Status is a health check: anything it cannot actually observe
    // (unreadable spool dirs or entries, a corrupt cursor, an
    // undeterminable hostname) makes the command exit nonzero after
    // printing every zone.
    let mut unhealthy: Vec<String> = Vec::new();
    for zone in resolve_zones(cfg, &args.zone)? {
        if !zone.spool_enabled {
            println!("spool status [{}]: spool disabled", zone.id);
            continue;
        }
        // A missing directory is an honestly-empty spool ("0"); any other
        // read_dir error must not masquerade as zero, and per-entry
        // errors are counted rather than silently discarded. Returns the
        // display string plus whether the count is trustworthy.
        let count = |d: &str| -> (String, bool) {
            match fs::read_dir(zone.spool_root.join(d)) {
                Ok(rd) => {
                    let mut n = 0usize;
                    let mut bad = 0usize;
                    for entry in rd {
                        match entry {
                            Ok(e) if e.path().extension().is_some_and(|x| x == "json") => n += 1,
                            Ok(_) => {}
                            Err(_) => bad += 1,
                        }
                    }
                    if bad > 0 {
                        (format!("{n} (unreadable entries: {bad})"), false)
                    } else {
                        (n.to_string(), true)
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => ("0".to_string(), true),
                Err(_) => ("unreadable".to_string(), false),
            }
        };
        // Distinguish "no cursor yet" (0) from a corrupt/unreadable one —
        // emit would silently start over from 0; status should say so.
        // No hostname at all means the cursor cannot even be looked up.
        let (host_label, cursor_display, cursor_ok) = match &host {
            None => (
                "unknown".to_string(),
                "cannot determine own hostname — pass --source".to_string(),
                false,
            ),
            Some(h) => {
                let cpath = cursor_path(&cursor_dir, &zone.id, h);
                let (display, ok) = if cpath.exists() {
                    match fs::read_to_string(&cpath)
                        .ok()
                        .and_then(|s| serde_json::from_str::<EmitCursor>(&s).ok())
                    {
                        Some(c) => (c.last_emitted_seq.to_string(), true),
                        None => (
                            "unreadable/corrupt (emit will restart from 0)".to_string(),
                            false,
                        ),
                    }
                } else {
                    ("0".to_string(), true)
                };
                (h.clone(), display, ok)
            }
        };
        let (incoming_display, incoming_ok) = count("incoming");
        let (processed_display, processed_ok) = count("processed");
        println!(
            "spool status [{}]: incoming={} processed={} cursor[{}]={} fleet_store={}",
            zone.id,
            incoming_display,
            processed_display,
            host_label,
            cursor_display,
            zone.fleet_store_path
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "(none — producer host)".into()),
        );
        if !(incoming_ok && processed_ok && cursor_ok) {
            unhealthy.push(zone.id.clone());
        }
    }
    if !unhealthy.is_empty() {
        bail!(
            "spool status found problems (unreadable dirs/entries, corrupt cursor, or \
             unknown hostname) in zone(s): {}",
            unhealthy.join(", ")
        );
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use ledger_core::Segment;
    use crate::config::ZoneConfig;
    use chrono::Utc;
    use ledger_core::{Event, EventClass, PayloadTier};
    use uuid::Uuid;

    fn sealed_segment(source: &str, seq: u64, n_events: usize) -> Segment {
        let mut seg = Segment::new(source, seq);
        for i in 0..n_events {
            seg.append(Event {
                event_id: Uuid::now_v7(),
                zone: "test-zone".to_string(),
                source: source.to_string(),
                source_seq: seq,
                timestamp: Utc::now(),
                correlation_id: None,
                causation_id: None,
                actor_ref: Some(format!("test-{i}")),
                object_ref: None,
                event_class: EventClass::Health,
                payload_tier: PayloadTier::MetadataOnly,
                payload: None,
            });
        }
        seg.seal().unwrap();
        seg
    }

    fn cfg_for(dir: &Path, fleet: Option<&Path>) -> JilogConfig {
        let mut zones = vec![ZoneConfig {
            id: "test-zone".into(),
            ledger_path: dir.join("ledger").display().to_string(),
            spool: true,
            spool_path: Some(dir.join("spool").display().to_string()),
            fleet_store_path: fleet.map(|f| f.display().to_string()),
            index_path: None,
        }];
        if let Some(f) = fleet {
            // The fleet store mounted as a read-only mirror zone, the way
            // a real config does it — spool disabled, LOCAL index path.
            zones.push(ZoneConfig {
                id: "test-zone-fleet".into(),
                ledger_path: f.display().to_string(),
                spool: false,
                spool_path: None,
                fleet_store_path: None,
                index_path: Some(dir.join("fleet-index.sqlite").display().to_string()),
            });
        }
        JilogConfig {
            zones,
            ..Default::default()
        }
    }

    #[test]
    fn emit_then_ingest_round_trip_with_dedup() {
        let tmp = tempfile::tempdir().unwrap();
        let fleet = tmp.path().join("fleet");
        let cfg = cfg_for(tmp.path(), Some(&fleet));

        // Producer's local ledger gets two sealed segments.
        let local = SegmentStore::new(ZoneId::new("test-zone"), tmp.path().join("ledger"));
        local.ensure_dirs().unwrap();
        local.write_segment(&sealed_segment("hostA", 1, 2)).unwrap();
        local.write_segment(&sealed_segment("hostA", 2, 1)).unwrap();

        let cursors = tmp.path().join("cursors");
        run_emit(
            &cfg,
            EmitArgs {
                zone: None,
                source: Some("hostA".into()),
                cursor_dir: Some(cursors.clone()),
            },
        )
        .unwrap();
        // Both segments in incoming/.
        assert_eq!(
            fs::read_dir(tmp.path().join("spool/incoming")).unwrap().count(),
            2
        );

        run_ingest(&cfg, IngestArgs { zone: None }).unwrap();
        let fleet_store = SegmentStore::new(ZoneId::new("test-zone"), &fleet);
        assert_eq!(fleet_store.list_segments().unwrap().len(), 2);
        // Audit trail: incoming drained, processed holds both.
        assert_eq!(
            fs::read_dir(tmp.path().join("spool/incoming")).unwrap().count(),
            0
        );
        assert_eq!(
            fs::read_dir(tmp.path().join("spool/processed")).unwrap().count(),
            2
        );
        // jilog query reads only the mirror zone's LOCAL index — ingest
        // must have refreshed it so committed events are query-visible,
        // and NO sqlite file may land inside the synced fleet tree.
        let db = ledger_sqlite::LedgerDb::open(tmp.path().join("fleet-index.sqlite")).unwrap();
        assert_eq!(db.event_count().unwrap(), 3, "fleet index holds all events");
        assert_eq!(db.segment_count().unwrap(), 2);
        drop(db);
        assert!(
            !fleet.join("index.sqlite").exists(),
            "index.sqlite must NOT be written inside the synced fleet store"
        );

        // Re-emit with a WIPED cursor: the processed/ belt prevents churn.
        fs::remove_dir_all(&cursors).unwrap();
        run_emit(
            &cfg,
            EmitArgs {
                zone: None,
                source: Some("hostA".into()),
                cursor_dir: Some(cursors.clone()),
            },
        )
        .unwrap();
        assert_eq!(
            fs::read_dir(tmp.path().join("spool/incoming")).unwrap().count(),
            0,
            "cursor loss must not re-spool already-processed segments"
        );

        // An IDENTICAL duplicate that lands in incoming/ again is skipped
        // by the ingester's store-level dedup, not committed twice.
        let writer = SpoolWriter::new(tmp.path().join("spool"));
        writer.write(&local.read_segment("hostA", 1).unwrap()).unwrap();
        run_ingest(&cfg, IngestArgs { zone: None }).unwrap();
        assert_eq!(fleet_store.list_segments().unwrap().len(), 2);
        let db = ledger_sqlite::LedgerDb::open(tmp.path().join("fleet-index.sqlite")).unwrap();
        assert_eq!(db.event_count().unwrap(), 3, "index unchanged after dedup");
    }

    #[test]
    fn emit_only_own_source_segments() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = cfg_for(tmp.path(), None);
        let local = SegmentStore::new(ZoneId::new("test-zone"), tmp.path().join("ledger"));
        local.ensure_dirs().unwrap();
        local.write_segment(&sealed_segment("hostA", 1, 1)).unwrap();
        local.write_segment(&sealed_segment("hostB", 1, 1)).unwrap();

        run_emit(
            &cfg,
            EmitArgs {
                zone: None,
                source: Some("hostA".into()),
                cursor_dir: Some(tmp.path().join("cursors")),
            },
        )
        .unwrap();
        let names: Vec<String> = fs::read_dir(tmp.path().join("spool/incoming"))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names.len(), 1);
        assert!(names[0].starts_with("hostA-"), "{names:?}");
    }

    #[test]
    fn ingest_without_fleet_store_is_a_loud_error() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = cfg_for(tmp.path(), None);
        let err = run_ingest(&cfg, IngestArgs { zone: None }).unwrap_err();
        assert!(err.to_string().contains("not configured as the spool authority"));
    }

    #[test]
    fn emit_is_incremental_and_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = cfg_for(tmp.path(), None);
        let local = SegmentStore::new(ZoneId::new("test-zone"), tmp.path().join("ledger"));
        local.ensure_dirs().unwrap();
        local.write_segment(&sealed_segment("hostA", 1, 1)).unwrap();
        let cursors = tmp.path().join("cursors");
        let args = || EmitArgs {
            zone: None,
            source: Some("hostA".into()),
            cursor_dir: Some(cursors.clone()),
        };
        run_emit(&cfg, args()).unwrap();
        assert_eq!(
            fs::read_dir(tmp.path().join("spool/incoming")).unwrap().count(),
            1
        );
        // Re-run with nothing new: segment 1 still sits in incoming/, so
        // nothing is duplicated or re-written.
        run_emit(&cfg, args()).unwrap();
        assert_eq!(
            fs::read_dir(tmp.path().join("spool/incoming")).unwrap().count(),
            1
        );
        // New segment still flows.
        local.write_segment(&sealed_segment("hostA", 2, 1)).unwrap();
        run_emit(&cfg, args()).unwrap();
        assert_eq!(
            fs::read_dir(tmp.path().join("spool/incoming")).unwrap().count(),
            2
        );
    }

    #[test]
    fn emit_read_failure_keeps_cursor_below_failure() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = cfg_for(tmp.path(), None);
        let local = SegmentStore::new(ZoneId::new("test-zone"), tmp.path().join("ledger"));
        local.ensure_dirs().unwrap();
        local.write_segment(&sealed_segment("hostA", 1, 1)).unwrap();
        local.write_segment(&sealed_segment("hostA", 2, 1)).unwrap();
        local.write_segment(&sealed_segment("hostA", 3, 1)).unwrap();
        // Corrupt segment 2 so read_segment fails mid-sequence.
        let seg2_path = tmp.path().join("ledger/segments/hostA-000002.json");
        fs::write(&seg2_path, "not json {").unwrap();

        let cursors = tmp.path().join("cursors");
        let args = || EmitArgs {
            zone: None,
            source: Some("hostA".into()),
            cursor_dir: Some(cursors.clone()),
        };
        let err = run_emit(&cfg, args()).unwrap_err();
        assert!(err.to_string().contains("per-segment failures"), "{err}");

        // 1 and 3 were still emitted (best-effort batch)...
        assert_eq!(
            fs::read_dir(tmp.path().join("spool/incoming")).unwrap().count(),
            2
        );
        // ...but the cursor froze BELOW the failure so 2 retries next run.
        let cursor = load_cursor(&cursor_path(&cursors, "test-zone", "hostA"));
        assert_eq!(cursor.last_emitted_seq, 1, "cursor must stay below the failed seq");

        // Repair segment 2 and re-run: it flows, cursor catches up.
        sealed_segment("hostA", 2, 1).write_to_file(&seg2_path).unwrap();
        run_emit(&cfg, args()).unwrap();
        assert_eq!(
            fs::read_dir(tmp.path().join("spool/incoming")).unwrap().count(),
            3
        );
        let cursor = load_cursor(&cursor_path(&cursors, "test-zone", "hostA"));
        assert_eq!(cursor.last_emitted_seq, 3);
    }

    #[test]
    fn emit_backfills_lower_seq_despite_higher_cursor() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = cfg_for(tmp.path(), None);
        let local = SegmentStore::new(ZoneId::new("test-zone"), tmp.path().join("ledger"));
        local.ensure_dirs().unwrap();
        local.write_segment(&sealed_segment("hostA", 1, 1)).unwrap();
        local.write_segment(&sealed_segment("hostA", 2, 1)).unwrap();
        let cursors = tmp.path().join("cursors");
        let args = || EmitArgs {
            zone: None,
            source: Some("hostA".into()),
            cursor_dir: Some(cursors.clone()),
        };
        run_emit(&cfg, args()).unwrap();
        let cursor = load_cursor(&cursor_path(&cursors, "test-zone", "hostA"));
        assert_eq!(cursor.last_emitted_seq, 2);

        // Segment 1 vanishes from incoming/ with NO processed/ record —
        // from the spool's perspective it was lost, and the cursor alone
        // must not be able to lose it.
        fs::remove_file(tmp.path().join("spool/incoming/hostA-000001.json")).unwrap();
        run_emit(&cfg, args()).unwrap();
        assert!(
            tmp.path().join("spool/incoming/hostA-000001.json").exists(),
            "backfill must re-emit a lower seq despite the higher cursor"
        );
        // Cursor unchanged (still at the high-water mark).
        let cursor = load_cursor(&cursor_path(&cursors, "test-zone", "hostA"));
        assert_eq!(cursor.last_emitted_seq, 2);
    }

    #[test]
    fn emit_skips_spool_disabled_zone() {
        let tmp = tempfile::tempdir().unwrap();
        let mut cfg = cfg_for(tmp.path(), None);
        cfg.zones[0].spool = false;
        let local = SegmentStore::new(ZoneId::new("test-zone"), tmp.path().join("ledger"));
        local.ensure_dirs().unwrap();
        local.write_segment(&sealed_segment("hostA", 1, 1)).unwrap();

        run_emit(
            &cfg,
            EmitArgs {
                zone: None,
                source: Some("hostA".into()),
                cursor_dir: Some(tmp.path().join("cursors")),
            },
        )
        .unwrap();
        assert!(
            !tmp.path().join("spool/incoming").exists(),
            "spool = false zone must not be emitted"
        );
    }

    #[test]
    fn emit_rejects_mislabeled_segment_file() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = cfg_for(tmp.path(), None);
        // An own-host-NAMED file whose deserialized source is a path
        // traversal attempt: must be rejected before any destination
        // path is built, nothing spooled anywhere.
        let segments_dir = tmp.path().join("ledger/segments");
        fs::create_dir_all(&segments_dir).unwrap();
        let evil = sealed_segment("../../evil", 1, 1);
        fs::write(
            segments_dir.join("hostA-000001.json"),
            serde_json::to_string_pretty(&evil).unwrap(),
        )
        .unwrap();

        let err = run_emit(
            &cfg,
            EmitArgs {
                zone: None,
                source: Some("hostA".into()),
                cursor_dir: Some(tmp.path().join("cursors")),
            },
        )
        .unwrap_err();
        assert!(err.to_string().contains("per-segment failures"), "{err}");
        let incoming = tmp.path().join("spool/incoming");
        assert!(
            !incoming.exists() || fs::read_dir(&incoming).unwrap().count() == 0,
            "mislabeled segment must not be spooled"
        );
        // Cursor never advanced past the rejected segment.
        let cursor = load_cursor(&cursor_path(
            &tmp.path().join("cursors"),
            "test-zone",
            "hostA",
        ));
        assert_eq!(cursor.last_emitted_seq, 0);
    }

    #[test]
    fn emit_conflicting_spool_copy_is_failure_not_skip() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = cfg_for(tmp.path(), None);
        let local = SegmentStore::new(ZoneId::new("test-zone"), tmp.path().join("ledger"));
        local.ensure_dirs().unwrap();
        local.write_segment(&sealed_segment("hostA", 1, 1)).unwrap();
        let cursors = tmp.path().join("cursors");
        let args = || EmitArgs {
            zone: None,
            source: Some("hostA".into()),
            cursor_dir: Some(cursors.clone()),
        };
        run_emit(&cfg, args()).unwrap();

        // Same identity, DIFFERENT content lands in incoming/ (hostname
        // collision / reinitialized ledger simulation).
        let planted = sealed_segment("hostA", 1, 3);
        planted
            .write_to_file(tmp.path().join("spool/incoming/hostA-000001.json"))
            .unwrap();

        let err = run_emit(&cfg, args()).unwrap_err();
        assert!(err.to_string().contains("per-segment failures"), "{err}");
        // The planted copy was NOT silently overwritten or dropped.
        let on_disk =
            Segment::read_from_file(tmp.path().join("spool/incoming/hostA-000001.json"))
                .unwrap();
        assert!(on_disk.content_matches(&planted), "conflict must not overwrite");

        // Identical copy (the local segment itself) skips cleanly again.
        local
            .read_segment("hostA", 1)
            .unwrap()
            .write_to_file(tmp.path().join("spool/incoming/hostA-000001.json"))
            .unwrap();
        run_emit(&cfg, args()).unwrap();
    }

    #[test]
    fn emit_conflicting_incoming_fails_even_with_identical_processed_copy() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = cfg_for(tmp.path(), None);
        let local = SegmentStore::new(ZoneId::new("test-zone"), tmp.path().join("ledger"));
        local.ensure_dirs().unwrap();
        local.write_segment(&sealed_segment("hostA", 1, 1)).unwrap();
        let cursors = tmp.path().join("cursors");
        let args = || EmitArgs {
            zone: None,
            source: Some("hostA".into()),
            cursor_dir: Some(cursors.clone()),
        };
        run_emit(&cfg, args()).unwrap();

        // processed/ holds the IDENTICAL copy (simulated ingest)...
        let good = local.read_segment("hostA", 1).unwrap();
        good.write_to_file(tmp.path().join("spool/processed/hostA-000001.json"))
            .unwrap();
        fs::remove_file(tmp.path().join("spool/incoming/hostA-000001.json")).unwrap();
        // ...and a CONFLICTING copy lands in incoming/ (collision).
        let planted = sealed_segment("hostA", 1, 3);
        planted
            .write_to_file(tmp.path().join("spool/incoming/hostA-000001.json"))
            .unwrap();

        // The identical processed/ copy must NOT vouch for the
        // conflicting incoming/ copy: the run fails.
        let err = run_emit(&cfg, args()).unwrap_err();
        assert!(err.to_string().contains("per-segment failures"), "{err}");
        // Neither copy was altered.
        let processed_on_disk =
            Segment::read_from_file(tmp.path().join("spool/processed/hostA-000001.json"))
                .unwrap();
        assert!(processed_on_disk.content_matches(&good));
        let incoming_on_disk =
            Segment::read_from_file(tmp.path().join("spool/incoming/hostA-000001.json"))
                .unwrap();
        assert!(incoming_on_disk.content_matches(&planted));
    }

    #[cfg(unix)]
    #[test]
    fn emit_write_failure_is_recorded_and_batch_continues() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().unwrap();
        let cfg = cfg_for(tmp.path(), None);
        let local = SegmentStore::new(ZoneId::new("test-zone"), tmp.path().join("ledger"));
        local.ensure_dirs().unwrap();
        local.write_segment(&sealed_segment("hostA", 1, 1)).unwrap();
        local.write_segment(&sealed_segment("hostA", 2, 1)).unwrap();

        // Read-only incoming/ makes every spool write fail.
        let incoming = tmp.path().join("spool/incoming");
        fs::create_dir_all(&incoming).unwrap();
        fs::set_permissions(&incoming, fs::Permissions::from_mode(0o555)).unwrap();

        let cursors = tmp.path().join("cursors");
        let args = || EmitArgs {
            zone: None,
            source: Some("hostA".into()),
            cursor_dir: Some(cursors.clone()),
        };
        // Best-effort: the run completes (nonzero via error), does not
        // abort on the first write failure, and stores a frozen cursor.
        let err = run_emit(&cfg, args()).unwrap_err();
        assert!(err.to_string().contains("per-segment failures"), "{err}");
        let cursor = load_cursor(&cursor_path(&cursors, "test-zone", "hostA"));
        assert_eq!(cursor.last_emitted_seq, 0, "cursor must not advance past failed writes");

        // Recovery: writable again, everything flows.
        fs::set_permissions(&incoming, fs::Permissions::from_mode(0o755)).unwrap();
        run_emit(&cfg, args()).unwrap();
        assert_eq!(fs::read_dir(&incoming).unwrap().count(), 2);
        let cursor = load_cursor(&cursor_path(&cursors, "test-zone", "hostA"));
        assert_eq!(cursor.last_emitted_seq, 2);
    }

    #[test]
    fn emit_surfaces_ledger_listing_errors_and_freezes_cursor() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = cfg_for(tmp.path(), None);
        let local = SegmentStore::new(ZoneId::new("test-zone"), tmp.path().join("ledger"));
        local.ensure_dirs().unwrap();
        local.write_segment(&sealed_segment("hostA", 1, 1)).unwrap();
        // A .json file in segments/ whose name doesn't parse: could be
        // ANY seq of ours, so the cursor must freeze for the whole run.
        fs::write(tmp.path().join("ledger/segments/garbage.json"), "{}").unwrap();

        let cursors = tmp.path().join("cursors");
        let args = || EmitArgs {
            zone: None,
            source: Some("hostA".into()),
            cursor_dir: Some(cursors.clone()),
        };
        let err = run_emit(&cfg, args()).unwrap_err();
        assert!(err.to_string().contains("per-segment failures"), "{err}");
        // The readable segment still emitted (best-effort)...
        assert_eq!(
            fs::read_dir(tmp.path().join("spool/incoming")).unwrap().count(),
            1
        );
        // ...but the cursor did not advance past the unknown entry.
        let cursor = load_cursor(&cursor_path(&cursors, "test-zone", "hostA"));
        assert_eq!(cursor.last_emitted_seq, 0, "listing errors must freeze the cursor");

        // Remove the garbage: next run is clean and the cursor catches up.
        fs::remove_file(tmp.path().join("ledger/segments/garbage.json")).unwrap();
        run_emit(&cfg, args()).unwrap();
        let cursor = load_cursor(&cursor_path(&cursors, "test-zone", "hostA"));
        assert_eq!(cursor.last_emitted_seq, 1);
    }

    #[test]
    fn emit_refuses_corrupt_local_segment() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = cfg_for(tmp.path(), None);
        let local = SegmentStore::new(ZoneId::new("test-zone"), tmp.path().join("ledger"));
        local.ensure_dirs().unwrap();
        local.write_segment(&sealed_segment("hostA", 1, 1)).unwrap();
        let seg2 = sealed_segment("hostA", 2, 1);
        local.write_segment(&seg2).unwrap();
        // Tamper seg2's checksum field: parseable, but verify() fails.
        let seg2_path = tmp.path().join("ledger/segments/hostA-000002.json");
        let tampered = fs::read_to_string(&seg2_path).unwrap().replace(
            &format!("\"checksum\": {}", seg2.checksum),
            "\"checksum\": 99999",
        );
        fs::write(&seg2_path, tampered).unwrap();

        let cursors = tmp.path().join("cursors");
        let err = run_emit(
            &cfg,
            EmitArgs {
                zone: None,
                source: Some("hostA".into()),
                cursor_dir: Some(cursors.clone()),
            },
        )
        .unwrap_err();
        assert!(err.to_string().contains("per-segment failures"), "{err}");
        // Only the valid segment was spooled; cursor stays below the
        // corrupt one so a repair gets retried.
        let names: Vec<String> = fs::read_dir(tmp.path().join("spool/incoming"))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["hostA-000001.json"], "corrupt segment must not spool");
        let cursor = load_cursor(&cursor_path(&cursors, "test-zone", "hostA"));
        assert_eq!(cursor.last_emitted_seq, 1);
    }

    #[test]
    fn status_healthy_and_disabled_zones_exit_zero() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = cfg_for(tmp.path(), None);
        let local = SegmentStore::new(ZoneId::new("test-zone"), tmp.path().join("ledger"));
        local.ensure_dirs().unwrap();
        local.write_segment(&sealed_segment("hostA", 1, 1)).unwrap();
        run_emit(
            &cfg,
            EmitArgs {
                zone: None,
                source: Some("hostA".into()),
                cursor_dir: Some(tmp.path().join("cursors")),
            },
        )
        .unwrap();

        // Healthy zone: Ok.
        run_status(
            &cfg,
            StatusArgs {
                zone: None,
                source: Some("hostA".into()),
                cursor_dir: Some(tmp.path().join("cursors")),
            },
        )
        .unwrap();

        // Disabled zone: prints "spool disabled", still Ok.
        let mut disabled = cfg_for(tmp.path(), None);
        disabled.zones[0].spool = false;
        run_status(
            &disabled,
            StatusArgs {
                zone: None,
                source: Some("hostA".into()),
                cursor_dir: Some(tmp.path().join("cursors")),
            },
        )
        .unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn status_unreadable_spool_dir_exits_nonzero() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().unwrap();
        let cfg = cfg_for(tmp.path(), None);
        let incoming = tmp.path().join("spool/incoming");
        fs::create_dir_all(&incoming).unwrap();
        fs::set_permissions(&incoming, fs::Permissions::from_mode(0o000)).unwrap();

        let result = run_status(
            &cfg,
            StatusArgs {
                zone: None,
                source: Some("hostA".into()),
                cursor_dir: Some(tmp.path().join("cursors")),
            },
        );
        fs::set_permissions(&incoming, fs::Permissions::from_mode(0o755)).unwrap();

        let err = result.unwrap_err();
        assert!(err.to_string().contains("spool status found problems"), "{err}");
    }

    #[test]
    fn status_corrupt_cursor_exits_nonzero() {
        // Deterministic via --source injection: no dependency on the
        // machine's hostname, so the corrupt branch is ALWAYS asserted.
        let tmp = tempfile::tempdir().unwrap();
        let cfg = cfg_for(tmp.path(), None);
        let cursors = tmp.path().join("cursors");
        fs::create_dir_all(&cursors).unwrap();
        fs::write(cursor_path(&cursors, "test-zone", "hostA"), "not json {").unwrap();

        let err = run_status(
            &cfg,
            StatusArgs {
                zone: None,
                source: Some("hostA".into()),
                cursor_dir: Some(cursors),
            },
        )
        .unwrap_err();
        assert!(err.to_string().contains("spool status found problems"), "{err}");
    }

    #[test]
    fn emit_rejects_invalid_source_name() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = cfg_for(tmp.path(), None);
        for bad in ["../evil", "a/b", ".hidden", ""] {
            let err = run_emit(
                &cfg,
                EmitArgs {
                    zone: None,
                    source: Some(bad.to_string()),
                    cursor_dir: Some(tmp.path().join("cursors")),
                },
            )
            .unwrap_err();
            assert!(
                err.to_string().contains("invalid source name"),
                "{bad:?}: {err}"
            );
        }
    }
}
