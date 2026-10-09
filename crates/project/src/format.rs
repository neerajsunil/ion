//! Formatting a file with the formatter its language usually uses (rustfmt,
//! gofmt, Prettier...), run on the machine that holds the project. The text
//! goes in on stdin and comes back on stdout, so nothing is written to disk.

use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use crate::FileSystem;

/// A formatter that takes longer than this is stopped and the file saved
/// as it is.
const TIMEOUT: Duration = Duration::from_secs(10);

/// One way to run a formatter.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FormatCommand {
    pub program: String,
    pub args: Vec<String>,
}

/// The formatters to try for a file, in order: the first one installed runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Formatter {
    pub commands: Vec<FormatCommand>,
}

#[derive(Debug)]
pub enum FormatError {
    /// None of the formatters is installed.
    NotInstalled,
    /// The formatter ran and refused (usually a syntax error).
    Failed {
        program: String,
        message: String,
    },
    TimedOut {
        program: String,
    },
}

impl std::fmt::Display for FormatError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotInstalled => f.write_str("no formatter installed"),
            Self::Failed { program, message } => write!(f, "{program}: {message}"),
            Self::TimedOut { program } => write!(f, "{program} took too long"),
        }
    }
}

fn command(program: impl Into<String>, args: &[&str]) -> FormatCommand {
    FormatCommand {
        program: program.into(),
        args: args.iter().map(|arg| (*arg).to_owned()).collect(),
    }
}

/// Extensions Prettier formats. Prettier only runs when the project
/// installs it, so these follow the project's choice.
const PRETTIER_EXTENSIONS: &[&str] = &[
    "js", "jsx", "mjs", "cjs", "ts", "tsx", "mts", "cts", "json", "jsonc", "json5", "css", "scss",
    "less", "html", "vue", "svelte", "md", "markdown", "yaml", "yml", "graphql",
];

const CLANG_EXTENSIONS: &[&str] = &[
    "c", "h", "cc", "cpp", "cxx", "c++", "hpp", "hh", "hxx", "h++", "inl",
];

/// The formatter for `path`, if its language has one. Looks for
/// project-local tools and config in the folders above `path`.
pub fn formatter_for(fs: &FileSystem, path: &Path) -> Option<Formatter> {
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    let name = path.to_string_lossy().into_owned();
    let commands = match extension.as_str() {
        "rs" => {
            let edition = rust_edition(fs, path).unwrap_or_else(|| "2021".to_owned());
            vec![command("rustfmt", &["--edition", &edition])]
        }
        "go" => vec![command("gofmt", &[])],
        "py" | "pyi" => vec![
            command(
                "ruff",
                &["format", "--quiet", "--stdin-filename", &name, "-"],
            ),
            command("black", &["--quiet", "--stdin-filename", &name, "-"]),
        ],
        "lua" => vec![command("stylua", &["--stdin-filepath", &name, "-"])],
        "sh" | "bash" => vec![command("shfmt", &["--filename", &name])],
        // Only with a .clang-format file: the fallback style is "leave it".
        ext if CLANG_EXTENSIONS.contains(&ext) => vec![command(
            "clang-format",
            &[
                &format!("--assume-filename={name}"),
                "--style=file",
                "--fallback-style=none",
            ],
        )],
        ext if PRETTIER_EXTENSIONS.contains(&ext) => {
            let prettier = local_prettier(fs, path)?;
            vec![command(
                prettier.to_string_lossy(),
                &["--stdin-filepath", &name],
            )]
        }
        _ => return None,
    };
    Some(Formatter { commands })
}

/// The edition of the crate holding `path`: the nearest Cargo.toml that
/// sets one (a workspace member inheriting it defers to the workspace's).
fn rust_edition(fs: &FileSystem, path: &Path) -> Option<String> {
    path.ancestors().skip(1).find_map(|dir| {
        let manifest = fs.load_text(&dir.join("Cargo.toml")).ok()?;
        parse_edition(&manifest.text)
    })
}

fn parse_edition(manifest: &str) -> Option<String> {
    manifest.lines().find_map(|line| {
        let value = line.trim().strip_prefix("edition")?.trim_start();
        let value = value.strip_prefix('=')?.trim();
        let edition = value.trim_matches('"');
        (edition.len() == 4 && edition.chars().all(|c| c.is_ascii_digit()))
            .then(|| edition.to_owned())
    })
}

/// Prettier installed in a `node_modules` above `path`.
fn local_prettier(fs: &FileSystem, path: &Path) -> Option<PathBuf> {
    let file = if cfg!(windows) && fs.remote().is_none() {
        "prettier.cmd"
    } else {
        "prettier"
    };
    path.ancestors().skip(1).find_map(|dir| {
        let candidate = dir.join("node_modules").join(".bin").join(file);
        fs.exists(&candidate).unwrap_or(false).then_some(candidate)
    })
}

/// Formats `text` (the contents of `path`) with the first installed command.
/// Blocks; call from a background thread.
pub fn format(
    fs: &FileSystem,
    formatter: &Formatter,
    path: &Path,
    text: &str,
) -> Result<String, FormatError> {
    let dir = path.parent().unwrap_or(path);
    for command in &formatter.commands {
        let result = match fs.remote() {
            None => run_local(command, dir, text),
            Some(connection) => run_remote(connection, command, dir, text),
        };
        match result {
            Err(FormatError::NotInstalled) => continue,
            result => return result,
        }
    }
    Err(FormatError::NotInstalled)
}

fn finish(
    command: &FormatCommand,
    success: bool,
    stdout: Vec<u8>,
    stderr: &[u8],
) -> Result<String, FormatError> {
    if success {
        return String::from_utf8(stdout).map_err(|_| FormatError::Failed {
            program: command.program.clone(),
            message: "output isn't UTF-8".to_owned(),
        });
    }
    let message = String::from_utf8_lossy(stderr);
    let message = message.trim();
    Err(FormatError::Failed {
        program: program_name(&command.program),
        // The first line says what's wrong; the rest is usually a snippet.
        message: message.lines().next().unwrap_or("failed").to_owned(),
    })
}

fn program_name(program: &str) -> String {
    Path::new(program)
        .file_stem()
        .map_or(program.to_owned(), |stem| {
            stem.to_string_lossy().into_owned()
        })
}

fn run_local(command: &FormatCommand, dir: &Path, text: &str) -> Result<String, FormatError> {
    let mut process = Command::new(&command.program);
    process
        .args(&command.args)
        // Formatters find their config (rustfmt.toml...) from here.
        .current_dir(dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // Don't flash a console window from the GUI app.
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        process.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = match process.spawn() {
        Ok(child) => child,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Err(FormatError::NotInstalled),
        Err(err) => {
            return Err(FormatError::Failed {
                program: program_name(&command.program),
                message: err.to_string(),
            });
        }
    };
    // Write and read on other threads so a large file can't deadlock on
    // full pipes, and so a stuck formatter can be stopped.
    let mut stdin = child.stdin.take().expect("stdin is piped");
    let input = text.to_owned();
    std::thread::spawn(move || stdin.write_all(input.as_bytes()));
    let mut stdout = child.stdout.take().expect("stdout is piped");
    let mut stderr = child.stderr.take().expect("stderr is piped");
    let (send, receive) = mpsc::channel();
    std::thread::spawn(move || {
        let mut out = Vec::new();
        let mut err = Vec::new();
        let read = stdout
            .read_to_end(&mut out)
            .and_then(|_| stderr.read_to_end(&mut err));
        let _ = send.send(read.map(|_| (out, err)));
    });
    let Ok(output) = receive.recv_timeout(TIMEOUT) else {
        let _ = child.kill();
        let _ = child.wait();
        return Err(FormatError::TimedOut {
            program: program_name(&command.program),
        });
    };
    let status = child.wait();
    let (out, err) = output.map_err(|err| FormatError::Failed {
        program: program_name(&command.program),
        message: err.to_string(),
    })?;
    let success = status.is_ok_and(|status| status.success());
    finish(command, success, out, &err)
}

fn run_remote(
    connection: &remote::Connection,
    command: &FormatCommand,
    dir: &Path,
    text: &str,
) -> Result<String, FormatError> {
    let line = std::iter::once(&command.program)
        .chain(&command.args)
        .map(|arg| remote::shell_quote(arg))
        .collect::<Vec<_>>()
        .join(" ");
    let line = format!(
        "cd -- {} && {line}",
        remote::shell_quote(&remote::posix_path(dir).to_string_lossy())
    );
    let output = connection
        .execute(&line, Some(text.as_bytes()))
        .map_err(|err| FormatError::Failed {
            program: program_name(&command.program),
            message: err.to_string(),
        })?;
    // The shell's "command not found".
    if output.status == 127 {
        return Err(FormatError::NotInstalled);
    }
    finish(command, output.status == 0, output.stdout, &output.stderr)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_edition() {
        assert_eq!(
            parse_edition("[package]\nname = \"a\"\nedition = \"2024\"\n").as_deref(),
            Some("2024")
        );
        assert_eq!(parse_edition("edition.workspace = true\n"), None);
    }

    #[test]
    fn picks_formatters_by_extension() {
        let fs = FileSystem::Local;
        let programs = |path: &str| {
            formatter_for(&fs, Path::new(path)).map(|f| {
                f.commands
                    .into_iter()
                    .map(|c| c.program)
                    .collect::<Vec<_>>()
            })
        };
        assert_eq!(programs("/x/a.go"), Some(vec!["gofmt".to_owned()]));
        assert_eq!(
            programs("/x/a.py"),
            Some(vec!["ruff".to_owned(), "black".to_owned()])
        );
        assert_eq!(programs("/x/notes.txt"), None);
        // No Prettier installed above it.
        assert_eq!(programs("/nonexistent-ion-test/a.ts"), None);
    }

    #[test]
    fn missing_formatter_is_not_installed() {
        let formatter = Formatter {
            commands: vec![command("ion-no-such-formatter", &[])],
        };
        let result = format(&FileSystem::Local, &formatter, Path::new("/tmp/a.x"), "x");
        assert!(
            matches!(result, Err(FormatError::NotInstalled)),
            "{result:?}"
        );
    }

    #[test]
    fn formats_rust_with_the_crate_edition() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/lib.rs");
        let formatter = formatter_for(&FileSystem::Local, &path).unwrap();
        // The workspace uses edition 2024.
        assert_eq!(formatter.commands[0].args, ["--edition", "2024"]);
        match format(
            &FileSystem::Local,
            &formatter,
            &path,
            "fn  main( ){let x=1;}",
        ) {
            Ok(text) => assert_eq!(text, "fn main() {\n    let x = 1;\n}\n"),
            // CI machines without rustfmt.
            Err(FormatError::NotInstalled) => {}
            Err(err) => panic!("{err}"),
        }
    }
}
