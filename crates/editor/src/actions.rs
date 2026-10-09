use gpui::{KeyBinding, actions};

actions!(
    editor,
    [
        MoveLeft,
        MoveRight,
        MoveUp,
        MoveDown,
        SelectLeft,
        SelectRight,
        SelectUp,
        SelectDown,
        MoveWordLeft,
        MoveWordRight,
        SelectWordLeft,
        SelectWordRight,
        MoveLineStart,
        MoveLineEnd,
        SelectLineStart,
        SelectLineEnd,
        MoveDocStart,
        MoveDocEnd,
        SelectDocStart,
        SelectDocEnd,
        PageUp,
        PageDown,
        SelectPageUp,
        SelectPageDown,
        Backspace,
        Delete,
        DeleteWordLeft,
        DeleteWordRight,
        Newline,
        Tab,
        Backtab,
        SelectAll,
        Copy,
        Cut,
        Paste,
        Undo,
        Redo,
        Save,
        ToggleComment,
        DuplicateLine,
        MoveLineUp,
        MoveLineDown,
        DeleteLine,
        RevertHunk,
        ToggleWordWrap,
        AddNextOccurrence,
        SelectAllOccurrences,
        ClearCursors,
        Fold,
        Unfold,
        FoldAll,
        UnfoldAll,
        ToggleDiffLayout,
    ]
);

const CONTEXT: Option<&str> = Some("Editor");

/// Default editor key bindings.
pub fn key_bindings() -> Vec<KeyBinding> {
    // Word movement is Ctrl on Windows/Linux and Option on macOS.
    let word = if cfg!(target_os = "macos") {
        "alt"
    } else {
        "ctrl"
    };
    let mut bindings = vec![
        KeyBinding::new("left", MoveLeft, CONTEXT),
        KeyBinding::new("right", MoveRight, CONTEXT),
        KeyBinding::new("up", MoveUp, CONTEXT),
        KeyBinding::new("down", MoveDown, CONTEXT),
        KeyBinding::new("shift-left", SelectLeft, CONTEXT),
        KeyBinding::new("shift-right", SelectRight, CONTEXT),
        KeyBinding::new("shift-up", SelectUp, CONTEXT),
        KeyBinding::new("shift-down", SelectDown, CONTEXT),
        KeyBinding::new(&format!("{word}-left"), MoveWordLeft, CONTEXT),
        KeyBinding::new(&format!("{word}-right"), MoveWordRight, CONTEXT),
        KeyBinding::new(&format!("{word}-shift-left"), SelectWordLeft, CONTEXT),
        KeyBinding::new(&format!("{word}-shift-right"), SelectWordRight, CONTEXT),
        KeyBinding::new(&format!("{word}-backspace"), DeleteWordLeft, CONTEXT),
        KeyBinding::new(&format!("{word}-delete"), DeleteWordRight, CONTEXT),
        KeyBinding::new("home", MoveLineStart, CONTEXT),
        KeyBinding::new("end", MoveLineEnd, CONTEXT),
        KeyBinding::new("shift-home", SelectLineStart, CONTEXT),
        KeyBinding::new("shift-end", SelectLineEnd, CONTEXT),
        KeyBinding::new("pageup", PageUp, CONTEXT),
        KeyBinding::new("pagedown", PageDown, CONTEXT),
        KeyBinding::new("shift-pageup", SelectPageUp, CONTEXT),
        KeyBinding::new("shift-pagedown", SelectPageDown, CONTEXT),
        KeyBinding::new("backspace", Backspace, CONTEXT),
        KeyBinding::new("shift-backspace", Backspace, CONTEXT),
        KeyBinding::new("delete", Delete, CONTEXT),
        KeyBinding::new("enter", Newline, CONTEXT),
        KeyBinding::new("shift-enter", Newline, CONTEXT),
        KeyBinding::new("tab", Tab, CONTEXT),
        KeyBinding::new("shift-tab", Backtab, CONTEXT),
        KeyBinding::new("secondary-a", SelectAll, CONTEXT),
        KeyBinding::new("secondary-c", Copy, CONTEXT),
        KeyBinding::new("secondary-x", Cut, CONTEXT),
        KeyBinding::new("secondary-v", Paste, CONTEXT),
        KeyBinding::new("secondary-z", Undo, CONTEXT),
        KeyBinding::new("secondary-shift-z", Redo, CONTEXT),
        KeyBinding::new("secondary-s", Save, CONTEXT),
        KeyBinding::new("secondary-/", ToggleComment, CONTEXT),
        KeyBinding::new("shift-alt-down", DuplicateLine, CONTEXT),
        KeyBinding::new("shift-alt-up", DuplicateLine, CONTEXT),
        KeyBinding::new("alt-up", MoveLineUp, CONTEXT),
        KeyBinding::new("alt-down", MoveLineDown, CONTEXT),
        KeyBinding::new("secondary-shift-k", DeleteLine, CONTEXT),
        KeyBinding::new("secondary-alt-z", RevertHunk, CONTEXT),
        KeyBinding::new("alt-z", ToggleWordWrap, CONTEXT),
        KeyBinding::new("secondary-d", AddNextOccurrence, CONTEXT),
        KeyBinding::new("secondary-shift-l", SelectAllOccurrences, CONTEXT),
        // Only with several cursors, so Escape still reaches find bars
        // and pickers otherwise.
        KeyBinding::new("escape", ClearCursors, Some("Editor && multicursor")),
        // Shift+[ arrives as { on most layouts.
        KeyBinding::new("secondary-shift-[", Fold, CONTEXT),
        KeyBinding::new("secondary-{", Fold, CONTEXT),
        KeyBinding::new("secondary-shift-]", Unfold, CONTEXT),
        KeyBinding::new("secondary-}", Unfold, CONTEXT),
        KeyBinding::new("secondary-k secondary-0", FoldAll, CONTEXT),
        KeyBinding::new("secondary-k secondary-j", UnfoldAll, CONTEXT),
    ];
    if cfg!(target_os = "macos") {
        bindings.extend([
            KeyBinding::new("cmd-left", MoveLineStart, CONTEXT),
            KeyBinding::new("cmd-right", MoveLineEnd, CONTEXT),
            KeyBinding::new("cmd-shift-left", SelectLineStart, CONTEXT),
            KeyBinding::new("cmd-shift-right", SelectLineEnd, CONTEXT),
            KeyBinding::new("cmd-up", MoveDocStart, CONTEXT),
            KeyBinding::new("cmd-down", MoveDocEnd, CONTEXT),
            KeyBinding::new("cmd-shift-up", SelectDocStart, CONTEXT),
            KeyBinding::new("cmd-shift-down", SelectDocEnd, CONTEXT),
        ]);
    } else {
        bindings.extend([
            KeyBinding::new("ctrl-home", MoveDocStart, CONTEXT),
            KeyBinding::new("ctrl-end", MoveDocEnd, CONTEXT),
            KeyBinding::new("ctrl-shift-home", SelectDocStart, CONTEXT),
            KeyBinding::new("ctrl-shift-end", SelectDocEnd, CONTEXT),
            KeyBinding::new("ctrl-y", Redo, CONTEXT),
        ]);
    }
    bindings
}
