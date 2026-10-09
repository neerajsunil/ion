//! Remembers recent folders and what was open, across restarts.
//!
//! One store is shared by every window (a GPUI global), so windows never
//! overwrite each other's recent projects.

use std::path::{Path, PathBuf};

use gpui::{App, AppContext, Global};
use remote::ConnectionOptions;
use serde::{Deserialize, Serialize};

const MAX_RECENT: usize = 10;

#[derive(Default, Clone, Serialize, Deserialize)]
pub struct Session {
    /// Most recent first.
    #[serde(default)]
    pub recent_folders: Vec<PathBuf>,
    /// Remote projects, most recent first. No secrets are stored.
    #[serde(default)]
    pub recent_remote: Vec<RemoteProject>,
    /// The layout of the last local window saved.
    #[serde(default)]
    pub last: Option<WorkspaceState>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteProject {
    pub connection: ConnectionOptions,
    pub folder: PathBuf,
}

impl RemoteProject {
    /// `folder — host` for menus.
    pub fn title(&self) -> String {
        let name = self
            .folder
            .to_string_lossy()
            .trim_end_matches('/')
            .rsplit('/')
            .next()
            .filter(|name| !name.is_empty())
            .unwrap_or("/")
            .to_owned();
        format!("{name} — {}", self.connection.host)
    }
}

struct Store(Session);

impl Global for Store {}

/// The shared session, if it has been loaded (`get` loads it).
pub fn read(cx: &App) -> &Session {
    static EMPTY: std::sync::LazyLock<Session> = std::sync::LazyLock::new(Session::default);
    cx.try_global::<Store>().map_or(&EMPTY, |store| &store.0)
}

/// The shared session, loaded from disk the first time it's needed.
pub fn get(cx: &mut App) -> &Session {
    if !cx.has_global::<Store>() {
        cx.set_global(Store(load()));
    }
    &cx.global::<Store>().0
}

/// Changes the shared session and saves it in the background.
pub fn update(cx: &mut App, change: impl FnOnce(&mut Session)) {
    get(cx);
    change(&mut cx.global_mut::<Store>().0);
    let snapshot = cx.global::<Store>().0.clone();
    cx.background_spawn(async move { save(&snapshot).ok() })
        .detach();
}

#[derive(Clone, Serialize, Deserialize)]
pub struct WorkspaceState {
    pub root: PathBuf,
    #[serde(default)]
    pub open_files: Vec<PathBuf>,
    #[serde(default)]
    pub active_file: usize,
    #[serde(default)]
    pub terminal_visible: bool,
    #[serde(default = "default_true")]
    pub sidebar_visible: bool,
    #[serde(default)]
    pub sidebar_width: Option<f32>,
    /// Pane layout of the editor area and the terminal dock.
    #[serde(default)]
    pub center: Option<SavedNode>,
    #[serde(default)]
    pub dock: Option<SavedNode>,
    #[serde(default)]
    pub dock_height: Option<f32>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SavedNode {
    Pane {
        items: Vec<SavedItem>,
        #[serde(default)]
        active: usize,
    },
    Split {
        /// Side by side (true) or stacked.
        row: bool,
        children: Vec<SavedNode>,
        flexes: Vec<f32>,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SavedItem {
    File {
        path: PathBuf,
    },
    Terminal {
        #[serde(default)]
        name: Option<String>,
        #[serde(default)]
        cwd: Option<PathBuf>,
        /// A shell picked from the menu, by name.
        #[serde(default)]
        shell: Option<String>,
        /// An agent harness it ran instead, by command (`claude`).
        #[serde(default)]
        harness: Option<String>,
    },
    Problems,
}

impl SavedNode {
    /// Every file in the layout, to load before rebuilding it.
    pub fn files(&self, out: &mut Vec<PathBuf>) {
        match self {
            SavedNode::Pane { items, .. } => {
                out.extend(items.iter().filter_map(|item| match item {
                    SavedItem::File { path } => Some(path.clone()),
                    SavedItem::Terminal { .. } | SavedItem::Problems => None,
                }))
            }
            SavedNode::Split { children, .. } => {
                for child in children {
                    child.files(out);
                }
            }
        }
    }
}

fn default_true() -> bool {
    true
}

impl Session {
    pub fn add_recent(&mut self, folder: &Path) {
        self.recent_folders.retain(|recent| recent != folder);
        self.recent_folders.insert(0, folder.to_path_buf());
        self.recent_folders.truncate(MAX_RECENT);
    }

    pub fn add_recent_remote(&mut self, project: RemoteProject) {
        self.recent_remote.retain(|recent| *recent != project);
        self.recent_remote.insert(0, project);
        self.recent_remote.truncate(MAX_RECENT);
    }
}

fn session_file() -> Option<PathBuf> {
    let base = if cfg!(windows) {
        std::env::var_os("APPDATA").map(PathBuf::from)
    } else if cfg!(target_os = "macos") {
        std::env::var_os("HOME").map(|home| PathBuf::from(home).join("Library/Application Support"))
    } else {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
    }?;
    Some(base.join("Ion").join("session.json"))
}

/// Loads the saved session, or an empty one if there is none (or it's unreadable).
pub fn load() -> Session {
    session_file()
        .and_then(|path| std::fs::read(path).ok())
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

/// Writes the session atomically (temp file + rename), so a crash mid-write
/// never leaves a corrupt file.
pub fn save(session: &Session) -> std::io::Result<()> {
    let Some(path) = session_file() else {
        return Ok(());
    };
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let json = serde_json::to_vec_pretty(session).map_err(std::io::Error::other)?;
    let temp = path.with_extension("json.tmp");
    std::fs::write(&temp, json)?;
    std::fs::rename(temp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recent_folders_are_deduplicated_and_capped() {
        let mut session = Session::default();
        for i in 0..12 {
            session.add_recent(Path::new(&format!("/p{i}")));
        }
        session.add_recent(Path::new("/p5"));
        assert_eq!(session.recent_folders.len(), MAX_RECENT);
        assert_eq!(session.recent_folders[0], Path::new("/p5"));
        assert_eq!(
            session
                .recent_folders
                .iter()
                .filter(|p| *p == Path::new("/p5"))
                .count(),
            1
        );
    }

    #[test]
    fn layouts_round_trip() {
        let layout = SavedNode::Split {
            row: true,
            children: vec![
                SavedNode::Pane {
                    items: vec![SavedItem::File {
                        path: PathBuf::from("/p/a.rs"),
                    }],
                    active: 0,
                },
                SavedNode::Pane {
                    items: vec![SavedItem::Terminal {
                        name: Some("claude".into()),
                        cwd: None,
                        shell: None,
                        harness: Some("claude".into()),
                    }],
                    active: 0,
                },
            ],
            flexes: vec![2., 1.],
        };
        let json = serde_json::to_string(&layout).unwrap();
        assert_eq!(serde_json::from_str::<SavedNode>(&json).unwrap(), layout);
        let mut files = Vec::new();
        layout.files(&mut files);
        assert_eq!(files, [PathBuf::from("/p/a.rs")]);
    }

    #[test]
    fn old_files_without_new_fields_still_load() {
        let session: Session = serde_json::from_str(r#"{"last":{"root":"/x"}}"#).unwrap();
        let last = session.last.unwrap();
        assert!(last.sidebar_visible);
        assert!(last.open_files.is_empty());
    }
}
