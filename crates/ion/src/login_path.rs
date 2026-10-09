//! Apps started from Finder or the Dock get launchd's minimal PATH
//! (`/usr/bin:/bin:/usr/sbin:/sbin`), so agents, Homebrew tools and Git hooks
//! wouldn't be found. Ion then reads PATH from the user's login shell and
//! restarts itself with it, so every child process sees the same PATH as in
//! Terminal. Started from a terminal, PATH is already right and this does
//! nothing.

use std::io::Read;
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

/// Set on the restarted process, so it never tries again.
const DONE: &str = "ION_LOGIN_PATH";
const MARKER: &str = "__ION_PATH__";

/// Restarts Ion with the login shell's PATH if it was started outside a
/// terminal. Returns only if there's nothing to do or it failed.
pub fn restart_with_login_path() {
    if std::env::var_os(DONE).is_some() || std::env::var_os("TERM").is_some() {
        return;
    }
    let Some(path) = login_path() else { return };
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    // `exec` only returns on failure; Ion then carries on with the old PATH.
    let _ = Command::new(exe)
        .args(std::env::args_os().skip(1))
        .env("PATH", path)
        .env(DONE, "1")
        .exec();
}

/// Longest wait for the login shell; a startup file that never returns (one
/// that attaches to tmux, say) leaves Ion on launchd's PATH.
const TIMEOUT: Duration = Duration::from_secs(5);

/// PATH as an interactive login shell sets it (`.zprofile` and `.zshrc`).
/// `printenv` prints the exported, colon-separated value in any shell (fish
/// would join `$PATH` with spaces).
fn login_path() -> Option<String> {
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".to_owned());
    let mut child = Command::new(shell)
        .args([
            "-l",
            "-i",
            "-c",
            &format!("printf '{MARKER}'; /usr/bin/printenv PATH; printf '{MARKER}'"),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let mut output = Vec::new();
        let _ = stdout.read_to_end(&mut output);
        let _ = sender.send(output);
    });
    let output = receiver.recv_timeout(TIMEOUT);
    if output.is_err() {
        let _ = child.kill();
    }
    let _ = child.wait();
    parse(&String::from_utf8_lossy(&output.ok()?))
}

/// The PATH between the markers; startup files may print around it.
fn parse(stdout: &str) -> Option<String> {
    let (_, rest) = stdout.split_once(MARKER)?;
    let (path, _) = rest.split_once(MARKER)?;
    let path = path.trim_end_matches('\n');
    (!path.is_empty()).then(|| path.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_path_between_startup_noise() {
        let out = format!("Welcome!\n{MARKER}/opt/homebrew/bin:/usr/bin\n{MARKER}\nbye");
        assert_eq!(parse(&out).as_deref(), Some("/opt/homebrew/bin:/usr/bin"));
        assert_eq!(parse(&format!("{MARKER}{MARKER}")), None);
        assert_eq!(parse("no markers"), None);
    }
}
