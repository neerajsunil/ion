//! SSH connections to remote projects. Calls block; callers run them on the
//! background executor.
//!
//! Each project uses one `ssh` process (the system OpenSSH client, so the
//! person's `~/.ssh/config`, agent and jump hosts all apply), shared by file
//! requests, searches, the watcher, Git and every terminal, so the person
//! logs in once. If the link drops, Ion reconnects with the answers it already
//! holds and reports the state to the UI.

use std::fmt;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak, mpsc};
use std::time::Duration;

use base64::{Engine, engine::general_purpose::STANDARD_NO_PAD};
use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use remote_protocol::WatchEvent;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

mod askpass;
mod auth;
mod config;
mod forward;
mod mux;
mod server;
mod ssh;
mod terminal;
mod tools;
mod trace;

pub use askpass::askpass_main;
pub use auth::{AuthPrompt, PromptField, Prompter};
pub use config::{Config, Target};
pub use forward::{Connector, Forward, LocalConnection, RemoteSocket};
pub use terminal::{Program, Terminal, TerminalEvent, TerminalSize};
pub use tools::RemoteTools;

use auth::Secrets;
use mux::{ChannelId, Event, Kind, Mux, Sink, Stream};

/// Automatic reconnect attempts after the link drops (seconds to wait first).
const RECONNECT_DELAYS: [u64; 10] = [0, 1, 2, 4, 8, 15, 30, 30, 30, 30];

/// What the person entered to connect. Saved with recent projects; holds no
/// secrets.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct ConnectionOptions {
    /// A host name, IP address or `~/.ssh/config` alias (`user@` and
    /// `:port` are accepted too).
    pub host: String,
    #[serde(default)]
    pub port: Option<u16>,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub identity: Option<PathBuf>,
}

impl ConnectionOptions {
    pub fn new(host: impl Into<String>) -> Self {
        Self {
            host: host.into(),
            port: None,
            username: None,
            identity: None,
        }
    }

    /// The effective destination: what `ssh -G` reports for the host (the
    /// SSH config applied), or, if `ssh` can't say, the config read here.
    pub fn resolve(&self) -> io::Result<Target> {
        let destination = ssh::Destination::new(self)?;
        ssh::resolve(&destination).or_else(|_| self.resolve_with(&Config::load()))
    }

    /// A quick guess from an already loaded config, without running `ssh`
    /// (for showing a preview while typing).
    pub fn resolve_with(&self, config: &Config) -> io::Result<Target> {
        let destination = ssh::Destination::new(self)?;
        config
            .resolve(
                self.host.trim(),
                destination.user.as_deref(),
                destination.port,
            )
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))
    }

    pub fn validate(&self) -> io::Result<()> {
        self.resolve_with(&Config::default()).map(drop)
    }
}

#[derive(Debug)]
pub enum ConnectError {
    /// A host (the server or a jump host) isn't in known_hosts yet. Ask the
    /// person, then connect again with `key` trusted (the fingerprint's
    /// bytes).
    UnknownHost {
        host: String,
        fingerprint: String,
        key: Vec<u8>,
    },
    /// The host's key differs from the one in known_hosts.
    HostKeyChanged {
        host: String,
    },
    Io(io::Error),
}

impl fmt::Display for ConnectError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownHost {
                host, fingerprint, ..
            } => {
                write!(f, "Unknown SSH host {host}: {fingerprint}")
            }
            Self::HostKeyChanged { host } => write!(
                f,
                "The SSH host key for {host} has changed. This could mean someone is intercepting the connection. If the server was reinstalled, remove its old entry from known_hosts."
            ),
            Self::Io(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for ConnectError {}

impl From<io::Error> for ConnectError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

/// The link's state, for the status bar.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConnectionState {
    Connected,
    /// Lost; trying again on its own.
    Reconnecting(String),
    /// Lost, and not retrying: the person has to reconnect.
    Disconnected(String),
}

pub struct CommandOutput {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub status: i32,
}

pub struct Connection {
    options: ConnectionOptions,
    target: Target,
    secrets: Arc<Mutex<Secrets>>,
    prompter: Mutex<Option<Arc<dyn Prompter>>>,
    /// Host key fingerprints (as bytes) the person approved.
    trusted: Vec<Vec<u8>>,
    link: Mutex<Option<Arc<Mux>>>,
    /// Held while connecting, so concurrent callers share one attempt.
    connecting: Mutex<()>,
    /// Disconnected on purpose; never reconnect.
    closed: AtomicBool,
    state: Mutex<ConnectionState>,
    listeners: Mutex<Vec<UnboundedSender<ConnectionState>>>,
    server: Mutex<Option<String>>,
    /// Searches in flight, for [`Connection::cancel_jobs`].
    jobs: Mutex<Vec<(Arc<Mux>, u64)>>,
    /// Environment variables for terminals started from now on.
    terminal_env: Mutex<Vec<(String, String)>>,
    this: Weak<Connection>,
}

impl fmt::Debug for Connection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Secrets never appear in Debug output.
        f.debug_struct("Connection")
            .field("target", &self.target.label())
            .finish_non_exhaustive()
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl Connection {
    /// Connects and logs in. `password` (optional) is tried first for
    /// passwords and key passphrases; anything else is asked through
    /// `prompter`. Fingerprints in `trusted` are accepted for hosts not yet
    /// in known_hosts, and `ssh` adds them to it.
    pub fn connect(
        options: ConnectionOptions,
        password: Option<String>,
        trusted: &[Vec<u8>],
        prompter: Option<Arc<dyn Prompter>>,
    ) -> Result<Arc<Self>, ConnectError> {
        let target = options.resolve()?;
        Self::connect_to(options, target, password, trusted, prompter)
    }

    /// Like [`Connection::connect`], with the destination already resolved.
    pub fn connect_to(
        options: ConnectionOptions,
        target: Target,
        password: Option<String>,
        trusted: &[Vec<u8>],
        prompter: Option<Arc<dyn Prompter>>,
    ) -> Result<Arc<Self>, ConnectError> {
        let connection = Arc::new_cyclic(|this| Self {
            options,
            secrets: Arc::new(Mutex::new(Secrets::with_password(password))),
            target,
            prompter: Mutex::new(prompter),
            trusted: trusted.to_vec(),
            link: Mutex::new(None),
            connecting: Mutex::new(()),
            closed: AtomicBool::new(false),
            state: Mutex::new(ConnectionState::Connected),
            listeners: Mutex::new(Vec::new()),
            server: Mutex::new(None),
            jobs: Mutex::new(Vec::new()),
            terminal_env: Mutex::new(Vec::new()),
            this: this.clone(),
        });
        let link = connection.open_link()?;
        *lock(&connection.link) = Some(link);
        Ok(connection)
    }

    pub fn options(&self) -> &ConnectionOptions {
        &self.options
    }

    pub fn target(&self) -> &Target {
        &self.target
    }

    /// `user@host[:port]`.
    pub fn label(&self) -> String {
        self.target.label()
    }

    /// Sets environment variables every terminal started from now on gets,
    /// e.g. the port agents use to reach Ion's IDE integration.
    pub fn set_terminal_env(&self, vars: Vec<(String, String)>) {
        *lock(&self.terminal_env) = vars;
    }

    pub(crate) fn terminal_env(&self) -> Vec<(String, String)> {
        lock(&self.terminal_env).clone()
    }

    /// Where login questions go from now on (the window using the
    /// connection).
    pub fn set_prompter(&self, prompter: Option<Arc<dyn Prompter>>) {
        *lock(&self.prompter) = prompter;
    }

    pub fn state(&self) -> ConnectionState {
        lock(&self.state).clone()
    }

    /// State changes from now on.
    pub fn subscribe(&self) -> UnboundedReceiver<ConnectionState> {
        let (sender, receiver) = unbounded();
        lock(&self.listeners).push(sender);
        receiver
    }

    fn set_state(&self, state: ConnectionState) {
        let mut current = lock(&self.state);
        if *current == state {
            return;
        }
        *current = state.clone();
        drop(current);
        lock(&self.listeners).retain(|listener| listener.unbounded_send(state.clone()).is_ok());
    }

    /// Ends the connection (and every terminal using it).
    pub fn disconnect(&self) {
        self.closed.store(true, Ordering::Release);
        lock(&self.jobs).clear();
        if let Some(link) = lock(&self.link).take() {
            link.shutdown();
        }
        self.set_state(ConnectionState::Disconnected("Disconnected".into()));
    }

    /// Reconnects now, after the link was lost.
    pub fn reconnect(&self) -> io::Result<()> {
        self.link().map(drop)
    }

    /// The live link, reconnecting first if it was lost.
    fn link(&self) -> io::Result<Arc<Mux>> {
        if let Some(link) = lock(&self.link).as_ref().filter(|link| link.is_alive()) {
            return Ok(link.clone());
        }
        if self.closed.load(Ordering::Acquire) {
            return Err(mux::lost("SSH connection closed"));
        }
        let _connecting = lock(&self.connecting);
        if let Some(link) = lock(&self.link).as_ref().filter(|link| link.is_alive()) {
            return Ok(link.clone());
        }
        match self.open_link() {
            Ok(link) => {
                let mut slot = lock(&self.link);
                // A disconnect during the attempt wins.
                if self.closed.load(Ordering::Acquire) {
                    drop(slot);
                    link.shutdown();
                    return Err(mux::lost("SSH connection closed"));
                }
                *slot = Some(link.clone());
                drop(slot);
                self.set_state(ConnectionState::Connected);
                Ok(link)
            }
            Err(error) => {
                let error = match error {
                    ConnectError::Io(error) => error,
                    other => io::Error::new(io::ErrorKind::PermissionDenied, other.to_string()),
                };
                self.set_state(ConnectionState::Disconnected(error.to_string()));
                Err(error)
            }
        }
    }

    /// Starts `ssh`, logs in (answering its questions through the askpass
    /// listener) and starts the server on the other end.
    fn open_link(&self) -> Result<Arc<Mux>, ConnectError> {
        let destination = ssh::Destination::new(&self.options)?;
        let label = self.label();
        let login = askpass::Login {
            label: label.clone(),
            target_prompt: format!("{}@{}'s password:", self.target.user, self.target.host_name),
            secrets: self.secrets.clone(),
            trusted: self.trusted.clone(),
            prompter: lock(&self.prompter).clone(),
            outcome: Arc::default(),
            asked: Default::default(),
        };
        let askpass = askpass::AskpassServer::start(login)?;
        let mut process = ssh::Process::spawn(&destination, &askpass.spec())?;
        let bootstrapped = server::bootstrap(&mut process);
        let outcome = askpass.finish();
        let server = match bootstrapped {
            Ok(server) => server,
            Err(error) => {
                process.kill();
                // `ssh` ending on its own is a failed login; anything else
                // is a problem with the server setup.
                if !matches!(
                    error.kind(),
                    io::ErrorKind::UnexpectedEof | io::ErrorKind::BrokenPipe
                ) {
                    return Err(error.into());
                }
                if let Some((host, fingerprint)) = outcome.unknown_host {
                    let key = askpass::trusted_key(&host, &fingerprint);
                    return Err(ConnectError::UnknownHost {
                        host,
                        key,
                        fingerprint,
                    });
                }
                if outcome.cancelled {
                    return Err(auth::cancelled().into());
                }
                return Err(ssh::login_error(&process.stderr.lines_after_exit(), &label));
            }
        };
        *lock(&self.server) = Some(server);
        let this = self.this.clone();
        let ssh::Process {
            child,
            stdin,
            stdout,
            stderr,
            ..
        } = process;
        Ok(Mux::start(
            child,
            stdin,
            stdout,
            stderr,
            Box::new(move |reason| {
                if let Some(connection) = this.upgrade() {
                    connection.link_lost(reason);
                }
            }),
        )?)
    }

    /// Runs on the reader thread when the link drops: retries in the
    /// background with the login answers already held.
    fn link_lost(&self, reason: String) {
        if self.closed.load(Ordering::Acquire) {
            return;
        }
        self.set_state(ConnectionState::Reconnecting(reason.clone()));
        let this = self.this.clone();
        let last_reason = reason.clone();
        let spawned = std::thread::Builder::new()
            .name("ion-ssh-reconnect".into())
            .spawn(move || {
                for delay in RECONNECT_DELAYS {
                    std::thread::sleep(Duration::from_secs(delay));
                    let Some(connection) = this.upgrade() else {
                        return;
                    };
                    if connection.closed.load(Ordering::Acquire) {
                        return;
                    }
                    match connection.link() {
                        Ok(_) => {
                            if !connection.closed.load(Ordering::Acquire) {
                                connection.set_state(ConnectionState::Connected);
                            }
                            return;
                        }
                        // Login problems won't fix themselves.
                        Err(error)
                            if matches!(
                                error.kind(),
                                io::ErrorKind::PermissionDenied | io::ErrorKind::Interrupted
                            ) =>
                        {
                            return;
                        }
                        Err(error) => {
                            connection.set_state(ConnectionState::Reconnecting(error.to_string()))
                        }
                    }
                }
                if let Some(connection) = this.upgrade() {
                    connection.set_state(ConnectionState::Disconnected(last_reason));
                }
            });
        if spawned.is_err() {
            self.set_state(ConnectionState::Disconnected(reason));
        }
    }

    pub fn canonicalize(&self, path: &Path) -> io::Result<PathBuf> {
        self.request::<String>(remote_protocol::Operation::Canonicalize {
            path: posix_path(path).to_string_lossy().into_owned(),
        })
        .map(PathBuf::from)
    }

    /// Runs a shell command on the server with optional stdin. No timeout:
    /// Git hooks can take as long as they take.
    pub fn execute(&self, command: &str, input: Option<&[u8]>) -> io::Result<CommandOutput> {
        let link = self.link()?;
        let mut stream = Stream::exec(&link, command);
        if let Some(input) = input {
            stream.write_all(input)?;
        }
        stream.send_eof();
        let (stdout, stderr, status) = stream.finish()?;
        Ok(CommandOutput {
            stdout,
            stderr,
            status,
        })
    }

    /// A long-running command whose output is read as it arrives. Dropping
    /// the stream (or its closer) ends the command.
    pub fn stream(&self, command: &str) -> io::Result<CommandStream> {
        let link = self.link()?;
        Ok(CommandStream(Stream::exec(&link, command)))
    }

    /// Watches `root` for changes: events arrive until the watch is closed
    /// (by dropping the closer) or the connection ends, which is reported as
    /// an event with `error` set.
    pub fn watch(&self, root: &Path) -> io::Result<(StreamCloser, mpsc::Receiver<WatchEvent>)> {
        let (sender, receiver) = mpsc::channel();
        let sink: Sink = Box::new(move |event| {
            let event = match event {
                Event::Watch(event) => event,
                Event::Failed(error) if error.kind() != io::ErrorKind::Interrupted => WatchEvent {
                    ready: false,
                    paths: Vec::new(),
                    structural: false,
                    error: Some(error.to_string()),
                },
                _ => return None,
            };
            let _ = sender.send(event);
            None
        });
        let (mux, id) = self.open(
            Kind::Watch {
                root: posix_path(root).to_string_lossy().into_owned(),
            },
            sink,
        )?;
        Ok((StreamCloser { mux, id }, receiver))
    }

    fn open(&self, kind: Kind, sink: Sink) -> io::Result<(Arc<Mux>, ChannelId)> {
        let link = self.link()?;
        let id = link.open(kind, sink);
        Ok((link, id))
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        if let Some(link) = lock(&self.link).take() {
            link.shutdown();
        }
    }
}

/// Output of [`Connection::stream`].
pub struct CommandStream(Stream);

impl CommandStream {
    pub fn closer(&self) -> StreamCloser {
        StreamCloser {
            mux: self.0.mux().clone(),
            id: self.0.id(),
        }
    }
}

impl Read for CommandStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.0.read(buf)
    }
}

/// Ends a [`CommandStream`] from another thread.
pub struct StreamCloser {
    mux: Arc<Mux>,
    id: ChannelId,
}

impl StreamCloser {
    pub fn close(&self) {
        self.mux.close(self.id);
    }
}

impl Drop for StreamCloser {
    fn drop(&mut self) {
        self.close();
    }
}

/// `SHA256:…`, as `ssh` shows it.
pub fn fingerprint(key: &[u8]) -> String {
    format!("SHA256:{}", STANDARD_NO_PAD.encode(Sha256::digest(key)))
}

/// Linux paths are kept as PathBuf for the editor's path API; separators are
/// normalized at the SSH boundary.
pub fn posix_path(path: &Path) -> PathBuf {
    PathBuf::from(path.to_string_lossy().replace('\\', "/"))
}

/// Joins a remote path and a child name with `/`, never `\`.
pub fn join_posix(dir: &Path, name: &str) -> PathBuf {
    let dir = dir.to_string_lossy().replace('\\', "/");
    let dir = dir.trim_end_matches('/');
    PathBuf::from(format!("{dir}/{name}"))
}

pub fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_arguments_cannot_inject_commands() {
        assert_eq!(
            shell_quote("a'; $(touch /tmp/x)"),
            "'a'\\''; $(touch /tmp/x)'"
        );
        assert_eq!(shell_quote(""), "''");
    }

    #[test]
    fn validates_destinations_and_normalizes_remote_paths() {
        let mut options = ConnectionOptions::new("server.example");
        options.username = Some("dev".into());
        assert!(options.validate().is_ok());
        options.host = "-oProxyCommand=evil".into();
        assert!(options.validate().is_err());
        options.host = "server.example".into();
        options.username = Some("a@b".into());
        assert!(options.validate().is_err());
        assert_eq!(
            posix_path(Path::new("/home/dev/project\\src\\main.rs")),
            PathBuf::from("/home/dev/project/src/main.rs")
        );
        assert_eq!(
            join_posix(Path::new("/home/dev/"), "src"),
            PathBuf::from("/home/dev/src")
        );
    }
}
