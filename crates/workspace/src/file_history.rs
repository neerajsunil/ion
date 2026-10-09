//! Which files were opened in each project, how often and how recently, kept
//! across restarts to rank Go to File ("frecency").

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// A use's weight halves after this long.
const HALF_LIFE_SECS: f64 = 3. * 24. * 60. * 60.;
/// Files remembered per project; the least used go first.
const MAX_FILES: usize = 300;
/// Projects remembered; the least recently opened go first.
const MAX_PROJECTS: usize = 30;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ProjectHistory {
    /// The project's root, prefixed with its host for remote projects.
    pub project: String,
    pub files: Vec<FileUse>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FileUse {
    pub path: PathBuf,
    /// Uses, each worth 1 when it happened and decaying since.
    pub score: f64,
    /// When it was last used, in seconds since the Unix epoch.
    pub last: u64,
}

impl FileUse {
    /// The score at `now`.
    pub fn frecency(&self, now: u64) -> f64 {
        let age = now.saturating_sub(self.last) as f64;
        self.score * 0.5f64.powf(age / HALF_LIFE_SECS)
    }
}

/// Seconds since the Unix epoch.
pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

/// Counts a use of `path` in `project` at `now`, and moves the project to
/// the front.
pub fn record(histories: &mut Vec<ProjectHistory>, project: &str, path: &Path, now: u64) {
    let ix = histories
        .iter()
        .position(|history| history.project == project);
    let mut history = match ix {
        Some(ix) => histories.remove(ix),
        None => ProjectHistory {
            project: project.to_owned(),
            files: Vec::new(),
        },
    };
    match history.files.iter_mut().find(|file| file.path == path) {
        Some(file) => {
            file.score = file.frecency(now) + 1.;
            file.last = now;
        }
        None => history.files.push(FileUse {
            path: path.to_path_buf(),
            score: 1.,
            last: now,
        }),
    }
    if history.files.len() > MAX_FILES {
        history
            .files
            .sort_by(|a, b| b.frecency(now).total_cmp(&a.frecency(now)));
        history.files.truncate(MAX_FILES);
    }
    histories.insert(0, history);
    histories.truncate(MAX_PROJECTS);
}

/// The files used in `project`.
pub fn files<'a>(histories: &'a [ProjectHistory], project: &str) -> &'a [FileUse] {
    histories
        .iter()
        .find(|history| history.project == project)
        .map_or(&[], |history| &history.files)
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: u64 = 24 * 60 * 60;

    #[test]
    fn uses_add_up_and_fade() {
        let mut histories = Vec::new();
        let now = 100 * DAY;
        for _ in 0..4 {
            record(
                &mut histories,
                "/p",
                Path::new("/p/daily.rs"),
                now - 6 * DAY,
            );
        }
        record(&mut histories, "/p", Path::new("/p/once.rs"), now);
        let used = files(&histories, "/p");
        let score = |name: &str| {
            used.iter()
                .find(|file| file.path.ends_with(name))
                .unwrap()
                .frecency(now)
        };
        // Four uses six days (two half-lives) ago are worth one use now.
        assert!((score("daily.rs") - 1.).abs() < 1e-9);
        assert!((score("once.rs") - 1.).abs() < 1e-9);
        record(&mut histories, "/p", Path::new("/p/daily.rs"), now);
        assert_eq!(files(&histories, "/p")[0].score, 2.);
    }

    #[test]
    fn projects_are_kept_apart_and_capped() {
        let mut histories = Vec::new();
        for ix in 0..MAX_PROJECTS + 2 {
            record(&mut histories, &format!("/p{ix}"), Path::new("/a.rs"), 0);
        }
        record(&mut histories, "/p5", Path::new("/b.rs"), 0);
        assert_eq!(histories.len(), MAX_PROJECTS);
        assert_eq!(histories[0].project, "/p5");
        assert_eq!(files(&histories, "/p5").len(), 2);
        assert!(files(&histories, "/p0").is_empty());
    }

    #[test]
    fn least_used_files_are_dropped() {
        let mut histories = Vec::new();
        record(&mut histories, "/p", Path::new("/p/keep.rs"), 0);
        record(&mut histories, "/p", Path::new("/p/keep.rs"), 0);
        for ix in 0..MAX_FILES {
            record(&mut histories, "/p", Path::new(&format!("/p/{ix}.rs")), 0);
        }
        let files = files(&histories, "/p");
        assert_eq!(files.len(), MAX_FILES);
        assert!(files.iter().any(|file| file.path.ends_with("keep.rs")));
    }
}
