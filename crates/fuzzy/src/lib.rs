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
/// it. Ties go to shorter paths. A query containing `/` or `\` matches against
/// the whole path instead.
pub fn match_paths<'a>(
    candidates: impl IntoIterator<Item = Candidate<'a>>,
    query: &str,
    limit: usize,
) -> Vec<Match> {
    let query = query.trim().to_lowercase().replace('\\', "/");
    let by_path = query.contains('/');
    let mut matches: Vec<(u32, usize, usize)> = candidates
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
            Some((rank, path.len(), index))
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
        .map(|(rank, len, index)| Match {
            index,
            score: rank * 100_000 + len.min(99_999) as u32,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(paths: &[&str], query: &str) -> Vec<String> {
        let lower: Vec<String> = paths.iter().map(|p| p.to_lowercase()).collect();
        let candidates = lower.iter().map(|path| Candidate {
            path_lower: path,
            name_start: path.rfind('/').map_or(0, |slash| slash + 1),
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
}
