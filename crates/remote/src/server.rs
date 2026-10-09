//! Deployment and requests, carried by the project's SSH connection.
//!
//! The `ssh` process starts a plain `sh`. Over its stdio Ion finds out what
//! the host is, installs the matching `ion-server` if needed and replaces the
//! shell with `ion-server --stdio`, which then speaks the framed protocol.
use std::io::{self, BufRead, Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};

use remote_protocol::{Frame, Hello, Operation, PROTOCOL, VERSION, read_frame};
use serde::de::DeserializeOwned;
use sha2::{Digest, Sha256};

use crate::askpass::random_token;
use crate::ssh::Process;
use crate::{Connection, lock, shell_quote};
include!(concat!(env!("OUT_DIR"), "/server_assets.rs"));

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn download(file: &str) -> io::Result<Vec<u8>> {
    let url =
        format!("https://github.com/neerajsunil/ion/releases/download/server-v{VERSION}/{file}");
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(std::time::Duration::from_secs(60)))
        .build()
        .into();
    agent
        .get(&url)
        .call()
        .map_err(io::Error::other)?
        .body_mut()
        .with_config()
        .limit(32 * 1024 * 1024)
        .read_to_vec()
        .map_err(io::Error::other)
}

/// A server binary for one target: its checksum is known up front, the bytes
/// are only read (or downloaded) when the host doesn't have it yet.
struct Artifact {
    hash: String,
    source: Source,
}

enum Source {
    Bundled(&'static [u8]),
    Dir(std::path::PathBuf, String),
    Release(String),
}

impl Artifact {
    fn locate(target: &str) -> io::Result<Self> {
        if let Some(dir) = std::env::var_os("ION_SERVER_ASSETS") {
            let dir = std::path::PathBuf::from(dir);
            let (file, hash) = from_manifest(target, &std::fs::read(dir.join("manifest.json"))?)?;
            return Ok(Self {
                hash,
                source: Source::Dir(dir, file),
            });
        }
        if let Some((_, hash, bytes)) = BUNDLED.iter().find(|(platform, _, _)| *platform == target)
        {
            return Ok(Self {
                hash: (*hash).into(),
                source: Source::Bundled(bytes),
            });
        }
        let (file, hash) = from_manifest(target, &download("manifest.json")?)?;
        Ok(Self {
            hash,
            source: Source::Release(file),
        })
    }

    /// The binary, checked against the manifest's checksum.
    fn bytes(&self) -> io::Result<Vec<u8>> {
        let bytes = match &self.source {
            Source::Bundled(bytes) => bytes.to_vec(),
            Source::Dir(dir, file) => std::fs::read(dir.join(file))?,
            Source::Release(file) => download(file)?,
        };
        if digest(&bytes) != self.hash {
            return Err(io::Error::other("Server binary checksum mismatch"));
        }
        Ok(bytes)
    }
}

/// The artifact file and checksum for `target`, from a manifest that must
/// match this client's version and protocol.
fn from_manifest(target: &str, manifest: &[u8]) -> io::Result<(String, String)> {
    let value: serde_json::Value = serde_json::from_slice(manifest)?;
    if value["version"] != VERSION || value["protocol"] != PROTOCOL {
        return Err(io::Error::other(format!(
            "the published server is version {}, protocol {}; this build needs {VERSION}, {PROTOCOL}",
            value["version"], value["protocol"]
        )));
    }
    let entry = value["artifacts"]
        .as_array()
        .and_then(|items| items.iter().find(|item| item["target"] == target))
        .ok_or_else(|| io::Error::other("Server release has no binary for this server's system"))?;
    let expected = release_file(target)
        .ok_or_else(|| io::Error::other("Server release has an unknown target"))?;
    if entry["file"] != expected {
        return Err(io::Error::other("Invalid server artifact filename"));
    }
    let hash = entry["sha256"]
        .as_str()
        .filter(|hash| hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit()))
        .ok_or_else(|| io::Error::other("Invalid server artifact checksum"))?;
    Ok((expected, hash.to_ascii_lowercase()))
}

/// Longest line accepted from the remote shell during setup.
const MAX_LINE: u64 = 64 * 1024;

/// Output lines kept per step (the tail is what matters).
const KEPT_LINES: usize = 64;

struct Step {
    lines: Vec<String>,
    status: i32,
}

impl Step {
    /// The failure message of a step that exited non-zero.
    fn failure(&self) -> io::Error {
        io::Error::other(
            self.lines
                .last()
                .cloned()
                .unwrap_or_else(|| format!("exit status {}", self.status)),
        )
    }

    fn ok(self) -> io::Result<Vec<String>> {
        if self.status == 0 {
            Ok(self.lines)
        } else {
            Err(self.failure())
        }
    }
}

fn keep(lines: &mut Vec<String>, line: String) {
    if line.trim().is_empty() {
        return;
    }
    if lines.len() == KEPT_LINES {
        lines.remove(0);
    }
    lines.push(line);
}

enum Line {
    Output(String),
    Marker(i32),
    Ready,
}

/// Talks to the remote `sh`, one command at a time. The shell may print
/// startup noise and may read ahead, so each command ends with a marker line
/// carrying its exit status, and nothing more is sent before it arrives.
struct Shell<R, W> {
    reader: R,
    writer: W,
    token: String,
    step: u32,
}

impl<R: BufRead, W: Write> Shell<R, W> {
    fn new(reader: R, writer: W, token: String) -> Self {
        Self {
            reader,
            writer,
            token,
            step: 0,
        }
    }

    fn marker(&self) -> String {
        format!("__ION_{}_{}__", self.token, self.step)
    }

    fn ready_marker(&self) -> String {
        format!("__ION_{}_ready__", self.token)
    }

    /// `command` on one line, then the marker with its exit status. One
    /// line, so the shell has read all of it before running any of it.
    fn send(&mut self, command: &str) -> io::Result<()> {
        self.step += 1;
        let line = format!("{command}; printf '\\n%s %s\\n' {} \"$?\"\n", self.marker());
        self.writer.write_all(line.as_bytes())?;
        self.writer.flush()
    }

    fn read_line(&mut self, wait_for_ready: bool) -> io::Result<Line> {
        let mut raw = Vec::new();
        let count = (&mut self.reader)
            .take(MAX_LINE)
            .read_until(b'\n', &mut raw)?;
        if count as u64 == MAX_LINE && raw.last() != Some(&b'\n') {
            return Err(io::Error::other(
                "The server's shell printed an overlong line during setup",
            ));
        }
        if count == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "The SSH connection closed during setup",
            ));
        }
        let line = String::from_utf8_lossy(&raw);
        let line = line.trim_end_matches(['\r', '\n']);
        if let Some(status) = line
            .strip_prefix(&self.marker())
            .and_then(|rest| rest.strip_prefix(' '))
            .and_then(|status| status.trim().parse().ok())
        {
            return Ok(Line::Marker(status));
        }
        if wait_for_ready && line == self.ready_marker() {
            return Ok(Line::Ready);
        }
        Ok(Line::Output(line.to_owned()))
    }

    fn read_step(&mut self) -> io::Result<Step> {
        let mut lines = Vec::new();
        loop {
            match self.read_line(false)? {
                Line::Output(line) => keep(&mut lines, line),
                Line::Marker(status) => return Ok(Step { lines, status }),
                Line::Ready => unreachable!("only awaited by upload"),
            }
        }
    }

    fn run(&mut self, command: &str) -> io::Result<Step> {
        self.send(command)?;
        self.read_step()
    }

    /// Runs a command that prints the ready marker and then reads exactly
    /// `bytes` from stdin; sends `bytes` once the marker arrives.
    fn upload(&mut self, command: impl FnOnce(&str) -> String, bytes: &[u8]) -> io::Result<Step> {
        let ready = self.ready_marker();
        self.send(&command(&ready))?;
        let mut lines = Vec::new();
        loop {
            match self.read_line(true)? {
                Line::Ready => break,
                // The command failed before it started reading.
                Line::Marker(status) => return Ok(Step { lines, status }),
                Line::Output(line) => keep(&mut lines, line),
            }
        }
        self.writer.write_all(bytes)?;
        self.writer.flush()?;
        self.read_step()
    }

    fn probe(&mut self, destination: &str) -> io::Result<()> {
        let step = self.run(&format!("{} --version 2>&1", shell_quote(destination)))?;
        if step.status != 0 {
            return Err(step.failure());
        }
        let line = step
            .lines
            .iter()
            .rev()
            .find(|line| line.starts_with('{'))
            .ok_or_else(|| io::Error::other("The Ion server printed no version"))?;
        serde_json::from_str::<Hello>(line)?.validate()
    }

    /// Installs the matching `ion-server` if it isn't there yet, and returns
    /// its path.
    fn install(&mut self) -> io::Result<String> {
        let info = self.run(r#"uname -s; uname -m; printf '%s\n' "$HOME""#)?;
        let tail = &info.lines[info.lines.len().saturating_sub(3)..];
        if info.status != 0 || tail.len() < 3 {
            return Err(io::Error::other("Couldn't identify the server's system"));
        }
        let target = server_target(&tail[0], &tail[1])?;
        let home = Some(tail[2].trim_end_matches('/'))
            .filter(|home| home.starts_with('/'))
            .ok_or_else(|| io::Error::other("Couldn't find your home folder on the server"))?
            .to_owned();
        let obtain = |error| io::Error::other(format!("Could not obtain the Ion server: {error}"));
        let artifact = Artifact::locate(target).map_err(obtain)?;
        let hash = &artifact.hash;
        let root = format!("{home}/.ion/server");
        let leaf = format!("{}-{}", platform(target).unwrap_or(target), &hash[..16]);
        let dir = format!("{root}/{VERSION}/{leaf}");
        let destination = format!("{dir}/ion-server");
        if self.probe(&destination).is_err() {
            let bytes = artifact.bytes().map_err(obtain)?;
            static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let temp = format!(
                "{destination}.upload-{}-{}",
                std::process::id(),
                SEQUENCE.fetch_add(1, Ordering::Relaxed)
            );
            let (dir_q, temp_q, dest_q) = (
                shell_quote(&dir),
                shell_quote(&temp),
                shell_quote(&destination),
            );
            let result = (|| {
                self.upload(
                    |ready| {
                        format!(
                            "mkdir -p {dir_q} && {{ chmod 700 {dir_q} 2>/dev/null || true; }} && umask 077 && printf '%s\\n' {ready} && head -c {} > {temp_q}",
                            bytes.len()
                        )
                    },
                    &bytes,
                )?
                .ok()?;
                // Read it back: the upload must arrive intact.
                let sums = self
                    .run(&format!(
                        "sha256sum {temp_q} 2>/dev/null || shasum -a 256 {temp_q}"
                    ))?
                    .ok()?;
                let sum = sums
                    .last()
                    .and_then(|line| line.split_whitespace().next())
                    .unwrap_or_default();
                if !sum.eq_ignore_ascii_case(hash) {
                    return Err(io::Error::other("Uploaded server checksum mismatch"));
                }
                // Only this versioned cache file is replaced, never a project file.
                self.run(&format!("chmod 700 {temp_q} && mv -f {temp_q} {dest_q}"))?
                    .ok()
                    .map(drop)
            })();
            if result.is_err() {
                let _ = self.run(&format!("rm -f {temp_q}"));
            }
            result.map_err(|error| {
                io::Error::other(format!("Couldn't install the Ion server: {error}"))
            })?;
            self.probe(&destination).map_err(|error| io::Error::other(format!("Ion server could not start: {error}. Check that your home directory permits executable files.")))?;
            // The new server works: drop other versions and builds. A server
            // still running from one keeps its open file until it exits.
            let _ = self.run(&prune_command(&root, &leaf));
        }
        Ok(destination)
    }

    /// Replaces the shell with the server.
    fn exec(&mut self, destination: &str) -> io::Result<()> {
        self.writer
            .write_all(format!("exec {} --stdio\n", shell_quote(destination)).as_bytes())?;
        self.writer.flush()
    }
}

/// The short platform name (`linux-x64`) used for a server build's release
/// file and install folder; the manifest keys builds by Rust target.
fn platform(target: &str) -> Option<&'static str> {
    Some(match target {
        "x86_64-unknown-linux-musl" => "linux-x64",
        "aarch64-unknown-linux-musl" => "linux-arm64",
        "aarch64-apple-darwin" => "macos-arm64",
        _ => return None,
    })
}

fn release_file(target: &str) -> Option<String> {
    platform(target).map(|platform| format!("ion-server-{VERSION}-{platform}"))
}

/// The server build for a host, from its `uname -s` and `uname -m`.
fn server_target(system: &str, arch: &str) -> io::Result<&'static str> {
    let target = match (system, arch) {
        ("Linux", "x86_64" | "amd64") => "x86_64-unknown-linux-musl",
        ("Linux", "aarch64" | "arm64") => "aarch64-unknown-linux-musl",
        ("Darwin", "arm64") => "aarch64-apple-darwin",
        ("Darwin", _) => {
            return Err(io::Error::other(
                "Remote projects on a Mac need Apple Silicon; Intel Macs aren't supported",
            ));
        }
        ("Linux", _) => {
            return Err(io::Error::other(format!(
                "Unsupported remote {system} architecture: {arch:?}"
            )));
        }
        _ => {
            return Err(io::Error::other(format!(
                "Remote projects support Linux and macOS servers, not {system:?}"
            )));
        }
    };
    Ok(target)
}

/// Removes everything under `root` except `<VERSION>/<leaf>`.
fn prune_command(root: &str, leaf: &str) -> String {
    let keep = |name: &str| shell_quote(&format!("{name}/"));
    format!(
        "(cd {} 2>/dev/null || exit 0; for d in */; do [ -d \"$d\" ] && [ \"$d\" != {} ] && rm -rf -- \"$d\"; done; cd {} 2>/dev/null || exit 0; for d in */; do [ -d \"$d\" ] && [ \"$d\" != {} ] && rm -rf -- \"$d\"; done; true)",
        shell_quote(root),
        keep(VERSION),
        shell_quote(VERSION),
        keep(leaf)
    )
}

/// Runs the setup on a freshly started `ssh`: installs the server if needed
/// and starts it. Returns the server's path once its Hello has been
/// validated; `process.stdout` then carries frames.
pub(crate) fn bootstrap(process: &mut Process) -> io::Result<String> {
    let mut shell = Shell::new(&mut process.stdout, &mut process.stdin, random_token());
    let destination = shell.install()?;
    shell.exec(&destination)?;
    let Some(Frame::Message(hello)) = read_frame::<Hello>(&mut process.stdout)? else {
        return Err(io::Error::other("Ion server closed before handshake"));
    };
    hello.validate()?;
    Ok(destination)
}

impl Connection {
    /// The server's path on the host, once connected.
    pub fn ensure_server(&self) -> io::Result<String> {
        self.link()?;
        lock(&self.server)
            .clone()
            .ok_or_else(|| io::Error::other("Ion server isn't running"))
    }

    pub fn request<T: DeserializeOwned>(&self, operation: Operation) -> io::Result<T> {
        let value = self.link()?.request(operation)?;
        serde_json::from_value(value).map_err(io::Error::other)
    }

    /// Long requests (index, search). A `Search` can be stopped with
    /// [`Connection::cancel_jobs`] or by setting `cancel` beforehand.
    pub fn request_job<T: DeserializeOwned>(
        &self,
        operation: Operation,
        cancel: &AtomicBool,
    ) -> io::Result<T> {
        let cancelled = || cancel.load(Ordering::Relaxed);
        if cancelled() {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "Search cancelled",
            ));
        }
        let link = self.link()?;
        let cancellable = matches!(&operation, Operation::Search { .. });
        let pending = link.begin_request(operation)?;
        let id = pending.id();
        if cancellable {
            lock(&self.jobs).push((link.clone(), id));
            if cancelled() {
                self.cancel_jobs();
            }
        }
        let result = pending.wait();
        if cancellable {
            lock(&self.jobs).retain(|(_, job)| *job != id);
        }
        serde_json::from_value(result?).map_err(io::Error::other)
    }

    /// Stops the running searches on the server.
    pub fn cancel_jobs(&self) {
        let jobs: Vec<_> = lock(&self.jobs).drain(..).collect();
        for (link, id) in jobs {
            link.cancel(id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn picks_server_target_from_uname() {
        let target = |system, arch| server_target(system, arch).ok();
        assert_eq!(target("Linux", "x86_64"), Some("x86_64-unknown-linux-musl"));
        assert_eq!(target("Linux", "arm64"), Some("aarch64-unknown-linux-musl"));
        assert_eq!(target("Darwin", "arm64"), Some("aarch64-apple-darwin"));
        assert_eq!(target("Darwin", "x86_64"), None);
        assert_eq!(target("Linux", "riscv64"), None);
        assert_eq!(target("FreeBSD", "amd64"), None);
        for (system, arch) in [
            ("Linux", "x86_64"),
            ("Linux", "aarch64"),
            ("Darwin", "arm64"),
        ] {
            assert!(release_file(target(system, arch).unwrap()).is_some());
        }
    }

    #[test]
    fn validates_artifact_before_installing() {
        let target = "x86_64-unknown-linux-musl";
        let manifest = serde_json::json!({"version":VERSION,"protocol":PROTOCOL,"artifacts":[{"target":target,"file":format!("ion-server-{VERSION}-linux-x64"),"sha256":digest(b"binary")}]});
        let bytes = serde_json::to_vec(&manifest).unwrap();
        let (file, hash) = from_manifest(target, &bytes).unwrap();
        assert_eq!(file, format!("ion-server-{VERSION}-linux-x64"));
        let dir = std::env::temp_dir().join(format!("ion-artifact-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let artifact = |contents: &[u8]| {
            std::fs::write(dir.join(&file), contents).unwrap();
            Artifact {
                hash: hash.clone(),
                source: Source::Dir(dir.clone(), file.clone()),
            }
            .bytes()
        };
        assert_eq!(artifact(b"binary").unwrap(), b"binary");
        assert!(artifact(b"corrupt").is_err());
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(from_manifest("other", &bytes).is_err());
        let stale = serde_json::json!({"version":VERSION,"protocol":PROTOCOL - 1,"artifacts":[]});
        let error = from_manifest(target, &serde_json::to_vec(&stale).unwrap()).unwrap_err();
        assert!(error.to_string().contains("protocol"));
    }

    #[test]
    fn prune_keeps_only_the_current_server() {
        let command = prune_command("/home/dev/.ion/server", "linux-x64-0123");
        assert!(command.starts_with("(cd '/home/dev/.ion/server' "));
        assert!(command.contains(&format!("!= '{VERSION}/'")));
        assert!(command.contains("!= 'linux-x64-0123/'"));
        assert!(command.ends_with("; true)"));
    }

    fn shell(output: &str) -> Shell<Cursor<Vec<u8>>, Vec<u8>> {
        Shell::new(
            Cursor::new(output.as_bytes().to_vec()),
            Vec::new(),
            "tok".into(),
        )
    }

    #[test]
    fn steps_skip_shell_noise_and_end_at_their_marker() {
        let mut shell = shell(
            "Welcome!\r\nLinux\nx86_64\n/home/dev\n\n__ION_tok_1__ 0\nmore\n\n__ION_tok_2__ 3\n",
        );
        let step = shell.run("uname -s").unwrap();
        assert_eq!(step.status, 0);
        assert_eq!(step.lines, ["Welcome!", "Linux", "x86_64", "/home/dev"]);
        let sent = String::from_utf8(shell.writer.clone()).unwrap();
        assert_eq!(
            sent,
            "uname -s; printf '\\n%s %s\\n' __ION_tok_1__ \"$?\"\n"
        );
        let step = shell.run("false").unwrap();
        assert_eq!(step.status, 3);
        assert_eq!(step.lines, ["more"]);
        assert!(step.ok().is_err());
        // The shell never answered: the link is gone.
        let error = shell.run("true").err().unwrap();
        assert_eq!(error.kind(), io::ErrorKind::UnexpectedEof);
    }

    #[test]
    fn overlong_shell_lines_are_refused() {
        let long = "x".repeat(MAX_LINE as usize + 10);
        let mut shell = shell(&format!("{long}\n__ION_tok_1__ 0\n"));
        assert!(shell.run("x").is_err());
    }

    #[test]
    fn markers_of_other_steps_are_just_output() {
        let mut shell = shell("__ION_tok_10__ 0\n__ION_other_1__ 0\n__ION_tok_1__ 7\n");
        let step = shell.run("x").unwrap();
        assert_eq!(step.status, 7);
        assert_eq!(step.lines.len(), 2);
    }

    #[test]
    fn upload_waits_for_ready_before_sending_bytes() {
        let mut ok = shell("motd\n__ION_tok_ready__\n\n__ION_tok_1__ 0\n");
        let step = ok
            .upload(|ready| format!("echo {ready}; head -c 3 > f"), b"abc")
            .unwrap();
        assert_eq!(step.status, 0);
        let sent = String::from_utf8(ok.writer.clone()).unwrap();
        assert!(sent.starts_with("echo __ION_tok_ready__; head -c 3 > f; printf"));
        assert!(sent.ends_with("abc"));
        // A command that fails first never gets the bytes.
        let mut failed = shell("mkdir: permission denied\n\n__ION_tok_1__ 1\n");
        let step = failed.upload(|_| "mkdir x".into(), b"abc").unwrap();
        assert_eq!(step.status, 1);
        assert!(
            !String::from_utf8(failed.writer.clone())
                .unwrap()
                .contains("abc")
        );
        assert!(step.failure().to_string().contains("permission denied"));
    }
}
