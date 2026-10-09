//! `git status --porcelain=v2` parsing.

/// How a file differs on one side (index or working tree).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileState {
    Unmodified,
    Modified,
    Added,
    Deleted,
    Renamed,
    Copied,
    TypeChanged,
    Untracked,
}

impl FileState {
    fn from_code(code: u8) -> Self {
        match code {
            b'M' => Self::Modified,
            b'A' => Self::Added,
            b'D' => Self::Deleted,
            b'R' => Self::Renamed,
            b'C' => Self::Copied,
            b'T' => Self::TypeChanged,
            _ => Self::Unmodified,
        }
    }

    /// One-letter label, as `git status --short` shows it.
    pub fn letter(self) -> &'static str {
        match self {
            Self::Unmodified => " ",
            Self::Modified => "M",
            Self::Added => "A",
            Self::Deleted => "D",
            Self::Renamed => "R",
            Self::Copied => "C",
            Self::TypeChanged => "T",
            Self::Untracked => "U",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StatusEntry {
    /// Path relative to the repository root, with `/` separators.
    pub path: String,
    /// The path before a rename or copy.
    pub orig_path: Option<String>,
    /// Staged change (HEAD to index).
    pub index: FileState,
    /// Unstaged change (index to working tree).
    pub worktree: FileState,
    /// Has merge conflicts.
    pub conflicted: bool,
}

impl StatusEntry {
    pub fn is_staged(&self) -> bool {
        self.index != FileState::Unmodified && !self.conflicted
    }

    pub fn is_unstaged(&self) -> bool {
        self.worktree != FileState::Unmodified && !self.conflicted
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BranchInfo {
    /// Current branch name; `None` when HEAD is detached.
    pub head: Option<String>,
    /// Commit HEAD points at; `None` before the first commit.
    pub oid: Option<String>,
    pub upstream: Option<String>,
    pub ahead: u32,
    pub behind: u32,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Status {
    pub branch: BranchInfo,
    /// Changed files, sorted by path.
    pub entries: Vec<StatusEntry>,
}

/// Parses `git status --porcelain=v2 -z --branch` output.
pub(crate) fn parse(output: &[u8]) -> Status {
    let mut status = Status::default();
    let mut fields = output.split(|&b| b == 0);
    while let Some(record) = fields.next() {
        let record = String::from_utf8_lossy(record);
        let Some((kind, rest)) = record.split_once(' ') else {
            continue;
        };
        match kind {
            "#" => parse_header(rest, &mut status.branch),
            "1" | "2" | "u" => {
                // Fields before the path; the path itself may contain spaces.
                let skip = match kind {
                    "1" => 7,
                    "2" => 8,
                    _ => 9,
                };
                let mut parts = rest.splitn(skip + 1, ' ');
                let xy = parts.next().unwrap_or("..").as_bytes();
                let Some(path) = parts.nth(skip - 1) else {
                    continue;
                };
                let orig_path = (kind == "2")
                    .then(|| fields.next())
                    .flatten()
                    .map(|orig| String::from_utf8_lossy(orig).into_owned());
                let conflicted = kind == "u";
                status.entries.push(StatusEntry {
                    path: path.to_owned(),
                    orig_path,
                    index: FileState::from_code(xy[0]),
                    worktree: FileState::from_code(xy.get(1).copied().unwrap_or(b'.')),
                    conflicted,
                });
            }
            "?" => status.entries.push(StatusEntry {
                path: rest.to_owned(),
                orig_path: None,
                index: FileState::Unmodified,
                worktree: FileState::Untracked,
                conflicted: false,
            }),
            _ => {}
        }
    }
    status.entries.sort_by(|a, b| a.path.cmp(&b.path));
    status
}

fn parse_header(header: &str, branch: &mut BranchInfo) {
    let Some((key, value)) = header.split_once(' ') else {
        return;
    };
    match key {
        "branch.oid" if value != "(initial)" => branch.oid = Some(value.to_owned()),
        "branch.head" if value != "(detached)" => branch.head = Some(value.to_owned()),
        "branch.upstream" => branch.upstream = Some(value.to_owned()),
        "branch.ab" => {
            for part in value.split(' ') {
                if let Some(n) = part.strip_prefix('+') {
                    branch.ahead = n.parse().unwrap_or(0);
                } else if let Some(n) = part.strip_prefix('-') {
                    branch.behind = n.parse().unwrap_or(0);
                }
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_porcelain_v2() {
        let output = b"# branch.oid 1234abcd\0\
# branch.head main\0\
# branch.upstream origin/main\0\
# branch.ab +2 -1\0\
1 .M N... 100644 100644 100644 aaa bbb src/main file.rs\0\
1 A. N... 000000 100644 100644 000 ccc new.rs\0\
2 R. N... 100644 100644 100644 ddd ddd R100 renamed.rs\0old.rs\0\
u UU N... 100644 100644 100644 100644 e f g conflict.rs\0\
? notes.txt\0";
        let status = parse(output);
        assert_eq!(status.branch.head.as_deref(), Some("main"));
        assert_eq!(status.branch.oid.as_deref(), Some("1234abcd"));
        assert_eq!((status.branch.ahead, status.branch.behind), (2, 1));
        let paths: Vec<_> = status.entries.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(
            paths,
            [
                "conflict.rs",
                "new.rs",
                "notes.txt",
                "renamed.rs",
                "src/main file.rs"
            ]
        );
        let modified = &status.entries[4];
        assert_eq!(modified.worktree, FileState::Modified);
        assert!(!modified.is_staged() && modified.is_unstaged());
        let renamed = &status.entries[3];
        assert_eq!(renamed.orig_path.as_deref(), Some("old.rs"));
        assert_eq!(renamed.index, FileState::Renamed);
        assert!(status.entries[0].conflicted);
        assert_eq!(status.entries[2].worktree, FileState::Untracked);
    }

    #[test]
    fn parses_detached_and_initial() {
        let status = parse(b"# branch.oid (initial)\0# branch.head (detached)\0");
        assert_eq!(status.branch, BranchInfo::default());
    }
}
