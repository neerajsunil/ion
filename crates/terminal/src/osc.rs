//! Finds the notification and progress escape codes that `alacritty_terminal`
//! ignores, in raw terminal output:
//!
//! - `OSC 9 ; body` (iTerm2), used by Codex.
//! - `OSC 777 ; notify ; title ; body` (rxvt, Ghostty), used by Claude Code.
//! - `OSC 99 ; metadata ; payload` (kitty), used by Claude Code.
//! - `OSC 9 ; 4 ; state ; percent` (ConEmu, Windows Terminal): progress.
//!
//! The scanner sees the bytes before alacritty parses them. It skips ahead to
//! each ESC, so ordinary output costs one byte search.

use base64::Engine;

/// Longest OSC kept; longer ones (e.g. OSC 52 clipboard data) are skipped.
const MAX_OSC: usize = 4096;
/// Longest title or body shown to the user, in characters.
const MAX_TEXT: usize = 256;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Signal {
    /// A program asked for a desktop notification.
    Notify { title: Option<String>, body: String },
    /// A program reported progress; `None` clears it.
    Progress(Option<Progress>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Progress {
    pub state: ProgressState,
    /// 0 to 100, when known.
    pub percent: Option<u8>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProgressState {
    Normal,
    Error,
    Indeterminate,
    Paused,
}

#[derive(Default)]
enum State {
    #[default]
    Ground,
    /// Saw ESC.
    Escape,
    /// Inside `ESC ]`.
    Osc,
    /// Saw ESC inside an OSC: `\` ends it.
    OscEscape,
    /// Inside an OSC that is too long to keep.
    Skip,
    /// Saw ESC inside a skipped OSC.
    SkipEscape,
}

/// A kitty notification sent in several chunks.
#[derive(Default)]
struct KittyDraft {
    id: String,
    title: String,
    body: String,
}

#[derive(Default)]
pub(crate) struct OscScanner {
    state: State,
    buf: Vec<u8>,
    kitty: Option<KittyDraft>,
}

impl OscScanner {
    pub fn scan(&mut self, mut bytes: &[u8], emit: &mut impl FnMut(Signal)) {
        while !bytes.is_empty() {
            match self.state {
                State::Ground => match bytes.iter().position(|&b| b == 0x1b) {
                    Some(ix) => {
                        self.state = State::Escape;
                        bytes = &bytes[ix + 1..];
                    }
                    None => return,
                },
                State::Escape => {
                    if bytes[0] == b']' {
                        self.buf.clear();
                        self.state = State::Osc;
                        bytes = &bytes[1..];
                    } else {
                        // Re-read the byte: it may be another ESC.
                        self.state = State::Ground;
                    }
                }
                State::Osc => {
                    let end = bytes
                        .iter()
                        .position(|&b| matches!(b, 0x07 | 0x1b | 0x18 | 0x1a));
                    let content = &bytes[..end.unwrap_or(bytes.len())];
                    if self.buf.len() + content.len() > MAX_OSC {
                        self.buf.clear();
                        self.state = State::Skip;
                        continue;
                    }
                    self.buf.extend_from_slice(content);
                    let Some(end) = end else { return };
                    match bytes[end] {
                        0x07 => {
                            self.dispatch(emit);
                            self.state = State::Ground;
                        }
                        0x1b => self.state = State::OscEscape,
                        // CAN and SUB cancel the sequence.
                        _ => self.state = State::Ground,
                    }
                    bytes = &bytes[end + 1..];
                }
                State::OscEscape => {
                    if bytes[0] == b'\\' {
                        self.dispatch(emit);
                        self.state = State::Ground;
                        bytes = &bytes[1..];
                    } else {
                        // Any other escape aborts the OSC and starts anew.
                        self.state = State::Escape;
                    }
                }
                State::Skip => {
                    match bytes
                        .iter()
                        .position(|&b| matches!(b, 0x07 | 0x1b | 0x18 | 0x1a))
                    {
                        Some(end) => {
                            self.state = if bytes[end] == 0x1b {
                                State::SkipEscape
                            } else {
                                State::Ground
                            };
                            bytes = &bytes[end + 1..];
                        }
                        None => return,
                    }
                }
                State::SkipEscape => {
                    if bytes[0] == b'\\' {
                        self.state = State::Ground;
                        bytes = &bytes[1..];
                    } else {
                        self.state = State::Escape;
                    }
                }
            }
        }
    }

    fn dispatch(&mut self, emit: &mut impl FnMut(Signal)) {
        let osc = std::mem::take(&mut self.buf);
        let (code, rest) = split(&osc);
        match code {
            b"9" => match conemu(rest) {
                Some((b"4", args)) => emit(Signal::Progress(progress(args))),
                // Other ConEmu commands (cwd, tab title, ...) aren't notifications.
                Some(_) => {}
                None => {
                    let body = text(rest);
                    if !body.is_empty() {
                        emit(Signal::Notify { title: None, body });
                    }
                }
            },
            b"777" => {
                let (kind, rest) = split(rest);
                if kind == b"notify" {
                    let (title, body) = split(rest);
                    notify(text(title), text(body), emit);
                }
            }
            b"99" => self.kitty(rest, emit),
            _ => {}
        }
        // Keep the allocation for the next sequence.
        self.buf = osc;
    }

    /// `OSC 99 ; key=value:key=value ; payload`. Chunks with `d=0` continue
    /// in the next sequence with the same `i`.
    fn kitty(&mut self, rest: &[u8], emit: &mut impl FnMut(Signal)) {
        let (metadata, payload) = split(rest);
        let mut id = "";
        let mut done = true;
        let mut kind: &[u8] = b"title";
        let mut base64 = false;
        for pair in metadata.split(|&b| b == b':') {
            let (key, value) = match pair.iter().position(|&b| b == b'=') {
                Some(ix) => (&pair[..ix], &pair[ix + 1..]),
                None => continue,
            };
            match key {
                b"i" => id = std::str::from_utf8(value).unwrap_or(""),
                b"d" => done = value != b"0",
                b"p" => kind = value,
                b"e" => base64 = value == b"1",
                _ => {}
            }
        }
        if !matches!(kind, b"title" | b"body") {
            // Queries, icons, buttons and close requests.
            return;
        }
        let decoded;
        let payload = if base64 {
            match base64::engine::general_purpose::STANDARD.decode(payload) {
                Ok(bytes) => {
                    decoded = bytes;
                    &decoded[..]
                }
                Err(_) => return,
            }
        } else {
            payload
        };
        let draft = match &mut self.kitty {
            Some(draft) if draft.id == id => draft,
            draft => draft.insert(KittyDraft {
                id: id.to_owned(),
                ..KittyDraft::default()
            }),
        };
        let target = if kind == b"title" {
            &mut draft.title
        } else {
            &mut draft.body
        };
        if target.len() < MAX_OSC {
            target.push_str(&String::from_utf8_lossy(payload));
        }
        if done && let Some(draft) = self.kitty.take() {
            notify(clean(&draft.title), clean(&draft.body), emit);
        }
    }
}

/// Emits a notification, showing the title alone when there is no body.
fn notify(title: String, body: String, emit: &mut impl FnMut(Signal)) {
    match (title.is_empty(), body.is_empty()) {
        (true, true) => {}
        (false, true) => emit(Signal::Notify {
            title: None,
            body: title,
        }),
        (true, false) => emit(Signal::Notify { title: None, body }),
        (false, false) => emit(Signal::Notify {
            title: Some(title),
            body,
        }),
    }
}

/// Splits at the first `;`.
fn split(bytes: &[u8]) -> (&[u8], &[u8]) {
    match bytes.iter().position(|&b| b == b';') {
        Some(ix) => (&bytes[..ix], &bytes[ix + 1..]),
        None => (bytes, &[]),
    }
}

/// ConEmu's `OSC 9 ; <number> ; ...` commands, told apart from iTerm2's
/// `OSC 9 ; <message>` the same way Windows Terminal and Ghostty do.
fn conemu(rest: &[u8]) -> Option<(&[u8], &[u8])> {
    let (command, args) = split(rest);
    let numeric = !command.is_empty() && command.iter().all(u8::is_ascii_digit);
    let whole = command.len() == rest.len() || rest.get(command.len()) == Some(&b';');
    (numeric && whole).then_some((command, args))
}

/// `state ; percent` of `OSC 9 ; 4`.
fn progress(args: &[u8]) -> Option<Progress> {
    let (state, percent) = split(args);
    let number = |bytes: &[u8]| std::str::from_utf8(bytes).ok()?.parse::<u32>().ok();
    let percent = number(percent).map(|percent| percent.min(100) as u8);
    let state = match number(state).unwrap_or(0) {
        1 => ProgressState::Normal,
        2 => ProgressState::Error,
        3 => ProgressState::Indeterminate,
        4 => ProgressState::Paused,
        _ => return None,
    };
    let percent = match state {
        ProgressState::Indeterminate => None,
        _ => percent,
    };
    Some(Progress { state, percent })
}

fn text(bytes: &[u8]) -> String {
    clean(&String::from_utf8_lossy(bytes))
}

/// Drops control characters and trims to a displayable length.
fn clean(text: &str) -> String {
    let text: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(MAX_TEXT)
        .collect();
    text.trim().to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scan(chunks: &[&[u8]]) -> Vec<Signal> {
        let mut scanner = OscScanner::default();
        let mut signals = Vec::new();
        for chunk in chunks {
            scanner.scan(chunk, &mut |signal| signals.push(signal));
        }
        signals
    }

    fn body(body: &str) -> Signal {
        Signal::Notify {
            title: None,
            body: body.to_owned(),
        }
    }

    #[test]
    fn iterm2_notifications_with_either_terminator() {
        assert_eq!(scan(&[b"a\x1b]9;Done\x07b"]), [body("Done")]);
        assert_eq!(scan(&[b"\x1b]9;Needs input\x1b\\"]), [body("Needs input")]);
    }

    #[test]
    fn sequences_split_across_reads() {
        let chunks: [&[u8]; 5] = [b"out\x1b", b"]9", b";Hel", b"lo\x1b", b"\\more"];
        assert_eq!(scan(&chunks), [body("Hello")]);
    }

    #[test]
    fn rxvt_notifications() {
        assert_eq!(
            scan(&[b"\x1b]777;notify;Claude Code;Waiting; for you\x07"]),
            [Signal::Notify {
                title: Some("Claude Code".to_owned()),
                body: "Waiting; for you".to_owned(),
            }]
        );
        assert_eq!(scan(&[b"\x1b]777;other;x;y\x07"]), []);
    }

    #[test]
    fn kitty_notifications_in_chunks() {
        let signals = scan(&[
            b"\x1b]99;i=1:d=0;Claude Code\x1b\\",
            b"\x1b]99;i=1:d=0:p=body;Task \x1b\\",
            b"\x1b]99;i=1:p=body;done\x1b\\",
        ]);
        assert_eq!(
            signals,
            [Signal::Notify {
                title: Some("Claude Code".to_owned()),
                body: "Task done".to_owned(),
            }]
        );
        assert_eq!(scan(&[b"\x1b]99;e=1;SGVsbG8=\x07"]), [body("Hello")]);
        assert_eq!(scan(&[b"\x1b]99;i=1:p=?;\x07"]), []);
    }

    #[test]
    fn conemu_progress_is_not_a_notification() {
        let progress = |state, percent| {
            Signal::Progress(Some(Progress {
                state,
                percent: Some(percent),
            }))
        };
        assert_eq!(
            scan(&[b"\x1b]9;4;1;42\x07"]),
            [progress(ProgressState::Normal, 42)]
        );
        assert_eq!(
            scan(&[b"\x1b]9;4;2;250\x07"]),
            [progress(ProgressState::Error, 100)]
        );
        assert_eq!(
            scan(&[b"\x1b]9;4;3\x07"]),
            [Signal::Progress(Some(Progress {
                state: ProgressState::Indeterminate,
                percent: None,
            }))]
        );
        assert_eq!(scan(&[b"\x1b]9;4;0;0\x07"]), [Signal::Progress(None)]);
        // The current directory, not a message.
        assert_eq!(scan(&[b"\x1b]9;9;C:\\src\x07"]), []);
        // Starts with a digit but isn't a ConEmu command.
        assert_eq!(
            scan(&[b"\x1b]9;3 tests failed\x07"]),
            [body("3 tests failed")]
        );
    }

    #[test]
    fn ignores_other_and_broken_sequences() {
        assert_eq!(
            scan(&[b"\x1b]0;title\x07\x1b[31mred\x1b]8;;http://x\x1b\\"]),
            []
        );
        // An escape inside the OSC aborts it; the next one still counts.
        assert_eq!(scan(&[b"\x1b]9;lost\x1b[m\x1b]9;kept\x07"]), [body("kept")]);
        // Cancelled by CAN.
        assert_eq!(scan(&[b"\x1b]9;no\x18\x07"]), []);
        // Control characters can't reach the UI.
        assert_eq!(scan(&[b"\x1b]9;a\tb\x07"]), [body("a b")]);
    }

    #[test]
    fn skips_oversized_sequences() {
        let mut long = b"\x1b]52;c;".to_vec();
        long.extend(std::iter::repeat_n(b'A', MAX_OSC * 2));
        long.extend_from_slice(b"\x07\x1b]9;after\x07");
        assert_eq!(scan(&[&long[..MAX_OSC], &long[MAX_OSC..]]), [body("after")]);
    }
}
