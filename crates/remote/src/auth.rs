//! Login answers. `ssh` asks for passwords, key passphrases and one-time
//! codes through its askpass helper (see `askpass.rs`); anything Ion doesn't
//! already hold goes to a [`Prompter`]. Answers are kept in memory for
//! reconnects.

use std::collections::HashMap;
use std::io;

/// One question in a login prompt.
#[derive(Clone, Debug)]
pub struct PromptField {
    pub label: String,
    /// Hide what's typed (passwords, codes).
    pub secret: bool,
}

/// Something the server asked for during login.
#[derive(Clone, Debug)]
pub struct AuthPrompt {
    /// `user@host` being logged in to.
    pub destination: String,
    pub title: String,
    pub instructions: String,
    pub fields: Vec<PromptField>,
}

/// Asks the person for login answers. Called on a background thread; it may
/// block until they answer. `None` means they cancelled.
pub trait Prompter: Send + Sync {
    fn ask(&self, prompt: AuthPrompt) -> Option<Vec<String>>;
}

/// Login answers held for the lifetime of a connection, never written to
/// disk.
#[derive(Default)]
pub(crate) struct Secrets {
    /// The password typed in the connection form: the first answer to a
    /// password or passphrase question.
    pub(crate) password: Option<String>,
    /// Passwords and passphrases by the question `ssh` asked.
    pub(crate) answers: HashMap<String, String>,
}

impl Secrets {
    pub(crate) fn with_password(password: Option<String>) -> Self {
        Self {
            password: password.filter(|password| !password.is_empty()),
            answers: HashMap::new(),
        }
    }
}

pub(crate) fn cancelled() -> io::Error {
    io::Error::new(io::ErrorKind::Interrupted, "Login cancelled")
}
