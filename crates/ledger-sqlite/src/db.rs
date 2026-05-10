//! The SQLite database -- schema, ingestion, queries, and rebuild.
//!
//! # Rust concepts in this file
//!
//! - **rusqlite::Connection**: The database handle. Like Python's
//!   `sqlite3.connect()`. We keep one open for the lifetime of LedgerDb.
//!
//! - **rusqlite::params![]**: Macro for SQL parameters. Prevents SQL
//!   injection by using parameterized queries. Never format SQL strings
//!   with user data directly.
//!
//! - **Transaction**: `conn.transaction()` starts a transaction.
//!   `tx.commit()` commits. If `tx` is dropped without commit, it
//!   automatically rolls back. This is Rust's "RAII" pattern -- cleanup
//!   happens when the variable goes out of scope.
//!
//! - **query_map + collect**: `stmt.query_map(params, |row| { ... })`
//!   returns an iterator over rows. Each row is mapped through a closure.
//!   `.collect::<Result<Vec<_>, _>>()` gathers all rows, failing on
//!   first error. It's like a list comprehension that can fail.

use rusqlite::{params, Connection};
use tracing;

use ledger_core::{Event, EventClass, PayloadTier, Segment, SegmentStore};

use crate::error::SqliteError;

// ---------------------------------------------------------------------------
// LedgerDb: the rebuildable SQLite projection
// ---------------------------------------------------------------------------

/// A rebuildable SQLite projection of ledger events.
///
/// This database is **not** the source of truth. It can be dropped and
/// rebuilt from segment files at any time using `rebuild_from_store()`.
pub struct LedgerDb {
    conn: Connection,
}

impl LedgerDb {
    /// Open (or create) a SQLite database at the given path.
    ///
    /// Creates the schema if the database is new.
    ///
    /// # Rust note: `Result` return
    ///
    /// Almost every method here returns `Result<T, SqliteError>`.
    /// The caller uses `?` to propagate errors. This is how Rust
    /// avoids exceptions -- errors are values, not control flow.
    pub fn open(path: impl AsRef<std::path::Path>) -> Result<Self, SqliteError> {
        let conn = Connection::open(path)?;
        let db = Self { conn };
        db.create_schema()?;
        Ok(db)
    }

    /// Open an in-memory database (useful for testing).
    pub fn open_in_memory() -> Result<Self, SqliteError> {
        let conn = Connection::open_in_memory()?;
        let db = Self { conn };
        db.create_schema()?;
        Ok(db)
    }

    // -----------------------------------------------------------------------
    // Schema
    // -----------------------------------------------------------------------

    /// Create the database schema if it doesn't exist.
    ///
    /// # Rust note: raw string literals
    ///
    /// `r#"..."#` is a raw string -- no escape sequences. You can put
    /// quotes and backslashes inside without escaping. The `#` count
    /// can be increased if you need `"#` inside the string. Very handy
    /// for SQL, regex, and JSON literals.
    fn create_schema(&self) -> Result<(), SqliteError> {
        self.conn.execute_batch(r#"
            -- Events table: one row per event, denormalized for fast queries.
            -- This is a projection -- always rebuildable from segment files.
            CREATE TABLE IF NOT EXISTS events (
                event_id        TEXT PRIMARY KEY,
                zone            TEXT NOT NULL,
                source          TEXT NOT NULL,
                source_seq      INTEGER NOT NULL,
                timestamp       TEXT NOT NULL,
                correlation_id  TEXT,
                causation_id    TEXT,
                actor_ref       TEXT,
                object_ref      TEXT,
                event_class     TEXT NOT NULL,
                payload_tier    TEXT NOT NULL,
                payload         TEXT,

                -- Segment provenance (which file this came from).
                segment_source  TEXT NOT NULL,
                segment_seq     INTEGER NOT NULL
            );

            -- Track which segments have been ingested (for incremental ingest).
            CREATE TABLE IF NOT EXISTS ingested_segments (
                source          TEXT NOT NULL,
                source_seq      INTEGER NOT NULL,
                event_count     INTEGER NOT NULL,
                ingested_at     TEXT NOT NULL DEFAULT (datetime('now')),
                PRIMARY KEY (source, source_seq)
            );

            -- Indexes for common query patterns.
            CREATE INDEX IF NOT EXISTS idx_events_timestamp
                ON events(timestamp DESC);

            CREATE INDEX IF NOT EXISTS idx_events_class
                ON events(event_class);

            CREATE INDEX IF NOT EXISTS idx_events_actor
                ON events(actor_ref)
                WHERE actor_ref IS NOT NULL;

            CREATE INDEX IF NOT EXISTS idx_events_object
                ON events(object_ref)
                WHERE object_ref IS NOT NULL;

            CREATE INDEX IF NOT EXISTS idx_events_zone_class
                ON events(zone, event_class);

            CREATE INDEX IF NOT EXISTS idx_events_correlation
                ON events(correlation_id)
                WHERE correlation_id IS NOT NULL;
        "#)?;

        Ok(())
    }

    // -----------------------------------------------------------------------
    // Ingestion
    // -----------------------------------------------------------------------

    /// Ingest a single segment into the database.
    ///
    /// Skips if this segment (source + seq) was already ingested
    /// (idempotent). All events are inserted in a single transaction.
    ///
    /// # Rust note: transactions
    ///
    /// `self.conn.transaction()` borrows `self.conn` mutably. While the
    /// transaction is alive, no other operation can use the connection.
    /// When `tx.commit()` is called, the borrow ends. If the function
    /// returns early (error), `tx` is dropped and auto-rolls-back.
    /// This is RAII -- Resource Acquisition Is Initialization.
    pub fn ingest_segment(&mut self, segment: &Segment) -> Result<usize, SqliteError> {
        // Check if already ingested (idempotent).
        if self.is_segment_ingested(&segment.source, segment.source_seq)? {
            tracing::debug!(
                source = %segment.source,
                seq = segment.source_seq,
                "segment already ingested, skipping"
            );
            return Ok(0);
        }

        let tx = self.conn.transaction()?;
        let mut count = 0;

        {
            let mut stmt = tx.prepare(r#"
                INSERT OR IGNORE INTO events (
                    event_id, zone, source, source_seq, timestamp,
                    correlation_id, causation_id, actor_ref, object_ref,
                    event_class, payload_tier, payload,
                    segment_source, segment_seq
                ) VALUES (
                    ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14
                )
            "#)?;

            for event in &segment.events {
                let payload_json = event.payload.as_ref()
                    .map(|p| serde_json::to_string(p))
                    .transpose()
                    .map_err(|e| SqliteError::Serialization(e.to_string()))?;

                stmt.execute(params![
                    event.event_id.to_string(),
                    event.zone,
                    event.source,
                    event.source_seq,
                    event.timestamp.to_rfc3339(),
                    event.correlation_id.map(|u| u.to_string()),
                    event.causation_id.map(|u| u.to_string()),
                    event.actor_ref,
                    event.object_ref,
                    format!("{:?}", event.event_class).to_lowercase(),
                    format!("{:?}", event.payload_tier).to_lowercase(),
                    payload_json,
                    segment.source,
                    segment.source_seq,
                ])?;

                count += 1;
            }

            // Record that this segment has been ingested.
            tx.execute(
                "INSERT OR IGNORE INTO ingested_segments (source, source_seq, event_count) VALUES (?1, ?2, ?3)",
                params![segment.source, segment.source_seq, count],
            )?;
        }

        tx.commit()?;

        tracing::info!(
            source = %segment.source,
            seq = segment.source_seq,
            events = count,
            "segment ingested"
        );

        Ok(count)
    }

    /// Check if a segment has already been ingested.
    fn is_segment_ingested(&self, source: &str, seq: u64) -> Result<bool, SqliteError> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM ingested_segments WHERE source = ?1 AND source_seq = ?2",
            params![source, seq],
            |row| row.get(0),
        )?;
        Ok(count > 0)
    }

    // -----------------------------------------------------------------------
    // Rebuild
    // -----------------------------------------------------------------------

    /// Drop all data and rebuild from a SegmentStore.
    ///
    /// This is the nuclear option -- proves that SQLite is truly
    /// rebuildable from segment files (a core architectural invariant).
    pub fn rebuild_from_store(&mut self, store: &SegmentStore) -> Result<usize, SqliteError> {
        tracing::info!(zone = %store.zone, "rebuilding SQLite from segments");

        // Drop everything.
        self.conn.execute_batch(r#"
            DELETE FROM events;
            DELETE FROM ingested_segments;
        "#)?;

        // Re-ingest all segments in order.
        let segments = store.read_all_segments()?;
        let mut total = 0;

        for segment in &segments {
            total += self.ingest_segment(&mut segment.clone())?;
        }

        tracing::info!(
            zone = %store.zone,
            segments = segments.len(),
            events = total,
            "rebuild complete"
        );

        Ok(total)
    }

    // -----------------------------------------------------------------------
    // Queries
    // -----------------------------------------------------------------------

    /// Count total events in the database.
    pub fn event_count(&self) -> Result<u64, SqliteError> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM events",
            [],
            |row| row.get(0),
        )?;
        Ok(count as u64)
    }

    /// Count ingested segments.
    pub fn segment_count(&self) -> Result<u64, SqliteError> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM ingested_segments",
            [],
            |row| row.get(0),
        )?;
        Ok(count as u64)
    }

    /// Get the N most recent events.
    ///
    /// # Rust note: row mapping
    ///
    /// `query_map` calls the closure for each row. The closure receives
    /// a `&Row` and returns a `Result<Event>`. Each `row.get::<_, T>(i)`
    /// extracts column `i` as type `T`. The `_` tells Rust to infer
    /// the column name type (it's always `usize` for positional params).
    pub fn recent_events(&self, limit: u32) -> Result<Vec<Event>, SqliteError> {
        let mut stmt = self.conn.prepare(
            "SELECT event_id, zone, source, source_seq, timestamp, \
             correlation_id, causation_id, actor_ref, object_ref, \
             event_class, payload_tier, payload \
             FROM events ORDER BY timestamp DESC LIMIT ?1"
        )?;

        let rows = stmt.query_map(params![limit], |row| {
            row_to_event(row)
        })?;

        rows.map(|r| r.map_err(SqliteError::from))
            .collect()
    }

    /// Get events by event class.
    pub fn events_by_class(
        &self,
        class: &EventClass,
        limit: u32,
    ) -> Result<Vec<Event>, SqliteError> {
        let class_str = format!("{:?}", class).to_lowercase();

        let mut stmt = self.conn.prepare(
            "SELECT event_id, zone, source, source_seq, timestamp, \
             correlation_id, causation_id, actor_ref, object_ref, \
             event_class, payload_tier, payload \
             FROM events WHERE event_class = ?1 \
             ORDER BY timestamp DESC LIMIT ?2"
        )?;

        let rows = stmt.query_map(params![class_str, limit], |row| {
            row_to_event(row)
        })?;

        rows.map(|r| r.map_err(SqliteError::from))
            .collect()
    }

    /// Get events referencing a specific object.
    pub fn events_for_object(
        &self,
        object_ref: &str,
        limit: u32,
    ) -> Result<Vec<Event>, SqliteError> {
        let mut stmt = self.conn.prepare(
            "SELECT event_id, zone, source, source_seq, timestamp, \
             correlation_id, causation_id, actor_ref, object_ref, \
             event_class, payload_tier, payload \
             FROM events WHERE object_ref = ?1 \
             ORDER BY timestamp DESC LIMIT ?2"
        )?;

        let rows = stmt.query_map(params![object_ref, limit], |row| {
            row_to_event(row)
        })?;

        rows.map(|r| r.map_err(SqliteError::from))
            .collect()
    }

    /// Get events by actor.
    pub fn events_by_actor(
        &self,
        actor_ref: &str,
        limit: u32,
    ) -> Result<Vec<Event>, SqliteError> {
        let mut stmt = self.conn.prepare(
            "SELECT event_id, zone, source, source_seq, timestamp, \
             correlation_id, causation_id, actor_ref, object_ref, \
             event_class, payload_tier, payload \
             FROM events WHERE actor_ref = ?1 \
             ORDER BY timestamp DESC LIMIT ?2"
        )?;

        let rows = stmt.query_map(params![actor_ref, limit], |row| {
            row_to_event(row)
        })?;

        rows.map(|r| r.map_err(SqliteError::from))
            .collect()
    }

    /// Get events sharing a correlation ID (workflow trace).
    pub fn events_by_correlation(
        &self,
        correlation_id: &str,
    ) -> Result<Vec<Event>, SqliteError> {
        let mut stmt = self.conn.prepare(
            "SELECT event_id, zone, source, source_seq, timestamp, \
             correlation_id, causation_id, actor_ref, object_ref, \
             event_class, payload_tier, payload \
             FROM events WHERE correlation_id = ?1 \
             ORDER BY timestamp ASC"
        )?;

        let rows = stmt.query_map(params![correlation_id], |row| {
            row_to_event(row)
        })?;

        rows.map(|r| r.map_err(SqliteError::from))
            .collect()
    }
}

// ---------------------------------------------------------------------------
// Row mapping helper
// ---------------------------------------------------------------------------

/// Convert a SQLite row back into an Event.
///
/// # Rust note: free functions vs methods
///
/// This is a module-level function, not a method on LedgerDb. It doesn't
/// need `self` because it only operates on the row data. In Rust, not
/// everything needs to be a method -- free functions are perfectly fine
/// and often clearer.
fn row_to_event(row: &rusqlite::Row) -> rusqlite::Result<Event> {
    let event_id_str: String = row.get(0)?;
    let zone: String = row.get(1)?;
    let source: String = row.get(2)?;
    let source_seq: i64 = row.get(3)?;
    let timestamp_str: String = row.get(4)?;
    let correlation_str: Option<String> = row.get(5)?;
    let causation_str: Option<String> = row.get(6)?;
    let actor_ref: Option<String> = row.get(7)?;
    let object_ref: Option<String> = row.get(8)?;
    let class_str: String = row.get(9)?;
    let tier_str: String = row.get(10)?;
    let payload_str: Option<String> = row.get(11)?;

    // Parse UUID, timestamp, enums. On parse failure, use defaults
    // rather than failing the entire query (projection is best-effort).
    let event_id = uuid::Uuid::parse_str(&event_id_str)
        .unwrap_or_else(|_| uuid::Uuid::nil());

    let timestamp = chrono::DateTime::parse_from_rfc3339(&timestamp_str)
        .map(|dt| dt.with_timezone(&chrono::Utc))
        .unwrap_or_else(|_| chrono::Utc::now());

    let correlation_id = correlation_str
        .and_then(|s| uuid::Uuid::parse_str(&s).ok());

    let causation_id = causation_str
        .and_then(|s| uuid::Uuid::parse_str(&s).ok());

    let event_class = parse_event_class(&class_str);
    let payload_tier = parse_payload_tier(&tier_str);

    let payload = payload_str
        .and_then(|s| serde_json::from_str(&s).ok());

    Ok(Event {
        event_id,
        zone,
        source,
        source_seq: source_seq as u64,
        timestamp,
        correlation_id,
        causation_id,
        actor_ref,
        object_ref,
        event_class,
        payload_tier,
        payload,
    })
}

fn parse_event_class(s: &str) -> EventClass {
    match s {
        "ingest" => EventClass::Ingest,
        "route" => EventClass::Route,
        "decision" => EventClass::Decision,
        "statechange" => EventClass::StateChange,
        "claim" => EventClass::Claim,
        "delivery" => EventClass::Delivery,
        "projection" => EventClass::Projection,
        "health" => EventClass::Health,
        "approval" => EventClass::Approval,
        "notemeta" => EventClass::NoteMeta,
        _ => EventClass::Health, // fallback
    }
}

fn parse_payload_tier(s: &str) -> PayloadTier {
    match s {
        "metadataonly" => PayloadTier::MetadataOnly,
        "structured" => PayloadTier::Structured,
        "confidential" => PayloadTier::Confidential,
        _ => PayloadTier::MetadataOnly, // fallback
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use uuid::Uuid;

    fn test_event(zone: &str, seq: u64, class: EventClass) -> Event {
        Event {
            event_id: Uuid::now_v7(),
            zone: zone.to_string(),
            source: "test".to_string(),
            source_seq: seq,
            timestamp: Utc::now(),
            correlation_id: None,
            causation_id: None,
            actor_ref: Some("person:test-user".to_string()),
            object_ref: Some("claim:test-claim".to_string()),
            event_class: class,
            payload_tier: PayloadTier::MetadataOnly,
            payload: Some(serde_json::json!({"action": "test"})),
        }
    }

    fn sealed_segment(source: &str, seq: u64, events: Vec<Event>) -> Segment {
        let mut seg = Segment::new(source, seq);
        for e in events {
            seg.append(e);
        }
        seg.seal().unwrap();
        seg
    }

    #[test]
    fn test_open_in_memory() {
        let db = LedgerDb::open_in_memory().unwrap();
        assert_eq!(db.event_count().unwrap(), 0);
        assert_eq!(db.segment_count().unwrap(), 0);
    }

    #[test]
    fn test_ingest_segment() {
        let mut db = LedgerDb::open_in_memory().unwrap();

        let seg = sealed_segment("macazbd", 1, vec![
            test_event("public-ops", 1, EventClass::Health),
            test_event("public-ops", 2, EventClass::Ingest),
        ]);

        let count = db.ingest_segment(&seg).unwrap();
        assert_eq!(count, 2);
        assert_eq!(db.event_count().unwrap(), 2);
        assert_eq!(db.segment_count().unwrap(), 1);
    }

    #[test]
    fn test_ingest_is_idempotent() {
        let mut db = LedgerDb::open_in_memory().unwrap();

        let seg = sealed_segment("macazbd", 1, vec![
            test_event("public-ops", 1, EventClass::Health),
        ]);

        db.ingest_segment(&seg).unwrap();
        let count = db.ingest_segment(&seg).unwrap(); // second time
        assert_eq!(count, 0); // skipped
        assert_eq!(db.event_count().unwrap(), 1); // still 1
    }

    #[test]
    fn test_recent_events() {
        let mut db = LedgerDb::open_in_memory().unwrap();

        let seg = sealed_segment("macazbd", 1, vec![
            test_event("public-ops", 1, EventClass::Health),
            test_event("public-ops", 2, EventClass::Ingest),
            test_event("public-ops", 3, EventClass::Approval),
        ]);

        db.ingest_segment(&seg).unwrap();

        let recent = db.recent_events(2).unwrap();
        assert_eq!(recent.len(), 2);
    }

    #[test]
    fn test_events_by_class() {
        let mut db = LedgerDb::open_in_memory().unwrap();

        let seg = sealed_segment("macazbd", 1, vec![
            test_event("public-ops", 1, EventClass::Health),
            test_event("public-ops", 2, EventClass::Ingest),
            test_event("public-ops", 3, EventClass::Health),
        ]);

        db.ingest_segment(&seg).unwrap();

        let health = db.events_by_class(&EventClass::Health, 10).unwrap();
        assert_eq!(health.len(), 2);

        let ingest = db.events_by_class(&EventClass::Ingest, 10).unwrap();
        assert_eq!(ingest.len(), 1);
    }

    #[test]
    fn test_events_for_object() {
        let mut db = LedgerDb::open_in_memory().unwrap();

        let seg = sealed_segment("macazbd", 1, vec![
            test_event("public-ops", 1, EventClass::Claim),
            test_event("public-ops", 2, EventClass::Approval),
        ]);

        db.ingest_segment(&seg).unwrap();

        let events = db.events_for_object("claim:test-claim", 10).unwrap();
        assert_eq!(events.len(), 2);

        let none = db.events_for_object("claim:nonexistent", 10).unwrap();
        assert_eq!(none.len(), 0);
    }

    #[test]
    fn test_events_by_actor() {
        let mut db = LedgerDb::open_in_memory().unwrap();

        let seg = sealed_segment("macazbd", 1, vec![
            test_event("public-ops", 1, EventClass::Decision),
        ]);

        db.ingest_segment(&seg).unwrap();

        let events = db.events_by_actor("person:test-user", 10).unwrap();
        assert_eq!(events.len(), 1);
    }

    #[test]
    fn test_events_by_correlation() {
        let mut db = LedgerDb::open_in_memory().unwrap();

        let corr_id = Uuid::now_v7();

        let mut e1 = test_event("public-ops", 1, EventClass::Claim);
        e1.correlation_id = Some(corr_id);

        let mut e2 = test_event("public-ops", 2, EventClass::Approval);
        e2.correlation_id = Some(corr_id);

        let e3 = test_event("public-ops", 3, EventClass::Health); // no correlation

        let seg = sealed_segment("macazbd", 1, vec![e1, e2, e3]);
        db.ingest_segment(&seg).unwrap();

        let correlated = db.events_by_correlation(&corr_id.to_string()).unwrap();
        assert_eq!(correlated.len(), 2);
    }

    #[test]
    fn test_rebuild_from_store() {
        use ledger_core::{SegmentStore, ZoneId};

        let dir = std::env::temp_dir().join("opsctl-test-sqlite-rebuild");
        let _ = std::fs::remove_dir_all(&dir);

        let store = SegmentStore::new(ZoneId::new("test"), &dir);

        // Write two segments to the store.
        let seg1 = sealed_segment("macazbd", 1, vec![
            test_event("test", 1, EventClass::Health),
            test_event("test", 2, EventClass::Ingest),
        ]);
        let seg2 = sealed_segment("macazbd", 2, vec![
            test_event("test", 3, EventClass::Approval),
        ]);

        store.write_segment(&seg1).unwrap();
        store.write_segment(&seg2).unwrap();

        // Build initial database.
        let mut db = LedgerDb::open_in_memory().unwrap();
        db.ingest_segment(&seg1).unwrap();
        db.ingest_segment(&seg2).unwrap();
        assert_eq!(db.event_count().unwrap(), 3);

        // Rebuild from store (drop + re-ingest).
        let total = db.rebuild_from_store(&store).unwrap();
        assert_eq!(total, 3);
        assert_eq!(db.event_count().unwrap(), 3);
        assert_eq!(db.segment_count().unwrap(), 2);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_roundtrip_event_fidelity() {
        // Verify that ingesting then querying preserves event data.
        let mut db = LedgerDb::open_in_memory().unwrap();

        let original = test_event("public-ops", 1, EventClass::Claim);
        let event_id = original.event_id;

        let seg = sealed_segment("macazbd", 1, vec![original]);
        db.ingest_segment(&seg).unwrap();

        let events = db.recent_events(1).unwrap();
        assert_eq!(events.len(), 1);

        let loaded = &events[0];
        assert_eq!(loaded.event_id, event_id);
        assert_eq!(loaded.zone, "public-ops");
        assert_eq!(loaded.source, "test");
        assert_eq!(loaded.actor_ref.as_deref(), Some("person:test-user"));
        assert_eq!(loaded.object_ref.as_deref(), Some("claim:test-claim"));
        assert!(loaded.payload.is_some());
    }
}
