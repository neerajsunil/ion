//! Text buffer for Ion: rope storage, selections and undo history.
//!
//! Pure logic with no UI dependencies, so it can be unit tested without a window.

mod buffer;

pub use buffer::{Buffer, EditBatch, Selection, TAB, TextEdit};
