//! Listeners for agents on the server to reach the client (Ion's IDE
//! integrations): a `Listen` stream owns the socket, and each `Accept`
//! stream carries one connection over the stdio link. Threads block in
//! `accept` or on a read; none polls.
use std::io::{self, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
#[cfg(unix)]
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use remote_protocol::ListenSocket;

/// A bound socket, shared by its `Listen` stream and pending `Accept`s.
pub struct Listening {
    socket: Socket,
    /// What the `Listen` stream reports: a port, or a socket path.
    pub address: String,
    closed: AtomicBool,
    /// `Accept` streams blocked in `accept`, woken on close.
    waiting: AtomicUsize,
}

enum Socket {
    Tcp(TcpListener),
    #[cfg(unix)]
    Unix(std::os::unix::net::UnixListener, Unix),
}

#[cfg(unix)]
struct Unix {
    path: PathBuf,
    inode: u64,
}

/// One accepted connection, split for the two pump threads.
pub struct Accepted {
    pub reader: Box<dyn Read + Send>,
    pub writer: Box<dyn Write + Send>,
    /// Ends the connection, waking its reader.
    pub shutdown: Box<dyn Fn() + Send + Sync>,
    /// Ends only what the server sends, so the agent sees the end of input.
    pub shutdown_write: Box<dyn Fn() + Send + Sync>,
}

impl Listening {
    pub fn bind(socket: &ListenSocket) -> io::Result<Self> {
        let (socket, address) = match socket {
            ListenSocket::Tcp => {
                let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))?;
                let port = listener.local_addr()?.port();
                (Socket::Tcp(listener), port.to_string())
            }
            #[cfg(unix)]
            ListenSocket::Unix { path } => {
                let path = expand_home(path)?;
                let (listener, unix) = bind_unix(path)?;
                let address = unix.path.to_string_lossy().into_owned();
                (Socket::Unix(listener, unix), address)
            }
            #[cfg(not(unix))]
            ListenSocket::Unix { .. } => {
                return Err(io::Error::other("Unix sockets need a Unix server"));
            }
        };
        Ok(Self {
            socket,
            address,
            closed: AtomicBool::new(false),
            waiting: AtomicUsize::new(0),
        })
    }

    /// Waits for the next connection; fails once the listener is closed.
    pub fn accept(&self) -> io::Result<Accepted> {
        self.waiting.fetch_add(1, Ordering::SeqCst);
        let accepted = self.accept_inner();
        self.waiting.fetch_sub(1, Ordering::SeqCst);
        if self.closed.load(Ordering::SeqCst) {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "Stopped listening",
            ));
        }
        accepted
    }

    fn accept_inner(&self) -> io::Result<Accepted> {
        match &self.socket {
            Socket::Tcp(listener) => {
                let (stream, _) = listener.accept()?;
                stream.set_nodelay(true)?;
                split(stream, TcpStream::try_clone, |s, how| {
                    let _ = s.shutdown(how);
                })
            }
            #[cfg(unix)]
            Socket::Unix(listener, _) => {
                let (stream, _) = listener.accept()?;
                split(
                    stream,
                    std::os::unix::net::UnixStream::try_clone,
                    |s, how| {
                        let _ = s.shutdown(how);
                    },
                )
            }
        }
    }

    /// Stops listening: wakes every waiting `Accept` and removes the socket file.
    pub fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
        for _ in 0..self.waiting.load(Ordering::SeqCst) {
            match &self.socket {
                Socket::Tcp(listener) => {
                    if let Ok(address) = listener.local_addr() {
                        let _ = TcpStream::connect(address);
                    }
                }
                #[cfg(unix)]
                Socket::Unix(_, unix) => {
                    let _ = std::os::unix::net::UnixStream::connect(&unix.path);
                }
            }
        }
        #[cfg(unix)]
        if let Socket::Unix(_, unix) = &self.socket {
            use std::os::unix::fs::MetadataExt;
            // Only Ion's own socket: something else may have replaced it.
            if std::fs::metadata(&unix.path).is_ok_and(|meta| meta.ino() == unix.inode) {
                let _ = std::fs::remove_file(&unix.path);
            }
        }
    }
}

fn split<S: Read + Write + Send + Sync + 'static>(
    stream: S,
    clone: fn(&S) -> io::Result<S>,
    shutdown: fn(&S, Shutdown),
) -> io::Result<Accepted> {
    let reader = clone(&stream)?;
    let writer = clone(&stream)?;
    let stream = Arc::new(Mutex::new(stream));
    let for_write = stream.clone();
    Ok(Accepted {
        reader: Box::new(reader),
        writer: Box::new(writer),
        shutdown: Box::new(move || {
            if let Ok(stream) = stream.lock() {
                shutdown(&stream, Shutdown::Both);
            }
        }),
        shutdown_write: Box::new(move || {
            if let Ok(stream) = for_write.lock() {
                shutdown(&stream, Shutdown::Write);
            }
        }),
    })
}

#[cfg(unix)]
fn expand_home(path: &str) -> io::Result<PathBuf> {
    match path.strip_prefix("~/") {
        Some(rest) => {
            let home = std::env::var_os("HOME").ok_or_else(|| io::Error::other("No HOME"))?;
            Ok(PathBuf::from(home).join(rest))
        }
        None => Ok(PathBuf::from(path)),
    }
}

#[cfg(unix)]
fn bind_unix(path: PathBuf) -> io::Result<(std::os::unix::net::UnixListener, Unix)> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    use std::os::unix::net::{UnixListener, UnixStream};

    let dir = path
        .parent()
        .ok_or_else(|| io::Error::other("The socket path has no folder"))?;
    std::fs::create_dir_all(dir)?;
    // Clients (Codex) refuse a folder other users could write to.
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    if UnixStream::connect(&path).is_ok() {
        return Err(io::Error::new(
            io::ErrorKind::AddrInUse,
            "Something already listens there",
        ));
    }
    // Nobody answers: a socket left by a process that ended.
    match std::fs::remove_file(&path) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
        _ => {}
    }
    let listener = UnixListener::bind(&path)?;
    let inode = std::fs::metadata(&path)?.ino();
    Ok((listener, Unix { path, inode }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tcp_connections_carry_bytes_and_close_wakes_accept() {
        let listening = Arc::new(Listening::bind(&ListenSocket::Tcp).unwrap());
        let port: u16 = listening.address.parse().unwrap();
        let client = std::thread::spawn(move || {
            let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
            stream.write_all(b"ping").unwrap();
            let mut reply = [0u8; 4];
            stream.read_exact(&mut reply).unwrap();
            reply
        });
        let mut accepted = listening.accept().unwrap();
        let mut request = [0u8; 4];
        accepted.reader.read_exact(&mut request).unwrap();
        assert_eq!(&request, b"ping");
        accepted.writer.write_all(b"pong").unwrap();
        assert_eq!(&client.join().unwrap(), b"pong");

        let waiting = listening.clone();
        let blocked = std::thread::spawn(move || waiting.accept().map(|_| ()));
        while listening.waiting.load(Ordering::SeqCst) == 0 {
            std::thread::yield_now();
        }
        listening.close();
        let error = blocked.join().unwrap().unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Interrupted);
    }

    #[cfg(unix)]
    #[test]
    fn unix_sockets_take_over_only_dead_ones() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ipc").join("ipc.sock");
        let spec = ListenSocket::Unix {
            path: path.to_string_lossy().into_owned(),
        };
        let first = Listening::bind(&spec).unwrap();
        let in_use = Listening::bind(&spec).map(|_| ()).unwrap_err();
        assert_eq!(in_use.kind(), io::ErrorKind::AddrInUse);
        // Ended without cleanup: the file stays, and nobody answers.
        drop(first);
        let second = Listening::bind(&spec).unwrap();
        second.close();
        assert!(!path.exists());
    }
}
