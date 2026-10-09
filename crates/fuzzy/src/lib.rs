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
///
/// Words separated by spaces match in any order (`e1000 main` finds
/// `intel/e1000/e1000_main.c`) when the query as a whole matches nothing:
/// every word in the file name first, then the last word in the name, then
/// all of them anywhere in the path.
pub fn match_paths<'a>(
    candidates: impl IntoIterator<Item = Candidate<'a>>,
    query: &str,
    limit: usize,
) -> Vec<Match> {
    match_paths_with_all(candidates, query, limit).0
}

/// Like [`match_paths`], also returning the position of every candidate that
/// matched. A longer query that [`narrows`] this one only matches among those.
/// An empty query narrows nothing, so it returns none.
pub fn match_paths_with_all<'a>(
    candidates: impl IntoIterator<Item = Candidate<'a>>,
    query: &str,
    limit: usize,
) -> (Vec<Match>, Vec<usize>) {
    let query = normalize(query);
    let by_path = query.contains('/');
    let words: Vec<&str> = query.split_whitespace().collect();
    let mut matches: Vec<(u32, u32, usize, usize)> = candidates
        .into_iter()
        .enumerate()
        .filter_map(|(index, candidate)| {
            let path = candidate.path_lower;
            let rank = rank(path, candidate.name_start, &query, by_path).or_else(|| {
                (words.len() > 1).then(|| rank_words(path, candidate.name_start, &words))?
            })?;
            Some((
                rank,
                candidate.recent.unwrap_or(u32::MAX),
                path.len(),
                index,
            ))
        })
        .collect();
    let all = if query.is_empty() {
        Vec::new()
    } else {
        matches.iter().map(|m| m.3).collect()
    };

    let limit = limit.min(matches.len());
    if limit == 0 {
        return (Vec::new(), all);
    }
    // Partial sort: only the top `limit` need ordering.
    matches.select_nth_unstable(limit - 1);
    matches.truncate(limit);
    matches.sort_unstable();
    let top = matches
        .into_iter()
        .map(|(rank, _, len, index)| Match {
            index,
            score: rank * 100_000 + len.min(99_999) as u32,
        })
        .collect();
    (top, all)
}

/// Whether every path matching `query` also matches `previous` (the query
/// only grew at the end), so `query` can search just `previous`'s matches.
pub fn narrows(previous: &str, query: &str) -> bool {
    let previous = normalize(previous);
    !previous.is_empty() && normalize(query).starts_with(&previous)
}

fn normalize(query: &str) -> String {
    query.trim().to_lowercase().replace('\\', "/")
}

/// How well the whole query matches, lower is better.
fn rank(path: &str, name_start: usize, query: &str, by_path: bool) -> Option<u32> {
    if query.is_empty() {
        return Some(0);
    }
    if by_path {
        return if path.ends_with(query) {
            Some(0)
        } else if path.contains(query) {
            Some(1)
        } else {
            None
        };
    }
    let name = &path[name_start..];
    let stem = name.rsplit_once('.').map_or(name, |(stem, _)| stem);
    if name == query {
        Some(0)
    } else if stem == query {
        Some(1)
    } else if name.starts_with(query) {
        Some(2)
    } else if name.contains(query) {
        Some(3)
    } else if path.contains(query) {
        Some(4)
    } else {
        None
    }
}

/// How well separate words match, ranked after every whole-query match.
fn rank_words(path: &str, name_start: usize, words: &[&str]) -> Option<u32> {
    if !words.iter().all(|word| path.contains(word)) {
        return None;
    }
    let name = &path[name_start..];
    let last = words.last()?;
    Some(if words.iter().all(|word| name.contains(word)) {
        5
    } else if name.contains(last) {
        6
    } else {
        7
    })
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
    fn words_match_in_any_order() {
        let paths = [
            "drivers/net/e1000/e1000_main.c",
            "drivers/net/e1000/e1000_hw.c",
            "drivers/main/e1000.c",
            "init/main.c",
            "docs/my notes.txt",
        ];
        assert_eq!(
            run(&paths, "e1000 main"),
            [
                // Both words in the name, then anywhere in the path.
                "drivers/net/e1000/e1000_main.c",
                "drivers/main/e1000.c",
            ]
        );
        // The last word in the name ranks above words only in folders.
        assert_eq!(
            run(&paths, "main e1000"),
            ["drivers/net/e1000/e1000_main.c", "drivers/main/e1000.c",]
        );
        assert_eq!(run(&paths, "net main")[0], "drivers/net/e1000/e1000_main.c");
        assert_eq!(run(&paths, "init main"), ["init/main.c"]);
        assert_eq!(run(&paths, "hw e1000"), ["drivers/net/e1000/e1000_hw.c"]);
        // A name with a space still matches as a whole first.
        assert_eq!(run(&paths, "my notes"), ["docs/my notes.txt"]);
        assert!(run(&paths, "e1000 zzz").is_empty());
    }

    #[test]
    fn longer_queries_narrow_shorter_ones() {
        assert!(narrows("mai", "main"));
        assert!(narrows("e1000", "e1000 m"));
        assert!(narrows("E1000 ", "e1000 main"));
        assert!(narrows("src", r"src\a"));
        assert!(!narrows("main", "mai"));
        assert!(!narrows("main", "xmain"));
        assert!(!narrows("", "main"));

        // Matching among the previous matches gives the same results.
        let paths = [
            "drivers/net/e1000/e1000_main.c",
            "drivers/net/e1000/e1000_hw.c",
            "init/main.c",
        ];
        let candidates = |only: &[usize]| -> Vec<Candidate<'static>> {
            only.iter()
                .map(|&ix| Candidate {
                    path_lower: paths[ix],
                    name_start: paths[ix].rfind('/').map_or(0, |slash| slash + 1),
                    recent: None,
                })
                .collect()
        };
        let (_, all) = match_paths_with_all(candidates(&[0, 1, 2]), "e1000", 10);
        assert_eq!(all, [0, 1]);
        let (top, _) = match_paths_with_all(candidates(&all), "e1000 main", 10);
        assert_eq!(top.len(), 1);
        assert_eq!(paths[all[top[0].index]], "drivers/net/e1000/e1000_main.c");
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
