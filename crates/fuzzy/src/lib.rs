//! File-name matching and ranking for the file finder.
//!
//! Matching is by substring, not fuzzy subsequence: typing `main` finds
//! `main.rs` and `domain.rs`, and exact names rank first. Pure logic with no UI
//! dependencies.

/// A candidate to match: its lowercased relative path and where the file name
/// starts in it.
pub struct Candidate<'a> {
    pub path_lower: &'a str,
    pub name_start: usize,
    /// Position in the recently used files (0 = most recent), if there.
    pub recent: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Match {
    /// Index of the candidate in the input.
    pub index: usize,
    /// Lower is better.
    pub score: u32,
}

/// Returns the best `limit` matches for `query`, best first.
///
/// Ranking: exact file name, then file name without extension, then names
/// starting with the query, then names containing it, then paths containing
/// it. Ties go to recently used files (most recent first), then shorter paths.
/// A query containing `/` or `\` matches against the whole path instead. An
/// empty query lists recent files first.
pub fn match_paths<'a>(
    candidates: impl IntoIterator<Item = Candidate<'a>>,
    query: &str,
    limit: usize,
) -> Vec<Match> {
    let query = query.trim().to_lowercase().replace('\\', "/");
    let by_path = query.contains('/');
    let mut matches: Vec<(u32, u32, usize, usize)> = candidates
        .into_iter()
        .enumerate()
        .filter_map(|(index, candidate)| {
            let path = candidate.path_lower;
            let rank = if query.is_empty() {
                0
            } else if by_path {
                if path.ends_with(&query) {
                    0
                } else if path.contains(&query) {
                    1
                } else {
                    return None;
                }
            } else {
                let name = &path[candidate.name_start..];
                let stem = name.rsplit_once('.').map_or(name, |(stem, _)| stem);
                if name == query {
                    0
                } else if stem == query {
                    1
                } else if name.starts_with(&query) {
                    2
                } else if name.contains(&query) {
                    3
                } else if path.contains(&query) {
                    4
                } else {
                    return None;
                }
            };
            Some((
                rank,
                candidate.recent.unwrap_or(u32::MAX),
                path.len(),
                index,
            ))
        })
        .collect();

    let limit = limit.min(matches.len());
    if limit == 0 {
        return Vec::new();
    }
    // Partial sort: only the top `limit` need ordering.
    matches.select_nth_unstable(limit - 1);
    matches.truncate(limit);
    matches.sort_unstable();
    matches
        .into_iter()
        .map(|(rank, _, len, index)| Match {
            index,
            score: rank * 100_000 + len.min(99_999) as u32,
        })
        .collect()
}

/// Splits a position off a query: `src/a.rs:12:5`, `a.rs:12`, `a.rs(12,5)`
/// (as compilers and agents print them). Returns the query without it and
/// the one-based line and column. A trailing `:` (still typing) is dropped.
pub fn split_position(query: &str) -> (&str, Option<(usize, Option<usize>)>) {
    let query = query.trim();
    let numbers = |text: &str, separator: char| -> Option<(usize, Option<usize>)> {
        let mut parts = text.split(separator);
        let line = parts.next()?.trim().parse().ok()?;
        let column = match parts.next() {
            Some(column) if !column.trim().is_empty() => Some(column.trim().parse().ok()?),
            _ => None,
        };
        parts.next().is_none().then_some((line, column))
    };
    if let Some(rest) = query.strip_suffix(')')
        && let Some((path, position)) = rest.rsplit_once('(')
        && let Some(position) = numbers(position, ',')
    {
        return (path, Some(position));
    }
    let trimmed = query.strip_suffix(':').unwrap_or(query);
    // `path:line` or `path:line:column`; a drive letter's colon isn't followed
    // by digits.
    let Some((first, rest)) = trimmed.split_once(':') else {
        return (trimmed, None);
    };
    if !first.is_empty() && rest.starts_with(|c: char| c.is_ascii_digit()) {
        return match numbers(rest, ':') {
            Some(position) => (first, Some(position)),
            None => (query, None),
        };
    }
    // `C:\code\a.rs:3`: past the drive letter's colon.
    if first.len() == 1
        && let Some((path, position)) = rest.split_once(':')
        && let Some(position) = numbers(position, ':')
    {
        return (&trimmed[..first.len() + 1 + path.len()], Some(position));
    }
    (query, None)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(paths: &[&str], query: &str) -> Vec<String> {
        let lower: Vec<String> = paths.iter().map(|p| p.to_lowercase()).collect();
        let candidates = lower.iter().map(|path| Candidate {
            path_lower: path,
            name_start: path.rfind('/').map_or(0, |slash| slash + 1),
            recent: None,
        });
        match_paths(candidates, query, 10)
            .into_iter()
            .map(|m| paths[m.index].to_owned())
            .collect()
    }

    #[test]
    fn exact_names_first() {
        let paths = [
            "src/domain.rs",
            "src/bin/main.rs",
            "main.rs",
            "src/main_window.rs",
        ];
        // `domain.rs` contains "main.rs" too, but exact names rank first.
        assert_eq!(
            run(&paths, "main.rs"),
            ["main.rs", "src/bin/main.rs", "src/domain.rs"]
        );
        assert_eq!(
            run(&paths, "main"),
            [
                "main.rs",
                "src/bin/main.rs",
                "src/main_window.rs",
                "src/domain.rs"
            ]
        );
    }

    #[test]
    fn case_insensitive_and_path_queries() {
        let paths = ["crates/editor/src/Editor.rs", "crates/text/src/lib.rs"];
        assert_eq!(run(&paths, "EDITOR.RS"), ["crates/editor/src/Editor.rs"]);
        assert_eq!(run(&paths, "text/src"), ["crates/text/src/lib.rs"]);
        assert_eq!(run(&paths, r"text\src"), ["crates/text/src/lib.rs"]);
    }

    #[test]
    fn empty_query_lists_everything() {
        assert_eq!(run(&["b", "a"], "").len(), 2);
    }

    #[test]
    fn recent_files_win_ties_and_lead_an_empty_query() {
        let paths = ["a/button.rs", "src/components/button.rs", "z.rs"];
        let recent = [None, Some(0), Some(1)];
        let candidates = || {
            paths.iter().zip(recent).map(|(path, recent)| Candidate {
                path_lower: path,
                name_start: path.rfind('/').map_or(0, |slash| slash + 1),
                recent,
            })
        };
        let names = |query| -> Vec<&str> {
            match_paths(candidates(), query, 10)
                .into_iter()
                .map(|m| paths[m.index])
                .collect()
        };
        // Same rank: the recent one first, despite its longer path.
        assert_eq!(names("button"), ["src/components/button.rs", "a/button.rs"]);
        // A better rank still wins over recency.
        assert_eq!(names("button.rs")[0], "src/components/button.rs");
        assert_eq!(
            names(""),
            ["src/components/button.rs", "z.rs", "a/button.rs"]
        );
    }

    #[test]
    fn splits_positions_off_queries() {
        assert_eq!(
            split_position("src/a.rs:12:5"),
            ("src/a.rs", Some((12, Some(5))))
        );
        assert_eq!(split_position("a.rs:12"), ("a.rs", Some((12, None))));
        assert_eq!(split_position("a.rs:12:"), ("a.rs", Some((12, None))));
        assert_eq!(split_position("a.rs:"), ("a.rs", None));
        assert_eq!(split_position("a.rs(12,5)"), ("a.rs", Some((12, Some(5)))));
        assert_eq!(split_position("a.rs(12)"), ("a.rs", Some((12, None))));
        assert_eq!(
            split_position(r"C:\code\a.rs:3"),
            (r"C:\code\a.rs", Some((3, None)))
        );
        assert_eq!(split_position(r"C:\code"), (r"C:\code", None));
        assert_eq!(
            split_position(r"C:\a.rs:3:5"),
            (r"C:\a.rs", Some((3, Some(5))))
        );
        assert_eq!(split_position("main"), ("main", None));
        assert_eq!(split_position("a.rs:x"), ("a.rs:x", None));
    }
}
