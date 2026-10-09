//! Tagged frames over SSH stdio: JSON messages plus raw stream data, multiplexed on one
//! connection. No listener or additional network port.
//!
//! Frame: `u32 BE length` (of the rest) + `u8 tag` + payload. Tag 0 is a JSON message, tag 1
//! is stream data (`u64 BE stream` + bytes; stdin to the server, stdout from it), tag 2 is
//! stderr data (server to client).
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::io::{self, Read, Write};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const PROTOCOL: u32 = 2;
pub const MAX_FRAME: usize = 64 * 1024 * 1024;

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
    Index {
        root: String,
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

fn write_tagged(writer: &mut impl Write, tag: u8, prefix: &[u8], body: &[u8]) -> io::Result<()> {
    let length = 1 + prefix.len() + body.len();
    if length > MAX_FRAME {
        return Err(io::Error::other("Ion server message exceeds 64 MiB"));
    }
    let mut frame = Vec::with_capacity(4 + length);
    frame.extend_from_slice(&(length as u32).to_be_bytes());
    frame.push(tag);
    frame.extend_from_slice(prefix);
    frame.extend_from_slice(body);
    writer.write_all(&frame)?;
    writer.flush()
}
pub fn write_message(writer: &mut impl Write, value: &impl Serialize) -> io::Result<()> {
    write_tagged(writer, TAG_MESSAGE, &[], &serde_json::to_vec(value)?)
}
pub fn write_data(
    writer: &mut impl Write,
    stream: u64,
    stderr: bool,
    bytes: &[u8],
) -> io::Result<()> {
    let tag = if stderr { TAG_STDERR } else { TAG_STDOUT };
    write_tagged(writer, tag, &stream.to_be_bytes(), bytes)
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
    match body[0] {
        TAG_MESSAGE => serde_json::from_slice(&body[1..])
            .map(|message| Some(Frame::Message(message)))
            .map_err(io::Error::other),
        tag @ (TAG_STDOUT | TAG_STDERR) => {
            if body.len() < 9 {
                return Err(io::Error::other("Invalid Ion server data frame"));
            }
            let stream = u64::from_be_bytes(body[1..9].try_into().expect("8 bytes"));
            Ok(Some(Frame::Data {
                stream,
                stderr: tag == TAG_STDERR,
                bytes: body.split_off(9),
            }))
        }
        tag => Err(io::Error::other(format!(
            "Unknown Ion server frame tag {tag}"
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
