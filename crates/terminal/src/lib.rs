//! Integrated terminal: runs a shell in a pseudo-terminal (ConPTY on Windows)
//! and renders it with GPUI. Terminal emulation is `alacritty_terminal`.

mod colors;
mod element;
mod harness;
mod keys;
mod links;
mod osc;
mod problems;
mod pty;
mod view;

pub use harness::{Harness, HarnessKind, available_harnesses};
pub use links::{FileLink, link_at, url_at};
pub use osc::{Progress, ProgressState};
pub use problems::{Problem, Severity, parse_problems};
pub use pty::{ShellProfile, available_shells, set_extra_env};
pub use view::{Copy, Paste, SendKeystroke, TerminalEvent, TerminalView, key_bindings, quote_path};
