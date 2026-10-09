//! Run with the isolated fixture in scripts/test-ssh.py; never uses a real server.
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use futures::StreamExt;
use ion_project as project;
use project::{FileSystem, join_path};
use remote::{AuthPrompt, ConnectError, Connection, ConnectionOptions, ConnectionState, Prompter};

fn port() -> u16 {
    std::env::var("ION_TEST_SSH_PORT")
        .expect("fixture port")
        .parse()
        .unwrap()
}

/// The fixture's project folder as the remote shell spells it.
fn project_dir() -> PathBuf {
    PathBuf::from(std::env::var("ION_TEST_PROJECT").expect("fixture project"))
}

/// The server's pty support is Unix-only; the fixture host is local.
const TERMINALS: bool = cfg!(unix);

fn options(user: &str) -> ConnectionOptions {
    ConnectionOptions {
        host: "127.0.0.1".into(),
        port: Some(port()),
        username: Some(user.into()),
        identity: None,
    }
}

/// How many SSH connections the fixture has accepted, and how many code
/// prompts it has sent.
fn stats() -> (u64, u64) {
    let path = std::env::var("ION_TEST_SSH_STATS").expect("fixture stats");
    let value: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    (
        value["transports"].as_u64().unwrap(),
        value["auth_prompts"].as_u64().unwrap(),
    )
}

/// Answers the fixture's one-time-code question.
struct Codes(AtomicUsize);

impl Prompter for Codes {
    fn ask(&self, prompt: AuthPrompt) -> Option<Vec<String>> {
        assert_eq!(prompt.fields.len(), 1);
        assert!(prompt.fields[0].secret);
        self.0.fetch_add(1, Ordering::SeqCst);
        Some(vec!["123456".into()])
    }
}

fn terminal_size() -> remote::TerminalSize {
    remote::TerminalSize {
        cols: 80,
        rows: 24,
        width: 640,
        height: 456,
    }
}

/// Starts a shell and waits until it's ready; returns it with its events.
fn ready_terminal(
    connection: &Arc<Connection>,
    root: &Path,
) -> (
    remote::Terminal,
    std::sync::mpsc::Receiver<remote::TerminalEvent>,
    Vec<u8>,
) {
    let (tx, rx) = std::sync::mpsc::channel();
    let terminal = remote::Terminal::spawn(
        connection.clone(),
        root.to_path_buf(),
        terminal_size(),
        move |event| {
            let _ = tx.send(event);
            None
        },
    )
    .unwrap();
    terminal.write(b"printf 'ION_TEST_%s\n' TERMINAL_READY\n".to_vec());
    let mut output = Vec::new();
    while !String::from_utf8_lossy(&output).contains("ION_TEST_TERMINAL_READY") {
        match rx.recv_timeout(Duration::from_secs(10)).unwrap() {
            remote::TerminalEvent::Data(bytes) => output.extend(bytes),
            remote::TerminalEvent::Error(error) => panic!("SSH terminal: {error}"),
            _ => panic!("Shell exited before ready"),
        }
    }
    (terminal, rx, output)
}

fn wait_for_state(
    states: &mut futures::channel::mpsc::UnboundedReceiver<ConnectionState>,
    wanted: impl Fn(&ConnectionState) -> bool,
) {
    let start = std::time::Instant::now();
    loop {
        match states.try_recv() {
            Ok(state) if wanted(&state) => return,
            Ok(_) => {}
            Err(error) if error.is_closed() => panic!("connection state stream ended"),
            Err(_) => {
                assert!(
                    start.elapsed() < Duration::from_secs(30),
                    "connection state never arrived"
                );
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    }
}

#[test]
#[ignore = "requires scripts/test-ssh.py's loopback SSH fixture"]
fn ssh_project_round_trip() {
    let password = Some("ion-integration".to_owned());

    // ---- host keys ---------------------------------------------------------
    let key = match Connection::connect(options("dev"), password.clone(), &[], None) {
        Err(ConnectError::UnknownHost {
            fingerprint, key, ..
        }) => {
            assert!(fingerprint.starts_with("SHA256:"));
            key
        }
        result => panic!("must require trust before logging in: {result:?}"),
    };
    assert!(matches!(
        Connection::connect(
            options("dev"),
            password.clone(),
            &[b"wrong-host-key".to_vec()],
            None
        ),
        Err(ConnectError::UnknownHost { .. })
    ));
    // Like ssh, an approved key is recorded even if the login then fails.
    assert!(
        Connection::connect(
            options("dev"),
            Some("wrong-password".into()),
            std::slice::from_ref(&key),
            None
        )
        .is_err()
    );
    let connection = Connection::connect(options("dev"), password.clone(), &[], None).unwrap();
    assert!(!format!("{connection:?}").contains("ion-integration"));
    // Trusting a key records it in known_hosts, like ssh.
    drop(Connection::connect(options("dev"), password.clone(), &[], None).unwrap());

    // ---- server and files --------------------------------------------------
    let installed = connection.ensure_server().unwrap();
    assert!(installed.contains("/.ion/server/"), "{installed}");
    assert_eq!(installed, connection.ensure_server().unwrap());
    let filesystem = FileSystem::Ssh(connection.clone());
    let root = connection.canonicalize(&project_dir()).unwrap();
    assert!(filesystem.is_dir(&root).unwrap());
    assert!(
        filesystem
            .save_text(&join_path(&root, "protected"), false, ["replacement"])
            .is_err()
    );
    assert_eq!(
        filesystem
            .load_text(&join_path(&root, "protected/original.txt"))
            .unwrap()
            .text,
        "original\n"
    );
    assert!(
        filesystem
            .read_dir(&root)
            .unwrap()
            .iter()
            .all(|entry| !entry.name.starts_with(".ion-save-")
                && !entry.path.to_string_lossy().contains('\\'))
    );
    let path = join_path(&root, "src/quote ' and space.rs");
    filesystem.create_dir_all(path.parent().unwrap()).unwrap();

    let (watcher, mut events) = project::watch_remote(connection.clone(), &root).unwrap();
    let (event_tx, event_rx) = std::sync::mpsc::channel();
    let watched_path = path.clone();
    let event_thread = std::thread::spawn(move || {
        futures::executor::block_on(async move {
            while let Some(event) = events.next().await {
                match event {
                    Ok(event) if event.paths.contains(&watched_path) => {
                        event_tx.send(()).unwrap();
                        return;
                    }
                    Err(error) => panic!("Remote watcher: {error}"),
                    _ => {}
                }
            }
        });
    });
    filesystem.create_file(&path).unwrap();
    event_rx.recv_timeout(Duration::from_secs(10)).unwrap();
    drop(watcher);
    event_thread.join().unwrap();

    assert!(filesystem.create_file(&path).is_err());
    filesystem
        .save_text(&path, true, ["// héllo\n", "fn main() {}\n"])
        .unwrap();
    let loaded = filesystem.load_text(&path).unwrap();
    assert!(loaded.has_bom);
    assert_eq!(loaded.text, "// héllo\nfn main() {}\n");
    let copy = filesystem
        .copy_into(&path, path.parent().unwrap(), false)
        .unwrap();
    assert_eq!(copy.file_name().unwrap(), "quote ' and space copy.rs");
    let renamed = join_path(&root, "src/renamed.rs");
    filesystem.rename(&copy, &renamed).unwrap();
    assert!(filesystem.rename(&path, &renamed).is_err());
    assert_eq!(
        filesystem.read_dir(path.parent().unwrap()).unwrap().len(),
        2
    );
    let index = filesystem.build_index(&root, None, &|_| {}).unwrap();
    assert!(
        index
            .files
            .iter()
            .any(|file| file.path.as_ref() == "src/renamed.rs")
    );
    // A rebuild fetches only the changes and applies them to the last list.
    let added = join_path(&root, "src/added.rs");
    filesystem.create_file(&added).unwrap();
    let next = filesystem
        .build_index(&root, Some(&index), &|_| {})
        .unwrap();
    assert_ne!(next.version, index.version);
    assert_eq!(next.files.len(), index.files.len() + 1);
    assert!(
        next.files
            .iter()
            .any(|file| file.path.as_ref() == "src/added.rs")
    );
    let src = join_path(&root, "src");
    assert_eq!(
        filesystem
            .files_among(vec![added.clone(), src, join_path(&root, "gone.rs")])
            .unwrap(),
        std::slice::from_ref(&added)
    );
    filesystem.delete(std::slice::from_ref(&added)).unwrap();
    let results = project::search::search_with_filesystem(
        &filesystem,
        &index,
        "héllo",
        true,
        &AtomicBool::new(false),
    )
    .unwrap();
    assert_eq!(results.files.len(), 2);
    assert!(
        results
            .files
            .iter()
            .all(|file| file.absolute.starts_with(join_path(&root, "src")))
    );

    // ---- Git -----------------------------------------------------------------
    let repo = git::Repository::init_with_connection(&root, Some(connection.clone())).unwrap();
    connection
        .execute(
            &format!(
                "cd {} && git config user.name 'Ion Test' && git config user.email ion-test@example.invalid",
                remote::shell_quote(&project_dir().to_string_lossy())
            ),
            None,
        )
        .unwrap();
    repo.stage_all().unwrap();
    repo.commit("Remote commit with 'quotes' and Unicode é")
        .unwrap();
    assert!(repo.status().unwrap().entries.is_empty());
    assert!(
        repo.head_text("src/renamed.rs")
            .unwrap()
            .unwrap()
            .contains("héllo")
    );
    assert_eq!(repo.absolute("src/renamed.rs"), renamed);

    // Commands keep stdout and stderr apart (Git warnings can't corrupt
    // output), and report the exit status.
    let output = connection.execute("ion-test-stderr", None).unwrap();
    assert_eq!(
        (
            output.stdout.as_slice(),
            output.stderr.as_slice(),
            output.status
        ),
        (&b"out"[..], &b"err"[..], 3)
    );

    // Input reaches the command's stdin and is read to its end.
    let echoed = connection.execute("cat", Some(b"piped input")).unwrap();
    assert_eq!(
        (echoed.stdout.as_slice(), echoed.status),
        (&b"piped input"[..], 0)
    );
    // A search whose cancel flag is already set never starts.
    let cancelled = AtomicBool::new(true);
    assert!(
        project::search::search_with_filesystem(&filesystem, &index, "main", true, &cancelled)
            .map(|results| results.files.is_empty())
            .unwrap_or(true)
    );
    connection.cancel_jobs();

    // ---- one connection for everything ----------------------------------------
    let (before, _) = stats();
    let shells: Vec<_> = (0..3)
        .filter(|_| TERMINALS)
        .map(|_| ready_terminal(&connection, &root))
        .collect();
    repo.status().unwrap();
    filesystem.read_dir(&root).unwrap();
    project::search::search_with_filesystem(
        &filesystem,
        &index,
        "main",
        true,
        &AtomicBool::new(false),
    )
    .unwrap();
    assert_eq!(
        stats().0,
        before,
        "terminals, Git and search must reuse the connection"
    );
    drop(shells);

    // ---- key login and terminals ----------------------------------------------
    let mut key_options = options("dev");
    key_options.identity = Some(PathBuf::from(std::env::var("ION_TEST_SSH_KEY").unwrap()));
    let key_connection = Connection::connect(key_options, None, &[], None).unwrap();
    let key_fs = FileSystem::Ssh(Arc::clone(&key_connection));
    assert_eq!(key_fs.load_text(&renamed).unwrap().text, loaded.text);
    filesystem.delete(&[renamed]).unwrap();
    assert_eq!(
        filesystem.read_dir(path.parent().unwrap()).unwrap().len(),
        1
    );

    if !TERMINALS {
        eprintln!("Skipping terminal assertions: the server's pty support is Unix-only");
    }
    for login in [connection.clone(), key_connection]
        .into_iter()
        .filter(|_| TERMINALS)
    {
        let (terminal, rx, mut output) = ready_terminal(&login, &root);
        terminal.resize(remote::TerminalSize {
            cols: 110,
            rows: 35,
            width: 880,
            height: 665,
        });
        let paste =
            "head -c 3000000 /dev/zero | tr '\\0' x | wc -c\nprintf 'AUTH_REUSED '; pwd\nexit\n";
        terminal.write(paste.as_bytes().to_vec());
        loop {
            match rx.recv_timeout(Duration::from_secs(10)).unwrap() {
                remote::TerminalEvent::Data(bytes) => output.extend(bytes),
                remote::TerminalEvent::Exited => break,
                remote::TerminalEvent::Error(error) => panic!("SSH terminal: {error}"),
                remote::TerminalEvent::Flush => panic!("Unexpected screen flush"),
            }
        }
        let text = String::from_utf8_lossy(&output);
        assert!(
            text.contains(&format!("AUTH_REUSED {}", root.display())),
            "Unexpected terminal output: {text}"
        );
        assert!(text.contains("3000000"));

        // A pending synchronized update gets its one-shot flush.
        let (tx, rx) = std::sync::mpsc::channel();
        let terminal =
            remote::Terminal::spawn(login, root.clone(), terminal_size(), move |event| {
                let deadline = matches!(event, remote::TerminalEvent::Data(_))
                    .then(|| std::time::Instant::now() + Duration::from_millis(150));
                let _ = tx.send(event);
                deadline
            })
            .unwrap();
        assert!(matches!(
            rx.recv_timeout(Duration::from_secs(10)).unwrap(),
            remote::TerminalEvent::Data(_)
        ));
        loop {
            match rx.recv_timeout(Duration::from_secs(10)).unwrap() {
                remote::TerminalEvent::Flush => break,
                remote::TerminalEvent::Data(_) => {}
                _ => panic!("Pending screen update did not flush"),
            }
        }
        // Closing a terminal ends its events.
        drop(terminal);
        loop {
            match rx.recv_timeout(Duration::from_secs(10)) {
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                Ok(remote::TerminalEvent::Data(_)) => {}
                Ok(_) => panic!("Closed terminal kept sending events"),
                Err(_) => panic!("Closed terminal never finished"),
            }
        }
    }

    // ---- one-time codes: asked once per connection ------------------------------
    let codes = Arc::new(Codes(AtomicUsize::new(0)));
    let (_, prompts_before) = stats();
    let mfa = Connection::connect(options("mfa"), None, &[], Some(codes.clone())).unwrap();
    let shells: Vec<_> = (0..2)
        .filter(|_| TERMINALS)
        .map(|_| ready_terminal(&mfa, &root))
        .collect();
    assert_eq!(mfa.execute("printf ok", None).unwrap().stdout, b"ok");
    drop(shells);
    assert_eq!(codes.0.load(Ordering::SeqCst), 1);
    assert_eq!(stats().1, prompts_before + 1);

    // ---- jump hosts from ~/.ssh/config --------------------------------------------
    let key_file = std::env::var("ION_TEST_SSH_KEY")
        .unwrap()
        .replace('\\', "/");
    let ssh_dir = PathBuf::from(std::env::var("ION_SSH_DIR").unwrap());
    std::fs::create_dir_all(&ssh_dir).unwrap();
    let mut config = std::fs::read_to_string(ssh_dir.join("config")).unwrap();
    config.push_str(&format!(
        "\nHost ion-jump\n  HostName 127.0.0.1\n  Port {port}\n  User dev\n  IdentityFile \"{key_file}\"\n\n\
             Host ion-target\n  HostName 127.0.0.1\n  Port {port}\n  User dev\n  IdentityFile \"{key_file}\"\n  ProxyJump ion-jump\n",
        port = port()
    ));
    std::fs::write(ssh_dir.join("config"), config).unwrap();
    let target = ConnectionOptions::new("ion-target").resolve().unwrap();
    assert_eq!((target.user.as_str(), target.port), ("dev", port()));
    let (before, _) = stats();
    let jumped =
        Connection::connect(ConnectionOptions::new("ion-target"), None, &[], None).unwrap();
    assert_eq!(
        stats().0,
        before + 2,
        "one connection to the jump host, one tunnelled"
    );
    assert_eq!(
        jumped.execute("printf jumped", None).unwrap().stdout,
        b"jumped"
    );
    assert!(FileSystem::Ssh(jumped.clone()).is_dir(&root).unwrap());
    drop(jumped);

    // ---- reconnecting after the link drops --------------------------------------------
    let mut states = connection.subscribe();
    let _ = connection.execute("ion-test-drop-connection", None);
    wait_for_state(&mut states, |state| {
        matches!(state, ConnectionState::Reconnecting(_))
    });
    wait_for_state(&mut states, |state| *state == ConnectionState::Connected);
    assert!(
        filesystem.is_dir(&root).unwrap(),
        "requests work again after reconnecting"
    );
    if TERMINALS {
        let (terminal, _rx, _) = ready_terminal(&connection, &root);
        drop(terminal);
    }

    connection.disconnect();
    assert!(matches!(
        connection.state(),
        ConnectionState::Disconnected(_)
    ));
    assert!(filesystem.is_dir(&root).is_err());
}
