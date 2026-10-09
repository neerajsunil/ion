//! Client-opened streams: shell commands, terminals and file watchers, all multiplexed
//! over the single stdio connection. Every thread blocks on a read; none polls.
use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::process::{Child, Command, Stdio};
use std::sync::{
    Arc, Mutex,
    mpsc::{Sender, channel, sync_channel},
};
use std::thread::{self, JoinHandle};

use remote_protocol::{PtySize, ServerMessage, StreamSpec, WatchEvent};

use crate::watch::Msg;
use crate::{Output, send, send_data};

struct Handle {
    input: Option<Sender<Vec<u8>>>,
    resize: Option<Box<dyn Fn(PtySize) + Send>>,
    kill: Box<dyn Fn() + Send>,
}
struct Running {
    child: Child,
    input: Box<dyn Write + Send>,
    readers: Vec<JoinHandle<()>>,
    resize: Option<Box<dyn Fn(PtySize) + Send>>,
}

#[derive(Clone)]
pub struct Streams {
    output: Output,
    map: Arc<Mutex<HashMap<u64, Handle>>>,
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|error| error.into_inner())
}
fn pump(mut source: impl Read, output: Output, stream: u64, stderr: bool) {
    let mut buffer = vec![0; 32 * 1024];
    while let Ok(count) = source.read(&mut buffer) {
        if count == 0 || send_data(&output, stream, stderr, &buffer[..count]).is_err() {
            break;
        }
    }
}
/// Kills the process and anything left in its group (each child leads its own).
fn kill_tree(pid: u32) {
    #[cfg(unix)]
    if let Some(pid) = rustix::process::Pid::from_raw(pid as i32) {
        let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::KILL);
    }
    #[cfg(not(unix))]
    let _ = Command::new("taskkill")
        .args(["/F", "/T", "/PID", &pid.to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

#[cfg(unix)]
struct Shared(Arc<pty_process::blocking::Pty>);
#[cfg(unix)]
impl Read for Shared {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        (&*self.0).read(buffer)
    }
}
#[cfg(unix)]
impl Write for Shared {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        (&*self.0).write(buffer)
    }
    fn flush(&mut self) -> io::Result<()> {
        (&*self.0).flush()
    }
}
#[cfg(unix)]
fn pty_size(size: PtySize) -> pty_process::Size {
    pty_process::Size::new_with_pixel(size.rows, size.cols, size.width, size.height)
}

impl Streams {
    pub fn new(output: Output) -> Self {
        Self {
            output,
            map: Arc::default(),
        }
    }

    pub fn open(&self, stream: u64, spec: StreamSpec) {
        if lock(&self.map).contains_key(&stream) {
            return self.exit(stream, None, Some("Stream already open".into()));
        }
        let started = match spec {
            StreamSpec::Exec {
                command,
                cwd,
                pty: None,
                merge_stderr,
            } => self.spawn_pipes(stream, &command, cwd.as_deref(), merge_stderr),
            StreamSpec::Exec {
                command,
                cwd,
                pty: Some(size),
                ..
            } => self.spawn_pty(stream, &command, cwd.as_deref(), size),
            StreamSpec::Watch { root } => return self.watch(stream, root),
        };
        match started {
            Ok(running) => self.register(stream, running),
            Err(error) => self.exit(stream, None, Some(error.to_string())),
        }
    }

    pub fn input(&self, stream: u64, bytes: Vec<u8>) {
        if let Some(input) = lock(&self.map)
            .get(&stream)
            .and_then(|handle| handle.input.as_ref())
        {
            let _ = input.send(bytes);
        }
    }
    pub fn resize(&self, stream: u64, size: PtySize) {
        if let Some(resize) = lock(&self.map)
            .get(&stream)
            .and_then(|handle| handle.resize.as_ref())
        {
            resize(size);
        }
    }
    pub fn close_input(&self, stream: u64) {
        if let Some(handle) = lock(&self.map).get_mut(&stream) {
            handle.input = None;
        }
    }
    pub fn close(&self, stream: u64) {
        if let Some(handle) = lock(&self.map).remove(&stream) {
            (handle.kill)();
        }
    }
    pub fn close_all(&self) {
        for (_, handle) in lock(&self.map).drain() {
            (handle.kill)();
        }
    }

    fn exit(&self, stream: u64, code: Option<i32>, error: Option<String>) {
        let _ = send(
            &self.output,
            &ServerMessage::Exit {
                stream,
                code,
                error,
            },
        );
    }

    fn spawn_pipes(
        &self,
        stream: u64,
        command: &str,
        cwd: Option<&str>,
        merge_stderr: bool,
    ) -> io::Result<Running> {
        let mut process = Command::new("sh");
        process
            .args(["-c", command])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(cwd) = cwd {
            process.current_dir(cwd);
        }
        #[cfg(unix)]
        std::os::unix::process::CommandExt::process_group(&mut process, 0);
        let mut child = process.spawn()?;
        let input = child.stdin.take().expect("Piped stdin");
        let stdout = child.stdout.take().expect("Piped stdout");
        let stderr = child.stderr.take().expect("Piped stderr");
        let readers = vec![
            self.reader(stdout, stream, false),
            self.reader(stderr, stream, !merge_stderr),
        ];
        Ok(Running {
            child,
            input: Box::new(input),
            readers,
            resize: None,
        })
    }

    #[cfg(unix)]
    fn spawn_pty(
        &self,
        stream: u64,
        command: &str,
        cwd: Option<&str>,
        size: PtySize,
    ) -> io::Result<Running> {
        let (pty, pts) = pty_process::blocking::open().map_err(io::Error::other)?;
        pty.resize(pty_size(size)).map_err(io::Error::other)?;
        let mut process = pty_process::blocking::Command::new("sh")
            .args(["-c", command])
            .env("TERM", "xterm-256color");
        if let Some(cwd) = cwd {
            process = process.current_dir(cwd);
        }
        let child = process.spawn(pts).map_err(io::Error::other)?;
        let pty = Arc::new(pty);
        let readers = vec![self.reader(Shared(pty.clone()), stream, false)];
        let resizer = pty.clone();
        Ok(Running {
            child,
            input: Box::new(Shared(pty)),
            readers,
            resize: Some(Box::new(move |size| {
                let _ = resizer.resize(pty_size(size));
            })),
        })
    }
    #[cfg(not(unix))]
    fn spawn_pty(&self, _: u64, _: &str, _: Option<&str>, _: PtySize) -> io::Result<Running> {
        Err(io::Error::other("Terminals need a Unix server"))
    }

    fn reader(
        &self,
        source: impl Read + Send + 'static,
        stream: u64,
        stderr: bool,
    ) -> JoinHandle<()> {
        let output = self.output.clone();
        thread::spawn(move || pump(source, output, stream, stderr))
    }

    fn register(&self, stream: u64, running: Running) {
        let Running {
            mut child,
            mut input,
            readers,
            resize,
        } = running;
        let pid = child.id();
        // Once the child is reaped its pid may be reused: never signal it then.
        let reaped = Arc::new(Mutex::new(false));
        let reaped_by_waiter = reaped.clone();
        let (sender, receiver) = channel::<Vec<u8>>();
        // A child that stops reading would block its writes; keep them off the dispatcher.
        thread::spawn(move || {
            for chunk in receiver {
                if input
                    .write_all(&chunk)
                    .and_then(|()| input.flush())
                    .is_err()
                {
                    break;
                }
            }
        });
        lock(&self.map).insert(
            stream,
            Handle {
                input: Some(sender),
                resize,
                kill: Box::new(move || {
                    if !*lock(&reaped) {
                        kill_tree(pid);
                    }
                }),
            },
        );
        let this = self.clone();
        thread::spawn(move || {
            let status = child.wait();
            *lock(&reaped_by_waiter) = true;
            for reader in readers {
                let _ = reader.join();
            }
            lock(&this.map).remove(&stream);
            match status {
                Ok(status) => this.exit(
                    stream,
                    status.code(),
                    status
                        .code()
                        .is_none()
                        .then(|| "Terminated by a signal".into()),
                ),
                Err(error) => this.exit(stream, None, Some(error.to_string())),
            }
        });
    }

    fn watch(&self, stream: u64, root: String) {
        let (tx, rx) = sync_channel(256);
        let stop = tx.clone();
        lock(&self.map).insert(
            stream,
            Handle {
                input: None,
                resize: None,
                kill: Box::new(move || {
                    let _ = stop.send(Msg::Stop);
                }),
            },
        );
        let this = self.clone();
        thread::spawn(move || {
            let output = this.output.clone();
            let emit =
                move |event: WatchEvent| send(&output, &ServerMessage::Watch { stream, event });
            let result = crate::watch::run(&root, emit, tx, rx);
            lock(&this.map).remove(&stream);
            match result {
                Ok(()) => this.exit(stream, Some(0), None),
                Err(error) => this.exit(stream, None, Some(error.to_string())),
            }
        });
    }
}
