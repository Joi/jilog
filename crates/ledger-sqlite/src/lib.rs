//! ledger-sqlite: Rebuildable SQLite index over ledger segments.
//!
//! This is a **projection**, not an authority. The SQLite database
//! can always be rebuilt from the segment files in ledger-core.
//!
//! Use it for:
//! - Fast event queries (by time, class, actor, object)
//! - Current-state projections
//! - Object resolution indexes
//! - Review queues
//!
//! # Rust concepts in this crate
//!
//! - **rusqlite**: A safe Rust wrapper around SQLite. You open a
//!   `Connection`, prepare SQL statements, and execute them. Parameters
//!   use `rusqlite::params![]` macro to prevent SQL injection.
//!
//! - **Traits**: Think of a trait like a Python abstract class or
//!   interface. `trait EventQuery` defines a set of methods that any
//!   implementor must provide. This lets us swap in a mock for testing.
//!
//! - **`&[T]` (slices)**: A borrowed view into a contiguous sequence.
//!   `&[Event]` means "a reference to a list of Events." The caller
//!   owns the data, we just borrow it for reading.
//!
//! - **`impl Trait` in return position**: `-> impl Iterator<Item = Event>`
//!   means "I return something that implements Iterator, but I'm not
//!   telling you the exact type." This is Rust's way of hiding complex
//!   iterator adapter chains behind a simple interface.

mod db;
pub mod error;

pub use db::{IndexRefreshReport, LedgerDb};
pub use error::SqliteError;
