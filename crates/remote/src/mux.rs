//! One `ion-server --stdio` behind one `ssh` process, carrying many streams:
//! file requests, searches, the watcher, Git commands and terminals.
//!
//! Writers send whole frames under a lock. One reader thread parses the
//! server's frames and hands each to its waiting request or stream sink. A
//! ticker thread sleeps until the earliest sink deadline (indefinitely when
//! there is none): there are no periodic wakeups. `ssh` does the keepalives;
//! a dead link ends the process, which ends the reader.

use std::collections::HashMap;
use std::io::{self, BufReader, Read, Write};
use std::process::{Child, ChildStdin, ChildStdout};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::time::Instant;

use remote_protocol::{
    ClientMessage, Frame, Operation, PtySize, RemoteError, ServerMessage, StreamSpec, WatchEvent,
    encode_data, encode_message, read_frame,
};

use crate::TerminalSize;
use crate::lock;
use crate::ssh::StderrTail;
use crate::trace::{self, Counting, Trace};

pub(crate) type ChannelId = u64;
/// A complete encoded frame, waiting for the writer thread.
type Outgoing = Vec<u8>;

type OnClosed = Box<dyn FnOnce(String) + Send>;

/// Largest slice of input sent in one frame.
const WRITE_CHUNK: usize = 256 * 1024;

pub(crate) enum Event<'a> {
    Stdout(&'a [u8]),
    Stderr(&'a [u8]),
    Watch(WatchEvent),
    /// The stream ended; the exit status of a command.
    Closed(Option<i32>),
    /// The stream (or the whole connection) failed. `Interrupted` means we
    /// closed it ourselves.
    Failed(io::Error),
    /// The deadline the sink asked for has passed.
    Tick,
}

/// Receives a stream's events on the reader (or ticker) thread. It may
/// return a deadline for a one-shot `Tick` (terminals flush synchronized
/// updates with it).
pub(crate) type Sink = Box<dyn FnMut(Event) -> Option<Instant> + Send>;

pub(crate) enum Kind {
    Exec {
        command: String,
        pty: Option<TerminalSize>,
        /// Send stderr as stdout (terminals).
        merge_stderr: bool,
    },
    Watch {
        root: String,
    },
    Listen(remote_protocol::ListenSocket),
    Accept {
        listener: ChannelId,
    },
}

#[derive(Default)]
struct Ticks {
    deadlines: HashMap<ChannelId, Instant>,
    stop: bool,
}

struct Inner {
    /// Queue to the writer thread; `None` once the link is ending.
    writer: Mutex<Option<mpsc::Sender<Outgoing>>>,
    child: Mutex<Option<Child>>,
    alive: AtomicBool,
    /// Ended on purpose: no `on_closed`.
    shutdown: AtomicBool,
    next_id: AtomicU64,
    streams: Mutex<HashMap<ChannelId, Arc<Mutex<Sink>>>>,
    pending: Mutex<HashMap<u64, mpsc::Sender<io::Result<serde_json::Value>>>>,
    ticks: Mutex<Ticks>,
    ticks_changed: Condvar,
    on_closed: Mutex<Option<OnClosed>>,
    stderr: Arc<StderrTail>,
    trace: Option<Trace>,
}

pub(crate) struct Mux {
    inner: Arc<Inner>,
}

pub(crate) fn lost(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::ConnectionAborted, message.to_owned())
}

fn remote_error(error: RemoteError) -> io::Error {
    let kind = match error.kind.as_str() {
        "NotFound" => io::ErrorKind::NotFound,
        "PermissionDenied" => io::ErrorKind::PermissionDenied,
        "AlreadyExists" => io::ErrorKind::AlreadyExists,
        "InvalidData" => io::ErrorKind::InvalidData,
        _ => io::ErrorKind::Other,
    };
    io::Error::new(kind, error.message)
}

fn pty_size(size: TerminalSize) -> PtySize {
    let clamp = |value: u32| u16::try_from(value).unwrap_or(u16::MAX);
    PtySize {
        cols: clamp(size.cols),
        rows: clamp(size.rows),
        width: clamp(size.width),
        height: clamp(size.height),
    }
}

fn spec(kind: Kind) -> StreamSpec {
    match kind {
        Kind::Exec {
            command,
            pty,
            merge_stderr,
        } => StreamSpec::Exec {
            command,
            cwd: None,
            pty: pty.map(pty_size),
            merge_stderr,
        },
        Kind::Watch { root } => StreamSpec::Watch { root },
        Kind::Listen(socket) => StreamSpec::Listen { socket },
        Kind::Accept { listener } => StreamSpec::Accept { listener },
    }
}

/// A request in flight.
pub(crate) struct Pending {
    id: u64,
    receiver: mpsc::Receiver<io::Result<serde_json::Value>>,
}

impl Pending {
    pub(crate) fn id(&self) -> u64 {
        self.id
    }

    pub(crate) fn wait(self) -> io::Result<serde_json::Value> {
        self.receiver
            .recv()
            .unwrap_or_else(|_| Err(lost("SSH connection closed")))
    }
}

impl Mux {
    /// Takes over a logged-in `ssh` whose remote end is `ion-server --stdio`
    /// (its Hello already read). `on_closed` runs once on the reader thread
    /// if the connection is lost (not after `shutdown`).
    pub(crate) fn start(
        child: Child,
        stdin: ChildStdin,
        stdout: BufReader<ChildStdout>,
        stderr: Arc<StderrTail>,
        on_closed: OnClosed,
    ) -> io::Result<Arc<Self>> {
        let (queue, outgoing) = mpsc::channel();
        let inner = Arc::new(Inner {
            writer: Mutex::new(Some(queue)),
            child: Mutex::new(Some(child)),
            alive: AtomicBool::new(true),
            shutdown: AtomicBool::new(false),
            next_id: AtomicU64::new(1),
            streams: Mutex::new(HashMap::new()),
            pending: Mutex::new(HashMap::new()),
            ticks: Mutex::new(Ticks::default()),
            ticks_changed: Condvar::new(),
            on_closed: Mutex::new(Some(on_closed)),
            stderr,
            trace: Trace::from_env(),
        });
        let reader = inner.clone();
        let spawned = std::thread::Builder::new()
            .name("ion-ssh".into())
            .spawn(move || read_loop(&reader, Counting::new(stdout)))
            .and_then(|_| {
                let writer = inner.clone();
                std::thread::Builder::new()
                    .name("ion-ssh-write".into())
                    .spawn(move || write_loop(&writer, stdin, &outgoing))
            })
            .and_then(|_| {
                let ticker = inner.clone();
                std::thread::Builder::new()
                    .name("ion-ssh-ticks".into())
                    .spawn(move || tick_loop(&ticker))
            });
        if let Err(error) = spawned {
            inner.end(true);
            if let Some(mut child) = lock(&inner.child).take() {
                let _ = child.kill();
                let _ = child.wait();
            }
            return Err(error);
        }
        Ok(Arc::new(Self { inner }))
    }

    pub(crate) fn is_alive(&self) -> bool {
        self.inner.alive.load(Ordering::Acquire)
    }

    fn next_id(&self) -> u64 {
        self.inner.next_id.fetch_add(1, Ordering::Relaxed)
    }

    pub(crate) fn open(&self, kind: Kind, sink: Sink) -> ChannelId {
        let id = self.next_id();
        let inner = &self.inner;
        let sink = Arc::new(Mutex::new(sink));
        lock(&inner.streams).insert(id, sink.clone());
        let label = inner.trace.as_ref().map(|_| match &kind {
            Kind::Exec { pty: Some(_), .. } => "terminal".to_owned(),
            Kind::Exec { command, .. } => trace::command(command),
            Kind::Watch { root } => format!("watch {root}"),
            Kind::Listen(_) => "listen".to_owned(),
            Kind::Accept { .. } => "accept".to_owned(),
        });
        // The link may have died between the check and the insert, after the
        // reader drained the streams.
        let result = if inner.alive.load(Ordering::Acquire) {
            inner.send(ClientMessage::Open {
                stream: id,
                spec: spec(kind),
            })
        } else {
            Err(lost("SSH connection closed"))
        };
        if let (Some(trace), Some(label), Ok(sent)) = (&inner.trace, label, &result) {
            trace.stream(id, label, *sent);
        }
        if let Err(error) = result
            && lock(&inner.streams).remove(&id).is_some()
        {
            (*lock(&sink))(Event::Failed(error));
        }
        id
    }

    pub(crate) fn write(&self, id: ChannelId, bytes: &[u8]) -> io::Result<()> {
        for chunk in bytes.chunks(WRITE_CHUNK) {
            self.inner.send_data(id, chunk)?;
        }
        Ok(())
    }

    pub(crate) fn send_eof(&self, id: ChannelId) {
        let _ = self.inner.send(ClientMessage::CloseInput { stream: id });
    }

    pub(crate) fn resize(&self, id: ChannelId, size: TerminalSize) {
        let _ = self.inner.send(ClientMessage::Resize {
            stream: id,
            size: pty_size(size),
        });
    }

    /// Closes a stream (killing its process). Its sink gets
    /// `Failed(Interrupted)` right away and nothing after.
    pub(crate) fn close(&self, id: ChannelId) {
        let Some(sink) = lock(&self.inner.streams).remove(&id) else {
            return;
        };
        let _ = self.inner.send(ClientMessage::Close { stream: id });
        lock(&self.inner.ticks).deadlines.remove(&id);
        if let Some(trace) = &self.inner.trace {
            trace.stream_end(id, "closed");
        }
        (*lock(&sink))(Event::Failed(io::Error::new(
            io::ErrorKind::Interrupted,
            "Channel closed",
        )));
    }

    pub(crate) fn begin_request(&self, operation: Operation) -> io::Result<Pending> {
        let id = self.next_id();
        let (sender, receiver) = mpsc::channel();
        lock(&self.inner.pending).insert(id, sender);
        let label = self.inner.trace.as_ref().map(|_| trace::method(&operation));
        let result = if self.is_alive() {
            self.inner.send(ClientMessage::Request { id, operation })
        } else {
            Err(lost("SSH connection closed"))
        };
        match result {
            Ok(sent) => {
                if let (Some(trace), Some(label)) = (&self.inner.trace, label) {
                    trace.request(id, label, sent);
                }
                Ok(Pending { id, receiver })
            }
            Err(error) => {
                lock(&self.inner.pending).remove(&id);
                Err(error)
            }
        }
    }

    pub(crate) fn request(&self, operation: Operation) -> io::Result<serde_json::Value> {
        self.begin_request(operation)?.wait()
    }

    /// Asks the server to stop request `id`.
    pub(crate) fn cancel(&self, id: u64) {
        let _ = self.inner.send(ClientMessage::Cancel { id });
    }

    /// Ends the connection on purpose (no `on_closed`).
    pub(crate) fn shutdown(&self) {
        self.inner.end(true);
    }
}

impl Drop for Mux {
    fn drop(&mut self) {
        self.shutdown();
    }
}

impl Inner {
    /// Queues a frame; never blocks on the pipe.
    fn enqueue(&self, frame: Outgoing) -> io::Result<()> {
        match lock(&self.writer).as_ref() {
            Some(queue) => queue.send(frame).map_err(|_| lost("SSH connection closed")),
            None => Err(lost("SSH connection closed")),
        }
    }

    /// Queues a message; returns its size on the wire.
    fn send(&self, message: ClientMessage) -> io::Result<usize> {
        let frame = encode_message(&message)?;
        let sent = frame.len();
        self.enqueue(frame)?;
        Ok(sent)
    }

    fn send_data(&self, id: ChannelId, bytes: &[u8]) -> io::Result<()> {
        // Encoded (and compressed) here, so the writer thread only writes.
        let frame = encode_data(id, false, bytes)?;
        if let Some(trace) = &self.trace {
            trace.stream_sent(id, frame.len());
        }
        self.enqueue(frame)
    }

    /// Stops `ssh`; the reader then sees the end of its output and wraps up.
    fn end(&self, on_purpose: bool) {
        if on_purpose {
            self.shutdown.store(true, Ordering::Release);
        }
        self.alive.store(false, Ordering::Release);
        // Kill first so a writer stuck on a stalled pipe gets an error.
        if let Some(child) = lock(&self.child).as_mut() {
            let _ = child.kill();
        }
        *lock(&self.writer) = None;
        lock(&self.ticks).stop = true;
        self.ticks_changed.notify_all();
    }

    fn set_deadline(&self, id: ChannelId, deadline: Option<Instant>) {
        let mut ticks = lock(&self.ticks);
        match deadline {
            Some(deadline) => {
                ticks.deadlines.insert(id, deadline);
                drop(ticks);
                self.ticks_changed.notify_one();
            }
            None => {
                ticks.deadlines.remove(&id);
            }
        }
    }

    fn deliver(&self, id: ChannelId, event: Event) {
        let Some(sink) = lock(&self.streams).get(&id).cloned() else {
            return;
        };
        let deadline = (*lock(&sink))(event);
        self.set_deadline(id, deadline);
    }

    /// Handles one message that took `wire` bytes to arrive.
    fn dispatch(&self, message: ServerMessage, wire: usize) {
        match message {
            ServerMessage::Response { id, result } => {
                if let Some(trace) = &self.trace {
                    trace.response(id, wire, &result);
                }
                if let Some(sender) = lock(&self.pending).remove(&id) {
                    let _ = sender.send(result.map_err(remote_error));
                }
            }
            ServerMessage::Watch { stream, event } => {
                if let Some(trace) = &self.trace {
                    trace.watch(stream, &event, wire);
                }
                self.deliver(stream, Event::Watch(event));
            }
            ServerMessage::Exit {
                stream,
                code,
                error,
            } => {
                let Some(sink) = lock(&self.streams).remove(&stream) else {
                    return;
                };
                let event = match error {
                    Some(error) => Event::Failed(io::Error::other(error)),
                    None => Event::Closed(code),
                };
                (*lock(&sink))(event);
                lock(&self.ticks).deadlines.remove(&stream);
                if let Some(trace) = &self.trace {
                    let how = code.map_or("failed".to_owned(), |code| format!("exit {code}"));
                    trace.stream_end(stream, &how);
                }
            }
        }
    }

    /// The connection is gone: fail everything waiting, reap `ssh`, report.
    fn finish(&self, reason: String) {
        self.alive.store(false, Ordering::Release);
        let on_purpose = self.shutdown.load(Ordering::Acquire);
        let reason = if on_purpose {
            "SSH connection closed".to_owned()
        } else {
            match self.stderr.last_message() {
                Some(message) => format!("{reason}: {message}"),
                None => reason,
            }
        };
        for (_, sender) in lock(&self.pending).drain() {
            let _ = sender.send(Err(lost(&reason)));
        }
        let streams: Vec<_> = lock(&self.streams).drain().collect();
        for (_, sink) in streams {
            (*lock(&sink))(Event::Failed(lost(&reason)));
        }
        self.end(false);
        if let Some(mut child) = lock(&self.child).take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        if !on_purpose && let Some(on_closed) = lock(&self.on_closed).take() {
            on_closed(reason);
        }
    }
}

/// Owns `ssh`'s stdin: writes queued frames whole, in order. A write error
/// means the link is dying; ending it brings the reader down too.
fn write_loop(inner: &Inner, mut stdin: ChildStdin, outgoing: &mpsc::Receiver<Outgoing>) {
    while let Ok(frame) = outgoing.recv() {
        if stdin
            .write_all(&frame)
            .and_then(|()| stdin.flush())
            .is_err()
        {
            inner.end(false);
            return;
        }
    }
}

fn read_loop(inner: &Inner, mut stdout: Counting<BufReader<ChildStdout>>) {
    let reason = loop {
        let before = stdout.count;
        let frame = read_frame::<ServerMessage>(&mut stdout);
        let wire = stdout.count - before;
        match frame {
            Ok(Some(Frame::Message(message))) => inner.dispatch(message, wire),
            Ok(Some(Frame::Data {
                stream,
                stderr,
                bytes,
            })) => {
                if let Some(trace) = &inner.trace {
                    trace.stream_received(stream, wire);
                }
                inner.deliver(
                    stream,
                    if stderr {
                        Event::Stderr(&bytes)
                    } else {
                        Event::Stdout(&bytes)
                    },
                );
            }
            Ok(None) => break "The SSH connection was closed".to_owned(),
            Err(error) => break format!("SSH connection lost: {error}"),
        }
    };
    inner.finish(reason);
}

/// Fires sink deadlines. Sleeps until the earliest one, or until woken when
/// there is none.
fn tick_loop(inner: &Inner) {
    let mut ticks = lock(&inner.ticks);
    loop {
        if ticks.stop {
            return;
        }
        let next = ticks
            .deadlines
            .iter()
            .min_by_key(|(_, deadline)| **deadline)
            .map(|(id, deadline)| (*id, *deadline));
        let Some((id, deadline)) = next else {
            ticks = inner
                .ticks_changed
                .wait(ticks)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            continue;
        };
        let now = Instant::now();
        if now < deadline {
            ticks = inner
                .ticks_changed
                .wait_timeout(ticks, deadline - now)
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .0;
            continue;
        }
        ticks.deadlines.remove(&id);
        drop(ticks);
        inner.deliver(id, Event::Tick);
        ticks = lock(&inner.ticks);
    }
}

/// A stream used like a pipe from a blocking thread: Git and other commands
/// read and write it with ordinary `Read`/`Write`.
pub(crate) struct Stream {
    mux: Arc<Mux>,
    id: ChannelId,
    receiver: mpsc::Receiver<Chunk>,
    buffer: Vec<u8>,
    position: usize,
    status: Option<Option<i32>>,
    stderr: Vec<u8>,
}

enum Chunk {
    Stdout(Vec<u8>),
    Stderr(Vec<u8>),
    Closed(Option<i32>),
    Failed(io::Error),
}

impl Stream {
    pub(crate) fn exec(mux: &Arc<Mux>, command: &str) -> Self {
        let (sender, receiver) = mpsc::channel();
        let sink: Sink = Box::new(move |event| {
            let chunk = match event {
                Event::Stdout(bytes) => Chunk::Stdout(bytes.to_vec()),
                Event::Stderr(bytes) => Chunk::Stderr(bytes.to_vec()),
                Event::Closed(status) => Chunk::Closed(status),
                Event::Failed(error) => Chunk::Failed(error),
                Event::Watch(_) | Event::Tick => return None,
            };
            let _ = sender.send(chunk);
            None
        });
        let id = mux.open(
            Kind::Exec {
                command: command.to_owned(),
                pty: None,
                merge_stderr: false,
            },
            sink,
        );
        Self {
            mux: mux.clone(),
            id,
            receiver,
            buffer: Vec::new(),
            position: 0,
            status: None,
            stderr: Vec::new(),
        }
    }

    pub(crate) fn id(&self) -> ChannelId {
        self.id
    }

    pub(crate) fn mux(&self) -> &Arc<Mux> {
        &self.mux
    }

    pub(crate) fn send_eof(&self) {
        self.mux.send_eof(self.id);
    }

    /// Waits for more stdout. False once the stream has ended.
    fn fill(&mut self) -> io::Result<bool> {
        while self.position >= self.buffer.len() {
            if self.status.is_some() {
                return Ok(false);
            }
            match self.receiver.recv() {
                Ok(Chunk::Stdout(bytes)) => {
                    self.buffer = bytes;
                    self.position = 0;
                }
                Ok(Chunk::Stderr(bytes)) => {
                    // Keep the start of stderr for error messages.
                    let room = (64 * 1024usize).saturating_sub(self.stderr.len());
                    self.stderr
                        .extend_from_slice(&bytes[..bytes.len().min(room)]);
                }
                Ok(Chunk::Closed(status)) => self.status = Some(status),
                Ok(Chunk::Failed(error)) => {
                    self.status = Some(None);
                    return Err(error);
                }
                Err(_) => {
                    self.status = Some(None);
                    return Err(lost("SSH connection closed"));
                }
            }
        }
        Ok(true)
    }

    /// Reads everything to the end: (stdout, stderr, exit status).
    pub(crate) fn finish(mut self) -> io::Result<(Vec<u8>, Vec<u8>, i32)> {
        let mut stdout = Vec::new();
        self.read_to_end(&mut stdout)?;
        let status = self.status.flatten().unwrap_or(-1);
        Ok((stdout, std::mem::take(&mut self.stderr), status))
    }
}

impl Read for Stream {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if !self.fill()? {
            return Ok(0);
        }
        let count = out.len().min(self.buffer.len() - self.position);
        out[..count].copy_from_slice(&self.buffer[self.position..self.position + count]);
        self.position += count;
        Ok(count)
    }
}

impl Write for Stream {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.mux.write(self.id, bytes)?;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        if self.status.is_none() {
            self.mux.close(self.id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oversized_requests_fail_before_reaching_the_writer() {
        let big = ClientMessage::Request {
            id: 1,
            operation: Operation::SaveText {
                path: "/f".into(),
                text: "x".repeat(remote_protocol::MAX_FRAME),
                has_bom: false,
            },
        };
        let error = encode_message(&big).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        let frame = encode_message(&ClientMessage::Cancel { id: 7 }).unwrap();
        match read_frame::<ClientMessage>(&mut frame.as_slice()).unwrap() {
            Some(Frame::Message(ClientMessage::Cancel { id: 7 })) => {}
            other => panic!("{other:?}"),
        }
    }
}
