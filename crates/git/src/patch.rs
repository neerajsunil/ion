//! Unified diff (`git diff`, `git show`) parsing.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineKind {
    Context,
    Added,
    Removed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffLine {
    pub kind: LineKind,
    pub text: String,
    /// One-based line numbers in the old and new file.
    pub old_line: Option<u32>,
    pub new_line: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hunk {
    /// The `@@ -a,b +c,d @@ context` line.
    pub header: String,
    pub lines: Vec<DiffLine>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FileDiff {
    /// `None` for an added file.
    pub old_path: Option<String>,
    /// `None` for a deleted file.
    pub new_path: Option<String>,
    pub binary: bool,
    pub hunks: Vec<Hunk>,
}

impl FileDiff {
    pub fn path(&self) -> &str {
        self.new_path
            .as_deref()
            .or(self.old_path.as_deref())
            .unwrap_or("")
    }

    /// A short description such as "renamed from x" or "new file".
    pub fn describe(&self) -> Option<String> {
        match (&self.old_path, &self.new_path) {
            (None, Some(_)) => Some("new file".into()),
            (Some(_), None) => Some("deleted".into()),
            (Some(old), Some(new)) if old != new => Some(format!("renamed from {old}")),
            _ => None,
        }
    }

    pub fn stats(&self) -> (usize, usize) {
        let lines = self.hunks.iter().flat_map(|hunk| &hunk.lines);
        lines.fold((0, 0), |(added, removed), line| match line.kind {
            LineKind::Added => (added + 1, removed),
            LineKind::Removed => (added, removed + 1),
            LineKind::Context => (added, removed),
        })
    }
}

/// Parses the output of `git diff` or `git show` into per-file diffs.
pub fn parse(text: &str) -> Vec<FileDiff> {
    let mut files: Vec<FileDiff> = Vec::new();
    let mut lines = text.lines().peekable();
    while let Some(line) = lines.next() {
        if let Some(rest) = line.strip_prefix("diff --git ") {
            let path = paths_from_git_header(rest);
            files.push(FileDiff {
                old_path: path.clone(),
                new_path: path,
                ..Default::default()
            });
            continue;
        }
        let Some(file) = files.last_mut() else {
            continue;
        };
        if let Some(path) = line.strip_prefix("--- ") {
            file.old_path = strip_side(path, "a/");
        } else if let Some(path) = line.strip_prefix("+++ ") {
            file.new_path = strip_side(path, "b/");
        } else if let Some(path) = line.strip_prefix("rename from ") {
            file.old_path = Some(path.to_owned());
        } else if let Some(path) = line.strip_prefix("rename to ") {
            file.new_path = Some(path.to_owned());
        } else if line.starts_with("new file mode") {
            file.old_path = None;
        } else if line.starts_with("deleted file mode") {
            file.new_path = None;
        } else if line.starts_with("Binary files ") || line == "GIT binary patch" {
            file.binary = true;
        } else if line.starts_with("@@ ") {
            let Some(((mut old, mut old_left), (mut new, mut new_left))) = parse_hunk_header(line)
            else {
                continue;
            };
            let mut hunk = Hunk {
                header: line.to_owned(),
                lines: Vec::new(),
            };
            while old_left + new_left > 0
                && let Some(&next) = lines.peek()
            {
                let (kind, text) = match next.as_bytes().first() {
                    Some(b' ') => (LineKind::Context, &next[1..]),
                    Some(b'+') => (LineKind::Added, &next[1..]),
                    Some(b'-') => (LineKind::Removed, &next[1..]),
                    // "\ No newline at end of file"
                    Some(b'\\') => {
                        lines.next();
                        continue;
                    }
                    // Some tools drop the space on empty context lines.
                    None => (LineKind::Context, ""),
                    _ => break,
                };
                lines.next();
                let (old_line, new_line) = match kind {
                    LineKind::Context => (Some(old), Some(new)),
                    LineKind::Added => (None, Some(new)),
                    LineKind::Removed => (Some(old), None),
                };
                if old_line.is_some() {
                    old += 1;
                    old_left = old_left.saturating_sub(1);
                }
                if new_line.is_some() {
                    new += 1;
                    new_left = new_left.saturating_sub(1);
                }
                hunk.lines.push(DiffLine {
                    kind,
                    text: text.strip_suffix('\r').unwrap_or(text).to_owned(),
                    old_line,
                    new_line,
                });
            }
            file.hunks.push(hunk);
        }
    }
    files
}

fn strip_side(path: &str, prefix: &str) -> Option<String> {
    let path = path.trim_end_matches('\t');
    (path != "/dev/null").then(|| path.strip_prefix(prefix).unwrap_or(path).to_owned())
}

/// The path from "a/<path> b/<path>" when both sides match (binary and
/// mode-only changes have no ---/+++ lines).
fn paths_from_git_header(rest: &str) -> Option<String> {
    let half = rest.len().checked_sub(1)? / 2;
    let (a, b) = (rest.get(..half)?, rest.get(half + 1..)?);
    let (a, b) = (a.strip_prefix("a/")?, b.strip_prefix("b/")?);
    (a == b).then(|| a.to_owned())
}

/// (start, length) of each side from "@@ -a,b +c,d @@". A missing length
/// means 1.
pub(crate) fn parse_hunk_header(line: &str) -> Option<((u32, u32), (u32, u32))> {
    let mut parts = line.split(' ').skip(1);
    let old = parts.next()?.strip_prefix('-')?;
    let new = parts.next()?.strip_prefix('+')?;
    let range = |range: &str| -> Option<(u32, u32)> {
        let (start, len) = range.split_once(',').unwrap_or((range, "1"));
        Some((start.parse().ok()?, len.parse().ok()?))
    };
    Some((range(old)?, range(new)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIFF: &str = "\
diff --git a/src/lib.rs b/src/lib.rs
index 1111111..2222222 100644
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -1,3 +1,4 @@ mod a;
 fn one() {}
-fn two() {}
+fn two() { 2 }
+fn three() {}
 fn four() {}
diff --git a/new file.txt b/new file.txt
new file mode 100644
index 0000000..3333333
--- /dev/null
+++ b/new file.txt
@@ -0,0 +1 @@
+hello
\\ No newline at end of file
diff --git a/old.rs b/moved.rs
similarity index 100%
rename from old.rs
rename to moved.rs
diff --git a/logo.png b/logo.png
index 4444444..5555555 100644
Binary files a/logo.png and b/logo.png differ
";

    #[test]
    fn parses_files_and_hunks() {
        let files = parse(DIFF);
        assert_eq!(files.len(), 4);

        let lib = &files[0];
        assert_eq!(lib.path(), "src/lib.rs");
        assert_eq!(lib.stats(), (2, 1));
        let lines = &lib.hunks[0].lines;
        assert_eq!(lines.len(), 5);
        assert_eq!(lines[1].kind, LineKind::Removed);
        assert_eq!((lines[1].old_line, lines[1].new_line), (Some(2), None));
        assert_eq!((lines[3].old_line, lines[3].new_line), (None, Some(3)));
        assert_eq!((lines[4].old_line, lines[4].new_line), (Some(3), Some(4)));

        let new = &files[1];
        assert_eq!(new.old_path, None);
        assert_eq!(new.path(), "new file.txt");
        assert_eq!(new.describe().as_deref(), Some("new file"));
        assert_eq!(new.hunks[0].lines.len(), 1);

        let moved = &files[2];
        assert_eq!(moved.describe().as_deref(), Some("renamed from old.rs"));
        assert!(moved.hunks.is_empty());

        assert!(files[3].binary);
        assert_eq!(files[3].path(), "logo.png");
    }
}
