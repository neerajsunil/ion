//! Agent harnesses Ion can launch in a terminal: the CLIs installed on this
//! machine, found the way a shell would find them.

use std::path::{Path, PathBuf};

use crate::pty::ShellProfile;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum HarnessKind {
    ClaudeCode,
    Codex,
}

impl HarnessKind {
    pub const ALL: [Self; 2] = [Self::ClaudeCode, Self::Codex];

    /// What menus and tabs show.
    pub fn name(self) -> &'static str {
        match self {
            Self::ClaudeCode => "Claude Code",
            Self::Codex => "Codex",
        }
    }

    /// The command that starts it, and the id saved in sessions.
    pub fn command(self) -> &'static str {
        match self {
            Self::ClaudeCode => "claude",
            Self::Codex => "codex",
        }
    }

    pub fn from_command(command: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.command() == command)
    }
}

/// An installed harness.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Harness {
    pub kind: HarnessKind,
    /// The executable or script that starts it.
    pub path: PathBuf,
}

impl Harness {
    /// How to start it in a PTY.
    pub(crate) fn profile(&self) -> ShellProfile {
        let path = self.path.to_string_lossy();
        let name = self.kind.name().to_owned();
        let is_script = self
            .path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("cmd") || ext.eq_ignore_ascii_case("bat"));
        if cfg!(windows) && is_script {
            // npm installs `.cmd` shims, which only cmd.exe can run.
            ShellProfile {
                name,
                program: "cmd.exe".to_owned(),
                args: vec!["/d".to_owned(), "/c".to_owned(), quote(&path)],
            }
        } else {
            ShellProfile {
                name,
                // Windows arguments are passed raw: quote paths with spaces.
                program: if cfg!(windows) {
                    quote(&path)
                } else {
                    path.into_owned()
                },
                args: Vec::new(),
            }
        }
    }
}

fn quote(path: &str) -> String {
    if path.contains(' ') {
        format!("\"{path}\"")
    } else {
        path.to_owned()
    }
}

/// The harnesses installed on this machine, in [`HarnessKind::ALL`] order.
pub fn available_harnesses() -> Vec<Harness> {
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).collect())
        .unwrap_or_default();
    // Claude Code's native installer puts it here; a GUI app started before
    // the installer ran may not have it on PATH yet.
    let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" });
    if let Some(home) = home {
        dirs.push(Path::new(&home).join(".local").join("bin"));
    }
    find(&dirs)
}

fn find(dirs: &[PathBuf]) -> Vec<Harness> {
    HarnessKind::ALL
        .into_iter()
        .filter_map(|kind| {
            let names = candidates(kind.command());
            let path = dirs.iter().find_map(|dir| {
                names
                    .iter()
                    .map(|name| dir.join(name))
                    .find(|candidate| candidate.is_file())
            })?;
            Some(Harness { kind, path })
        })
        .collect()
}

/// File names a shell would run for `command`, in its order of preference.
fn candidates(command: &str) -> Vec<String> {
    if cfg!(windows) {
        ["exe", "cmd", "bat"]
            .iter()
            .map(|ext| format!("{command}.{ext}"))
            .collect()
    } else {
        vec![command.to_owned()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_only_installed_harnesses_first_dir_first() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let exe = |name: &str| {
            if cfg!(windows) {
                format!("{name}.exe")
            } else {
                name.to_owned()
            }
        };
        std::fs::write(second.path().join(exe("codex")), "").unwrap();
        // A directory with the right name isn't a program.
        std::fs::create_dir(first.path().join(exe("claude"))).unwrap();
        let dirs = [first.path().to_owned(), second.path().to_owned()];
        assert_eq!(
            find(&dirs),
            [Harness {
                kind: HarnessKind::Codex,
                path: second.path().join(exe("codex")),
            }]
        );
        std::fs::write(first.path().join(exe("codex")), "").unwrap();
        assert_eq!(find(&dirs)[0].path, first.path().join(exe("codex")));
    }

    #[cfg(windows)]
    #[test]
    fn windows_launch_commands() {
        let harness = |path: &str| Harness {
            kind: HarnessKind::Codex,
            path: PathBuf::from(path),
        };
        let profile = harness(r"C:\Users\A B\npm\codex.CMD").profile();
        assert_eq!(profile.program, "cmd.exe");
        assert_eq!(
            profile.args,
            ["/d", "/c", r#""C:\Users\A B\npm\codex.CMD""#]
        );
        let profile = harness(r"C:\Program Files\Codex\codex.exe").profile();
        assert_eq!(profile.program, r#""C:\Program Files\Codex\codex.exe""#);
        assert!(profile.args.is_empty());
        assert_eq!(
            harness(r"C:\bin\codex.exe").profile().program,
            r"C:\bin\codex.exe"
        );
    }

    #[test]
    fn session_ids_round_trip() {
        for kind in HarnessKind::ALL {
            assert_eq!(HarnessKind::from_command(kind.command()), Some(kind));
        }
        assert_eq!(HarnessKind::from_command("pwsh"), None);
    }
}
