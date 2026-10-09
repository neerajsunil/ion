//! Text buffer for Ion: rope storage, selections and undo history.
//!
//! Pure logic with no UI dependencies, so it can be unit tested without a window.

mod buffer;
mod packed;

pub use buffer::{Buffer, EditBatch, IndentRules, Selection, TAB, TextEdit};
pub use packed::{Packable, Packed, pack};
pub use ropey::Rope;
