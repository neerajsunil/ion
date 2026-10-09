//! Agents on the server reaching this machine: `ion-server` listens there,
//! and each connection to it is carried over the project's SSH link to a
//! local endpoint (Ion's IDE integrations). No extra SSH forwarding, so it
//! works wherever the link does.
//!
//! One `Accept` stream always waits on the server. The first bytes from an
//! agent mean it connected: the sink connects locally, opens the next
//! `Accept`, and a thread copies the local side's replies back.

use std::io::{self, Read, Write};
use std::sync::{Arc, OnceLock, Weak, mpsc};
use std::time::Duration;

use remote_protocol::ListenSocket;

use crate::Connection;
use crate::mux::{ChannelId, Event, Kind, Mux, Sink};

/// How long the server gets to start listening.
const LISTEN_TIMEOUT: Duration = Duration::from_secs(15);
/// Largest read from the local side per frame.
const CHUNK: usize = 64 * 1024;

/// Where agents on the server connect.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RemoteSocket {
    /// A loopback port the server picks.
    Tcp,
    /// A Unix socket path (`~/` is the server's home folder).
    Unix(String),
}

/// The local end of one forwarded connection.
pub struct LocalConnection {
    pub reader: Box<dyn Read + Send>,
    pub writer: Box<dyn Write + Send>,
    /// Ends the connection, so `reader` returns.
    pub close: Box<dyn Fn() + Send + Sync>,
}

/// The local side of a connection in progress: where the agent's bytes go,
/// and how to close it.
type Local = (Box<dyn Write + Send>, Arc<dyn Fn() + Send + Sync>);

/// Opens the local end for each connection an agent makes.
pub type Connector = Arc<dyn Fn() -> io::Result<LocalConnection> + Send + Sync>;

/// A listener on the server. Dropping it stops listening; connections in
/// progress carry on until either side closes them.
pub struct Forward {
    mux: Weak<Mux>,
    listener: ChannelId,
    address: String,
}

impl Forward {
    /// The port (for `Tcp`) or socket path agents connect to.
    pub fn address(&self) -> &str {
        &self.address
    }
}

impl Drop for Forward {
    fn drop(&mut self) {
        if let Some(mux) = self.mux.upgrade() {
            mux.close(self.listener);
        }
    }
}

impl Connection {
    /// Listens on the server and carries each connection there to `connect`.
    /// Blocks until the server is listening.
    pub fn forward(&self, socket: RemoteSocket, connect: Connector) -> io::Result<Forward> {
        let (sender, receiver) = mpsc::channel();
        let mut line = Vec::new();
        let mut reported = Some(sender);
        let sink: Sink = Box::new(move |event| {
            let result = match event {
                Event::Stdout(bytes) => {
                    line.extend_from_slice(bytes);
                    let end = line.iter().position(|byte| *byte == b'\n')?;
                    Ok(String::from_utf8_lossy(&line[..end]).into_owned())
                }
                Event::Failed(error) => Err(error),
                Event::Closed(_) => Err(io::Error::other("The server stopped listening")),
                Event::Stderr(_) | Event::Watch(_) | Event::Tick => return None,
            };
            if let Some(sender) = reported.take() {
                let _ = sender.send(result);
            }
            None
        });
        let kind = Kind::Listen(match socket {
            RemoteSocket::Tcp => ListenSocket::Tcp,
            RemoteSocket::Unix(path) => ListenSocket::Unix { path },
        });
        let (mux, listener) = self.open(kind, sink)?;
        let address = match receiver.recv_timeout(LISTEN_TIMEOUT) {
            Ok(result) => result,
            Err(_) => Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "The server didn't start listening",
            )),
        };
        // Dropped on an error, which stops the listener.
        let mut forward = Forward {
            mux: Arc::downgrade(&mux),
            listener,
            address: String::new(),
        };
        forward.address = address?;
        accept_next(&mux, listener, connect);
        Ok(forward)
    }
}

/// Opens an `Accept` stream that waits for the next agent connection.
fn accept_next(mux: &Arc<Mux>, listener: ChannelId, connect: Connector) {
    let weak = Arc::downgrade(mux);
    let id: Arc<OnceLock<ChannelId>> = Arc::default();
    let own_id = id.clone();
    let mut local: Option<Local> = None;
    let mut connected = false;
    let sink: Sink = Box::new(move |event| {
        match event {
            Event::Stdout(bytes) => {
                if !connected {
                    connected = true;
                    if let Some(mux) = weak.upgrade() {
                        // Keep one waiting for the next agent.
                        accept_next(&mux, listener, connect.clone());
                    }
                    local = start_local(&weak, &own_id, &connect);
                }
                let failed = match &mut local {
                    Some((writer, _)) => writer.write_all(bytes).and_then(|()| writer.flush()),
                    None => Err(io::ErrorKind::NotConnected.into()),
                }
                .is_err();
                if failed {
                    end_remote(&weak, &own_id);
                }
            }
            // The agent hung up (or the link dropped): end the local side.
            Event::Closed(_) | Event::Failed(_) => {
                if let Some((_, close)) = local.take() {
                    close();
                }
            }
            Event::Stderr(_) | Event::Watch(_) | Event::Tick => {}
        }
        None
    });
    let opened = mux.open(Kind::Accept { listener }, sink);
    let _ = id.set(opened);
}

/// Connects locally and starts copying the local side's replies to the
/// server. Returns the writer for the agent's bytes, and how to close.
fn start_local(
    mux: &Weak<Mux>,
    id: &Arc<OnceLock<ChannelId>>,
    connect: &Connector,
) -> Option<Local> {
    let local = connect().ok()?;
    let mut reader = local.reader;
    let mux = mux.clone();
    let id = id.clone();
    std::thread::Builder::new()
        .name("ion-ssh-forward".into())
        .spawn(move || {
            let mut buffer = vec![0u8; CHUNK];
            loop {
                let count = match reader.read(&mut buffer) {
                    Ok(0) | Err(_) => break,
                    Ok(count) => count,
                };
                let (Some(mux), Some(id)) = (mux.upgrade(), id.get()) else {
                    break;
                };
                if mux.write(*id, &buffer[..count]).is_err() {
                    break;
                }
            }
            // The local side is done: the agent sees the end of input.
            if let (Some(mux), Some(id)) = (mux.upgrade(), id.get()) {
                mux.send_eof(*id);
            }
        })
        .ok()?;
    Some((local.writer, Arc::from(local.close)))
}

/// Closes a forwarded connection whose local side failed. Not from the
/// sink itself: closing calls the sink.
fn end_remote(mux: &Weak<Mux>, id: &Arc<OnceLock<ChannelId>>) {
    let mux = mux.clone();
    let id = id.clone();
    std::thread::spawn(move || {
        if let (Some(mux), Some(id)) = (mux.upgrade(), id.get()) {
            mux.close(*id);
        }
    });
}
