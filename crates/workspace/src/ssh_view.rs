//! The SSH dialog: connect to a host, answer login questions, trust a new
//! host key, and pick the remote folder to open.

use std::path::{Path, PathBuf};
use std::sync::{Arc, mpsc};

use editor::{Editor, EditorEvent};
use futures::StreamExt;
use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use gpui::{
    AppContext, ClickEvent, Context, Div, Entity, EventEmitter, Focusable, FontWeight,
    InteractiveElement, IntoElement, ParentElement, PathPromptOptions, Render, ScrollStrategy,
    SharedString, Stateful, StatefulInteractiveElement, Styled, Subscription, Task,
    UniformListScrollHandle, Window, actions, div, prelude::FluentBuilder, px, uniform_list,
};
use project::FileSystem;
use remote::{AuthPrompt, ConnectError, Connection, ConnectionOptions, Prompter};
use ui::IconName;

use crate::remote_paths::Query;
use crate::session::{self, RemoteProject};

actions!(ssh_connection, [Dismiss, Confirm]);

pub(crate) fn key_bindings() -> Vec<gpui::KeyBinding> {
    vec![
        gpui::KeyBinding::new("escape", Dismiss, Some("SshConnection")),
        gpui::KeyBinding::new("enter", Confirm, Some("SshConnection")),
    ]
}

pub(crate) enum SshEvent {
    /// Connected and a folder was chosen.
    Connected(Arc<Connection>, PathBuf),
    Dismissed,
}

/// A login question waiting for the person's answer.
pub(crate) struct PromptRequest {
    pub prompt: AuthPrompt,
    pub answer: mpsc::Sender<Option<Vec<String>>>,
}

/// Sends login questions from the connection's background thread to a
/// window, and waits for the answer.
pub(crate) struct UiPrompter(UnboundedSender<PromptRequest>);

impl Prompter for UiPrompter {
    fn ask(&self, prompt: AuthPrompt) -> Option<Vec<String>> {
        let (answer, receive) = mpsc::channel();
        self.0
            .unbounded_send(PromptRequest { prompt, answer })
            .ok()?;
        receive.recv().ok().flatten()
    }
}

pub(crate) fn prompter() -> (Arc<dyn Prompter>, UnboundedReceiver<PromptRequest>) {
    let (sender, receiver) = unbounded();
    (Arc::new(UiPrompter(sender)), receiver)
}

/// A host offered in the connection form.
#[derive(Clone)]
struct HostChoice {
    title: String,
    detail: String,
    options: ConnectionOptions,
    recent: bool,
}

#[derive(Clone)]
struct Completion {
    path: String,
    recent: bool,
}

enum Step {
    Connect,
    Connecting,
    /// A host isn't in known_hosts: show its fingerprint first.
    Trust {
        host: String,
        fingerprint: String,
        key: Vec<u8>,
    },
    Folder {
        connection: Arc<Connection>,
        home: PathBuf,
    },
    /// Only answering a login question for an open remote window.
    PromptOnly,
}

/// A login question shown over the current step.
struct Auth {
    prompt: AuthPrompt,
    fields: Vec<Entity<Editor>>,
    answer: Option<mpsc::Sender<Option<Vec<String>>>>,
}

pub(crate) struct SshView {
    focus_handle: gpui::FocusHandle,
    host: Entity<Editor>,
    port: Entity<Editor>,
    username: Entity<Editor>,
    identity: Entity<Editor>,
    password: Entity<Editor>,
    folder: Entity<Editor>,
    step: Step,
    auth: Option<Auth>,
    /// Shown under the form: what went wrong, or progress.
    message: String,
    /// Where the typed host will actually connect.
    resolved: String,
    show_options: bool,
    config: remote::Config,
    hosts: Vec<HostChoice>,
    host_selected: Option<usize>,
    trusted: Vec<Vec<u8>>,
    prompter: Arc<dyn Prompter>,
    /// Reopening a recent project: go straight to this folder.
    reopen: Option<PathBuf>,
    /// Opened from a remote window to pick another folder on its host.
    existing: bool,
    opening: bool,
    completions: Vec<Completion>,
    completion_selected: usize,
    completion_error: Option<String>,
    completion_cache: Vec<(PathBuf, Vec<project::DirEntry>)>,
    completion_task: Option<Task<()>>,
    completion_scroll: UniformListScrollHandle,
    task: Option<Task<()>>,
    _prompts: Task<()>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<SshEvent> for SshView {}

fn input(initial: &str, placeholder: &str, cx: &mut Context<SshView>) -> Entity<Editor> {
    cx.new(|cx| {
        let mut input = Editor::single_line(placeholder.to_owned(), cx);
        input.set_text(initial, cx);
        let end = initial.chars().count();
        input.select_range((0, end), (0, end), cx);
        input
    })
}

impl SshView {
    fn base(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let host = input("", "host, user@host or SSH config name", cx);
        let username = input("", "", cx);
        let port = input("", "22", cx);
        let identity = input("", "Default keys and SSH agent", cx);
        let password = cx.new(Editor::password);
        let folder = input("", "/home/you/project", cx);
        let mut subscriptions = Vec::new();
        for field in [&host, &username, &port, &identity] {
            subscriptions.push(cx.subscribe(field, |this, _, event, cx| {
                if let EditorEvent::Edited = event {
                    this.host_selected = None;
                    this.update_resolved(cx);
                }
            }));
        }
        subscriptions.push(cx.subscribe(&folder, |this, _, event, cx| {
            if let EditorEvent::Edited = event {
                this.complete_folder(cx);
            }
        }));
        let (prompter, mut requests) = prompter();
        let prompts = cx.spawn_in(window, async move |this, cx| {
            while let Some(request) = requests.next().await {
                if this
                    .update_in(cx, |this, window, cx| this.show_auth(request, window, cx))
                    .is_err()
                {
                    break;
                }
            }
        });
        window.focus(&host.focus_handle(cx));
        let config = remote::Config::load();
        let mut view = Self {
            focus_handle: cx.focus_handle(),
            host,
            port,
            username,
            identity,
            password,
            folder,
            step: Step::Connect,
            auth: None,
            message: String::new(),
            resolved: String::new(),
            show_options: false,
            hosts: Vec::new(),
            config,
            host_selected: None,
            trusted: Vec::new(),
            prompter,
            reopen: None,
            existing: false,
            opening: false,
            completions: Vec::new(),
            completion_selected: 0,
            completion_error: None,
            completion_cache: Vec::new(),
            completion_task: None,
            completion_scroll: UniformListScrollHandle::new(),
            task: None,
            _prompts: prompts,
            _subscriptions: subscriptions,
        };
        view.hosts = view.host_choices(cx);
        view.update_resolved(cx);
        view
    }

    /// The connection form.
    pub(crate) fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self::base(window, cx)
    }

    /// Reconnects to a recent project and opens its folder.
    pub(crate) fn reopen(
        project: RemoteProject,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut view = Self::base(window, cx);
        view.fill(&project.connection, cx);
        view.reopen = Some(project.folder);
        view.submit(window, cx);
        view
    }

    /// Picks another folder on the host an open remote window uses.
    pub(crate) fn pick_folder(
        connection: Arc<Connection>,
        current: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut view = Self::base(window, cx);
        view.existing = true;
        view.step = Step::Connecting;
        view.message = "Loading folders…".into();
        let home_connection = connection.clone();
        view.task = Some(cx.spawn_in(window, async move |this, cx| {
            let home = cx
                .background_spawn(async move { home_connection.canonicalize(Path::new(".")) })
                .await;
            this.update_in(cx, |this, window, cx| match home {
                Ok(home) => this.show_folder_step(connection, home, current, window, cx),
                Err(error) => {
                    this.message = format!("Couldn't reach the server: {error}");
                    cx.notify();
                }
            })
            .ok();
        }));
        view
    }

    /// Answers one login question for an open remote window.
    pub(crate) fn prompt_only(
        request: PromptRequest,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut view = Self::base(window, cx);
        view.step = Step::PromptOnly;
        view.show_auth(request, window, cx);
        view
    }

    /// Recent connections first, then hosts from `~/.ssh/config`.
    fn host_choices(&self, cx: &mut Context<Self>) -> Vec<HostChoice> {
        let mut choices: Vec<HostChoice> = Vec::new();
        for project in &session::get(cx).recent_remote {
            if choices
                .iter()
                .any(|choice| choice.options == project.connection)
            {
                continue;
            }
            let detail = project
                .connection
                .resolve_with(&self.config)
                .map(|target| target.label())
                .unwrap_or_default();
            choices.push(HostChoice {
                title: project.connection.host.clone(),
                detail,
                options: project.connection.clone(),
                recent: true,
            });
        }
        for host in self.config.hosts() {
            if choices.iter().any(|choice| choice.options.host == *host) {
                continue;
            }
            let options = ConnectionOptions::new(host.clone());
            let detail = options
                .resolve_with(&self.config)
                .map(|target| target.label())
                .unwrap_or_default();
            choices.push(HostChoice {
                title: host.clone(),
                detail,
                options,
                recent: false,
            });
        }
        choices
    }

    fn matching_hosts(&self, cx: &gpui::App) -> Vec<usize> {
        let query = self.host.read(cx).text().trim().to_lowercase();
        self.hosts
            .iter()
            .enumerate()
            .filter(|(_, choice)| {
                query.is_empty()
                    || choice.title.to_lowercase().contains(&query)
                    || choice.detail.to_lowercase().contains(&query)
            })
            .map(|(ix, _)| ix)
            .take(6)
            .collect()
    }

    fn fill(&mut self, options: &ConnectionOptions, cx: &mut Context<Self>) {
        let set = |field: &Entity<Editor>, text: &str, cx: &mut Context<Self>| {
            field.update(cx, |field, cx| {
                field.set_text(text, cx);
                let end = text.chars().count();
                field.select_range((0, end), (0, end), cx);
            })
        };
        set(&self.host, &options.host, cx);
        set(
            &self.username,
            options.username.as_deref().unwrap_or(""),
            cx,
        );
        set(
            &self.port,
            &options
                .port
                .map(|port| port.to_string())
                .unwrap_or_default(),
            cx,
        );
        let identity = options
            .identity
            .as_ref()
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_default();
        set(&self.identity, &identity, cx);
        self.show_options |= options.port.is_some() || options.identity.is_some();
        self.update_resolved(cx);
    }

    fn options(&self, cx: &gpui::App) -> Result<ConnectionOptions, String> {
        let text = |field: &Entity<Editor>| field.read(cx).text().trim().to_owned();
        let port = text(&self.port);
        let port = if port.is_empty() {
            None
        } else {
            Some(
                port.parse::<u16>()
                    .ok()
                    .filter(|port| *port > 0)
                    .ok_or("Enter a port between 1 and 65535")?,
            )
        };
        let username = text(&self.username);
        let identity = text(&self.identity);
        Ok(ConnectionOptions {
            host: text(&self.host),
            port,
            username: (!username.is_empty()).then_some(username),
            identity: (!identity.is_empty()).then(|| PathBuf::from(identity)),
        })
    }

    fn update_resolved(&mut self, cx: &mut Context<Self>) {
        // The username field shows who you'll log in as by default.
        let default_user = self
            .options(cx)
            .ok()
            .and_then(|options| {
                let options = ConnectionOptions {
                    username: None,
                    host: if options.host.is_empty() {
                        "localhost".into()
                    } else {
                        options.host
                    },
                    ..options
                };
                options.resolve_with(&self.config).ok()
            })
            .map(|target| target.user)
            .unwrap_or_default();
        self.username
            .update(cx, |field, cx| field.set_placeholder(default_user, cx));
        self.resolved = match self.options(cx) {
            Ok(options) if !options.host.is_empty() => match options.resolve_with(&self.config) {
                Ok(target) => {
                    let mut text = format!("Connects as {}", target.label());
                    if target.host_name != target.alias {
                        text = format!("Connects as {}@{}", target.user, target.host_name);
                        if target.port != 22 {
                            text.push_str(&format!(":{}", target.port));
                        }
                    }
                    text
                }
                Err(error) => error.to_string(),
            },
            _ => String::new(),
        };
        cx.notify();
    }

    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.auth.is_some() {
            self.answer_auth(true, cx);
            return;
        }
        match &self.step {
            Step::Connect => {}
            Step::Folder { connection, home } => {
                let (connection, home) = (connection.clone(), home.clone());
                let folder = self.folder.read(cx).text();
                self.open_folder(connection, home, folder, window, cx);
                return;
            }
            Step::Trust { key, .. } => {
                self.trusted.push(key.clone());
            }
            Step::Connecting | Step::PromptOnly => return,
        }
        let options = match self.options(cx) {
            Ok(options) => options,
            Err(error) => {
                self.message = error;
                cx.notify();
                return;
            }
        };
        let target = match options.resolve_with(&self.config) {
            Ok(target) => target,
            Err(error) => {
                self.step = Step::Connect;
                self.message = error.to_string();
                cx.notify();
                return;
            }
        };
        let password = self.password.read(cx).text();
        let password = (!password.is_empty()).then_some(password);
        let label = target.label();
        self.step = Step::Connecting;
        self.message = format!("Connecting to {label}…");
        window.focus(&self.focus_handle);
        cx.notify();
        let trusted = self.trusted.clone();
        let prompter = self.prompter.clone();
        self.task = Some(cx.spawn_in(window, async move |this, cx| {
            let connected = cx
                .background_spawn(async move {
                    let connection = Connection::connect_to(
                        options,
                        target,
                        password,
                        &trusted,
                        Some(prompter),
                    )?;
                    let home = connection.canonicalize(Path::new("."))?;
                    Ok::<_, ConnectError>((connection, home))
                })
                .await;
            this.update_in(cx, |this, window, cx| {
                this.auth = None;
                match connected {
                    Ok((connection, home)) => {
                        this.password
                            .update(cx, |password, cx| password.set_text("", cx));
                        // Reopening a recent project goes straight in.
                        if let Some(folder) = this.reopen.take() {
                            let folder = folder.to_string_lossy().into_owned();
                            this.step = Step::Folder {
                                connection: connection.clone(),
                                home: home.clone(),
                            };
                            this.open_folder(connection, home, folder, window, cx);
                        } else {
                            this.show_folder_step(connection, home, None, window, cx);
                        }
                    }
                    Err(ConnectError::UnknownHost {
                        host,
                        fingerprint,
                        key,
                    }) => {
                        this.step = Step::Trust {
                            host,
                            fingerprint,
                            key,
                        };
                        this.message.clear();
                        window.focus(&this.focus_handle);
                        cx.notify();
                    }
                    Err(error) => {
                        this.step = Step::Connect;
                        this.message = match &error {
                            ConnectError::Io(io)
                                if io.kind() == std::io::ErrorKind::Interrupted =>
                            {
                                "Login cancelled".into()
                            }
                            error => error.to_string(),
                        };
                        window.focus(&this.host.focus_handle(cx));
                        cx.notify();
                    }
                }
            })
            .ok();
        }));
    }

    fn cancel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.auth.is_some() {
            self.answer_auth(false, cx);
            return;
        }
        match self.step {
            Step::Connecting if !self.existing => {
                // Dropping the task abandons the attempt.
                self.task = None;
                self.step = Step::Connect;
                self.message = "Cancelled".into();
                window.focus(&self.host.focus_handle(cx));
                cx.notify();
            }
            Step::Trust { .. } => {
                self.step = Step::Connect;
                self.message = "Not connected: the host key wasn't trusted.".into();
                cx.notify();
            }
            _ => cx.emit(SshEvent::Dismissed),
        }
    }

    fn show_auth(&mut self, request: PromptRequest, window: &mut Window, cx: &mut Context<Self>) {
        // A newer question replaces an unanswered one.
        self.answer_auth(false, cx);
        let fields: Vec<_> = request
            .prompt
            .fields
            .iter()
            .map(|field| {
                cx.new(|cx| {
                    if field.secret {
                        let mut editor = Editor::password(cx);
                        editor.set_placeholder(field.label.clone(), cx);
                        editor
                    } else {
                        Editor::single_line(field.label.clone(), cx)
                    }
                })
            })
            .collect();
        if let Some(first) = fields.first() {
            window.focus(&first.focus_handle(cx));
        }
        self.auth = Some(Auth {
            prompt: request.prompt,
            fields,
            answer: Some(request.answer),
        });
        cx.notify();
    }

    fn answer_auth(&mut self, submit: bool, cx: &mut Context<Self>) {
        let Some(mut auth) = self.auth.take() else {
            return;
        };
        let answers = submit.then(|| {
            auth.fields
                .iter()
                .map(|field| field.read(cx).text())
                .collect::<Vec<_>>()
        });
        if let Some(answer) = auth.answer.take() {
            let _ = answer.send(answers);
        }
        if matches!(self.step, Step::PromptOnly) {
            cx.emit(SshEvent::Dismissed);
        }
        cx.notify();
    }

    fn show_folder_step(
        &mut self,
        connection: Arc<Connection>,
        home: PathBuf,
        current: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let start = current
            .as_deref()
            .and_then(Path::parent)
            .map(Path::to_path_buf)
            .unwrap_or_else(|| home.clone());
        let initial = format!(
            "{}/",
            start
                .to_string_lossy()
                .replace('\\', "/")
                .trim_end_matches('/')
        );
        self.step = Step::Folder { connection, home };
        self.message.clear();
        self.folder.update(cx, |folder, cx| {
            folder.set_text(&initial, cx);
            let end = initial.chars().count();
            folder.select_range((0, end), (0, end), cx);
        });
        window.focus(&self.folder.focus_handle(cx));
        // set_text's edit event lists the folder; make sure it runs now.
        self.complete_folder(cx);
    }

    fn recent_folders(&self, cx: &mut Context<Self>) -> Vec<String> {
        let Step::Folder { connection, .. } = &self.step else {
            return Vec::new();
        };
        let options = connection.options().clone();
        session::get(cx)
            .recent_remote
            .iter()
            .filter(|project| project.connection == options)
            .map(|project| project.folder.to_string_lossy().replace('\\', "/"))
            .take(5)
            .collect()
    }

    fn complete_folder(&mut self, cx: &mut Context<Self>) {
        let Step::Folder { connection, home } = &self.step else {
            return;
        };
        let (connection, home) = (connection.clone(), home.clone());
        let input = self.folder.read(cx).text();
        let query = Query::new(&input, &home);
        self.completion_task = None;
        self.completion_error = None;
        self.completion_selected = 0;
        self.completion_scroll
            .scroll_to_item(0, ScrollStrategy::Top);
        // Recent folders on this host come first while nothing specific is typed.
        let typed = input.trim().trim_end_matches('/').to_owned();
        let recent: Vec<Completion> = if query.prefix.is_empty() {
            self.recent_folders(cx)
                .into_iter()
                // All of them at home; matching ones once a path is typed.
                .filter(|path| {
                    *path != typed && (path.starts_with(&typed) || query.directory == home)
                })
                .map(|path| Completion { path, recent: true })
                .collect()
        } else {
            Vec::new()
        };
        if let Some((_, entries)) = self
            .completion_cache
            .iter()
            .find(|(dir, _)| *dir == query.directory)
        {
            let mut completions = recent;
            completions.extend(
                query
                    .suggestions(entries)
                    .into_iter()
                    .map(|path| Completion {
                        path,
                        recent: false,
                    }),
            );
            self.completions = completions;
            cx.notify();
            return;
        }
        self.completions = recent;
        cx.notify();
        self.completion_task = Some(cx.spawn(async move |this, cx| {
            // Typing fast lists only the last folder.
            cx.background_executor()
                .timer(std::time::Duration::from_millis(80))
                .await;
            let directory = query.directory.clone();
            let result = cx
                .background_spawn(async move { FileSystem::Ssh(connection).read_dir(&directory) })
                .await;
            this.update(cx, |this, cx| {
                if this.folder.read(cx).text() != input {
                    return;
                }
                match result {
                    Ok(entries) => {
                        this.completions
                            .extend(query.suggestions(&entries).into_iter().map(|path| {
                                Completion {
                                    path,
                                    recent: false,
                                }
                            }));
                        if this.completion_cache.len() >= 32 {
                            this.completion_cache.remove(0);
                        }
                        this.completion_cache.push((query.directory, entries));
                    }
                    Err(error) => {
                        this.completion_error = Some(format!("Can't list this folder: {error}"))
                    }
                }
                cx.notify();
            })
            .ok();
        }));
    }

    fn choose_completion(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = self.completions.get(index).map(|c| c.path.clone()) else {
            return;
        };
        let path = if path.ends_with('/') {
            path
        } else {
            format!("{path}/")
        };
        self.folder.update(cx, |folder, cx| {
            folder.set_text(&path, cx);
            let end = path.chars().count();
            folder.select_range((0, end), (0, end), cx);
        });
        self.message.clear();
        window.focus(&self.folder.focus_handle(cx));
        cx.notify();
    }

    fn move_selection(&mut self, delta: isize, cx: &mut Context<Self>) {
        match self.step {
            Step::Folder { .. } if !self.completions.is_empty() => {
                self.completion_selected = (self.completion_selected as isize + delta)
                    .rem_euclid(self.completions.len() as isize)
                    as usize;
                self.completion_scroll
                    .scroll_to_item(self.completion_selected, ScrollStrategy::Top);
            }
            Step::Connect => {
                let count = self.matching_hosts(cx).len();
                if count == 0 {
                    return;
                }
                self.host_selected = Some(match self.host_selected {
                    None if delta > 0 => 0,
                    None => count - 1,
                    Some(ix) => (ix as isize + delta).rem_euclid(count as isize) as usize,
                });
            }
            _ => return,
        }
        cx.notify();
    }

    fn choose_host(&mut self, choice: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(options) = self.hosts.get(choice).map(|choice| choice.options.clone()) else {
            return;
        };
        self.fill(&options, cx);
        self.host_selected = None;
        self.submit(window, cx);
    }

    fn open_folder(
        &mut self,
        connection: Arc<Connection>,
        home: PathBuf,
        folder: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let folder = folder.trim().replace('\\', "/");
        if folder.is_empty() {
            self.message = "Enter a folder on the server".into();
            cx.notify();
            return;
        }
        if self.opening {
            return;
        }
        self.opening = true;
        self.message = "Opening…".into();
        self.completion_task = None;
        cx.notify();
        self.task = Some(cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    let folder = if folder == "~" || folder.starts_with("~/") {
                        project::join_path(&home, folder.trim_start_matches('~'))
                    } else if folder.starts_with('/') {
                        PathBuf::from(folder)
                    } else {
                        project::join_path(&home, &folder)
                    };
                    let root = connection.canonicalize(&folder)?;
                    let filesystem = FileSystem::Ssh(connection.clone());
                    if !filesystem.is_dir(&root)? {
                        return Err(std::io::Error::other("That's a file, not a folder"));
                    }
                    filesystem.read_dir(&root)?;
                    Ok((connection, root))
                })
                .await;
            this.update(cx, |this, cx| {
                this.opening = false;
                match result {
                    Ok((connection, root)) => cx.emit(SshEvent::Connected(connection, root)),
                    Err(error) => {
                        this.message = format!("Can't open that folder: {error}");
                        this.complete_folder(cx);
                    }
                }
            })
            .ok();
        }));
    }

    fn pick_key(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let picked = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Choose an SSH private key".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            if let Ok(Ok(Some(paths))) = picked.await
                && let Some(path) = paths.first()
            {
                this.update(cx, |this, cx| {
                    this.identity
                        .update(cx, |input, cx| input.set_text(&path.to_string_lossy(), cx))
                })
                .ok();
            }
        })
        .detach();
    }
}

// ---- rendering ---------------------------------------------------------------

fn label(text: impl Into<SharedString>) -> Div {
    div()
        .text_size(theme::ui_font_size_small())
        .text_color(theme::text_muted())
        .child(text.into())
}

fn hint(text: impl Into<SharedString>) -> Div {
    div()
        .text_size(theme::ui_font_size_small())
        .text_color(theme::text_faint())
        .child(text.into())
}

fn field(id: &'static str, title: &'static str, editor: &Entity<Editor>) -> Div {
    div()
        .flex_1()
        .min_w_0()
        .flex()
        .flex_col()
        .gap_1()
        .child(label(title))
        .child(text_box(id, editor))
}

fn text_box(id: impl Into<gpui::ElementId>, editor: &Entity<Editor>) -> Stateful<Div> {
    div()
        .id(id)
        .h(px(30.))
        .bg(theme::bg())
        .border_1()
        .border_color(theme::border())
        .rounded(px(6.))
        .child(editor.clone())
}

fn title(text: impl Into<SharedString>) -> Div {
    div()
        .text_size(px(16.))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(theme::text())
        .child(text.into())
}

impl SshView {
    fn render_auth(&self, auth: &Auth, cx: &mut Context<Self>) -> Div {
        let prompt = &auth.prompt;
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(title(prompt.title.clone()))
            .when(!prompt.instructions.is_empty(), |form| {
                form.child(
                    div()
                        .text_color(theme::text_muted())
                        .child(prompt.instructions.clone()),
                )
            })
            .children(auth.fields.iter().zip(&prompt.fields).enumerate().map(
                |(ix, (editor, field))| {
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .when(prompt.fields.len() > 1, |row| {
                            row.child(label(field.label.clone()))
                        })
                        .child(text_box(("ssh-auth", ix), editor))
                },
            ))
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap_2()
                    .child(ui::secondary_button("ssh-auth-cancel", "Cancel").on_click(
                        cx.listener(|this, _: &ClickEvent, window, cx| this.cancel(window, cx)),
                    ))
                    .child(ui::primary_button("ssh-auth-ok", "Continue").on_click(
                        cx.listener(|this, _: &ClickEvent, window, cx| this.submit(window, cx)),
                    )),
            )
    }

    fn render_connect(&self, cx: &mut Context<Self>) -> Div {
        let busy = matches!(self.step, Step::Connecting);
        let hosts = self.matching_hosts(cx);
        let selected = self.host_selected;
        let host_rows = hosts.iter().enumerate().map(|(row, &choice)| {
            let host = &self.hosts[choice];
            div()
                .id(("ssh-host-choice", choice))
                .h(px(32.))
                .px_2()
                .flex()
                .items_center()
                .gap_2()
                .rounded(px(6.))
                .cursor_pointer()
                .hover(|row| row.bg(theme::hover_bg()))
                .when(selected == Some(row), |row| row.bg(theme::active_bg()))
                .child(ui::icon(if host.recent {
                    IconName::History
                } else {
                    IconName::Monitor
                }))
                .child(
                    div()
                        .flex_none()
                        .text_color(theme::text())
                        .child(host.title.clone()),
                )
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_size(theme::ui_font_size_small())
                        .text_color(theme::text_faint())
                        .child(host.detail.clone()),
                )
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                    this.choose_host(choice, window, cx)
                }))
        });
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(title("Connect to SSH"))
            .child(
                div()
                    .text_color(theme::text_muted())
                    .child("Open a folder on a Linux server. Files, Git and terminals run there."),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(label("Host"))
                    .child(text_box("ssh-host", &self.host))
                    .when(!self.resolved.is_empty(), |column| column.child(hint(self.resolved.clone()))),
            )
            .when(!hosts.is_empty() && !busy, |form| {
                form.child(
                    div()
                        .flex()
                        .flex_col()
                        .child(label("Saved hosts"))
                        .children(host_rows),
                )
            })
            .child(
                div()
                    .flex()
                    .gap_3()
                    .child(field("ssh-user", "Username", &self.username))
                    .child(field("ssh-password", "Password (optional)", &self.password)),
            )
            .child(
                div()
                    .id("ssh-more")
                    .flex()
                    .items_center()
                    .gap_1()
                    .cursor_pointer()
                    .text_size(theme::ui_font_size_small())
                    .text_color(theme::text_muted())
                    .hover(|row| row.text_color(theme::text()))
                    .child(ui::icon_sized(
                        if self.show_options {
                            IconName::ChevronDown
                        } else {
                            IconName::ChevronRight
                        },
                        px(14.),
                        theme::text_muted(),
                    ))
                    .child("Port and private key")
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.show_options = !this.show_options;
                        cx.notify();
                    })),
            )
            .when(self.show_options, |form| {
                form.child(
                    div()
                        .flex()
                        .items_end()
                        .gap_3()
                        .child(div().w(px(90.)).child(field("ssh-port", "Port", &self.port)))
                        .child(field("ssh-key", "Private key", &self.identity))
                        .child(
                            ui::secondary_button("ssh-pick-key", "Browse…").on_click(cx.listener(
                                |this, _: &ClickEvent, window, cx| this.pick_key(window, cx),
                            )),
                        ),
                )
            })
            .child(hint(
                "Your SSH agent and keys are tried first. You'll be asked if the server needs a password or a code. Nothing you type is saved.",
            ))
            .when(!self.message.is_empty(), |form| {
                form.child(div().text_color(theme::text_muted()).child(self.message.clone()))
            })
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap_2()
                    .child(
                        ui::secondary_button("ssh-cancel", "Cancel")
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.cancel(window, cx))),
                    )
                    .child(
                        ui::primary_button("ssh-connect", if busy { "Connecting…" } else { "Connect" })
                            .when(busy, |button| button.opacity(0.6))
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.submit(window, cx))),
                    ),
            )
    }

    fn render_trust(&self, host: &str, fingerprint: &str, cx: &mut Context<Self>) -> Div {
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(title(format!("Trust {host}?")))
            .child(div().text_color(theme::text_muted()).child(
                "This is the first connection to this host. Check that its fingerprint matches the one your server admin gave you:",
            ))
            .child(
                div()
                    .p_2()
                    .rounded(px(6.))
                    .bg(theme::bg())
                    .border_1()
                    .border_color(theme::border())
                    .font_family(theme::mono_font())
                    .text_size(theme::ui_font_size_small())
                    .child(fingerprint.to_owned()),
            )
            .child(hint("Trusted keys are added to your known_hosts file, like ssh does."))
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap_2()
                    .child(
                        ui::secondary_button("ssh-trust-cancel", "Cancel")
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.cancel(window, cx))),
                    )
                    .child(
                        ui::primary_button("ssh-trust", "Trust and Connect")
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.submit(window, cx))),
                    ),
            )
    }

    fn completion_rows(
        &mut self,
        range: std::ops::Range<usize>,
        cx: &mut Context<Self>,
    ) -> Vec<Stateful<Div>> {
        range
            .filter_map(|index| {
                let completion = self.completions.get(index)?.clone();
                Some(
                    div()
                        .id(index)
                        .w_full()
                        .h(px(28.))
                        .px_2()
                        .flex()
                        .items_center()
                        .gap_2()
                        .rounded(px(5.))
                        .cursor_pointer()
                        .hover(|row| row.bg(theme::hover_bg()))
                        .when(index == self.completion_selected, |row| {
                            row.bg(theme::active_bg())
                        })
                        .child(ui::icon(if completion.recent {
                            IconName::History
                        } else {
                            IconName::Folder
                        }))
                        .child(div().min_w_0().truncate().child(completion.path))
                        .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                            this.choose_completion(index, window, cx);
                            // Double-click opens it.
                            if event.click_count() >= 2 {
                                this.submit(window, cx);
                            }
                        })),
                )
            })
            .collect()
    }

    fn render_folder(&self, connection: &Arc<Connection>, cx: &mut Context<Self>) -> Div {
        let rows = self.completions.len().min(7);
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(title(if self.existing {
                "Open Remote Folder"
            } else {
                "Open a folder"
            }))
            .child(div().text_color(theme::text_muted()).child(format!(
                "On {}. Type a path; Tab completes it.",
                connection.label()
            )))
            .child(text_box("ssh-folder", &self.folder))
            .when(rows > 0, |form| {
                form.child(
                    uniform_list(
                        "ssh-folder-completions",
                        self.completions.len(),
                        cx.processor(|this, range, _, cx| this.completion_rows(range, cx)),
                    )
                    .track_scroll(self.completion_scroll.clone())
                    .h(px(rows as f32 * 28.)),
                )
            })
            .when_some(self.completion_error.clone(), |form, error| {
                form.child(hint(error))
            })
            .when(!self.message.is_empty(), |form| {
                form.child(
                    div()
                        .text_color(theme::text_muted())
                        .child(self.message.clone()),
                )
            })
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .flex()
                            .gap_2()
                            .child(ui::kbd("Tab"))
                            .child(hint("complete"))
                            .child(ui::kbd("↑↓"))
                            .child(hint("choose"))
                            .child(ui::kbd("Enter"))
                            .child(hint("open")),
                    )
                    .child(
                        ui::secondary_button(
                            "ssh-folder-cancel",
                            if self.existing {
                                "Cancel"
                            } else {
                                "Disconnect"
                            },
                        )
                        .on_click(
                            cx.listener(|_, _: &ClickEvent, _, cx| cx.emit(SshEvent::Dismissed)),
                        ),
                    )
                    .child(
                        ui::primary_button(
                            "ssh-open",
                            if self.opening { "Opening…" } else { "Open" },
                        )
                        .on_click(
                            cx.listener(|this, _: &ClickEvent, window, cx| this.submit(window, cx)),
                        ),
                    ),
            )
    }
}

impl Render for SshView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let content = if let Some(auth) = &self.auth {
            self.render_auth(auth, cx)
        } else {
            match &self.step {
                Step::Connect | Step::Connecting if !self.existing => self.render_connect(cx),
                Step::Connecting | Step::PromptOnly => div()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(title("Remote folder"))
                    .child(
                        div()
                            .text_color(theme::text_muted())
                            .child(self.message.clone()),
                    ),
                Step::Connect => self.render_connect(cx),
                Step::Trust {
                    host, fingerprint, ..
                } => {
                    let (host, fingerprint) = (host.clone(), fingerprint.clone());
                    self.render_trust(&host, &fingerprint, cx)
                }
                Step::Folder { connection, .. } => {
                    let connection = connection.clone();
                    self.render_folder(&connection, cx)
                }
            }
        };
        div()
            .key_context("SshConnection")
            .track_focus(&self.focus_handle)
            .w(px(520.))
            .flex()
            .flex_col()
            .p_4()
            .bg(theme::elevated_bg())
            .border_1()
            .border_color(theme::border())
            .rounded(px(10.))
            .shadow_lg()
            .on_action(cx.listener(|this, _: &editor::Newline, window, cx| {
                if let (Step::Connect, Some(row)) = (&this.step, this.host_selected)
                    && this.auth.is_none()
                    && let Some(&choice) = this.matching_hosts(cx).get(row)
                {
                    this.choose_host(choice, window, cx);
                    return;
                }
                this.submit(window, cx)
            }))
            .on_action(cx.listener(|this, _: &editor::Tab, window, cx| {
                if matches!(this.step, Step::Folder { .. })
                    && this.auth.is_none()
                    && !this.completions.is_empty()
                {
                    this.choose_completion(this.completion_selected, window, cx);
                } else {
                    cx.propagate();
                }
            }))
            .on_action(cx.listener(|this, _: &editor::MoveUp, _, cx| this.move_selection(-1, cx)))
            .on_action(cx.listener(|this, _: &editor::MoveDown, _, cx| this.move_selection(1, cx)))
            .on_action(cx.listener(|this, _: &Dismiss, window, cx| this.cancel(window, cx)))
            .on_action(cx.listener(|this, _: &Confirm, window, cx| this.submit(window, cx)))
            .child(content)
    }
}
