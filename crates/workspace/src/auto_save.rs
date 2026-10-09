//! Auto save (Settings > Editor > Auto save).

use std::time::Duration;

use editor::Editor;
use gpui::{Context, Entity, EntityId};
use settings::AutoSave;

use crate::workspace::Workspace;

/// "After a delay" saves this long after the last edit.
const DELAY: Duration = Duration::from_millis(1000);

impl Workspace {
    /// Restarts the save timer for an edited file.
    pub(crate) fn schedule_auto_save(&mut self, editor: &Entity<Editor>, cx: &mut Context<Self>) {
        if settings::get(cx).auto_save != AutoSave::AfterDelay || !can_save(editor, cx) {
            return;
        }
        let id = editor.entity_id();
        let editor = editor.downgrade();
        // Replacing the task cancels the previous timer.
        self.auto_save_tasks.insert(
            id,
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(DELAY).await;
                if let Ok(save) = editor.update(cx, |editor, cx| {
                    (editor.is_dirty() && editor.path().is_some()).then(|| editor.save(cx))
                }) && let Some(save) = save
                {
                    save.await;
                }
                this.update(cx, |this, _| this.auto_save_tasks.remove(&id))
                    .ok();
            }),
        );
    }

    /// Saves the editor that just lost focus, with "On focus change".
    pub(crate) fn auto_save_on_focus_change(&mut self, previous: EntityId, cx: &mut Context<Self>) {
        if settings::get(cx).auto_save != AutoSave::OnFocusChange {
            return;
        }
        let Some((pane, ix)) = self.find_item(previous) else {
            return;
        };
        if let Some(editor) = self.panes[&pane].items[ix].editor().cloned() {
            save_if_dirty(&editor, cx);
        }
    }

    /// Saves every modified file, when Ion loses focus.
    pub(crate) fn auto_save_all(&mut self, cx: &mut Context<Self>) {
        if settings::get(cx).auto_save != AutoSave::OnFocusChange {
            return;
        }
        for editor in self.editors() {
            save_if_dirty(&editor, cx);
        }
    }
}

fn can_save(editor: &Entity<Editor>, cx: &gpui::App) -> bool {
    let editor = editor.read(cx);
    editor.path().is_some() && !editor.is_read_only()
}

fn save_if_dirty(editor: &Entity<Editor>, cx: &mut gpui::App) {
    if can_save(editor, cx) && editor.read(cx).is_dirty() {
        editor.update(cx, |editor, cx| editor.save(cx)).detach();
    }
}
