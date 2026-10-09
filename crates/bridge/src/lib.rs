//! The bridge to terminal agents: a local MCP server that harness IDE
//! integrations connect to (Claude Code's `/ide`), and the socket Codex's
//! `/ide` asks for context (see [`codex`]).
//!
//! Ion listens on a random localhost port and writes a lock file under
//! `~/.claude/ide/` naming the port, the open folders and a secret token.
//! Agents started in Ion's terminals find the port through environment
//! variables; others discover it with `/ide`. Messages are JSON-RPC (MCP)
//! over WebSocket.
//!
//! Threads: one blocked in `accept`, plus one per connected agent blocked
//! reading its socket. Nothing wakes while no agent talks. Pure logic with
//! no UI dependencies; tool calls go to the UI through a channel.

mod codex;
mod mcp;
mod ws;

pub use codex::ContextRequest;

use std::io::{self, BufReader, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use serde_json::{Value, json};

use crate::mcp::Dispatch;
use crate::ws::Incoming;

/// Clients must finish the opening handshake within this time.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);

/// What the server tells the UI.
pub enum IdeEvent {
    /// An agent connected or disconnected; how many are connected now.
    Connections(usize),
    Call(ToolCall),
    /// Codex wants the active file, selection and open tabs.
    Context(ContextRequest),
}

/// A tool call from an agent. Reply exactly once; dropping it unanswered
/// replies with an error so the agent doesn't wait forever.
pub struct ToolCall {
    pub name: String,
    pub arguments: Value,
    id: Value,
    connection: Arc<Connection>,
    answered: bool,
}

impl ToolCall {
    /// A string argument, or `None` if it's missing or empty.
    pub fn string(&self, name: &str) -> Option<&str> {
        self.arguments
            .get(name)
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
    }

    pub fn bool(&self, name: &str, default: bool) -> bool {
        self.arguments
            .get(name)
            .and_then(Value::as_bool)
            .unwrap_or(default)
    }

    /// Which connection asked, so calls can be cancelled when it goes away.
    pub fn connection_id(&self) -> u64 {
        self.connection.id
    }

    pub fn reply_text(mut self, texts: &[&str]) {
        self.answered = true;
        self.connection
            .send_json(&mcp::text_result(self.id.clone(), texts));
    }

    /// Replies with a JSON value as the single text block.
    pub fn reply_json(self, value: Value) {
        self.reply_text(&[&value.to_string()]);
    }

    pub fn reply_error(mut self, message: &str) {
        self.answered = true;
        self.connection
            .send_json(&mcp::error(self.id.clone(), -32603, message));
    }
}

impl Drop for ToolCall {
    fn drop(&mut self) {
        if !self.answered {
            self.connection.send_json(&mcp::error(
                self.id.clone(),
                -32603,
                "The IDE closed without answering",
            ));
        }
    }
}

struct Connection {
    id: u64,
    /// The write half; frames are written whole under the lock.
    writer: Mutex<TcpStream>,
}

impl Connection {
    fn send(&self, bytes: &[u8]) {
        if let Ok(mut writer) = self.writer.lock() {
            // A failed write means the agent left; its reader notices.
            writer.write_all(bytes).ok();
        }
    }

    fn send_json(&self, value: &Value) {
        self.send(&ws::text_frame(&value.to_string()));
    }
}

struct Shared {
    token: String,
    connections: Mutex<Vec<Arc<Connection>>>,
    next_id: AtomicU64,
    stopped: AtomicBool,
    events: UnboundedSender<IdeEvent>,
}

impl Shared {
    fn connection_count(&self) -> usize {
        self.connections.lock().map_or(0, |list| list.len())
    }
}

/// The running server. Dropping it removes the lock file and disconnects
/// every agent.
pub struct IdeServer {
    port: u16,
    ide_name: String,
    lock_path: Option<PathBuf>,
    shared: Arc<Shared>,
    codex: Option<codex::Listener>,
}

impl IdeServer {
    /// Starts listening on a random localhost port.
    pub fn start(ide_name: &str) -> io::Result<(Self, UnboundedReceiver<IdeEvent>)> {
        let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))?;
        let port = listener.local_addr()?.port();
        let (events, receiver) = unbounded();
        let shared = Arc::new(Shared {
            token: new_token()?,
            connections: Mutex::default(),
            next_id: AtomicU64::new(1),
            stopped: AtomicBool::new(false),
            events,
        });
        let accept_shared = shared.clone();
        std::thread::Builder::new()
            .name("ion-ide-accept".into())
            .spawn(move || accept_loop(listener, accept_shared))?;
        let server = Self {
            port,
            ide_name: ide_name.to_owned(),
            lock_path: lock_dir().map(|dir| dir.join(format!("{port}.lock"))),
            shared,
            codex: None,
        };
        Ok((server, receiver))
    }

    /// Also answers Codex's `/ide`. Fails when another editor already does.
    pub fn listen_for_codex(&mut self) -> io::Result<()> {
        if self.codex.is_none() {
            self.codex = Some(codex::Listener::start(self.shared.events.clone())?);
        }
        Ok(())
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    /// Environment variables for terminals, so agents started in them
    /// connect without `/ide`.
    pub fn terminal_env(&self) -> Vec<(String, String)> {
        vec![
            ("CLAUDE_CODE_SSE_PORT".into(), self.port.to_string()),
            ("ENABLE_IDE_INTEGRATION".into(), "true".into()),
        ]
    }

    pub fn connection_count(&self) -> usize {
        self.shared.connection_count()
    }

    /// Writes the lock file that advertises the server for these folders.
    pub fn set_workspace_folders(&self, folders: &[PathBuf]) -> io::Result<()> {
        let Some(path) = &self.lock_path else {
            return Ok(());
        };
        let folders: Vec<String> = folders
            .iter()
            .map(|folder| folder.to_string_lossy().into_owned())
            .collect();
        let lock = self.lock_file(&folders, std::process::id(), cfg!(windows));
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(path, lock)
    }

    /// A lock file's contents, for these folders. On an SSH server, `pid` is
    /// a process there that lives as long as the connection.
    pub fn lock_file(&self, folders: &[String], pid: u32, windows: bool) -> String {
        json!({
            "pid": pid,
            "workspaceFolders": folders,
            "ideName": self.ide_name,
            "transport": "ws",
            "runningInWindows": windows,
            "authToken": self.shared.token,
        })
        .to_string()
    }

    /// Opens connections for a Codex CLI on an SSH server, answered as if it
    /// had connected here; its requests carry `origin`. Each call returns the
    /// end to read Ion's replies from and the end to write the CLI's bytes to.
    pub fn codex_connector(
        &self,
        origin: u64,
    ) -> impl Fn() -> io::Result<(io::PipeReader, io::PipeWriter)> + Send + Sync + 'static {
        let events = self.shared.events.clone();
        move || codex::connect(events.clone(), origin)
    }

    /// Sends a notification (e.g. `selection_changed`) to every agent.
    pub fn notify(&self, method: &str, params: Value) {
        let frame = ws::text_frame(&mcp::notification(method, params).to_string());
        let connections = self
            .shared
            .connections
            .lock()
            .map(|list| list.clone())
            .unwrap_or_default();
        for connection in connections {
            connection.send(&frame);
        }
    }
}

impl Drop for IdeServer {
    fn drop(&mut self) {
        self.shared.stopped.store(true, Ordering::SeqCst);
        if let Some(path) = &self.lock_path {
            std::fs::remove_file(path).ok();
        }
        // Wake the accept thread so it sees `stopped` and exits.
        TcpStream::connect(SocketAddr::from(([127, 0, 0, 1], self.port))).ok();
        if let Ok(connections) = self.shared.connections.lock() {
            for connection in connections.iter() {
                if let Ok(writer) = connection.writer.lock() {
                    writer.shutdown(Shutdown::Both).ok();
                }
            }
        }
    }
}

fn accept_loop(listener: TcpListener, shared: Arc<Shared>) {
    for stream in listener.incoming() {
        if shared.stopped.load(Ordering::SeqCst) {
            break;
        }
        let Ok(stream) = stream else {
            continue;
        };
        let shared = shared.clone();
        std::thread::Builder::new()
            .name("ion-ide-agent".into())
            .spawn(move || serve(stream, shared))
            .ok();
    }
}

/// Runs one connection until the agent leaves.
fn serve(mut stream: TcpStream, shared: Arc<Shared>) {
    stream.set_nodelay(true).ok();
    stream.set_read_timeout(Some(HANDSHAKE_TIMEOUT)).ok();
    let Ok(read_half) = stream.try_clone() else {
        return;
    };
    let mut reader = BufReader::new(read_half);
    let Ok(handshake) = ws::read_handshake(&mut reader) else {
        return;
    };
    let authorized = handshake
        .auth
        .as_deref()
        .is_some_and(|token| constant_time_eq(token.as_bytes(), shared.token.as_bytes()));
    if !authorized {
        ws::write_unauthorized(&mut stream).ok();
        return;
    }
    if ws::write_accept(&mut stream, &handshake).is_err() {
        return;
    }
    stream.set_read_timeout(None).ok();
    let connection = Arc::new(Connection {
        id: shared.next_id.fetch_add(1, Ordering::Relaxed),
        writer: Mutex::new(stream),
    });
    if let Ok(mut list) = shared.connections.lock() {
        list.push(connection.clone());
    }
    let count = shared.connection_count();
    shared
        .events
        .unbounded_send(IdeEvent::Connections(count))
        .ok();

    while let Ok(message) = ws::read_message(&mut reader) {
        match message {
            Incoming::Text(text) => handle_text(&text, &connection, &shared),
            Incoming::Ping(payload) => connection.send(&ws::pong_frame(&payload)),
            Incoming::Close => {
                connection.send(&ws::close_frame());
                break;
            }
        }
    }

    if let Ok(mut list) = shared.connections.lock() {
        list.retain(|other| other.id != connection.id);
    }
    let count = shared.connection_count();
    shared
        .events
        .unbounded_send(IdeEvent::Connections(count))
        .ok();
}

fn handle_text(text: &str, connection: &Arc<Connection>, shared: &Shared) {
    let Ok(value) = serde_json::from_str::<Value>(text) else {
        connection.send_json(&mcp::error(Value::Null, -32700, "Parse error"));
        return;
    };
    let messages = match value {
        Value::Array(batch) => batch,
        message => vec![message],
    };
    for message in messages {
        match mcp::dispatch(&message) {
            Dispatch::Reply(reply) => connection.send_json(&reply),
            Dispatch::Call(id, name, arguments) => {
                let call = ToolCall {
                    name,
                    arguments,
                    id,
                    connection: connection.clone(),
                    answered: false,
                };
                // If the UI is gone the call drops and answers with an error.
                shared.events.unbounded_send(IdeEvent::Call(call)).ok();
            }
            Dispatch::Ignore => {}
        }
    }
}

/// Where Claude Code looks for IDE lock files.
fn lock_dir() -> Option<PathBuf> {
    let config = std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .or_else(|| {
            let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })?;
            Some(Path::new(&home).join(".claude"))
        })?;
    Some(config.join("ide"))
}

/// 32 lowercase hex characters from the OS's secure random source.
fn new_token() -> io::Result<String> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).map_err(|err| io::Error::other(err.to_string()))?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    use std::io::Read;

    use futures::StreamExt;

    use super::*;

    /// Connects like Claude Code does and returns the socket after the upgrade.
    fn connect(port: u16, token: &str) -> io::Result<TcpStream> {
        let mut stream = TcpStream::connect(("127.0.0.1", port))?;
        write!(
            stream,
            "GET / HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nUpgrade: websocket\r\n\
             Connection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\
             Sec-WebSocket-Version: 13\r\nx-claude-code-ide-authorization: {token}\r\n\r\n"
        )?;
        let mut response = [0u8; 12];
        stream.read_exact(&mut response)?;
        if &response != b"HTTP/1.1 101" {
            return Err(io::Error::other("refused"));
        }
        // Skip the rest of the response headers.
        let mut last = [0u8; 4];
        while &last != b"\r\n\r\n" {
            let mut byte = [0u8; 1];
            stream.read_exact(&mut byte)?;
            last.rotate_left(1);
            last[3] = byte[0];
        }
        Ok(stream)
    }

    /// A masked client text frame.
    fn send(stream: &mut TcpStream, text: &str) {
        let mut frame = ws::text_frame(text);
        let header = frame.len() - text.len();
        frame[1] |= 0x80;
        let payload = frame.split_off(header);
        frame.extend_from_slice(&[0, 0, 0, 0]);
        frame.extend(payload);
        stream.write_all(&frame).unwrap();
    }

    fn receive(stream: &mut TcpStream) -> Value {
        match ws::read_message(stream).unwrap() {
            Incoming::Text(text) => serde_json::from_str(&text).unwrap(),
            other => panic!("expected text, got {other:?}"),
        }
    }

    #[test]
    fn serves_an_authorized_agent() {
        let (mut server, mut events) = IdeServer::start("Ion").unwrap();
        // Keep the test's lock file out of the real config folder.
        server.lock_path = None;
        let port = server.port();
        assert!(connect(port, "wrong").is_err());

        let token = server.shared.token.clone();
        let mut stream = connect(port, &token).unwrap();
        let Some(IdeEvent::Connections(1)) = futures::executor::block_on(events.next()) else {
            panic!("connection event");
        };
        send(
            &mut stream,
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
        );
        assert_eq!(receive(&mut stream)["result"]["serverInfo"]["name"], "ion");

        send(
            &mut stream,
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"getWorkspaceFolders"}}"#,
        );
        let Some(IdeEvent::Call(call)) = futures::executor::block_on(events.next()) else {
            panic!("tool call event");
        };
        assert_eq!(call.name, "getWorkspaceFolders");
        call.reply_json(json!({ "success": true }));
        let reply = receive(&mut stream);
        assert_eq!(reply["id"], 2);
        assert_eq!(reply["result"]["content"][0]["text"], r#"{"success":true}"#);

        server.notify("selection_changed", json!({ "text": "x" }));
        assert_eq!(receive(&mut stream)["method"], "selection_changed");
    }

    #[test]
    fn tokens_are_32_hex_characters() {
        let token = new_token().unwrap();
        assert_eq!(token.len(), 32);
        assert!(
            token
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        );
    }
}
