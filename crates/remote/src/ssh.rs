//! The system OpenSSH client: finding it, building its command line,
//! running it and making sense of what it says on stderr.

use std::collections::VecDeque;
use std::ffi::OsString;
use std::io::{self, BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::Duration;

use crate::config::{Target, split_destination};
use crate::{ConnectError, ConnectionOptions, lock};

/// Stderr lines kept for error messages.
const STDERR_LINES: usize = 20;

/// Where the login is going, validated and split from the connection form.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Destination {
    pub(crate) host: String,
    pub(crate) user: Option<String>,
    pub(crate) port: Option<u16>,
    pub(crate) identity: Option<PathBuf>,
}

impl Destination {
    pub(crate) fn new(options: &ConnectionOptions) -> io::Result<Self> {
        let invalid = |message: String| io::Error::new(io::ErrorKind::InvalidInput, message);
        let user = options
            .username
            .as_deref()
            .map(str::trim)
            .filter(|user| !user.is_empty());
        if user.is_some_and(|user| user.contains(['\0', '\n', '\r', '@']) || user.starts_with('-'))
        {
            return Err(invalid("Invalid username".into()));
        }
        if options.host.trim().is_empty() {
            return Err(invalid("Enter a host".into()));
        }
        let (spec_user, host, spec_port) = split_destination(&options.host).map_err(invalid)?;
        Ok(Self {
            host,
            user: user.map(str::to_owned).or(spec_user),
            port: options.port.or(spec_port),
            identity: options
                .identity
                .clone()
                .filter(|path| !path.as_os_str().is_empty()),
        })
    }
}

/// `ssh`, preferring the Windows system copy (it talks to the Windows
/// ssh-agent service).
pub(crate) fn find_ssh() -> io::Result<PathBuf> {
    #[cfg(windows)]
    if let Some(root) = std::env::var_os("SystemRoot") {
        let system = Path::new(&root).join("System32/OpenSSH/ssh.exe");
        if system.is_file() {
            return Ok(system);
        }
    }
    let names: &[&str] = if cfg!(windows) {
        &["ssh.exe"]
    } else {
        &["ssh"]
    };
    std::env::var_os("PATH")
        .into_iter()
        .flat_map(|path| std::env::split_paths(&path).collect::<Vec<_>>())
        .flat_map(|dir| names.iter().map(move |name| dir.join(name)))
        .find(|candidate| candidate.is_file())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "Ion needs the OpenSSH client (ssh). On Windows, add it under Settings › System › Optional features.",
            )
        })
}

fn command(args: &[OsString]) -> io::Result<Command> {
    let mut command = Command::new(find_ssh()?);
    command.args(args);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NO_WINDOW
        command.creation_flags(0x0800_0000);
    }
    Ok(command)
}

/// Options that point `ssh` at a private config and known_hosts (tests and
/// portable installs set `ION_SSH_DIR`).
fn test_hook(dir: Option<&Path>) -> Vec<OsString> {
    let Some(dir) = dir else {
        return Vec::new();
    };
    let config = dir.join("config");
    let null = if cfg!(windows) { "NUL" } else { "/dev/null" };
    let known_hosts = dir.join("known_hosts").to_string_lossy().replace('\\', "/");
    vec![
        "-F".into(),
        if config.is_file() {
            config.into_os_string()
        } else {
            null.into()
        },
        "-o".into(),
        format!("UserKnownHostsFile=\"{known_hosts}\"").into(),
    ]
}

fn ssh_dir_override() -> Option<PathBuf> {
    std::env::var_os("ION_SSH_DIR").map(PathBuf::from)
}

fn destination_args(destination: &Destination, args: &mut Vec<OsString>) {
    if let Some(port) = destination.port {
        args.extend(["-p".into(), port.to_string().into()]);
    }
    if let Some(user) = &destination.user {
        args.extend(["-l".into(), user.into()]);
    }
}

/// The command line that logs in and starts a shell.
pub(crate) fn login_args(destination: &Destination, ssh_dir: Option<&Path>) -> Vec<OsString> {
    let mut args: Vec<OsString> = [
        "-T",
        "-e",
        "none",
        "-o",
        "BatchMode=no",
        "-o",
        "ServerAliveInterval=15",
        "-o",
        "ServerAliveCountMax=4",
        "-o",
        "ConnectTimeout=10",
    ]
    .into_iter()
    .map(OsString::from)
    .collect();
    args.extend(test_hook(ssh_dir));
    destination_args(destination, &mut args);
    if let Some(identity) = &destination.identity {
        args.extend(["-i".into(), identity.into()]);
    }
    args.extend(["--".into(), (&destination.host).into(), "sh".into()]);
    args
}

fn config_args(destination: &Destination, ssh_dir: Option<&Path>) -> Vec<OsString> {
    let mut args: Vec<OsString> = vec!["-G".into()];
    args.extend(test_hook(ssh_dir));
    destination_args(destination, &mut args);
    args.extend(["--".into(), (&destination.host).into()]);
    args
}

/// What `ssh -G` (the effective config for the host, no network) says.
pub(crate) fn resolve(destination: &Destination) -> io::Result<Target> {
    let output = command(&config_args(destination, ssh_dir_override().as_deref()))?
        .stdin(Stdio::null())
        .output()?;
    if !output.status.success() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        ));
    }
    parse_config_dump(&String::from_utf8_lossy(&output.stdout), &destination.host)
}

fn parse_config_dump(text: &str, alias: &str) -> io::Result<Target> {
    let value = |key: &str| {
        text.lines()
            .find_map(|line| line.strip_prefix(key)?.strip_prefix(' '))
            .map(|value| value.trim().to_owned())
    };
    let (Some(host_name), Some(user)) = (value("hostname"), value("user")) else {
        return Err(io::Error::other("Unexpected output from ssh -G"));
    };
    Ok(Target {
        alias: alias.to_owned(),
        host_name,
        port: value("port")
            .and_then(|port| port.parse().ok())
            .unwrap_or(22),
        user,
    })
}

/// The last lines `ssh` wrote to stderr, collected by a thread so the pipe
/// never fills.
pub(crate) struct StderrTail {
    state: Mutex<(VecDeque<String>, bool)>,
    done: Condvar,
}

impl StderrTail {
    pub(crate) fn spawn(stderr: ChildStderr) -> io::Result<Arc<Self>> {
        let tail = Arc::new(Self {
            state: Mutex::new((VecDeque::new(), false)),
            done: Condvar::new(),
        });
        let thread_tail = tail.clone();
        std::thread::Builder::new()
            .name("ion-ssh-stderr".into())
            .spawn(move || {
                let mut reader = BufReader::new(stderr);
                let mut line = Vec::new();
                loop {
                    line.clear();
                    if !matches!(reader.read_until(b'\n', &mut line), Ok(count) if count > 0) {
                        break;
                    }
                    let text = String::from_utf8_lossy(&line).trim_end().to_owned();
                    let mut state = lock(&thread_tail.state);
                    if state.0.len() == STDERR_LINES {
                        state.0.pop_front();
                    }
                    state.0.push_back(text);
                }
                lock(&thread_tail.state).1 = true;
                thread_tail.done.notify_all();
            })?;
        Ok(tail)
    }

    /// The lines so far, after giving the reader a moment to see the end of
    /// the stream (a process that has exited may still be flushing).
    pub(crate) fn lines_after_exit(&self) -> Vec<String> {
        let state = lock(&self.state);
        let (state, _) = self
            .done
            .wait_timeout_while(state, Duration::from_millis(500), |state| !state.1)
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.0.iter().cloned().collect()
    }

    /// The most recent line worth showing, for "connection lost" messages.
    pub(crate) fn last_message(&self) -> Option<String> {
        let state = lock(&self.state);
        state.0.iter().rev().find_map(|line| message_line(line))
    }
}

fn message_line(line: &str) -> Option<String> {
    let line = line.trim();
    (!line.is_empty() && !line.starts_with("Warning:")).then(|| line.to_owned())
}

/// The error for an `ssh` that ended while logging in.
pub(crate) fn login_error(lines: &[String], label: &str) -> ConnectError {
    let has = |text: &str| lines.iter().any(|line| line.contains(text));
    if has("REMOTE HOST IDENTIFICATION HAS CHANGED") {
        return ConnectError::HostKeyChanged {
            host: label.to_owned(),
        };
    }
    if has("Permission denied") {
        return ConnectError::Io(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("Couldn't log in to {label}. Check the username, key or password."),
        ));
    }
    let message = lines
        .iter()
        .rev()
        .find_map(|line| message_line(line))
        .unwrap_or_else(|| format!("The SSH connection to {label} closed unexpectedly"));
    ConnectError::Io(io::Error::other(message))
}

/// The `(major, minor)` in `ssh -V` output (`OpenSSH_9.6p1, ...`,
/// `OpenSSH_for_Windows_9.5p2, ...`).
fn parse_version(text: &str) -> Option<(u32, u32)> {
    let rest = &text[text.find("OpenSSH_")? + "OpenSSH_".len()..];
    let rest = rest.strip_prefix("for_Windows_").unwrap_or(rest);
    let (major, rest) = rest.split_once('.')?;
    let minor: String = rest.chars().take_while(char::is_ascii_digit).collect();
    Some((major.parse().ok()?, minor.parse().ok()?))
}

/// `SSH_ASKPASS_REQUIRE` (how Ion gets `ssh`'s questions) needs OpenSSH 8.4.
/// Checked once per process.
fn check_version() -> io::Result<()> {
    static VERSION: OnceLock<Result<(u32, u32), String>> = OnceLock::new();
    let version = VERSION.get_or_init(|| {
        let output = command(&["-V".into()])
            .and_then(|mut command| command.stdin(Stdio::null()).output())
            .map_err(|error| error.to_string())?;
        // `ssh -V` prints to stderr.
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stderr),
            String::from_utf8_lossy(&output.stdout)
        );
        parse_version(&text).ok_or_else(|| {
            format!(
                "Couldn't tell which OpenSSH version ssh is (it printed {:?})",
                text.trim()
            )
        })
    });
    match version {
        Ok(found) if *found >= (8, 4) => Ok(()),
        Ok((major, minor)) => Err(io::Error::other(format!(
            "Ion needs OpenSSH 8.4 or newer; this ssh is {major}.{minor}. Update the OpenSSH client."
        ))),
        Err(message) => Err(io::Error::other(message.clone())),
    }
}

/// A running `ssh` whose stdio carries the remote shell.
pub(crate) struct Process {
    pub(crate) child: Child,
    pub(crate) stdin: ChildStdin,
    pub(crate) stdout: BufReader<ChildStdout>,
    pub(crate) stderr: Arc<StderrTail>,
}

impl Process {
    pub(crate) fn spawn(destination: &Destination, askpass_spec: &str) -> io::Result<Self> {
        check_version()?;
        let program = std::env::var_os("ION_ASKPASS_PROGRAM")
            .map(PathBuf::from)
            .map_or_else(std::env::current_exe, Ok)?;
        let mut child = command(&login_args(destination, ssh_dir_override().as_deref()))?
            .env("SSH_ASKPASS", program)
            .env("SSH_ASKPASS_REQUIRE", "force")
            .env("ION_ASKPASS", askpass_spec)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let (Some(stdin), Some(stdout), Some(stderr)) =
            (child.stdin.take(), child.stdout.take(), child.stderr.take())
        else {
            let _ = child.kill();
            let _ = child.wait();
            return Err(io::Error::other("Couldn't connect to the ssh process"));
        };
        let stderr = match StderrTail::spawn(stderr) {
            Ok(stderr) => stderr,
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error);
            }
        };
        Ok(Self {
            child,
            stdin,
            stdout: BufReader::new(stdout),
            stderr,
        })
    }

    /// Stops `ssh` (if it hasn't ended by itself) and reaps it.
    pub(crate) fn kill(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn destination(host: &str) -> Destination {
        Destination::new(&ConnectionOptions::new(host)).unwrap()
    }

    fn strings(args: Vec<OsString>) -> Vec<String> {
        args.into_iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn login_command_line_ends_with_the_host_after_a_separator() {
        let mut options = ConnectionOptions::new("ops@server.example:2200");
        options.identity = Some("/keys/id".into());
        let args = strings(login_args(&Destination::new(&options).unwrap(), None));
        assert_eq!(&args[..3], ["-T", "-e", "none"]);
        assert!(args.contains(&"ServerAliveInterval=15".to_owned()));
        assert!(args.contains(&"ConnectTimeout=10".to_owned()));
        let tail = &args[args.len() - 9..];
        assert_eq!(
            tail,
            [
                "-p",
                "2200",
                "-l",
                "ops",
                "-i",
                "/keys/id",
                "--",
                "server.example",
                "sh"
            ]
        );
        // Explicit fields beat the destination string.
        options.port = Some(22);
        options.username = Some("me".into());
        let args = strings(login_args(&Destination::new(&options).unwrap(), None));
        assert!(args.windows(2).any(|pair| pair == ["-p", "22"]));
        assert!(args.windows(2).any(|pair| pair == ["-l", "me"]));
    }

    #[test]
    fn hosts_cannot_become_options() {
        assert!(Destination::new(&ConnectionOptions::new("-oProxyCommand=evil")).is_err());
        let mut options = ConnectionOptions::new("host");
        options.username = Some("-oProxyCommand=evil".into());
        assert!(Destination::new(&options).is_err());
    }

    #[test]
    fn test_hook_selects_config_and_known_hosts() {
        let dir = std::env::temp_dir().join(format!("ion-ssh-hook-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let without = strings(login_args(&destination("h"), Some(&dir)));
        let null = if cfg!(windows) { "NUL" } else { "/dev/null" };
        assert!(without.windows(2).any(|pair| pair == ["-F", null]));
        assert!(
            without
                .iter()
                .any(|arg| arg.starts_with("UserKnownHostsFile=") && arg.contains("known_hosts"))
        );
        std::fs::write(dir.join("config"), "").unwrap();
        let with = strings(config_args(&destination("h"), Some(&dir)));
        assert_eq!(with[0], "-G");
        let config = dir.join("config").to_string_lossy().into_owned();
        assert!(
            with.windows(2)
                .any(|pair| pair[0] == "-F" && pair[1] == config)
        );
        assert_eq!(&with[with.len() - 2..], ["--", "h"]);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn parses_openssh_versions() {
        assert_eq!(
            parse_version("OpenSSH_9.6p1 Ubuntu, OpenSSL 3.0"),
            Some((9, 6))
        );
        assert_eq!(
            parse_version("OpenSSH_for_Windows_9.5p2, LibreSSL 3.8.2\r\n"),
            Some((9, 5))
        );
        assert_eq!(parse_version("OpenSSH_8.4p1"), Some((8, 4)));
        assert!(parse_version("OpenSSH_8.4p1").unwrap() >= (8, 4));
        assert!(parse_version("OpenSSH_7.9p1").unwrap() < (8, 4));
        assert!(parse_version("OpenSSH_10.0p2").unwrap() >= (8, 4));
        assert_eq!(parse_version("Dropbear v2022"), None);
        assert_eq!(parse_version("OpenSSH_x.y"), None);
    }

    #[test]
    fn reads_the_effective_config() {
        let target = parse_config_dump(
            "user deploy\nhostname prod.example.com\nport 2200\nidentityfile ~/.ssh/id\n",
            "prod",
        )
        .unwrap();
        assert_eq!(target.label(), "deploy@prod:2200");
        assert_eq!(target.host_name, "prod.example.com");
        assert!(parse_config_dump("port 22\n", "x").is_err());
    }

    fn lines(text: &[&str]) -> Vec<String> {
        text.iter().map(|line| (*line).to_owned()).collect()
    }

    #[test]
    fn stderr_becomes_a_login_error() {
        assert!(matches!(
            login_error(
                &lines(&["@@@ WARNING: REMOTE HOST IDENTIFICATION HAS CHANGED! @@@"]),
                "dev@h"
            ),
            ConnectError::HostKeyChanged { host } if host == "dev@h"
        ));
        match login_error(
            &lines(&["dev@h: Permission denied (publickey,password)."]),
            "dev@h",
        ) {
            ConnectError::Io(error) => {
                assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
                assert_eq!(
                    error.to_string(),
                    "Couldn't log in to dev@h. Check the username, key or password."
                );
            }
            other => panic!("{other:?}"),
        }
        match login_error(
            &lines(&[
                "ssh: connect to host h port 22: Connection refused",
                "Warning: Permanently added 'h' to the list of known hosts.",
                "",
            ]),
            "dev@h",
        ) {
            ConnectError::Io(error) => assert_eq!(
                error.to_string(),
                "ssh: connect to host h port 22: Connection refused"
            ),
            other => panic!("{other:?}"),
        }
        assert!(matches!(login_error(&[], "dev@h"), ConnectError::Io(_)));
    }
}
