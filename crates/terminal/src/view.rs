use std::ops::Range;
use std::path::{Path, PathBuf};

use alacritty_terminal::event::Event as TermEvent;
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line, Point as GridPoint, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::TermMode;
use futures::StreamExt;
use gpui::{
    App, AppContext, Bounds, ClipboardItem, Context, CursorStyle, EntityInputHandler, EventEmitter,
    FocusHandle, Focusable, InteractiveElement, IntoElement, KeyBinding, KeyDownEvent, Keystroke,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, ParentElement, Pixels, Point,
    Render, ScrollWheelEvent, SharedString, Styled, Subscription, Task, UTF16Selection, Window,
    actions, div, prelude::FluentBuilder,
};

use crate::colors;
use crate::element::{GridLayout, TerminalElement};
use crate::harness::{Harness, HarnessKind};
use crate::keys;
use crate::osc::{Progress, Signal};
use crate::pty::{GridSize, Pty, PtyEvent, SharedTerm, ShellProfile};

/// Sends a keystroke to the shell. Bound in the terminal for shortcuts the
/// workspace would otherwise take (Ctrl+W, Ctrl+B, ...), which shells and
/// agents use.
#[derive(Clone, PartialEq, Debug, gpui::Action)]
#[action(namespace = terminal, no_json)]
pub struct SendKeystroke(pub &'static str);

actions!(terminal, [Copy, Paste]);

const CONTEXT: Option<&str> = Some("Terminal");
const DEFAULT_TITLE: &str = "Terminal";

pub fn key_bindings() -> Vec<KeyBinding> {
    let mut bindings = vec![
        KeyBinding::new("ctrl-shift-c", Copy, CONTEXT),
        KeyBinding::new("ctrl-shift-v", Paste, CONTEXT),
    ];
    if cfg!(target_os = "macos") {
        bindings.extend([
            KeyBinding::new("cmd-c", Copy, CONTEXT),
            KeyBinding::new("cmd-v", Paste, CONTEXT),
        ]);
    } else {
        bindings.push(KeyBinding::new("ctrl-v", Paste, CONTEXT));
        // Readline and agent shortcuts that workspace bindings would take.
        // Ctrl+P is left to the file finder.
        for key in ["ctrl-w", "ctrl-b", "ctrl-o", "ctrl-f", "ctrl-h", "ctrl-n"] {
            bindings.push(KeyBinding::new(key, SendKeystroke(key), CONTEXT));
        }
    }
    bindings
}

pub enum TerminalEvent {
    /// Title, attention or progress changed.
    Changed,
    /// The shell exited.
    Exited,
    /// Ctrl+click on a file reference in the output.
    OpenLink(crate::FileLink),
    /// The program rang the bell or sent a notification while the terminal
    /// wasn't focused (an agent finished or needs input). Carries the
    /// notification's text, if it had one.
    Attention(Option<SharedString>),
    /// A command's output stopped for a moment: a good time to look at it
    /// (for errors). Only shells send it, not agents.
    OutputSettled,
}

/// How long output must pause before it counts as settled.
const SETTLE: std::time::Duration = std::time::Duration::from_millis(600);

/// What a remote terminal starts, and where.
#[derive(Clone)]
struct RemoteLaunch {
    connection: std::sync::Arc<remote::Connection>,
    folder: PathBuf,
    program: remote::Program,
}

pub struct TerminalView {
    pub(crate) focus_handle: FocusHandle,
    pty: Option<Pty>,
    error: Option<SharedString>,
    title: SharedString,
    needs_attention: bool,
    /// The message of the notification that set `needs_attention`, if any.
    notification: Option<SharedString>,
    progress: Option<Progress>,
    exited: bool,
    /// The agent harness this terminal runs instead of a shell.
    harness: Option<HarnessKind>,
    /// The harness exited; Enter starts it again.
    ended: bool,
    /// When the program last printed something.
    last_output: Option<std::time::Instant>,
    /// What started the terminal, to start a harness again.
    working_directory: Option<PathBuf>,
    shell: Option<ShellProfile>,
    pub(crate) layout: Option<GridLayout>,
    selecting: bool,
    /// Fractional wheel lines carried over between scroll events.
    scroll_remainder: f32,
    /// For a remote terminal: where and what to start again after the
    /// connection drops or the agent exits.
    remote: Option<RemoteLaunch>,
    /// The screen line the last command was typed on (prompt and command),
    /// marking where its output starts.
    command_line: Option<String>,
    /// A command was run here, so its output is worth checking.
    ran_command: bool,
    /// A command to type once the shell is ready (its startup output pauses).
    pending_command: Option<String>,
    /// Waits for output to pause, then sends [`TerminalEvent::OutputSettled`].
    settle: Option<Task<()>>,
    _events: Option<Task<()>>,
    _focus: Subscription,
}

impl EventEmitter<TerminalEvent> for TerminalView {}

impl Focusable for TerminalView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl TerminalView {
    pub fn new(
        working_directory: Option<PathBuf>,
        shell: Option<ShellProfile>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::create(working_directory, shell, None, window, cx)
    }

    /// A terminal running an agent harness. When the harness exits, the
    /// terminal stays open so its last output can be read.
    pub fn new_harness(
        working_directory: Option<PathBuf>,
        harness: &Harness,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut view = Self::create(working_directory, Some(harness.profile()), None, window, cx);
        view.harness = Some(harness.kind);
        view.title = harness.kind.name().into();
        view
    }

    /// A terminal on a remote project's server, running a shell or, with
    /// `harness`, that agent.
    pub fn new_remote(
        connection: std::sync::Arc<remote::Connection>,
        folder: PathBuf,
        program: remote::Program,
        harness: Option<HarnessKind>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut view = Self::create(Some(folder), None, Some((connection, program)), window, cx);
        if let Some(kind) = harness {
            view.harness = Some(kind);
            view.title = kind.name().into();
        }
        view
    }

    fn create(
        working_directory: Option<PathBuf>,
        shell: Option<ShellProfile>,
        remote: Option<(std::sync::Arc<remote::Connection>, remote::Program)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus_handle = cx.focus_handle();
        // Real size is applied on first paint.
        let size = GridSize {
            cols: 80,
            rows: 24,
            cell_width: 8,
            cell_height: 19,
        };
        let remote = remote.map(|(connection, program)| RemoteLaunch {
            connection,
            folder: working_directory
                .clone()
                .unwrap_or_else(|| PathBuf::from(".")),
            program,
        });
        let (pty, error, events) = Self::start_backend(
            working_directory.clone(),
            shell.as_ref(),
            remote.clone(),
            size,
            window,
            cx,
        );
        let focus = cx.on_focus(&focus_handle, window, |view, _, cx| {
            if view.needs_attention {
                view.needs_attention = false;
                view.notification = None;
                cx.emit(TerminalEvent::Changed);
            }
        });
        Self {
            focus_handle,
            pty,
            error,
            title: DEFAULT_TITLE.into(),
            needs_attention: false,
            notification: None,
            progress: None,
            exited: false,
            harness: None,
            ended: false,
            last_output: None,
            working_directory,
            shell,
            layout: None,
            selecting: false,
            scroll_remainder: 0.,
            remote,
            command_line: None,
            ran_command: false,
            pending_command: None,
            settle: None,
            _events: events,
            _focus: focus,
        }
    }

    /// Starts the shell and the task that feeds its output to the view.
    fn start_backend(
        working_directory: Option<PathBuf>,
        shell: Option<&ShellProfile>,
        remote: Option<RemoteLaunch>,
        size: GridSize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> (Option<Pty>, Option<SharedString>, Option<Task<()>>) {
        let backend = match remote {
            Some(launch) => {
                Pty::spawn_remote(launch.connection, launch.folder, &launch.program, size)
            }
            None => Pty::spawn(working_directory, shell, size),
        };
        match backend {
            Ok((pty, mut events)) => {
                let task = cx.spawn_in(window, async move |this, cx| {
                    while let Some(event) = events.next().await {
                        // Handle everything that's queued in one update, so a burst
                        // of output costs one redraw.
                        let mut batch = vec![event];
                        while let Ok(event) = events.try_recv() {
                            batch.push(event);
                        }
                        let handled = this.update_in(cx, |view, window, cx| {
                            view.handle_events(batch, window, cx)
                        });
                        if handled.is_err() {
                            break;
                        }
                    }
                });
                (Some(pty), None, Some(task))
            }
            Err(err) => (
                None,
                Some(format!("Failed to start the shell: {err}").into()),
                None,
            ),
        }
    }

    /// A remote terminal whose connection dropped starts a new shell (the
    /// connection reconnects first if needed).
    fn restart_remote(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let size = self.pty.as_ref().map(|pty| pty.size()).unwrap_or(GridSize {
            cols: 80,
            rows: 24,
            cell_width: 8,
            cell_height: 19,
        });
        let (pty, error, events) =
            Self::start_backend(None, None, self.remote.clone(), size, window, cx);
        self.pty = pty;
        self.error = error;
        self._events = events;
        cx.notify();
    }

    /// Starts the harness again, stopping it first if it is still running.
    pub fn restart(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.restart_harness(window, cx);
    }

    /// Whether the shell or harness is still running.
    pub fn is_running(&self) -> bool {
        self.pty.is_some() && !self.ended && !self.exited
    }

    /// When the program last printed something, to tell which of several
    /// agents is the busy one.
    pub fn last_output(&self) -> Option<std::time::Instant> {
        self.last_output
    }

    fn restart_harness(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(harness) = self.harness else {
            return;
        };
        let size = self.pty.as_ref().map(|pty| pty.size()).unwrap_or(GridSize {
            cols: 80,
            rows: 24,
            cell_width: 8,
            cell_height: 19,
        });
        let (pty, error, events) = Self::start_backend(
            self.working_directory.clone(),
            self.shell.as_ref(),
            self.remote.clone(),
            size,
            window,
            cx,
        );
        self.pty = pty;
        self.error = error;
        self._events = events;
        self.ended = false;
        self.title = harness.name().into();
        cx.emit(TerminalEvent::Changed);
        cx.notify();
    }

    /// The agent harness this terminal runs, if any.
    pub fn harness(&self) -> Option<HarnessKind> {
        self.harness
    }

    fn default_title(&self) -> SharedString {
        self.harness.map_or(DEFAULT_TITLE, HarnessKind::name).into()
    }

    /// Writes a dimmed line of Ion's own into the terminal.
    fn print_notice(&self, notice: &str) {
        if let Some(term) = self.term() {
            let text = format!("\r\n\x1b[0;2m[{notice}]\x1b[0m\r\n");
            let mut parser: alacritty_terminal::vte::ansi::Processor =
                alacritty_terminal::vte::ansi::Processor::new();
            parser.advance(&mut *term.lock(), text.as_bytes());
        }
    }

    pub fn title(&self) -> SharedString {
        self.title.clone()
    }

    /// The terminal rang its bell or sent a notification while it wasn't
    /// focused (e.g. an agent finished or is waiting for input).
    pub fn needs_attention(&self) -> bool {
        self.needs_attention
    }

    /// The text of the notification that needs attention.
    pub fn notification(&self) -> Option<SharedString> {
        self.notification.clone()
    }

    /// Progress the program reported with `OSC 9;4`, e.g. an agent working.
    pub fn progress(&self) -> Option<Progress> {
        self.progress
    }

    pub(crate) fn term(&self) -> Option<&SharedTerm> {
        self.pty.as_ref().map(|pty| &pty.term)
    }

    pub(crate) fn resize(&mut self, size: GridSize) {
        if let Some(pty) = &mut self.pty {
            pty.resize(size);
        }
    }

    fn handle_events(
        &mut self,
        events: Vec<PtyEvent>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut changed = false;
        let mut alert = None;
        for event in events {
            let event = match event {
                PtyEvent::Term(event) => event,
                PtyEvent::Signal(Signal::Notify { title, body }) => {
                    if !self.focus_handle.is_focused(window) {
                        self.needs_attention = true;
                        self.notification = Some(match title {
                            Some(title) => format!("{title}: {body}").into(),
                            None => body.into(),
                        });
                        alert = Some(self.notification.clone());
                        changed = true;
                    }
                    continue;
                }
                PtyEvent::Signal(Signal::Progress(progress)) => {
                    if self.progress != progress {
                        self.progress = progress;
                        changed = true;
                    }
                    continue;
                }
            };
            match event {
                TermEvent::Title(title) => {
                    self.title = display_title(&title);
                    changed = true;
                }
                TermEvent::ResetTitle => {
                    self.title = self.default_title();
                    changed = true;
                }
                TermEvent::Bell => {
                    if !self.focus_handle.is_focused(window) {
                        if !self.needs_attention && alert.is_none() {
                            alert = Some(None);
                        }
                        self.needs_attention = true;
                        changed = true;
                    }
                }
                TermEvent::PtyWrite(text) => self.write(text.into_bytes()),
                TermEvent::ClipboardStore(_, text) => {
                    cx.write_to_clipboard(ClipboardItem::new_string(text));
                }
                TermEvent::ClipboardLoad(_, format) => {
                    let text = cx
                        .read_from_clipboard()
                        .and_then(|item| item.text())
                        .unwrap_or_default();
                    self.write(format(&text).into_bytes());
                }
                TermEvent::ColorRequest(index, format) => {
                    if let Some(term) = self.term() {
                        let color = colors::by_index(index, term.lock().colors());
                        self.write(format(color).into_bytes());
                    }
                }
                TermEvent::TextAreaSizeRequest(format) => {
                    if let Some(pty) = &self.pty {
                        self.write(format(pty.size().window_size()).into_bytes());
                    }
                }
                TermEvent::ChildExit(_) | TermEvent::Exit => {
                    self.progress = None;
                    if let Some(harness) = self.harness {
                        if !self.ended {
                            self.ended = true;
                            let name = harness.name();
                            self.print_notice(&format!(
                                "{name} exited. Press Enter to start it again."
                            ));
                            if !self.focus_handle.is_focused(window) {
                                self.needs_attention = true;
                            }
                            changed = true;
                        }
                    } else if !self.exited {
                        self.exited = true;
                        cx.emit(TerminalEvent::Exited);
                    }
                }
                TermEvent::Wakeup => {
                    self.last_output = Some(std::time::Instant::now());
                    self.schedule_settle(cx);
                }
                TermEvent::MouseCursorDirty | TermEvent::CursorBlinkingChange => {}
            }
        }
        if changed {
            cx.emit(TerminalEvent::Changed);
        }
        if let Some(message) = alert {
            cx.emit(TerminalEvent::Attention(message));
        }
        cx.notify();
    }

    fn write(&self, bytes: Vec<u8>) {
        if let Some(pty) = &self.pty {
            pty.write(bytes);
        }
    }

    /// After output, waits until it pauses and then reports it settled.
    fn schedule_settle(&mut self, cx: &mut Context<Self>) {
        let waiting = self.ran_command || self.pending_command.is_some();
        if self.settle.is_some() || self.harness.is_some() || !waiting {
            return;
        }
        self.settle = Some(cx.spawn(async move |this, cx| {
            let mut wait = SETTLE;
            loop {
                cx.background_executor().timer(wait).await;
                let next = this.update(cx, |view, cx| {
                    let quiet_for = view.last_output.map_or(SETTLE, |at| at.elapsed());
                    if quiet_for >= SETTLE {
                        view.settle = None;
                        match view.pending_command.take() {
                            Some(command) => view.type_command(&command),
                            None => cx.emit(TerminalEvent::OutputSettled),
                        }
                        None
                    } else {
                        Some(SETTLE - quiet_for)
                    }
                });
                match next {
                    Ok(Some(remaining)) => wait = remaining,
                    _ => break,
                }
            }
        }));
    }

    /// Runs `command` at the prompt as soon as the shell is ready. Its
    /// output is scanned for problems like a command typed by hand.
    pub fn run_command(&mut self, command: &str, cx: &mut Context<Self>) {
        if self.last_output.is_some() && self.settle.is_none() {
            self.type_command(command);
        } else {
            // Typed once the startup output (prompt) has paused; the
            // first output schedules that.
            self.pending_command = Some(command.to_owned());
            if self.last_output.is_some() {
                self.schedule_settle(cx);
            }
        }
    }

    fn type_command(&mut self, command: &str) {
        // Everything after the prompt is this command's output.
        self.ran_command = true;
        self.command_line = None;
        self.write(format!("{command}\r").into_bytes());
    }

    /// The line the last command was typed on, prompt included.
    pub fn command_line(&self) -> Option<&str> {
        self.command_line.as_deref()
    }

    /// The output of the last command typed here: the lines below the line
    /// it was typed on (or the last `max_lines`), wrapped rows joined.
    pub fn command_output(&self, max_lines: usize) -> Vec<String> {
        use alacritty_terminal::term::cell::Flags;

        let Some(term) = self.term() else {
            return Vec::new();
        };
        let term = term.lock();
        let grid = term.grid();
        let top = -(grid.history_size() as i32);
        let bottom = term.screen_lines() as i32 - 1;
        let columns = term.columns();
        // Rows from the bottom up: (text, continues on the next row).
        let mut rows = Vec::new();
        let mut line = bottom;
        while line >= top && rows.len() < max_lines {
            let row = &grid[Line(line)];
            let text: String = (0..columns)
                .map(|col| &row[Column(col)])
                .filter(|cell| !cell.flags.intersects(Flags::WIDE_CHAR_SPACER))
                .map(|cell| cell.c)
                .collect();
            let text = text.trim_end().to_owned();
            if self.command_line.as_deref() == Some(text.as_str()) {
                break;
            }
            let wraps = row[Column(columns - 1)].flags.contains(Flags::WRAPLINE);
            rows.push((text, wraps));
            line -= 1;
        }
        rows.reverse();
        let mut lines: Vec<String> = Vec::new();
        let mut continued = false;
        for (text, wraps) in rows {
            match lines.last_mut() {
                Some(last) if continued => last.push_str(&text),
                _ => lines.push(text),
            }
            continued = wraps;
        }
        lines
    }

    /// Remembers the line a command is typed on, when Enter is pressed at
    /// a shell prompt.
    fn note_command(&mut self, bytes: &[u8]) {
        if self.harness.is_some() || !bytes.contains(&b'\r') {
            return;
        }
        let Some(term) = self.term() else {
            return;
        };
        let term = term.lock();
        if term.mode().contains(TermMode::ALT_SCREEN) {
            return;
        }
        let cursor = term.grid().cursor.point;
        let row = &term.grid()[cursor.line];
        let text: String = (0..term.columns()).map(|col| row[Column(col)].c).collect();
        let text = text.trim_end().to_owned();
        drop(term);
        self.ran_command = true;
        if !text.is_empty() {
            self.command_line = Some(text);
        }
    }

    /// Sends user input: jumps back to the live screen and drops the selection.
    fn input(&mut self, bytes: Vec<u8>, cx: &mut Context<Self>) {
        self.note_command(&bytes);
        let Some(pty) = &self.pty else {
            return;
        };
        {
            let mut term = pty.term.lock();
            term.scroll_display(Scroll::Bottom);
            term.selection = None;
        }
        pty.write(bytes);
        cx.notify();
    }

    fn mode(&self) -> TermMode {
        self.term()
            .map_or(TermMode::empty(), |term| *term.lock().mode())
    }

    fn has_selection(&self) -> bool {
        self.term().is_some_and(|term| {
            term.lock()
                .selection
                .as_ref()
                .is_some_and(|s| !s.is_empty())
        })
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
        let mods = &keystroke.modifiers;
        if self.ended {
            if keystroke.key == "enter" {
                self.restart_harness(window, cx);
            }
            cx.stop_propagation();
            return;
        }
        if self.remote.is_some() && self.pty.as_ref().is_some_and(Pty::is_lost) {
            if keystroke.key == "enter" {
                self.restart_remote(window, cx);
            }
            cx.stop_propagation();
            return;
        }
        // Ctrl+C copies when something is selected, and interrupts otherwise.
        if keystroke.key == "c" && mods.control && !mods.alt && !mods.shift && self.has_selection()
        {
            self.copy_selection(cx);
            cx.stop_propagation();
            return;
        }
        if let Some(bytes) = keys::encode(keystroke, self.mode()) {
            self.input(bytes, cx);
            cx.stop_propagation();
        }
    }

    fn send_keystroke(&mut self, action: &SendKeystroke, _: &mut Window, cx: &mut Context<Self>) {
        if let Ok(keystroke) = Keystroke::parse(action.0)
            && let Some(bytes) = keys::encode(&keystroke, self.mode())
        {
            self.input(bytes, cx);
        }
    }

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        self.copy_selection(cx);
    }

    fn copy_selection(&mut self, cx: &mut Context<Self>) {
        let Some(term) = self.term() else {
            return;
        };
        let text = {
            let mut term = term.lock();
            let text = term.selection_to_string();
            term.selection = None;
            text
        };
        if let Some(text) = text {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
        cx.notify();
    }

    /// Pastes text, or a copied image (a screenshot) as the path of a PNG
    /// saved for it, which agents read as an attachment.
    fn paste(&mut self, _: &Paste, _: &mut Window, cx: &mut Context<Self>) {
        let Some(item) = cx.read_from_clipboard() else {
            return;
        };
        if let Some(text) = item.text() {
            self.insert_text(&text, cx);
            return;
        }
        // The image is saved on this machine; a remote shell can't read it.
        if self.remote.is_some() {
            return;
        }
        let image = item.entries().iter().find_map(|entry| match entry {
            gpui::ClipboardEntry::Image(image) => Some(image.clone()),
            gpui::ClipboardEntry::String(_) => None,
        });
        let Some(image) = image else {
            return;
        };
        let save = cx.background_spawn(async move { save_pasted_image(&image) });
        cx.spawn(async move |this, cx| {
            if let Ok(path) = save.await {
                this.update(cx, |view, cx| {
                    view.insert_text(&format!("{} ", quote_path(&path)), cx)
                })
                .ok();
            }
        })
        .detach();
    }

    /// Types text into the shell as a paste (agents see it as one block,
    /// not as keystrokes that might submit early).
    pub fn insert_text(&mut self, text: &str, cx: &mut Context<Self>) {
        let text = text.replace("\r\n", "\r").replace('\n', "\r");
        let bytes = if self.mode().contains(TermMode::BRACKETED_PASTE) {
            // Strip any embedded end marker so pasted text can't escape the paste.
            format!("\x1b[200~{}\x1b[201~", text.replace("\x1b[201~", ""))
        } else {
            text
        };
        self.input(bytes.into_bytes(), cx);
    }

    // ---- mouse -------------------------------------------------------------

    fn grid_point(&self, position: Point<Pixels>) -> Option<(GridPoint, Side)> {
        let layout = self.layout?;
        let term = self.term()?.lock();
        let x = (position.x - layout.origin.x) / layout.cell.width;
        let y = (position.y - layout.origin.y) / layout.cell.height;
        let col = (x.max(0.) as usize).min(term.columns() - 1);
        let row = (y.max(0.) as usize).min(term.screen_lines() - 1);
        let side = if x.fract() < 0.5 {
            Side::Left
        } else {
            Side::Right
        };
        let line = Line(row as i32 - term.grid().display_offset() as i32);
        Some((GridPoint::new(line, Column(col)), side))
    }

    /// The web address at a grid point, if any.
    fn url_at(&self, point: GridPoint) -> Option<String> {
        let term = self.term()?.lock();
        let row = &term.grid()[point.line];
        let text: String = (0..term.columns()).map(|col| row[Column(col)].c).collect();
        crate::url_at(&text, point.column.0)
    }

    /// The file reference at a grid point, if any.
    fn link_at(&self, point: GridPoint) -> Option<crate::FileLink> {
        let term = self.term()?.lock();
        let row = &term.grid()[point.line];
        let text: String = (0..term.columns()).map(|col| row[Column(col)].c).collect();
        crate::link_at(&text, point.column.0)
    }

    fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus_handle);
        let Some((point, side)) = self.grid_point(event.position) else {
            return;
        };
        // Ctrl+click opens a web address or a file named in the output.
        if event.modifiers.secondary() {
            if let Some(url) = self.url_at(point) {
                cx.open_url(&url);
                return;
            }
            if let Some(link) = self.link_at(point) {
                cx.emit(TerminalEvent::OpenLink(link));
                return;
            }
        }
        let kind = match event.click_count {
            2 => SelectionType::Semantic,
            3.. => SelectionType::Lines,
            _ => SelectionType::Simple,
        };
        if let Some(term) = self.term() {
            term.lock().selection = Some(Selection::new(kind, point, side));
        }
        self.selecting = true;
        cx.notify();
    }

    fn on_mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if !self.selecting || event.pressed_button != Some(MouseButton::Left) {
            return;
        }
        let Some((point, side)) = self.grid_point(event.position) else {
            return;
        };
        if let Some(term) = self.term()
            && let Some(selection) = term.lock().selection.as_mut()
        {
            selection.update(point, side);
        }
        cx.notify();
    }

    fn on_mouse_up(&mut self, _: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.selecting = false;
        let mut copied = None;
        if let Some(term) = self.term() {
            let mut term = term.lock();
            if term.selection.as_ref().is_some_and(Selection::is_empty) {
                term.selection = None;
            } else if term.selection.is_some() && settings::get(cx).terminal_copy_on_select {
                copied = term.selection_to_string();
            }
        }
        if let Some(text) = copied {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
        cx.notify();
    }

    /// Right click copies the selection, or pastes when nothing is selected.
    fn on_right_click(&mut self, _: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus_handle);
        if self.has_selection() {
            self.copy(&Copy, window, cx);
        } else {
            self.paste(&Paste, window, cx);
        }
    }

    fn on_scroll_wheel(
        &mut self,
        event: &ScrollWheelEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(layout) = self.layout else {
            return;
        };
        let lines = event.delta.pixel_delta(layout.cell.height).y / layout.cell.height
            + self.scroll_remainder;
        self.scroll_remainder = lines.fract();
        let lines = lines.trunc() as i32;
        if lines == 0 {
            return;
        }
        let mode = self.mode();
        if mode.contains(TermMode::ALT_SCREEN | TermMode::ALTERNATE_SCROLL)
            && !event.modifiers.shift
        {
            // Full-screen apps (less, vim, agent TUIs) get arrow keys instead.
            let arrow = match (lines > 0, mode.contains(TermMode::APP_CURSOR)) {
                (true, true) => "\x1bOA",
                (true, false) => "\x1b[A",
                (false, true) => "\x1bOB",
                (false, false) => "\x1b[B",
            };
            self.write(arrow.repeat(lines.unsigned_abs() as usize).into_bytes());
        } else if let Some(term) = self.term() {
            term.lock().scroll_display(Scroll::Delta(lines));
        }
        cx.notify();
        cx.stop_propagation();
    }
}

/// Shells often set the title to their executable path
/// (`C:\Program Files\PowerShell\7\pwsh.exe`); show just `pwsh` then.
fn display_title(title: &str) -> SharedString {
    let path = Path::new(title.trim());
    let is_executable = path
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("exe"));
    match path.file_stem() {
        Some(stem) if path.is_absolute() && is_executable => {
            stem.to_string_lossy().into_owned().into()
        }
        _ => title.to_owned().into(),
    }
}

/// Saves a pasted image under the temp folder and returns its path.
fn save_pasted_image(image: &gpui::Image) -> std::io::Result<PathBuf> {
    use gpui::ImageFormat;
    let extension = match image.format() {
        ImageFormat::Jpeg => "jpg",
        ImageFormat::Gif => "gif",
        ImageFormat::Webp => "webp",
        ImageFormat::Svg => "svg",
        ImageFormat::Bmp => "bmp",
        ImageFormat::Tiff => "tiff",
        _ => "png",
    };
    let dir = std::env::temp_dir().join("ion-pastes");
    std::fs::create_dir_all(&dir)?;
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |time| time.as_millis());
    let path = dir.join(format!("paste-{millis}.{extension}"));
    std::fs::write(&path, image.bytes())?;
    Ok(path)
}

/// A path as typed into a shell: quoted when it has spaces or quotes.
pub fn quote_path(path: &Path) -> String {
    let text = path.to_string_lossy();
    if text.contains([' ', '\'', '"', '&', '(', ')', ';']) {
        format!("\"{}\"", text.replace('"', "\\\""))
    } else {
        text.into_owned()
    }
}

impl EntityInputHandler for TerminalView {
    fn text_for_range(
        &mut self,
        _: Range<usize>,
        _: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        None
    }

    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: 0..0,
            reversed: false,
        })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        None
    }

    fn unmark_text(&mut self, _: &mut Window, _: &mut Context<Self>) {}

    /// Typed text (including IME results) goes straight to the shell.
    fn replace_text_in_range(
        &mut self,
        _: Option<Range<usize>>,
        text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.input(text.as_bytes().to_vec(), cx);
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        _: Option<Range<usize>>,
        _: &str,
        _: Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) {
    }

    /// Places IME candidate windows at the cursor.
    fn bounds_for_range(
        &mut self,
        _: Range<usize>,
        _: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        self.layout?.cursor
    }

    fn character_index_for_point(
        &mut self,
        _: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        None
    }
}

impl Render for TerminalView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let focused = self.focus_handle.is_focused(window);
        div()
            .key_context("Terminal")
            .track_focus(&self.focus_handle)
            .size_full()
            .overflow_hidden()
            .bg(theme::bg())
            .cursor(CursorStyle::IBeam)
            .on_key_down(cx.listener(Self::on_key_down))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::send_keystroke))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_down(MouseButton::Right, cx.listener(Self::on_right_click))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_scroll_wheel(cx.listener(Self::on_scroll_wheel))
            .map(|terminal| match &self.error {
                Some(error) => terminal
                    .p_4()
                    .text_color(theme::text_muted())
                    .child(error.clone()),
                None => terminal.child(TerminalElement::new(cx.entity(), focused)),
            })
    }
}

#[cfg(test)]
mod tests {
    use super::display_title;

    #[test]
    fn executable_paths_become_short_titles() {
        #[cfg(windows)]
        assert_eq!(
            display_title(r"C:\Program Files\PowerShell\7\pwsh.exe"),
            "pwsh"
        );
        assert_eq!(display_title("npm run dev"), "npm run dev");
    }
}
