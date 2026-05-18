//! Built-in session-log readers.

pub mod amplifier;
pub mod claude_code;
pub mod codex;
pub mod copilot;
pub mod generic;

pub use amplifier::AmplifierReader;
pub use claude_code::ClaudeCodeReader;
pub use codex::CodexReader;
pub use copilot::CopilotReader;
pub use generic::{GenericReader, SessionIdSource};
