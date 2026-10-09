//! What an editor gives up while its tab is hidden: the syntax tree right
//! away, and after a while its text and HEAD text are kept lz4-compressed.
//! Reads decompress on demand, so nothing else needs to know.

use std::sync::Arc;
use std::time::Duration;

use gpui::{App, AppContext, Context};

use crate::Editor;

/// A tab hidden this long has its text compressed. A one-shot timer started
/// by hiding the tab, so switching back and forth doesn't recompress.
const PACK_DELAY: Duration = Duration::from_secs(30);

impl Editor {
    /// Whether a hidden editor still holds memory it could give up.
    pub fn holds_hidden_state(&self, cx: &App) -> bool {
        self.holds_syntax_tree(cx) || (self.pack_task.is_none() && self.can_pack())
    }

    fn can_pack(&self) -> bool {
        self.buffer.is_unpacked()
            || self
                .git_diff
                .as_ref()
                .is_some_and(|state| state.base.is_unpacked())
    }

    /// For an editor that isn't on screen: drops its syntax tree now and
    /// compresses its text after [`PACK_DELAY`].
    pub fn release_hidden(&mut self, cx: &mut Context<Self>) {
        self.release_syntax_tree(cx);
        if self.pack_task.is_some() || !self.can_pack() {
            return;
        }
        self.pack_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(PACK_DELAY).await;
            // Text decompressed by a read since the last packing just drops
            // again; the rest is compressed off the UI thread.
            let Ok((text, base)) = this.update(cx, |editor, _| {
                let text = (!editor.buffer.repack())
                    .then(|| editor.buffer.packable())
                    .flatten();
                let base = editor.git_diff.as_mut().and_then(|state| {
                    (state.base.is_unpacked() && !state.base.repack())
                        .then(|| (*state.base).clone())
                });
                (text, base)
            }) else {
                return;
            };
            let packed = cx
                .background_spawn(async move {
                    let text = text.map(|(rope, version)| (version, text::pack(&rope)));
                    let base = base.map(|base| (text::pack(&base), base));
                    (text, base)
                })
                .await;
            this.update(cx, |editor, _| {
                editor.pack_task = None;
                let (text, base) = packed;
                if let Some((version, bytes)) = text {
                    editor.buffer.set_packed(version, bytes);
                }
                if let (Some((bytes, base)), Some(state)) = (base, &mut editor.git_diff)
                    && state.base.is_unpacked()
                    && Arc::ptr_eq(&state.base, &base)
                {
                    state.base.set_packed(bytes);
                }
            })
            .ok();
        }));
    }

    /// For a tab that's shown: cancels packing and decompresses.
    pub(crate) fn wake(&mut self) {
        self.pack_task = None;
        self.buffer.unpack();
        if let Some(state) = &mut self.git_diff {
            state.base.unpack();
        }
    }
}
