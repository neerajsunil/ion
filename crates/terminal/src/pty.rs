//! The terminal backend: a shell running in a pseudo-terminal, parsed by
//! `alacritty_terminal`.
//!
//! Each terminal has one I/O thread (alacritty's event loop) that blocks on the
//! PTY and costs nothing while the shell is quiet. Events are forwarded to the
//! UI thread over a channel, together with the notification and progress
//! signals found in the output (see [`crate::osc`]).

use std::borrow::Cow;
use std::collections::HashMap;
use std::io;
use std::path::PathBuf;
use std::sync::Arc;

use alacritty_terminal::Term;
use alacritty_terminal::event::{Event, EventListener, OnResize, WindowSize};
use alacritty_terminal::event_loop::{EventLoop, EventLoopSender, Msg};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::Config;
use alacritty_terminal::tty::{self, ChildEvent, EventedPty, EventedReadWrite};
use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use polling::{PollMode, Poller};

use crate::osc::{OscScanner, Signal};

/// Extra environment variables for local shells, e.g. the port agents use
/// to reach Ion's IDE integration.
static EXTRA_ENV: std::sync::Mutex<Vec<(String, String)>> = std::sync::Mutex::new(Vec::new());

/// Sets environment variables every local terminal started from now on gets.
pub fn set_extra_env(vars: Vec<(String, String)>) {
    if let Ok(mut env) = EXTRA_ENV.lock() {
        *env = vars;
    }
}

pub(crate) enum PtyEvent {
    Term(Event),
    Signal(Signal),
}

/// Forwards terminal events from the I/O thread to the UI thread.
#[derive(Clone)]
pub(crate) struct Listener(UnboundedSender<PtyEvent>);

impl Listener {
    fn signal(&self, signal: Signal) {
        let _ = self.0.unbounded_send(PtyEvent::Signal(signal));
    }
}

impl EventListener for Listener {
    fn send_event(&self, event: Event) {
        // The receiver is gone only when the view was dropped.
        let _ = self.0.unbounded_send(PtyEvent::Term(event));
    }
}

/// A local PTY whose output passes through an [`OscScanner`] on the I/O
/// thread, just before alacritty parses it: alacritty drops the codes the
/// scanner looks for.
struct ScanningPty {
    inner: tty::Pty,
    scanner: OscScanner,
    listener: Listener,
}

impl io::Read for ScanningPty {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let read = self.inner.reader().read(buf)?;
        let listener = &self.listener;
        self.scanner
            .scan(&buf[..read], &mut |signal| listener.signal(signal));
        Ok(read)
    }
}

impl EventedReadWrite for ScanningPty {
    type Reader = Self;
    type Writer = <tty::Pty as EventedReadWrite>::Writer;

    // The trait method is unsafe; this is Ion's only unsafe code.
    #[allow(unsafe_code)]
    unsafe fn register(
        &mut self,
        poll: &Arc<Poller>,
        interest: polling::Event,
        mode: PollMode,
    ) -> io::Result<()> {
        // SAFETY: the caller guarantees the sources outlive their
        // registration. They belong to `inner`, which lives as long as `self`.
        unsafe { self.inner.register(poll, interest, mode) }
    }

    fn reregister(
        &mut self,
        poll: &Arc<Poller>,
        interest: polling::Event,
        mode: PollMode,
    ) -> io::Result<()> {
        self.inner.reregister(poll, interest, mode)
    }

    fn deregister(&mut self, poll: &Arc<Poller>) -> io::Result<()> {
        self.inner.deregister(poll)
    }

    fn reader(&mut self) -> &mut Self {
        self
    }

    fn writer(&mut self) -> &mut Self::Writer {
        self.inner.writer()
    }
}

impl EventedPty for ScanningPty {
    fn next_child_event(&mut self) -> Option<ChildEvent> {
        self.inner.next_child_event()
    }
}

impl OnResize for ScanningPty {
    fn on_resize(&mut self, window_size: WindowSize) {
        self.inner.on_resize(window_size);
    }
}

/// Terminal size in cells, plus cell size in pixels (needed by some programs).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct GridSize {
    pub cols: usize,
    pub rows: usize,
    pub cell_width: u16,
    pub cell_height: u16,
}

impl GridSize {
    pub fn window_size(&self) -> WindowSize {
        WindowSize {
            num_lines: self.rows as u16,
            num_cols: self.cols as u16,
            cell_width: self.cell_width,
            cell_height: self.cell_height,
        }
    }
}

impl Dimensions for GridSize {
    fn total_lines(&self) -> usize {
        self.rows
    }

    fn screen_lines(&self) -> usize {
        self.rows
    }

    fn columns(&self) -> usize {
        self.cols
    }
}

pub(crate) type SharedTerm = Arc<FairMutex<Term<Listener>>>;

pub(crate) struct Pty {
    pub term: SharedTerm,
    sender: Sender,
    size: GridSize,
    /// A remote shell whose SSH connection dropped.
    lost: Arc<std::sync::atomic::AtomicBool>,
}

enum Sender {
    Local(EventLoopSender),
    Remote(remote::Terminal),
}
impl Sender {
    fn send(&self, message: Msg) {
        match self {
            Self::Local(sender) => {
                let _ = sender.send(message);
            }
            Self::Remote(terminal) => match message {
                Msg::Input(bytes) => terminal.write(bytes.into_owned()),
                Msg::Resize(size) => terminal.resize(remote_size(size)),
                Msg::Shutdown => terminal.close(),
            },
        }
    }
}
fn remote_size(size: WindowSize) -> remote::TerminalSize {
    remote::TerminalSize {
        cols: size.num_cols.into(),
        rows: size.num_lines.into(),
        width: u32::from(size.num_cols) * u32::from(size.cell_width),
        height: u32::from(size.num_lines) * u32::from(size.cell_height),
    }
}

impl Pty {
    pub fn spawn(
        working_directory: Option<PathBuf>,
        shell: Option<&ShellProfile>,
        size: GridSize,
    ) -> io::Result<(Self, UnboundedReceiver<PtyEvent>)> {
        let (tx, rx) = unbounded();
        let listener = Listener(tx);
        let mut env = HashMap::from([
            ("TERM".to_owned(), "xterm-256color".to_owned()),
            ("COLORTERM".to_owned(), "truecolor".to_owned()),
            ("TERM_PROGRAM".to_owned(), "ion".to_owned()),
        ]);
        if let Ok(extra) = EXTRA_ENV.lock() {
            env.extend(extra.iter().cloned());
        }
        let options = tty::Options {
            shell: shell
                .map(|shell| tty::Shell::new(shell.program.clone(), shell.args.clone()))
                .or_else(default_shell),
            working_directory,
            env,
            ..Default::default()
        };
        let pty = ScanningPty {
            inner: tty::new(&options, size.window_size(), 0)?,
            scanner: OscScanner::default(),
            listener: listener.clone(),
        };
        let term = Arc::new(FairMutex::new(Term::new(
            Config::default(),
            &size,
            listener.clone(),
        )));
        let event_loop = EventLoop::new(term.clone(), listener, pty, false, false)?;
        let sender = event_loop.channel();
        // The thread exits on its own when the shell exits or we send Shutdown.
        drop(event_loop.spawn());
        Ok((
            Self {
                term,
                sender: Sender::Local(sender),
                size,
                lost: Arc::default(),
            },
            rx,
        ))
    }

    pub fn spawn_remote(
        connection: Arc<remote::Connection>,
        folder: PathBuf,
        program: &remote::Program,
        size: GridSize,
    ) -> io::Result<(Self, UnboundedReceiver<PtyEvent>)> {
        let (tx, rx) = unbounded();
        let listener = Listener(tx);
        let term = Arc::new(FairMutex::new(Term::new(
            Config::default(),
            &size,
            listener.clone(),
        )));
        let screen = term.clone();
        let lost = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let lost_flag = lost.clone();
        let mut parser: alacritty_terminal::vte::ansi::Processor =
            alacritty_terminal::vte::ansi::Processor::new();
        let mut scanner = OscScanner::default();
        let terminal = remote::Terminal::spawn(
            connection,
            folder,
            program,
            remote_size(size.window_size()),
            move |event| {
                match event {
                    remote::TerminalEvent::Data(bytes) => {
                        scanner.scan(&bytes, &mut |signal| listener.signal(signal));
                        parser.advance(&mut *screen.lock(), &bytes);
                        listener.send_event(Event::Wakeup);
                    }
                    remote::TerminalEvent::Exited => listener.send_event(Event::Exit),
                    remote::TerminalEvent::Flush => {
                        parser.stop_sync(&mut *screen.lock());
                        listener.send_event(Event::Wakeup);
                    }
                    remote::TerminalEvent::Error(error) => {
                        lost_flag.store(true, std::sync::atomic::Ordering::Release);
                        let message = format!(
                            "\r\n\x1b[0;2m[{error}. Press Enter to start a new shell.]\x1b[0m\r\n"
                        );
                        parser.advance(&mut *screen.lock(), message.as_bytes());
                        listener.send_event(Event::Wakeup);
                    }
                }
                parser.sync_timeout().sync_timeout()
            },
        )?;
        Ok((
            Self {
                term,
                sender: Sender::Remote(terminal),
                size,
                lost,
            },
            rx,
        ))
    }

    pub fn is_lost(&self) -> bool {
        self.lost.load(std::sync::atomic::Ordering::Acquire)
    }

    pub fn write(&self, bytes: impl Into<Cow<'static, [u8]>>) {
        self.sender.send(Msg::Input(bytes.into()));
    }

    pub fn resize(&mut self, size: GridSize) {
        if size == self.size {
            return;
        }
        self.size = size;
        self.sender.send(Msg::Resize(size.window_size()));
        self.term.lock().resize(size);
    }

    pub fn size(&self) -> GridSize {
        self.size
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        self.sender.send(Msg::Shutdown);
    }
}

/// A shell a terminal can run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShellProfile {
    /// What menus show, e.g. "PowerShell 7".
    pub name: String,
    pub program: String,
    pub args: Vec<String>,
}

impl ShellProfile {
    /// A profile for a program the user named in settings: a known shell's
    /// name ("Git Bash"), or a program path or command.
    pub fn from_setting(setting: &str) -> Option<Self> {
        let setting = setting.trim();
        if setting.is_empty() {
            return None;
        }
        if let Some(known) = available_shells()
            .into_iter()
            .find(|shell| shell.name.eq_ignore_ascii_case(setting))
        {
            return Some(known);
        }
        Some(Self {
            name: setting.to_owned(),
            program: setting.to_owned(),
            args: Vec::new(),
        })
    }
}

/// The shells installed on this machine, the default first.
pub fn available_shells() -> Vec<ShellProfile> {
    let shell = |name: &str, program: String, args: &[&str]| ShellProfile {
        name: name.to_owned(),
        program,
        args: args.iter().map(|arg| (*arg).to_owned()).collect(),
    };
    let mut shells = Vec::new();
    if cfg!(windows) {
        if let Some(pwsh) = find_in_path("pwsh.exe") {
            shells.push(shell("PowerShell 7", pwsh, &["-NoLogo"]));
        }
        if let Some(powershell) = find_in_path("powershell.exe") {
            shells.push(shell("Windows PowerShell", powershell, &["-NoLogo"]));
        }
        if let Some(cmd) = find_in_path("cmd.exe") {
            shells.push(shell("Command Prompt", cmd, &[]));
        }
        let git_bash = std::path::Path::new(r"C:\Program Files\Git\bin\bash.exe");
        if git_bash.is_file() {
            let program = git_bash.to_string_lossy().into_owned();
            shells.push(shell("Git Bash", program, &["--login", "-i"]));
        }
        if let Some(wsl) = find_in_path("wsl.exe") {
            shells.push(shell("WSL", wsl, &[]));
        }
    } else {
        let login = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_owned());
        let name = login.rsplit('/').next().unwrap_or("shell").to_owned();
        shells.push(shell(&name, login, &["-l"]));
    }
    shells
}

/// PowerShell 7 if installed, otherwise Windows PowerShell. Other platforms use
/// the user's login shell (alacritty's default).
fn default_shell() -> Option<tty::Shell> {
    if !cfg!(windows) {
        return None;
    }
    let program = find_in_path("pwsh.exe").unwrap_or_else(|| "powershell.exe".to_owned());
    Some(tty::Shell::new(program, vec!["-NoLogo".to_owned()]))
}

fn find_in_path(exe: &str) -> Option<String> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(exe))
        .find(|candidate| candidate.is_file())
        .map(|found| found.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use futures::channel::mpsc::TryRecvError;

    use super::*;
    use crate::osc::{Progress, ProgressState};

    /// The codes reach the scanner through a real PTY (ConPTY on Windows,
    /// which re-renders output and could drop them).
    #[test]
    fn signals_pass_through_the_pty() {
        let esc = "[char]27+";
        // A literal backslash would be lost in PowerShell's argument parsing.
        let st = "+[char]27+[char]92";
        let script = [
            format!("{esc}']9;iterm'+[char]7"),
            format!("{esc}']777;notify;Title;rxvt'+[char]7"),
            format!("{esc}']99;;kitty'{st}"),
            format!("{esc}']9;4;3'+[char]7"),
        ]
        .join("+");
        let shell = if cfg!(windows) {
            let program = find_in_path("pwsh.exe")
                .or_else(|| find_in_path("powershell.exe"))
                .expect("PowerShell");
            // No spaces or double quotes: Windows arguments are passed raw.
            let command = format!("[Console]::Write({script});Start-Sleep;-Milliseconds;300");
            ShellProfile {
                name: "test".to_owned(),
                program,
                args: vec!["-NoProfile".to_owned(), "-Command".to_owned(), command],
            }
        } else {
            let command = concat!(
                r"printf '\033]9;iterm\007\033]777;notify;Title;rxvt\007",
                r"\033]99;;kitty\033\\\033]9;4;3\007'",
            );
            ShellProfile {
                name: "test".to_owned(),
                program: "/bin/sh".to_owned(),
                args: vec!["-c".to_owned(), command.to_owned()],
            }
        };
        let size = GridSize {
            cols: 80,
            rows: 24,
            cell_width: 8,
            cell_height: 16,
        };
        let (_pty, mut events) = Pty::spawn(None, Some(&shell), size).expect("spawn");
        let mut signals = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(20);
        while signals.len() < 4 && Instant::now() < deadline {
            match events.try_recv() {
                Ok(PtyEvent::Signal(signal)) => signals.push(signal),
                Ok(PtyEvent::Term(_)) => {}
                Err(TryRecvError::Closed) => break,
                Err(TryRecvError::Empty) => std::thread::sleep(Duration::from_millis(10)),
            }
        }
        let notify = |title: Option<&str>, body: &str| Signal::Notify {
            title: title.map(str::to_owned),
            body: body.to_owned(),
        };
        assert_eq!(
            signals,
            [
                notify(None, "iterm"),
                notify(Some("Title"), "rxvt"),
                notify(None, "kitty"),
                Signal::Progress(Some(Progress {
                    state: ProgressState::Indeterminate,
                    percent: None,
                })),
            ]
        );
    }
}
