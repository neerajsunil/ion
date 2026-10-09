//! Login questions from `ssh`. OpenSSH runs its `SSH_ASKPASS` program for
//! every password, passphrase, one-time code and host-key confirmation when
//! it has no terminal. That program is Ion itself ([`askpass_main`]): it
//! relays the question over a loopback socket to the running Ion, which
//! answers from its memory or asks the person.
//!
//! Wire format (one connection per question): the client sends
//! `token\n<prompt>` and closes its write side; the server replies `1\n<answer>`
//! or `0`.

use std::collections::HashSet;
use std::io::{self, Read, Write};
use std::net::{Ipv4Addr, Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::auth::{AuthPrompt, PromptField, Prompter, Secrets};
use crate::lock;

const ENV: &str = "ION_ASKPASS";
const MAX_REQUEST: u64 = 64 * 1024;

/// Entry point for the askpass role. Call it first thing in `main`: if `ssh`
/// started this process to ask a question, it answers and returns the exit
/// code; otherwise it returns `None` and the program carries on.
pub fn askpass_main() -> Option<i32> {
    let spec = std::env::var(ENV).ok()?;
    let prompt = std::env::args().nth(1).unwrap_or_default();
    let Ok(Some(answer)) = ask(&spec, &prompt) else {
        return Some(1);
    };
    let mut out = io::stdout();
    Some(
        if writeln!(out, "{answer}").and_then(|()| out.flush()).is_ok() {
            0
        } else {
            1
        },
    )
}

/// Sends `prompt` to the listener described by `spec` (`port:token`).
/// `Ok(None)` means the person cancelled.
fn ask(spec: &str, prompt: &str) -> io::Result<Option<String>> {
    let (port, token) = spec
        .split_once(':')
        .and_then(|(port, token)| Some((port.parse::<u16>().ok()?, token)))
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "Invalid ION_ASKPASS"))?;
    let mut stream = TcpStream::connect((Ipv4Addr::LOCALHOST, port))?;
    stream.write_all(format!("{token}\n{prompt}").as_bytes())?;
    stream.shutdown(Shutdown::Write)?;
    let mut reply = String::new();
    stream.read_to_string(&mut reply)?;
    Ok(match reply.split_once('\n') {
        Some(("1", answer)) => Some(answer.to_owned()),
        _ => None,
    })
}

/// Reads one question from `stream`, checks the token and replies with
/// `answer`'s result. False if the request was refused.
fn serve(mut stream: TcpStream, token: &str, answer: impl FnOnce(&str) -> Option<String>) -> bool {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let mut request = Vec::new();
    if (&stream)
        .take(MAX_REQUEST)
        .read_to_end(&mut request)
        .is_err()
    {
        return false;
    }
    let request = String::from_utf8_lossy(&request);
    let Some((sent, prompt)) = request.split_once('\n') else {
        return false;
    };
    if sent != token {
        return false;
    }
    let reply = match answer(prompt) {
        Some(answer) => format!("1\n{answer}"),
        None => "0".to_owned(),
    };
    let _ = stream.write_all(reply.as_bytes());
    true
}

/// 32 hex characters nobody could guess: std's per-process random hasher
/// keys, the clock, the process id and a counter, mixed through SHA-256.
pub(crate) fn random_token() -> String {
    use sha2::{Digest, Sha256};
    use std::hash::{BuildHasher, Hasher, RandomState};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos());
    let mut digest = Sha256::new();
    for _ in 0..2 {
        let mut hasher = RandomState::new().build_hasher();
        hasher.write_u128(nanos);
        hasher.write_u32(std::process::id());
        hasher.write_u64(COUNTER.fetch_add(1, Ordering::Relaxed));
        digest.update(hasher.finish().to_le_bytes());
    }
    digest.update(nanos.to_le_bytes());
    digest.finalize()[..16]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[derive(PartialEq, Eq, Debug)]
pub(crate) enum PromptKind {
    HostKey {
        host: String,
        fingerprint: String,
    },
    Password,
    Passphrase,
    /// Keyboard-interactive questions (one-time codes and the like).
    Other,
}

/// Recognizes OpenSSH's wording.
pub(crate) fn classify(prompt: &str) -> PromptKind {
    // OpenSSH's own wording, at the start: a keyboard-interactive prompt that
    // merely contains these words (the server controls its text) isn't one.
    if prompt
        .trim_start()
        .starts_with("The authenticity of host '")
        && prompt.contains("(yes/no")
    {
        let host = prompt
            .split_once("authenticity of host '")
            .and_then(|(_, rest)| rest.split_once('\''))
            .map(|(host, _)| host.split(" (").next().unwrap_or(host).to_owned())
            .unwrap_or_default();
        let fingerprint = prompt
            .find("SHA256:")
            .map(|start| {
                prompt[start..]
                    .split(char::is_whitespace)
                    .next()
                    .unwrap_or_default()
                    .trim_end_matches(['.', ',', ')'])
                    .to_owned()
            })
            .unwrap_or_default();
        return PromptKind::HostKey { host, fingerprint };
    }
    let lower = prompt.trim().to_lowercase();
    if lower.contains("passphrase for key") {
        PromptKind::Passphrase
    } else if lower.ends_with("password:") {
        PromptKind::Password
    } else {
        PromptKind::Other
    }
}

/// The bytes identifying an approved host key: the host and its fingerprint,
/// so a key approved for one host can't vouch for another.
pub(crate) fn trusted_key(host: &str, fingerprint: &str) -> Vec<u8> {
    format!("{host} {fingerprint}").into_bytes()
}

/// What the questions of one connection attempt led to.
#[derive(Clone, Default, Debug)]
pub(crate) struct Outcome {
    /// `(host, fingerprint)` of a host key that wasn't trusted.
    pub(crate) unknown_host: Option<(String, String)>,
    /// The person (or a missing prompter) declined to answer.
    pub(crate) cancelled: bool,
}

/// Answers the questions of one connection attempt.
pub(crate) struct Login {
    pub(crate) label: String,
    /// The password question for the target itself, as `ssh` prints it
    /// (`user@host's password:`): the only one the form's password answers.
    pub(crate) target_prompt: String,
    pub(crate) secrets: Arc<Mutex<Secrets>>,
    /// Fingerprints (`SHA256:…` as bytes) the person approved.
    pub(crate) trusted: Vec<Vec<u8>>,
    pub(crate) prompter: Option<Arc<dyn Prompter>>,
    pub(crate) outcome: Arc<Mutex<Outcome>>,
    /// Passwords and passphrases already answered in this attempt: asked
    /// again, the earlier answer was wrong.
    pub(crate) asked: HashSet<String>,
}

impl Login {
    pub(crate) fn answer(&mut self, prompt: &str) -> Option<String> {
        match classify(prompt) {
            PromptKind::HostKey { host, fingerprint } => {
                if !fingerprint.is_empty()
                    && self
                        .trusted
                        .iter()
                        .any(|trusted| trusted == trusted_key(&host, &fingerprint).as_slice())
                {
                    return Some("yes".into());
                }
                lock(&self.outcome).unknown_host = Some((host, fingerprint));
                Some("no".into())
            }
            kind @ (PromptKind::Password | PromptKind::Passphrase) => {
                let repeated = !self.asked.insert(prompt.to_owned());
                let for_target = prompt.trim().eq_ignore_ascii_case(&self.target_prompt);
                {
                    let mut secrets = lock(&self.secrets);
                    if repeated {
                        secrets.answers.remove(prompt);
                        if for_target {
                            secrets.password = None;
                        }
                    } else if let Some(known) = secrets
                        .answers
                        .get(prompt)
                        .cloned()
                        .or_else(|| secrets.password.clone().filter(|_| for_target))
                    {
                        return Some(known);
                    }
                }
                let title = if kind == PromptKind::Password {
                    "Password"
                } else {
                    "Key passphrase"
                };
                let answer = self.ask(title, prompt)?;
                lock(&self.secrets)
                    .answers
                    .insert(prompt.to_owned(), answer.clone());
                Some(answer)
            }
            PromptKind::Other => self.ask("Verification", prompt),
        }
    }

    fn ask(&mut self, title: &str, prompt: &str) -> Option<String> {
        let answer = self.prompter.as_ref().and_then(|prompter| {
            prompter
                .ask(AuthPrompt {
                    destination: self.label.clone(),
                    title: title.to_owned(),
                    instructions: String::new(),
                    fields: vec![PromptField {
                        label: prompt.trim().to_owned(),
                        secret: true,
                    }],
                })
                .and_then(|answers| answers.into_iter().next())
        });
        if answer.is_none() {
            lock(&self.outcome).cancelled = true;
        }
        answer
    }
}

/// The loopback listener `ssh`'s askpass helper talks to, for the login
/// phase of one connection attempt.
pub(crate) struct AskpassServer {
    address: SocketAddr,
    token: String,
    stop: Arc<AtomicBool>,
    outcome: Arc<Mutex<Outcome>>,
}

impl AskpassServer {
    pub(crate) fn start(mut login: Login) -> io::Result<Self> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        let address = listener.local_addr()?;
        let token = random_token();
        let stop = Arc::new(AtomicBool::new(false));
        let outcome = login.outcome.clone();
        let (thread_token, thread_stop) = (token.clone(), stop.clone());
        std::thread::Builder::new()
            .name("ion-ssh-askpass".into())
            .spawn(move || {
                for stream in listener.incoming() {
                    if thread_stop.load(Ordering::Acquire) {
                        break;
                    }
                    if let Ok(stream) = stream {
                        serve(stream, &thread_token, |prompt| login.answer(prompt));
                    }
                }
            })?;
        Ok(Self {
            address,
            token,
            stop,
            outcome,
        })
    }

    /// The value for the `ION_ASKPASS` variable of the `ssh` process.
    pub(crate) fn spec(&self) -> String {
        format!("{}:{}", self.address.port(), self.token)
    }

    /// Ends the listener (the login phase is over) and reports what happened.
    pub(crate) fn finish(&self) -> Outcome {
        if !self.stop.swap(true, Ordering::AcqRel) {
            // Wake the blocked accept.
            // Without the wake-up the thread would wait for the next
            // connection; try a few times.
            for _ in 0..3 {
                if TcpStream::connect_timeout(&self.address, Duration::from_secs(1)).is_ok() {
                    break;
                }
            }
        }
        lock(&self.outcome).clone()
    }
}

impl Drop for AskpassServer {
    fn drop(&mut self) {
        self.finish();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOST_KEY: &str = "The authenticity of host 'example.com (203.0.113.7)' can't be established.\nED25519 key fingerprint is SHA256:abc+DEF/123.\nThis key is not known by any other names.\nAre you sure you want to continue connecting (yes/no/[fingerprint])? ";

    #[test]
    fn classifies_openssh_prompts() {
        assert_eq!(
            classify(HOST_KEY),
            PromptKind::HostKey {
                host: "example.com".into(),
                fingerprint: "SHA256:abc+DEF/123".into()
            }
        );
        assert_eq!(
            classify("dev@example.com's password: "),
            PromptKind::Password
        );
        assert_eq!(classify("Password:"), PromptKind::Password);
        assert_eq!(
            classify("Enter passphrase for key '/home/dev/.ssh/id_ed25519': "),
            PromptKind::Passphrase
        );
        assert_eq!(classify("Verification code: "), PromptKind::Other);
        // Text from the server that imitates ssh's wording is just a question.
        assert_eq!(
            classify("(dev@h) Are you sure you want to continue connecting (yes/no)? "),
            PromptKind::Other
        );
    }

    struct Scripted(Mutex<Vec<Option<String>>>, Mutex<usize>);
    impl Prompter for Scripted {
        fn ask(&self, _: AuthPrompt) -> Option<Vec<String>> {
            *lock(&self.1) += 1;
            lock(&self.0).remove(0).map(|answer| vec![answer])
        }
    }

    const TARGET_PROMPT: &str = "dev@example.com's password:";

    fn make_login(
        password: Option<&str>,
        trusted: &[&str],
        answers: Vec<Option<String>>,
    ) -> (Login, Arc<Scripted>) {
        let prompter = Arc::new(Scripted(Mutex::new(answers), Mutex::new(0)));
        let login = Login {
            label: "dev@example.com".into(),
            target_prompt: TARGET_PROMPT.into(),
            secrets: Arc::new(Mutex::new(Secrets::with_password(
                password.map(str::to_owned),
            ))),
            trusted: trusted.iter().map(|t| t.as_bytes().to_vec()).collect(),
            prompter: Some(prompter.clone()),
            outcome: Arc::default(),
            asked: HashSet::new(),
        };
        (login, prompter)
    }

    #[test]
    fn host_keys_are_trusted_or_reported() {
        let (mut trusting, _) = make_login(None, &["example.com SHA256:abc+DEF/123"], Vec::new());
        assert_eq!(trusting.answer(HOST_KEY).as_deref(), Some("yes"));
        assert!(lock(&trusting.outcome).unknown_host.is_none());
        // A fingerprint approved for another host doesn't count.
        let (mut other, _) = make_login(None, &["other.com SHA256:abc+DEF/123"], Vec::new());
        assert_eq!(other.answer(HOST_KEY).as_deref(), Some("no"));
        let (mut strict, _) = make_login(None, &[], Vec::new());
        assert_eq!(strict.answer(HOST_KEY).as_deref(), Some("no"));
        assert_eq!(
            lock(&strict.outcome).unknown_host,
            Some(("example.com".into(), "SHA256:abc+DEF/123".into()))
        );
    }

    #[test]
    fn a_repeated_password_question_means_the_cached_answer_was_wrong() {
        let (mut login, prompter) = make_login(Some("stale"), &[], vec![Some("fresh".into())]);
        let question = "dev@example.com's password: ";
        assert_eq!(login.answer(question).as_deref(), Some("stale"));
        assert_eq!(*lock(&prompter.1), 0);
        assert_eq!(login.answer(question).as_deref(), Some("fresh"));
        assert_eq!(*lock(&prompter.1), 1);
        // The next attempt starts with what worked.
        login.asked.clear();
        assert_eq!(login.answer(question).as_deref(), Some("fresh"));
        assert_eq!(*lock(&prompter.1), 1);
    }

    #[test]
    fn the_form_password_only_answers_the_targets_own_prompt() {
        let (mut login, prompter) = make_login(Some("form"), &[], vec![Some("typed".into())]);
        // A keyboard-interactive "Password:" from the server isn't the target's prompt.
        assert_eq!(login.answer("Password: ").as_deref(), Some("typed"));
        assert_eq!(*lock(&prompter.1), 1);
        let (mut login, prompter) = make_login(Some("form"), &[], Vec::new());
        assert_eq!(
            login.answer("dev@example.com's password: ").as_deref(),
            Some("form")
        );
        assert_eq!(*lock(&prompter.1), 0);
        // A passphrase question never gets it.
        let (mut login, _) = make_login(Some("form"), &[], vec![None]);
        assert_eq!(login.answer("Enter passphrase for key '/k': "), None);
    }

    #[test]
    fn codes_are_always_asked_and_cancelling_is_recorded() {
        let (mut login, prompter) = make_login(
            Some("pw"),
            &[],
            vec![Some("111".into()), Some("222".into()), None],
        );
        assert_eq!(login.answer("Verification code: ").as_deref(), Some("111"));
        assert_eq!(login.answer("Verification code: ").as_deref(), Some("222"));
        assert_eq!(*lock(&prompter.1), 2);
        assert!(!lock(&login.outcome).cancelled);
        assert_eq!(login.answer("Verification code: "), None);
        assert!(lock(&login.outcome).cancelled);
        let (mut silent, _) = make_login(None, &[], Vec::new());
        silent.prompter = None;
        assert_eq!(silent.answer("Verification code: "), None);
        assert!(lock(&silent.outcome).cancelled);
    }

    #[test]
    fn the_listener_rejects_a_wrong_token() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let mut served = Vec::new();
            for _ in 0..3 {
                let (stream, _) = listener.accept().unwrap();
                served.push(serve(stream, "secret", |prompt| {
                    Some(format!("re:{}", prompt.trim()))
                }));
            }
            served
        });
        assert_eq!(ask(&format!("{port}:wrong"), "Password: ").unwrap(), None);
        assert_eq!(
            ask(&format!("{port}:secret"), "Password: ")
                .unwrap()
                .as_deref(),
            Some("re:Password:")
        );
        // Prompts may span lines.
        assert_eq!(
            ask(&format!("{port}:secret"), "a\nb").unwrap().as_deref(),
            Some("re:a\nb")
        );
        assert_eq!(server.join().unwrap(), [false, true, true]);
    }

    #[test]
    fn tokens_are_random_hex() {
        let (a, b) = (random_token(), random_token());
        assert_eq!(a.len(), 32);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b);
    }
}
