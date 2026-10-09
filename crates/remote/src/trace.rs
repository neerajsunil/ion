//! `ION_REMOTE_TRACE=1` prints one stderr line per remote request (bytes each
//! way and time) and per finished command, terminal or watcher event, to see
//! where a slow connection spends its bytes. Off, it costs one branch.

use std::collections::HashMap;
use std::io::{self, Read};
use std::sync::Mutex;
use std::time::Instant;

use remote_protocol::{Operation, RemoteError, WatchEvent};

use crate::lock;

struct Started {
    label: String,
    at: Instant,
    sent: usize,
    received: usize,
}

impl Started {
    fn new(label: String, sent: usize) -> Self {
        Self {
            label,
            at: Instant::now(),
            sent,
            received: 0,
        }
    }
}

#[derive(Default)]
pub(crate) struct Trace {
    requests: Mutex<HashMap<u64, Started>>,
    streams: Mutex<HashMap<u64, Started>>,
}

/// `1.2 MB`, `340 B`.
fn size(bytes: usize) -> String {
    match bytes {
        0..1024 => format!("{bytes} B"),
        1024..1_048_576 => format!("{:.1} KB", bytes as f64 / 1024.),
        _ => format!("{:.1} MB", bytes as f64 / 1_048_576.),
    }
}

fn line(started: &Started, extra: &str) {
    eprintln!(
        "ion-remote: {} {} up, {} down{extra}, {} ms",
        started.label,
        size(started.sent),
        size(started.received),
        started.at.elapsed().as_millis()
    );
}

/// `index`, `read_text`: the operation's name without its fields.
pub(crate) fn method(operation: &Operation) -> String {
    let debug = format!("{operation:?}");
    let name = debug.split([' ', '{']).next().unwrap_or_default();
    let mut snake = String::new();
    for (i, c) in name.chars().enumerate() {
        if c.is_uppercase() && i > 0 {
            snake.push('_');
        }
        snake.push(c.to_ascii_lowercase());
    }
    snake
}

/// A command's label: what runs, without the leading `cd -- dir &&` and
/// environment assignments.
pub(crate) fn command(command: &str) -> String {
    let command = match command.strip_prefix("cd ") {
        Some(rest) => rest.split_once(" && ").map_or(command, |(_, rest)| rest),
        None => command,
    };
    let words = command.split(' ').skip_while(|word| {
        word.split_once('=')
            .is_some_and(|(name, _)| name.chars().all(|c| c.is_ascii_uppercase() || c == '_'))
    });
    let command: String = words.collect::<Vec<_>>().join(" ");
    format!("`{}`", command.chars().take(80).collect::<String>())
}

impl Trace {
    pub(crate) fn from_env() -> Option<Self> {
        std::env::var_os("ION_REMOTE_TRACE")
            .filter(|value| !value.is_empty() && value != "0")
            .map(|_| Self::default())
    }

    pub(crate) fn request(&self, id: u64, label: String, sent: usize) {
        lock(&self.requests).insert(id, Started::new(label, sent));
    }

    pub(crate) fn response(
        &self,
        id: u64,
        received: usize,
        result: &Result<serde_json::Value, RemoteError>,
    ) {
        let Some(mut started) = lock(&self.requests).remove(&id) else {
            return;
        };
        started.received = received;
        let json = match result {
            Ok(value) => serde_json::to_vec(value).map_or(0, |json| json.len()),
            Err(_) => 0,
        };
        let extra = if json > received {
            format!(" ({} as JSON)", size(json))
        } else {
            String::new()
        };
        line(&started, &extra);
    }

    pub(crate) fn stream(&self, id: u64, label: String, sent: usize) {
        lock(&self.streams).insert(id, Started::new(label, sent));
    }

    pub(crate) fn stream_sent(&self, id: u64, sent: usize) {
        if let Some(started) = lock(&self.streams).get_mut(&id) {
            started.sent += sent;
        }
    }

    pub(crate) fn stream_received(&self, id: u64, received: usize) {
        if let Some(started) = lock(&self.streams).get_mut(&id) {
            started.received += received;
        }
    }

    pub(crate) fn watch(&self, id: u64, event: &WatchEvent, received: usize) {
        self.stream_received(id, received);
        if !event.paths.is_empty() {
            eprintln!(
                "ion-remote: watch event, {} paths{}, {}",
                event.paths.len(),
                if event.structural {
                    " (structural)"
                } else {
                    ""
                },
                size(received)
            );
        }
    }

    pub(crate) fn stream_end(&self, id: u64, how: &str) {
        if let Some(started) = lock(&self.streams).remove(&id) {
            line(&started, &format!(", {how}"));
        }
    }
}

/// Counts the bytes read through it, to measure frames as they arrived.
pub(crate) struct Counting<R> {
    inner: R,
    pub(crate) count: usize,
}

impl<R> Counting<R> {
    pub(crate) fn new(inner: R) -> Self {
        Self { inner, count: 0 }
    }
}

impl<R: Read> Read for Counting<R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let count = self.inner.read(buffer)?;
        self.count += count;
        Ok(count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_operations_and_sizes() {
        let operation = Operation::ReadDir { path: "/a".into() };
        assert_eq!(method(&operation), "read_dir");
        assert_eq!(size(10), "10 B");
        assert_eq!(size(1536), "1.5 KB");
        assert_eq!(size(3 * 1_048_576), "3.0 MB");
        assert_eq!(
            command("cd -- '/a b' && GIT_X=0 LC_ALL=C git -c a=b status -z"),
            "`git -c a=b status -z`"
        );
    }
}
