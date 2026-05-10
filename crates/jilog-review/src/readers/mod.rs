//! Built-in session-log readers.

pub mod amplifier;
pub mod claude_code;
pub mod generic;

pub use amplifier::AmplifierReader;
pub use claude_code::ClaudeCodeReader;
pub use generic::{GenericReader, SessionIdSource};
