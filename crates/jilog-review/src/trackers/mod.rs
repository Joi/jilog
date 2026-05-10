//! Built-in issue trackers.

pub mod beads;
pub mod github;
pub mod none;

pub use beads::BeadsTracker;
pub use github::GithubTracker;
pub use none::NoneTracker;
