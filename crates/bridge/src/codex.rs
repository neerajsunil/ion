//! Codex's IDE integration (`/ide` in the Codex CLI): before each prompt the
//! CLI asks the editor for the active file, its selection and the open tabs.
//!
//! Ion listens where the CLI looks: `$CODEX_HOME/ipc/ipc.sock` on Unix,
//! `\\.\pipe\codex-ipc` on Windows. A frame is a little-endian `u32` length
//! followed by a JSON message. If another editor (or the Codex app) already
//! listens there, Ion leaves it alone.
//!
//! A CLI on an SSH server reaches Ion through the project's connection
//! instead; its connections arrive through [`connect`].
//!
//! Threads: one blocked in `accept`, plus one per CLI connection, which
//! lasts one request.

use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use futures::channel::mpsc::UnboundedSender;
use futures::channel::oneshot;
use serde_json::{Value, json};

use crate::IdeEvent;

/// Requests are a few hundred bytes; anything this large isn't the CLI.
const MAX_FRAME: usize = 1024 * 1024;
/// What the CLI reports when nobody can answer for its folder.
const NO_CLIENT: &str = "no-client-found";

/// The CLI's request for IDE context. Reply once; dropping it unanswered
/// tells the CLI no editor has its folder open.
pub struct ContextRequest {
    /// The folder the CLI runs in.
    pub workspace_root: PathBuf,
    /// For a CLI on an SSH server: what [`crate::IdeServer::codex_connector`]
    /// was given. `None` for one on this machine.
    pub origin: Option<u64>,
    reply: oneshot::Sender<Value>,
}

impl ContextRequest {
    /// Answers with an `ideContext` object: `activeFile` and `openTabs`.
    pub fn reply(self, ide_context: Value) {
        self.reply.send(ide_context).ok();
    }
}

/// The listening socket. Dropping it stops the accept thread and removes
/// the socket file.
pub(crate) struct Listener {
    stopped: Arc<AtomicBool>,
    address: platform::Address,
}

impl Listener {
    pub(crate) fn start(events: UnboundedSender<IdeEvent>) -> io::Result<Self> {
        let (listener, address) = platform::bind()?;
        let stopped = Arc::new(AtomicBool::new(false));
        let accept_stopped = stopped.clone();
        std::thread::Builder::new()
            .name("ion-codex-accept".into())
            .spawn(move || {
                loop {
                    let stream = platform::accept(&listener);
                    if accept_stopped.load(Ordering::SeqCst) {
                        break;
                    }
                    let Ok(stream) = stream else {
                        continue;
                    };
                    let events = events.clone();
                    std::thread::Builder::new()
                        .name("ion-codex-request".into())
                        .spawn(move || serve(stream, &events, None))
                        .ok();
                }
            })?;
        Ok(Self { stopped, address })
    }
}

impl Drop for Listener {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::SeqCst);
        platform::release(&self.address);
    }
}

/// A connection for a CLI elsewhere, answered on its own thread: returns
/// the end to read Ion's replies from and the end to write the CLI's bytes to.
pub(crate) fn connect(
    events: UnboundedSender<IdeEvent>,
    origin: u64,
) -> io::Result<(io::PipeReader, io::PipeWriter)> {
    let (from_cli, to_ion) = io::pipe()?;
    let (from_ion, to_cli) = io::pipe()?;
    let duplex = Duplex {
        reader: from_cli,
        writer: to_cli,
    };
    std::thread::Builder::new()
        .name("ion-codex-request".into())
        .spawn(move || serve(duplex, &events, Some(origin)))?;
    Ok((from_ion, to_ion))
}

/// Two pipes as one stream.
struct Duplex {
    reader: io::PipeReader,
    writer: io::PipeWriter,
}

impl Read for Duplex {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.reader.read(buf)
    }
}

impl Write for Duplex {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.writer.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.writer.flush()
    }
}

/// Answers one connection's requests until the CLI hangs up.
fn serve<S: Read + Write>(mut stream: S, events: &UnboundedSender<IdeEvent>, origin: Option<u64>) {
    while let Ok(message) = read_frame(&mut stream) {
        let Some(reply) = answer(&message, events, origin) else {
            continue;
        };
        if write_frame(&mut stream, &reply).is_err() {
            break;
        }
    }
}

/// The reply to one message, if it needs one.
fn answer(
    message: &Value,
    events: &UnboundedSender<IdeEvent>,
    origin: Option<u64>,
) -> Option<Value> {
    let kind = message.get("type").and_then(Value::as_str)?;
    let request_id = message.get("requestId").and_then(Value::as_str)?;
    match kind {
        "request" => {}
        // The CLI only asks; it can't handle anything itself.
        "client-discovery-request" => {
            return Some(json!({
                "type": "client-discovery-response",
                "requestId": request_id,
                "response": { "canHandle": false },
            }));
        }
        _ => return None,
    }
    let method = message.get("method").and_then(Value::as_str);
    if method != Some("ide-context") {
        return Some(error(request_id, "no-handler-for-request"));
    }
    let workspace_root = message
        .pointer("/params/workspaceRoot")
        .and_then(Value::as_str)
        .map(PathBuf::from)?;
    let (sender, receiver) = oneshot::channel();
    let request = ContextRequest {
        workspace_root,
        origin,
        reply: sender,
    };
    events.unbounded_send(IdeEvent::Context(request)).ok();
    // This thread exists for this request; parking it costs nothing. A
    // dropped request cancels the receiver.
    Some(match futures::executor::block_on(receiver) {
        Ok(ide_context) => json!({
            "type": "response",
            "requestId": request_id,
            "resultType": "success",
            "method": "ide-context",
            "handledByClientId": "ion",
            "result": { "ideContext": ide_context },
        }),
        Err(_) => error(request_id, NO_CLIENT),
    })
}

fn error(request_id: &str, error: &str) -> Value {
    json!({
        "type": "response",
        "requestId": request_id,
        "resultType": "error",
        "error": error,
    })
}

fn read_frame(reader: &mut impl Read) -> io::Result<Value> {
    let mut len = [0u8; 4];
    reader.read_exact(&mut len)?;
    let len = u32::from_le_bytes(len) as usize;
    if len > MAX_FRAME {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "frame too large",
        ));
    }
    let mut payload = vec![0u8; len];
    reader.read_exact(&mut payload)?;
    serde_json::from_slice(&payload).map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))
}

fn write_frame(writer: &mut impl Write, message: &Value) -> io::Result<()> {
    let payload = message.to_string();
    let len = u32::try_from(payload.len())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "frame too large"))?;
    writer.write_all(&len.to_le_bytes())?;
    writer.write_all(payload.as_bytes())?;
    writer.flush()
}

#[cfg(unix)]
mod platform {
    use std::fs::Permissions;
    use std::io;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    /// A stuck client gives up its thread after this long.
    const READ_TIMEOUT: Duration = Duration::from_secs(10);

    /// The socket file, and its inode so Ion only removes its own.
    pub(super) struct Address {
        path: PathBuf,
        inode: u64,
    }

    pub(super) fn bind() -> io::Result<(UnixListener, Address)> {
        bind_in(&codex_home()?)
    }

    pub(super) fn bind_in(codex_home: &Path) -> io::Result<(UnixListener, Address)> {
        let dir = codex_home.join("ipc");
        std::fs::create_dir_all(&dir)?;
        // The CLI refuses a folder other users could write to.
        std::fs::set_permissions(&dir, Permissions::from_mode(0o700))?;
        let path = dir.join("ipc.sock");
        if UnixStream::connect(&path).is_ok() {
            return Err(io::ErrorKind::AddrInUse.into());
        }
        // Nobody answers: a socket left by a process that crashed.
        match std::fs::remove_file(&path) {
            Err(err) if err.kind() != io::ErrorKind::NotFound => return Err(err),
            _ => {}
        }
        let listener = UnixListener::bind(&path)?;
        let inode = std::fs::metadata(&path)?.ino();
        Ok((listener, Address { path, inode }))
    }

    pub(super) fn accept(listener: &UnixListener) -> io::Result<UnixStream> {
        let (stream, _) = listener.accept()?;
        stream.set_read_timeout(Some(READ_TIMEOUT))?;
        Ok(stream)
    }

    pub(super) fn release(address: &Address) {
        // Wake the accept thread so it sees `stopped` and exits.
        UnixStream::connect(&address.path).ok();
        if std::fs::metadata(&address.path).is_ok_and(|meta| meta.ino() == address.inode) {
            std::fs::remove_file(&address.path).ok();
        }
    }

    fn codex_home() -> io::Result<PathBuf> {
        if let Some(home) = std::env::var_os("CODEX_HOME").filter(|home| !home.is_empty()) {
            return Ok(PathBuf::from(home));
        }
        let home = std::env::var_os("HOME").ok_or(io::ErrorKind::NotFound)?;
        Ok(Path::new(&home).join(".codex"))
    }
}

#[cfg(windows)]
mod platform {
    use std::io;

    use interprocess::os::windows::named_pipe::{
        DuplexPipeStream, PipeListener, PipeListenerOptions, PipeMode, pipe_mode,
    };

    /// The CLI connects here; there's no per-user path on Windows.
    const PIPE: &str = r"\\.\pipe\codex-ipc";

    pub(super) struct Address;

    type Listener = PipeListener<pipe_mode::Bytes, pipe_mode::Bytes>;

    pub(super) fn bind() -> io::Result<(Listener, Address)> {
        // Creating the first instance fails if another server owns the name.
        let listener = PipeListenerOptions::new()
            .path(PIPE)
            .mode(PipeMode::Bytes)
            .create_duplex::<pipe_mode::Bytes>()?;
        Ok((listener, Address))
    }

    pub(super) fn accept(listener: &Listener) -> io::Result<DuplexPipeStream<pipe_mode::Bytes>> {
        listener.accept()
    }

    pub(super) fn release(_: &Address) {
        // Wake the accept thread so it sees `stopped` and exits.
        DuplexPipeStream::<pipe_mode::Bytes>::connect_by_path(PIPE).ok();
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use futures::StreamExt;
    use futures::channel::mpsc::unbounded;

    use super::*;

    fn frame(message: &Value) -> Vec<u8> {
        let mut bytes = Vec::new();
        write_frame(&mut bytes, message).unwrap();
        bytes
    }

    fn request(method: &str) -> Value {
        json!({
            "type": "request",
            "requestId": "r1",
            "sourceClientId": "codex-tui",
            "version": 0,
            "method": method,
            "params": { "workspaceRoot": "/repo" },
        })
    }

    #[test]
    fn frames_round_trip() {
        let message = json!({ "type": "broadcast" });
        let bytes = frame(&message);
        assert_eq!(&bytes[..4], &(bytes.len() as u32 - 4).to_le_bytes());
        assert_eq!(read_frame(&mut Cursor::new(bytes)).unwrap(), message);
    }

    #[test]
    fn answers_ide_context_from_the_ui() {
        let (events, mut receiver) = unbounded();
        let ui = std::thread::spawn(move || {
            let Some(IdeEvent::Context(request)) = futures::executor::block_on(receiver.next())
            else {
                panic!("context request");
            };
            assert_eq!(request.workspace_root, PathBuf::from("/repo"));
            request.reply(json!({ "activeFile": null, "openTabs": [] }));
        });
        let reply = answer(&request("ide-context"), &events, None).unwrap();
        ui.join().unwrap();
        assert_eq!(reply["requestId"], "r1");
        assert_eq!(reply["resultType"], "success");
        assert_eq!(reply["result"]["ideContext"]["openTabs"], json!([]));
    }

    #[test]
    fn unanswered_requests_report_no_client() {
        let (events, receiver) = unbounded();
        // The UI is gone: the request drops as soon as it's sent.
        drop(receiver);
        let reply = answer(&request("ide-context"), &events, None).unwrap();
        assert_eq!(reply["resultType"], "error");
        assert_eq!(reply["error"], NO_CLIENT);
    }

    #[test]
    fn answers_a_cli_elsewhere_with_its_origin() {
        let (events, mut receiver) = unbounded();
        let ui = std::thread::spawn(move || {
            let Some(IdeEvent::Context(request)) = futures::executor::block_on(receiver.next())
            else {
                panic!("context request");
            };
            assert_eq!(request.origin, Some(7));
            request.reply(json!({ "openTabs": [] }));
        });
        let (mut from_ion, mut to_ion) = connect(events, 7).unwrap();
        write_frame(&mut to_ion, &request("ide-context")).unwrap();
        let reply = read_frame(&mut from_ion).unwrap();
        assert_eq!(reply["resultType"], "success");
        // The CLI hanging up ends the request thread, which ends Ion's side.
        drop(to_ion);
        assert!(read_frame(&mut from_ion).is_err());
        ui.join().unwrap();
    }

    #[test]
    fn refuses_other_requests() {
        let (events, _receiver) = unbounded();
        let reply = answer(&request("something-else"), &events, None).unwrap();
        assert_eq!(reply["error"], "no-handler-for-request");
        let discovery = json!({ "type": "client-discovery-request", "requestId": "d1" });
        let reply = answer(&discovery, &events, None).unwrap();
        assert_eq!(reply["response"]["canHandle"], false);
        assert!(
            answer(
                &json!({ "type": "broadcast", "requestId": "b" }),
                &events,
                None
            )
            .is_none()
        );
    }

    #[cfg(unix)]
    #[test]
    fn takes_over_only_dead_sockets() {
        use std::os::unix::fs::PermissionsExt;

        let home = tempfile::tempdir().unwrap();
        let (live, address) = platform::bind_in(home.path()).unwrap();
        let mode = std::fs::metadata(home.path().join("ipc"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o700);
        let in_use = platform::bind_in(home.path()).map(|_| ()).unwrap_err();
        assert_eq!(in_use.kind(), io::ErrorKind::AddrInUse);
        // Closed without cleanup, as after a crash: the file stays behind.
        drop(live);
        let (_listener, _ours) = platform::bind_in(home.path()).unwrap();
        // The old address no longer names the file, so releasing it keeps ours.
        platform::release(&address);
        assert!(home.path().join("ipc").join("ipc.sock").exists());
    }

    #[cfg(unix)]
    #[test]
    fn serves_the_cli_over_a_socket() {
        use std::os::unix::net::{UnixListener, UnixStream};

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ipc.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let (events, mut receiver) = unbounded();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            serve(stream, &events, None);
        });
        let ui = std::thread::spawn(move || {
            if let Some(IdeEvent::Context(request)) = futures::executor::block_on(receiver.next()) {
                request.reply(json!({ "openTabs": [{ "label": "a.rs", "path": "a.rs" }] }));
            }
        });
        let mut client = UnixStream::connect(&path).unwrap();
        write_frame(&mut client, &request("ide-context")).unwrap();
        let reply = read_frame(&mut client).unwrap();
        assert_eq!(reply["result"]["ideContext"]["openTabs"][0]["path"], "a.rs");
        drop(client);
        server.join().unwrap();
        ui.join().unwrap();
    }
}
