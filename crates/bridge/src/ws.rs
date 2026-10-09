//! The small part of WebSocket (RFC 6455) a local MCP server needs: the
//! opening handshake and unfragmented-or-fragmented text frames. Blocking
//! I/O; each connection's reader is one thread parked on its socket.

use std::io::{self, BufRead, Read, Write};

use base64::Engine;

const ACCEPT_GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";
/// Requests and responses larger than this end the connection.
pub(crate) const MAX_MESSAGE: usize = 256 * 1024 * 1024;
/// The opening request is a few headers; anything longer isn't a client.
const MAX_HEADERS: usize = 16 * 1024;

const OP_CONTINUATION: u8 = 0;
const OP_TEXT: u8 = 1;
const OP_BINARY: u8 = 2;
const OP_CLOSE: u8 = 8;
const OP_PING: u8 = 9;
const OP_PONG: u8 = 10;

/// The parts of the client's opening request the server checks.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Handshake {
    pub key: String,
    pub auth: Option<String>,
    pub protocol: Option<String>,
}

/// Reads the HTTP upgrade request.
pub(crate) fn read_handshake(reader: &mut impl BufRead) -> io::Result<Handshake> {
    let mut handshake = Handshake::default();
    let mut total = 0;
    let mut line = String::new();
    loop {
        line.clear();
        let read = reader.read_line(&mut line)?;
        total += read;
        if read == 0 || total > MAX_HEADERS {
            return Err(invalid("bad upgrade request"));
        }
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim().to_owned();
        match name.trim().to_ascii_lowercase().as_str() {
            "sec-websocket-key" => handshake.key = value,
            "x-claude-code-ide-authorization" => handshake.auth = Some(value),
            "sec-websocket-protocol" => {
                handshake.protocol = value.split(',').next().map(|p| p.trim().to_owned())
            }
            _ => {}
        }
    }
    if handshake.key.is_empty() {
        return Err(invalid("not a WebSocket request"));
    }
    Ok(handshake)
}

pub(crate) fn accept_key(key: &str) -> String {
    let mut sha = sha1_smol::Sha1::new();
    sha.update(key.as_bytes());
    sha.update(ACCEPT_GUID.as_bytes());
    base64::engine::general_purpose::STANDARD.encode(sha.digest().bytes())
}

pub(crate) fn write_accept(stream: &mut impl Write, handshake: &Handshake) -> io::Result<()> {
    let protocol = handshake
        .protocol
        .as_ref()
        .map(|protocol| format!("Sec-WebSocket-Protocol: {protocol}\r\n"))
        .unwrap_or_default();
    write!(
        stream,
        "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\
         Sec-WebSocket-Accept: {}\r\n{protocol}\r\n",
        accept_key(&handshake.key)
    )?;
    stream.flush()
}

pub(crate) fn write_unauthorized(stream: &mut impl Write) -> io::Result<()> {
    stream.write_all(b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\n\r\n")?;
    stream.flush()
}

/// What the reader got from the client.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Incoming {
    Text(String),
    /// Needs a pong with the same payload.
    Ping(Vec<u8>),
    Close,
}

/// Reads frames until a whole message (or a control frame) arrives.
pub(crate) fn read_message(reader: &mut impl Read) -> io::Result<Incoming> {
    let mut message = Vec::new();
    loop {
        let mut head = [0u8; 2];
        reader.read_exact(&mut head)?;
        let fin = head[0] & 0x80 != 0;
        let opcode = head[0] & 0x0f;
        let masked = head[1] & 0x80 != 0;
        let len = match head[1] & 0x7f {
            126 => {
                let mut len = [0u8; 2];
                reader.read_exact(&mut len)?;
                u16::from_be_bytes(len) as u64
            }
            127 => {
                let mut len = [0u8; 8];
                reader.read_exact(&mut len)?;
                u64::from_be_bytes(len)
            }
            len => len as u64,
        };
        if len as usize > MAX_MESSAGE || message.len() + len as usize > MAX_MESSAGE {
            return Err(invalid("message too large"));
        }
        let mut mask = [0u8; 4];
        if masked {
            reader.read_exact(&mut mask)?;
        }
        let mut payload = vec![0u8; len as usize];
        reader.read_exact(&mut payload)?;
        if masked {
            for (ix, byte) in payload.iter_mut().enumerate() {
                *byte ^= mask[ix % 4];
            }
        }
        match opcode {
            OP_CLOSE => return Ok(Incoming::Close),
            OP_PING => return Ok(Incoming::Ping(payload)),
            OP_PONG => continue,
            OP_TEXT | OP_BINARY | OP_CONTINUATION => {
                message.extend_from_slice(&payload);
                if fin {
                    return String::from_utf8(message)
                        .map(Incoming::Text)
                        .map_err(|_| invalid("message isn't UTF-8"));
                }
            }
            _ => return Err(invalid("unknown frame")),
        }
    }
}

/// One unmasked frame from the server.
pub(crate) fn frame(opcode: u8, payload: &[u8]) -> Vec<u8> {
    let mut frame = Vec::with_capacity(payload.len() + 10);
    frame.push(0x80 | opcode);
    match payload.len() {
        len @ 0..=125 => frame.push(len as u8),
        len @ 126..=0xffff => {
            frame.push(126);
            frame.extend_from_slice(&(len as u16).to_be_bytes());
        }
        len => {
            frame.push(127);
            frame.extend_from_slice(&(len as u64).to_be_bytes());
        }
    }
    frame.extend_from_slice(payload);
    frame
}

pub(crate) fn text_frame(text: &str) -> Vec<u8> {
    frame(OP_TEXT, text.as_bytes())
}

pub(crate) fn pong_frame(payload: &[u8]) -> Vec<u8> {
    frame(OP_PONG, payload)
}

pub(crate) fn close_frame() -> Vec<u8> {
    frame(OP_CLOSE, &[])
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn computes_the_rfc_example_accept_key() {
        assert_eq!(
            accept_key("dGhlIHNhbXBsZSBub25jZQ=="),
            "s3pPLMBiTxaQ9kYGzzhZRbK+xOo="
        );
    }

    #[test]
    fn reads_the_upgrade_request() {
        let request = "GET / HTTP/1.1\r\nHost: 127.0.0.1\r\nUpgrade: websocket\r\n\
                       Sec-WebSocket-Key: abc==\r\nX-Claude-Code-Ide-Authorization: tok\r\n\
                       Sec-WebSocket-Protocol: mcp\r\n\r\n";
        let handshake = read_handshake(&mut request.as_bytes()).unwrap();
        assert_eq!(handshake.key, "abc==");
        assert_eq!(handshake.auth.as_deref(), Some("tok"));
        assert_eq!(handshake.protocol.as_deref(), Some("mcp"));
        assert!(read_handshake(&mut "GET / HTTP/1.1\r\n\r\n".as_bytes()).is_err());
    }

    /// A client frame: masked, as browsers and Node send them.
    fn client_frame(opcode: u8, fin: bool, payload: &[u8]) -> Vec<u8> {
        let mut unmasked = frame(opcode, payload);
        if !fin {
            unmasked[0] &= 0x7f;
        }
        let header_len = unmasked.len() - payload.len();
        let mask = [1u8, 2, 3, 4];
        let mut out = unmasked[..header_len].to_vec();
        out[1] |= 0x80;
        out.extend_from_slice(&mask);
        out.extend(payload.iter().enumerate().map(|(ix, b)| b ^ mask[ix % 4]));
        out
    }

    #[test]
    fn reads_masked_and_fragmented_messages() {
        let long = "x".repeat(70_000);
        let mut bytes = client_frame(OP_TEXT, true, b"hello");
        bytes.extend(client_frame(OP_PING, true, b"p"));
        bytes.extend(client_frame(OP_TEXT, false, b"he"));
        bytes.extend(client_frame(OP_CONTINUATION, true, b"llo"));
        bytes.extend(client_frame(OP_TEXT, true, long.as_bytes()));
        bytes.extend(client_frame(OP_CLOSE, true, b""));
        let mut reader = bytes.as_slice();
        assert_eq!(
            read_message(&mut reader).unwrap(),
            Incoming::Text("hello".into())
        );
        assert_eq!(
            read_message(&mut reader).unwrap(),
            Incoming::Ping(b"p".to_vec())
        );
        assert_eq!(
            read_message(&mut reader).unwrap(),
            Incoming::Text("hello".into())
        );
        assert_eq!(read_message(&mut reader).unwrap(), Incoming::Text(long));
        assert_eq!(read_message(&mut reader).unwrap(), Incoming::Close);
    }
}
