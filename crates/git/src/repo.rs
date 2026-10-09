//! A repository and the git operations Ion runs on it.
//!
//! Every method blocks while git runs; call them from background threads.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::blame::{self, Blame};
use crate::cmd::{GitError, Result, run, run_remote, run_text, run_with_input};
use crate::log::{self, CommitDetails, CommitSummary};
use crate::patch::{self, FileDiff};
use crate::status::{self, Status};

/// Options that keep diff output in the shape the parser expects.
const DIFF_ARGS: [&str; 5] = [
    "--no-ext-diff",
    "--no-textconv",
    "--src-prefix=a/",
    "--dst-prefix=b/",
    "-M",
];

#[derive(Clone, Debug)]
pub struct Repository {
    workdir: PathBuf,
    git_dir: PathBuf,
    connection: Option<Arc<remote::Connection>>,
}

impl Repository {
    /// Finds the repository containing `path`. `Ok(None)` if there isn't one.
    pub fn discover(path: &Path) -> Result<Option<Self>> {
        Self::discover_with_connection(path, None)
    }

    pub fn discover_with_connection(
        path: &Path,
        connection: Option<Arc<remote::Connection>>,
    ) -> Result<Option<Self>> {
        let args = &["rev-parse", "--show-toplevel", "--absolute-git-dir"];
        let output = match &connection {
            Some(connection) => run_remote(connection, path, args, None)
                .map(|bytes| String::from_utf8_lossy(&bytes).into_owned()),
            None => run_text(path, args),
        };
        match output {
            Ok(output) => {
                let mut lines = output.lines();
                let (Some(workdir), Some(git_dir)) = (lines.next(), lines.next()) else {
                    return Ok(None);
                };
                // Remote paths stay as the server wrote them.
                let path = |text: &str| match connection {
                    Some(_) => PathBuf::from(text),
                    None => PathBuf::from(text.replace('/', std::path::MAIN_SEPARATOR_STR)),
                };
                Ok(Some(Self {
                    workdir: path(workdir),
                    git_dir: path(git_dir),
                    connection,
                }))
            }
            // Not a repository (or a bare one, or inside `.git`).
            Err(GitError::Failed(_)) => Ok(None),
            Err(err) => Err(err),
        }
    }

    /// Runs `git init` in `path`.
    pub fn init(path: &Path) -> Result<Self> {
        run(path, &["init"])?;
        Self::discover(path)?.ok_or_else(|| GitError::Failed("git init failed".into()))
    }

    pub fn init_with_connection(
        path: &Path,
        connection: Option<Arc<remote::Connection>>,
    ) -> Result<Self> {
        match &connection {
            Some(connection) => run_remote(connection, path, &["init"], None)?,
            None => run(path, &["init"])?,
        };
        Self::discover_with_connection(path, connection)?
            .ok_or_else(|| GitError::Failed("git init failed".into()))
    }

    fn run(&self, args: &[&str]) -> Result<Vec<u8>> {
        self.run_with_input(args, None)
    }
    fn run_text(&self, args: &[&str]) -> Result<String> {
        self.run(args)
            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
    }
    fn run_with_input(&self, args: &[&str], input: Option<&[u8]>) -> Result<Vec<u8>> {
        match &self.connection {
            Some(connection) => run_remote(connection, &self.workdir, args, input),
            None => run_with_input(&self.workdir, args, input),
        }
    }

    pub fn workdir(&self) -> &Path {
        &self.workdir
    }

    pub fn git_dir(&self) -> &Path {
        &self.git_dir
    }

    /// `path` relative to the repository root with `/` separators, if it's
    /// inside the repository.
    pub fn relative(&self, path: &Path) -> Option<String> {
        let relative = match path.strip_prefix(&self.workdir) {
            Ok(relative) => relative.to_path_buf(),
            // Drive letters and folder names can differ in case on Windows.
            Err(_) if cfg!(windows) => {
                let path = path.to_str()?;
                let root = self.workdir.to_str()?;
                let prefix = path.get(..root.len())?;
                let rest = &path[root.len()..];
                // "C:\proj" must not match "C:\project2".
                let at_boundary = rest.is_empty() || rest.starts_with(['\\', '/']);
                if !prefix.eq_ignore_ascii_case(root) || !at_boundary {
                    return None;
                }
                PathBuf::from(rest.trim_start_matches(['\\', '/']))
            }
            Err(_) => return None,
        };
        Some(relative.to_str()?.replace('\\', "/"))
    }

    pub fn absolute(&self, relative: &str) -> PathBuf {
        join(&self.workdir, relative)
    }

    pub fn status(&self) -> Result<Status> {
        let output = self.run(&[
            "status",
            "--porcelain=v2",
            "-z",
            "--branch",
            "--untracked-files=all",
        ])?;
        Ok(status::parse(&output))
    }

    /// The file's text at HEAD; `None` if it isn't in HEAD or isn't text.
    pub fn head_text(&self, relative: &str) -> Result<Option<String>> {
        match self.run(&["show", &format!("HEAD:{relative}")]) {
            Ok(bytes) if !bytes.contains(&0) => Ok(String::from_utf8(bytes).ok()),
            Ok(_) | Err(GitError::Failed(_)) => Ok(None),
            Err(err) => Err(err),
        }
    }

    /// Who last changed each line of the file as of HEAD.
    pub fn blame(&self, relative: &str) -> Result<Blame> {
        let output = self.run_text(&["blame", "--porcelain", "HEAD", "--", relative])?;
        Ok(blame::parse(&output))
    }

    /// Up to `limit` commits reachable from HEAD, newest first, skipping
    /// the first `skip`.
    pub fn log(&self, skip: usize, limit: usize) -> Result<Vec<CommitSummary>> {
        let skip = format!("--skip={skip}");
        let limit = format!("--max-count={limit}");
        match self.run_text(&["log", log::LOG_FORMAT, &skip, &limit]) {
            Ok(output) => Ok(log::parse_log(&output)),
            // No commits yet.
            Err(GitError::Failed(message)) if message.contains("does not have any commits") => {
                Ok(Vec::new())
            }
            Err(err) => Err(err),
        }
    }

    pub fn commit_details(&self, oid: &str) -> Result<CommitDetails> {
        let mut args = vec![
            "show",
            log::SHOW_FORMAT,
            "--date=format-local:%Y-%m-%d %H:%M",
            "--diff-merges=first-parent",
        ];
        args.extend(DIFF_ARGS);
        args.extend([oid, "--"]);
        let output = self.run_text(&args)?;
        log::parse_show(&output).ok_or_else(|| GitError::Failed(format!("can't read {oid}")))
    }

    /// Unstaged changes to a file (index to working tree).
    pub fn diff_unstaged(&self, relative: &str) -> Result<Vec<FileDiff>> {
        self.diff(&[], relative)
    }

    /// Staged changes to a file (HEAD to index).
    pub fn diff_staged(&self, relative: &str) -> Result<Vec<FileDiff>> {
        self.diff(&["--cached"], relative)
    }

    fn diff(&self, extra: &[&str], relative: &str) -> Result<Vec<FileDiff>> {
        let mut args = vec!["diff"];
        args.extend(DIFF_ARGS);
        args.extend(extra);
        args.extend(["--", relative]);
        Ok(patch::parse(&self.run_text(&args)?))
    }

    pub fn stage(&self, paths: &[&str]) -> Result<()> {
        self.run_on_paths(&["add", "--all"], paths)
    }

    pub fn stage_all(&self) -> Result<()> {
        self.run(&["add", "--all"]).map(drop)
    }

    pub fn unstage(&self, paths: &[&str]) -> Result<()> {
        self.run_on_paths(&["restore", "--staged"], paths)
            // Before the first commit there's no HEAD to restore from.
            .or_else(|_| self.run_on_paths(&["rm", "--cached", "-r", "--quiet"], paths))
    }

    pub fn unstage_all(&self) -> Result<()> {
        self.run(&["restore", "--staged", "."])
            .or_else(|_| self.run(&["rm", "--cached", "-r", "--quiet", "."]))
            .map(drop)
    }

    /// Throws away unstaged changes to tracked files.
    pub fn discard(&self, paths: &[&str]) -> Result<()> {
        self.run_on_paths(&["restore", "--worktree"], paths)
    }

    pub fn commit(&self, message: &str) -> Result<()> {
        self.run_with_input(&["commit", "--file=-"], Some(message.as_bytes()))
            .map(drop)
    }

    /// Downloads new commits from every remote.
    pub fn fetch(&self) -> Result<()> {
        self.run(&["fetch", "--all", "--prune"]).map(drop)
    }

    /// Brings the current branch up to date with its upstream (merging or
    /// rebasing as the repository is configured to).
    pub fn pull(&self) -> Result<()> {
        self.run(&["pull", "--no-edit"]).map(drop)
    }

    /// Pushes the current branch. A branch without an upstream is published
    /// to the first remote (usually `origin`) and tracks it from then on.
    pub fn push(&self, has_upstream: bool) -> Result<()> {
        if has_upstream {
            return self.run(&["push"]).map(drop);
        }
        let remotes = self.run_text(&["remote"])?;
        let remote = remotes
            .lines()
            .find(|name| *name == "origin")
            .or_else(|| remotes.lines().next())
            .ok_or_else(|| GitError::Failed("This repository has no remote to push to.".into()))?
            .to_owned();
        self.run(&["push", "--set-upstream", &remote, "HEAD"])
            .map(drop)
    }

    /// Local branches, most recently committed to first.
    pub fn branches(&self) -> Result<Vec<String>> {
        let output = self.run_text(&[
            "for-each-ref",
            "--sort=-committerdate",
            "--format=%(refname:short)",
            "refs/heads",
        ])?;
        Ok(output.lines().map(str::to_owned).collect())
    }

    pub fn switch_branch(&self, name: &str, create: bool) -> Result<()> {
        let args: &[&str] = if create {
            &["switch", "--create", name]
        } else {
            &["switch", name]
        };
        self.run(args).map(drop)
    }

    fn run_on_paths(&self, args: &[&str], paths: &[&str]) -> Result<()> {
        let mut all = args.to_vec();
        all.push("--");
        all.extend(paths);
        self.run(&all).map(drop)
    }
}

/// git prints `/` separators even on Windows.
/// Remote (Linux) work trees start with `/` and keep `/` separators.
fn join(dir: &Path, relative: &str) -> PathBuf {
    let text = dir.to_string_lossy();
    if text.starts_with('/') {
        PathBuf::from(format!("{}/{relative}", text.trim_end_matches('/')))
    } else {
        dir.join(relative.replace('/', std::path::MAIN_SEPARATOR_STR))
    }
}
