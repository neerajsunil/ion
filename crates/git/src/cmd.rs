//! Running the `git` executable.

use std::fmt;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

#[derive(Debug)]
pub enum GitError {
    /// `git` couldn't be started (not installed or not on PATH).
    NotFound(std::io::Error),
    /// git ran and failed; holds its error output.
    Failed(String),
}

impl fmt::Display for GitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound(err) => write!(f, "couldn't run git: {err}"),
            Self::Failed(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for GitError {}

pub type Result<T> = std::result::Result<T, GitError>;

/// Runs git in `dir` and returns its output. Blocks; call from a background
/// thread.
pub(crate) fn run(dir: &Path, args: &[&str]) -> Result<Vec<u8>> {
    run_with_input(dir, args, None)
}

pub(crate) fn run_with_input(dir: &Path, args: &[&str], input: Option<&[u8]>) -> Result<Vec<u8>> {
    let mut command = Command::new("git");
    command
        .current_dir(dir)
        // Stable, uncolored, unquoted output regardless of user config.
        .args(["-c", "core.quotepath=false", "-c", "color.ui=false"])
        .args(args)
        // Never block on a credential or editor prompt.
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_EDITOR", "true")
        // Reads (like status) must not write the index: that would wake our
        // own file watcher and refresh again.
        .env("GIT_OPTIONAL_LOCKS", "0")
        // Paths are file names, never glob patterns (`[id].tsx`).
        .env("GIT_LITERAL_PATHSPECS", "1")
        .env("LC_ALL", "C")
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // Don't flash a console window from the GUI app.
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }

    let mut child = command.spawn().map_err(GitError::NotFound)?;
    if let Some(input) = input {
        let mut stdin = child.stdin.take().expect("stdin is piped");
        // Write on another thread so a large input can't deadlock against
        // git filling its output pipe.
        let input = input.to_vec();
        std::thread::spawn(move || stdin.write_all(&input));
    }
    let output = child.wait_with_output().map_err(GitError::NotFound)?;
    if output.status.success() {
        Ok(output.stdout)
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let message = if stderr.trim().is_empty() {
            stdout.trim()
        } else {
            stderr.trim()
        };
        Err(GitError::Failed(message.to_owned()))
    }
}

pub(crate) fn run_remote(
    connection: &remote::Connection,
    dir: &Path,
    args: &[&str],
    input: Option<&[u8]>,
) -> Result<Vec<u8>> {
    let args = args
        .iter()
        .map(|arg| remote::shell_quote(arg))
        .collect::<Vec<_>>()
        .join(" ");
    let command = format!(
        "cd -- {} && GIT_TERMINAL_PROMPT=0 GIT_EDITOR=true GIT_OPTIONAL_LOCKS=0 GIT_LITERAL_PATHSPECS=1 LC_ALL=C git -c core.quotepath=false -c color.ui=false {args}",
        remote::shell_quote(&remote::posix_path(dir).to_string_lossy())
    );
    let output = connection
        .execute(&command, input)
        .map_err(GitError::NotFound)?;
    if output.status == 0 {
        Ok(output.stdout)
    } else {
        let message = if output.stderr.is_empty() {
            &output.stdout
        } else {
            &output.stderr
        };
        Err(GitError::Failed(
            String::from_utf8_lossy(message).trim().to_owned(),
        ))
    }
}

pub(crate) fn run_text(dir: &Path, args: &[&str]) -> Result<String> {
    run(dir, args).map(|out| String::from_utf8_lossy(&out).into_owned())
}
