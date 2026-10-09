mod service;
mod streams;
mod watch;

use remote_protocol::{
    ClientMessage, Frame, Hello, RemoteError, ServerMessage, read_frame, write_data, write_message,
};
use std::collections::HashMap;
use std::io::{self, Stdout, Write};
use std::panic::AssertUnwindSafe;
use std::sync::{
    Arc, Mutex, MutexGuard,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

/// Every frame goes through this lock, so frames from different threads never interleave.
pub type Output = Arc<Mutex<Stdout>>;
type Cancels = Arc<Mutex<HashMap<u64, Arc<AtomicBool>>>>;

fn locked(output: &Output) -> io::Result<MutexGuard<'_, Stdout>> {
    output
        .lock()
        .map_err(|_| io::Error::other("Output lock poisoned"))
}
pub fn send(output: &Output, message: &ServerMessage) -> io::Result<()> {
    write_message(&mut *locked(output)?, message)
}
pub fn send_data(output: &Output, stream: u64, stderr: bool, bytes: &[u8]) -> io::Result<()> {
    write_data(&mut *locked(output)?, stream, stderr, bytes)
}

/// Counts an active request until dropped, even if its thread panics.
struct ActiveSlot(Arc<AtomicUsize>);
impl Drop for ActiveSlot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

fn run_stdio() -> io::Result<()> {
    let mut input = io::stdin().lock();
    let output: Output = Arc::new(Mutex::new(io::stdout()));
    write_message(&mut *locked(&output)?, &Hello::current())?;
    let service = Arc::new(service::Service::default());
    let streams = streams::Streams::new(output.clone());
    let cancels = Cancels::default();
    let active = Arc::new(AtomicUsize::new(0));
    std::thread::scope(|scope| {
        let result = (|| {
            loop {
                let message = match read_frame::<ClientMessage>(&mut input)? {
                    Some(Frame::Message(message)) => message,
                    Some(Frame::Data { stream, bytes, .. }) => {
                        streams.input(stream, bytes);
                        continue;
                    }
                    None => return Ok(()),
                };
                match message {
                    ClientMessage::Request { id, operation } => {
                        if active.load(Ordering::Relaxed) >= 16 {
                            send(
                                &output,
                                &ServerMessage::Response {
                                    id,
                                    result: Err(RemoteError {
                                        kind: "Busy".into(),
                                        message: "Too many active Ion requests".into(),
                                    }),
                                },
                            )?;
                            continue;
                        }
                        active.fetch_add(1, Ordering::Relaxed);
                        let slot = ActiveSlot(active.clone());
                        let flag = Arc::new(AtomicBool::new(false));
                        if let Ok(mut map) = cancels.lock() {
                            map.insert(id, flag.clone());
                        }
                        let service = service.clone();
                        let output = output.clone();
                        let cancels = cancels.clone();
                        scope.spawn(move || {
                            let _slot = slot;
                            let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
                                service.handle(operation, &flag)
                            }))
                            .unwrap_or_else(|_| {
                                Err(RemoteError {
                                    kind: "Other".into(),
                                    message: "Internal error in the Ion server".into(),
                                })
                            });
                            if let Ok(mut map) = cancels.lock() {
                                map.remove(&id);
                            }
                            if send(&output, &ServerMessage::Response { id, result }).is_err() {
                                // Most likely too large for one frame: say so
                                // rather than leaving the request unanswered.
                                let _ = send(
                                    &output,
                                    &ServerMessage::Response {
                                        id,
                                        result: Err(RemoteError {
                                            kind: "InvalidData".into(),
                                            message: "The result is too large to send".into(),
                                        }),
                                    },
                                );
                            }
                        });
                    }
                    ClientMessage::Cancel { id } => {
                        let flag = cancels.lock().ok().and_then(|map| map.get(&id).cloned());
                        if let Some(flag) = flag {
                            flag.store(true, Ordering::Relaxed);
                        }
                    }
                    ClientMessage::Open { stream, spec } => streams.open(stream, spec),
                    ClientMessage::Resize { stream, size } => streams.resize(stream, size),
                    ClientMessage::CloseInput { stream } => streams.close_input(stream),
                    ClientMessage::Close { stream } => streams.close(stream),
                }
            }
        })();
        if let Ok(map) = cancels.lock() {
            for flag in map.values() {
                flag.store(true, Ordering::Relaxed);
            }
        }
        streams.close_all();
        result
    })
}

fn run() -> io::Result<()> {
    match std::env::args().nth(1).as_deref() {
        Some("--version") => {
            writeln!(
                io::stdout(),
                "{}",
                serde_json::to_string(&Hello::current())?
            )?;
            Ok(())
        }
        Some("--stdio") => run_stdio(),
        _ => Err(io::Error::other("Usage: ion-server --version | --stdio")),
    }
}
fn main() {
    if let Err(error) = run() {
        eprintln!("Ion server: {error}");
        std::process::exit(1);
    }
}
