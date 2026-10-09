//! Change markers against the last commit, computed in the background.

use std::sync::Arc;
use std::time::Duration;

use git::LineHunk;
use gpui::{AppContext, Context, Task};

use crate::Editor;

/// Wait this long after an edit before re-diffing, so typing stays cheap.
const DIFF_DELAY: Duration = Duration::from_millis(120);

pub(crate) struct GitDiffState {
    /// The file's text at HEAD.
    base: Arc<str>,
    /// Changed regions, sorted, for the text at `version`.
    pub(crate) hunks: Arc<[LineHunk]>,
    version: Option<u64>,
    task: Option<Task<()>>,
}

/// Where a row of the buffer was in the last commit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BaseRow {
    /// Not known yet (no diff base, or the first diff is still running).
    Unknown,
    /// The row was added or changed since the last commit.
    Uncommitted,
    Row(usize),
}

impl Editor {
    /// Sets the file's committed text to diff against, or `None` to hide the
    /// markers (untracked files, no repository).
    pub fn set_diff_base(&mut self, base: Option<&str>, cx: &mut Context<Self>) {
        let Some(base) = base else {
            if self.git_diff.take().is_some() {
                cx.notify();
            }
            return;
        };
        if self
            .git_diff
            .as_ref()
            .is_some_and(|state| &*state.base == base)
        {
            return;
        }
        self.git_diff = Some(GitDiffState {
            base: base.into(),
            hunks: Arc::from([]),
            version: None,
            task: None,
        });
        self.schedule_git_diff(cx);
    }

    pub fn has_diff_base(&self) -> bool {
        self.git_diff.is_some()
    }

    pub(crate) fn schedule_git_diff(&mut self, cx: &mut Context<Self>) {
        let Some(state) = &mut self.git_diff else {
            return;
        };
        if state.task.is_some() {
            // The running diff re-checks the version when it finishes.
            return;
        }
        let delay = if state.version.is_some() {
            DIFF_DELAY
        } else {
            Duration::ZERO
        };
        state.task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(delay).await;
            let Ok(Some((base, rope, version))) = this.update(cx, |editor, _| {
                let state = editor.git_diff.as_ref()?;
                let rope = editor.buffer.rope().clone();
                Some((state.base.clone(), rope, editor.buffer.version()))
            }) else {
                return;
            };
            let hunks = cx
                .background_spawn(async move { git::diff_lines(&base, &rope.to_string()) })
                .await;
            this.update(cx, |editor, cx| {
                let current = editor.buffer.version();
                let Some(state) = &mut editor.git_diff else {
                    return;
                };
                state.task = None;
                state.hunks = hunks.into();
                state.version = Some(version);
                if current != version {
                    editor.schedule_git_diff(cx);
                }
                cx.notify();
            })
            .ok();
        }));
    }

    /// Maps a buffer row to the matching row in the last commit, for blame.
    pub fn base_row(&self, row: usize) -> BaseRow {
        let Some(state) = &self.git_diff else {
            return BaseRow::Unknown;
        };
        // A slightly stale diff (mid-typing) is still close enough.
        if state.version.is_none() {
            return BaseRow::Unknown;
        }
        match git::base_row(&state.hunks, row as u32) {
            Some(base) => BaseRow::Row(base as usize),
            None => BaseRow::Uncommitted,
        }
    }

    /// The change shown at `row` (a deletion shows on the row below it), if
    /// the diff is up to date with the text.
    pub(crate) fn hunk_at_row(&self, row: usize) -> Option<&LineHunk> {
        let state = self.git_diff.as_ref()?;
        if state.version != Some(self.buffer.version()) {
            return None;
        }
        let row = row as u32;
        state
            .hunks
            .iter()
            .find(|hunk| hunk.new.start <= row && row < hunk.new.end.max(hunk.new.start + 1))
    }

    /// Puts the committed lines back in place of the change at `row`, as one
    /// undoable edit. A file without unsaved edits is saved right away, so
    /// the revert reaches the disk (and any agent working on it).
    pub fn revert_hunk_at_row(&mut self, row: usize, cx: &mut Context<Self>) -> bool {
        if self.read_only {
            return false;
        }
        let (Some(hunk), Some(state)) = (self.hunk_at_row(row), &self.git_diff) else {
            return false;
        };
        let base: Vec<&str> = state.base.lines().collect();
        let old_end = (hunk.old.end as usize).min(base.len());
        let old = &base[(hunk.old.start as usize).min(old_end)..old_end];
        let rows = hunk.new.start as usize..hunk.new.end as usize;
        let text = git::replace_lines(&self.buffer.text(), rows, old);
        let was_dirty = self.is_dirty();
        self.apply_text(&text, cx);
        if !was_dirty && self.path().is_some() {
            self.save(cx).detach();
        }
        true
    }

    /// Hunks touching the given rows, for drawing.
    pub(crate) fn git_hunks_in(&self, rows: std::ops::Range<usize>) -> &[LineHunk] {
        let Some(state) = &self.git_diff else {
            return &[];
        };
        let (start, end) = (rows.start as u32, rows.end as u32);
        let hunks = &state.hunks[..];
        // A deletion marker sits at `new.start`, so include hunks ending there.
        let first = hunks.partition_point(|hunk| hunk.new.end.max(hunk.new.start + 1) <= start);
        let last = hunks.partition_point(|hunk| hunk.new.start < end);
        &hunks[first..last.max(first)]
    }
}
