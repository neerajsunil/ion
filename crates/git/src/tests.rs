//! End-to-end tests against throwaway repositories (needs `git` on PATH).

use std::fs;
use std::path::PathBuf;

use crate::cmd::run;
use crate::{FileState, LineKind, Repository};

fn scratch_repo(name: &str) -> (PathBuf, Repository) {
    let dir = std::env::temp_dir().join(format!("ion-git-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let repo = Repository::init(&dir).unwrap();
    for (key, value) in [
        ("user.name", "Ion Test"),
        ("user.email", "test@example.com"),
        ("commit.gpgsign", "false"),
        ("core.autocrlf", "false"),
    ] {
        run(&dir, &["config", key, value]).unwrap();
    }
    (dir, repo)
}

#[test]
fn status_stage_commit_and_history() {
    let (dir, repo) = scratch_repo("flow");
    assert!(repo.log(0, 10).unwrap().is_empty());

    fs::write(dir.join("a.txt"), "one\ntwo\n").unwrap();
    let status = repo.status().unwrap();
    assert_eq!(status.entries.len(), 1);
    assert_eq!(status.entries[0].worktree, FileState::Untracked);

    repo.stage(&["a.txt"]).unwrap();
    assert_eq!(repo.status().unwrap().entries[0].index, FileState::Added);
    repo.unstage(&["a.txt"]).unwrap();
    assert!(!repo.status().unwrap().entries[0].is_staged());

    repo.stage_all().unwrap();
    repo.commit("First commit\n\nWith a body.").unwrap();
    let status = repo.status().unwrap();
    assert!(status.entries.is_empty());
    assert!(status.branch.oid.is_some());

    fs::write(dir.join("a.txt"), "one\n2\n").unwrap();
    let diff = repo.diff_unstaged("a.txt").unwrap();
    assert_eq!(diff[0].stats(), (1, 1));
    assert_eq!(diff[0].hunks[0].lines[1].kind, LineKind::Removed);
    assert_eq!(
        repo.head_text("a.txt").unwrap().as_deref(),
        Some("one\ntwo\n")
    );
    assert_eq!(repo.head_text("missing.txt").unwrap(), None);

    repo.discard(&["a.txt"]).unwrap();
    assert_eq!(fs::read_to_string(dir.join("a.txt")).unwrap(), "one\ntwo\n");

    let log = repo.log(0, 10).unwrap();
    assert_eq!(log.len(), 1);
    assert_eq!(log[0].subject, "First commit");
    let details = repo.commit_details(&log[0].oid).unwrap();
    assert_eq!(details.message, "First commit\n\nWith a body.");
    assert_eq!(details.files[0].path(), "a.txt");

    let blame = repo.blame("a.txt").unwrap();
    assert_eq!(blame.lines.len(), 2);
    assert_eq!(blame.commit_for_line(1).unwrap().author, "Ion Test");

    repo.switch_branch("feature", true).unwrap();
    assert_eq!(
        repo.status().unwrap().branch.head.as_deref(),
        Some("feature")
    );
    assert_eq!(repo.branches().unwrap().len(), 2);

    fs::remove_dir_all(&dir).ok();
}

#[test]
fn discovers_from_a_subfolder() {
    let (dir, _) = scratch_repo("discover");
    fs::create_dir_all(dir.join("src/deep")).unwrap();
    let repo = Repository::discover(&dir.join("src/deep"))
        .unwrap()
        .unwrap();
    let file = repo.workdir().join("src").join("deep").join("x.rs");
    assert_eq!(repo.relative(&file).as_deref(), Some("src/deep/x.rs"));
    assert_eq!(repo.absolute("src/deep/x.rs"), file);
    assert!(repo.git_dir().ends_with(".git"));

    let outside = std::env::temp_dir().join(format!("ion-git-none-{}", std::process::id()));
    fs::create_dir_all(&outside).unwrap();
    // The temp folder itself might live inside some repository; only check
    // that discovery doesn't fail.
    assert!(Repository::discover(&outside).is_ok());
    fs::remove_dir_all(&outside).ok();
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn publish_push_fetch_and_pull() {
    let (dir, repo) = scratch_repo("sync-a");
    let remote = std::env::temp_dir().join(format!("ion-git-remote-{}", std::process::id()));
    let _ = fs::remove_dir_all(&remote);
    fs::create_dir_all(&remote).unwrap();
    run(&remote, &["init", "--bare"]).unwrap();
    run(&dir, &["remote", "add", "origin", remote.to_str().unwrap()]).unwrap();
    fs::write(dir.join("a.txt"), "one\n").unwrap();
    repo.stage_all().unwrap();
    repo.commit("First").unwrap();

    // No upstream yet: publishing sets it.
    assert!(repo.status().unwrap().branch.upstream.is_none());
    repo.push(false).unwrap();
    assert!(repo.status().unwrap().branch.upstream.is_some());

    // A second clone pushes a commit; the first fetches and pulls it.
    let (other_dir, other) = scratch_repo("sync-b");
    run(
        &other_dir,
        &["remote", "add", "origin", remote.to_str().unwrap()],
    )
    .unwrap();
    other.fetch().unwrap();
    let branch = repo.status().unwrap().branch.head.unwrap();
    run(
        &other_dir,
        &["switch", "--track", &format!("origin/{branch}")],
    )
    .unwrap();
    fs::write(other_dir.join("b.txt"), "two\n").unwrap();
    other.stage_all().unwrap();
    other.commit("Second").unwrap();
    other.push(true).unwrap();

    repo.fetch().unwrap();
    assert_eq!(repo.status().unwrap().branch.behind, 1);
    repo.pull().unwrap();
    assert_eq!(fs::read_to_string(dir.join("b.txt")).unwrap(), "two\n");
    assert_eq!(repo.status().unwrap().branch.behind, 0);

    for path in [dir, other_dir, remote] {
        let _ = fs::remove_dir_all(path);
    }
}
