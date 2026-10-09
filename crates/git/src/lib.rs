//! Git integration: status, diffs, blame, history and commits.
//!
//! Runs the user's own `git`, so hooks, signing, credential helpers and
//! config all behave exactly as in the terminal. Pure logic with no UI
//! dependencies; every call blocks, so run them on background threads.

mod blame;
mod cmd;
mod graph;
mod line_diff;
mod log;
mod patch;
mod repo;
mod revert;
mod status;
mod text_diff;

pub use blame::{Blame, BlameCommit};
pub use cmd::{GitError, Result};
pub use graph::{Edge, Graph, GraphRow, Span};
pub use line_diff::{LineChange, LineHunk, base_row, diff_lines};
pub use log::{CommitDetails, CommitSummary, RefName, now, relative_time};
pub use patch::{DiffLine, FileDiff, Hunk, LineKind};
pub use repo::Repository;
pub use revert::{Revert, replace_lines};
pub use status::{BranchInfo, FileState, Status, StatusEntry};
pub use text_diff::{diff_texts, first_changed_line};

#[cfg(test)]
mod tests;
