//! Built-in issue trackers.

pub mod github;
pub mod kata;
pub mod none;

pub use github::GithubTracker;
pub use kata::KataTracker;
pub use none::NoneTracker;
