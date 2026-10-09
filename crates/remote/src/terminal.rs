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

pub struct Terminal {
    mux: Arc<Mux>,
    id: ChannelId,
}

impl Terminal {
    /// Starts the login shell in `folder`. `output` runs on the connection's
    /// reader thread; it can ask for a one-shot `Flush` by returning a deadline,
    /// and returns `None` while idle (no timers).
    pub fn spawn(
        connection: Arc<Connection>,
        folder: PathBuf,
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
                command: shell_command(&folder),
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

fn shell_command(folder: &std::path::Path) -> String {
    format!(
        "cd -- {} && exec \"${{SHELL:-/bin/sh}}\" -l",
        shell_quote(&posix_path(folder).to_string_lossy())
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn shell_directory_is_one_escaped_argument() {
        assert_eq!(
            shell_command(std::path::Path::new("/home/dev/my 'project")),
            "cd -- '/home/dev/my '\\''project' && exec \"${SHELL:-/bin/sh}\" -l"
        );
    }
}
