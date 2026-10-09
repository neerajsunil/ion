//! Remote terminals: a PTY stream on the project's SSH connection, so a new
//! terminal never logs in again.
use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use crate::mux::{ChannelId, Event, Kind, Mux, Sink};
use crate::{Connection, posix_path, shell_quote};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TerminalSize {
    pub cols: u32,
    pub rows: u32,
    pub width: u32,
    pub height: u32,
}

pub enum TerminalEvent {
    Data(Vec<u8>),
    /// Flush a pending synchronized screen update when its deadline expires.
    Flush,
    Exited,
    /// The terminal ended because of an error (for example the connection
    /// dropped).
    Error(String),
}

/// What a remote terminal runs, in the project folder.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Program {
    /// The user's login shell.
    #[default]
    LoginShell,
    /// Another shell, by path, as a login shell.
    Shell(String),
    /// A command such as `claude`, run the way the login shell runs what's
    /// typed into it, so its `PATH` and environment apply.
    Command(String),
}

pub struct Terminal {
    mux: Arc<Mux>,
    id: ChannelId,
}

impl Terminal {
    /// Starts `program` in `folder`. `output` runs on the connection's
    /// reader thread; it can ask for a one-shot `Flush` by returning a deadline,
    /// and returns `None` while idle (no timers).
    pub fn spawn(
        connection: Arc<Connection>,
        folder: PathBuf,
        program: &Program,
        size: TerminalSize,
        mut output: impl FnMut(TerminalEvent) -> Option<Instant> + Send + 'static,
    ) -> io::Result<Self> {
        let sink: Sink = Box::new(move |event| match event {
            Event::Stdout(bytes) | Event::Stderr(bytes) => {
                output(TerminalEvent::Data(bytes.to_vec()))
            }
            Event::Watch(_) => None,
            Event::Tick => output(TerminalEvent::Flush),
            Event::Closed(_) => output(TerminalEvent::Exited),
            // Closed by us: nothing to report.
            Event::Failed(error) if error.kind() == io::ErrorKind::Interrupted => None,
            Event::Failed(error) => output(TerminalEvent::Error(error.to_string())),
        });
        let (mux, id) = connection.open(
            Kind::Exec {
                command: shell_command(&folder, program, &connection.terminal_env()),
                pty: Some(size),
                merge_stderr: true,
            },
            sink,
        )?;
        Ok(Self { mux, id })
    }

    pub fn write(&self, bytes: Vec<u8>) {
        // A dead link is reported through the sink.
        let _ = self.mux.write(self.id, &bytes);
    }

    pub fn resize(&self, size: TerminalSize) {
        self.mux.resize(self.id, size);
    }

    pub fn close(&self) {
        self.mux.close(self.id);
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        self.close();
    }
}

fn shell_command(folder: &std::path::Path, program: &Program, env: &[(String, String)]) -> String {
    let run = match program {
        Program::LoginShell => "\"${SHELL:-/bin/sh}\" -l".to_owned(),
        Program::Shell(path) => format!("{} -l", shell_quote(path)),
        Program::Command(command) => format!(
            "\"${{SHELL:-/bin/sh}}\" -lic {}",
            shell_quote(&format!("exec {}", shell_quote(command)))
        ),
    };
    let exports: String = env
        .iter()
        .filter(|(name, _)| {
            !name.is_empty() && name.bytes().all(|b| b == b'_' || b.is_ascii_alphanumeric())
        })
        .map(|(name, value)| format!("export {name}={}; ", shell_quote(value)))
        .collect();
    format!(
        "cd -- {} && {exports}exec {run}",
        shell_quote(&posix_path(folder).to_string_lossy())
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn shell_directory_is_one_escaped_argument() {
        assert_eq!(
            shell_command(
                std::path::Path::new("/home/dev/my 'project"),
                &Program::LoginShell,
                &[]
            ),
            "cd -- '/home/dev/my '\\''project' && exec \"${SHELL:-/bin/sh}\" -l"
        );
        let folder = std::path::Path::new("/p");
        assert_eq!(
            shell_command(folder, &Program::Shell("/bin/zsh".into()), &[]),
            "cd -- '/p' && exec '/bin/zsh' -l"
        );
        assert_eq!(
            shell_command(
                folder,
                &Program::Command("claude".into()),
                &[
                    ("PORT".into(), "1 2".into()),
                    ("bad;name".into(), "x".into())
                ]
            ),
            "cd -- '/p' && export PORT='1 2'; exec \"${SHELL:-/bin/sh}\" -lic 'exec '\\''claude'\\'''"
        );
    }
}
