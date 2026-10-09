//! Tagged frames over SSH stdio: JSON messages plus raw stream data, multiplexed on one
//! connection. No listener or additional network port.
//!
//! Frame: `u32 BE length` (of the rest) + `u8 tag` + payload. Tag 0 is a JSON message, tag 1
//! is stream data (`u64 BE stream` + bytes; stdin to the server, stdout from it), tag 2 is
//! stderr data (server to client).
//!
//! A tag with [`COMPRESSED`] set carries its body (after any stream number) as an LZ4 block,
//! preceded by its `u32 LE` uncompressed size. Senders compress bodies of at least
//! [`COMPRESS_MIN`] bytes when that makes them smaller, so keystrokes and small replies go
//! as they are while file lists, file contents and command output shrink several times.
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::io::{self, Read, Write};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const PROTOCOL: u32 = 4;
/// Largest frame, and largest body once decompressed.
pub const MAX_FRAME: usize = 64 * 1024 * 1024;
/// Flag on a frame tag: the body is compressed.
pub const COMPRESSED: u8 = 0x80;
/// Smaller bodies are sent uncompressed.
pub const COMPRESS_MIN: usize = 4 * 1024;

#[derive(Debug, Serialize, Deserialize)]
pub struct Hello {
    pub version: String,
    pub protocol: u32,
}
impl Hello {
    pub fn current() -> Self {
        Self {
            version: VERSION.into(),
            protocol: PROTOCOL,
        }
    }
    pub fn validate(&self) -> io::Result<()> {
        if self.version != VERSION || self.protocol != PROTOCOL {
            return Err(io::Error::other(format!(
                "Incompatible Ion server: version {}, protocol {}; expected {VERSION}, {PROTOCOL}",
                self.version, self.protocol
            )));
        }
        Ok(())
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "method", rename_all = "snake_case")]
pub enum Operation {
    Canonicalize {
        path: String,
    },
    ReadText {
        path: String,
    },
    SaveText {
        path: String,
        text: String,
        has_bom: bool,
    },
    ReadDir {
        path: String,
    },
    IsDir {
        path: String,
    },
    Exists {
        path: String,
    },
    /// Those of `paths` that are regular files (following symlinks).
    Files {
        paths: Vec<String>,
    },
    CreateDir {
        path: String,
    },
    CreateFile {
        path: String,
    },
    Rename {
        from: String,
        to: String,
    },
    CopyInto {
        from: String,
        dir: String,
        cut: bool,
    },
    Delete {
        paths: Vec<String>,
    },
    /// The project's files; answered with an [`IndexUpdate`].
    Index {
        root: String,
        /// The version of the list the client already holds, to receive only the changes.
        #[serde(default)]
        base: Option<String>,
    },
    Search {
        root: String,
        query: String,
        case_sensitive: bool,
    },
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RemoteError {
    pub kind: String,
    pub message: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Text {
    pub text: String,
    pub has_bom: bool,
}
#[derive(Debug, Serialize, Deserialize)]
pub struct Entry {
    pub name: String,
    pub is_dir: bool,
}
/// A project's file list (paths relative to the root, with `/`). With `base`, `added` and
/// `removed` are the changes since that version; otherwise `added` is the whole list.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct IndexUpdate {
    pub version: String,
    pub base: Option<String>,
    pub added: Vec<String>,
    #[serde(default)]
    pub removed: Vec<String>,
}
#[derive(Debug, Serialize, Deserialize)]
pub struct Match {
    pub path: String,
    pub line: usize,
    pub columns: [usize; 2],
    pub preview: String,
    pub preview_range: [usize; 2],
}
#[derive(Debug, Serialize, Deserialize)]
pub struct SearchResults {
    pub matches: Vec<Match>,
    pub truncated: bool,
}
#[derive(Debug, Serialize, Deserialize)]
pub struct WatchEvent {
    #[serde(default)]
    pub ready: bool,
    #[serde(default)]
    pub paths: Vec<String>,
    #[serde(default)]
    pub structural: bool,
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
    Request { id: u64, operation: Operation },
    Cancel { id: u64 },
    Open { stream: u64, spec: StreamSpec },
    Resize { stream: u64, size: PtySize },
    CloseInput { stream: u64 },
    Close { stream: u64 },
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMessage {
    Response {
        id: u64,
        result: Result<serde_json::Value, RemoteError>,
    },
    /// Always the last message for a stream.
    Exit {
        stream: u64,
        code: Option<i32>,
        error: Option<String>,
    },
    Watch {
        stream: u64,
        event: WatchEvent,
    },
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StreamSpec {
    /// Runs through `sh -c`; with `pty`, in a new session with a controlling terminal.
    Exec {
        command: String,
        cwd: Option<String>,
        pty: Option<PtySize>,
        merge_stderr: bool,
    },
    Watch {
        root: String,
    },
    /// Listens on the server, for agents there to reach the client. The stream's first
    /// stdout line is the address (a port, or a socket path); closing it stops listening.
    Listen {
        socket: ListenSocket,
    },
    /// Waits for one connection to the `listener` stream, then carries its bytes both
    /// ways. The first stdout data means a connection arrived.
    Accept {
        listener: u64,
    },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ListenSocket {
    /// A loopback TCP port the server picks.
    Tcp,
    /// A Unix socket at `path` (`~/` is the home folder), in a folder only the user can
    /// open. Fails if something already listens there; replaces a dead socket.
    Unix { path: String },
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PtySize {
    pub cols: u16,
    pub rows: u16,
    pub width: u16,
    pub height: u16,
}

#[derive(Debug)]
pub enum Frame<T> {
    Message(T),
    Data {
        stream: u64,
        stderr: bool,
        bytes: Vec<u8>,
    },
}

const TAG_MESSAGE: u8 = 0;
const TAG_STDOUT: u8 = 1;
const TAG_STDERR: u8 = 2;

/// A whole frame, compressed when that pays. Fails (`InvalidInput`) if the body is too big
/// for the protocol, so callers can report it instead of losing the connection.
fn encode_frame(tag: u8, prefix: &[u8], body: &[u8]) -> io::Result<Vec<u8>> {
    if 1 + prefix.len() + body.len() > MAX_FRAME {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "The message is too large to send over the connection",
        ));
    }
    let compressed = (body.len() >= COMPRESS_MIN)
        .then(|| lz4_flex::block::compress_prepend_size(body))
        .filter(|compressed| compressed.len() < body.len());
    let (tag, body) = match &compressed {
        Some(compressed) => (tag | COMPRESSED, compressed.as_slice()),
        None => (tag, body),
    };
    let length = 1 + prefix.len() + body.len();
    let mut frame = Vec::with_capacity(4 + length);
    frame.extend_from_slice(&(length as u32).to_be_bytes());
    frame.push(tag);
    frame.extend_from_slice(prefix);
    frame.extend_from_slice(body);
    Ok(frame)
}
pub fn encode_message(value: &impl Serialize) -> io::Result<Vec<u8>> {
    encode_frame(TAG_MESSAGE, &[], &serde_json::to_vec(value)?)
}
pub fn write_message(writer: &mut impl Write, value: &impl Serialize) -> io::Result<()> {
    writer.write_all(&encode_message(value)?)?;
    writer.flush()
}
/// A stream data frame (stdout, or stderr from the server).
pub fn encode_data(stream: u64, stderr: bool, bytes: &[u8]) -> io::Result<Vec<u8>> {
    let tag = if stderr { TAG_STDERR } else { TAG_STDOUT };
    encode_frame(tag, &stream.to_be_bytes(), bytes)
}
pub fn write_data(
    writer: &mut impl Write,
    stream: u64,
    stderr: bool,
    bytes: &[u8],
) -> io::Result<()> {
    writer.write_all(&encode_data(stream, stderr, bytes)?)?;
    writer.flush()
}
fn decompress(body: &[u8]) -> io::Result<Vec<u8>> {
    let size = body
        .get(..4)
        .map(|size| u32::from_le_bytes(size.try_into().expect("4 bytes")) as usize)
        .ok_or_else(|| io::Error::other("Invalid compressed Ion server frame"))?;
    if size > MAX_FRAME {
        return Err(io::Error::other("Invalid compressed Ion server frame"));
    }
    lz4_flex::block::decompress_size_prepended(body).map_err(io::Error::other)
}
pub fn read_frame<T: DeserializeOwned>(reader: &mut impl Read) -> io::Result<Option<Frame<T>>> {
    let mut header = [0; 4];
    match reader.read(&mut header[..1])? {
        0 => return Ok(None),
        _ => reader.read_exact(&mut header[1..])?,
    }
    let length = u32::from_be_bytes(header) as usize;
    if length == 0 || length > MAX_FRAME {
        return Err(io::Error::other("Invalid Ion server frame length"));
    }
    let mut body = vec![0; length];
    reader.read_exact(&mut body)?;
    let compressed = body[0] & COMPRESSED != 0;
    match body[0] & !COMPRESSED {
        TAG_MESSAGE => {
            let json = if compressed {
                decompress(&body[1..])?
            } else {
                body.split_off(1)
            };
            serde_json::from_slice(&json)
                .map(|message| Some(Frame::Message(message)))
                .map_err(io::Error::other)
        }
        tag @ (TAG_STDOUT | TAG_STDERR) => {
            if body.len() < 9 {
                return Err(io::Error::other("Invalid Ion server data frame"));
            }
            let stream = u64::from_be_bytes(body[1..9].try_into().expect("8 bytes"));
            let bytes = if compressed {
                decompress(&body[9..])?
            } else {
                body.split_off(9)
            };
            Ok(Some(Frame::Data {
                stream,
                stderr: tag == TAG_STDERR,
                bytes,
            }))
        }
        _ => Err(io::Error::other(format!(
            "Unknown Ion server frame tag {}",
            body[0]
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn message(bytes: &[u8]) -> ClientMessage {
        match read_frame(&mut &bytes[..]).unwrap().unwrap() {
            Frame::Message(message) => message,
            Frame::Data { .. } => panic!("expected message"),
        }
    }
    #[test]
    fn round_trip_and_reject_truncated_or_oversized_messages() {
        let request = ClientMessage::Request {
            id: 42,
            operation: Operation::SaveText {
                path: "/tmp/a 'é".into(),
                text: "héllo
"
                .into(),
                has_bom: true,
            },
        };
        let mut bytes = Vec::new();
        write_message(&mut bytes, &request).unwrap();
        assert!(matches!(
            message(&bytes),
            ClientMessage::Request { id: 42, .. }
        ));
        assert!(read_frame::<ClientMessage>(&mut &bytes[..bytes.len() - 1]).is_err());
        assert!(read_frame::<ClientMessage>(&mut (u32::MAX.to_be_bytes().as_slice())).is_err());
        assert!(read_frame::<ClientMessage>(&mut &[][..]).unwrap().is_none());
    }
    #[test]
    fn data_frames_round_trip() {
        let mut bytes = Vec::new();
        write_data(&mut bytes, 7, false, b"out").unwrap();
        write_data(&mut bytes, u64::MAX, true, b"").unwrap();
        let mut reader = bytes.as_slice();
        match read_frame::<ClientMessage>(&mut reader).unwrap().unwrap() {
            Frame::Data {
                stream: 7,
                stderr: false,
                bytes,
            } => assert_eq!(bytes, b"out"),
            other => panic!("{other:?}"),
        }
        match read_frame::<ClientMessage>(&mut reader).unwrap().unwrap() {
            Frame::Data {
                stream: u64::MAX,
                stderr: true,
                bytes,
            } => assert!(bytes.is_empty()),
            other => panic!("{other:?}"),
        }
    }
    #[test]
    fn large_bodies_are_compressed_and_round_trip() {
        let files: Vec<String> = (0..2000)
            .map(|n| format!("src/module_{n}/lib.rs"))
            .collect();
        let update = IndexUpdate {
            version: "v".into(),
            added: files.clone(),
            ..IndexUpdate::default()
        };
        let frame = encode_message(&update).unwrap();
        assert_eq!(frame[4], TAG_MESSAGE | COMPRESSED);
        assert!(frame.len() < serde_json::to_vec(&update).unwrap().len() / 3);
        let decoded: IndexUpdate = match read_frame(&mut frame.as_slice()).unwrap().unwrap() {
            Frame::Message(message) => message,
            Frame::Data { .. } => panic!("expected message"),
        };
        assert_eq!(decoded.added, files);

        let output = b"Compiling ion v0.2.0\n".repeat(400);
        let mut bytes = Vec::new();
        write_data(&mut bytes, 3, true, &output).unwrap();
        write_data(&mut bytes, 3, false, b"small").unwrap();
        assert_eq!(bytes[4], TAG_STDERR | COMPRESSED);
        let mut reader = bytes.as_slice();
        match read_frame::<ClientMessage>(&mut reader).unwrap().unwrap() {
            Frame::Data {
                stream: 3,
                stderr: true,
                bytes,
            } => assert_eq!(bytes, output),
            other => panic!("{other:?}"),
        }
        match read_frame::<ClientMessage>(&mut reader).unwrap().unwrap() {
            Frame::Data { bytes, .. } => assert_eq!(bytes, b"small"),
            other => panic!("{other:?}"),
        }
    }
    #[test]
    fn incompressible_bodies_go_as_they_are() {
        // A byte sequence LZ4 can't shrink.
        let mut state = 0x2545_f491_4f6c_dd1du64;
        let noise: Vec<u8> = (0..COMPRESS_MIN * 2)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                state as u8
            })
            .collect();
        let frame = encode_frame(TAG_STDOUT, &1u64.to_be_bytes(), &noise).unwrap();
        assert_eq!(frame[4], TAG_STDOUT);
    }
    #[test]
    fn rejects_corrupt_or_oversized_compressed_bodies() {
        let mut frame = vec![0, 0, 0, 9, TAG_MESSAGE | COMPRESSED];
        frame.extend_from_slice(&u32::MAX.to_le_bytes());
        frame.extend_from_slice(&[0; 4]);
        assert!(read_frame::<ClientMessage>(&mut frame.as_slice()).is_err());
        let frame = [0, 0, 0, 3, TAG_MESSAGE | COMPRESSED, 1, 2];
        assert!(read_frame::<ClientMessage>(&mut frame.as_slice()).is_err());
    }
    #[test]
    fn rejects_unknown_tags_and_short_data() {
        assert!(read_frame::<ClientMessage>(&mut [0, 0, 0, 1, 9].as_slice()).is_err());
        assert!(read_frame::<ClientMessage>(&mut [0, 0, 0, 3, 1, 0, 0].as_slice()).is_err());
    }
    #[test]
    fn refuses_incompatible_servers() {
        assert!(Hello::current().validate().is_ok());
        assert!(
            Hello {
                version: VERSION.into(),
                protocol: PROTOCOL + 1
            }
            .validate()
            .is_err()
        );
    }
}
