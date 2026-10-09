//! The code editor view: renders a text buffer and turns input into edits.

mod actions;
mod diagnostics;
mod editor;
mod element;
mod folding;
mod git_gutter;
mod hidden;
mod highlighting;
mod input;
mod layout;
mod scrollbar;
mod split_diff;
mod wrap;

pub use actions::*;
pub use diagnostics::{Diagnostic, DiagnosticSeverity};
pub use editor::{DiffRow, DiffRowKind, Editor, EditorEvent};
pub use git_gutter::BaseRow;
